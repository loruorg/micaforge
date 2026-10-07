//! `POST /api/mcp` — a Model Context Protocol server over the read endpoints.
//!
//! `PRODUCT.md` lists, among the differentiating surface, "an MCP server so an
//! agent can query the analytics it is helping to produce". That sentence is the
//! whole design brief. The agents this product counts on one screen can, through
//! this endpoint, read the count. Nothing here is a new capability: every tool
//! is one of the HTTP reads already in `docs/CONTRACT.md` §3.3, answered by the
//! same query functions, over the same query envelope.
//!
//! ## The shape
//!
//! JSON-RPC 2.0 over a single HTTP POST, protocol revision `2024-11-05`. Three
//! methods carry the protocol — `initialize`, `tools/list`, `tools/call` — plus
//! `ping`. Batches are accepted, and a notification (a member with no `id`) is
//! answered with no response, which is what JSON-RPC requires. Every tool here
//! is a read, so a notification has nothing to perform either.
//!
//! ## Errors are JSON-RPC objects, not HTTP statuses
//!
//! Once a request has been parsed, every failure — an unknown tool, a bad time
//! range, a site the caller may not read, a store that would not answer — comes
//! back as `{"jsonrpc":"2.0","id":…,"error":{…}}` with HTTP 200. An MCP client
//! that receives a naked 4xx has to guess which of its batched calls failed and
//! generally just reports "the server is broken". The one exception is
//! authentication, which is a property of the transport rather than of any
//! request in it and is answered with the ordinary `401` envelope.
//!
//! Error text is filtered the same way the HTTP layer filters it: driver and
//! upstream messages carry query fragments and host names, so
//! [`micaforge_core::Error::is_public`] decides whether the sentence travels or
//! whether a fixed one does. The stable `snake_case` code always travels, in
//! `error.data.code`, because that is what a caller can branch on.
//!
//! ## Why an API key and not a session
//!
//! The caller here is a program. A session cookie belongs to a browser, and an
//! agent that has one is an agent running inside somebody's dashboard tab. The
//! endpoint therefore insists on `Authorization: Bearer mfg_…`, which is a
//! credential an operator minted deliberately, can name, can scope, and can
//! revoke without logging anybody out.
//!
//! ## What the key can read
//!
//! Its own grant ([`KeyGrant`]) and nothing more: `stats:read` to reach the
//! endpoint at all, the sites of its own organisation, and only its own site
//! when it was minted for one. The key resolves against the organisation's
//! owner, who may belong to other organisations; those are not the key's.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use chrono_tz::Tz;
use micaforge_core::{
    filters, time, Audience, Compare, Dimension, Error, Granularity, Metric, QueryEnvelope,
};
use micaforge_query::{agents_q, gap, stats};
use micaforge_store::models::sites::{self, Site};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::agents::{agent_envelope, json_body};
use crate::error::ApiError;
use crate::extract::{AuthUser, KeyGrant};
use crate::state::AppState;

/// The protocol revision this server speaks.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Most rows one tool call will return.
///
/// Lower than the HTTP ceiling of ten thousand on purpose. The consumer here is
/// a context window, and ten thousand breakdown rows is not an answer to any
/// question an agent can usefully ask — it is a way to spend a budget. A caller
/// that genuinely needs the whole table uses the CSV export, which streams.
const MAX_TOOL_ROWS: u32 = 1_000;

/// The MCP endpoint.
///
/// Absolute path, merged at the root. See the note on [`super::agents::router`].
pub fn router() -> Router<AppState> {
    Router::new().route("/api/mcp", post(handle))
}

// ---------------------------------------------------------------------------
// JSON-RPC vocabulary
// ---------------------------------------------------------------------------

/// The body was not JSON at all.
const PARSE_ERROR: i64 = -32700;
/// Well-formed JSON that is not a JSON-RPC request.
const INVALID_REQUEST: i64 = -32600;
/// A method this server does not implement.
const METHOD_NOT_FOUND: i64 = -32601;
/// The method exists; the arguments do not work.
const INVALID_PARAMS: i64 = -32602;
/// Ours, not the caller's.
const INTERNAL_ERROR: i64 = -32603;

// The `-32000..-32099` block is reserved for server-defined errors. These mirror
// the HTTP statuses the same failures get on the REST surface, so a client that
// uses both sees one taxonomy rather than two.

/// No usable credential.
const ERR_UNAUTHORIZED: i64 = -32001;
/// A valid credential without the rights for this call.
const ERR_FORBIDDEN: i64 = -32002;
/// The site, goal or record does not exist, or the caller may not know it does.
const ERR_NOT_FOUND: i64 = -32003;
/// Too many calls.
const ERR_RATE_LIMITED: i64 = -32004;
/// The call collides with existing state.
const ERR_CONFLICT: i64 = -32005;
/// A dependency outside the process failed.
const ERR_UPSTREAM: i64 = -32006;

