//! `/api/uptime/*` — monitors, their checks, and their incidents.
//!
//! A monitor is a URL this server requests on its own schedule
//! ([`micaforge_jobs::uptime`] runs the checks). This module is the
//! dashboard's side of it: creating and editing monitors, running one check on
//! demand, and reading the results back out of ClickHouse for the Uptime
//! screen.
//!
//! ## A monitor is validated as completely as an alert
//!
//! The sweep runs with nobody watching, so everything it will need is checked
//! here, at write time, against the same functions it will use: the URL through
//! the outbound guard ([`micaforge_jobs::outbound::guard_url`]), the JSON path
//! through the parser the check runs ([`probe::parse_json_path`]), the channels
//! through the alert screen's own validator. A monitor that saves is a monitor
//! the sweep can run.
//!
//! ## A test check is still an outbound request
//!
//! "Test now" runs a real check from this server, so it goes through the same
//! guard and is limited per site. It records nothing and changes no state: the
//! answer is for the person looking at the form.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_core::{new_id, Config, Error, Result};
use micaforge_jobs::uptime::{self as job, probe};
use micaforge_query::exec::fetch_all;
use micaforge_query::sql::{fmt_ts, Param};
use micaforge_store::models::uptime::{self, Monitor, MonitorFields};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{require_at_least, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Most monitors one site may hold.
///
/// Every monitor is a request this server makes on a schedule. A hosted
/// install with open sign-up must not let one account turn it into a load
/// generator; fifty is far past what one property needs.
const MAX_MONITORS_PER_SITE: usize = 50;

/// "Test now" presses one site may make per minute.
const TEST_LIMIT_PER_MINUTE: u32 = 20;

/// Longest name, keyword, path or expected value accepted.
const MAX_TEXT: usize = 200;

/// Longest URL accepted.
const MAX_URL: usize = 2048;

/// Every uptime route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/uptime/monitors",
            get(list_monitors).post(create_monitor),
        )
        .route(
            "/api/uptime/monitors/{id}",
            get(get_monitor)
                .patch(update_monitor)
                .delete(delete_monitor),
        )
        .route("/api/uptime/monitors/{id}/test", post(test_monitor))
        .route("/api/uptime/monitors/{id}/series", get(monitor_series))
        .route("/api/uptime/monitors/{id}/checks", get(monitor_checks))
        .route("/api/uptime/test", post(test_draft))
        .route("/api/uptime/summary", get(summary))
        .route("/api/uptime/incidents", get(incidents))
}

// ---------------------------------------------------------------------------
// Monitors
// ---------------------------------------------------------------------------

/// A monitor as the dashboard sends it. Everything but `name` and `url` has a
/// default.
#[derive(Debug, Deserialize)]
pub(crate) struct MonitorBody {
    name: String,
    url: String,
    #[serde(default = "default_method")]
    method: String,
    #[serde(default = "default_interval")]
    interval_minutes: i64,
    #[serde(default = "default_timeout")]
    timeout_ms: i64,
    #[serde(default = "default_status_min")]
    status_min: i64,
    #[serde(default = "default_status_max")]
    status_max: i64,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    json_path: Option<String>,
    #[serde(default)]
    json_expected: Option<String>,
    #[serde(default = "yes")]
    ssl_check: bool,
    #[serde(default = "default_ssl_days")]
    ssl_expiry_days: i64,
    #[serde(default = "default_threshold")]
    failure_threshold: i64,
    #[serde(default)]
    channels: Option<Value>,
    #[serde(default = "yes")]
    enabled: bool,
}

fn default_method() -> String {
    "GET".to_string()
}
/// Five minutes: frequent enough to notice an outage within one coffee.
fn default_interval() -> i64 {
    5
}
fn default_timeout() -> i64 {
    10_000
}
fn default_status_min() -> i64 {
    200
}
fn default_status_max() -> i64 {
    299
}
fn yes() -> bool {
    true
}
/// Two weeks, the window in which a renewal that failed can still be fixed
/// without anybody's browser showing a warning.
fn default_ssl_days() -> i64 {
    14
}
/// Two in a row: one lost packet is not an outage.
fn default_threshold() -> i64 {
    2
}

