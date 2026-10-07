//! `api_keys` — bearer credentials for the stats API. `docs/CONTRACT.md` §2.
//!
//! A key is shown to its owner exactly once, at creation. What is stored is a
//! SHA-256 digest and a short public prefix. The prefix is not a secret: it is
//! how a presented token is looked up in one indexed read instead of hashing
//! every row, and it is what the settings screen shows so a key can be told
//! apart from its siblings.
//!
//! Tokens are `mfg_...`, per `docs/CONTRACT.md` §3.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A bearer credential.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApiKey {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Owning organisation.
    pub org_id: Uuid,
    /// Scoped to one site when set, the whole organisation when null.
    pub site_id: Option<Uuid>,
    /// What the operator called it.
    pub name: String,
    /// The public leading characters of the token.
    pub prefix: String,
    /// SHA-256 of the whole token, from `micaforge_core::crypto::token_hash`.
    pub hash: String,
    /// Which endpoints the key may call.
    pub scopes: Json,
    /// Last time the key authenticated a request.
    pub last_used_at: Option<DateTime<Utc>>,
    /// When the key stops working. `None` never expires.
    pub expires_at: Option<DateTime<Utc>>,
}

impl ApiKey {
    /// True when the key may still be used at `now`.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.map(|e| e > now).unwrap_or(true)
    }
}

const COLS: &str = "id, org_id, site_id, name, prefix, hash, scopes, last_used_at, expires_at";

/// Store a new key.
///
/// Takes the digest and the prefix, never the token. The plaintext exists only
/// in the response that creates it, and this crate has no way to see it.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    org_id: Uuid,
    site_id: Option<Uuid>,
    name: &str,
    prefix: &str,
    hash: &str,
    scopes: &Json,
    expires_at: Option<DateTime<Utc>>,
) -> Result<ApiKey> {
    let row = sqlx::query_as::<_, ApiKey>(&format!(
        "INSERT INTO api_keys (id, org_id, site_id, name, prefix, hash, scopes, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING {COLS}"
    ))
    .bind(id)
    .bind(org_id)
    .bind(site_id)
    .bind(name.trim())
    .bind(prefix)
    .bind(hash)
    .bind(scopes)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<ApiKey> {
    let row = sqlx::query_as::<_, ApiKey>(&format!("SELECT {COLS} FROM api_keys WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "api key")
}

/// Candidate keys for a presented token's prefix.
///
/// A `Vec`, not an `Option`, because a prefix is short enough to collide. The
/// caller compares the full digest against each candidate; that comparison, and
/// not this lookup, is what authenticates.
pub async fn find_by_prefix(pool: &PgPool, prefix: &str) -> Result<Vec<ApiKey>> {
    let rows =
        sqlx::query_as::<_, ApiKey>(&format!("SELECT {COLS} FROM api_keys WHERE prefix = $1"))
            .bind(prefix)
            .fetch_all(pool)
            .await?;
    Ok(rows)
}

/// Every key in an organisation.
pub async fn list_for_org(pool: &PgPool, org_id: Uuid) -> Result<Vec<ApiKey>> {
    let rows = sqlx::query_as::<_, ApiKey>(&format!(
        "SELECT {COLS} FROM api_keys WHERE org_id = $1 ORDER BY id DESC"
    ))
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Record that a key was just used.
///
/// Fire-and-forget from the caller's point of view: this runs after the request
/// has been authorised, and a failed write costs an inaccurate "last used"
/// timestamp and nothing else.
pub async fn touch_last_used(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Rename a key, or change what it may reach.
pub async fn update(pool: &PgPool, id: Uuid, name: &str, scopes: &Json) -> Result<ApiKey> {
    let row = sqlx::query_as::<_, ApiKey>(&format!(
        "UPDATE api_keys SET name = $2, scopes = $3 WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(scopes)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "api key")
}

/// Revoke a key.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM api_keys WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
