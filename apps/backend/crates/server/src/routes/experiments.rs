//! `/api/flags/*` and `/api/experiments/*`.
//!
//! Two tables that look alike and answer different questions. A **flag** is a
//! runtime switch: is this on, for whom, at what percentage. An **experiment** is
//! a measurement: this hypothesis, these variants, *this goal*, this window.
//!
//! The goal is named when the experiment is created and cannot be swapped
//! afterwards, which is the point: choosing the metric after seeing the data is
//! how experiments lie, and an analytics tool that made it easy would be selling
//! a way to be wrong on purpose.
//!
//! ## Bucketing is a pure function, computed twice
//!
//! `GET /api/flags/evaluate` is the SDK's hot path, so it must give the same
//! answer every time with no state written anywhere.
//! [`bucket`] is `blake3(salt=flag key ‖ visitor) mod 10 000`, taken through
//! [`micaforge_core::crypto::visitor_hash`] so the tracker's identity primitive
//! and this one cannot drift apart. Rollout uses the flag's key as the salt;
//! variant assignment uses `"<key>:variant"`, a *different* salt, so a visitor
//! who lands inside a 10% rollout is not thereby biased toward the first
//! variant.
//!
//! ## What a flag condition may say
//!
//! Conditions are the filter shape from `docs/CONTRACT.md` §3.2, but they are
//! evaluated **here**, against the small context the SDK sends, rather than in
//! ClickHouse against an event row. Every operator is supported except
//! `matches`, which is refused at write time with a message saying so — silently
//! accepting a regular expression that would never be applied would be worse
//! than refusing it.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{Extensions, HeaderMap};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_core::{
    filters, new_id, Error, Filter, FilterGroup, FilterNode, FilterOp, Logic, Result,
};
use micaforge_query::{bind_all, GoalDef, Param};
use micaforge_store::models::{flags, goals, sites};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::client_ip::{address_tag, client_ip};
use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::middleware::LIMITER_BUDGET;
use crate::response::ok;
use crate::routes::orgs::{require_at_least, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Buckets a visitor can fall into. Ten thousand, so a rollout can be expressed
/// to a hundredth of a percent even though the stored field is whole percent.
const BUCKETS: u64 = 10_000;

/// How long the SDK-facing flag list is cached in Redis.
///
/// Thirty seconds. Long enough that a busy site does not read Postgres on every
/// page load, short enough that turning a flag off feels immediate. Writes
/// invalidate the key anyway, so the TTL only bounds the staleness of a change
/// made by another replica.
const FLAG_CACHE_TTL_SECS: u32 = 30;

/// Evaluations one address may make against one site per minute.
///
/// Keyed on the site *and* the caller's address tag. A single per-site bucket
/// let any stranger spend a site's whole allowance from one machine, after
/// which every real visitor's flags read as off. Six hundred a minute is ten
/// page loads a second from one address, the same allowance the beacon gets,
/// which a whole office behind one NAT stays well inside.
const EVAL_LIMIT: u32 = 600;
/// That window, in seconds.
const EVAL_WINDOW: u32 = 60;

/// The four states an experiment moves through.
const STATUSES: &[&str] = &["draft", "running", "paused", "finished"];

/// Below this many visitors in a variant, no significance is reported.
///
/// A z-test on thirty visitors will happily produce a confident-looking number,
/// and reporting it would be the most harmful thing this endpoint could do.
const MIN_VARIANT_VISITORS: u64 = 100;

/// The threshold at which a result is called significant.
const SIGNIFICANCE_P: f64 = 0.05;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every feature-flag and experiment route.
///
/// `GET /api/flags/evaluate` is the one route here with no authentication: it is
/// the SDK's own call and it must work from a browser on the tracked site.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/flags", get(list_flags).post(create_flag))
        .route("/api/flags/evaluate", get(evaluate))
        .route(
            "/api/flags/{id}",
            get(get_flag).patch(update_flag).delete(delete_flag),
        )
        .route("/api/flags/{id}/toggle", post(toggle_flag))
        .route(
            "/api/experiments",
            get(list_experiments).post(create_experiment),
        )
        .route(
            "/api/experiments/{id}",
            get(get_experiment)
                .patch(update_experiment)
                .delete(delete_experiment),
        )
        .route("/api/experiments/{id}/status", post(set_status))
        .route("/api/experiments/{id}/results", get(results))
}

// ---------------------------------------------------------------------------
// Feature flags
// ---------------------------------------------------------------------------

/// `GET /api/flags?site=…`
async fn list_flags(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = flags::list_flags(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(flag_json).collect::<Vec<_>>()))
}

/// A flag, as the editor sends it.
#[derive(Debug, Deserialize)]
struct FlagBody {
    /// The key the SDK asks for.
    key: String,
    #[serde(default)]
    enabled: Option<bool>,
    /// Percentage of eligible visitors the flag is on for.
    #[serde(default)]
    rollout_pct: Option<i32>,
    /// `[{ "key": "control", "weight": 50 }, …]`, or absent for an on/off flag.
    #[serde(default)]
    variants: Option<Value>,
    /// A filter group narrowing who is eligible.
    #[serde(default)]
    conditions: Option<Value>,
}

/// `POST /api/flags?site=…`
async fn create_flag(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<FlagBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let checked = check_flag(&body)?;

    if flags::get_flag_by_key(&st.pg, access.site.id, &checked.key)
        .await?
        .is_some()
    {
        return Err(
            Error::Conflict(format!("a flag named `{}` already exists", checked.key)).into(),
        );
    }

    let flag = flags::create_flag(
        &st.pg,
        new_id(),
        access.site.id,
        &checked.key,
        checked.enabled,
        checked.rollout_pct,
        &checked.variants,
        &checked.conditions,
    )
    .await?;

    invalidate_cache(&st, access.site_id).await;
    Ok(ok(flag_json(&flag)))
}

/// `GET /api/flags/{id}`
async fn get_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    reachable(&st, flag.site_id, auth.user.id, VIEWER, "feature flag").await?;
    Ok(ok(flag_json(&flag)))
}