/// Validate a body into the fields the store writes.
pub(crate) fn fields_from(cfg: &Config, body: &MonitorBody) -> Result<MonitorFields> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(Error::validation("name", "must be 1 to 120 characters"));
    }

    let raw_url = body.url.trim();
    if raw_url.len() > MAX_URL {
        return Err(Error::validation(
            "url",
            format!("must be at most {MAX_URL} characters"),
        ));
    }
    let url = micaforge_jobs::outbound::guard_url(raw_url).map_err(|e| {
        Error::validation(
            "url",
            e.to_string()
                .trim_start_matches("invalid url: ")
                .to_string(),
        )
    })?;

    let method = body.method.trim().to_ascii_uppercase();
    if !matches!(method.as_str(), "GET" | "HEAD" | "POST") {
        return Err(Error::validation("method", "must be GET, HEAD or POST"));
    }

    let interval = in_range(body.interval_minutes, 1, 60, "interval_minutes", "minutes")?;
    let timeout_ms = in_range(body.timeout_ms, 1_000, 30_000, "timeout_ms", "milliseconds")?;
    let status_min = in_range(body.status_min, 100, 599, "status_min", "")?;
    let status_max = in_range(body.status_max, 100, 599, "status_max", "")?;
    if status_min > status_max {
        return Err(Error::validation(
            "status_min",
            "must not be greater than status_max",
        ));
    }

    let keyword = optional_text(body.keyword.as_deref(), "keyword")?;
    let json_path = optional_text(body.json_path.as_deref(), "json_path")?;
    let json_expected = optional_text(body.json_expected.as_deref(), "json_expected")?;
    if let Some(path) = json_path.as_deref() {
        probe::parse_json_path(path).map_err(|e| Error::validation("json_path", e))?;
    } else if json_expected.is_some() {
        return Err(Error::validation(
            "json_expected",
            "needs a JSON path to compare against",
        ));
    }
    if method == "HEAD" && (keyword.is_some() || json_path.is_some()) {
        return Err(Error::validation(
            "method",
            "a HEAD request has no body, so a keyword or JSON check needs GET or POST",
        ));
    }

    let ssl_expiry_days = in_range(body.ssl_expiry_days, 1, 365, "ssl_expiry_days", "days")?;
    let failure_threshold = in_range(body.failure_threshold, 1, 10, "failure_threshold", "")?;

    // A monitor may notify nobody: the screen is the notification. Anything
    // that names a channel is held to the alert screen's rules.
    let channels = match body.channels.as_ref() {
        None | Some(Value::Null) => json!([]),
        Some(value)
            if micaforge_jobs::notify::Channels::from_json(value).is_empty() && is_blank(value) =>
        {
            json!([])
        }
        Some(value) => crate::routes::notify::check_channels(cfg, value, "channels")?,
    };

    Ok(MonitorFields {
        name: name.to_string(),
        url: url.to_string(),
        method,
        interval_secs: interval * 60,
        timeout_ms,
        status_min,
        status_max,
        keyword,
        json_path,
        json_expected,
        ssl_check: body.ssl_check,
        ssl_expiry_days,
        failure_threshold,
        channels,
        enabled: body.enabled,
    })
}

/// True for a channels document that names nothing at all: `[]`, `{}`, or an
/// object whose every value is empty. Such a document means "no channels",
/// which is allowed; a document with something in it that the parser cannot
/// read is refused instead.
fn is_blank(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.values().all(|v| match v {
            Value::Null => true,
            Value::String(s) => s.trim().is_empty(),
            Value::Array(items) => items.is_empty(),
            _ => false,
        }),
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// An integer inside an inclusive range, as an `i32`.
fn in_range(value: i64, min: i64, max: i64, field: &str, unit: &str) -> Result<i32> {
    if !(min..=max).contains(&value) {
        let unit = if unit.is_empty() {
            String::new()
        } else {
            format!(" {unit}")
        };
        return Err(Error::validation(
            field,
            format!("must be from {min} to {max}{unit}"),
        ));
    }
    i32::try_from(value).map_err(|_| Error::validation(field, "is out of range"))
}

/// An optional short text: trimmed of nothing (a keyword's spaces are part of
/// it), empty read as absent, held to [`MAX_TEXT`].
fn optional_text(value: Option<&str>, field: &str) -> Result<Option<String>> {
    match value {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) if s.chars().count() > MAX_TEXT => Err(Error::validation(
            field,
            format!("must be at most {MAX_TEXT} characters"),
        )),
        Some(s) => Ok(Some(s.to_string())),
    }
}

