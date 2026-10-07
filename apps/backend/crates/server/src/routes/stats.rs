//! The core analytics reads: `docs/CONTRACT.md` §3.3, first ten endpoints.
//!
//! Every handler here is deliberately thin. It parses the few parameters that
//! sit *outside* the standard query envelope (§3.1) — a dimension, a metric, a
//! goal id — validates them against the contract's vocabulary, calls the one
//! matching function in [`micaforge_query::stats`], and shapes the result into
//! the response body. Nothing here decides what a number means; that lives in
//! the query engine, once, so the dashboard card and the CSV export cannot
//! disagree about "bounce rate".
//!
//! ## Two response shapes per endpoint
//!
//! `pages`, `sources`, `locations` and `devices` each answer the same question
//! at two levels. Without `dimension` they return the composite the engine
//! builds — a channel/source/campaign tree, a country/region/city tree — which
//! is what a screen wants on first paint. With `dimension` they return one
//! ranked list, which is what the same screen wants once a person has drilled
//! into it. The set of dimensions each endpoint accepts is closed and checked:
//! `/api/stats/devices?dimension=pathname` is a mistake, not a breakdown, and
//! saying so is better than quietly answering a different question.
//!
//! ## Paths are absolute
//!
//! Each route module carries its own full path rather than relying on a `nest`,
//! because these modules do not partition cleanly by prefix — the replay routes
//! live under `/api/replay` while sitting beside the session routes they belong
//! to. One rule for all of them is easier to keep true than two.
//!
//! Path parameters use axum 0.8's `{name}` syntax; the contract writes `:name`,
//! which is the same route under the older spelling.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use micaforge_core::time::previous_period;
use micaforge_core::{
    Audience, Dimension, Error, Filter, FilterGroup, FilterNode, FilterOp, FilterValue, Metric,
    QueryEnvelope, Table,
};
use micaforge_query::exec::fetch_all;
use micaforge_query::sql::{envelope_over, f64_of, scope_predicate, Param};
use micaforge_query::stats;
use micaforge_store::models::goals as goal_repo;

use super::agents::{json_body, same_site};
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess, SiteOrShare};
use crate::response::{ok, ok_meta};
use crate::state::AppState;

/// Every route in `docs/CONTRACT.md` §3.3 that reports on the event stream
/// itself, as opposed to a configured goal, a person, or a file download.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats/overview", get(overview))
        .route("/api/stats/timeseries", get(timeseries))
        .route("/api/stats/breakdown", get(breakdown))
        .route("/api/stats/realtime", get(realtime))
        .route("/api/stats/pages", get(pages))
        .route("/api/stats/sources", get(sources))
        .route("/api/stats/locations", get(locations))
        .route("/api/stats/devices", get(devices))
        .route("/api/stats/performance", get(performance))
        .route("/api/stats/events", get(events))
}

// ---------------------------------------------------------------------------
// Shared parameter handling
// ---------------------------------------------------------------------------

/// The parameters these endpoints take beyond the envelope.
///
/// Every field is optional because the envelope is what makes a read valid; a
/// dimension or a metric only narrows it. Unknown keys are ignored rather than
/// rejected, which is what lets one struct sit under a query string that also
/// carries `site`, `range`, `filters` and the rest of §3.1.
#[derive(Debug, Default, Deserialize)]
struct StatsParams {
    /// A `Dimension` from §3.2, including the `props.<key>` and `flag.<key>`
    /// forms.
    dimension: Option<String>,
    /// A single `Metric`. Chooses the series on `timeseries` and the ranking
    /// column on `breakdown`.
    metric: Option<String>,
    /// A comma-separated metric list. Only the first entry changes the ranking;
    /// the rest are accepted so a client can name what it intends to display.
    metrics: Option<String>,
    /// A goal id, for the two metrics that have no meaning without one.
    goal: Option<String>,
    /// A custom event name, for the `events` property drill-down.
    event: Option<String>,
    /// Truthy to ask `events` for one event's properties instead of the list.
    properties: Option<String>,
}

