//! MCP server analytics: `POST /api/log/mcp`, and the `/api/stats/mcp/*` reads.
//!
//! A team that runs a Model Context Protocol server has the questions a web
//! team has about its pages, asked about tools instead: which tools are
//! called, by which clients (Claude, Cursor, an IDE plugin), how fast, and how
//! often they fail. Nothing outside the server's own process can answer them:
//! a call arrives over stdio or a long-lived HTTP stream, and the only witness
//! to how long a handler ran and how it ended is the handler.
//!
//! So the measuring happens there. `instrumentMcpServer` in
//! `@micaforge/sdk-server` wraps the server's `tools/call`, `resources/read`
//! and `prompts/get` handlers, records one measurement per call, and sends
//! them in batches through the same signed transport as `POST /api/log/edge`.
//! This module is the other end of that transport and the reports over what
//! it wrote.
//!
//! ## Trust
//!
//! The same rule as the edge intake, by the same code: the body is signed
//! with the site's ingest key (`x-micaforge-signature: t=…,v1=…`, HMAC-SHA256
//! over `"{t}.{body}"`), checked in constant time inside a five-minute window,
//! and an exact repeat of a signature is refused with `409`. The check is
//! [`crate::routes::ingest::verify_server_batch`], which shares its replay
//! memory with the edge route, so a batch signed for one route cannot be
//! replayed onto the other. A missing signature, a wrong one and a site that
//! does not exist get one `401` after the same work.
//!
//! ## What is kept
//!
//! A call is kept only for one of the three methods the wrapper measures,
//! because a row for `tools/list` or a made-up method would be a row no report
//! reads. Every text column is cut to a fixed length, a session digest that is
//! not hexadecimal is dropped (a raw session id is a bearer credential on the
//! Streamable HTTP transport and must never be stored), a resource URI loses
//! its query string and fragment, and opted-in arguments larger than
//! [`MAX_ARGUMENTS`] are replaced by a note rather than cut into JSON that
//! cannot be parsed. An error message is never on the wire at all.
//!
//! Every accepted batch is answered `202` with how many calls were kept and
//! how many refused, the edge route's shape, so an operator wiring it up can
//! see a bad method or a broken clock in the response.
//!
//! ## The reads
//!
//! Ordinary analytics reads under `/api/stats/`: the envelope, `SiteAccess`
//! with the viewer floor, and the `stats:read` scope an API key needs for any
//! `GET` there. Each one calls the function in [`micaforge_query::mcp_q`] that
//! the MCP read tool and the AI analyst call too.

use std::net::SocketAddr;

use axum::body::to_bytes;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_core::Result;
use micaforge_query::mcp_q::{self, CallsView, McpBucket, McpFilter, METHODS};
use micaforge_store::McpCallRow;
use serde::Deserialize;
use serde_json::{json, Value};

use super::agents::same_site;
use crate::client_ip::{address_tag, client_ip};
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess};
use crate::middleware::LIMITER_BUDGET;
use crate::response::{ok, ok_meta};
use crate::routes::ingest::verify_server_batch;
use crate::routes::orgs::{require_at_least, VIEWER};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// The intake's path. `middleware::is_collection_path` names it too.
pub(crate) const INTAKE_PATH: &str = "/api/log/mcp";

/// Largest batch accepted. Five hundred calls without arguments are well
/// under a hundred kilobytes; this leaves room for opted-in arguments.
const MAX_BODY: usize = 1024 * 1024;

/// Most calls read from one batch. The rest are refused and counted.
const MAX_CALLS: usize = 500;

/// Longest tool, prompt or resource name kept.
const MAX_NAME: usize = 200;

/// Longest server, client, version, transport or error label kept.
const MAX_LABEL: usize = 128;

/// Longest session digest kept: a SHA-256 in hex.
const MAX_SESSION_HASH: usize = 64;

/// Largest opted-in arguments document kept, serialised.
const MAX_ARGUMENTS: usize = 4 * 1024;

/// Batches per minute per site, and per sending address before the signature
/// is checked. The SDK sends at most one batch per flush window, so this
/// bounds a loop rather than a busy server.
const RATE_LIMIT_PER_MIN: u32 = 600;

