//! The public side of feature flags: `GET /api/flags/evaluate` and
//! `POST /api/flags/events`.
//!
//! Both are called from a stranger's browser on the tracked site (and from a
//! customer's server), with no account, so they live in the collection group
//! beside `routes::ingest` rather than behind the API's origin allow-list: a
//! flag the browser could not read cross-origin would be a flag that only
//! worked on the dashboard's own domain. They write their own CORS headers,
//! `*` and no credentials, for the reason `routes::ingest` explains.
//!
//! **Evaluate** answers only decisions: for each flag, on or off, which
//! variant, and why. The conditions, the rollout, the target groups and every
//! other part of a definition stay on the server, because anybody can load the
//! page that asks. The rule is [`micaforge_core::flags`], the same one the
//! SDKs run on a ruleset.
//!
//! **Events** takes the evaluation and exposure counts the SDKs aggregate and
//! send in batches, and queues them for the `flag_events` table. A count for a
//! key the site does not have is dropped, so a stranger cannot fill the table
//! with invented flags, and every batch is bounded in entries, in count per
//! entry and by a per-address limit.

use std::net::SocketAddr;

use axum::body::to_bytes;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::Utc;
use micaforge_core::flags::{check_environment_name, EvalInput, Ruleset};
use micaforge_core::{Error, Result};
use micaforge_store::ch::FlagEventRow;
use micaforge_store::models::sites;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::client_ip::{address_tag, client_ip};
use crate::error::ApiError;
use crate::middleware::LIMITER_BUDGET;
use crate::response::ok;
use crate::routes::flags::{build_ruleset, ruleset_cache_key, RULESET_TTL_SECS};
use crate::state::AppState;

/// Evaluations one address may make against one site per minute.
///
/// Keyed on the site *and* the caller's address tag. A single per-site bucket
/// let any stranger spend a site's whole allowance from one machine, after
/// which every real visitor's flags read as off. Six hundred a minute is ten
/// page loads a second from one address, the same allowance the beacon gets,
/// which a whole office behind one NAT stays well inside.
const EVAL_LIMIT: u32 = 600;

/// Stats batches one address may send to one site per minute. An SDK sends
/// one every ten seconds or so; a browser one per page.
const EVENTS_LIMIT: u32 = 300;

/// The window of both limits, in seconds.
const LIMIT_WINDOW: u32 = 60;

/// Largest stats batch body.
const MAX_EVENTS_BODY: usize = 64 * 1024;

/// Most entries one batch may carry.
const MAX_EVENTS: usize = 200;

/// Most evaluations or exposures one entry may stand for. A Node process
/// evaluating in a hot loop for ten seconds can honestly reach thousands; past
/// this the count is clamped rather than trusted.
const MAX_COUNT: u32 = 100_000;

/// Longest variant key kept, in characters.
const MAX_VARIANT: usize = 64;

/// Every public flag route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/flags/evaluate", get(evaluate).options(preflight))
        .route(
            "/api/flags/events",
            axum::routing::post(events).options(preflight),
        )
}

/// The CORS preflight. `navigator.sendBeacon` with a `text/plain` body never
/// preflights; a `fetch` with JSON from a browser does.
async fn preflight() -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    open_cors(res.headers_mut());
    res.headers_mut().insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    res.headers_mut().insert(
        "access-control-allow-headers",
        HeaderValue::from_static("content-type"),
    );
    res.headers_mut()
        .insert("access-control-max-age", HeaderValue::from_static("86400"));
    res
}

/// `*`, and no credentials: these routes read no cookie.
fn open_cors(headers: &mut HeaderMap) {
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
}

/// An answer with the open CORS header on it, whatever it is.
fn with_cors(res: impl IntoResponse) -> Response {
    let mut res = res.into_response();
    open_cors(res.headers_mut());
    res
}

// ---------------------------------------------------------------------------
// Evaluate
// ---------------------------------------------------------------------------

/// What the SDK sends to resolve its flags.
#[derive(Debug, Deserialize)]
struct EvaluateQuery {
    /// Numeric site id, as in the tracking snippet.
    site: u16,
    /// The visitor id the SDK holds. Absent, with no identity either, means
    /// "no stable subject", and a partial rollout then evaluates to off.
    #[serde(default)]
    visitor: Option<String>,
    /// The identified id, which buckets ahead of the visitor when present.
    #[serde(default)]
    identity: Option<String>,
    /// Resolve one flag rather than all of them.
    #[serde(default)]
    key: Option<String>,
    /// The environment the page runs in.
    #[serde(default)]
    environment: Option<String>,
    /// A JSON object of attributes for the conditions to match against, such as
    /// `{"country":"PT","plan":"pro"}`.
    #[serde(default)]
    context: Option<String>,
}