/// Parse a dimension, naming the offending value when it is not one.
fn dimension_of(raw: &str) -> Result<Dimension, Error> {
    Dimension::parse(raw)
}

/// Parse a dimension and check it against the closed set an endpoint offers.
///
/// The check is not pedantry. `screen_class` is derived from a screen width and
/// `city` is derived from an address at ingest, so a "devices by city" table is
/// not a table this product can draw. Refusing it by name is more use than
/// returning an empty one.
fn dimension_within(raw: &str, endpoint: &str, allowed: &[&str]) -> Result<Dimension, Error> {
    let dim = dimension_of(raw)?;
    let spelled = dim.as_str();
    if allowed.contains(&spelled.as_str()) {
        return Ok(dim);
    }
    Err(Error::validation(
        "dimension",
        format!(
            "`{spelled}` is not a dimension {endpoint} reports on; it accepts {}",
            allowed.join(", ")
        ),
    ))
}

/// The metrics a request named, first one first. Defaults to `visitors`.
fn metrics_of(params: &StatsParams) -> Result<Vec<Metric>, Error> {
    let raw = params
        .metric
        .as_deref()
        .or(params.metrics.as_deref())
        .unwrap_or("visitors");

    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(Metric::parse(part)?);
    }
    if out.is_empty() {
        out.push(Metric::Visitors);
    }
    Ok(out)
}

/// Load one goal and convert it to the engine's form.
///
/// The site is checked against the goal's own `site_id` rather than trusted from
/// the query string. A goal id is a UUID a caller could have obtained anywhere,
/// and without this check `?site=1&goal=<a goal on site 2>` would report site
/// 2's conversion rule against site 1's events. It answers `not_found` rather
/// than `forbidden` for the same reason site lookups do: a 403 confirms the row
/// exists.
async fn load_goal(st: &AppState, site: Uuid, id: &str) -> Result<stats::GoalDef, Error> {
    let uuid =
        Uuid::parse_str(id.trim()).map_err(|_| Error::validation("goal", "a goal id is a UUID"))?;

    let goal = goal_repo::get(&st.pg, uuid).await?;
    if goal.site_id != site {
        return Err(Error::not_found("goal"));
    }

    stats::GoalDef::parse(
        goal.id.to_string(),
        goal.name,
        &goal.kind,
        &goal.conditions.to_string(),
        goal.value_cents,
    )
}

/// The envelope's filters, plus "this event and no other".
///
/// An empty filter group is not wrapped in a second one: the engine reads an
/// empty group as no constraint, and nesting adds nothing but noise to a debug
/// log of the compiled predicate.
fn narrowed_to(existing: Option<&FilterGroup>, event: &str) -> FilterGroup {
    let leaf = FilterNode::Leaf(Filter::new(
        Dimension::EventName,
        FilterOp::Is,
        FilterValue::One(event.to_string()),
    ));
    match existing.filter(|g| !g.is_empty()) {
        Some(group) => FilterGroup::and(vec![FilterNode::Group(group.clone()), leaf]),
        None => FilterGroup::and(vec![leaf]),
    }
}

/// A row's share of a total, as a fraction between 0 and 1.
///
/// Zero rather than a null when the total is zero: nothing happened, so nothing
/// has a share of it, and every caller here took both numbers from the same
/// read.
fn share_of(part: f64, total: f64) -> f64 {
    if total <= 0.0 {
        0.0
    } else {
        part / total
    }
}

// ---------------------------------------------------------------------------
// Overview, series, breakdown
// ---------------------------------------------------------------------------

/// `GET /api/stats/overview` — totals, the comparison window's totals, deltas.
async fn overview(
    State(st): State<AppState>,
    SiteOrShare(access): SiteOrShare,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let data = stats::overview(&st.ch, &env).await?;
    Ok(ok(json_body(&data)?))
}

