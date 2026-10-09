//! `uptime_monitors` and `uptime_incidents`.
//!
//! A monitor is a URL the server checks on its own schedule; an incident is a
//! stretch of time a monitor spent down. The check results are an append-only
//! stream in ClickHouse (`uptime_checks`); what lives here is what has to be
//! read and changed in place, and every change that two workers could race on
//! is made in one statement:
//!
//! - **Claiming a check** ([`claim_due`]) moves the monitor's next due time
//!   forward and sets a lease in the same `UPDATE ... FOR UPDATE SKIP LOCKED`.
//!   A monitor whose lease is still running is not claimable, so there is
//!   never more than one check of one monitor in flight, across sweeps and
//!   across replicas.
//! - **Recording a check** ([`record_check`]) writes the monitor's new state
//!   and opens or closes its incident in one transaction. At most one open
//!   incident per monitor is a unique index, not a convention.
//!
//! Which state comes next is not decided here: the job computes it with a pure
//! function it can test, and this module stores the decision.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A monitored URL and its schedule and state.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Monitor {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// Display name.
    pub name: String,
    /// The URL checked. `http` or `https`, guarded again before every check.
    pub url: String,
    /// `GET` | `HEAD` | `POST`.
    pub method: String,
    /// Seconds between checks, 60 to 3600.
    pub interval_secs: i32,
    /// Milliseconds a check may take, 1000 to 30000.
    pub timeout_ms: i32,
    /// Lowest accepted status, inclusive.
    pub status_min: i32,
    /// Highest accepted status, inclusive.
    pub status_max: i32,
    /// Text the body must contain.
    pub keyword: Option<String>,
    /// A path into a JSON body.
    pub json_path: Option<String>,
    /// The text the value at `json_path` must equal. `None` means the path
    /// must exist.
    pub json_expected: Option<String>,
    /// Whether the certificate's expiry is checked.
    pub ssl_check: bool,
    /// Fail when fewer whole days than this remain on the certificate.
    pub ssl_expiry_days: i32,
    /// Consecutive failures that open an incident.
    pub failure_threshold: i32,
    /// Where notifications go: the alert channels document.
    pub channels: Json,
    /// `false` keeps the monitor without checking it.
    pub enabled: bool,
    /// `unknown` | `up` | `down`.
    pub state: String,
    /// Failed checks in a row, up to now.
    pub consecutive_failures: i32,
    /// When the last check finished.
    pub last_checked_at: Option<DateTime<Utc>>,
    /// When the monitor is next due.
    pub next_check_at: DateTime<Utc>,
    /// Set while a check is in flight.
    pub lease_until: Option<DateTime<Utc>>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
}

/// The fields an operator sets. Everything else is the scheduler's.
#[derive(Debug, Clone)]
pub struct MonitorFields {
    /// Display name.
    pub name: String,
    /// The URL checked.
    pub url: String,
    /// `GET` | `HEAD` | `POST`.
    pub method: String,
    /// Seconds between checks.
    pub interval_secs: i32,
    /// Milliseconds a check may take.
    pub timeout_ms: i32,
    /// Lowest accepted status.
    pub status_min: i32,
    /// Highest accepted status.
    pub status_max: i32,
    /// Text the body must contain.
    pub keyword: Option<String>,
    /// A path into a JSON body.
    pub json_path: Option<String>,
    /// The value expected at `json_path`.
    pub json_expected: Option<String>,
    /// Whether the certificate's expiry is checked.
    pub ssl_check: bool,
    /// The certificate threshold in days.
    pub ssl_expiry_days: i32,
    /// Consecutive failures that open an incident.
    pub failure_threshold: i32,
    /// The channels document.
    pub channels: Json,
    /// Whether the monitor is checked.
    pub enabled: bool,
}

/// A stretch of time one monitor spent down.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Incident {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The monitor that went down.
    pub monitor_id: Uuid,
    /// Its site, by UUID.
    pub site_id: Uuid,
    /// The monitor's name, joined in for the list.
    pub monitor_name: String,
    /// When the check that crossed the threshold ran.
    pub started_at: DateTime<Utc>,
    /// When the first successful check after it ran. `None` while open.
    pub resolved_at: Option<DateTime<Utc>>,
    /// The error that opened it.
    pub cause: String,
    /// Failed checks counted against it.
    pub failed_checks: i32,
}

/// What [`record_check`] did to the monitor's incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentChange {
    /// Nothing opened or closed.
    None,
    /// This incident was opened.
    Opened(Uuid),
    /// This incident was closed.
    Closed(Uuid),
}

const MONITOR_COLS: &str = "id, site_id, name, url, method, interval_secs, timeout_ms, \
     status_min, status_max, keyword, json_path, json_expected, ssl_check, ssl_expiry_days, \
     failure_threshold, channels, enabled, state, consecutive_failures, last_checked_at, \
     next_check_at, lease_until, created_at";