/// A JSON-RPC error object, before it is placed in a response.
#[derive(Debug, Clone)]
struct RpcError {
    /// The numeric code a client branches on.
    code: i64,
    /// One human sentence. Never carries store or driver text.
    message: String,
    /// The Micaforge error code, and the offending field when there is one.
    data: Option<Value>,
}

impl RpcError {
    /// A protocol-level failure, which has no Micaforge error behind it.
    fn protocol(code: i64, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Translate a Micaforge failure into a JSON-RPC error.
///
/// `tool` names the call in the log line, which is the only place the real
/// message of a store failure is written. The response gets a fixed sentence:
/// ClickHouse error text quotes the failing statement, and a bound filter value
/// is inside that statement.
fn rpc_error_from(tool: &str, e: Error) -> RpcError {
    let code = match &e {
        Error::NotFound(_) => ERR_NOT_FOUND,
        Error::Unauthorized(_) => ERR_UNAUTHORIZED,
        Error::Forbidden(_) => ERR_FORBIDDEN,
        Error::Conflict(_) => ERR_CONFLICT,
        Error::Validation { .. } | Error::BadRequest(_) => INVALID_PARAMS,
        Error::RateLimited { .. } => ERR_RATE_LIMITED,
        Error::Upstream(_) => ERR_UPSTREAM,
        Error::Database(_) | Error::Internal(_) => INTERNAL_ERROR,
    };

    let mut data = Map::new();
    data.insert("code".to_string(), Value::String(e.code().to_string()));
    if let Some(field) = e.field() {
        data.insert("field".to_string(), Value::String(field.to_string()));
    }
    data.insert("tool".to_string(), Value::String(tool.to_string()));

    let message = if e.is_public() {
        e.to_string()
    } else {
        tracing::error!(tool, error = %e, "mcp tool call failed");
        "the analytics store could not answer this call".to_string()
    };

    RpcError {
        code,
        message,
        data: Some(Value::Object(data)),
    }
}

/// A successful JSON-RPC response.
fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A failed JSON-RPC response.
///
/// `data` is omitted rather than sent as `null` when there is nothing to put in
/// it, because a client that tests for the key's presence should not have to
/// also test for its nullity.
fn rpc_fail(id: Value, error: &RpcError) -> Value {
    let mut object = Map::new();
    object.insert("code".to_string(), json!(error.code));
    object.insert("message".to_string(), Value::String(error.message.clone()));
    if let Some(data) = &error.data {
        object.insert("data".to_string(), data.clone());
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": Value::Object(object) })
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// The single MCP entry point.
///
/// The body is taken as bytes and parsed here rather than through
/// `Json<Value>`, so that a malformed body becomes a JSON-RPC `-32700` instead
/// of the framework's own `400`. A client that spoke JSON-RPC to us deserves to
/// be told what was wrong in the dialect it was speaking.
async fn handle(
    State(st): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    require_api_key(&headers)?;
    let user = acting_user(&auth);
    // `AuthUser` only carries a grant for a key, and the header check above
    // only passes for a key, so this is never `None` in practice. It is
    // checked rather than assumed: a grantless caller here would be reading
    // with its owner's whole authority.
    let Some(grant) = auth.key.as_ref() else {
        return Err(Error::Unauthorized(
            "the MCP endpoint takes an API key: `Authorization: Bearer mfg_...`".to_string(),
        )
        .into());
    };

    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        let err = RpcError::protocol(PARSE_ERROR, "the request body is not valid JSON");
        return Ok(json_response(rpc_fail(Value::Null, &err)));
    };

    match payload {
        Value::Array(items) if items.is_empty() => {
            let err = RpcError::protocol(INVALID_REQUEST, "a batch must hold at least one call");
            Ok(json_response(rpc_fail(Value::Null, &err)))
        }
        Value::Array(items) => {
            let mut replies = Vec::with_capacity(items.len());
            for item in &items {
                if let Some(reply) = dispatch(&st, user, grant, item).await {
                    replies.push(reply);
                }
            }
            // A batch of nothing but notifications gets no body, per JSON-RPC.
            if replies.is_empty() {
                Ok(accepted())
            } else {
                Ok(json_response(Value::Array(replies)))
            }
        }
        item @ Value::Object(_) => match dispatch(&st, user, grant, &item).await {
            Some(reply) => Ok(json_response(reply)),
            None => Ok(accepted()),
        },
        _ => {
            let err = RpcError::protocol(
                INVALID_REQUEST,
                "a JSON-RPC request is an object, or an array of objects",
            );
            Ok(json_response(rpc_fail(Value::Null, &err)))
        }
    }
}

/// Answer one member of a batch, or `None` when it was a notification.
async fn dispatch(st: &AppState, user: Uuid, grant: &KeyGrant, item: &Value) -> Option<Value> {
    // A member with no `id` is a notification: it is performed, if there is
    // anything to perform, and never answered. Every tool on this server is a
    // read, so there is nothing to perform and the whole member is a no-op.
    let id = item.get("id")?.clone();

    if item.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(rpc_fail(
            id,
            &RpcError::protocol(INVALID_REQUEST, "`jsonrpc` must be exactly \"2.0\""),
        ));
    }

    let Some(method) = item.get("method").and_then(Value::as_str) else {
        return Some(rpc_fail(
            id,
            &RpcError::protocol(INVALID_REQUEST, "a request needs a `method`"),
        ));
    };

    let params = item.get("params").cloned().unwrap_or(Value::Null);

    let outcome = match method {
        "initialize" => Ok(initialize_result()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(st, user, grant, &params).await,
        other => Err(RpcError::protocol(
            METHOD_NOT_FOUND,
            format!("`{other}` is not a method this server implements"),
        )),
    };

    Some(match outcome {
        Ok(result) => rpc_ok(id, result),
        Err(error) => rpc_fail(id, &error),
    })
}

/// A JSON-RPC body always travels with `200`; the error is inside it.
fn json_response(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

/// Nothing to say: a notification, or a batch of them.
fn accepted() -> Response {
    StatusCode::ACCEPTED.into_response()
}

/// Insist on an API key rather than a browser session.
///
/// The `AuthUser` extractor has already established *that* the caller is
/// authenticated; this establishes *how*. See the module note.
fn require_api_key(headers: &HeaderMap) -> micaforge_core::Result<()> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    let mut parts = raw.splitn(2, ' ');
    let scheme = parts.next().unwrap_or_default();
    let token = parts.next().unwrap_or_default().trim();

    if scheme.eq_ignore_ascii_case("bearer") && token.starts_with("mfg_") {
        Ok(())
    } else {
        Err(Error::Unauthorized(
            "the MCP endpoint takes an API key: `Authorization: Bearer mfg_...`".to_string(),
        ))
    }
}

/// The one thing this module needs from the shared authentication extractor.
///
/// Isolated in a function on purpose. Everything downstream is scoped to this
/// identity, so if the extractor ever changes how it names the authenticated
/// user, exactly one line here changes with it.
fn acting_user(auth: &AuthUser) -> Uuid {
    auth.user.id
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

/// What the server tells a client on connection.
///
/// The `instructions` field is not filler. An agent reading these numbers has
/// two ways to be badly wrong about them — mistaking crawler traffic for
/// readers, and reading a Citation Gap ratio as a performance score — and this
/// is the only place to say so before the first call is made.
fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "micaforge",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "Micaforge is cookieless web and product analytics that treats machine \
    readers as a first-class audience. Two things to hold on to before reading any number here. \
    First, `audience` selects the population: `human` (people in browsers, the default), `agent` \
    (named crawlers and AI fetchers), or `all`. A tool called without it reports on people only. \
    Second, the Citation Gap compares `agent_fetches` (how much AI agents took from a URL) with \
    `answer_arrivals` (how many readers came back to it from an answer engine). A high ratio is \
    not success; it means the page is being consumed without returning readers. Every verdict is \
    drawn against thresholds that travel with the answer. Start with `list_sites` to learn the \
    numeric site ids every other tool needs.",
    })
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

/// The wire names of every dimension, for a tool description.
///
/// Written out rather than left as "see the docs": the model choosing a
/// dimension has only this string to choose from, and a wrong guess costs a
/// round trip and an error.
const DIMENSION_NAMES: &str = "pathname, entry_path, exit_path, hostname, page_title, \
querystring, hash, referrer, referrer_host, channel, source, utm_source, utm_medium, \
utm_campaign, utm_term, utm_content, browser, browser_version, os, os_version, device_type, \
screen_class, language, timezone, country, region, city, asn_org, is_datacenter, event_name, \
goal, visitor_id, identified_id, experiment, variant, revenue, agent_id, operator, purpose, \
verified, robots_allowed, status, content_type, and the open forms `props.<key>` and \
`flag.<key>`";

/// The query-envelope properties every site-scoped tool accepts.
///
/// One function so that the twelve schemas cannot drift from each other or from
/// `docs/CONTRACT.md` §3.1.
fn envelope_properties() -> Map<String, Value> {
    let mut props = Map::new();

    props.insert(
        "site".to_string(),
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 65535,
            "description": "Numeric site id, from `list_sites`. Required."
        }),
    );
    props.insert(
        "range".to_string(),
        json!({
            "type": "string",
            "enum": ["today", "yesterday", "24h", "7d", "30d", "90d", "12mo", "mtd", "ytd", "all"],
            "description": "A named window in the site's own calendar and timezone. \
        Ignored when `start` is given. Defaults to `24h`. The `<n>h`, `<n>d` and `<n>mo` forms are \
        also accepted."
        }),
    );
    props.insert(
        "start".to_string(),
        json!({
            "type": "string",
            "description": "ISO-8601 date or timestamp for the inclusive start of the window. \
        A bare date is read as midnight in the site's timezone."
        }),
    );
    props.insert(
        "end".to_string(),
        json!({
            "type": "string",
            "description": "ISO-8601 date or timestamp for the end of the window. A bare date \
        includes that whole day. Requires `start`; defaults to now."
        }),
    );
    props.insert(
        "compare".to_string(),
        json!({
            "type": "string",
            "enum": ["none", "previous", "year"],
            "default": "none",
            "description": "Which earlier window to report beside this one."
        }),
    );
    props.insert(
        "tz".to_string(),
        json!({
            "type": "string",
            "description": "IANA timezone the window is cut in. Defaults to the site's own."
        }),
    );
    props.insert(
        "granularity".to_string(),
        json!({
            "type": "string",
            "enum": ["minute", "hour", "day", "week", "month"],
            "description": "Bucket width for series results. Chosen from the window when omitted."
        }),
    );
    props.insert("filters".to_string(), filter_schema());
    props.insert(
        "audience".to_string(),
        json!({
            "type": "string",
            "enum": ["human", "agent", "all"],
            "default": "human",
            "description": "Which population to report on. `human` is people in browsers, \
        `agent` is named crawlers and AI fetchers, `all` is both. This is the toggle that makes \
        machine traffic visible; leaving it out reports on people only."
        }),
    );
    props.insert(
        "limit".to_string(),
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": MAX_TOOL_ROWS,
            "default": 100,
            "description": "Rows per page for list results."
        }),
    );
    props.insert(
        "page".to_string(),
        json!({
            "type": "integer",
            "minimum": 1,
            "default": 1,
            "description": "1-based page number."
        }),
    );

    props
}