/// `GET /api/stats/timeseries` — one metric, bucketed, with its ghost trace.
///
/// The metric chooses the trace rather than the envelope's audience: `visitors`
/// is always the human record and `agent_fetches` is always the machine one, so
/// a chart that wants both asks twice instead of toggling between them.
async fn timeseries(
    State(st): State<AppState>,
    SiteOrShare(access): SiteOrShare,
    Envelope(env): Envelope,
    Query(params): Query<StatsParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let metric = metrics_of(&params)?[0];

    // Conversions have no meaning without the rule that defines them, so the
    // goal is required for exactly those two metrics and ignored for the rest.
    let needs_goal = matches!(metric, Metric::Conversions | Metric::ConversionRate);
    let goal = match (needs_goal, params.goal.as_deref()) {
        (true, Some(id)) => Some(load_goal(&st, access.site.id, id).await?),
        (true, None) => {
            return Err(Error::validation(
                "goal",
                format!(
                    "the `{}` metric needs a goal to count against",
                    metric.as_str()
                ),
            )
            .into())
        }
        (false, _) => None,
    };

    let data = stats::timeseries(&st.ch, &env, metric, goal.as_ref()).await?;
    Ok(ok(json_body(&data)?))
}

/// `GET /api/stats/breakdown` — any dimension, ranked, with shares.
async fn breakdown(
    State(st): State<AppState>,
    SiteOrShare(access): SiteOrShare,
    Envelope(env): Envelope,
    Query(params): Query<StatsParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let raw = params.dimension.as_deref().ok_or_else(|| {
        Error::validation(
            "dimension",
            "a breakdown needs a dimension to break down by",
        )
    })?;
    let dimension = dimension_of(raw)?;
    let metrics = metrics_of(&params)?;

    let rows = stats::breakdown(&st.ch, &env, &dimension, &metrics).await?;
    let meta = json!({ "rows": rows.len(), "dimension": dimension.as_str() });
    Ok(ok_meta(json_body(&rows)?, meta))
}

// ---------------------------------------------------------------------------
// Realtime
// ---------------------------------------------------------------------------

/// How long a realtime reading is reused before it is taken again.
///
/// The live panel polls every three seconds and several people can have it open
/// at once. One second is short enough that the tape still moves at the rate the
/// eye reads it, and long enough that a room full of dashboards costs
/// ClickHouse one read per second per site rather than one per viewer per poll.
const REALTIME_CACHE_SECS: u32 = 1;

/// The cache key for a realtime reading.
///
/// The filters and the audience are part of the question, so they are part of
/// the key: two people watching the same site through different filters must not
/// be served each other's answer. The range deliberately is not — realtime is
/// its own window and the engine ignores the envelope's.
fn realtime_key(env: &QueryEnvelope) -> String {
    let mut hasher = DefaultHasher::new();
    env.audience.as_str().hash(&mut hasher);
    serde_json::to_string(&env.filters)
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("mf:cache:rt:{}:{:016x}", env.site, hasher.finish())
}

/// `GET /api/stats/realtime` — the last five minutes, both traces.
async fn realtime(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let key = realtime_key(&env);

    // A cold or broken Redis costs a ClickHouse read, never a failed request:
    // the cache is an optimisation and is treated as one in both directions.
    if let Ok(Some(cached)) = st.kv.get_json::<stats::Realtime>(&key).await {
        return Ok(ok(shape_realtime(&cached)));
    }

    let live = stats::realtime(&st.ch, &env).await?;
    if let Err(e) = st.kv.set_json(&key, &live, Some(REALTIME_CACHE_SECS)).await {
        tracing::debug!(error = %e, "realtime reading not cached");
    }

    Ok(ok(shape_realtime(&live)))
}

