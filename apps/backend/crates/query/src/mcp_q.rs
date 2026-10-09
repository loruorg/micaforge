//! MCP server analytics: the reports over `mcp_calls`.
//!
//! The rows are measurements an MCP server's own process made of the requests
//! it answered (`tools/call`, `resources/read`, `prompts/get`), sent by the
//! server SDK's wrapper. Five readings of them, one per question an operator
//! asks of a server they run: how much and how well ([`overview`]), when
//! ([`timeseries`]), which tools ([`tools`]), which clients ([`clients`]), and
//! which single calls were the slowest or failed ([`calls`]).
//!
//! Every function here is the one the HTTP route, the MCP read tool and the AI
//! analyst all call, so the three cannot describe the same server differently.
//!
//! ## What "error" means here
//!
//! Two kinds of failure are kept apart, because they have different owners:
//!
//! - `error`: the request failed. The handler threw, or the server answered
//!   with a JSON-RPC error. This is the server's problem.
//! - `tool_error`: the request succeeded and the tool reported a failure in
//!   its result (`isError: true`), which MCP defines as a result the model is
//!   meant to read and react to. Often the caller's input, not a fault.
//!
//! The error rate counts both, and every report also carries them apart.
//!
//! ## Latency
//!
//! Percentiles are `quantileTiming`, ClickHouse's estimator built for response
//! times in milliseconds, which is exact below 1,024 ms and accurate to a
//! millisecond-scale bucket above it. An empty window answers `0`, never `NaN`:
//! RowBinary has no way to say "no value", and a `NaN` would reach a JSON
//! encoder that refuses it.

use chrono::{DateTime, Utc};
use clickhouse::Row;
use micaforge_core::{Error, QueryEnvelope, Result};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::fetch_all;
use crate::sql::{bucket_millis, fmt_ts, Param};

/// The methods the wrapper measures. Anything else is refused as a filter.
pub const METHODS: &[&str] = &["tools/call", "resources/read", "prompts/get"];

/// Most rows a list report returns.
const MAX_LIST: u32 = 1_000;

/// Most single calls the calls report returns.
const MAX_CALLS: u32 = 500;

/// Most servers the overview lists.
const MAX_SERVERS: u64 = 50;

/// The narrowing a caller may add to the site and window.
///
/// Every field is matched exactly and bound as a parameter. A server, a
/// method and a client are all values the reports themselves return, so an
/// exact match is the only operator a screen needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpFilter {
    /// Only this server (`serverInfo.name`).
    #[serde(default)]
    pub server: Option<String>,
    /// Only this method, one of [`METHODS`].
    #[serde(default)]
    pub method: Option<String>,
    /// Only this client (`clientInfo.name`).
    #[serde(default)]
    pub client: Option<String>,
}

impl McpFilter {
    /// Trim the fields, drop empty ones, and refuse a method that is not one
    /// the wrapper measures.
    ///
    /// A method outside [`METHODS`] cannot match a row, so it is refused by
    /// name rather than answered with an empty report that looks like a
    /// server nobody called.
    pub fn normalised(self) -> Result<McpFilter> {
        let clean = |v: Option<String>| {
            v.map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(200).collect::<String>())
        };
        let method = clean(self.method);
        if let Some(m) = method.as_deref() {
            if !METHODS.contains(&m) {
                return Err(Error::validation(
                    "method",
                    format!("must be one of: {}", METHODS.join(", ")),
                ));
            }
        }
        Ok(McpFilter {
            server: clean(self.server),
            method,
            client: clean(self.client),
        })
    }
}

/// The `WHERE` every report shares, and its parameters.
///
/// Public so a caller can show the read it ran; the strings formatted into it
/// are this module's own column names.
pub fn scope(env: &QueryEnvelope, filter: &McpFilter) -> (String, Vec<Param>) {
    let mut sql = "site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC') \
                   AND timestamp < toDateTime64(?, 3, 'UTC')"
        .to_string();
    let mut params = vec![
        Param::U16(env.site),
        Param::Str(fmt_ts(env.range.start)),
        Param::Str(fmt_ts(env.range.end)),
    ];
    for (column, value) in [
        ("server_name", &filter.server),
        ("method", &filter.method),
        ("client_name", &filter.client),
    ] {
        if let Some(value) = value {
            sql.push_str(&format!(" AND {column} = ?"));
            params.push(Param::Str(value.clone()));
        }
    }
    (sql, params)
}

/// The percentile expression, guarded so an empty group reads `0`.
fn quantile(level: &str) -> String {
    format!("if(count() = 0, 0, toFloat64(quantileTiming({level})(duration_ms)))")
}