/// The `filters` property, spelled out as real JSON Schema.
fn filter_schema() -> Value {
    json!({
        "type": "array",
        "description": "Conditions, all of which must hold. A `{\"logic\":\"or\",\"filters\":[…]}` \
    group is also accepted in place of the array when alternatives are needed.",
        "items": {
            "type": "object",
            "properties": {
                "dimension": {
                    "type": "string",
                    "description": format!("The column to test. One of: {DIMENSION_NAMES}.")
                },
                "op": {
                    "type": "string",
                    "enum": [
                        "is", "is_not", "contains", "not_contains", "starts_with", "ends_with",
                        "gt", "lt", "gte", "lte", "matches", "is_set", "is_not_set"
                    ]
                },
                "value": {
                    "description": "A string, a number, or an array of either. Omitted for \
    `is_set` and `is_not_set`."
                }
            },
            "required": ["dimension", "op"],
            "additionalProperties": false
        }
    })
}

/// Build one tool definition over the shared envelope schema.
fn envelope_tool(
    name: &str,
    description: &str,
    extra: &[(&str, Value)],
    extra_required: &[&str],
) -> Value {
    let mut props = envelope_properties();
    for (key, schema) in extra {
        props.insert((*key).to_string(), schema.clone());
    }

    let mut required = vec![Value::String("site".to_string())];
    required.extend(
        extra_required
            .iter()
            .map(|r| Value::String((*r).to_string())),
    );

    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": Value::Object(props),
            "required": Value::Array(required),
            "additionalProperties": false
        }
    })
}