/// Shape a realtime reading for the live panel.
///
/// The side lists and the feed carry each value under the name the panel reads
/// it by *and* the name the engine gives it. They are the same number: `n` is
/// `visitors`, `path` is `pathname`, `agent` is `agent_id`. Two spellings of one
/// value is a small cost for not having a rename in the engine change what a
/// live tape shows.
fn shape_realtime(live: &stats::Realtime) -> Value {
    let pages: Vec<Value> = live
        .top_pages
        .iter()
        .map(|row| json!({ "key": row.key, "path": row.key, "visitors": row.visitors, "n": row.visitors }))
        .collect();

    let sources: Vec<Value> = live
        .top_sources
        .iter()
        .map(|row| json!({ "key": row.key, "source": row.key, "visitors": row.visitors, "n": row.visitors }))
        .collect();

    let recent: Vec<Value> = live
        .recent
        .iter()
        .map(|hit| {
            json!({
                "t": hit.t,
                "kind": hit.kind,
                "path": hit.pathname,
                "pathname": hit.pathname,
                // The feed's "source" column shows where the hit came from, and
                // the referring host is what the live window knows: resolving it
                // to a named source is the acquisition report's job, not the
                // tape's.
                "source": hit.referrer_host,
                "referrer_host": hit.referrer_host,
                "country": hit.country,
                "city": hit.city,
                "browser": hit.browser,
                "os": hit.os,
                "device_type": hit.device_type,
                "is_agent": hit.is_agent,
                "agent": hit.agent_id,
                "agent_id": hit.agent_id,
            })
        })
        .collect();

    json!({
        "visitors_now": live.visitors_now,
        "agents_now": live.agents_now,
        "per_minute": live.per_minute,
        "top_pages": pages,
        "top_sources": sources,
        "recent": recent,
    })
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// The page dimensions `/api/stats/pages` reports on.
const PAGE_DIMENSIONS: &[&str] = &["pathname", "entry_path", "exit_path"];

/// `GET /api/stats/pages` — pages with entries, exits, scroll and time.
///
/// Two readings sit behind one endpoint. The human record of `pathname` gets the
/// full page report, because entries, exits, scroll depth and engagement time
/// only exist for a person in a browser. Everything else — an entry or exit
/// path, or any path at all when the audience is machines — is a ranked
/// breakdown, because that is the honest shape of what is known.
async fn pages(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<StatsParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let raw = params.dimension.as_deref().unwrap_or("pathname");
    let dimension = dimension_within(raw, "the page report", PAGE_DIMENSIONS)?;

    if dimension == Dimension::Pathname && env.audience == Audience::Human {
        let rows = stats::pages(&st.ch, &env).await?;
        let total: f64 = rows.iter().map(|r| r.pageviews as f64).sum();

        let shaped: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "key": r.path,
                    "label": if r.title.is_empty() { &r.path } else { &r.title },
                    "pathname": r.path,
                    "title": r.title,
                    "pageviews": r.pageviews,
                    "visitors": r.visitors,
                    "entries": r.entries,
                    "exits": r.exits,
                    "bounce_rate": r.bounce_rate,
                    // Engagement on the page, under the name the page report
                    // gives it and the name every other ranked table gives it.
                    // Same number: see `micaforge_query::stats::pages` on how a
                    // hit's `duration_ms` is attributed to the page it measured.
                    "avg_time_s": r.avg_time_s,
                    "avg_duration_s": r.avg_time_s,
                    "avg_scroll_pct": r.avg_scroll_pct,
                    "share": share_of(r.pageviews as f64, total),
                })
            })
            .collect();

        let meta = json!({ "rows": shaped.len(), "dimension": "pathname" });
        return Ok(ok_meta(
            json!({ "dimension": "pathname", "rows": shaped }),
            meta,
        ));
    }

    let rows = stats::breakdown(&st.ch, &env, &dimension, &[Metric::Visitors]).await?;
    ranked(&dimension, &rows)
}

/// One ranked list, under the dimension it was ranked by.
///
/// The four narrowed reports all answer in this shape so a table can be pointed
/// at any of them without knowing which one it is reading.
fn ranked(dimension: &Dimension, rows: &[stats::BreakdownRow]) -> Result<Json<Value>, ApiError> {
    let meta = json!({ "rows": rows.len(), "dimension": dimension.as_str() });
    Ok(ok_meta(
        json!({ "dimension": dimension.as_str(), "rows": json_body(rows)? }),
        meta,
    ))
}