/// The rate-limit window.
const RATE_WINDOW_SECS: u32 = 60;

/// How far back a call's own timestamp is believed. The SDK sends within
/// seconds; a week covers a server that queued through an outage, and
/// anything older is a broken clock.
const MAX_AGE: chrono::Duration = chrono::Duration::days(7);

/// How far ahead a call's timestamp is believed, for a fast clock.
const MAX_SKEW: chrono::Duration = chrono::Duration::minutes(10);

// ---------------------------------------------------------------------------
// Routers
// ---------------------------------------------------------------------------

/// The reads, for the API group.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats/mcp/overview", get(overview))
        .route("/api/stats/mcp/timeseries", get(timeseries))
        .route("/api/stats/mcp/tools", get(tools))
        .route("/api/stats/mcp/clients", get(clients))
        .route("/api/stats/mcp/calls", get(calls))
}

/// The intake, for the collection group: no CORS layer (the caller is a
/// server, never a browser) and its own body limit.
pub fn collection_router() -> Router<AppState> {
    Router::new().route(INTAKE_PATH, post(collect))
}

// ---------------------------------------------------------------------------
// The intake
// ---------------------------------------------------------------------------

/// The body the SDK sends.
#[derive(Debug, Deserialize)]
struct McpPayload {
    site_id: u32,
    /// Decoded one by one, so one malformed call does not cost the batch.
    #[serde(default)]
    mcp_calls: Vec<Value>,
}

/// One call on the wire. Every field is optional so a call missing one is
/// judged by [`row_of`] rather than refused by the decoder.
#[derive(Debug, Default, Deserialize)]
struct WireCall {
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    server_name: String,
    #[serde(default)]
    server_version: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    client_name: String,
    #[serde(default)]
    client_version: String,
    #[serde(default)]
    protocol_version: String,
    #[serde(default)]
    transport: String,
    #[serde(default)]
    session_hash: String,
    #[serde(default)]
    duration_ms: Option<f64>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    error_type: String,
    #[serde(default)]
    error_code: Option<i64>,
    #[serde(default)]
    arg_bytes: Option<f64>,
    #[serde(default)]
    result_bytes: Option<f64>,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    sdk_version: String,
}

/// `POST /api/log/mcp` — one signed batch of MCP calls.
async fn collect(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let signature = header(&parts.headers, "x-micaforge-signature").map(str::to_string);
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let caller = address_tag(client_ip(&parts.headers, peer));

    let Ok(body) = to_bytes(body, MAX_BODY).await else {
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "bad_request",
            "the batch is larger than one MCP batch can be",
        );
    };

    // The cheapest rejection first: an unsigned request never reaches
    // Postgres, so a flood on this open endpoint is not database load.
    let Some(signature) = signature else {
        return unauthorized();
    };
    if !allow(&state, &format!("mcp-ip:{caller}")).await {
        return too_many();
    }

    let Ok(payload) = serde_json::from_slice::<McpPayload>(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body is not an MCP batch: expected {\"site_id\":…,\"mcp_calls\":[…]}",
        );
    };
    let Ok(site_id) = u16::try_from(payload.site_id) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "site_id is out of range",
        );
    };

    let Some(batch) = verify_server_batch(&state, site_id, &body, &signature).await else {
        return unauthorized();
    };
    if !allow(&state, &format!("mcp:{site_id}")).await {
        return too_many();
    }
    // Claimed only now, after the limiter, so a batch refused with `429` can
    // be sent again under the same signature.
    if !batch.claim() {
        return api_error(
            StatusCode::CONFLICT,
            "conflict",
            "this signed batch was already received; sign each send afresh",
        );
    }

    let now = Utc::now();
    let total = payload.mcp_calls.len();
    let mut accepted = 0usize;
    for raw in payload.mcp_calls.into_iter().take(MAX_CALLS) {
        let row = serde_json::from_value::<WireCall>(raw)
            .ok()
            .and_then(|call| row_of(batch.site.site_id_u16(), call, now));
        if let Some(row) = row {
            state.mcp_calls.push(row);
            accepted += 1;
        }
    }

    let mut res = ok(json!({ "accepted": accepted, "rejected": total - accepted })).into_response();
    *res.status_mut() = StatusCode::ACCEPTED;
    res
}

