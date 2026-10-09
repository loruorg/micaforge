//! What people did once they arrived: goals, funnels, journeys, retention and
//! the errors they hit. `docs/CONTRACT.md` §3.3.
//!
//! These five reports differ from the ones in [`super::stats`] in one way that
//! shapes the whole module: they read **configuration** as well as events. A
//! goal and a funnel are rows in Postgres, and the query engine deliberately
//! does not know that — it takes the parsed rule and answers against
//! ClickHouse. So the work here is to load the rule, check it belongs to the
//! site the caller asked about, translate it into the engine's form, and shape
//! the answer.
//!
//! ## Ownership is checked, never assumed
//!
//! A goal id and a funnel id are UUIDs a caller could hold from anywhere. Every
//! load here compares the row's own `site_id` against the site the request
//! resolved, and answers `not_found` when they differ — the same way a site
//! lookup does, because a `403` confirms the row exists.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Datelike, TimeZone, Timelike};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use micaforge_core::{Compare, Error, Filter, FilterGroup, FilterNode, Granularity, Metric};
use micaforge_query::stats::{self, FunnelStepDef, GoalDef};
use micaforge_store::models::{error_states, funnels as funnel_repo, goals as goal_repo};

use super::agents::{json_body, same_site};
use crate::error::ApiError;
use crate::extract::{AuthUser, Envelope, SiteAccess};
use crate::response::{ok, ok_meta};
use crate::routes::orgs::{require_at_least, MEMBER};
use crate::state::AppState;

/// Goals, funnels, journeys, retention and errors.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats/goals", get(goals))
        // Written `:id` in the contract; axum 0.8 spells a path parameter
        // `{id}`. Same route.
        .route("/api/stats/funnel/{id}", get(funnel))
        .route("/api/stats/journeys", get(journeys))
        .route("/api/stats/retention", get(retention))
        .route("/api/stats/errors", get(errors))
        .route(
            "/api/stats/errors/{fingerprint}/status",
            post(set_error_status),
        )
}

/// Most goals a request will draw a trend for.
///
/// Each trend is its own bucketed read, so a site with a hundred goals would
/// turn one screen into a hundred queries. Past this many the rows still carry
/// their totals and the sparklines are simply absent, which is a visibly thinner
/// answer rather than a slow one.
const MAX_TREND_GOALS: usize = 20;

/// Default number of cohorts in a retention grid.
const DEFAULT_COHORTS: u32 = 12;

/// Default depth of a journey graph, in hops.
const DEFAULT_JOURNEY_DEPTH: u32 = 4;

// ---------------------------------------------------------------------------
// Goals
// ---------------------------------------------------------------------------

/// Turn a `goals` row into the rule the engine evaluates.
fn goal_def(row: &goal_repo::Goal) -> Result<GoalDef, Error> {
    GoalDef::parse(
        row.id.to_string(),
        row.name.clone(),
        &row.kind,
        &row.conditions.to_string(),
        row.value_cents,
    )
}