/// One monitor on the wire.
fn monitor_json(st: &AppState, m: &Monitor) -> Value {
    json!({
        "id": m.id,
        "site_id": m.site_id,
        "name": m.name,
        "url": m.url,
        "method": m.method,
        "interval_minutes": m.interval_secs / 60,
        "timeout_ms": m.timeout_ms,
        "status_min": m.status_min,
        "status_max": m.status_max,
        "keyword": m.keyword,
        "json_path": m.json_path,
        "json_expected": m.json_expected,
        "ssl_check": m.ssl_check,
        "ssl_expiry_days": m.ssl_expiry_days,
        "failure_threshold": m.failure_threshold,
        "channels": m.channels,
        "enabled": m.enabled,
        "state": m.state,
        "consecutive_failures": m.consecutive_failures,
        "last_checked_at": m.last_checked_at,
        "next_check_at": m.next_check_at,
        "region": st.cfg.region,
        "created_at": m.created_at,
    })
}

/// `GET /api/uptime/monitors?site=`
async fn list_monitors(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    let rows = uptime::list_for_site(&st.pg, access.site.id).await?;
    Ok(ok(rows
        .iter()
        .map(|m| monitor_json(&st, m))
        .collect::<Vec<_>>()))
}

/// `POST /api/uptime/monitors?site=`
async fn create_monitor(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<MonitorBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, MEMBER)?;
    let fields = fields_from(&st.cfg, &body)?;
    let existing = uptime::list_for_site(&st.pg, access.site.id).await?;
    if existing.len() >= MAX_MONITORS_PER_SITE {
        return Err(Error::Conflict(format!(
            "a site can have at most {MAX_MONITORS_PER_SITE} monitors"
        ))
        .into());
    }
    let monitor = uptime::create(&st.pg, new_id(), access.site.id, &fields).await?;
    Ok(ok(monitor_json(&st, &monitor)))
}

/// `GET /api/uptime/monitors/{id}`
async fn get_monitor(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let monitor = reachable(&st, id, auth.user.id, VIEWER).await?;
    Ok(ok(monitor_json(&st, &monitor)))
}

/// `PATCH /api/uptime/monitors/{id}`
async fn update_monitor(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<MonitorBody>,
) -> ApiResult<Json<Value>> {
    let monitor = reachable(&st, id, auth.user.id, MEMBER).await?;
    let fields = fields_from(&st.cfg, &body)?;
    let updated = uptime::update(&st.pg, monitor.id, &fields).await?;
    Ok(ok(monitor_json(&st, &updated)))
}

/// `DELETE /api/uptime/monitors/{id}`
async fn delete_monitor(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let monitor = reachable(&st, id, auth.user.id, MEMBER).await?;
    uptime::delete(&st.pg, monitor.id).await?;
    Ok(ok(json!({ "deleted": monitor.id })))
}