/// One wire call as a row, or `None` for a call no report could read.
///
/// The only reasons to refuse are a method the wrapper does not measure and
/// a status that is not one of the three; everything else is cleaned rather
/// than refused, because a call with an odd client name is still a call.
fn row_of(site_id: u16, call: WireCall, now: DateTime<Utc>) -> Option<McpCallRow> {
    let method = call.method.trim();
    if !METHODS.contains(&method) {
        return None;
    }
    let error_code = call
        .error_code
        .map(|c| c.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
        .unwrap_or(0);
    let error_type = label(&call.error_type);
    let status = match call.status.trim() {
        "ok" | "error" | "tool_error" => call.status.trim().to_string(),
        // An SDK that did not say: an error class or code means it failed.
        "" if !error_type.is_empty() || error_code != 0 => "error".to_string(),
        "" => "ok".to_string(),
        _ => return None,
    };
    let name = if method == "resources/read" {
        resource_name(&call.name)
    } else {
        clamp(&call.name, MAX_NAME)
    };
    Some(McpCallRow {
        site_id,
        timestamp: call_time(&call.timestamp, now),
        server_name: label(&call.server_name),
        server_version: label(&call.server_version),
        method: method.to_string(),
        name,
        client_name: label(&call.client_name),
        client_version: label(&call.client_version),
        protocol_version: label(&call.protocol_version),
        transport: label(&call.transport).to_ascii_lowercase(),
        session_hash: session_digest(&call.session_hash),
        duration_ms: whole(call.duration_ms),
        status,
        error_type,
        error_code,
        arg_bytes: whole(call.arg_bytes),
        result_bytes: whole(call.result_bytes),
        arguments: arguments_text(call.arguments),
        sdk_version: label(&call.sdk_version),
    })
}

/// The call's own time when it is believable, the arrival time otherwise.
fn call_time(raw: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
        .filter(|t| *t >= now - MAX_AGE && *t <= now + MAX_SKEW)
        .unwrap_or(now)
}

/// A resource URI with its query string and fragment removed, then cut.
///
/// A query string is where a URI carries the one-off value (a search term, a
/// user's id) that would make every read its own row and could be personal.
/// The path is what names the resource.
fn resource_name(raw: &str) -> String {
    let end = raw.find(['?', '#']).unwrap_or(raw.len());
    clamp(&raw[..end], MAX_NAME)
}

/// A session digest, or empty for anything that is not lower-case hex.
///
/// The SDK sends a digest. Something else in this field could be the raw id,
/// which on the Streamable HTTP transport is what lets a client speak in an
/// open session, so it is dropped rather than stored.
fn session_digest(raw: &str) -> String {
    let raw = raw.trim();
    let hex = !raw.is_empty()
        && raw.len() <= MAX_SESSION_HASH
        && raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if hex {
        raw.to_string()
    } else {
        String::new()
    }
}

/// Opted-in arguments as stored text: the JSON the SDK sent, or a note when
/// it is too large. Never cut, because cut JSON cannot be parsed.
fn arguments_text(raw: Option<Value>) -> String {
    match raw {
        None | Some(Value::Null) => String::new(),
        Some(value) => {
            let text = value.to_string();
            if text.len() > MAX_ARGUMENTS {
                json!({ "_dropped": "arguments exceeded the size limit" }).to_string()
            } else {
                text
            }
        }
    }
}

/// A non-negative whole number that fits the column, from any JSON number.
fn whole(raw: Option<f64>) -> u32 {
    match raw {
        Some(v) if v.is_finite() && v > 0.0 => v.round().min(f64::from(u32::MAX)) as u32,
        _ => 0,
    }
}

/// A label column: control characters removed, trimmed, cut.
fn label(raw: &str) -> String {
    let clean: String = raw.chars().filter(|c| !c.is_control()).collect();
    clamp(&clean, MAX_LABEL)
}

/// Trim, then cut to `max` bytes without splitting a character.
fn clamp(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// A header value as `&str`, or `None` when absent, empty or not UTF-8.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Consume one unit against a rate-limit key. A limiter that is down or slow
/// answers "allowed", like every limiter on the collection path.
async fn allow(state: &AppState, key: &str) -> bool {
    match tokio::time::timeout(
        LIMITER_BUDGET,
        state
            .kv
            .rate_limit(key, RATE_LIMIT_PER_MIN, RATE_WINDOW_SECS),
    )
    .await
    {
        Ok(Ok(allowed)) => allowed,
        Ok(Err(_)) | Err(_) => true,
    }
}

/// One sentence for every failure to prove the key, so the answer says
/// nothing about which one it was.
fn unauthorized() -> Response {
    api_error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "the signature did not verify for this site",
    )
}

/// `429`, with the `Retry-After` the SDK backs off on.
fn too_many() -> Response {
    let mut res = api_error(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "too many MCP batches; send them less often",
    );
    if let Ok(value) = "60".parse() {
        res.headers_mut().insert("retry-after", value);
    }
    res
}

/// The error envelope from `docs/CONTRACT.md` §3.3.
fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = json!({ "error": { "code": code, "message": message } });
    let mut res = (status, Json(body)).into_response();
    if let Ok(value) = "no-store".parse() {
        res.headers_mut().insert("cache-control", value);
    }
    res
}