/// `PATCH /api/flags/{id}`
async fn update_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<FlagBody>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    let site = reachable(&st, flag.site_id, auth.user.id, MEMBER, "feature flag").await?;
    let checked = check_flag(&body)?;

    if checked.key != flag.key {
        if let Some(other) = flags::get_flag_by_key(&st.pg, flag.site_id, &checked.key).await? {
            if other.id != flag.id {
                return Err(Error::Conflict(format!(
                    "a flag named `{}` already exists",
                    checked.key
                ))
                .into());
            }
        }
    }

    let updated = flags::update_flag(
        &st.pg,
        flag.id,
        &checked.key,
        checked.enabled,
        checked.rollout_pct,
        &checked.variants,
        &checked.conditions,
    )
    .await?;

    invalidate_cache(&st, site.site_id_u16()).await;
    Ok(ok(flag_json(&updated)))
}

/// The one field a kill switch changes.
#[derive(Debug, Deserialize)]
struct ToggleBody {
    enabled: bool,
}

/// `POST /api/flags/{id}/toggle` — flip it without touching anything else.
///
/// A separate endpoint from `PATCH` on purpose. Turning a flag off in an
/// incident must not require sending back the rollout and conditions, because
/// that is how somebody at three in the morning accidentally re-enables the
/// thing they were trying to switch off.
async fn toggle_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ToggleBody>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    let site = reachable(&st, flag.site_id, auth.user.id, MEMBER, "feature flag").await?;

    let updated = flags::set_flag_enabled(&st.pg, flag.id, body.enabled).await?;
    invalidate_cache(&st, site.site_id_u16()).await;

    tracing::info!(flag = %flag.id, key = %flag.key, enabled = body.enabled, "flag toggled");
    Ok(ok(flag_json(&updated)))
}

/// `DELETE /api/flags/{id}`
async fn delete_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    let site = reachable(&st, flag.site_id, auth.user.id, MEMBER, "feature flag").await?;

    flags::delete_flag(&st.pg, flag.id).await?;
    invalidate_cache(&st, site.site_id_u16()).await;
    Ok(ok(json!({ "deleted": flag.id })))
}

// ---------------------------------------------------------------------------
// Evaluation (public)
// ---------------------------------------------------------------------------

/// What the SDK sends to resolve its flags.
#[derive(Debug, Deserialize)]
struct EvaluateQuery {
    /// Numeric site id, as in the tracking snippet.
    site: u16,
    /// The visitor hash the tracker already holds. Absent means "no stable
    /// identity", and a partial rollout then evaluates to off.
    #[serde(default)]
    visitor: Option<String>,
    /// Resolve one flag rather than all of them.
    #[serde(default)]
    key: Option<String>,
    /// A JSON object of attributes for the conditions to match against, such as
    /// `{"country":"PT","device_type":"mobile"}`.
    #[serde(default)]
    context: Option<String>,
}

/// `GET /api/flags/evaluate` — the SDK's own call. No authentication.
///
/// Returns only what a browser is allowed to know: for each flag, whether it is
/// on and which variant. The conditions, the rollout percentage and every other
/// flag's configuration stay on the server, because the page this answers is a
/// page anybody can load.
///
/// **Known exposure:** without `key`, the answer lists every flag key the site
/// has, so a flag named after an unreleased feature is visible to anyone who
/// reads the site id from its page. The SDK resolves its flags in one call, so
/// the listing stays; a caller that wants one flag passes `key`, and a flag key
/// should not be a secret.
///
/// The limiter fails open, and fast: a Valkey that errors or is slower than
/// [`LIMITER_BUDGET`] must not turn every page view into flags read as off.
async fn evaluate(
    State(st): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Query(q): Query<EvaluateQuery>,
) -> ApiResult<Json<Value>> {
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let caller = address_tag(client_ip(&headers, peer));
    let allowed = tokio::time::timeout(
        LIMITER_BUDGET,
        st.kv.rate_limit(
            &format!("flag-eval:{}:{caller}", q.site),
            EVAL_LIMIT,
            EVAL_WINDOW,
        ),
    )
    .await;
    if let Ok(Ok(false)) = allowed {
        return Err(Error::RateLimited {
            retry_after: u64::from(EVAL_WINDOW),
        }
        .into());
    }

    let all = cached_flags(&st, q.site).await?;
    let visitor = q.visitor.unwrap_or_default();
    let context = parse_context(q.context.as_deref())?;

    let mut resolved = serde_json::Map::new();
    for flag in all {
        if let Some(wanted) = q.key.as_deref() {
            if flag.key != wanted.trim() {
                continue;
            }
        }
        resolved.insert(flag.key.clone(), decide(&flag, &visitor, &context));
    }

    Ok(ok(json!({
        "site": q.site,
        "flags": resolved,
        // The SDK caches on this, so it is part of the contract with the page.
        "ttl_seconds": FLAG_CACHE_TTL_SECS,
    })))
}

/// A flag as the evaluation path needs it: serialisable, so it can be cached.
///
/// The repository row is not `Serialize`, and giving it that derive would be a
/// change to another crate. Copying the five fields is cheaper than the coupling.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EvalFlag {
    key: String,
    enabled: bool,
    rollout_pct: i32,
    variants: Value,
    conditions: Value,
}

/// `mf:flags:<site>` — the SDK-facing flag list.
fn flag_cache_key(site: u16) -> String {
    format!("mf:flags:{site}")
}