/// Load a monitor and assert the caller's role on its site.
async fn reachable(st: &AppState, id: Uuid, user: Uuid, needed: &str) -> Result<Monitor> {
    let monitor = uptime::get(&st.pg, id).await?;
    match authorize_site(&st.pg, monitor.site_id, user, needed).await {
        Ok(_) => Ok(monitor),
        // A monitor on a site the caller cannot see does not exist for them.
        Err(Error::NotFound(_)) => Err(Error::not_found("monitor")),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Checking on demand
// ---------------------------------------------------------------------------

/// `POST /api/uptime/monitors/{id}/test` — check a saved monitor now.
async fn test_monitor(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let monitor = reachable(&st, id, auth.user.id, MEMBER).await?;
    limit_tests(&st, monitor.site_id).await?;
    let outcome = probe::check(&job::spec_of(&monitor), &st.cfg.region).await;
    Ok(ok(outcome))
}

/// `POST /api/uptime/test?site=` — check a monitor that is not saved yet.
async fn test_draft(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<MonitorBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, MEMBER)?;
    let fields = fields_from(&st.cfg, &body)?;
    limit_tests(&st, access.site.id).await?;
    let spec = probe::Spec {
        url: fields.url,
        method: fields.method,
        timeout: Duration::from_millis(u64::try_from(fields.timeout_ms).unwrap_or(10_000)),
        status_min: u16::try_from(fields.status_min).unwrap_or(200),
        status_max: u16::try_from(fields.status_max).unwrap_or(299),
        keyword: fields.keyword,
        json_path: fields.json_path,
        json_expected: fields.json_expected,
        ssl_check: fields.ssl_check,
        ssl_expiry_days: fields.ssl_expiry_days,
    };
    Ok(ok(probe::check(&spec, &st.cfg.region).await))
}

/// Hold a site to [`TEST_LIMIT_PER_MINUTE`] test checks.
async fn limit_tests(st: &AppState, site: Uuid) -> Result<()> {
    let key = format!("uptime-test:{site}");
    match st.kv.rate_limit(&key, TEST_LIMIT_PER_MINUTE, 60).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::RateLimited { retry_after: 60 }),
        // A cold Valkey must not stop a person testing one URL.
        Err(e) => {
            tracing::warn!(error = %e, "the uptime test limiter could not be consulted; allowing");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Reading the results
// ---------------------------------------------------------------------------

/// One monitor's figures, from ClickHouse.
#[derive(Debug, Clone, clickhouse::Row, Deserialize)]
struct MonitorFigures {
    #[serde(with = "clickhouse::serde::uuid")]
    monitor_id: Uuid,
    checks_24h: u64,
    ok_24h: u64,
    checks_7d: u64,
    ok_7d: u64,
    checks_30d: u64,
    ok_30d: u64,
    avg_latency_24h: Option<f64>,
    last_latency: u32,
    last_ssl: Option<i32>,
}

/// `GET /api/uptime/summary?site=`
async fn summary(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    Ok(ok(summary_value(&st, &access.site).await?))
}

/// The Uptime screen's strip and list, for the route and the MCP tool alike.
pub(crate) async fn summary_value(
    st: &AppState,
    site: &micaforge_store::models::sites::Site,
) -> Result<Value> {
    let monitors = uptime::list_for_site(&st.pg, site.id).await?;
    let open_incidents = uptime::open_incident_count(&st.pg, site.id).await?;

    let figures = if monitors.is_empty() {
        Vec::new()
    } else {
        let since = fmt_ts(Utc::now() - chrono::Duration::days(30));
        let day = fmt_ts(Utc::now() - chrono::Duration::days(1));
        let week = fmt_ts(Utc::now() - chrono::Duration::days(7));
        let sql = "SELECT monitor_id,
                countIf(timestamp >= toDateTime64(?, 3, 'UTC')) AS checks_24h,
                countIf(timestamp >= toDateTime64(?, 3, 'UTC') AND ok = 1) AS ok_24h,
                countIf(timestamp >= toDateTime64(?, 3, 'UTC')) AS checks_7d,
                countIf(timestamp >= toDateTime64(?, 3, 'UTC') AND ok = 1) AS ok_7d,
                count() AS checks_30d,
                countIf(ok = 1) AS ok_30d,
                if(countIf(timestamp >= toDateTime64(?, 3, 'UTC') AND status_code > 0) = 0, NULL,
                   avgIf(latency_ms, timestamp >= toDateTime64(?, 3, 'UTC') AND status_code > 0))
                    AS avg_latency_24h,
                argMax(latency_ms, timestamp) AS last_latency,
                argMax(ssl_days_left, timestamp) AS last_ssl
            FROM uptime_checks
            WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC')
            GROUP BY monitor_id";
        let params = vec![
            Param::Str(day.clone()),
            Param::Str(day.clone()),
            Param::Str(week.clone()),
            Param::Str(week),
            Param::Str(day.clone()),
            Param::Str(day),
            Param::U16(site.site_id_u16()),
            Param::Str(since),
        ];
        fetch_all::<MonitorFigures>(&st.ch, "uptime_summary", sql, &params).await?
    };

    let rows: Vec<Value> = monitors
        .iter()
        .map(|m| {
            let f = figures.iter().find(|f| f.monitor_id == m.id);
            json!({
                "id": m.id,
                "name": m.name,
                "url": m.url,
                "enabled": m.enabled,
                "state": m.state,
                "last_checked_at": m.last_checked_at,
                "last_latency_ms": f.filter(|f| f.checks_30d > 0).map(|f| f.last_latency),
                "ssl_days_left": f.and_then(|f| f.last_ssl),
                "uptime_24h": f.and_then(|f| fraction(f.ok_24h, f.checks_24h)),
                "uptime_7d": f.and_then(|f| fraction(f.ok_7d, f.checks_7d)),
                "uptime_30d": f.and_then(|f| fraction(f.ok_30d, f.checks_30d)),
                "avg_latency_24h_ms": f.and_then(|f| f.avg_latency_24h).map(|v| v.round()),
            })
        })
        .collect();

    Ok(json!({
        "region": st.cfg.region,
        "monitors": rows,
        "open_incidents": open_incidents,
    }))
}

/// `part / total`, or `None` when nothing was measured.
fn fraction(part: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| part as f64 / total as f64)
}

/// `?range=` for one monitor's chart.
#[derive(Debug, Deserialize)]
struct SeriesQuery {
    #[serde(default)]
    range: Option<String>,
}

/// A chart window: its length, and the bucket width that keeps it under two
/// hundred points.
fn series_window(range: Option<&str>) -> Result<(&'static str, chrono::Duration, u32)> {
    match range.map(str::trim).unwrap_or("24h") {
        "24h" | "" => Ok(("24h", chrono::Duration::hours(24), 10)),
        "7d" => Ok(("7d", chrono::Duration::days(7), 60)),
        "30d" => Ok(("30d", chrono::Duration::days(30), 240)),
        other => Err(Error::validation(
            "range",
            format!("`{other}` is not a range; expected 24h, 7d or 30d"),
        )),
    }
}

/// One bucket of a monitor's chart.
#[derive(Debug, Clone, clickhouse::Row, Deserialize)]
struct SeriesBucket {
    t: i64,
    checks: u64,
    failures: u64,
    latency_avg: Option<f64>,
    latency_p95: Option<f64>,
}

/// `GET /api/uptime/monitors/{id}/series?range=24h|7d|30d`
async fn monitor_series(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<SeriesQuery>,
) -> ApiResult<Json<Value>> {
    let monitor = reachable(&st, id, auth.user.id, VIEWER).await?;
    let site =
        crate::routes::sites::authorize_site(&st.pg, monitor.site_id, auth.user.id, VIEWER).await?;
    let (label, span, bucket_minutes) = series_window(q.range.as_deref())?;
    let now = Utc::now();
    let start = now - span;

    let sql = "SELECT
            toUnixTimestamp64Milli(toDateTime64(
                toStartOfInterval(timestamp, toIntervalMinute(?)), 3)) AS t,
            count() AS checks,
            countIf(ok = 0) AS failures,
            if(countIf(status_code > 0) = 0, NULL, avgIf(latency_ms, status_code > 0)) AS latency_avg,
            if(countIf(status_code > 0) = 0, NULL,
               quantileIf(0.95)(latency_ms, status_code > 0)) AS latency_p95
        FROM uptime_checks
        WHERE site_id = ? AND monitor_id = toUUID(?) AND timestamp >= toDateTime64(?, 3, 'UTC')
        GROUP BY t ORDER BY t";
    let params = vec![
        Param::U64(u64::from(bucket_minutes)),
        Param::U16(site.site_id_u16()),
        Param::Str(monitor.id.to_string()),
        Param::Str(fmt_ts(start)),
    ];
    let buckets = fetch_all::<SeriesBucket>(&st.ch, "uptime_series", sql, &params).await?;
    let points = fill_buckets(&buckets, start, now, bucket_minutes);

    let checks: u64 = buckets.iter().map(|b| b.checks).sum();
    let failures: u64 = buckets.iter().map(|b| b.failures).sum();
    Ok(ok(json!({
        "range": label,
        "uptime": fraction(checks.saturating_sub(failures), checks),
        "points": points,
    })))
}

/// Every bucket of the window, the empty ones included, so a chart draws a
/// gap where no check ran rather than joining across it.
fn fill_buckets(
    buckets: &[SeriesBucket],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    bucket_minutes: u32,
) -> Vec<Value> {
    let width = i64::from(bucket_minutes) * 60_000;
    let first = start.timestamp_millis().div_euclid(width) * width;
    let last = end.timestamp_millis();
    let mut out = Vec::new();
    let mut t = first;
    while t <= last {
        let found = buckets.iter().find(|b| b.t == t);
        out.push(json!({
            "t": t,
            "checks": found.map(|b| b.checks).unwrap_or(0),
            "failures": found.map(|b| b.failures).unwrap_or(0),
            "latency_avg_ms": found.and_then(|b| b.latency_avg).map(f64::round),
            "latency_p95_ms": found.and_then(|b| b.latency_p95).map(f64::round),
        }));
        t += width;
    }
    out
}

/// `?limit=` for the checks list.
#[derive(Debug, Deserialize)]
struct LimitQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// One stored check.
#[derive(Debug, Clone, clickhouse::Row, Deserialize, Serialize)]
struct CheckRow {
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    timestamp: DateTime<Utc>,
    ok: u8,
    status_code: u16,
    latency_ms: u32,
    error: String,
    ssl_days_left: Option<i32>,
    region: String,
}

/// `GET /api/uptime/monitors/{id}/checks?limit=50`
async fn monitor_checks(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let monitor = reachable(&st, id, auth.user.id, VIEWER).await?;
    let site = authorize_site(&st.pg, monitor.site_id, auth.user.id, VIEWER).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let sql = "SELECT timestamp, ok, status_code, latency_ms, error, ssl_days_left, toString(region) AS region
        FROM uptime_checks
        WHERE site_id = ? AND monitor_id = toUUID(?)
        ORDER BY timestamp DESC LIMIT ?";
    let params = vec![
        Param::U16(site.site_id_u16()),
        Param::Str(monitor.id.to_string()),
        Param::U64(u64::from(limit)),
    ];
    let rows = fetch_all::<CheckRow>(&st.ch, "uptime_checks", sql, &params).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "checked_at": r.timestamp,
                "ok": r.ok == 1,
                "status_code": (r.status_code > 0).then_some(r.status_code),
                "latency_ms": r.latency_ms,
                "error": (!r.error.is_empty()).then_some(r.error.as_str()),
                "ssl_days_left": r.ssl_days_left,
                "region": r.region,
            })
        })
        .collect();
    Ok(ok(out))
}