// ---------------------------------------------------------------------------
// Sources, locations, devices
// ---------------------------------------------------------------------------

/// The acquisition dimensions `/api/stats/sources` reports on.
const SOURCE_DIMENSIONS: &[&str] = &[
    "channel",
    "source",
    "referrer",
    "referrer_host",
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
];

/// The place dimensions `/api/stats/locations` reports on.
const LOCATION_DIMENSIONS: &[&str] = &["country", "region", "city"];

/// The client dimensions `/api/stats/devices` reports on.
const DEVICE_DIMENSIONS: &[&str] = &[
    "browser",
    "browser_version",
    "os",
    "os_version",
    "device_type",
    "screen_class",
    "language",
    "timezone",
];

/// Refuse a composite read that the composite cannot honestly answer.
///
/// The channel/source/campaign, country/region/city and browser/os/device trees
/// are built from the session rollup, and a session is a human idea: a crawler
/// has no session to bounce out of and no city. Rather than return a
/// human-shaped tree to a caller who asked for machines, name the dimensions
/// that *do* read the agent record.
fn require_dimension_for_agents(
    env: &QueryEnvelope,
    endpoint: &str,
    allowed: &[&str],
) -> Result<(), Error> {
    if env.audience == Audience::Human {
        return Ok(());
    }
    Err(Error::validation(
        "dimension",
        format!(
            "{endpoint} reports the agent record one dimension at a time; \
             pass dimension= one of {}",
            allowed.join(", ")
        ),
    ))
}

/// `GET /api/stats/sources` — channels, then sources, then campaigns.
async fn sources(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<StatsParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    match params.dimension.as_deref() {
        Some(raw) => {
            let dimension = dimension_within(raw, "the acquisition report", SOURCE_DIMENSIONS)?;
            let rows = stats::breakdown(&st.ch, &env, &dimension, &[Metric::Visitors]).await?;
            ranked(&dimension, &rows)
        }
        None => {
            require_dimension_for_agents(&env, "the acquisition report", SOURCE_DIMENSIONS)?;
            let tree = stats::sources(&st.ch, &env).await?;
            Ok(ok(json_body(&tree)?))
        }
    }
}

/// `GET /api/stats/locations` — country, region and city.
async fn locations(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<StatsParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    match params.dimension.as_deref() {
        Some(raw) => {
            let dimension = dimension_within(raw, "the location report", LOCATION_DIMENSIONS)?;
            let rows = stats::breakdown(&st.ch, &env, &dimension, &[Metric::Visitors]).await?;
            ranked(&dimension, &rows)
        }
        None => {
            require_dimension_for_agents(&env, "the location report", LOCATION_DIMENSIONS)?;
            let tree = stats::locations(&st.ch, &env).await?;
            Ok(ok(json_body(&tree)?))
        }
    }
}

