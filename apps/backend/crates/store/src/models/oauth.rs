//! `oauth_clients` and `oauth_grants` — OAuth 2.1 for the MCP server.
//!
//! The protocol rules (PKCE, redirect matching, scopes, lifetimes, refresh
//! rotation) live in the server's `routes::oauth`. This module stores the
//! outcome and makes the two operations that must be atomic atomic:
//!
//! - **Rotation.** [`rotate`] replaces the access and refresh digests of a grant
//!   only if the refresh digest presented is still the current one, in one
//!   `UPDATE … WHERE refresh_hash = $old`. Two clients racing with the same
//!   refresh token get one new pair between them, not two.
//! - **Revocation.** [`revoke`] stamps `revoked_at`; every read that
//!   authenticates filters on it, so a revoked grant stops working on the very
//!   next request.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A registered client.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Client {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The public identifier the client sends.
    pub client_id: String,
    /// The name it registered under.
    pub client_name: String,
    /// The exact redirect addresses it may use.
    pub redirect_uris: Json,
    /// `none` | `client_secret_post` | `client_secret_basic`.
    pub token_endpoint_auth_method: String,
    /// SHA-256 of the client secret, for a confidential client.
    pub client_secret_hash: Option<String>,
    /// The client's home page, when it gave one.
    pub client_uri: Option<String>,
    /// When it registered.
    pub created_at: DateTime<Utc>,
}

/// One consent, with its current tokens' digests.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Grant {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The client, by its row id.
    pub client_id: Uuid,
    /// The person who consented; the tokens act for them.
    pub user_id: Uuid,
    /// The one organisation the tokens may reach.
    pub org_id: Uuid,
    /// The scopes the person granted.
    pub scopes: Json,
    /// The resource the tokens are bound to.
    pub resource: String,
    /// The access token's public leading characters.
    pub access_prefix: String,
    /// SHA-256 of the access token.
    pub access_hash: String,
    /// When the access token stops working.
    pub access_expires_at: DateTime<Utc>,
    /// SHA-256 of the refresh token.
    pub refresh_hash: String,
    /// When the refresh token stops working.
    pub refresh_expires_at: DateTime<Utc>,
    /// The refresh digest this one replaced.
    pub previous_refresh_hash: Option<String>,
    /// When the person consented.
    pub created_at: DateTime<Utc>,
    /// When a token from this grant last authenticated.
    pub last_used_at: Option<DateTime<Utc>>,
    /// When it was revoked.
    pub revoked_at: Option<DateTime<Utc>>,
}

/// A grant as the "Connected apps" screen lists it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GrantRow {
    /// The grant.
    pub id: Uuid,
    /// The client's public id.
    pub client_id: String,
    /// The client's name.
    pub client_name: String,
    /// The organisation.
    pub org_id: Uuid,
    /// Its name.
    pub org_name: String,
    /// The scopes granted.
    pub scopes: Json,
    /// When.
    pub created_at: DateTime<Utc>,
    /// Last use.
    pub last_used_at: Option<DateTime<Utc>>,
    /// When the refresh token, and so the connection, runs out.
    pub refresh_expires_at: DateTime<Utc>,
}

/// The digests and lifetimes of a freshly minted token pair.
#[derive(Debug, Clone)]
pub struct TokenPair<'a> {
    /// The access token's public leading characters.
    pub access_prefix: &'a str,
    /// SHA-256 of the access token.
    pub access_hash: &'a str,
    /// When it expires.
    pub access_expires_at: DateTime<Utc>,
    /// SHA-256 of the refresh token.
    pub refresh_hash: &'a str,
    /// When it expires.
    pub refresh_expires_at: DateTime<Utc>,
}

const CLIENT_COLS: &str = "id, client_id, client_name, redirect_uris, token_endpoint_auth_method, \
    client_secret_hash, client_uri, created_at";
const GRANT_COLS: &str = "id, client_id, user_id, org_id, scopes, resource, access_prefix, \
    access_hash, access_expires_at, refresh_hash, refresh_expires_at, previous_refresh_hash, \
    created_at, last_used_at, revoked_at";

/// Register a client.
#[allow(clippy::too_many_arguments)]
pub async fn create_client(
    pool: &PgPool,
    id: Uuid,
    client_id: &str,
    client_name: &str,
    redirect_uris: &Json,
    auth_method: &str,
    client_secret_hash: Option<&str>,
    client_uri: Option<&str>,
) -> Result<Client> {
    let row = sqlx::query_as::<_, Client>(&format!(
        "INSERT INTO oauth_clients (id, client_id, client_name, redirect_uris,
             token_endpoint_auth_method, client_secret_hash, client_uri)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {CLIENT_COLS}"
    ))
    .bind(id)
    .bind(client_id)
    .bind(client_name)
    .bind(redirect_uris)
    .bind(auth_method)
    .bind(client_secret_hash)
    .bind(client_uri)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// A client by its public id.
