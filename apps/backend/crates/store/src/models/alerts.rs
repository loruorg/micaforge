//! `alerts` and `reports` — the two ways Micaforge reaches out. `docs/CONTRACT.md` §2.
//!
//! An alert is conditional and fires when something crosses a threshold. A
//! report is scheduled and goes out whether or not anything happened. They share
//! a `channels` document (email addresses, a Slack webhook) and nothing else.
//!
//! Both carry the state the worker needs to avoid duplicates: `last_fired_at`
//! for the alert cooldown, `next_run_at` for the report schedule. Keeping that
//! in the row rather than in the scheduler means a worker restart cannot resend
//! yesterday's report.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A threshold that sends a notification when it is crossed.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Alert {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// `spike` | `drop` | `goal` | `error` | `agent_surge` | `citation_gap`.
    ///
    /// The last two are the wedge: a crawl surge, and a URL whose agent traffic
    /// climbed while answer-engine arrivals did not.
    pub kind: String,
    /// Thresholds and comparison window for this kind.
    pub config: Json,
    /// Where a notification goes.
    pub channels: Json,
    /// Last time this alert sent something, for the cooldown.
    pub last_fired_at: Option<DateTime<Utc>>,
}

/// A scheduled digest.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Report {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// `daily` | `weekly` | `monthly`.
    pub cadence: String,
    /// Who receives it.
    pub recipients: Json,
    /// How it is delivered.
    pub channels: Json,
    /// When the worker should next build it.
    pub next_run_at: Option<DateTime<Utc>>,
}

const ALERT_COLS: &str = "id, site_id, kind, config, channels, last_fired_at";
const REPORT_COLS: &str = "id, site_id, cadence, recipients, channels, next_run_at";

// ---------------------------------------------------------------------------
// Alerts
// ---------------------------------------------------------------------------

