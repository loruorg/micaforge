//! `log_drains` — the addresses a hosting platform posts request logs to.
//!
//! One row is one drain URL, `POST /api/log/vercel/{id}`, owned by one site. The
//! row holds the HMAC key the platform signs each delivery with, sealed, and
//! the ownership token some platform flows ask the endpoint to echo. Neither
//! the sealing nor the meaning of a delivery lives here: this crate stores the
//! sealed text it is handed and never sees a plaintext secret, the same rule
//! `api_keys` follows for its digests.
//!
//! Every read and write a management route makes is scoped by `site_id` as
//! well as by `id`. A drain id arrives in a URL next to a site reference, and a
//! query keyed on the drain alone would let a caller authorised on one site act
//! on another site's drain by pairing the two. [`find`] is the one unscoped
//! read, for the collection route, where the drain id is the whole address and
//! the signature is the authorisation.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// The platforms a drain can be created for. The `CHECK` on `provider` is the
/// same list.
pub const PROVIDERS: &[&str] = &["vercel"];

/// A drain.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LogDrain {
    /// UUIDv7 primary key, and the last segment of the drain URL.
    pub id: Uuid,
    /// The owning site's UUID, never the numeric ClickHouse id.
    pub site_id: Uuid,
    /// One of [`PROVIDERS`].
    pub provider: String,
    /// The signing secret, sealed with the drain id as additional data and
    /// encoded as unpadded url-safe base64. Never serialised.
    pub secret_sealed: String,
    /// What the platform's URL ownership check expects back, when it asks.
    pub verify_token: Option<String>,
    /// The account that created it, while that account exists.
    pub created_by: Option<Uuid>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// The last verified delivery, at minute resolution.
    pub last_received_at: Option<DateTime<Utc>>,
}

const COLS: &str =
    "id, site_id, provider, secret_sealed, verify_token, created_by, created_at, last_received_at";

/// Store a new drain.
///
/// `id` is minted by the caller before the insert because the secret is
/// sealed against it, so the sealed text cannot exist until the id does.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    provider: &str,
    secret_sealed: &str,
    verify_token: Option<&str>,
    created_by: Option<Uuid>,
) -> Result<LogDrain> {
    let row = sqlx::query_as::<_, LogDrain>(&format!(
        "INSERT INTO log_drains (id, site_id, provider, secret_sealed, verify_token, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(provider)
    .bind(secret_sealed)
    .bind(verify_token)
    .bind(created_by)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// One drain by id alone, or `None`.
///
/// For the collection route only, which has nothing but the id and reads the
/// row fresh on every delivery, so a rotated secret stops verifying at once.
pub async fn find(pool: &PgPool, id: Uuid) -> Result<Option<LogDrain>> {
    let row =
        sqlx::query_as::<_, LogDrain>(&format!("SELECT {COLS} FROM log_drains WHERE id = $1"))
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

/// One drain of one site. A drain of another site is `not_found`.
pub async fn get_for_site(pool: &PgPool, site_id: Uuid, id: Uuid) -> Result<LogDrain> {
    let row = sqlx::query_as::<_, LogDrain>(&format!(
        "SELECT {COLS} FROM log_drains WHERE id = $1 AND site_id = $2"
    ))
    .bind(id)
    .bind(site_id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "log drain")
}

/// Every drain of one site, oldest first.
pub async fn list_for_site(pool: &PgPool, site_id: Uuid) -> Result<Vec<LogDrain>> {
    let rows = sqlx::query_as::<_, LogDrain>(&format!(
        "SELECT {COLS} FROM log_drains WHERE site_id = $1 ORDER BY id"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// How many drains one site has.
pub async fn count_for_site(pool: &PgPool, site_id: Uuid) -> Result<i64> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM log_drains WHERE site_id = $1")
        .bind(site_id)
        .fetch_one(pool)
        .await?;
    Ok(count)
}

/// Replace the sealed secret. The previous one stops verifying when this
/// commits, because the collection route reads the row on every delivery.
pub async fn set_secret(
    pool: &PgPool,
    site_id: Uuid,
    id: Uuid,
    secret_sealed: &str,
) -> Result<LogDrain> {
    let row = sqlx::query_as::<_, LogDrain>(&format!(
        "UPDATE log_drains SET secret_sealed = $3 WHERE id = $1 AND site_id = $2 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(secret_sealed)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "log drain")
}

/// Set or clear the ownership token.
pub async fn set_verify_token(
    pool: &PgPool,
    site_id: Uuid,
    id: Uuid,
    verify_token: Option<&str>,
) -> Result<LogDrain> {
    let row = sqlx::query_as::<_, LogDrain>(&format!(
        "UPDATE log_drains SET verify_token = $3 WHERE id = $1 AND site_id = $2 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(verify_token)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "log drain")
}

/// Record a verified delivery, unless one was recorded in the last minute.
///
/// The minute is enforced in the statement as well as by the caller, so two
/// replicas receiving deliveries for the same drain in the same second write
/// one row version between them rather than one each. Best effort from the
/// caller's side: a failure costs a stale "last received" and nothing else.
pub async fn mark_received(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE log_drains SET last_received_at = now()
         WHERE id = $1
           AND (last_received_at IS NULL OR last_received_at < now() - interval '1 minute')",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete a drain. Its URL stops verifying at once; a drain that was already
/// gone is `not_found`, so a double click reports what happened.
pub async fn delete(pool: &PgPool, site_id: Uuid, id: Uuid) -> Result<()> {
    let done = sqlx::query("DELETE FROM log_drains WHERE id = $1 AND site_id = $2")
        .bind(id)
        .bind(site_id)
        .execute(pool)
        .await?;
    if done.rows_affected() == 0 {
        return Err(micaforge_core::Error::not_found("log drain"));
    }
    Ok(())
}