/// `GET /api/stats/goals` — conversions, rate and revenue per configured goal.
///
/// The site-level `conversion_rate` is `null` on purpose. A visitor who
/// converted three goals is one converting visitor, and the per-goal rows cannot
/// be summed without counting them three times; reporting a number that can
/// exceed 100% would be worse than reporting none. `conversions` and `sessions`
/// are both here, so a client that wants conversions-per-session can say so in
/// its own words.
async fn goals(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let configured = goal_repo::list(&st.pg, access.site.id).await?;
    let defs: Vec<GoalDef> = configured
        .iter()
        .map(goal_def)
        .collect::<Result<Vec<_>, Error>>()?;

    let rows = stats::goals(&st.ch, &env, &defs).await?;
    let totals = stats::overview(&st.ch, &env).await?;

    // A trend is the same goal counted per bucket. The comparison window is
    // dropped for it: a sparkline draws one line, and asking for the ghost would
    // double every read behind this screen for nothing.
    let mut trend_env = env.clone();
    trend_env.compare = Compare::None;

    let mut shaped = Vec::with_capacity(rows.len());
    for (row, def) in rows.iter().zip(defs.iter()) {
        let trend = if defs.len() <= MAX_TREND_GOALS {
            stats::timeseries(&st.ch, &trend_env, Metric::Conversions, Some(def))
                .await?
                .buckets
                .into_iter()
                .map(|b| b.v)
                .collect::<Vec<f64>>()
        } else {
            Vec::new()
        };

        shaped.push(json!({
            "id": row.id,
            "name": row.name,
            "conversions": row.conversions,
            "visitors": row.visitors,
            "rate": row.rate,
            "revenue": row.revenue,
            "trend": trend,
        }));
    }

    let conversions: u64 = rows.iter().map(|r| r.conversions).sum();
    let revenue: f64 = rows.iter().map(|r| r.revenue).sum();

    let meta = json!({ "rows": shaped.len() });
    Ok(ok_meta(
        json!({
        "goals": shaped,
        "conversions": conversions,
        "conversion_rate": Value::Null,
        "revenue": revenue,
        "sessions": totals.sessions,
        "visitors": totals.visitors,
        }),
        meta,
    ))
}

// ---------------------------------------------------------------------------
// Funnels
// ---------------------------------------------------------------------------

/// Parameters the funnel report takes beyond the envelope.
#[derive(Debug, Default, Deserialize)]
struct FunnelParams {
    /// 1-based step to list sessions for. Only read with `sessions`.
    step: Option<u32>,
    /// Truthy to ask for the sessions behind a step instead of the step counts.
    sessions: Option<String>,
}

/// One step of a configured funnel, in either shape a `funnels.steps` array
/// carries.
///
/// The editor writes `{kind, op, value, label}`, which is what a person builds
/// in the UI. The contract's own filter shape, `{label, conditions}`, is what an
/// API client or an import writes. Both are read, because both are real: a
/// funnel created through the dashboard and one created through the API have to
/// mean the same thing.
#[derive(Debug, Deserialize)]
struct StepSpec {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    conditions: Option<Value>,
}

/// Translate one configured step into the engine's step definition.
///
/// A `goal` step is expressed by the goal's own conditions. The goal's *kind*
/// gate — "this must have been a pageview", "this must have been a download" —
/// is not carried, because a step is a `FilterGroup` and the event kind is not
/// one of the dimensions in §3.2. In practice a goal's conditions already name
/// what identifies it (a path, an event name), and a goal with no conditions at
/// all is refused by [`FunnelStepDef::predicate`] rather than silently matching
/// every row.
async fn step_def(
    st: &AppState,
    site: Uuid,
    index: usize,
    spec: &StepSpec,
) -> Result<FunnelStepDef, Error> {
    let field = format!("steps.{index}");

    if let Some(conditions) = &spec.conditions {
        let label = spec
            .label
            .clone()
            .unwrap_or_else(|| format!("Step {}", index + 1));
        return FunnelStepDef::parse(label, &conditions.to_string());
    }

    let kind = spec.kind.as_deref().unwrap_or("page");
    let op = spec.op.as_deref().unwrap_or("is");
    let value = spec.value.as_deref().unwrap_or("").trim();

    if value.is_empty() {
        return Err(Error::validation(
            field.as_str(),
            "this step has nothing to match",
        ));
    }

    let label = spec
        .label
        .clone()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| value.to_string());

    let dimension = match kind {
        "page" => "pathname",
        "event" => "event_name",
        "goal" => {
            let uuid = Uuid::parse_str(value).map_err(|_| {
                Error::validation(field.as_str(), "a goal step names a goal by its UUID")
            })?;
            let goal = goal_repo::get(&st.pg, uuid).await?;
            if goal.site_id != site {
                return Err(Error::not_found("goal"));
            }
            let label = spec
                .label
                .clone()
                .filter(|l| !l.trim().is_empty())
                .unwrap_or(goal.name);
            return FunnelStepDef::parse(label, &goal.conditions.to_string());
        }
        other => {
            return Err(Error::validation(
                &field,
                format!("unknown step kind `{other}`, expected page, event or goal"),
            ))
        }
    };

    // Built through core's own deserialiser so the operator vocabulary and the
    // value rules are the ones §3.2 defines, rather than a second copy of them.
    let filter: Filter = serde_json::from_value(json!({
        "dimension": dimension,
        "op": op,
        "value": value,
    }))
    .map_err(|e| Error::validation(field.as_str(), e.to_string()))?;
    filter.validate()?;

    let group = FilterGroup::and(vec![FilterNode::Leaf(filter)]);
    Ok(FunnelStepDef {
        label,
        conditions: group,
    })
}