/// Read a site's flags, through Redis.
///
/// A cache miss reads Postgres and repopulates. A *cache failure* falls through
/// to Postgres rather than failing the request: the flag list is derived data,
/// and a cold Redis must not turn every page load on a customer's site into an
/// error.
async fn cached_flags(st: &AppState, site: u16) -> Result<Vec<EvalFlag>> {
    let key = flag_cache_key(site);
    if let Ok(Some(hit)) = st.kv.get_json::<Vec<EvalFlag>>(&key).await {
        return Ok(hit);
    }

    let site_row = sites::get_by_site_id(&st.pg, site).await?;
    let rows: Vec<EvalFlag> = flags::list_flags(&st.pg, site_row.id)
        .await?
        .into_iter()
        .map(|f| EvalFlag {
            key: f.key,
            enabled: f.enabled,
            rollout_pct: f.rollout_pct,
            variants: f.variants,
            conditions: f.conditions,
        })
        .collect();

    if let Err(e) = st.kv.set_json(&key, &rows, Some(FLAG_CACHE_TTL_SECS)).await {
        tracing::debug!(site, error = %e, "could not cache the flag list");
    }
    Ok(rows)
}

/// Drop a site's cached flag list after a write.
///
/// Best-effort: the TTL bounds the damage of a failure to at most
/// [`FLAG_CACHE_TTL_SECS`], and failing an otherwise successful write because
/// Redis was unreachable would be the wrong trade.
async fn invalidate_cache(st: &AppState, site: u16) {
    if let Err(e) = st.kv.del(&flag_cache_key(site)).await {
        tracing::debug!(site, error = %e, "could not clear the flag cache");
    }
}

/// Resolve one flag for one visitor.
fn decide(flag: &EvalFlag, visitor: &str, context: &BTreeMap<String, String>) -> Value {
    if !flag.enabled {
        return json!({ "enabled": false, "variant": Value::Null, "reason": "disabled" });
    }

    // Conditions run before the rollout: a 50% rollout among mobile visitors is
    // half of the mobile visitors, not a quarter of everybody.
    if let Ok(group) = filters::parse_filters(&flag.conditions.to_string()) {
        if !group.is_empty() && !group_matches(&group, context) {
            return json!({ "enabled": false, "variant": Value::Null, "reason": "conditions" });
        }
    }

    let rollout = flag.rollout_pct.clamp(0, 100) as u64;
    if rollout < 100 {
        // With no stable identity there is no honest way to hold somebody in or
        // out of a partial rollout across page loads, so they stay out.
        if visitor.is_empty() {
            return json!({ "enabled": false, "variant": Value::Null, "reason": "no_visitor" });
        }
        if bucket(&flag.key, visitor) >= rollout * (BUCKETS / 100) {
            return json!({ "enabled": false, "variant": Value::Null, "reason": "rollout" });
        }
    }

    json!({
        "enabled": true,
        "variant": pick_variant(&flag.variants, &flag.key, visitor),
        "reason": "match",
    })
}

/// Which of `BUCKETS` a visitor falls into for one salt.
///
/// Reuses [`micaforge_core::crypto::visitor_hash`] so bucketing and visitor
/// identity share one hash. The site id is deliberately zero: the salt already
/// carries the flag key, and flag keys are per-site.
fn bucket(salt: &str, visitor: &str) -> u64 {
    let digest = micaforge_core::crypto::visitor_hash(salt.as_bytes(), 0, visitor, "");
    u64::from_str_radix(&digest, 16).unwrap_or(0) % BUCKETS
}

/// Choose a variant by weight, or `null` for an on/off flag.
///
/// Salted differently from the rollout, so falling inside a narrow rollout does
/// not correlate with landing in the first variant.
fn pick_variant(variants: &Value, key: &str, visitor: &str) -> Value {
    let Some(list) = variants.as_array().filter(|l| !l.is_empty()) else {
        return Value::Null;
    };

    let weights: Vec<(String, u64)> = list
        .iter()
        .filter_map(|v| {
            let name = v.get("key").and_then(Value::as_str)?;
            let weight = v.get("weight").and_then(Value::as_u64).unwrap_or(1);
            Some((name.to_string(), weight))
        })
        .collect();

    let total: u64 = weights.iter().map(|(_, w)| *w).sum();
    if total == 0 {
        return weights
            .first()
            .map(|(name, _)| json!(name))
            .unwrap_or(Value::Null);
    }

    let point = bucket(&format!("{key}:variant"), visitor) * total / BUCKETS;
    let mut seen = 0u64;
    for (name, weight) in &weights {
        seen += weight;
        if point < seen {
            return json!(name);
        }
    }
    // Only reachable through integer truncation at the very top of the range.
    weights
        .last()
        .map(|(name, _)| json!(name))
        .unwrap_or(Value::Null)
}

/// Parse the SDK's context object into a flat string map.
fn parse_context(raw: Option<&str>) -> Result<BTreeMap<String, String>> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|e| Error::validation("context", format!("is not JSON: {e}")))?;
    let Some(object) = parsed.as_object() else {
        return Err(Error::validation("context", "must be a JSON object"));
    };

    let mut out = BTreeMap::new();
    for (key, value) in object {
        let flat = match value {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        out.insert(key.trim().to_lowercase(), flat);
    }
    Ok(out)
}

/// Does a filter group hold for this context?
fn group_matches(group: &FilterGroup, context: &BTreeMap<String, String>) -> bool {
    match group.logic {
        Logic::And => group.filters.iter().all(|n| node_matches(n, context)),
        Logic::Or => group.filters.iter().any(|n| node_matches(n, context)),
    }
}

/// One node of a filter tree.
fn node_matches(node: &FilterNode, context: &BTreeMap<String, String>) -> bool {
    match node {
        FilterNode::Leaf(filter) => leaf_matches(filter, context),
        FilterNode::Group(group) => group_matches(group, context),
    }
}