/// `GET /api/uptime/incidents?site=&limit=`
async fn incidents(
    State(st): State<AppState>,
    access: SiteAccess,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    Ok(ok(incidents_value(
        &st,
        access.site.id,
        i64::from(q.limit.unwrap_or(50)),
    )
    .await?))
}

/// A site's incidents, newest first, for the route and the MCP tool alike.
pub(crate) async fn incidents_value(st: &AppState, site: Uuid, limit: i64) -> Result<Value> {
    let rows = uptime::list_incidents(&st.pg, site, limit).await?;
    let now = Utc::now();
    let out: Vec<Value> = rows
        .iter()
        .map(|i| {
            let until = i.resolved_at.unwrap_or(now);
            json!({
                "id": i.id,
                "monitor_id": i.monitor_id,
                "monitor_name": i.monitor_name,
                "started_at": i.started_at,
                "resolved_at": i.resolved_at,
                "cause": i.cause,
                "failed_checks": i.failed_checks,
                "duration_secs": (until - i.started_at).num_seconds().max(0),
            })
        })
        .collect();
    Ok(Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::notify::tests::test_cfg;

    fn body(url: &str) -> MonitorBody {
        serde_json::from_value(json!({ "name": "Home", "url": url })).unwrap()
    }

    #[test]
    fn a_minimal_body_gets_every_default() {
        let f = fields_from(&test_cfg(), &body("https://example.com/")).unwrap();
        assert_eq!(f.method, "GET");
        assert_eq!(f.interval_secs, 300);
        assert_eq!((f.status_min, f.status_max), (200, 299));
        assert_eq!(f.failure_threshold, 2);
        assert_eq!(f.ssl_expiry_days, 14);
        assert!(f.ssl_check && f.enabled);
        assert_eq!(f.channels, json!([]));
    }

    #[test]
    fn a_local_or_odd_url_is_refused_at_write_time() {
        for url in [
            "http://127.0.0.1/",
            "http://169.254.169.254/latest",
            "ftp://example.com/",
            "nonsense",
        ] {
            let e = fields_from(&test_cfg(), &body(url)).unwrap_err();
            assert_eq!(e.field(), Some("url"), "{url}");
        }
    }

    #[test]
    fn ranges_and_combinations_are_checked() {
        let with = |extra: Value| {
            let mut v = json!({ "name": "Home", "url": "https://example.com/" });
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            fields_from(
                &test_cfg(),
                &serde_json::from_value::<MonitorBody>(v).unwrap(),
            )
        };
        assert_eq!(
            with(json!({ "interval_minutes": 0 })).unwrap_err().field(),
            Some("interval_minutes")
        );
        assert_eq!(
            with(json!({ "interval_minutes": 61 })).unwrap_err().field(),
            Some("interval_minutes")
        );
        assert_eq!(
            with(json!({ "timeout_ms": 500 })).unwrap_err().field(),
            Some("timeout_ms")
        );
        assert_eq!(
            with(json!({ "status_min": 300, "status_max": 200 }))
                .unwrap_err()
                .field(),
            Some("status_min")
        );
        assert_eq!(
            with(json!({ "json_path": "a..b" })).unwrap_err().field(),
            Some("json_path")
        );
        assert_eq!(
            with(json!({ "json_expected": "ok" })).unwrap_err().field(),
            Some("json_expected")
        );
        assert_eq!(
            with(json!({ "method": "HEAD", "keyword": "ok" }))
                .unwrap_err()
                .field(),
            Some("method")
        );
        assert_eq!(
            with(json!({ "method": "DELETE" })).unwrap_err().field(),
            Some("method")
        );
        assert!(with(
            json!({ "json_path": "$.status", "json_expected": "ok", "interval_minutes": 1 })
        )
        .is_ok());
    }

    #[test]
    fn an_empty_channels_document_means_no_channels_and_junk_is_refused() {
        let with = |channels: Value| {
            fields_from(
                &test_cfg(),
                &serde_json::from_value::<MonitorBody>(
                    json!({ "name": "Home", "url": "https://example.com/", "channels": channels }),
                )
                .unwrap(),
            )
        };
        assert_eq!(with(json!({})).unwrap().channels, json!([]));
        assert_eq!(
            with(json!({ "emails": [], "webhook": "" }))
                .unwrap()
                .channels,
            json!([])
        );
        assert_eq!(
            with(json!(["not-an-address"])).unwrap_err().field(),
            Some("channels")
        );
        assert!(with(json!({ "emails": ["ops@example.com"] })).is_ok());
        assert!(with(json!({ "webhook": "http://10.0.0.1/hook" })).is_err());
    }

    #[test]
    fn every_window_fits_a_chart_and_an_unknown_one_is_refused() {
        for range in ["24h", "7d", "30d"] {
            let (_, span, minutes) = series_window(Some(range)).unwrap();
            let points = span.num_minutes() / i64::from(minutes);
            assert!((100..=200).contains(&points), "{range}: {points}");
        }
        assert_eq!(
            series_window(Some("1y")).unwrap_err().field(),
            Some("range")
        );
    }

    #[test]
    fn the_filled_series_has_every_bucket_and_keeps_measured_ones() {
        let start = DateTime::from_timestamp_millis(1_000 * 60 * 60 * 10).unwrap();
        let end = start + chrono::Duration::minutes(30);
        let measured = SeriesBucket {
            t: start.timestamp_millis() + 600_000,
            checks: 2,
            failures: 1,
            latency_avg: Some(120.4),
            latency_p95: Some(180.0),
        };
        let points = fill_buckets(&[measured], start, end, 10);
        assert_eq!(points.len(), 4);
        assert_eq!(points[0]["checks"], 0);
        assert!(points[0]["latency_avg_ms"].is_null());
        assert_eq!(points[1]["checks"], 2);
        assert_eq!(points[1]["latency_avg_ms"], 120.0);
    }
}