/// A short human sentence describing what a step matches.
///
/// Written from the configured step rather than from the compiled predicate, so
/// it reads like the rule a person typed and not like SQL.
fn step_detail(spec: &StepSpec) -> String {
    let kind = spec.kind.as_deref().unwrap_or("page");
    let op = spec.op.as_deref().unwrap_or("is").replace('_', " ");
    let value = spec.value.as_deref().unwrap_or("");

    match kind {
        "page" => format!("path {op} {value}"),
        "event" => format!("event {op} {value}"),
        "goal" => "the goal's own rule".to_string(),
        _ => "a saved condition".to_string(),
    }
}

/// `GET /api/stats/funnel/:id` — step counts and drop-off.
///
/// With `?sessions=1&step=N` it answers with the sessions behind one step
/// instead. That listing is marked `approximate` in the payload and it is not
/// decoration: the step counts come from `windowFunnel`, which requires the
/// steps to have happened *in order*, while the listing selects the sessions
/// that satisfied step N's condition at all. The two agree for almost every
/// funnel and differ for a visitor who did step three before step two.
async fn funnel(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Path(id): Path<String>,
    Query(params): Query<FunnelParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let uuid =
        Uuid::parse_str(id.trim()).map_err(|_| Error::validation("id", "a funnel id is a UUID"))?;

    let row = funnel_repo::get(&st.pg, uuid).await?;
    if row.site_id != access.site.id {
        return Err(Error::not_found("funnel").into());
    }

    let specs: Vec<StepSpec> = serde_json::from_value(row.steps.clone()).map_err(|e| {
        Error::validation("steps", format!("this funnel's steps are unreadable: {e}"))
    })?;

    let mut defs = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        defs.push(step_def(&st, access.site.id, index, spec).await?);
    }

    let window_hours = u32::try_from(row.window_hours.max(1)).unwrap_or(24);

    let wants_sessions = params
        .sessions
        .as_deref()
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false);

    if wants_sessions {
        let step = params.step.unwrap_or(1).max(1) as usize;
        let spec = defs.get(step - 1).ok_or_else(|| {
            Error::validation("step", format!("this funnel has {} steps", defs.len()))
        })?;

        // The step's condition is intersected with whatever the envelope already
        // carries, so a filtered funnel lists filtered sessions.
        let mut scoped = env.clone();
        scoped.filters = Some(merge_filters(env.filters.as_ref(), &spec.conditions));

        let page = stats::sessions(&st.ch, &scoped).await?;
        let rows = json_body(&page.rows)?;
        return Ok(ok(json!({
            "rows": rows,
            "total": page.total,
            "step": step,
            "approximate": true,
            "basis": "step_condition",
        })));
    }

    let report = stats::funnel(&st.ch, &env, &defs, window_hours).await?;

    let mut steps = Vec::with_capacity(report.steps.len());
    let mut previous: Option<u64> = None;
    for (index, step) in report.steps.iter().enumerate() {
        let detail = specs.get(index).map(step_detail).unwrap_or_default();
        // The engine reports the drop as a count; the chart draws it as a share
        // of the step it fell from, which is the only denominator that makes a
        // drop comparable between the first step and the fourth.
        let drop_share = match previous {
            Some(before) if before > 0 => step.drop_off as f64 / before as f64,
            _ => 0.0,
        };
        steps.push(json!({
            "index": index + 1,
            "label": step.label,
            "detail": detail,
            "visitors": step.visitors,
            "sessions": step.visitors,
            "rate": step.rate,
            "drop_off": drop_share,
            "drop_count": step.drop_off,
            "avg_seconds_from_previous": Value::Null,
        }));
        previous = Some(step.visitors);
    }

    let conversion_rate = if report.total_entered > 0 {
        report.completed as f64 / report.total_entered as f64
    } else {
        0.0
    };

    Ok(ok(json!({
        "id": row.id,
        "name": row.name,
        "window_hours": row.window_hours,
        "entered": report.total_entered,
        "completed": report.completed,
        "conversion_rate": conversion_rate,
        "steps": steps,
    })))
}