/// One condition against the supplied context.
///
/// An attribute the SDK did not send is the empty string, which is what makes
/// `is_not_set` work and what makes `is` fail closed for an unknown attribute.
fn leaf_matches(filter: &Filter, context: &BTreeMap<String, String>) -> bool {
    let dimension = filter.dimension.as_str().to_lowercase();
    let actual = context.get(&dimension).cloned().unwrap_or_default();
    let wanted = filter.value.as_strings();
    let lower = actual.to_lowercase();

    match filter.op {
        FilterOp::Is => wanted.iter().any(|v| v.eq_ignore_ascii_case(&actual)),
        FilterOp::IsNot => !wanted.iter().any(|v| v.eq_ignore_ascii_case(&actual)),
        FilterOp::Contains => wanted.iter().any(|v| lower.contains(&v.to_lowercase())),
        FilterOp::NotContains => !wanted.iter().any(|v| lower.contains(&v.to_lowercase())),
        FilterOp::StartsWith => wanted.iter().any(|v| lower.starts_with(&v.to_lowercase())),
        FilterOp::EndsWith => wanted.iter().any(|v| lower.ends_with(&v.to_lowercase())),
        FilterOp::IsSet => !actual.is_empty(),
        FilterOp::IsNotSet => actual.is_empty(),
        FilterOp::Gt | FilterOp::Lt | FilterOp::Gte | FilterOp::Lte => {
            let (Some(left), Some(right)) =
                (actual.trim().parse::<f64>().ok(), filter.value.as_f64())
            else {
                return false;
            };
            match filter.op {
                FilterOp::Gt => left > right,
                FilterOp::Lt => left < right,
                FilterOp::Gte => left >= right,
                _ => left <= right,
            }
        }
        // Refused at write time; if one is somehow stored, it fails closed
        // rather than silently behaving like a different operator.
        FilterOp::Matches => false,
    }
}

// ---------------------------------------------------------------------------
// Experiments
// ---------------------------------------------------------------------------

/// `GET /api/experiments?site=…`
async fn list_experiments(
    State(st): State<AppState>,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = flags::list_experiments(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(experiment_json).collect::<Vec<_>>()))
}

/// An experiment, as the editor sends it.
#[derive(Debug, Deserialize)]
struct ExperimentBody {
    key: String,
    hypothesis: String,
    /// `[{ "key": "control", "weight": 50 }, { "key": "treatment", "weight": 50 }]`.
    variants: Value,
    /// The goal that decides the outcome.
    #[serde(default)]
    goal_id: Option<Uuid>,
}

/// `POST /api/experiments?site=…` — create one, in `draft`.
async fn create_experiment(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<ExperimentBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let (key, hypothesis, variants) = check_experiment(&body)?;
    check_goal_belongs(&st, body.goal_id, access.site.id).await?;

    let experiment = flags::create_experiment(
        &st.pg,
        new_id(),
        access.site.id,
        &key,
        &hypothesis,
        &variants,
        body.goal_id,
        "draft",
    )
    .await?;
    Ok(ok(experiment_json(&experiment)))
}

/// `GET /api/experiments/{id}`
async fn get_experiment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    reachable(&st, experiment.site_id, auth.user.id, VIEWER, "experiment").await?;
    Ok(ok(experiment_json(&experiment)))
}

/// `PATCH /api/experiments/{id}`
///
/// Refused once the experiment has started. Changing the variants or the goal
/// mid-flight would mean the data collected before the change and the data
/// collected after it are answering different questions, and the report would
/// average them into a number that means nothing.
async fn update_experiment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ExperimentBody>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    let site = reachable(&st, experiment.site_id, auth.user.id, MEMBER, "experiment").await?;

    if experiment.started_at.is_some() {
        return Err(Error::Conflict(
            "an experiment that has collected data cannot be redefined; finish it and start another"
                .into(),
        )
        .into());
    }

    let (key, hypothesis, variants) = check_experiment(&body)?;
    check_goal_belongs(&st, body.goal_id, site.id).await?;

    let updated = flags::update_experiment(
        &st.pg,
        experiment.id,
        &key,
        &hypothesis,
        &variants,
        body.goal_id,
    )
    .await?;
    Ok(ok(experiment_json(&updated)))
}

/// Where an experiment is being moved to.
#[derive(Debug, Deserialize)]
struct StatusBody {
    status: String,
}

/// `POST /api/experiments/{id}/status` — start, pause, resume or finish.
async fn set_status(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<StatusBody>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    reachable(&st, experiment.site_id, auth.user.id, MEMBER, "experiment").await?;

    let status = body.status.trim();
    if !STATUSES.contains(&status) {
        return Err(
            Error::validation("status", format!("must be one of {}", STATUSES.join(", "))).into(),
        );
    }
    if experiment.status == "finished" && status != "finished" {
        return Err(Error::Conflict(
            "a finished experiment cannot be restarted; its window has closed".into(),
        )
        .into());
    }
    if status == "running" && experiment.goal_id.is_none() {
        return Err(Error::validation(
            "goal_id",
            "an experiment needs a goal before it starts, so the metric cannot be chosen afterwards",
        )
        .into());
    }

    let updated = flags::set_experiment_status(&st.pg, experiment.id, status).await?;
    tracing::info!(experiment = %experiment.id, key = %experiment.key, status, "experiment moved");
    Ok(ok(experiment_json(&updated)))
}

/// `DELETE /api/experiments/{id}`
async fn delete_experiment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    reachable(&st, experiment.site_id, auth.user.id, MEMBER, "experiment").await?;
    flags::delete_experiment(&st.pg, experiment.id).await?;
    Ok(ok(json!({
        "deleted": experiment.id,
        "note": "the exposures stay on the event rows; only the definition is gone",
    })))
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// One variant's counts, as ClickHouse reports them.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct VariantRow {
    variant: String,
    visitors: u64,
    exposures: u64,
    conversions: u64,
}