/// The twelve tools, in the order a caller would sensibly meet them.
fn tool_definitions() -> Vec<Value> {
    let metric_enum = json!([
        "visitors",
        "sessions",
        "pageviews",
        "bounce_rate",
        "avg_duration",
        "views_per_session",
        "events",
        "conversions",
        "conversion_rate",
        "revenue",
        "agent_fetches",
        "agent_bytes",
        "unique_agents",
        "answer_arrivals",
        "gap_ratio"
    ]);

    let dimension_schema = json!({
        "type": "string",
        "description": format!("The column to group by. One of: {DIMENSION_NAMES}."),
    });

    let metrics_schema = json!({
        "type": "array",
        "items": { "type": "string", "enum": metric_enum },
        "description": "Which metrics to compute for each row. Defaults to visitors and pageviews.",
    });

    vec![
        json!({
            "name": "list_sites",
            "description": "List the sites this key can read, with the numeric `site` id every \
        other tool needs, plus each site's domain and timezone. Call this first.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        envelope_tool(
            "get_overview",
            "Headline totals for a window: visitors, sessions, pageviews, bounce rate, average \
session duration and views per session, each with its change against the comparison window.",
            &[],
            &[],
        ),
        envelope_tool(
            "get_timeseries",
            "One metric bucketed over time, plus the comparison series when `compare` is set. \
Use `metric: \"agent_fetches\"` for crawl volume and `metric: \"gap_ratio\"` for the Citation \
Gap over time.",
            &[(
                "metric",
                json!({
                    "type": "string",
                    "enum": metric_enum,
                    "default": "visitors",
                    "description": "The metric to plot. The metric chooses the trace: \
                `visitors` is always human, `agent_fetches` is always the agent trace, `gap_ratio` reads both."
                }),
            )],
            &[],
        ),
        envelope_tool(
            "get_breakdown",
            "Ranked rows for one dimension, with metrics and each row's share of the total. \
The general-purpose table behind most questions of the form \"which X drove the most Y\".",
            &[
                ("dimension", dimension_schema.clone()),
                ("metrics", metrics_schema.clone()),
            ],
            &["dimension"],
        ),
        envelope_tool(
            "get_pages",
            "Pages with their entries, exits, scroll depth and time on page.",
            &[],
            &[],
        ),
        envelope_tool(
            "get_sources",
            "Acquisition as a hierarchy: channels, then sources, then campaigns. Answer engines \
(ChatGPT, Perplexity, Claude, Gemini, Copilot) are a top-level channel beside search, not a \
kind of referral.",
            &[],
            &[],
        ),
        envelope_tool(
            "get_agent_overview",
            "Agent traffic for a window: fetches, how many distinct named agents, what share of \
those fetches verified against the operator's published addresses, bytes served, and the \
busiest operators and purposes. Always reports the agent trace whatever `audience` says.",
            &[],
            &[],
        ),
        envelope_tool(
            "get_agent_coverage",
            "Crawl coverage per URL: first crawl, last crawl, fetch count, how stale the crawl \
is against the page's last modification, and which agents fetched it. Built from the content \
register, so a page nothing has ever crawled appears with a zero — which is usually the row \
worth finding.",
            &[],
            &[],
        ),
        envelope_tool(
            "get_citation_gap",
            "The Citation Gap. For each URL, how many times AI agents fetched it beside how many \
readers arrived at it from an answer engine, with the ratio and a verdict: `extracted` (taken \
with nothing coming back), `compounding` (the exchange is working), `invisible` (returning \
readers on almost no crawl trace) or `quiet` (no signal yet). Use `view: \"summary\"` for the \
site-level totals and trend instead of the per-URL table.",
            &[(
                "view",
                json!({
                    "type": "string",
                    "enum": ["pages", "summary"],
                    "default": "pages",
                    "description": "`pages` for the per-URL table, `summary` for site totals, \
                the trend and the verdict split."
                }),
            )],
            &[],
        ),
        envelope_tool(
            "get_policy_violations",
            "Agents that fetched a path the site's own robots.txt disallowed them, per agent and \
per path. `compliance` is `violation` only when at least one of those fetches came from an \
address the operator publishes; otherwise it is `unverified`, because a user-agent string is a \
claim and not a fact. Both counts are on every row.",
            &[],
            &[],
        ),
        envelope_tool(
            "get_content_decay",
            "Pages whose human traffic fell sharply while agent traffic held or grew: the \
content the machines still read and people no longer do. Always measured against the window \
immediately before the one requested.",
            &[],
            &[],
        ),
        envelope_tool(
            "run_query",
            "The general query console. With a `dimension` it returns a ranked breakdown for the \
metrics asked for; without one it returns the headline totals. Combine with `filters` and \
`audience` for questions the named tools do not cover.",
            &[("dimension", dimension_schema), ("metrics", metrics_schema)],
            &[],
        ),
    ]
}

// ---------------------------------------------------------------------------
// tools/call
// ---------------------------------------------------------------------------

/// Run one tool and wrap its payload in an MCP tool result.
async fn call_tool(
    st: &AppState,
    user: Uuid,
    grant: &KeyGrant,
    params: &Value,
) -> std::result::Result<Value, RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err(RpcError::protocol(
            INVALID_PARAMS,
            "`tools/call` needs the tool's `name`",
        ));
    };

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match run_tool(st, user, grant, name, &arguments).await {
        Ok(payload) => Ok(json!({
            // Compact rather than pretty: the reader is a context window, and
            // indentation is tokens spent on whitespace.
            "content": [ { "type": "text", "text": payload.to_string() } ],
            "isError": false
        })),
        Err(e) => Err(rpc_error_from(name, e)),
    }
}