/// `GET /api/flags/evaluate` — the browser SDK's own call. No authentication.
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
) -> Response {
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let caller = address_tag(client_ip(&headers, peer));
    if !allowed(&st, &format!("flag-eval:{}:{caller}", q.site), EVAL_LIMIT).await {
        return with_cors(ApiError::from(Error::RateLimited {
            retry_after: u64::from(LIMIT_WINDOW),
        }));
    }
    match decide_all(&st, &q).await {
        Ok(body) => with_cors(ok(body)),
        Err(e) => with_cors(ApiError::from(e)),
    }
}

/// The decisions for one request.
async fn decide_all(st: &AppState, q: &EvaluateQuery) -> Result<Value> {
    let rules = cached_rules(st, q.site).await?;
    let context = parse_context(q.context.as_deref())?;
    let subject = q
        .identity
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(q.visitor.as_deref().map(str::trim))
        .unwrap_or("");
    let input = EvalInput::new(subject, &context, q.environment.as_deref());

    let mut resolved = Map::new();
    match q.key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        Some(wanted) => {
            let wanted = wanted.to_lowercase();
            if rules.keys().any(|k| *k == wanted) {
                resolved.insert(wanted.clone(), rules.evaluate(&wanted, &input).to_json());
            }
        }
        None => {
            for (key, decision) in rules.evaluate_all(&input) {
                resolved.insert(key, decision.to_json());
            }
        }
    }
    Ok(json!({
        "site": q.site,
        "flags": resolved,
        // The SDK caches on this, so it is part of the contract with the page.
        "ttl_seconds": RULESET_TTL_SECS,
    }))
}

/// A site's ruleset, through Valkey.
///
/// A cache miss reads Postgres and repopulates. A *cache failure* falls through
/// to Postgres rather than failing the request: the ruleset is derived data,
/// and a cold Valkey must not turn every page load on a customer's site into an
/// error. An unknown site is `404`.
async fn cached_rules(st: &AppState, site: u16) -> Result<Ruleset> {
    let key = ruleset_cache_key(site);
    if let Ok(Some(doc)) = st.kv.get_json::<Value>(&key).await {
        return Ok(Ruleset::from_json(&doc));
    }
    let row = sites::get_by_site_id(&st.pg, site).await?;
    let doc = build_ruleset(st, row.id).await?;
    if let Err(e) = st.kv.set_json(&key, &doc, Some(RULESET_TTL_SECS)).await {
        tracing::debug!(site, error = %e, "could not cache the flag ruleset");
    }
    Ok(Ruleset::from_json(&doc))
}

/// Parse the SDK's context object.
fn parse_context(raw: Option<&str>) -> Result<Map<String, Value>> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Map::new());
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(Error::validation("context", "must be a JSON object")),
        Err(e) => Err(Error::validation("context", format!("is not JSON: {e}"))),
    }
}

