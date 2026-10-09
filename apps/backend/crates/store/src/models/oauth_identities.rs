//! `oauth_identities`: a Google or GitHub account linked to a Micaforge one.
//!
//! The provider's subject id is the key, so a person found once by their
//! verified address is found by the identity every time after, whatever the
//! address at the provider becomes. No provider token is stored; see
//! `migrations/0042_oauth_identities.sql`.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::pg::PgPool;

/// One linked identity.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Identity {
    /// `google` | `github`.
    pub provider: String,
    /// The provider's stable id for the person.
    pub subject: String,
    /// The Micaforge account.
    pub user_id: Uuid,
    /// The verified address the provider gave last time.
    pub email: String,
    /// When it was linked.
    pub created_at: DateTime<Utc>,
    /// When it last signed somebody in.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// `email` is cast for the same reason `users.email` is: the driver has no
/// `citext` decoder.
const COLS: &str = "provider, subject, user_id, email::text AS email, created_at, last_used_at";

/// The identity for one provider subject.
pub async fn find(pool: &PgPool, provider: &str, subject: &str) -> Result<Option<Identity>> {
    let row = sqlx::query_as::<_, Identity>(&format!(
        "SELECT {COLS} FROM oauth_identities WHERE provider = $1 AND subject = $2"
    ))
    .bind(provider)
    .bind(subject)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Link an identity to an account, recording this sign-in as its last use.
///
/// An account that already has an identity for this provider is a
/// [`micaforge_core::Error::Conflict`] (the per-account unique index): a second
/// GitHub account is not silently swapped in for the first.
pub async fn link(
    pool: &PgPool,
    provider: &str,
    subject: &str,
    user_id: Uuid,
    email: &str,
) -> Result<Identity> {
    let row = sqlx::query_as::<_, Identity>(&format!(
        "INSERT INTO oauth_identities (provider, subject, user_id, email, last_used_at)
         VALUES ($1, $2, $3, $4, now())
         RETURNING {COLS}"
    ))
    .bind(provider)
    .bind(subject)
    .bind(user_id)
    .bind(email.trim().to_lowercase())
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Record a sign-in through an identity, and the address the provider gave.
pub async fn touch(pool: &PgPool, provider: &str, subject: &str, email: &str) -> Result<()> {
    sqlx::query(
        "UPDATE oauth_identities SET last_used_at = now(), email = $3
         WHERE provider = $1 AND subject = $2",
    )
    .bind(provider)
    .bind(subject)
    .bind(email.trim().to_lowercase())
    .execute(pool)
    .await?;
    Ok(())
}

/// Every identity linked to one account.
pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<Identity>> {
    let rows = sqlx::query_as::<_, Identity>(&format!(
        "SELECT {COLS} FROM oauth_identities WHERE user_id = $1 ORDER BY provider ASC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Unlink one provider from an account. `true` when there was one.
pub async fn unlink(pool: &PgPool, user_id: Uuid, provider: &str) -> Result<bool> {
    let done = sqlx::query("DELETE FROM oauth_identities WHERE user_id = $1 AND provider = $2")
        .bind(user_id)
        .bind(provider)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}