/// `GET /api/stats/devices` — browser, operating system, device and screen.
async fn devices(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<StatsParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    match params.dimension.as_deref() {
        Some(raw) => {
            let dimension = dimension_within(raw, "the device report", DEVICE_DIMENSIONS)?;
            let rows = stats::breakdown(&st.ch, &env, &dimension, &[Metric::Visitors]).await?;
            ranked(&dimension, &rows)
        }
        None => {
            require_dimension_for_agents(&env, "the device report", DEVICE_DIMENSIONS)?;
            let tree = stats::devices(&st.ch, &env).await?;
            Ok(ok(json_body(&tree)?))
        }
    }
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

/// The five Core Web Vitals, in the order the readout strip prints them.
const VITALS: [&str; 5] = ["lcp", "cls", "inp", "fcp", "ttfb"];

/// `GET /api/stats/performance` — Core Web Vitals, whole-site or cut.
///
/// Without a dimension this is the site's own distribution: one set of
/// percentiles per vital, plus the same set over the comparison window when the
/// envelope asks for one. With a dimension it is those percentiles per value of
/// that dimension. The two are separate readings rather than one aggregated into
/// the other, because percentiles do not add up: a p75 over five pages is not
/// any function of the five p75s.
async fn performance(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<StatsParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    if let Some(raw) = params.dimension.as_deref() {
        let dimension = dimension_of(raw)?;
        let rows = stats::performance(&st.ch, &env, &dimension).await?;

        let shaped: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "key": r.key,
                    "label": r.key,
                    "samples": r.samples,
                    "rating": r.rating,
                    "metrics": {
                        "lcp": vital(r.samples, Some(r.lcp_p50), r.lcp_p75, Some(r.lcp_p95)),
                        "cls": vital(r.samples, None, r.cls_p75, None),
                        "inp": vital(r.samples, None, r.inp_p75, None),
                        "fcp": vital(r.samples, None, r.fcp_p75, None),
                        "ttfb": vital(r.samples, None, r.ttfb_p75, None),
                    },
                })
            })
            .collect();

        let meta = json!({ "rows": shaped.len(), "dimension": dimension.as_str() });
        return Ok(ok_meta(
            json!({ "dimension": dimension.as_str(), "rows": shaped }),
            meta,
        ));
    }

    let current = vitals_summary(&st, &env).await?;
    let compare = match previous_period(&env.range, env.compare) {
        Some(prev) => {
            let earlier = vitals_summary(&st, &envelope_over(&env, &prev)).await?;
            Some(json!({ "samples": earlier.0, "metrics": earlier.1 }))
        }
        None => None,
    };

    Ok(ok(json!({
        "samples": current.0,
        "metrics": current.1,
        "compare": compare,
    })))
}

/// One vital's statistic, in the shape the vitals distribution reads.
///
/// `null` rather than zero for a percentile that was not measured. Zero is a
/// reading — the fastest possible page — and printing it for "nobody reported
/// this" would grade a site that has not shipped the vitals bundle as perfect.
fn vital(samples: u64, p50: Option<f64>, p75: f64, p95: Option<f64>) -> Value {
    let some = |v: f64| if v > 0.0 { json!(v) } else { Value::Null };
    json!({
        "p50": p50.map(some).unwrap_or(Value::Null),
        "p75": some(p75),
        "p95": p95.map(some).unwrap_or(Value::Null),
        "samples": samples,
    })
}

/// The site's own vitals distribution: `(loads that reported one, per-vital
/// percentiles)`.
///
/// This is the one report in this module that builds its own statement. The
/// query engine reports vitals *by* a dimension, and there is no dimension whose
/// every row falls in one group, so a whole-site reading cannot be asked for
/// through it — and it cannot be derived from a per-dimension one either,
/// because percentiles do not aggregate. The statement follows the same rules
/// the engine does: the scope predicate comes from
/// [`micaforge_query::sql::scope_predicate`] so the window, the site and the
/// filters are compiled exactly as every other report compiles them, the
/// percentile expressions are this module's own text, and no caller value is
/// formatted into SQL.
///
/// It belongs in `micaforge_query::stats` as `performance_summary`; it is here
/// because that function does not exist yet.
async fn vitals_summary(st: &AppState, env: &QueryEnvelope) -> Result<(u64, Value), Error> {
    let (scope, scope_params) = scope_predicate(env, Table::Events)?;

    // One branch counts the loads that reported anything at all; one branch per
    // vital carries that vital's own percentiles. `UNION ALL` does not promise
    // an order, so the branches are matched by their label in Rust rather than
    // by position.
    let mut branches = vec![format!(
        "SELECT 'any' AS m, \
         toUInt64(countIf(isNotNull(lcp) OR isNotNull(cls) OR isNotNull(inp) \
                          OR isNotNull(fcp) OR isNotNull(ttfb))) AS n, \
         toFloat64(0) AS p50, toFloat64(0) AS p75, toFloat64(0) AS p95 \
         FROM events WHERE {scope}"
    )];

    for name in VITALS {
        branches.push(format!(
            "SELECT '{name}', toUInt64(countIf(isNotNull({name}))), {p50}, {p75}, {p95} \
             FROM events WHERE {scope}",
            p50 = f64_of(&format!("quantileTDigest(0.5)({name})")),
            p75 = f64_of(&format!("quantileTDigest(0.75)({name})")),
            p95 = f64_of(&format!("quantileTDigest(0.95)({name})")),
        ));
    }

    let sql = branches.join("\nUNION ALL\n");

    // Positional binding: each branch repeats the scope, so each branch repeats
    // the scope's parameters, in the order the branches were written.
    let mut params: Vec<Param> = Vec::with_capacity(scope_params.len() * branches.len());
    for _ in 0..branches.len() {
        params.extend(scope_params.iter().cloned());
    }

    let rows =
        fetch_all::<(String, u64, f64, f64, f64)>(&st.ch, "performance_summary", &sql, &params)
            .await?;

    let mut samples = 0u64;
    let mut metrics = serde_json::Map::new();
    for (name, n, p50, p75, p95) in rows {
        if name == "any" {
            samples = n;
            continue;
        }
        metrics.insert(name, vital(n, Some(p50), p75, Some(p95)));
    }

    Ok((samples, Value::Object(metrics)))
}