// ---------------------------------------------------------------------------
// The reads
// ---------------------------------------------------------------------------

/// What every read takes beyond the envelope.
#[derive(Debug, Default, Deserialize)]
struct McpParams {
    /// Only this server.
    #[serde(default)]
    server: Option<String>,
    /// Only this method.
    #[serde(default)]
    method: Option<String>,
    /// Only this client.
    #[serde(default)]
    client: Option<String>,
    /// `slowest` or `failing`, for the calls list.
    #[serde(default)]
    view: Option<String>,
}

impl McpParams {
    fn filter(&self) -> Result<McpFilter> {
        McpFilter {
            server: self.server.clone(),
            method: self.method.clone(),
            client: self.client.clone(),
        }
        .normalised()
    }
}

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The checks every read makes before it touches ClickHouse.
fn authorised(access: &SiteAccess, env: &micaforge_core::QueryEnvelope) -> Result<()> {
    require_at_least(&access.role, VIEWER)?;
    same_site(access, env)
}

/// `GET /api/stats/mcp/overview` — calls, errors, latency, sessions, clients
/// and the servers that sent them.
async fn overview(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<McpParams>,
) -> ApiResult<Json<Value>> {
    authorised(&access, &env)?;
    let filter = params.filter()?;
    Ok(ok(mcp_q::overview(&st.ch, &env, &filter).await?))
}

/// `GET /api/stats/mcp/timeseries` — calls, errors and latency per bucket,
/// with every bucket of the window present.
async fn timeseries(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<McpParams>,
) -> ApiResult<Json<Value>> {
    authorised(&access, &env)?;
    let filter = params.filter()?;
    let rows = mcp_q::timeseries(&st.ch, &env, &filter).await?;
    let starts = crate::routes::links::bucket_starts(
        env.range.start,
        env.range.end - chrono::Duration::milliseconds(1),
        env.granularity,
        env.range.tz,
    );
    Ok(ok_meta(
        fill(&starts, rows),
        json!({ "granularity": env.granularity.as_str() }),
    ))
}

/// One bucket per start, the measured one where there was one and zeros
/// elsewhere, so a quiet hour is drawn as a zero rather than skipped.
fn fill(starts: &[i64], rows: Vec<McpBucket>) -> Vec<McpBucket> {
    let mut rows = rows.into_iter().peekable();
    starts
        .iter()
        .map(|t| {
            while rows.peek().is_some_and(|r| r.t < *t) {
                rows.next();
            }
            match rows.peek() {
                Some(r) if r.t == *t => rows.next().unwrap_or_default(),
                _ => McpBucket {
                    t: *t,
                    ..McpBucket::default()
                },
            }
        })
        .collect()
}

/// `GET /api/stats/mcp/tools` — tools, resources and prompts with their
/// error rates and latency.
async fn tools(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<McpParams>,
) -> ApiResult<Json<Value>> {
    authorised(&access, &env)?;
    let filter = params.filter()?;
    Ok(ok(mcp_q::tools(&st.ch, &env, &filter).await?))
}

