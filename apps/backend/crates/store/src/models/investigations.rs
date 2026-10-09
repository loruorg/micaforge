//! `investigations`, `investigation_events` and `insight_subscriptions`.
//!
//! The rules of *when* an investigation changes state live in the job that
//! runs the detector (`micaforge_jobs::investigate`). This module only stores
//! what was decided, with two guarantees the decision relies on:
//!
//! - **One live investigation per signal.** [`open`] inserts against the
//!   partial unique index from the migration and reports a conflict as `None`,
//!   so two sweeps that notice the same drop in the same second open one case,
//!   not two.
//! - **History is append-only.** Every change of state writes an event; no
//!   event is ever rewritten, so the timeline on the detail screen is the
//!   record of what was believed and when.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// One investigation.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Investigation {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// What changed, as a stable key the detector can find again.
    pub signal_key: String,
    /// One sentence naming the change.
    pub title: String,
    /// `open` | `monitoring` | `resolved`.
    pub status: String,
    /// `info` | `warning` | `critical`.
    pub severity: String,
    /// The metric that moved.
    pub metric: String,
    /// The dimension, for a mover inside one (a source, a page).
    pub dimension: Option<String>,
    /// The dimension's value.
    pub dimension_value: Option<String>,
    /// `up` | `down`.
    pub direction: String,
    /// The latest measured value.
    pub current_value: f64,
    /// What it was compared with.
    pub baseline_value: f64,
    /// The change in percent, `None` against a zero baseline.
    pub change_pct: Option<f64>,
    /// What was compared with what, in words.
    pub window_label: String,
    /// The statistical findings.
    pub findings: Json,
    /// The queries behind the findings and the explanation.
    pub evidence: Json,
    /// The model's reading of the likely causes.
    pub explanation: Option<String>,
    /// `ready` | `not_configured` | `failed`.
    pub explanation_status: String,
    /// Consecutive re-checks that found the change gone.
    pub calm_checks: i32,
    /// When it opened.
    pub created_at: DateTime<Utc>,
    /// When anything about it last changed.
    pub updated_at: DateTime<Utc>,
    /// When the next re-check is due. `None` once resolved.
    pub next_check_at: Option<DateTime<Utc>>,
    /// When it was resolved.
    pub resolved_at: Option<DateTime<Utc>>,
}

/// One entry in an investigation's timeline.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Event {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The investigation.
    pub investigation_id: Uuid,
    /// `opened` | `checked` | `explained` | `status` | `posted`.
    pub kind: String,
    /// One sentence.
    pub summary: String,
    /// The numbers behind the sentence.
    pub data: Json,
    /// When it happened.
    pub created_at: DateTime<Utc>,
}

/// What the detector measured, written on open and on every re-check.
#[derive(Debug, Clone)]
pub struct Measure<'a> {
    /// One sentence naming the change.
    pub title: &'a str,
    /// `info` | `warning` | `critical`.
    pub severity: &'a str,
    /// `up` | `down`.
    pub direction: &'a str,
    /// The latest value.
    pub current: f64,
    /// The comparison value.
    pub baseline: f64,
    /// The change in percent.
    pub change_pct: Option<f64>,
    /// What was compared with what.
    pub window_label: &'a str,
    /// The statistical findings.
    pub findings: &'a Json,
    /// The queries behind them.
    pub evidence: &'a Json,
}

/// A subscriber to a site's weekly digest.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Subscriber {
    /// The account.
    pub user_id: Uuid,
    /// Where the digest goes.
    pub email: String,
    /// How the digest greets them.
    pub name: String,
}

const COLS: &str = "id, site_id, signal_key, title, status, severity, metric, dimension, \
    dimension_value, direction, current_value, baseline_value, change_pct, window_label, \
    findings, evidence, explanation, explanation_status, calm_checks, created_at, updated_at, \
    next_check_at, resolved_at";
const EVENT_COLS: &str = "id, investigation_id, kind, summary, data, created_at";