pub async fn find_client(pool: &PgPool, client_id: &str) -> Result<Option<Client>> {
    let row = sqlx::query_as::<_, Client>(&format!(
        "SELECT {CLIENT_COLS} FROM oauth_clients WHERE client_id = $1"
    ))
    .bind(client_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// A client by its row id.
pub async fn get_client(pool: &PgPool, id: Uuid) -> Result<Client> {
    let row = sqlx::query_as::<_, Client>(&format!(
        "SELECT {CLIENT_COLS} FROM oauth_clients WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "oauth client")
}

/// Record a consent and its first token pair.
#[allow(clippy::too_many_arguments)]
pub async fn create_grant(
    pool: &PgPool,
    id: Uuid,
    client_id: Uuid,
    user_id: Uuid,
    org_id: Uuid,
    scopes: &Json,
    resource: &str,
    tokens: &TokenPair<'_>,
) -> Result<Grant> {
    let row = sqlx::query_as::<_, Grant>(&format!(
        "INSERT INTO oauth_grants (id, client_id, user_id, org_id, scopes, resource, access_prefix,
             access_hash, access_expires_at, refresh_hash, refresh_expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {GRANT_COLS}"
    ))
    .bind(id)
    .bind(client_id)
    .bind(user_id)
    .bind(org_id)
    .bind(scopes)
    .bind(resource)
    .bind(tokens.access_prefix)
    .bind(tokens.access_hash)
    .bind(tokens.access_expires_at)
    .bind(tokens.refresh_hash)
    .bind(tokens.refresh_expires_at)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Live grants whose access token starts with `prefix`.
///
/// A `Vec`: the caller compares the full digest in constant time, exactly as
/// API keys are checked.
pub async fn find_by_access_prefix(pool: &PgPool, prefix: &str) -> Result<Vec<Grant>> {
    let rows = sqlx::query_as::<_, Grant>(&format!(
        "SELECT {GRANT_COLS} FROM oauth_grants WHERE access_prefix = $1 AND revoked_at IS NULL"
    ))
    .bind(prefix)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The live grant whose current refresh digest is `hash`.
pub async fn find_by_refresh(pool: &PgPool, hash: &str) -> Result<Option<Grant>> {
    let row = sqlx::query_as::<_, Grant>(&format!(
        "SELECT {GRANT_COLS} FROM oauth_grants WHERE refresh_hash = $1 AND revoked_at IS NULL"
    ))
    .bind(hash)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// The grant whose *previous* refresh digest is `hash`: a replayed token.
pub async fn find_by_previous_refresh(pool: &PgPool, hash: &str) -> Result<Option<Grant>> {
    let row = sqlx::query_as::<_, Grant>(&format!(
        "SELECT {GRANT_COLS} FROM oauth_grants WHERE previous_refresh_hash = $1"
    ))
    .bind(hash)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// A grant by id.
pub async fn get_grant(pool: &PgPool, id: Uuid) -> Result<Grant> {
    let row = sqlx::query_as::<_, Grant>(&format!(
        "SELECT {GRANT_COLS} FROM oauth_grants WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "connected app")
}

/// Swap a grant's tokens, only if `old_refresh_hash` is still its current one.
///
/// `None` when it was not: another request rotated it first, or it was revoked.
pub async fn rotate(
    pool: &PgPool,
    id: Uuid,
    old_refresh_hash: &str,
    tokens: &TokenPair<'_>,
) -> Result<Option<Grant>> {
    let row = sqlx::query_as::<_, Grant>(&format!(
        "UPDATE oauth_grants SET access_prefix = $3, access_hash = $4, access_expires_at = $5,
             refresh_hash = $6, refresh_expires_at = $7, previous_refresh_hash = $2,
             last_used_at = now()
         WHERE id = $1 AND refresh_hash = $2 AND revoked_at IS NULL
         RETURNING {GRANT_COLS}"
    ))
    .bind(id)
    .bind(old_refresh_hash)
    .bind(tokens.access_prefix)
    .bind(tokens.access_hash)
    .bind(tokens.access_expires_at)
    .bind(tokens.refresh_hash)
    .bind(tokens.refresh_expires_at)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Revoke a grant. Idempotent.
pub async fn revoke(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE oauth_grants SET revoked_at = COALESCE(revoked_at, now()) WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record that a grant's access token was just used. Best effort.
pub async fn touch(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE oauth_grants SET last_used_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// A person's live connections, newest first.
pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<GrantRow>> {
    let rows = sqlx::query_as::<_, GrantRow>(
        "SELECT g.id, c.client_id, c.client_name, g.org_id, o.name AS org_name, g.scopes,
                g.created_at, g.last_used_at, g.refresh_expires_at
         FROM oauth_grants g
         JOIN oauth_clients c ON c.id = g.client_id
         JOIN organizations o ON o.id = g.org_id
         WHERE g.user_id = $1 AND g.revoked_at IS NULL AND g.refresh_expires_at > now()
         ORDER BY g.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