// ---------------------------------------------------------------------------
// Custom events
// ---------------------------------------------------------------------------

/// `GET /api/stats/events` — custom event names, or one event's properties.
///
/// `?event=<name>&properties=1` switches from the register of names to the
/// property drill-down for one of them. Both readings are scoped by the same
/// envelope, so opening a row does not quietly widen the window it was ranked
/// in.
async fn events(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<StatsParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let wants_properties = params
        .properties
        .as_deref()
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false);

    if let (true, Some(name)) = (wants_properties, params.event.as_deref()) {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::validation("event", "name the event to read properties of").into());
        }

        let properties = stats::event_properties(&st.ch, &env, name).await?;

        // The event's own total comes from the register rather than from the
        // property values: an event that carries no properties at all still
        // fired, and summing the values would report it as never having
        // happened.
        //
        // Asked for by name rather than looked up in the page of names the
        // caller's `limit` would have returned — a drill-down asks for eight
        // property values, and the event it is drilling into is not necessarily
        // in the top eight of anything.
        let mut one = env.clone();
        one.limit = 1;
        one.page = 1;
        one.filters = Some(narrowed_to(env.filters.as_ref(), name));

        let total = stats::events(&st.ch, &one)
            .await?
            .into_iter()
            .find(|row| row.name == name)
            .map(|row| row.count)
            .unwrap_or(0);

        let shaped: Vec<Value> = properties
            .iter()
            .map(|prop| {
                let seen: f64 = prop.values.iter().map(|v| v.count as f64).sum();
                json!({
                    "key": prop.key,
                    "distinct": prop.values.len(),
                    "values": prop.values.iter().map(|v| json!({
                        "value": v.value,
                        "count": v.count,
                        "visitors": v.visitors,
                        "share": share_of(v.count as f64, seen),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();

        return Ok(ok(json!({
            "event": name,
            "total": total,
            "properties": shaped,
        })));
    }

    let rows = stats::events(&st.ch, &env).await?;
    let total: u64 = rows.iter().map(|r| r.count).sum();

    // Sessions and distinct visitors are properties of the window, not of one
    // event name, so they come from the same totals card the overview draws.
    let totals = stats::overview(&st.ch, &env).await?;
    let per_session = if totals.sessions == 0 {
        0.0
    } else {
        total as f64 / totals.sessions as f64
    };

    let shaped: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "name": r.name,
                // Every row here came from `kind = 'custom'`; the register does
                // not mix pageviews and downloads into the same table.
                "kind": "custom",
                "count": r.count,
                "visitors": r.visitors,
                "share": share_of(r.count as f64, total as f64),
            })
        })
        .collect();

    let meta = json!({ "rows": shaped.len() });
    Ok(ok_meta(
        json!({
            "total": total,
            "distinct_names": rows.len(),
            "visitors": totals.visitors,
            "per_session": per_session,
            "rows": shaped,
        }),
        meta,
    ))
}