/// `GET /api/experiments/{id}/results` — the computation hook.
///
/// **What is counted.** A visitor is *in* a variant when an event row carries
/// this experiment's key and that variant. A *conversion* is a row that both
/// carries the variant and satisfies the experiment's goal, which works because
/// the tracker stamps the active experiment and variant onto every event it
/// sends while the experiment is running. Both are `uniqExact` over
/// `visitor_id`, so one person converting twice is one conversion.
///
/// **The window** is `started_at` to `ended_at`, or to now for a running one. It
/// is never the caller's date range: an experiment's result is a property of the
/// experiment, not of whatever the reader happens to be looking at.
async fn results(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    let site = reachable(&st, experiment.site_id, auth.user.id, VIEWER, "experiment").await?;

    let Some(started) = experiment.started_at else {
        return Ok(ok(json!({
            "experiment": experiment_json(&experiment),
            "status": "not_started",
            "variants": Vec::<Value>::new(),
        })));
    };
    let ended = experiment.ended_at.unwrap_or_else(Utc::now);

    let goal = match experiment.goal_id {
        Some(goal_id) => Some(goals::get(&st.pg, goal_id).await?),
        None => None,
    };
    let (predicate, predicate_params) = match goal.as_ref() {
        Some(g) => GoalDef::parse(
            g.id.to_string(),
            g.name.as_str(),
            &g.kind,
            &g.conditions.to_string(),
            g.value_cents,
        )?
        .predicate()?,
        // No goal: the exposure counts are still real and worth showing, and a
        // conversion count of zero is honestly labelled by `goal: null`.
        None => ("1 = 0".to_string(), Vec::new()),
    };

    let rows = variant_counts(
        &st,
        site.site_id_u16(),
        &experiment.key,
        started,
        ended,
        &predicate,
        predicate_params,
    )
    .await?;

    let value_cents = goal.as_ref().map(|g| g.value_cents).unwrap_or(0);
    let control = choose_control(&rows);
    let baseline = control.and_then(|name| rows.iter().find(|r| r.variant == *name));

    let variants: Vec<Value> = rows
        .iter()
        .map(|row| variant_result(row, baseline, value_cents))
        .collect();

    Ok(ok(json!({
        "experiment": experiment_json(&experiment),
        "goal": goal.as_ref().map(|g| json!({
            "id": g.id,
            "name": g.name,
            "kind": g.kind,
            "value_cents": g.value_cents,
        })),
        "window": { "start": started, "end": ended },
        "control": control,
        "variants": variants,
        "significance_threshold": SIGNIFICANCE_P,
        "minimum_visitors_per_variant": MIN_VARIANT_VISITORS,
    })))
}

/// Read exposure and conversion counts per variant.
///
/// The goal predicate is interpolated into the `SELECT`, so its bind parameters
/// come **first**: ClickHouse binds positionally, and a list that is one out
/// does not fail cleanly, it silently answers a different question.
async fn variant_counts(
    st: &AppState,
    site: u16,
    experiment_key: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    predicate: &str,
    mut params: Vec<Param>,
) -> Result<Vec<VariantRow>> {
    let sql = format!(
        "SELECT
             variant,
             uniqExact(visitor_id) AS visitors,
             count() AS exposures,
             uniqExactIf(visitor_id, {predicate}) AS conversions
         FROM events
         WHERE site_id = ?
           AND experiment = ?
           AND variant != ''
           AND timestamp >= toDateTime64(?, 3, 'UTC')
           AND timestamp <  toDateTime64(?, 3, 'UTC')
         GROUP BY variant
         ORDER BY variant"
    );

    params.push(Param::U16(site));
    params.push(Param::Str(experiment_key.to_string()));
    params.push(Param::Str(micaforge_query::sql::fmt_ts(start)));
    params.push(Param::Str(micaforge_query::sql::fmt_ts(end)));

    bind_all(st.ch.client().query(&sql), &params)
        .fetch_all::<VariantRow>()
        .await
        .map_err(|e| Error::Database(format!("clickhouse experiment results: {e}")))
}

/// Which variant everything else is measured against.
///
/// A variant literally called `control`, otherwise the first alphabetically.
/// Something has to be the baseline, and picking the *best-performing* one would
/// manufacture a winner out of noise.
fn choose_control(rows: &[VariantRow]) -> Option<&String> {
    rows.iter()
        .find(|r| r.variant.eq_ignore_ascii_case("control"))
        .or_else(|| rows.first())
        .map(|r| &r.variant)
}

/// One variant's result, against the baseline.
fn variant_result(row: &VariantRow, baseline: Option<&VariantRow>, value_cents: i64) -> Value {
    let rate = ratio(row.conversions, row.visitors);
    let is_control = baseline.is_some_and(|b| b.variant == row.variant);

    let (lift, p_value, significant) = match baseline {
        Some(base) if !is_control => {
            let base_rate = ratio(base.conversions, base.visitors);
            let lift = if base_rate > 0.0 {
                Some((rate - base_rate) / base_rate)
            } else {
                None
            };
            // Below the floor no significance is reported at all. A p-value on
            // a handful of visitors is a confident-looking lie.
            let enough =
                row.visitors >= MIN_VARIANT_VISITORS && base.visitors >= MIN_VARIANT_VISITORS;
            let p = if enough {
                two_proportion_p(
                    base.conversions,
                    base.visitors,
                    row.conversions,
                    row.visitors,
                )
            } else {
                None
            };
            (lift, p, p.map(|p| p < SIGNIFICANCE_P))
        }
        _ => (None, None, None),
    };

    json!({
        "variant": row.variant,
        "is_control": is_control,
        "visitors": row.visitors,
        "exposures": row.exposures,
        "conversions": row.conversions,
        "conversion_rate": rate,
        "revenue_cents": row.conversions as i64 * value_cents,
        "lift": lift,
        "p_value": p_value,
        "significant": significant,
        "confidence": p_value.map(|p| 1.0 - p),
    })
}

/// A conversion rate, with an empty variant reported as zero rather than NaN.
fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Two-tailed p-value for a difference between two proportions.
///
/// The pooled two-proportion z-test: the standard test for "did this variant
/// convert at a different rate from the control", and the one every A/B
/// calculator on the internet implements, so the number here matches the number
/// an operator gets when they check it elsewhere.
///
/// `None` when the test is undefined, which happens when both arms converted at
/// exactly 0% or exactly 100% and there is no variance to test.
fn two_proportion_p(
    control_conversions: u64,
    control_visitors: u64,
    variant_conversions: u64,
    variant_visitors: u64,
) -> Option<f64> {
    if control_visitors == 0 || variant_visitors == 0 {
        return None;
    }
    let n1 = control_visitors as f64;
    let n2 = variant_visitors as f64;
    let p1 = control_conversions as f64 / n1;
    let p2 = variant_conversions as f64 / n2;

    let pooled = (control_conversions + variant_conversions) as f64 / (n1 + n2);
    let variance = pooled * (1.0 - pooled) * (1.0 / n1 + 1.0 / n2);
    if variance <= 0.0 {
        return None;
    }

    let z = (p2 - p1) / variance.sqrt();
    Some((2.0 * (1.0 - normal_cdf(z.abs()))).clamp(0.0, 1.0))
}