/// Create an alert.
pub async fn create_alert(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    kind: &str,
    config: &Json,
    channels: &Json,
) -> Result<Alert> {
    let row = sqlx::query_as::<_, Alert>(&format!(
        "INSERT INTO alerts (id, site_id, kind, config, channels)
         VALUES ($1, $2, $3, $4, $5) RETURNING {ALERT_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(kind)
    .bind(config)
    .bind(channels)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch an alert by primary key.
pub async fn get_alert(pool: &PgPool, id: Uuid) -> Result<Alert> {
    let row = sqlx::query_as::<_, Alert>(&format!("SELECT {ALERT_COLS} FROM alerts WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "alert")
}

/// Every alert on a site.
pub async fn list_alerts(pool: &PgPool, site_id: Uuid) -> Result<Vec<Alert>> {
    let rows = sqlx::query_as::<_, Alert>(&format!(
        "SELECT {ALERT_COLS} FROM alerts WHERE site_id = $1 ORDER BY id DESC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every alert on the install, for the worker's evaluation sweep.
pub async fn list_all_alerts(pool: &PgPool) -> Result<Vec<Alert>> {
    let rows = sqlx::query_as::<_, Alert>(&format!(
        "SELECT {ALERT_COLS} FROM alerts ORDER BY site_id, id"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update an alert.
pub async fn update_alert(
    pool: &PgPool,
    id: Uuid,
    kind: &str,
    config: &Json,
    channels: &Json,
) -> Result<Alert> {
    let row = sqlx::query_as::<_, Alert>(&format!(
        "UPDATE alerts SET kind = $2, config = $3, channels = $4
         WHERE id = $1 RETURNING {ALERT_COLS}"
    ))
    .bind(id)
    .bind(kind)
    .bind(config)
    .bind(channels)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "alert")
}

/// Claim an alert for firing, respecting its cooldown.
///
/// The cooldown is enforced in the `UPDATE`'s own `WHERE`, not read and then
/// checked, so two workers evaluating the same alert in the same second cannot
/// both decide to send. Whichever one's update matches a row is the one that
/// sends; the other gets `false`.
///
/// A claim is not a delivery. Prefer [`claim_alert`], which returns the claim
/// so that a delivery that fails can hand it back with [`release_alert`].
pub async fn try_fire_alert(pool: &PgPool, id: Uuid, cooldown_secs: i64) -> Result<bool> {
    Ok(claim_alert(pool, id, cooldown_secs).await?.is_some())
}

/// Claim an alert for firing, and return the claim's timestamp.
///
/// The same one-statement cooldown check as [`try_fire_alert`]. `None` means
/// the alert is inside its cooldown or another worker claimed it first.
pub async fn claim_alert(
    pool: &PgPool,
    id: Uuid,
    cooldown_secs: i64,
) -> Result<Option<DateTime<Utc>>> {
    let claimed: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(
        "UPDATE alerts SET last_fired_at = now()
         WHERE id = $1
           AND (last_fired_at IS NULL
                OR last_fired_at < now() - make_interval(secs => $2::double precision))
         RETURNING last_fired_at",
    )
    .bind(id)
    .bind(cooldown_secs.max(0) as f64)
    .fetch_optional(pool)
    .await?;
    Ok(claimed.and_then(|(at,)| at))
}

/// Hand back a claim whose notification reached nobody.
///
/// Restores the cooldown to `previous`, so the next sweep evaluates the alert
/// again instead of staying silent for the whole cooldown about something
/// nobody was told. Only the claim made at `claimed_at` is undone: if another
/// worker has fired the alert since, this changes nothing.
pub async fn release_alert(
    pool: &PgPool,
    id: Uuid,
    claimed_at: DateTime<Utc>,
    previous: Option<DateTime<Utc>>,
) -> Result<bool> {
    let result =
        sqlx::query("UPDATE alerts SET last_fired_at = $3 WHERE id = $1 AND last_fired_at = $2")
            .bind(id)
            .bind(claimed_at)
            .bind(previous)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() == 1)
}

/// Delete an alert.
pub async fn delete_alert(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM alerts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

/// Create a scheduled report.
pub async fn create_report(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    cadence: &str,
    recipients: &Json,
    channels: &Json,
    next_run_at: Option<DateTime<Utc>>,
) -> Result<Report> {
    let row = sqlx::query_as::<_, Report>(&format!(
        "INSERT INTO reports (id, site_id, cadence, recipients, channels, next_run_at)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {REPORT_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(cadence)
    .bind(recipients)
    .bind(channels)
    .bind(next_run_at)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch a report by primary key.
pub async fn get_report(pool: &PgPool, id: Uuid) -> Result<Report> {
    let row =
        sqlx::query_as::<_, Report>(&format!("SELECT {REPORT_COLS} FROM reports WHERE id = $1"))
            .bind(id)
            .fetch_optional(pool)
            .await?;
    or_missing(row, "report")
}

/// Every report on a site.
pub async fn list_reports(pool: &PgPool, site_id: Uuid) -> Result<Vec<Report>> {
    let rows = sqlx::query_as::<_, Report>(&format!(
        "SELECT {REPORT_COLS} FROM reports WHERE site_id = $1 ORDER BY cadence ASC, id DESC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Reports whose schedule has come due at `now`.
pub async fn due_reports(pool: &PgPool, now: DateTime<Utc>) -> Result<Vec<Report>> {
    let rows = sqlx::query_as::<_, Report>(&format!(
        "SELECT {REPORT_COLS} FROM reports
         WHERE next_run_at IS NOT NULL AND next_run_at <= $1
         ORDER BY next_run_at ASC"
    ))
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update a report.
pub async fn update_report(
    pool: &PgPool,
    id: Uuid,
    cadence: &str,
    recipients: &Json,
    channels: &Json,
) -> Result<Report> {
    let row = sqlx::query_as::<_, Report>(&format!(
        "UPDATE reports SET cadence = $2, recipients = $3, channels = $4
         WHERE id = $1 RETURNING {REPORT_COLS}"
    ))
    .bind(id)
    .bind(cadence)
    .bind(recipients)
    .bind(channels)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "report")
}

/// Move a report's schedule forward after it has been sent.
///
/// The next time is computed by the caller, which owns the cadence calendar and
/// the site's timezone. This only records the decision.
pub async fn set_next_run(pool: &PgPool, id: Uuid, next_run_at: DateTime<Utc>) -> Result<()> {
    sqlx::query("UPDATE reports SET next_run_at = $2 WHERE id = $1")
        .bind(id)
        .bind(next_run_at)
        .execute(pool)
        .await?;
    Ok(())
}

/// Claim a report for sending.
///
/// Moves `next_run_at` from `expected` (the value the caller read) to a lease
/// `lease_secs` ahead, in one conditional statement, and returns the lease.
/// `None` means another worker changed the row first and is sending it, so
/// two sweeps that read the same due report cannot both mail it. A worker that
/// dies mid-send leaves the lease in place, and the report comes due again when
/// it expires.
///
/// The caller ends the claim with [`finish_report_claim`]: the next scheduled
/// time after a delivery, or `expected` again after a failure worth retrying.
pub async fn claim_report(
    pool: &PgPool,
    id: Uuid,
    expected: Option<DateTime<Utc>>,
    lease_secs: i64,
) -> Result<Option<DateTime<Utc>>> {
    let claimed: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(
        "UPDATE reports
         SET next_run_at = now() + make_interval(secs => $3::double precision)
         WHERE id = $1 AND next_run_at IS NOT DISTINCT FROM $2
         RETURNING next_run_at",
    )
    .bind(id)
    .bind(expected)
    .bind(lease_secs.max(1) as f64)
    .fetch_optional(pool)
    .await?;
    Ok(claimed.and_then(|(at,)| at))
}

/// End a claim taken by [`claim_report`], setting `next_run_at` to `next`.
///
/// Conditional on the lease still being in place, so an operator who changed
/// the cadence (and with it the next run) while the report was being built
/// keeps the schedule they asked for. Returns whether the row was updated.
pub async fn finish_report_claim(
    pool: &PgPool,
    id: Uuid,
    lease: DateTime<Utc>,
    next: Option<DateTime<Utc>>,
) -> Result<bool> {
    let result =
        sqlx::query("UPDATE reports SET next_run_at = $3 WHERE id = $1 AND next_run_at = $2")
            .bind(id)
            .bind(lease)
            .bind(next)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() == 1)
}

/// Delete a report.
pub async fn delete_report(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM reports WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