/// The per-address limiter, failing open on a slow or absent Valkey.
async fn allowed(st: &AppState, key: &str, limit: u32) -> bool {
    !matches!(
        tokio::time::timeout(LIMITER_BUDGET, st.kv.rate_limit(key, limit, LIMIT_WINDOW)).await,
        Ok(Ok(false))
    )
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// One batch from an SDK.
#[derive(Debug, Deserialize)]
struct Batch {
    site: u16,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    events: Vec<Value>,
}

/// `POST /api/flags/events` — counts from an SDK. Answers `204` for anything
/// it could read, so a browser console never shows an error for stats.
async fn events(State(st): State<AppState>, req: Request) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let caller = address_tag(client_ip(req.headers(), peer));
    let Ok(body) = to_bytes(req.into_body(), MAX_EVENTS_BODY).await else {
        return with_cors(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let Ok(batch) = serde_json::from_slice::<Batch>(&body) else {
        return with_cors(StatusCode::NO_CONTENT);
    };
    if !allowed(
        &st,
        &format!("flag-events:{}:{caller}", batch.site),
        EVENTS_LIMIT,
    )
    .await
    {
        return with_cors(ApiError::from(Error::RateLimited {
            retry_after: u64::from(LIMIT_WINDOW),
        }));
    }
    let Ok(rules) = cached_rules(&st, batch.site).await else {
        return with_cors(StatusCode::NO_CONTENT);
    };

    for row in rows_of(&batch, &rules) {
        st.writer.push_flag(row);
    }
    with_cors(StatusCode::NO_CONTENT)
}

/// The rows a batch stands for: entries for keys the site has, with a known
/// kind, bounded, and labelled the way the stats screen groups them.
fn rows_of(batch: &Batch, rules: &Ruleset) -> Vec<FlagEventRow> {
    let environment = batch
        .environment
        .as_deref()
        .and_then(|e| check_environment_name(e).ok())
        .unwrap_or_default();
    let source = match batch.source.as_deref() {
        Some("server") => "server",
        _ => "browser",
    };
    let now = Utc::now();

    batch
        .events
        .iter()
        .take(MAX_EVENTS)
        .filter_map(|entry| {
            let key = entry
                .get("key")
                .and_then(Value::as_str)?
                .trim()
                .to_lowercase();
            if !rules.keys().any(|k| *k == key) {
                return None;
            }
            let kind = match entry.get("kind").and_then(Value::as_str)? {
                "evaluation" => "evaluation",
                "exposure" => "exposure",
                _ => return None,
            };
            let enabled = entry
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let variant = entry
                .get("variant")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let label = match (enabled, variant) {
                (false, _) => "off".to_string(),
                (true, Some(v)) => v.chars().take(MAX_VARIANT).collect(),
                (true, None) => "on".to_string(),
            };
            let count = entry
                .get("count")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, u64::from(MAX_COUNT)) as u32;
            Some(FlagEventRow {
                site_id: batch.site,
                timestamp: now,
                flag_key: key,
                variant: label,
                enabled: u8::from(enabled),
                kind: kind.to_string(),
                source: source.to_string(),
                environment: environment.clone(),
                count,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Ruleset {
        Ruleset::from_json(&json!({ "flags": [
            { "key": "new-nav", "enabled": true },
            { "key": "hero", "enabled": true, "variants": [{ "key": "a" }, { "key": "b" }] }
        ]}))
    }

    #[test]
    fn a_batch_keeps_only_known_keys_and_kinds() {
        let batch: Batch = serde_json::from_value(json!({
            "site": 3,
            "environment": " Staging ",
            "source": "server",
            "events": [
                { "key": "New-Nav", "enabled": true, "kind": "evaluation", "count": 12 },
                { "key": "hero", "enabled": true, "variant": "b", "kind": "exposure" },
                { "key": "hero", "enabled": false, "variant": "b", "kind": "evaluation" },
                { "key": "invented", "enabled": true, "kind": "evaluation" },
                { "key": "new-nav", "enabled": true, "kind": "click" },
                { "key": "new-nav", "enabled": true, "kind": "evaluation", "count": 10_000_000 }
            ]
        }))
        .unwrap();
        let rows = rows_of(&batch, &rules());
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].flag_key, "new-nav");
        assert_eq!(rows[0].variant, "on");
        assert_eq!(rows[0].count, 12);
        assert_eq!(rows[0].environment, "staging");
        assert_eq!(rows[0].source, "server");
        assert_eq!(rows[1].variant, "b");
        assert_eq!(rows[1].kind, "exposure");
        assert_eq!(rows[2].variant, "off", "an off decision is labelled off");
        assert_eq!(rows[3].count, MAX_COUNT, "a count is clamped, not trusted");
    }

    #[test]
    fn an_unusable_environment_or_source_is_neutral() {
        let batch: Batch = serde_json::from_value(json!({
            "site": 3, "environment": "not an env!", "source": "satellite",
            "events": [{ "key": "new-nav", "enabled": true, "kind": "evaluation" }]
        }))
        .unwrap();
        let rows = rows_of(&batch, &rules());
        assert_eq!(rows[0].environment, "");
        assert_eq!(rows[0].source, "browser");
    }

    #[test]
    fn a_batch_is_bounded() {
        let events: Vec<Value> = (0..MAX_EVENTS + 50)
            .map(|_| json!({ "key": "new-nav", "enabled": true, "kind": "evaluation" }))
            .collect();
        let batch = Batch {
            site: 1,
            environment: None,
            source: None,
            events,
        };
        assert_eq!(rows_of(&batch, &rules()).len(), MAX_EVENTS);
    }

    #[test]
    fn context_must_be_an_object() {
        let parsed = parse_context(Some(r#"{"Plan":"pro","seats":3}"#)).unwrap();
        assert_eq!(parsed.get("Plan"), Some(&json!("pro")));
        assert!(parse_context(None).unwrap().is_empty());
        assert!(parse_context(Some("not json")).is_err());
        assert!(parse_context(Some("[1,2]")).is_err());
    }
}