/// How far before its nominal time a monitor becomes due again.
///
/// The sweep runs once a minute, counted from the end of the previous sweep,
/// so a one-minute monitor claimed late in one sweep would otherwise just miss
/// the next one and be checked every two minutes. Ten seconds of slack keeps
/// it on every sweep.
const DUE_SLACK_SECS: f64 = 10.0;

/// How long a claimed check may hold its lease past its own timeout.
///
/// Covers the time to record the result. A worker that dies mid-check leaves
/// the lease to expire, and the monitor is claimable again a minute later.
const LEASE_SLACK_SECS: f64 = 60.0;

/// Create a monitor, due now.
pub async fn create(pool: &PgPool, id: Uuid, site_id: Uuid, f: &MonitorFields) -> Result<Monitor> {
    let row = sqlx::query_as::<_, Monitor>(&format!(
        "INSERT INTO uptime_monitors (id, site_id, name, url, method, interval_secs, timeout_ms,
             status_min, status_max, keyword, json_path, json_expected, ssl_check,
             ssl_expiry_days, failure_threshold, channels, enabled, next_check_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, now())
         RETURNING {MONITOR_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(&f.name)
    .bind(&f.url)
    .bind(&f.method)
    .bind(f.interval_secs)
    .bind(f.timeout_ms)
    .bind(f.status_min)
    .bind(f.status_max)
    .bind(&f.keyword)
    .bind(&f.json_path)
    .bind(&f.json_expected)
    .bind(f.ssl_check)
    .bind(f.ssl_expiry_days)
    .bind(f.failure_threshold)
    .bind(&f.channels)
    .bind(f.enabled)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Monitor> {
    let row = sqlx::query_as::<_, Monitor>(&format!(
        "SELECT {MONITOR_COLS} FROM uptime_monitors WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "monitor")
}

/// Every monitor on a site, oldest first.
pub async fn list_for_site(pool: &PgPool, site_id: Uuid) -> Result<Vec<Monitor>> {
    let rows = sqlx::query_as::<_, Monitor>(&format!(
        "SELECT {MONITOR_COLS} FROM uptime_monitors WHERE site_id = $1 ORDER BY created_at, id"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Replace the operator's fields, and make the monitor due now.
///
/// Due now, because an edited URL or assertion is exactly the moment the
/// operator wants to see a result, and waiting out an hour's interval to learn
/// that a typo took the monitor down would be a poor answer.
pub async fn update(pool: &PgPool, id: Uuid, f: &MonitorFields) -> Result<Monitor> {
    let row = sqlx::query_as::<_, Monitor>(&format!(
        "UPDATE uptime_monitors SET name = $2, url = $3, method = $4, interval_secs = $5,
             timeout_ms = $6, status_min = $7, status_max = $8, keyword = $9, json_path = $10,
             json_expected = $11, ssl_check = $12, ssl_expiry_days = $13,
             failure_threshold = $14, channels = $15, enabled = $16, next_check_at = now()
         WHERE id = $1 RETURNING {MONITOR_COLS}"
    ))
    .bind(id)
    .bind(&f.name)
    .bind(&f.url)
    .bind(&f.method)
    .bind(f.interval_secs)
    .bind(f.timeout_ms)
    .bind(f.status_min)
    .bind(f.status_max)
    .bind(&f.keyword)
    .bind(&f.json_path)
    .bind(&f.json_expected)
    .bind(f.ssl_check)
    .bind(f.ssl_expiry_days)
    .bind(f.failure_threshold)
    .bind(&f.channels)
    .bind(f.enabled)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "monitor")
}

/// Delete a monitor. Its incidents, and its place on status pages, go with it.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM uptime_monitors WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Claim up to `limit` due monitors for checking.
///
/// One statement: the rows are locked with `SKIP LOCKED`, their next due time
/// moves one interval ahead, and a lease is set to the check's own timeout
/// plus [`LEASE_SLACK_SECS`]. A monitor that is disabled, not yet due, or
/// still leased is never returned, so this is the whole of "at most one check
/// in flight per monitor".
pub async fn claim_due(pool: &PgPool, limit: i64) -> Result<Vec<Monitor>> {
    let rows = sqlx::query_as::<_, Monitor>(&format!(
        "UPDATE uptime_monitors m SET
             next_check_at = now() + make_interval(secs => m.interval_secs - $2::double precision),
             lease_until = now() + make_interval(
                 secs => m.timeout_ms / 1000.0 + $3::double precision)
         WHERE m.id IN (
             SELECT id FROM uptime_monitors
             WHERE enabled AND next_check_at <= now()
               AND (lease_until IS NULL OR lease_until < now())
             ORDER BY next_check_at
             LIMIT $1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING {cols}",
        cols = prefixed(MONITOR_COLS, "m"),
    ))
    .bind(limit.max(1))
    .bind(DUE_SLACK_SECS)
    .bind(LEASE_SLACK_SECS)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// True when at least one enabled monitor exists on the install.
///
/// The sweep asks this before anything else, so an install with no monitor
/// does no further work and opens no socket.
pub async fn any_enabled(pool: &PgPool) -> Result<bool> {
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM uptime_monitors WHERE enabled)")
            .fetch_one(pool)
            .await?;
    Ok(exists)
}

/// The state a check leaves a monitor in, as the job decided it.
#[derive(Debug, Clone)]
pub struct CheckRecord<'a> {
    /// `up` or `down`.
    pub state: &'a str,
    /// Failed checks in a row after this one.
    pub consecutive_failures: i32,
    /// When the check ran.
    pub checked_at: DateTime<Utc>,
    /// Open an incident with this cause.
    pub open: Option<&'a str>,
    /// Close the open incident.
    pub close: bool,
}

/// Store one check's outcome: the monitor's state, and its incident.
///
/// One transaction, so a monitor is never `down` without its incident or
/// `up` with one still open. The lease is released here; a concurrent edit of
/// the operator's fields is untouched, because only the scheduler's columns
/// are written.
pub async fn record_check(
    pool: &PgPool,
    monitor: &Monitor,
    record: &CheckRecord<'_>,
) -> Result<IncidentChange> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "UPDATE uptime_monitors SET state = $2, consecutive_failures = $3,
             last_checked_at = $4, lease_until = NULL
         WHERE id = $1",
    )
    .bind(monitor.id)
    .bind(record.state)
    .bind(record.consecutive_failures)
    .bind(record.checked_at)
    .execute(&mut *tx)
    .await?;

    let mut change = IncidentChange::None;
    if let Some(cause) = record.open {
        // ON CONFLICT against the one-open-incident index: an incident that is
        // somehow already open stays the one that is open.
        let opened: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO uptime_incidents (id, monitor_id, site_id, started_at, cause, failed_checks)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (monitor_id) WHERE resolved_at IS NULL DO NOTHING
             RETURNING id",
        )
        .bind(micaforge_core::new_id())
        .bind(monitor.id)
        .bind(monitor.site_id)
        .bind(record.checked_at)
        .bind(cause)
        .bind(record.consecutive_failures)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id,)) = opened {
            change = IncidentChange::Opened(id);
        }
    } else if record.close {
        let closed: Option<(Uuid,)> = sqlx::query_as(
            "UPDATE uptime_incidents SET resolved_at = $2
             WHERE monitor_id = $1 AND resolved_at IS NULL
             RETURNING id",
        )
        .bind(monitor.id)
        .bind(record.checked_at)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id,)) = closed {
            change = IncidentChange::Closed(id);
        }
    } else if record.state == "down" {
        sqlx::query(
            "UPDATE uptime_incidents SET failed_checks = $2
             WHERE monitor_id = $1 AND resolved_at IS NULL",
        )
        .bind(monitor.id)
        .bind(record.consecutive_failures)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(change)
}

/// Give a lease back without recording a check, after a failure to check at
/// all (a worker shutting down mid-sweep, say).
pub async fn release(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE uptime_monitors SET lease_until = NULL WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

const INCIDENT_SELECT: &str = "SELECT i.id, i.monitor_id, i.site_id, m.name AS monitor_name, \
     i.started_at, i.resolved_at, i.cause, i.failed_checks \
     FROM uptime_incidents i JOIN uptime_monitors m ON m.id = i.monitor_id";

/// A site's incidents, newest first.
pub async fn list_incidents(pool: &PgPool, site_id: Uuid, limit: i64) -> Result<Vec<Incident>> {
    let rows = sqlx::query_as::<_, Incident>(&format!(
        "{INCIDENT_SELECT} WHERE i.site_id = $1 ORDER BY i.started_at DESC, i.id DESC LIMIT $2"
    ))
    .bind(site_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// One incident.
pub async fn get_incident(pool: &PgPool, id: Uuid) -> Result<Incident> {
    let row = sqlx::query_as::<_, Incident>(&format!("{INCIDENT_SELECT} WHERE i.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "incident")
}

/// How many incidents are open on a site.
pub async fn open_incident_count(pool: &PgPool, site_id: Uuid) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM uptime_incidents WHERE site_id = $1 AND resolved_at IS NULL",
    )
    .bind(site_id)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Qualify a column list with a table alias.
fn prefixed(cols: &str, alias: &str) -> String {
    cols.split(',')
        .map(|c| format!("{alias}.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_claim_returns_every_column_qualified() {
        let cols = prefixed(MONITOR_COLS, "m");
        assert!(cols.starts_with("m.id, m.site_id"));
        assert!(cols.ends_with("m.created_at"));
        assert_eq!(
            cols.matches("m.").count(),
            MONITOR_COLS.split(',').count(),
            "every column, and nothing else, is qualified"
        );
    }
}