/// `AND` a step's condition onto the envelope's own filters.
///
/// An empty envelope filter is not wrapped in a redundant group: the engine
/// treats an empty group as "no constraint", and nesting one inside another only
/// makes the compiled predicate harder to read in a debug log.
fn merge_filters(existing: Option<&FilterGroup>, extra: &FilterGroup) -> FilterGroup {
    match existing.filter(|g| !g.is_empty()) {
        Some(group) => FilterGroup::and(vec![
            FilterNode::Group(group.clone()),
            FilterNode::Group(extra.clone()),
        ]),
        None => extra.clone(),
    }
}

// ---------------------------------------------------------------------------
// Journeys
// ---------------------------------------------------------------------------

/// Parameters the journey graph takes beyond the envelope.
#[derive(Debug, Default, Deserialize)]
struct JourneyParams {
    /// Hops to follow. Clamped by the engine.
    depth: Option<u32>,
    /// Anchor the graph to sessions that began on this path.
    start: Option<String>,
}

/// `GET /api/stats/journeys` — the Sankey nodes and links.
async fn journeys(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<JourneyParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let depth = params.depth.unwrap_or(DEFAULT_JOURNEY_DEPTH);
    let start = params
        .start
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let graph = stats::journeys(&st.ch, &env, depth, start).await?;

    // The diagram measures a band by `value`; the engine counts `visitors`. One
    // number, two names, and the diagram's is the one this endpoint speaks.
    let nodes: Vec<Value> = graph
        .nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "label": n.label,
                "depth": n.depth,
                "value": n.visitors,
                "visitors": n.visitors,
                "kind": "page",
            })
        })
        .collect();

    let links: Vec<Value> = graph
        .links
        .iter()
        .map(|l| {
            json!({
                "source": l.source,
                "target": l.target,
                "value": l.visitors,
                "visitors": l.visitors,
            })
        })
        .collect();

    // Sessions entering the graph are the first column's total: every session
    // the graph knows about passes through exactly one depth-1 node.
    let sessions: u64 = graph
        .nodes
        .iter()
        .filter(|n| n.depth == 1)
        .map(|n| n.visitors)
        .sum();

    Ok(ok(json!({
        "nodes": nodes,
        "links": links,
        "depth": depth,
        "sessions": sessions,
    })))
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// Parameters the retention grid takes beyond the envelope.
#[derive(Debug, Default, Deserialize)]
struct RetentionParams {
    /// How many cohorts to return, newest last. Clamped by the engine.
    cohorts: Option<u32>,
}

/// A cohort's own label, cut in the site's zone.
///
/// The grid prints this verbatim, so it has to be the period as a person writes
/// it — `2026-W31`, `2026-07` — rather than an instant. It is generic over the
/// zone so this module never has to name the timezone crate; the envelope
/// carries the zone the whole report was resolved in.
fn cohort_label<T: TimeZone>(start_ms: i64, tz: &T, period: Granularity) -> String {
    let Some(at) = tz.timestamp_millis_opt(start_ms).single() else {
        return String::new();
    };
    match period {
        Granularity::Minute => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            at.year(),
            at.month(),
            at.day(),
            at.hour(),
            at.minute()
        ),
        Granularity::Hour => format!(
            "{:04}-{:02}-{:02} {:02}:00",
            at.year(),
            at.month(),
            at.day(),
            at.hour()
        ),
        Granularity::Day => format!("{:04}-{:02}-{:02}", at.year(), at.month(), at.day()),
        Granularity::Week => {
            let week = at.iso_week();
            format!("{:04}-W{:02}", week.year(), week.week())
        }
        Granularity::Month => format!("{:04}-{:02}", at.year(), at.month()),
    }
}

