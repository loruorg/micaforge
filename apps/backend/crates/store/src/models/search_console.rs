//! `search_console_connections` — one site's link to Google Search Console.
//!
//! The row holds the sealed refresh token, the chosen property and where the
//! sync stands. This crate stores the sealed bytes it is handed and never sees
//! a plaintext token, the same rule `log_drains` and `imports` follow: the
//! sealing lives in `micaforge_jobs::search_console`, beside the only code that
//! opens it.
//!
//! Every function is keyed by the site's UUID, which is the primary key: a
//! site has at most one connection, and an authorised site is the only way a
//! route reaches one.

use chrono::{DateTime, NaiveDate, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// Connected, no property chosen yet.
pub const PENDING: &str = "pending";
/// A property is chosen and the daily sync runs.
pub const ACTIVE: &str = "active";
/// Google refused the grant; the site must connect again.
pub const REVOKED: &str = "revoked";

/// A site's connection.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Connection {
    /// The site's UUID.
    pub site_id: Uuid,
    /// Who approved the grant, while that account exists.
    pub connected_by: Option<Uuid>,
    /// The sealed refresh token. `None` once Google refused it. Never
    /// serialised.
    pub refresh_token_sealed: Option<Vec<u8>>,
    /// `sc-domain:…` or a URL prefix; `None` until one is chosen.
    pub property: Option<String>,
    /// [`PENDING`], [`ACTIVE`] or [`REVOKED`].
    pub status: String,
    /// The oldest day the backfill will reach.
    pub backfill_until: Option<NaiveDate>,
    /// The oldest day fetched so far.
    pub backfilled_from: Option<NaiveDate>,
    /// The newest day fetched so far.
    pub synced_through: Option<NaiveDate>,
    /// Until when a worker holds this site's sync.
    pub lease_until: Option<DateTime<Utc>>,
    /// When the last sync began.
    pub last_sync_started_at: Option<DateTime<Utc>>,
    /// When the last sync ended, successfully or not.
    pub last_sync_finished_at: Option<DateTime<Utc>>,
    /// The last failure, in a sentence safe to show an admin.
    pub last_error: Option<String>,
    /// When it happened.
    pub last_error_at: Option<DateTime<Utc>>,
    /// Rows written to ClickHouse over the connection's life.
    pub rows_synced: i64,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

impl Connection {
    /// True when the daily sync should run for this site.
    pub fn is_syncable(&self) -> bool {
        self.status == ACTIVE && self.property.is_some() && self.refresh_token_sealed.is_some()
    }
}

const COLS: &str = "site_id, connected_by, refresh_token_sealed, property, status, \
    backfill_until, backfilled_from, synced_through, lease_until, last_sync_started_at, \
    last_sync_finished_at, last_error, last_error_at, rows_synced, created_at, updated_at";

/// The site's connection, if it has one.
pub async fn find(pool: &PgPool, site_id: Uuid) -> Result<Option<Connection>> {
    let row = sqlx::query_as::<_, Connection>(&format!(
        "SELECT {COLS} FROM search_console_connections WHERE site_id = $1"
    ))
    .bind(site_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// The site's connection, or `404`.
pub async fn get(pool: &PgPool, site_id: Uuid) -> Result<Connection> {
    or_missing(find(pool, site_id).await?, "Search Console connection")
}

/// Store a fresh grant for a site.
///
/// Connecting again replaces the token and starts over at `pending`, because
/// the new grant may belong to another Google account that sees other
/// properties. The sync window is reset with it; the rows already in
/// ClickHouse stay, keyed by property, and are read again if the same property
/// is chosen.
pub async fn upsert_grant(
    pool: &PgPool,
    site_id: Uuid,
    connected_by: Uuid,
    sealed: &[u8],
) -> Result<Connection> {
    let row = sqlx::query_as::<_, Connection>(&format!(
        "INSERT INTO search_console_connections (site_id, connected_by, refresh_token_sealed, status)
         VALUES ($1, $2, $3, 'pending')
         ON CONFLICT (site_id) DO UPDATE SET
             connected_by = EXCLUDED.connected_by,
             refresh_token_sealed = EXCLUDED.refresh_token_sealed,
             property = NULL,
             status = 'pending',
             backfill_until = NULL,
             backfilled_from = NULL,
             synced_through = NULL,
             last_error = NULL,
             last_error_at = NULL,
             updated_at = now()
         RETURNING {COLS}"
    ))
    .bind(site_id)
    .bind(connected_by)
    .bind(sealed)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Choose the property to read, and start its sync window.
///
/// Refused (`404`) when the grant has been revoked meanwhile: a property
/// cannot be chosen through a token Google no longer accepts.
pub async fn set_property(
    pool: &PgPool,
    site_id: Uuid,
    property: &str,
    backfill_until: NaiveDate,
) -> Result<Connection> {
    let row = sqlx::query_as::<_, Connection>(&format!(
        "UPDATE search_console_connections SET
             property = $2,
             status = 'active',
             backfill_until = $3,
             backfilled_from = NULL,
             synced_through = NULL,
             last_error = NULL,
             last_error_at = NULL,
             updated_at = now()
         WHERE site_id = $1 AND refresh_token_sealed IS NOT NULL
         RETURNING {COLS}"
    ))
    .bind(site_id)
    .bind(property)
    .bind(backfill_until)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "Search Console connection")
}

/// Remove the connection. Returns the row that was removed, if any.
pub async fn delete(pool: &PgPool, site_id: Uuid) -> Result<Option<Connection>> {
    let row = sqlx::query_as::<_, Connection>(&format!(
        "DELETE FROM search_console_connections WHERE site_id = $1 RETURNING {COLS}"
    ))
    .bind(site_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Every connection the daily sweep should sync.
pub async fn list_syncable(pool: &PgPool) -> Result<Vec<Connection>> {
    let rows = sqlx::query_as::<_, Connection>(&format!(
        "SELECT {COLS} FROM search_console_connections
         WHERE status = 'active' AND property IS NOT NULL AND refresh_token_sealed IS NOT NULL
         ORDER BY last_sync_finished_at NULLS FIRST"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Take the site's sync lease for `secs` seconds.
///
/// `None` when another worker holds it, or when the connection is no longer
/// syncable (disconnected, revoked, or without a property). One conditional
/// statement, so two workers cannot both win.
pub async fn take_lease(pool: &PgPool, site_id: Uuid, secs: i64) -> Result<Option<Connection>> {
    let row = sqlx::query_as::<_, Connection>(&format!(
        "UPDATE search_console_connections SET
             lease_until = now() + make_interval(secs => $2::double precision),
             last_sync_started_at = now(),
             updated_at = now()
         WHERE site_id = $1
           AND status = 'active' AND property IS NOT NULL AND refresh_token_sealed IS NOT NULL
           AND (lease_until IS NULL OR lease_until < now())
         RETURNING {COLS}"
    ))
    .bind(site_id)
    .bind(secs as f64)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// What one sync run achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The oldest day fetched so far.
    pub backfilled_from: Option<NaiveDate>,
    /// The newest day fetched so far.
    pub synced_through: Option<NaiveDate>,
    /// Rows this run wrote.
    pub rows: i64,
}

/// Record a run that succeeded, and give the lease back.
///
/// Written only while the property is still the one the run read: a run that
/// finishes after the admin switched properties must not stamp its window on
/// the new one.
pub async fn finish_ok(pool: &PgPool, site_id: Uuid, property: &str, p: &Progress) -> Result<()> {
    sqlx::query(
        "UPDATE search_console_connections SET
             backfilled_from = $3,
             synced_through = $4,
             rows_synced = rows_synced + $5,
             lease_until = NULL,
             last_sync_finished_at = now(),
             last_error = NULL,
             last_error_at = NULL,
             updated_at = now()
         WHERE site_id = $1 AND property = $2",
    )
    .bind(site_id)
    .bind(property)
    .bind(p.backfilled_from)
    .bind(p.synced_through)
    .bind(p.rows)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record a run that failed, keep whatever it achieved, and give the lease
/// back. `error` is shown to admins, so it must already be safe to show.
pub async fn finish_failed(
    pool: &PgPool,
    site_id: Uuid,
    property: &str,
    p: &Progress,
    error: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE search_console_connections SET
             backfilled_from = $3,
             synced_through = $4,
             rows_synced = rows_synced + $5,
             lease_until = NULL,
             last_sync_finished_at = now(),
             last_error = $6,
             last_error_at = now(),
             updated_at = now()
         WHERE site_id = $1 AND property = $2",
    )
    .bind(site_id)
    .bind(property)
    .bind(p.backfilled_from)
    .bind(p.synced_through)
    .bind(p.rows)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

/// Google refused the grant: drop the token, mark the connection revoked,
/// and say why. The rows already synced stay readable.
pub async fn mark_revoked(pool: &PgPool, site_id: Uuid, error: &str) -> Result<()> {
    sqlx::query(
        "UPDATE search_console_connections SET
             refresh_token_sealed = NULL,
             status = 'revoked',
             lease_until = NULL,
             last_sync_finished_at = now(),
             last_error = $2,
             last_error_at = now(),
             updated_at = now()
         WHERE site_id = $1",
    )
    .bind(site_id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}