/// The standard normal cumulative distribution.
fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

/// Abramowitz and Stegun 7.1.26.
///
/// Maximum absolute error 1.5e-7, which is four orders of magnitude finer than
/// any p-value this endpoint reports to two decimal places. Written out rather
/// than pulled from a statistics crate because it is nine lines and the
/// dependency would be the larger cost.
fn erf(x: f64) -> f64 {
    const A1: f64 = 0.254_829_592;
    const A2: f64 = -0.284_496_736;
    const A3: f64 = 1.421_413_741;
    const A4: f64 = -1.453_152_027;
    const A5: f64 = 1.061_405_429;
    const P: f64 = 0.327_591_1;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + P * x);
    let y = 1.0 - (((((A5 * t + A4) * t) + A3) * t + A2) * t + A1) * t * (-x * x).exp();
    sign * y
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A flag body that has been checked.
#[derive(Debug)]
struct CheckedFlag {
    key: String,
    enabled: bool,
    rollout_pct: i32,
    variants: Value,
    conditions: Value,
}

/// Validate a flag body.
fn check_flag(body: &FlagBody) -> Result<CheckedFlag> {
    let key = clean_key(&body.key, "key")?;
    let rollout = body.rollout_pct.unwrap_or(100);
    if !(0..=100).contains(&rollout) {
        return Err(Error::validation(
            "rollout_pct",
            "must be between 0 and 100",
        ));
    }

    let variants = body.variants.clone().unwrap_or_else(|| json!([]));
    check_variants(&variants, false)?;

    let conditions = body.conditions.clone().unwrap_or_else(|| json!({}));
    let group = filters::parse_filters(&conditions.to_string())
        .map_err(|e| Error::validation("conditions", e.to_string()))?;
    group
        .validate()
        .map_err(|e| Error::validation("conditions", e.to_string()))?;
    refuse_unevaluable(&group)?;

    Ok(CheckedFlag {
        key,
        enabled: body.enabled.unwrap_or(true),
        rollout_pct: rollout,
        variants,
        conditions,
    })
}

/// Validate an experiment body.
fn check_experiment(body: &ExperimentBody) -> Result<(String, String, Value)> {
    let key = clean_key(&body.key, "key")?;
    let hypothesis = body.hypothesis.trim();
    if hypothesis.is_empty() {
        return Err(Error::validation(
            "hypothesis",
            "say what you expect to happen; a result with no prediction proves nothing",
        ));
    }
    if hypothesis.chars().count() > 1000 {
        return Err(Error::validation("hypothesis", "is too long"));
    }

    check_variants(&body.variants, true)?;
    Ok((key, hypothesis.to_string(), body.variants.clone()))
}

/// A flag or experiment key: lower-case, url and code safe.
fn clean_key(raw: &str, field: &str) -> Result<String> {
    let key = raw.trim().to_lowercase();
    let usable = (1..=64).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !usable {
        return Err(Error::validation(
            field,
            "must be 1 to 64 characters of letters, digits, hyphens, underscores or dots",
        ));
    }
    Ok(key)
}

/// Validate a variant list.
///
/// `required` is true for an experiment, which is meaningless with fewer than
/// two arms, and false for a flag, where an empty list means a plain on/off
/// switch.
fn check_variants(variants: &Value, required: bool) -> Result<()> {
    let Some(list) = variants.as_array() else {
        return Err(Error::validation("variants", "must be an array"));
    };
    if list.is_empty() {
        if required {
            return Err(Error::validation(
                "variants",
                "an experiment needs at least two variants to compare",
            ));
        }
        return Ok(());
    }
    if required && list.len() < 2 {
        return Err(Error::validation(
            "variants",
            "an experiment needs at least two variants to compare",
        ));
    }

    let mut seen: Vec<String> = Vec::with_capacity(list.len());
    let mut total = 0u64;
    for entry in list {
        let name = entry
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .ok_or_else(|| Error::validation("variants", "every variant needs a `key`"))?;
        if seen.iter().any(|s| s == name) {
            return Err(Error::validation(
                "variants",
                format!("`{name}` appears twice"),
            ));
        }
        seen.push(name.to_string());
        total += entry.get("weight").and_then(Value::as_u64).unwrap_or(1);
    }

    if total == 0 {
        return Err(Error::validation(
            "variants",
            "at least one variant needs a weight above zero",
        ));
    }
    Ok(())
}

/// Refuse a condition this endpoint could not honestly evaluate.
///
/// `matches` compiles to a ClickHouse RE2 call in a report. Flag evaluation runs
/// in this process against a handful of attributes, so accepting one here would
/// store a rule that silently never fires.
fn refuse_unevaluable(group: &FilterGroup) -> Result<()> {
    for node in &group.filters {
        match node {
            FilterNode::Leaf(filter) if filter.op == FilterOp::Matches => {
                return Err(Error::validation(
                    "conditions",
                    "`matches` cannot be used in a flag condition, because flags are evaluated \
                     against the request rather than against the event store",
                ));
            }
            FilterNode::Group(inner) => refuse_unevaluable(inner)?,
            FilterNode::Leaf(_) => {}
        }
    }
    Ok(())
}