/// `GET /api/stats/retention` — the cohort grid.
///
/// The period is the envelope's own `granularity`, because a retention column
/// and a chart bucket are the same unit of time and having two ways to say it
/// invites them to disagree.
async fn retention(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<RetentionParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let period = env.granularity;
    let cohorts = params.cohorts.unwrap_or(DEFAULT_COHORTS);

    let grid = stats::retention(&st.ch, &env, period, cohorts).await?;
    let periods = grid
        .cohorts
        .iter()
        .map(|c| c.cells.len())
        .max()
        .unwrap_or(0);

    let rows: Vec<Value> = grid
        .cohorts
        .iter()
        .map(|c| {
            json!({
                "cohort": cohort_label(c.start, &env.range.tz, period),
                "started": c.start,
                "start": c.start,
                "size": c.size,
                "values": c.cells,
                "cells": c.cells,
            })
        })
        .collect();

    Ok(ok(json!({
        "unit": period.as_str(),
        "period": period.as_str(),
        "periods": periods,
        "cohorts": rows,
    })))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Parameters the error register takes beyond the envelope.
#[derive(Debug, Default, Deserialize)]
struct ErrorParams {
    /// A release to narrow to: a column of `errors`, not one of the filter
    /// dimensions of §3.2, so it travels by name rather than in `filters`.
    release: Option<String>,
    /// One group. With it the answer is that group's detail, not a list.
    fingerprint: Option<String>,
}

/// A fingerprint as the ingest path writes one, or `None`.
///
/// Lower-case hex, sixteen characters today; up to sixty-four accepted so a
/// longer digest in a later build does not need this changed in step.
fn clean_fingerprint(raw: &str) -> Option<String> {
    let fp = raw.trim();
    (!fp.is_empty() && fp.len() <= 64 && fp.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| fp.to_ascii_lowercase())
}

/// A release name to narrow by, or `None` for none.
fn clean_release(raw: Option<&str>) -> Result<Option<String>, Error> {
    match raw.map(str::trim).filter(|r| !r.is_empty()) {
        None => Ok(None),
        Some(r) if r.chars().count() <= 64 => Ok(Some(r.to_string())),
        Some(_) => Err(Error::validation(
            "release",
            "a release is at most 64 characters",
        )),
    }
}

/// One group on the wire, with the state a person gave it.
fn group_json(g: &stats::ErrorGroup, marks: &HashMap<String, error_states::ErrorState>) -> Value {
    json!({
        "fingerprint": g.fingerprint,
        "name": g.name,
        "message": g.message,
        "count": g.count,
        // The engine calls this `users`; the register prints it under the word
        // every other report on this site uses.
        "visitors": g.users,
        "session_count": g.sessions,
        "first_seen": g.first_seen,
        "last_seen": g.last_seen,
        "handled": g.handled,
        "release": g.release,
        "top_path": g.top_path,
        "browsers": g.browsers,
        "status": error_states::effective(marks.get(&g.fingerprint), g.last_seen),
        "trend": g.sparkline,
    })
}

/// `GET /api/stats/errors` — client-side exceptions grouped by fingerprint.
///
/// The list carries totals over every group in scope (not only the page
/// listed) and the four facets the screen narrows by, so the strip above the
/// table and the menus beside it describe the whole window. With
/// `?fingerprint=` the answer is one group in full instead: its stack, where and
/// when it fired, and the sessions it fired in; `data` is `null` when that group
/// has no occurrence in the window, which the detail screen reads as "nothing
/// here", not as a failure.
async fn errors(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<ErrorParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let release = clean_release(params.release.as_deref())?;
    let marks = error_states::for_site(&st.pg, access.site.id).await?;

    if let Some(raw) = params.fingerprint.as_deref() {
        let fingerprint = clean_fingerprint(raw)
            .ok_or_else(|| Error::validation("fingerprint", "is not an error fingerprint"))?;
        let Some(detail) = stats::error_detail(&st.ch, &env, &fingerprint, release).await? else {
            return Ok(ok(Value::Null));
        };
        let mut body = group_json(&detail.group, &marks);
        if let Value::Object(map) = &mut body {
            map.insert("stack".into(), json!(detail.stack));
            map.insert("filename".into(), json!(detail.filename));
            map.insert("lineno".into(), json!(detail.lineno));
            map.insert("colno".into(), json!(detail.colno));
            map.insert("by_page".into(), json_body(&detail.by_page)?);
            map.insert("by_browser".into(), json_body(&detail.by_browser)?);
            map.insert("by_os".into(), json_body(&detail.by_os)?);
            map.insert("series".into(), json_body(&detail.series)?);
            map.insert("sessions".into(), json_body(&detail.sessions)?);
        }
        return Ok(ok(body));
    }

    let narrow = stats::ErrorNarrow {
        release,
        fingerprint: None,
    };
    let groups = stats::error_groups(&st.ch, &env, &narrow).await?;
    let totals = stats::error_totals(&st.ch, &env, &narrow).await?;
    let facets = stats::error_facets(&st.ch, &env, &narrow).await?;

    let rows: Vec<Value> = groups.iter().map(|g| group_json(g, &marks)).collect();
    let meta = json!({ "rows": rows.len(), "total": totals.groups });
    Ok(ok_meta(
        json!({
            "rows": rows,
            "totals": json_body(&totals)?,
            "facets": json_body(&facets)?,
            // Kept at the top level for callers written before `totals`.
            "groups": totals.groups,
            "occurrences": totals.occurrences,
        }),
        meta,
    ))
}

/// What a person may set an error group to.
#[derive(Debug, Deserialize)]
struct StatusBody {
    /// `open` | `resolved` | `ignored`.
    status: String,
}

/// `POST /api/stats/errors/{fingerprint}/status` — resolve, ignore or reopen
/// one group. Needs `member` on the site: it changes what every reader of the
/// register sees, which is a decision about the site, not a reading of it.
///
/// The group does not have to have fired in any particular window, so nothing
/// is read from ClickHouse; a fingerprint is only checked for its shape.
async fn set_error_status(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
    Path(fingerprint): Path<String>,
    Json(body): Json<StatusBody>,
) -> Result<Json<Value>, ApiError> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let fingerprint = clean_fingerprint(&fingerprint)
        .ok_or_else(|| Error::validation("fingerprint", "is not an error fingerprint"))?;

    let status = match body.status.trim() {
        "open" => {
            error_states::clear(&st.pg, access.site.id, &fingerprint).await?;
            "open"
        }
        s @ ("resolved" | "ignored") => {
            error_states::set(&st.pg, access.site.id, &fingerprint, s, auth.user.id).await?;
            if s == "resolved" {
                "resolved"
            } else {
                "ignored"
            }
        }
        _ => {
            return Err(
                Error::validation("status", "an error can be open, resolved or ignored").into(),
            )
        }
    };
    Ok(ok(json!({ "fingerprint": fingerprint, "status": status })))
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn only_a_hex_fingerprint_is_one() {
        assert_eq!(
            clean_fingerprint(" A1b2c3d4e5f60718 "),
            Some("a1b2c3d4e5f60718".into())
        );
        assert_eq!(clean_fingerprint(""), None);
        assert_eq!(clean_fingerprint("abc' OR 1=1"), None);
        assert_eq!(clean_fingerprint(&"a".repeat(65)), None);
    }

    #[test]
    fn a_release_is_narrowed_by_when_given_and_bounded() {
        assert_eq!(clean_release(None).unwrap(), None);
        assert_eq!(clean_release(Some("  ")).unwrap(), None);
        assert_eq!(
            clean_release(Some(" 2026.10.1 ")).unwrap(),
            Some("2026.10.1".into())
        );
        assert!(clean_release(Some(&"x".repeat(65))).is_err());
    }
}
