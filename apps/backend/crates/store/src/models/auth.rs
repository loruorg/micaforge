//! `sessions_auth` — dashboard login sessions. `docs/CONTRACT.md` §2.
//!
//! Not to be confused with an analytics session. This is the cookie a person
//! holds after signing in, and it is the only session in the product that is
//! tied to an identity.
//!
//! What is stored is a SHA-256 digest of the cookie value, never the value. A
//! database that leaks cannot be replayed as a set of logins: the digests do not
//! reverse, and a stolen digest is not a cookie.
//!
//! `ip_hash` is a digest too, for the same reason `visitor_id` is: the "recent
//! sign-ins" screen needs to distinguish two devices, which a hash does, and it
//! does not need an address, which a hash does not give it.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::pg::PgPool;

/// A signed-in session.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuthSession {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The signed-in user.
    pub user_id: Uuid,
    /// SHA-256 of the cookie value, from `micaforge_core::crypto::token_hash`.
    pub token_hash: String,
    /// User-agent at sign-in, for the device list.
    pub user_agent: Option<String>,
    /// Salted digest of the address at sign-in. Never the address.
    pub ip_hash: Option<String>,
    /// When the cookie stops being accepted.
    pub expires_at: DateTime<Utc>,
}

const COLS: &str = "id, user_id, token_hash, user_agent, ip_hash, expires_at";

/// Open a session.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    user_id: Uuid,
    token_hash: &str,
    user_agent: Option<&str>,
    ip_hash: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<AuthSession> {
    let row = sqlx::query_as::<_, AuthSession>(&format!(
        "INSERT INTO sessions_auth (id, user_id, token_hash, user_agent, ip_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLS}"
    ))
    .bind(id)
    .bind(user_id)
    .bind(token_hash)
    .bind(user_agent)
    .bind(ip_hash)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Resolve a presented cookie to a live session.
///
/// Expiry is part of the `WHERE`. A caller cannot forget to check it, and an
/// expired session is indistinguishable from one that never existed, which is
/// the correct answer to give a caller holding a stale cookie.
pub async fn find_by_token_hash(pool: &PgPool, token_hash: &str) -> Result<Option<AuthSession>> {
    let row = sqlx::query_as::<_, AuthSession>(&format!(
        "SELECT {COLS} FROM sessions_auth WHERE token_hash = $1 AND expires_at > now()"
    ))
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Every live session for a user, for the "signed in on these devices" screen.
pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<AuthSession>> {
    let rows = sqlx::query_as::<_, AuthSession>(&format!(
        "SELECT {COLS} FROM sessions_auth
         WHERE user_id = $1 AND expires_at > now()
         ORDER BY expires_at DESC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Push a session's expiry forward. This is what keeps an active user signed in.
pub async fn extend(pool: &PgPool, id: Uuid, expires_at: DateTime<Utc>) -> Result<()> {
    sqlx::query("UPDATE sessions_auth SET expires_at = $2 WHERE id = $1")
        .bind(id)
        .bind(expires_at)
        .execute(pool)
        .await?;
    Ok(())
}

/// Sign out one session.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM sessions_auth WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Sign out a session by the cookie that was presented.
pub async fn delete_by_token_hash(pool: &PgPool, token_hash: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions_auth WHERE token_hash = $1")
        .bind(token_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Sign a user out everywhere.
///
/// What a password change and a "sign out all devices" button both call. An
/// account recovery that left old cookies working would not be a recovery.
pub async fn delete_for_user(pool: &PgPool, user_id: Uuid) -> Result<u64> {
    let result = sqlx::query("DELETE FROM sessions_auth WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Delete expired rows. Housekeeping, run from the job queue.
pub async fn purge_expired(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM sessions_auth WHERE expires_at <= now()")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