/// Dispatch to the query engine.
///
/// Every branch below is the same function the matching HTTP endpoint calls.
/// The tools are a second front door onto the reports, never a second
/// implementation of them.
async fn run_tool(
    st: &AppState,
    user: Uuid,
    grant: &KeyGrant,
    name: &str,
    args: &Value,
) -> micaforge_core::Result<Value> {
    if name == "list_sites" {
        return list_sites(st, user, grant).await;
    }

    // Authorisation happens before the envelope is even built, so an
    // unreachable site cannot be probed through a malformed time range.
    let site = authorize_site(st, user, grant, site_argument(args)?).await?;
    let env = envelope_from_args(&site, args)?;

    match name {
        "get_overview" => json_body(&stats::overview(&st.ch, &env).await?),

        "get_timeseries" => {
            let metric = metric_argument(args)?;
            json_body(&stats::timeseries(&st.ch, &env, metric, None).await?)
        }

        "get_breakdown" => {
            let dimension = dimension_argument(args, true)?
                .ok_or_else(|| Error::validation("dimension", "is required"))?;
            let metrics = metrics_argument(args)?;
            json_body(&stats::breakdown(&st.ch, &env, &dimension, &metrics).await?)
        }

        "get_pages" => json_body(&stats::pages(&st.ch, &env).await?),

        "get_sources" => json_body(&stats::sources(&st.ch, &env).await?),

        "get_agent_overview" => {
            let env = agent_envelope(env);
            json_body(&agents_q::overview(&st.ch, &env).await?)
        }

        "get_agent_coverage" => {
            let env = agent_envelope(env);
            json_body(&agents_q::coverage(&st.ch, &env).await?)
        }

        "get_citation_gap" => match args.get("view").and_then(Value::as_str).unwrap_or("pages") {
            "pages" => json_body(&gap::by_page(&st.ch, &env).await?),
            "summary" => json_body(&gap::summary(&st.ch, &env).await?),
            other => Err(Error::validation(
                "view",
                format!("`{other}` is not a view; expected `pages` or `summary`"),
            )),
        },

        "get_policy_violations" => {
            let env = agent_envelope(env);
            json_body(&super::policy::violations_report(st, &env).await?)
        }

        "get_content_decay" => json_body(&gap::decay(&st.ch, &env).await?),

        "run_query" => match dimension_argument(args, false)? {
            Some(dimension) => {
                let metrics = metrics_argument(args)?;
                json_body(&stats::breakdown(&st.ch, &env, &dimension, &metrics).await?)
            }
            // No dimension is a legitimate query: "the totals, under these
            // filters, for this audience".
            None => json_body(&stats::overview(&st.ch, &env).await?),
        },

        other => Err(Error::validation(
            "name",
            format!("`{other}` is not a tool this server offers; call `tools/list`"),
        )),
    }
}