/// `part / whole`, rounded to four places; zero when nothing happened.
fn rate(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    ((part as f64 / whole as f64) * 10_000.0).round() / 10_000.0
}

/// A millisecond figure, rounded to one place.
fn ms(value: f64) -> f64 {
    if value.is_finite() {
        (value * 10.0).round() / 10.0
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

/// One server seen in the window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerSeen {
    /// `serverInfo.name`, empty when the SDK could not read it.
    pub name: String,
    /// The version seen most recently.
    pub version: String,
    /// Calls it answered.
    pub calls: u64,
    /// When it last answered one.
    pub last_seen: DateTime<Utc>,
}

/// The headline figures.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct McpOverview {
    /// Every measured call.
    pub calls: u64,
    /// Calls that failed: a thrown handler or a JSON-RPC error.
    pub errors: u64,
    /// Calls a tool answered with `isError: true`.
    pub tool_errors: u64,
    /// `(errors + tool_errors) / calls`, between 0 and 1.
    pub error_rate: f64,
    /// Median handler time, in milliseconds.
    pub p50_ms: f64,
    /// 95th percentile handler time, in milliseconds.
    pub p95_ms: f64,
    /// Distinct MCP sessions, from the SDK's digest of the session id.
    pub sessions: u64,
    /// Distinct clients by `clientInfo.name`.
    pub clients: u64,
    /// Distinct tools called.
    pub tools: u64,
    /// The servers that sent calls, busiest first.
    pub servers: Vec<McpServerSeen>,
}

#[derive(Debug, Row, Deserialize)]
struct OverviewRaw {
    calls: u64,
    errors: u64,
    tool_errors: u64,
    p50: f64,
    p95: f64,
    sessions: u64,
    clients: u64,
    tools: u64,
}

#[derive(Debug, Row, Deserialize)]
struct ServerRaw {
    name: String,
    version: String,
    calls: u64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    last_seen: DateTime<Utc>,
}