/// Open an investigation, unless one is already live for this signal.
///
/// `None` means the partial unique index refused the insert: another sweep got
/// there first, and the caller should update that one instead.
#[allow(clippy::too_many_arguments)]
pub async fn open(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    signal_key: &str,
    metric: &str,
    dimension: Option<&str>,
    dimension_value: Option<&str>,
    measure: &Measure<'_>,
    explanation_status: &str,
    next_check_at: DateTime<Utc>,
) -> Result<Option<Investigation>> {
    let row = sqlx::query_as::<_, Investigation>(&format!(
        "INSERT INTO investigations (id, site_id, signal_key, title, severity, metric, dimension,
             dimension_value, direction, current_value, baseline_value, change_pct, window_label,
             findings, evidence, explanation_status, next_check_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
         ON CONFLICT (site_id, signal_key) WHERE status IN ('open', 'monitoring') DO NOTHING
         RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(signal_key)
    .bind(measure.title)
    .bind(measure.severity)
    .bind(metric)
    .bind(dimension)
    .bind(dimension_value)
    .bind(measure.direction)
    .bind(measure.current)
    .bind(measure.baseline)
    .bind(measure.change_pct)
    .bind(measure.window_label)
    .bind(measure.findings)
    .bind(measure.evidence)
    .bind(explanation_status)
    .bind(next_check_at)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// The live investigation for a signal, if there is one.
pub async fn find_live(
    pool: &PgPool,
    site_id: Uuid,
    signal_key: &str,
) -> Result<Option<Investigation>> {
    let row = sqlx::query_as::<_, Investigation>(&format!(
        "SELECT {COLS} FROM investigations
         WHERE site_id = $1 AND signal_key = $2 AND status IN ('open', 'monitoring')"
    ))
    .bind(site_id)
    .bind(signal_key)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Investigation> {
    let row = sqlx::query_as::<_, Investigation>(&format!(
        "SELECT {COLS} FROM investigations WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "investigation")
}

/// A site's investigations, most recently changed first.
///
/// `status` of `None` is every status.
pub async fn list(
    pool: &PgPool,
    site_id: Uuid,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<Investigation>> {
    let rows = sqlx::query_as::<_, Investigation>(&format!(
        "SELECT {COLS} FROM investigations
         WHERE site_id = $1 AND ($2::text IS NULL OR status = $2)
         ORDER BY updated_at DESC LIMIT $3"
    ))
    .bind(site_id)
    .bind(status)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Investigations opened or changed since `since`, for a digest.
pub async fn changed_since(
    pool: &PgPool,
    site_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<Investigation>> {
    let rows = sqlx::query_as::<_, Investigation>(&format!(
        "SELECT {COLS} FROM investigations
         WHERE site_id = $1 AND updated_at >= $2
         ORDER BY (status = 'resolved'), updated_at DESC LIMIT 50"
    ))
    .bind(site_id)
    .bind(since)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Write a fresh measurement onto a live investigation.
pub async fn record_measure(
    pool: &PgPool,
    id: Uuid,
    measure: &Measure<'_>,
) -> Result<Investigation> {
    let row = sqlx::query_as::<_, Investigation>(&format!(
        "UPDATE investigations SET title = $2, severity = $3, direction = $4, current_value = $5,
             baseline_value = $6, change_pct = $7, window_label = $8, findings = $9,
             evidence = $10, updated_at = now()
         WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(measure.title)
    .bind(measure.severity)
    .bind(measure.direction)
    .bind(measure.current)
    .bind(measure.baseline)
    .bind(measure.change_pct)
    .bind(measure.window_label)
    .bind(measure.findings)
    .bind(measure.evidence)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "investigation")
}

/// Store the model's explanation, or the reason there is none.
pub async fn set_explanation(
    pool: &PgPool,
    id: Uuid,
    explanation: Option<&str>,
    status: &str,
    evidence: &Json,
) -> Result<Investigation> {
    let row = sqlx::query_as::<_, Investigation>(&format!(
        "UPDATE investigations SET explanation = $2, explanation_status = $3, evidence = $4,
             updated_at = now()
         WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(explanation)
    .bind(status)
    .bind(evidence)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "investigation")
}

/// Move an investigation to a status, with its re-check schedule.
///
/// `resolved_at` follows the status: set when it resolves, cleared when it is
/// reopened, so the field never contradicts the state beside it.
pub async fn set_status(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    calm_checks: i32,
    next_check_at: Option<DateTime<Utc>>,
) -> Result<Investigation> {
    let row = sqlx::query_as::<_, Investigation>(&format!(
        "UPDATE investigations SET status = $2, calm_checks = $3, next_check_at = $4,
             resolved_at = CASE WHEN $2 = 'resolved' THEN COALESCE(resolved_at, now()) ELSE NULL END,
             updated_at = now()
         WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(status)
    .bind(calm_checks)
    .bind(next_check_at)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "investigation")
}

/// Live investigations whose re-check is due, oldest due first.
pub async fn due(pool: &PgPool, now: DateTime<Utc>, limit: i64) -> Result<Vec<Investigation>> {
    let rows = sqlx::query_as::<_, Investigation>(&format!(
        "SELECT {COLS} FROM investigations
         WHERE status IN ('open', 'monitoring') AND next_check_at IS NOT NULL
           AND next_check_at <= $1
         ORDER BY next_check_at ASC LIMIT $2"
    ))
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Append a timeline entry.
pub async fn add_event(
    pool: &PgPool,
    id: Uuid,
    investigation_id: Uuid,
    kind: &str,
    summary: &str,
    data: &Json,
) -> Result<Event> {
    let row = sqlx::query_as::<_, Event>(&format!(
        "INSERT INTO investigation_events (id, investigation_id, kind, summary, data)
         VALUES ($1, $2, $3, $4, $5) RETURNING {EVENT_COLS}"
    ))
    .bind(id)
    .bind(investigation_id)
    .bind(kind)
    .bind(summary)
    .bind(data)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// An investigation's timeline, oldest first.
pub async fn list_events(pool: &PgPool, investigation_id: Uuid) -> Result<Vec<Event>> {
    let rows = sqlx::query_as::<_, Event>(&format!(
        "SELECT {EVENT_COLS} FROM investigation_events
         WHERE investigation_id = $1 ORDER BY created_at, id"
    ))
    .bind(investigation_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// The weekly digest
// ---------------------------------------------------------------------------

/// Whether `user_id` asked for this site's weekly digest.
pub async fn weekly_digest(pool: &PgPool, site_id: Uuid, user_id: Uuid) -> Result<bool> {
    let wanted: Option<bool> = sqlx::query_scalar(
        "SELECT weekly_digest FROM insight_subscriptions WHERE site_id = $1 AND user_id = $2",
    )
    .bind(site_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(wanted.unwrap_or(false))
}

/// Record the choice.
pub async fn set_weekly_digest(
    pool: &PgPool,
    site_id: Uuid,
    user_id: Uuid,
    wanted: bool,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO insight_subscriptions (site_id, user_id, weekly_digest) VALUES ($1, $2, $3)
         ON CONFLICT (site_id, user_id) DO UPDATE SET weekly_digest = EXCLUDED.weekly_digest",
    )
    .bind(site_id)
    .bind(user_id)
    .bind(wanted)
    .execute(pool)
    .await?;
    Ok(())
}

/// Everyone subscribed to a site's digest, with the address to send it to.
///
/// The caller still checks that each of them can read the site today: a
/// subscription outlives a membership that was revoked, and the digest must
/// not.
pub async fn subscribers(pool: &PgPool, site_id: Uuid) -> Result<Vec<Subscriber>> {
    let rows = sqlx::query_as::<_, Subscriber>(
        "SELECT u.id AS user_id, u.email::text AS email, u.name
         FROM insight_subscriptions s JOIN users u ON u.id = s.user_id
         WHERE s.site_id = $1 AND s.weekly_digest",
    )
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Sites with at least one digest subscriber.
pub async fn sites_with_subscribers(pool: &PgPool) -> Result<Vec<Uuid>> {
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT site_id FROM insight_subscriptions WHERE weekly_digest",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