/// Refuse a goal that belongs to another site.
///
/// An experiment judged against a goal on somebody else's site would report a
/// conversion rate assembled from two different populations.
async fn check_goal_belongs(st: &AppState, goal_id: Option<Uuid>, site_id: Uuid) -> Result<()> {
    let Some(goal_id) = goal_id else {
        return Ok(());
    };
    let goal = goals::get(&st.pg, goal_id).await?;
    if goal.site_id != site_id {
        return Err(Error::validation("goal_id", "belongs to another site"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shaping and shared authorisation
// ---------------------------------------------------------------------------

/// One flag on the wire, for an authenticated reader.
fn flag_json(flag: &flags::FeatureFlag) -> Value {
    json!({
        "id": flag.id,
        "site_id": flag.site_id,
        "key": flag.key,
        "enabled": flag.enabled,
        "rollout_pct": flag.rollout_pct,
        "variants": flag.variants,
        "conditions": flag.conditions,
    })
}

/// One experiment on the wire.
fn experiment_json(experiment: &flags::Experiment) -> Value {
    json!({
        "id": experiment.id,
        "site_id": experiment.site_id,
        "key": experiment.key,
        "hypothesis": experiment.hypothesis,
        "variants": experiment.variants,
        "goal_id": experiment.goal_id,
        "status": experiment.status,
        "started_at": experiment.started_at,
        "ended_at": experiment.ended_at,
        "editable": experiment.started_at.is_none(),
    })
}

/// Assert the caller may act on an object belonging to `site_id`, and hand back
/// the site so the caller can reach its numeric id.
async fn reachable(
    st: &AppState,
    site_id: Uuid,
    user_id: Uuid,
    needed: &str,
    what: &str,
) -> Result<micaforge_store::models::sites::Site> {
    match authorize_site(&st.pg, site_id, user_id, needed).await {
        Ok(site) => Ok(site),
        Err(Error::NotFound(_)) => Err(Error::not_found(what.to_string())),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(rollout: i32, variants: Value, conditions: Value) -> EvalFlag {
        EvalFlag {
            key: "new-nav".into(),
            enabled: true,
            rollout_pct: rollout,
            variants,
            conditions,
        }
    }

    #[test]
    fn bucketing_is_stable_and_salted_per_flag() {
        let a = bucket("new-nav", "visitor-1");
        assert_eq!(a, bucket("new-nav", "visitor-1"), "must not drift per call");
        assert_ne!(a, bucket("other-flag", "visitor-1"), "salted by flag key");
        assert!(a < BUCKETS);

        // The variant salt is independent of the rollout salt, so being inside a
        // narrow rollout does not bias which arm you land in.
        assert_ne!(
            bucket("new-nav", "visitor-1"),
            bucket("new-nav:variant", "visitor-1")
        );
    }

    #[test]
    fn a_full_rollout_reaches_everybody_and_a_zero_rollout_nobody() {
        let all = flag(100, json!([]), json!({}));
        let none = flag(0, json!([]), json!({}));
        let context = BTreeMap::new();

        for visitor in ["a", "b", "c", "d", "e"] {
            assert_eq!(decide(&all, visitor, &context)["enabled"], json!(true));
            assert_eq!(decide(&none, visitor, &context)["enabled"], json!(false));
        }
    }

    #[test]
    fn a_partial_rollout_splits_roughly_where_it_says() {
        let half = flag(50, json!([]), json!({}));
        let context = BTreeMap::new();

        let on = (0..2_000)
            .filter(|i| decide(&half, &format!("visitor-{i}"), &context)["enabled"] == json!(true))
            .count();
        // Binomial noise on 2000 draws is a few percent; anything outside this
        // band means the hash is not uniform.
        assert!(
            (900..1_100).contains(&on),
            "50% rollout reached {on} of 2000"
        );
    }

    #[test]
    fn a_visitor_with_no_identity_stays_out_of_a_partial_rollout() {
        let context = BTreeMap::new();
        assert_eq!(
            decide(&flag(50, json!([]), json!({})), "", &context)["enabled"],
            json!(false)
        );
        // A full rollout still applies: there is nothing to hold stable.
        assert_eq!(
            decide(&flag(100, json!([]), json!({})), "", &context)["enabled"],
            json!(true)
        );
    }

    #[test]
    fn a_disabled_flag_is_off_whatever_else_says() {
        let mut f = flag(100, json!([]), json!({}));
        f.enabled = false;
        let decided = decide(&f, "visitor-1", &BTreeMap::new());
        assert_eq!(decided["enabled"], json!(false));
        assert_eq!(decided["reason"], json!("disabled"));
    }

    #[test]
    fn conditions_are_matched_against_the_supplied_context() {
        let mobile = flag(
            100,
            json!([]),
            json!([{ "dimension": "device_type", "op": "is", "value": "mobile" }]),
        );

        let mut context = BTreeMap::new();
        context.insert("device_type".into(), "mobile".into());
        assert_eq!(decide(&mobile, "v1", &context)["enabled"], json!(true));

        context.insert("device_type".into(), "desktop".into());
        assert_eq!(
            decide(&mobile, "v1", &context)["reason"],
            json!("conditions")
        );

        // An attribute the SDK never sent fails closed rather than matching.
        assert_eq!(
            decide(&mobile, "v1", &BTreeMap::new())["reason"],
            json!("conditions")
        );
    }

    #[test]
    fn variants_are_assigned_by_weight() {
        let variants = json!([
            { "key": "control", "weight": 50 },
            { "key": "treatment", "weight": 50 }
        ]);

        let mut treatment = 0;
        for i in 0..2_000 {
            let picked = pick_variant(&variants, "exp", &format!("visitor-{i}"));
            assert!(picked == json!("control") || picked == json!("treatment"));
            if picked == json!("treatment") {
                treatment += 1;
            }
        }
        assert!(
            (900..1_100).contains(&treatment),
            "50/50 split gave {treatment}"
        );

        // A weight of zero must never be assigned.
        let skewed = json!([
            { "key": "on", "weight": 1 },
            { "key": "never", "weight": 0 }
        ]);
        for i in 0..200 {
            assert_eq!(pick_variant(&skewed, "exp", &format!("v{i}")), json!("on"));
        }
    }

    #[test]
    fn an_on_off_flag_has_no_variant() {
        assert_eq!(pick_variant(&json!([]), "exp", "v1"), Value::Null);
        assert_eq!(pick_variant(&Value::Null, "exp", "v1"), Value::Null);
    }

    #[test]
    fn a_variant_assignment_never_moves_for_one_visitor() {
        let variants = json!([
            { "key": "a", "weight": 1 },
            { "key": "b", "weight": 1 },
            { "key": "c", "weight": 1 }
        ]);
        let first = pick_variant(&variants, "exp", "visitor-7");
        for _ in 0..50 {
            assert_eq!(pick_variant(&variants, "exp", "visitor-7"), first);
        }
    }

    #[test]
    fn the_normal_tail_matches_the_textbook() {
        // Values every statistics table carries, to four decimal places.
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-6);
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-3);
        assert!((normal_cdf(-1.96) - 0.025).abs() < 1e-3);
        assert!(erf(0.0).abs() < 1e-7);
    }

    #[test]
    fn a_clear_win_is_significant_and_a_tie_is_not() {
        // 10% against 15% on a thousand each: a textbook significant result.
        let p = two_proportion_p(100, 1_000, 150, 1_000).unwrap();
        assert!(p < SIGNIFICANCE_P, "p was {p}");

        // Identical rates: nothing to see.
        let tie = two_proportion_p(100, 1_000, 100, 1_000).unwrap();
        assert!(tie > 0.9, "p was {tie}");

        // No variance at all: the test is undefined rather than certain.
        assert!(two_proportion_p(0, 500, 0, 500).is_none());
        assert!(two_proportion_p(0, 0, 0, 0).is_none());
    }

    #[test]
    fn a_small_variant_reports_no_significance_at_all() {
        let control = VariantRow {
            variant: "control".into(),
            visitors: 20,
            exposures: 30,
            conversions: 2,
        };
        let treatment = VariantRow {
            variant: "treatment".into(),
            visitors: 20,
            exposures: 31,
            conversions: 8,
        };

        let shaped = variant_result(&treatment, Some(&control), 0);
        assert_eq!(
            shaped["p_value"],
            Value::Null,
            "20 visitors cannot be significant"
        );
        assert_eq!(shaped["significant"], Value::Null);
        // The counts themselves are still reported honestly.
        assert_eq!(shaped["conversions"], json!(8));
    }

    #[test]
    fn the_control_is_chosen_by_name_not_by_performance() {
        let rows = vec![
            VariantRow {
                variant: "a-treatment".into(),
                visitors: 10,
                exposures: 10,
                conversions: 9,
            },
            VariantRow {
                variant: "control".into(),
                visitors: 10,
                exposures: 10,
                conversions: 1,
            },
        ];
        assert_eq!(choose_control(&rows).unwrap(), "control");

        // With nothing named `control`, the first alphabetically is the
        // baseline, because ClickHouse returned them ordered by variant.
        let unnamed = vec![
            VariantRow {
                variant: "blue".into(),
                visitors: 10,
                exposures: 10,
                conversions: 1,
            },
            VariantRow {
                variant: "green".into(),
                visitors: 10,
                exposures: 10,
                conversions: 9,
            },
        ];
        assert_eq!(choose_control(&unnamed).unwrap(), "blue");
        assert!(choose_control(&[]).is_none());
    }

    #[test]
    fn a_regex_condition_is_refused_at_write_time() {
        let body = FlagBody {
            key: "new-nav".into(),
            enabled: None,
            rollout_pct: None,
            variants: None,
            conditions: Some(
                json!([{ "dimension": "pathname", "op": "matches", "value": "^/docs" }]),
            ),
        };
        assert_eq!(check_flag(&body).unwrap_err().field(), Some("conditions"));
    }

    #[test]
    fn keys_and_rollouts_are_bounded() {
        let base = |key: &str, rollout: Option<i32>| FlagBody {
            key: key.into(),
            enabled: None,
            rollout_pct: rollout,
            variants: None,
            conditions: None,
        };
        assert_eq!(
            check_flag(&base("New Nav", None)).unwrap_err().field(),
            Some("key")
        );
        assert_eq!(
            check_flag(&base("", None)).unwrap_err().field(),
            Some("key")
        );
        assert_eq!(
            check_flag(&base("new-nav", Some(101))).unwrap_err().field(),
            Some("rollout_pct")
        );
        assert_eq!(
            check_flag(&base("New-Nav.v2", None)).unwrap().key,
            "new-nav.v2"
        );
    }

    #[test]
    fn an_experiment_needs_two_distinct_weighted_variants() {
        assert!(check_variants(&json!([]), true).is_err());
        assert!(check_variants(&json!([{ "key": "only" }]), true).is_err());
        assert!(check_variants(
            &json!([{ "key": "a", "weight": 1 }, { "key": "a", "weight": 1 }]),
            true
        )
        .is_err());
        assert!(check_variants(
            &json!([{ "key": "a", "weight": 0 }, { "key": "b", "weight": 0 }]),
            true
        )
        .is_err());
        assert!(check_variants(
            &json!([{ "key": "a", "weight": 1 }, { "key": "b", "weight": 3 }]),
            true
        )
        .is_ok());

        // A flag is allowed to have none: that is an on/off switch.
        assert!(check_variants(&json!([]), false).is_ok());
    }

    #[test]
    fn context_values_are_flattened_and_lower_cased() {
        let parsed = parse_context(Some(r#"{"Device_Type":"mobile","plan":2,"x":null}"#)).unwrap();
        assert_eq!(
            parsed.get("device_type").map(String::as_str),
            Some("mobile")
        );
        assert_eq!(parsed.get("plan").map(String::as_str), Some("2"));
        assert_eq!(parsed.get("x").map(String::as_str), Some(""));

        assert!(parse_context(None).unwrap().is_empty());
        assert!(parse_context(Some("not json")).is_err());
        assert!(parse_context(Some("[1,2]")).is_err());
    }
}