/// The sites this caller may read.
///
/// Deliberately does not return the internal UUID. An MCP client only ever needs
/// the numeric `site` the envelope takes, and a identifier nothing consumes is
/// an identifier that leaks.
async fn list_sites(st: &AppState, user: Uuid, grant: &KeyGrant) -> micaforge_core::Result<Value> {
    let sites = sites::list_sites_for_user(&st.pg, user).await?;
    let rows: Vec<Value> = sites
        .iter()
        .filter(|site| grant.permits_site(site.org_id, site.id))
        .map(|site| {
            json!({
                "site": site.site_id_u16(),
                "name": site.name,
                "domain": site.domain,
                "timezone": site.timezone,
                "public": site.public,
                "created_at": site.created_at,
            })
        })
        .collect();
    Ok(Value::Array(rows))
}

/// Resolve a numeric site id and prove the caller may read it.
///
/// A site the caller cannot reach is reported as missing rather than forbidden,
/// which is the same answer the REST surface gives for the same reason: with
/// 65,535 possible ids, a `403` would turn this endpoint into a way to enumerate
/// every site on the install.
///
/// Two gates. The key's grant (its organisation, and its site when it has one),
/// checked here explicitly; then the owner's role on the site through the same
/// function the REST surface uses, which also applies the grant.
async fn authorize_site(
    st: &AppState,
    user: Uuid,
    grant: &KeyGrant,
    site_id: u16,
) -> micaforge_core::Result<Site> {
    let missing = || Error::not_found(format!("site {site_id}"));
    let site = sites::get_by_site_id(&st.pg, site_id).await?;

    if !grant.permits_site(site.org_id, site.id) {
        return Err(missing());
    }
    match crate::routes::sites::effective_site_role(&st.pg, &site, user).await {
        Ok(_) => Ok(site),
        Err(Error::NotFound(_)) => Err(missing()),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// The `site` argument, accepting the number or its string spelling.
///
/// Models write `"site": "1"` often enough that refusing it would cost a round
/// trip to teach a lesson nobody learns.
fn site_argument(args: &Value) -> micaforge_core::Result<u16> {
    let raw = args
        .get("site")
        .ok_or_else(|| Error::validation("site", "is required; call `list_sites` first"))?;

    let number = match raw {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    };

    match number {
        Some(n) if n >= 1 && n <= u64::from(u16::MAX) => Ok(n as u16),
        _ => Err(Error::validation("site", "must be a numeric site id")),
    }
}

/// Build the query envelope from a tool's arguments.
///
/// The same resolution the HTTP envelope extractor performs, over JSON instead
/// of a query string, so a tool call and a dashboard request asking for `7d`
/// mean the same seven days.
fn envelope_from_args(site: &Site, args: &Value) -> micaforge_core::Result<QueryEnvelope> {
    let tz = match string_argument(args, "tz") {
        Some(raw) => raw
            .parse::<Tz>()
            .map_err(|_| Error::validation("tz", format!("`{raw}` is not an IANA timezone")))?,
        // A site whose stored zone will not parse is a configuration fault, not
        // a reason to refuse the read: UTC is wrong by hours at worst, and an
        // error here would take the whole site's analytics offline.
        None => site.timezone.parse::<Tz>().unwrap_or(chrono_tz::UTC),
    };

    let range = time::resolve_range(
        string_argument(args, "range"),
        string_argument(args, "start"),
        string_argument(args, "end"),
        tz,
    )?;

    let mut env = QueryEnvelope::new(site.site_id_u16(), range);

    if let Some(raw) = string_argument(args, "compare") {
        env.compare = Compare::parse(raw)?;
    }
    if let Some(raw) = string_argument(args, "granularity") {
        env.granularity =
            micaforge_core::time::fit_granularity(&env.range, Granularity::parse(raw)?);
    }
    if let Some(raw) = string_argument(args, "audience") {
        env.audience = Audience::parse(raw)?;
    }

    if let Some(raw) = args.get("filters").filter(|value| !value.is_null()) {
        // Re-serialised rather than deserialised here, so that the one parser in
        // `micaforge_core::filters` decides what a filter is for this endpoint
        // too, including its depth and size limits.
        let text = serde_json::to_string(raw)
            .map_err(|e| Error::validation("filters", format!("could not be read: {e}")))?;
        let group = filters::parse_filters(&text)?;
        if !group.is_empty() {
            env.filters = Some(group);
        }
    }

    if let Some(limit) = args.get("limit").and_then(Value::as_u64) {
        env.limit = limit.clamp(1, u64::from(MAX_TOOL_ROWS)) as u32;
    } else {
        env.limit = env.limit.min(MAX_TOOL_ROWS);
    }
    if let Some(page) = args.get("page").and_then(Value::as_u64) {
        env.page = page.clamp(1, u64::from(u32::MAX)) as u32;
    }

    Ok(env)
}

/// A trimmed, non-empty string argument.
fn string_argument<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The `metric` argument for `get_timeseries`.
fn metric_argument(args: &Value) -> micaforge_core::Result<Metric> {
    match string_argument(args, "metric") {
        Some(raw) => Metric::parse(raw),
        None => Ok(Metric::Visitors),
    }
}

/// The `metrics` list for a breakdown.
///
/// An empty or absent list is the two numbers every breakdown table shows,
/// rather than an error: a caller who did not say what to count still asked a
/// meaningful question.
fn metrics_argument(args: &Value) -> micaforge_core::Result<Vec<Metric>> {
    let Some(items) = args.get("metrics").and_then(Value::as_array) else {
        return Ok(vec![Metric::Visitors, Metric::Pageviews]);
    };

    let mut metrics = Vec::with_capacity(items.len());
    for item in items {
        let raw = item
            .as_str()
            .ok_or_else(|| Error::validation("metrics", "each entry must be a metric name"))?;
        metrics.push(Metric::parse(raw)?);
    }

    if metrics.is_empty() {
        metrics.push(Metric::Visitors);
        metrics.push(Metric::Pageviews);
    }
    Ok(metrics)
}

/// The `dimension` argument, when the tool takes one.
///
/// `required` only changes the message: a missing dimension on `run_query` is a
/// legitimate request for totals, and on `get_breakdown` it is a mistake worth
/// naming.
fn dimension_argument(args: &Value, required: bool) -> micaforge_core::Result<Option<Dimension>> {
    match string_argument(args, "dimension") {
        Some(raw) => Ok(Some(Dimension::parse(raw)?)),
        None if required => Err(Error::validation(
            "dimension",
            "is required; call `tools/list` for the accepted names",
        )),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bearer_api_key_is_accepted() {
        let mut headers = HeaderMap::new();
        assert!(require_api_key(&headers).is_err());

        headers.insert(header::AUTHORIZATION, "Bearer mfg_abc123".parse().unwrap());
        assert!(require_api_key(&headers).is_ok());

        // The scheme is case-insensitive; the token prefix is not optional.
        headers.insert(header::AUTHORIZATION, "bearer mfg_abc123".parse().unwrap());
        assert!(require_api_key(&headers).is_ok());

        headers.insert(header::AUTHORIZATION, "Bearer sess_abc123".parse().unwrap());
        assert!(require_api_key(&headers).is_err());

        headers.insert(header::AUTHORIZATION, "Basic bWZnXw==".parse().unwrap());
        assert!(require_api_key(&headers).is_err());
    }

    #[test]
    fn a_site_id_arrives_as_a_number_or_as_its_spelling() {
        assert_eq!(site_argument(&json!({ "site": 7 })).unwrap(), 7);
        assert_eq!(site_argument(&json!({ "site": "7" })).unwrap(), 7);
        assert!(site_argument(&json!({})).is_err());
        assert!(site_argument(&json!({ "site": 0 })).is_err());
        assert!(site_argument(&json!({ "site": 70_000 })).is_err());
        assert!(site_argument(&json!({ "site": "docs" })).is_err());
    }

    #[test]
    fn every_tool_carries_a_usable_schema() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 12);

        for tool in &tools {
            let name = tool["name"].as_str().expect("a tool has a name");
            assert!(!name.is_empty());
            assert!(
                tool["description"].as_str().is_some_and(|d| d.len() > 40),
                "{name} needs a description an agent can choose from"
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["inputSchema"]["properties"].is_object());
        }

        // Every site-scoped tool requires the site, and `list_sites` is the one
        // that cannot, because it is how a caller learns the ids.
        for tool in &tools {
            let required = &tool["inputSchema"]["required"];
            if tool["name"] == "list_sites" {
                assert!(required.is_null());
            } else {
                assert!(
                    required
                        .as_array()
                        .is_some_and(|r| r.contains(&json!("site"))),
                    "{} must require a site",
                    tool["name"]
                );
            }
        }
    }

    #[test]
    fn tool_names_are_the_ones_the_contract_lists() {
        let names: Vec<String> = tool_definitions()
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default().to_string())
            .collect();

        for expected in [
            "list_sites",
            "get_overview",
            "get_timeseries",
            "get_breakdown",
            "get_pages",
            "get_sources",
            "get_agent_overview",
            "get_agent_coverage",
            "get_citation_gap",
            "get_policy_violations",
            "get_content_decay",
            "run_query",
        ] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
    }

    #[test]
    fn a_store_failure_never_leaks_its_query_text() {
        let leaky = Error::Database("SELECT ... WHERE pathname = '/secret'".to_string());
        let rpc = rpc_error_from("get_pages", leaky);
        assert_eq!(rpc.code, INTERNAL_ERROR);
        assert!(!rpc.message.contains("SELECT"));
        assert_eq!(rpc.data.as_ref().unwrap()["code"], "database_error");

        // A caller's own mistake is handed back verbatim, with the field named.
        let mine = Error::validation("range", "unknown range `7q`");
        let rpc = rpc_error_from("get_overview", mine);
        assert_eq!(rpc.code, INVALID_PARAMS);
        assert!(rpc.message.contains("7q"));
        assert_eq!(rpc.data.as_ref().unwrap()["field"], "range");
    }

    #[test]
    fn a_failed_response_omits_data_it_has_none_of() {
        let err = RpcError::protocol(METHOD_NOT_FOUND, "no such method");
        let body = rpc_fail(json!(3), &err);
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 3);
        assert_eq!(body["error"]["code"], METHOD_NOT_FOUND);
        assert!(body["error"].get("data").is_none());
    }

    #[test]
    fn metrics_default_rather_than_refuse() {
        let defaults = metrics_argument(&json!({})).unwrap();
        assert_eq!(defaults, vec![Metric::Visitors, Metric::Pageviews]);
        assert_eq!(
            metrics_argument(&json!({ "metrics": [] })).unwrap(),
            defaults
        );
        assert_eq!(
            metrics_argument(&json!({ "metrics": ["agent_fetches"] })).unwrap(),
            vec![Metric::AgentFetches]
        );
        assert!(metrics_argument(&json!({ "metrics": ["nonsense"] })).is_err());
        assert!(metrics_argument(&json!({ "metrics": [7] })).is_err());
    }
}
