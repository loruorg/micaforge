//! `users` — the person behind a login. `docs/CONTRACT.md` §2.
//!
//! This is the only table in the product that holds a personal identifier. It is
//! not part of the analytics data path: no event row references a user, and
//! nothing here is joined against ClickHouse. That separation is what lets the
//! privacy claim be an engineering fact rather than a policy.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::{clamp_limit, or_missing};
use crate::pg::PgPool;

/// A registered user.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Login address, unique case-insensitively.
    pub email: String,
    /// Argon2id digest from `micaforge_core::crypto::hash_password`.
    ///
    /// Never leaves this crate. The only correct thing to do with it is pass it
    /// to `verify_password`.
    pub password_hash: String,
    /// Display name.
    pub name: String,
    /// Avatar URL, when the user set one.
    pub avatar_url: Option<String>,
    /// When the address was confirmed. `None` means unverified.
    pub email_verified_at: Option<DateTime<Utc>>,
    /// TOTP seed sealed with XChaCha20-Poly1305.
    ///
    /// Sealed rather than hashed because it has to be recovered to verify a
    /// code. Open it with `micaforge_core::crypto::open` and a key derived for
    /// the `totp` purpose.
    pub totp_secret_sealed: Option<Vec<u8>>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
}

impl User {
    /// True when two-factor authentication is set up.
    pub fn has_totp(&self) -> bool {
        self.totp_secret_sealed.is_some()
    }
}

/// Every column, in declaration order.
///
/// `email` is cast to `text` deliberately. The column is `citext` so that two
/// registrations differing only in case collide, and `citext` is an extension
/// type the driver has no decoder for. The cast makes the read work whether the
/// deployment has the extension or a `lower(email)` unique index instead.
const COLS: &str = "id, email::text AS email, password_hash, name, avatar_url, \
     email_verified_at, totp_secret_sealed, created_at";

/// Insert a user.
///
/// `id` is supplied rather than generated here so the caller can reference the
/// new user (in an org membership, in an audit line) inside the same
/// transaction, before this row is committed.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    email: &str,
    password_hash: &str,
    name: &str,
) -> Result<User> {
    let row = sqlx::query_as::<_, User>(&format!(
        "INSERT INTO users (id, email, password_hash, name, created_at)
         VALUES ($1, $2, $3, $4, now())
         RETURNING {COLS}"
    ))
    .bind(id)
    .bind(email.trim().to_lowercase())
    .bind(password_hash)
    .bind(name.trim())
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<User> {
    let row = sqlx::query_as::<_, User>(&format!("SELECT {COLS} FROM users WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "user")
}

/// Fetch by email address.
///
/// Returns `Option` rather than a `404`, because the caller is a login attempt
/// and it must answer identically whether the address exists or the password was
/// wrong. Turning "no such user" into a distinguishable response would make this
/// endpoint an account enumeration oracle.
pub async fn find_by_email(pool: &PgPool, email: &str) -> Result<Option<User>> {
    let row = sqlx::query_as::<_, User>(&format!("SELECT {COLS} FROM users WHERE email = $1"))
        .bind(email.trim().to_lowercase())
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// List users, newest first. Admin surface only.
pub async fn list(pool: &PgPool, limit: Option<i64>, offset: i64) -> Result<Vec<User>> {
    let rows = sqlx::query_as::<_, User>(&format!(
        "SELECT {COLS} FROM users ORDER BY created_at DESC, id DESC LIMIT $1 OFFSET $2"
    ))
    .bind(clamp_limit(limit))
    .bind(offset.max(0))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// How many users exist.
///
/// The install decides whether the first registration becomes the owner from
/// this, so it is a real query and not a cached number.
pub async fn count(pool: &PgPool) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Update the fields a user can edit about themselves.
pub async fn update_profile(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    avatar_url: Option<&str>,
) -> Result<User> {
    let row = sqlx::query_as::<_, User>(&format!(
        "UPDATE users SET name = $2, avatar_url = $3 WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(avatar_url)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "user")
}

/// Replace the password digest.
///
/// Takes a digest, never a password. Hashing belongs to
/// `micaforge_core::crypto`, and keeping it out of this crate means there is no
/// path by which a plaintext password reaches a query.
pub async fn set_password(pool: &PgPool, id: Uuid, password_hash: &str) -> Result<()> {
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(id)
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark the address confirmed, if it is not already.
///
/// The `IS NULL` guard keeps the original confirmation time rather than moving
/// it forward every time a verification link is followed again.
pub async fn mark_email_verified(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE users SET email_verified_at = now() WHERE id = $1 AND email_verified_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Store or clear the sealed TOTP seed. `None` disables two-factor.
pub async fn set_totp(pool: &PgPool, id: Uuid, sealed: Option<&[u8]>) -> Result<()> {
    sqlx::query("UPDATE users SET totp_secret_sealed = $2 WHERE id = $1")
        .bind(id)
        .bind(sealed)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete a user.
///
/// Their events are unaffected, because no event ever referenced them.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