/// `GET /api/stats/mcp/overview`.
pub async fn overview(ch: &Ch, env: &QueryEnvelope, filter: &McpFilter) -> Result<McpOverview> {
    let (scope, params) = scope(env, filter);
    let sql = format!(
        "SELECT
  toUInt64(count()) AS calls,
  toUInt64(countIf(status = 'error')) AS errors,
  toUInt64(countIf(status = 'tool_error')) AS tool_errors,
  {p50} AS p50,
  {p95} AS p95,
  toUInt64(uniqExactIf(session_hash, session_hash != '')) AS sessions,
  toUInt64(uniqExactIf(client_name, client_name != '')) AS clients,
  toUInt64(uniqExactIf(name, method = 'tools/call' AND name != '')) AS tools
FROM mcp_calls
WHERE {scope}",
        p50 = quantile("0.5"),
        p95 = quantile("0.95"),
    );
    let head = fetch_all::<OverviewRaw>(ch, "mcp_overview", &sql, &params)
        .await?
        .into_iter()
        .next();

    let mut server_params = params.clone();
    server_params.push(Param::U64(MAX_SERVERS));
    let servers = fetch_all::<ServerRaw>(
        ch,
        "mcp_servers",
        &format!(
            "SELECT toString(server_name) AS name,
  argMax(toString(server_version), timestamp) AS version,
  toUInt64(count()) AS calls,
  max(timestamp) AS last_seen
FROM mcp_calls
WHERE {scope}
GROUP BY name
ORDER BY calls DESC, name ASC
LIMIT ?"
        ),
        &server_params,
    )
    .await?;

    let Some(head) = head else {
        return Ok(McpOverview::default());
    };
    Ok(McpOverview {
        calls: head.calls,
        errors: head.errors,
        tool_errors: head.tool_errors,
        error_rate: rate(head.errors + head.tool_errors, head.calls),
        p50_ms: ms(head.p50),
        p95_ms: ms(head.p95),
        sessions: head.sessions,
        clients: head.clients,
        tools: head.tools,
        servers: servers
            .into_iter()
            .map(|s| McpServerSeen {
                name: s.name,
                version: s.version,
                calls: s.calls,
                last_seen: s.last_seen,
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Over time
// ---------------------------------------------------------------------------

/// One bucket of the window.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct McpBucket {
    /// The bucket's start, epoch milliseconds.
    pub t: i64,
    /// Calls in the bucket.
    pub calls: u64,
    /// Failed calls and tool errors together.
    pub errors: u64,
    /// Median handler time in the bucket.
    pub p50_ms: f64,
    /// 95th percentile handler time in the bucket.
    pub p95_ms: f64,
}

#[derive(Debug, Row, Deserialize)]
struct BucketRaw {
    t: i64,
    calls: u64,
    errors: u64,
    p50: f64,
    p95: f64,
}

/// `GET /api/stats/mcp/timeseries`.
///
/// Only buckets that had a call come back, in time order, like every other
/// series this crate reads; the HTTP route fills the empty ones in.
pub async fn timeseries(
    ch: &Ch,
    env: &QueryEnvelope,
    filter: &McpFilter,
) -> Result<Vec<McpBucket>> {
    let (scope, params) = scope(env, filter);
    let sql = format!(
        "SELECT {t} AS t,
  toUInt64(count()) AS calls,
  toUInt64(countIf(status != 'ok')) AS errors,
  {p50} AS p50,
  {p95} AS p95
FROM mcp_calls
WHERE {scope}
GROUP BY t
ORDER BY t ASC",
        t = bucket_millis(env.granularity, env.range.tz, "timestamp"),
        p50 = quantile("0.5"),
        p95 = quantile("0.95"),
    );
    Ok(fetch_all::<BucketRaw>(ch, "mcp_timeseries", &sql, &params)
        .await?
        .into_iter()
        .map(|r| McpBucket {
            t: r.t,
            calls: r.calls,
            errors: r.errors,
            p50_ms: ms(r.p50),
            p95_ms: ms(r.p95),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// One tool, resource or prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolRow {
    /// `tools/call`, `resources/read` or `prompts/get`.
    pub method: String,
    /// The tool or prompt name, or the resource URI.
    pub name: String,
    /// Calls.
    pub calls: u64,
    /// Failed calls.
    pub errors: u64,
    /// Tool errors (`isError: true`).
    pub tool_errors: u64,
    /// `(errors + tool_errors) / calls`.
    pub error_rate: f64,
    /// Median handler time.
    pub p50_ms: f64,
    /// 95th percentile handler time.
    pub p95_ms: f64,
    /// Mean size of the arguments, in bytes.
    pub avg_arg_bytes: f64,
    /// Mean size of the result, in bytes.
    pub avg_result_bytes: f64,
    /// Distinct clients that called it.
    pub clients: u64,
    /// The commonest error class among its failed calls, or empty.
    pub top_error: String,
}

#[derive(Debug, Row, Deserialize)]
struct ToolRaw {
    method: String,
    name: String,
    calls: u64,
    errors: u64,
    tool_errors: u64,
    p50: f64,
    p95: f64,
    arg_bytes: f64,
    result_bytes: f64,
    clients: u64,
    top_error: String,
}

/// `GET /api/stats/mcp/tools`: busiest first.
pub async fn tools(ch: &Ch, env: &QueryEnvelope, filter: &McpFilter) -> Result<Vec<McpToolRow>> {
    let (scope, mut params) = scope(env, filter);
    params.push(Param::U64(u64::from(env.limit.clamp(1, MAX_LIST))));
    let sql = format!(
        "SELECT toString(method) AS method, name,
  toUInt64(count()) AS calls,
  toUInt64(countIf(status = 'error')) AS errors,
  toUInt64(countIf(status = 'tool_error')) AS tool_errors,
  {p50} AS p50,
  {p95} AS p95,
  toFloat64(avg(arg_bytes)) AS arg_bytes,
  toFloat64(avg(result_bytes)) AS result_bytes,
  toUInt64(uniqExactIf(client_name, client_name != '')) AS clients,
  topKIf(1)(toString(error_type), status = 'error' AND error_type != '')[1] AS top_error
FROM mcp_calls
WHERE {scope}
GROUP BY method, name
ORDER BY calls DESC, method ASC, name ASC
LIMIT ?",
        p50 = quantile("0.5"),
        p95 = quantile("0.95"),
    );
    Ok(fetch_all::<ToolRaw>(ch, "mcp_tools", &sql, &params)
        .await?
        .into_iter()
        .map(|r| McpToolRow {
            error_rate: rate(r.errors + r.tool_errors, r.calls),
            method: r.method,
            name: r.name,
            calls: r.calls,
            errors: r.errors,
            tool_errors: r.tool_errors,
            p50_ms: ms(r.p50),
            p95_ms: ms(r.p95),
            avg_arg_bytes: ms(r.arg_bytes),
            avg_result_bytes: ms(r.result_bytes),
            clients: r.clients,
            top_error: r.top_error,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Clients
// ---------------------------------------------------------------------------

/// One MCP client, by the name it gave in `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpClientRow {
    /// `clientInfo.name`, such as `claude-ai` or `cursor-vscode`. Empty when
    /// the client sent none or the SDK could not read it.
    pub client: String,
    /// Its commonest versions, at most three.
    pub versions: Vec<String>,
    /// Calls it made.
    pub calls: u64,
    /// Its share of every call in the window, between 0 and 1.
    pub share: f64,
    /// Distinct sessions it opened.
    pub sessions: u64,
    /// Failed calls and tool errors together.
    pub errors: u64,
    /// `errors / calls`.
    pub error_rate: f64,
    /// Median handler time for its calls.
    pub p50_ms: f64,
}

#[derive(Debug, Row, Deserialize)]
struct ClientRaw {
    client: String,
    versions: Vec<String>,
    calls: u64,
    sessions: u64,
    errors: u64,
    p50: f64,
}

/// `GET /api/stats/mcp/clients`: busiest first.
pub async fn clients(
    ch: &Ch,
    env: &QueryEnvelope,
    filter: &McpFilter,
) -> Result<Vec<McpClientRow>> {
    let (scope, mut params) = scope(env, filter);
    params.push(Param::U64(u64::from(env.limit.clamp(1, MAX_LIST))));
    let sql = format!(
        "SELECT toString(client_name) AS client,
  arrayFilter(v -> v != '', topK(3)(toString(client_version))) AS versions,
  toUInt64(count()) AS calls,
  toUInt64(uniqExactIf(session_hash, session_hash != '')) AS sessions,
  toUInt64(countIf(status != 'ok')) AS errors,
  {p50} AS p50
FROM mcp_calls
WHERE {scope}
GROUP BY client
ORDER BY calls DESC, client ASC
LIMIT ?",
        p50 = quantile("0.5"),
    );
    let rows = fetch_all::<ClientRaw>(ch, "mcp_clients", &sql, &params).await?;
    Ok(shape_clients(rows))
}

/// Shares are of the rows returned, which is every call unless the list was
/// cut at its limit; a client past the limit has a share too small to read.
fn shape_clients(rows: Vec<ClientRaw>) -> Vec<McpClientRow> {
    let total: u64 = rows.iter().map(|r| r.calls).sum();
    rows.into_iter()
        .map(|r| McpClientRow {
            share: rate(r.calls, total),
            error_rate: rate(r.errors, r.calls),
            client: r.client,
            versions: r.versions,
            calls: r.calls,
            sessions: r.sessions,
            errors: r.errors,
            p50_ms: ms(r.p50),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Single calls
// ---------------------------------------------------------------------------

/// Which calls to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallsView {
    /// The slowest calls, slowest first.
    Slowest,
    /// Failed calls and tool errors, newest first.
    Failing,
}

impl CallsView {
    /// Parse the wire spelling; absent means [`CallsView::Slowest`].
    pub fn parse(raw: Option<&str>) -> Result<CallsView> {
        match raw.map(str::trim).unwrap_or("slowest") {
            "" | "slowest" => Ok(CallsView::Slowest),
            "failing" => Ok(CallsView::Failing),
            other => Err(Error::validation(
                "view",
                format!("`{other}` is not a view; expected `slowest` or `failing`"),
            )),
        }
    }

    /// The wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            CallsView::Slowest => "slowest",
            CallsView::Failing => "failing",
        }
    }
}

/// One measured call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Row)]
pub struct McpCallRecord {
    /// When it started.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// The server that answered it.
    pub server_name: String,
    /// The JSON-RPC method.
    pub method: String,
    /// The tool, prompt or resource.
    pub name: String,
    /// The client that sent it.
    pub client_name: String,
    /// That client's version.
    pub client_version: String,
    /// The transport it arrived on.
    pub transport: String,
    /// How long the handler took.
    pub duration_ms: u32,
    /// `ok`, `error` or `tool_error`.
    pub status: String,
    /// The error's class, never its message.
    pub error_type: String,
    /// The JSON-RPC error code, `0` when there was none.
    pub error_code: i32,
    /// Size of the arguments.
    pub arg_bytes: u32,
    /// Size of the result.
    pub result_bytes: u32,
}

/// `GET /api/stats/mcp/calls?view=slowest|failing`.
///
/// The stored arguments are never part of this list, even when the operator
/// opted in to capturing them: the list is read by everyone who can read the
/// site, and arguments are the one column that can hold what a user typed.
pub async fn calls(
    ch: &Ch,
    env: &QueryEnvelope,
    filter: &McpFilter,
    view: CallsView,
) -> Result<Vec<McpCallRecord>> {
    let (scope, mut params) = scope(env, filter);
    params.push(Param::U64(u64::from(env.limit.clamp(1, MAX_CALLS))));
    let (extra, order) = match view {
        CallsView::Slowest => ("", "duration_ms DESC, timestamp DESC"),
        CallsView::Failing => (" AND status != 'ok'", "timestamp DESC"),
    };
    let sql = format!(
        "SELECT timestamp, toString(server_name) AS server_name, toString(method) AS method,
  name, toString(client_name) AS client_name, toString(client_version) AS client_version,
  toString(transport) AS transport, duration_ms, toString(status) AS status,
  toString(error_type) AS error_type, error_code, arg_bytes, result_bytes
FROM mcp_calls
WHERE {scope}{extra}
ORDER BY {order}
LIMIT ?"
    );
    fetch_all::<McpCallRecord>(ch, "mcp_calls", &sql, &params).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::placeholder_count;
    use chrono::TimeZone;
    use micaforge_core::TimeRange;

    fn env() -> QueryEnvelope {
        QueryEnvelope::new(
            3,
            TimeRange::new(
                Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 10, 8, 0, 0, 0).unwrap(),
                chrono_tz::UTC,
            ),
        )
    }

    #[test]
    fn the_scope_binds_one_parameter_per_slot() {
        let (sql, params) = scope(&env(), &McpFilter::default());
        assert_eq!(placeholder_count(&sql), params.len());
        assert_eq!(params.len(), 3);

        let filter = McpFilter {
            server: Some("docs".into()),
            method: Some("tools/call".into()),
            client: Some("claude-ai".into()),
        };
        let (sql, params) = scope(&env(), &filter);
        assert_eq!(placeholder_count(&sql), params.len());
        assert_eq!(params.len(), 6);
        assert!(sql.contains("server_name = ?") && sql.contains("client_name = ?"));
        // The values travel as parameters, never as SQL text.
        assert!(!sql.contains("docs") && !sql.contains("claude-ai"));
    }

    #[test]
    fn a_filter_is_trimmed_and_an_unknown_method_is_refused() {
        let clean = McpFilter {
            server: Some("  docs  ".into()),
            method: Some(" ".into()),
            client: None,
        }
        .normalised()
        .unwrap();
        assert_eq!(clean.server.as_deref(), Some("docs"));
        assert_eq!(clean.method, None);

        let bad = McpFilter {
            method: Some("tools/list".into()),
            ..McpFilter::default()
        }
        .normalised();
        assert!(bad.is_err(), "a method the wrapper never measures");
        for method in METHODS {
            let ok = McpFilter {
                method: Some((*method).into()),
                ..McpFilter::default()
            }
            .normalised();
            assert!(ok.is_ok(), "{method}");
        }
    }

    #[test]
    fn rates_are_fractions_and_nothing_divided_is_zero() {
        assert_eq!(rate(0, 0), 0.0);
        assert_eq!(rate(1, 4), 0.25);
        assert_eq!(rate(1, 3), 0.3333);
        assert_eq!(ms(f64::NAN), 0.0);
        assert_eq!(ms(12.345), 12.3);
    }

    #[test]
    fn client_shares_add_up_to_the_whole() {
        let raw = |client: &str, calls: u64, errors: u64| ClientRaw {
            client: client.into(),
            versions: vec!["1.0.0".into()],
            calls,
            sessions: 1,
            errors,
            p50: 10.0,
        };
        let rows = shape_clients(vec![
            raw("claude-ai", 60, 6),
            raw("cursor-vscode", 30, 0),
            raw("", 10, 5),
        ]);
        assert_eq!(rows[0].share, 0.6);
        assert_eq!(rows[0].error_rate, 0.1);
        assert_eq!(rows[2].error_rate, 0.5);
        let total: f64 = rows.iter().map(|r| r.share).sum();
        assert!((total - 1.0).abs() < 1e-9);
        assert!(shape_clients(Vec::new()).is_empty());
    }

    #[test]
    fn the_calls_view_parses_its_two_spellings_only() {
        assert_eq!(CallsView::parse(None).unwrap(), CallsView::Slowest);
        assert_eq!(
            CallsView::parse(Some("failing")).unwrap(),
            CallsView::Failing
        );
        assert!(CallsView::parse(Some("recent")).is_err());
        assert_eq!(CallsView::Failing.as_str(), "failing");
    }

    #[test]
    fn the_record_reads_the_columns_the_query_selects_in_order() {
        // RowBinary is positional: the struct's fields are the SELECT list.
        assert_eq!(
            McpCallRecord::COLUMN_NAMES,
            [
                "timestamp",
                "server_name",
                "method",
                "name",
                "client_name",
                "client_version",
                "transport",
                "duration_ms",
                "status",
                "error_type",
                "error_code",
                "arg_bytes",
                "result_bytes",
            ]
        );
    }
}