/// `GET /api/stats/mcp/clients` — the clients by `clientInfo.name`.
async fn clients(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<McpParams>,
) -> ApiResult<Json<Value>> {
    authorised(&access, &env)?;
    let filter = params.filter()?;
    Ok(ok(mcp_q::clients(&st.ch, &env, &filter).await?))
}

/// `GET /api/stats/mcp/calls?view=slowest|failing` — single calls.
async fn calls(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<McpParams>,
) -> ApiResult<Json<Value>> {
    authorised(&access, &env)?;
    let filter = params.filter()?;
    let view = CallsView::parse(params.view.as_deref())?;
    let rows = mcp_q::calls(&st.ch, &env, &filter, view).await?;
    Ok(ok_meta(
        &rows,
        json!({ "rows": rows.len(), "view": view.as_str() }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::Duration;

    /// The server SDK's default send timeout (`timeout` in its options). The
    /// intake's time budget must end first, so a slow batch is answered `503`
    /// and retried rather than abandoned by the SDK.
    const SDK_TIMEOUT: Duration = Duration::from_secs(10);

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap()
    }

    fn call(method: &str) -> WireCall {
        WireCall {
            timestamp: "2026-10-09T11:59:58.250Z".into(),
            method: method.into(),
            name: "search_docs".into(),
            server_name: "docs".into(),
            client_name: "claude-ai".into(),
            client_version: "0.1.0".into(),
            transport: "Streamable-HTTP".into(),
            session_hash: "ab12".into(),
            duration_ms: Some(41.6),
            arg_bytes: Some(120.0),
            ..WireCall::default()
        }
    }

    #[test]
    fn a_measured_call_becomes_a_row() {
        let row = row_of(4, call("tools/call"), now()).expect("kept");
        assert_eq!(row.site_id, 4);
        assert_eq!(row.method, "tools/call");
        assert_eq!(row.name, "search_docs");
        assert_eq!(row.status, "ok");
        assert_eq!(row.duration_ms, 42);
        assert_eq!(row.arg_bytes, 120);
        assert_eq!(row.transport, "streamable-http");
        assert_eq!(row.timestamp.timestamp_millis() % 1000, 250);
        assert!(row.arguments.is_empty(), "nothing unless opted in");
    }

    #[test]
    fn only_the_three_measured_methods_are_kept() {
        for method in METHODS {
            assert!(row_of(1, call(method), now()).is_some(), "{method}");
        }
        for method in ["tools/list", "initialize", "", "TOOLS/CALL"] {
            assert!(row_of(1, call(method), now()).is_none(), "{method:?}");
        }
    }

    #[test]
    fn the_status_is_one_of_three_or_derived_from_the_error() {
        let mut thrown = call("tools/call");
        thrown.error_type = "McpError".into();
        thrown.error_code = Some(-32602);
        let row = row_of(1, thrown, now()).unwrap();
        assert_eq!((row.status.as_str(), row.error_code), ("error", -32602));

        let mut tool = call("tools/call");
        tool.status = "tool_error".into();
        assert_eq!(row_of(1, tool, now()).unwrap().status, "tool_error");

        let mut odd = call("tools/call");
        odd.status = "maybe".into();
        assert!(row_of(1, odd, now()).is_none());

        let mut huge = call("tools/call");
        huge.error_code = Some(i64::MAX);
        assert_eq!(row_of(1, huge, now()).unwrap().error_code, i32::MAX);
    }

    #[test]
    fn a_session_id_that_is_not_a_digest_is_never_stored() {
        assert_eq!(session_digest("0a1b2c"), "0a1b2c");
        assert_eq!(session_digest(""), "");
        // A UUID, which is what a raw Streamable HTTP session id looks like.
        assert_eq!(session_digest("5f0c8a52-1b9e-4c1e-9a77-3a0e3c7d2b11"), "");
        assert_eq!(session_digest("ABCDEF"), "");
        assert_eq!(session_digest(&"a".repeat(65)), "");
        assert_eq!(session_digest(&"a".repeat(64)).len(), 64);
    }

    #[test]
    fn a_resource_loses_its_query_and_fragment() {
        let mut read = call("resources/read");
        read.name = "file:///docs/guide.md?user=ana@example.com#intro".into();
        assert_eq!(
            row_of(1, read, now()).unwrap().name,
            "file:///docs/guide.md"
        );
        // A tool name is a name, not a URI; it is only cut.
        let mut tool = call("tools/call");
        tool.name = format!("t{}", "x".repeat(400));
        assert_eq!(row_of(1, tool, now()).unwrap().name.len(), MAX_NAME);
    }

    #[test]
    fn opted_in_arguments_are_kept_whole_or_replaced_by_a_note() {
        assert_eq!(arguments_text(None), "");
        assert_eq!(arguments_text(Some(Value::Null)), "");
        assert_eq!(
            arguments_text(Some(json!({ "query": "rust" }))),
            r#"{"query":"rust"}"#
        );
        let big = arguments_text(Some(json!({ "blob": "x".repeat(MAX_ARGUMENTS) })));
        let parsed: Value = serde_json::from_str(&big).expect("still JSON");
        assert!(parsed.get("_dropped").is_some());
    }

    #[test]
    fn a_wild_timestamp_becomes_the_arrival_time() {
        let at = |raw: &str| call_time(raw, now());
        assert_eq!(at("not a time"), now());
        assert_eq!(at("2020-01-01T00:00:00Z"), now(), "older than a week");
        assert_eq!(at("2026-10-09T12:30:00Z"), now(), "too far ahead");
        assert_ne!(
            at("2026-10-09T12:05:00Z"),
            now(),
            "a fast clock is believed"
        );
    }

    #[test]
    fn numbers_are_clamped_into_their_columns() {
        assert_eq!(whole(None), 0);
        assert_eq!(whole(Some(-4.0)), 0);
        assert_eq!(whole(Some(f64::NAN)), 0);
        assert_eq!(whole(Some(1e12)), u32::MAX);
        assert_eq!(label("  claude\u{0}-ai\n "), "claude-ai");
        assert_eq!(label(&"é".repeat(100)).len(), MAX_LABEL);
    }

    #[test]
    fn a_batch_decodes_with_one_bad_call_in_it() {
        let body = json!({
            "site_id": 3,
            "mcp_calls": [
                { "method": "tools/call", "name": "a" },
                { "method": 7 },
                { "method": "prompts/get", "name": "b" }
            ]
        });
        let payload: McpPayload = serde_json::from_value(body).unwrap();
        let kept: Vec<_> = payload
            .mcp_calls
            .into_iter()
            .filter_map(|v| serde_json::from_value::<WireCall>(v).ok())
            .filter_map(|c| row_of(3, c, now()))
            .collect();
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn empty_buckets_are_filled_with_zeros_in_order() {
        let bucket = |t: i64, calls: u64| McpBucket {
            t,
            calls,
            ..McpBucket::default()
        };
        let filled = fill(&[0, 10, 20, 30], vec![bucket(10, 4), bucket(30, 2)]);
        let calls: Vec<u64> = filled.iter().map(|b| b.calls).collect();
        assert_eq!(calls, [0, 4, 0, 2]);
        assert_eq!(
            filled.iter().map(|b| b.t).collect::<Vec<_>>(),
            [0, 10, 20, 30]
        );
        // A row outside the starts (a bucket the walk did not produce) is dropped.
        assert_eq!(fill(&[0], vec![bucket(5, 1)])[0].calls, 0);
    }

    #[test]
    fn the_intake_is_a_quiet_collection_path_inside_the_sdk_timeout() {
        assert!(crate::middleware::is_collection_path(INTAKE_PATH));
        let budget = crate::middleware::budget_for(&axum::http::Method::POST, INTAKE_PATH);
        assert_eq!(budget, Some(crate::middleware::INGEST_BUDGET));
        assert!(crate::middleware::INGEST_BUDGET < SDK_TIMEOUT);
    }
}

/// The intake and a read, end to end, against real stores: a batch signed
/// with a site's ingest key lands in ClickHouse and reads back for that site's
/// members only. See [`crate::routes::oauth::harness`] for how to run these;
/// this one also needs `MICAFORGE_TEST_CLICKHOUSE_URL`.
#[cfg(test)]
mod live {
    use axum::body::Body;
    use axum::http::Request;
    use hmac::{Hmac, Mac};
    use micaforge_core::new_id;
    use micaforge_store::models::sites;
    use micaforge_store::{TableWriter, WriterCfg};
    use serde_json::json;
    use sha2::Sha256;

    use crate::routes::flags::tests::{drop_clickhouse, live_clickhouse};
    use crate::routes::oauth::harness::Harness;

    /// The SDK's signature: hex HMAC-SHA256 over `"{t}.{body}"`.
    fn sign(key: &str, body: &str) -> String {
        let t = chrono::Utc::now().timestamp();
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(format!("{t}.{body}").as_bytes());
        format!("t={t},v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn post(body: &str, signature: Option<&str>) -> Request<Body> {
        let mut req = Request::post(super::INTAKE_PATH).header("content-type", "application/json");
        if let Some(signature) = signature {
            req = req.header("x-micaforge-signature", signature);
        }
        req.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    #[ignore = "needs Postgres, Valkey and ClickHouse"]
    async fn a_signed_batch_lands_and_reads_back_for_its_site_only() {
        let mut h = Harness::new().await;
        let ch = live_clickhouse().await;
        h.st.ch = ch.clone();
        h.st.mcp_calls = TableWriter::spawn(ch.clone(), "mcp_calls", WriterCfg::default());

        let ada = h.person("ada@example.com").await;
        let bob = h.person("bob@example.com").await;
        let site = sites::create(
            &h.st.pg,
            new_id(),
            ada.org,
            "docs.example.com",
            "Docs",
            "UTC",
        )
        .await
        .unwrap();
        let key = crate::routes::sites::ingest_key(&h.st.pg, &h.st.cfg, &site)
            .await
            .unwrap();
        let site_id = site.site_id_u16();

        let body = json!({
            "site_id": site_id,
            "mcp_calls": [
                { "method": "tools/call", "name": "search", "client_name": "claude-ai",
                  "duration_ms": 12, "status": "ok", "session_hash": "ab" },
                { "method": "tools/call", "name": "search", "client_name": "cursor-vscode",
                  "duration_ms": 340, "status": "error", "error_type": "McpError",
                  "error_code": -32602 },
                { "method": "tools/list" }
            ]
        })
        .to_string();
        let signature = sign(&key, &body);

        let (status, _, answer) = h.send(post(&body, Some(&signature))).await;
        assert_eq!(status, 202, "{answer}");
        assert_eq!(answer["data"], json!({ "accepted": 2, "rejected": 1 }));

        let (status, _, _) = h.send(post(&body, Some(&signature))).await;
        assert_eq!(status, 409, "an exact repeat is refused");
        let (status, _, _) = h.send(post(&body, Some(&sign("mfi_wrong", &body)))).await;
        assert_eq!(status, 401);
        let (status, _, _) = h.send(post(&body, None)).await;
        assert_eq!(status, 401);
        let huge = format!(
            r#"{{"site_id":{site_id},"mcp_calls":["{}"]}}"#,
            "x".repeat(1 << 20)
        );
        let (status, _, _) = h.send(post(&huge, Some(&sign(&key, &huge)))).await;
        assert_eq!(status, 413);

        assert!(h.st.mcp_calls.flush().await, "the lane wrote its rows");
        let read = |cookie: &str| {
            Request::get(format!("/api/stats/mcp/overview?site={site_id}&range=24h"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap()
        };
        let (status, _, overview) = h.send(read(&ada.cookie)).await;
        assert_eq!(status, 200, "{overview}");
        assert_eq!(overview["data"]["calls"], 2);
        assert_eq!(overview["data"]["errors"], 1);
        assert_eq!(overview["data"]["clients"], 2);

        let (status, _, _) = h.send(read(&bob.cookie)).await;
        assert_eq!(status, 404, "another organisation cannot read it");

        drop_clickhouse(&ch).await;
        h.finish().await;
    }
}
