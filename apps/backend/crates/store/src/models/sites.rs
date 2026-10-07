//! `sites`, `site_access`, `share_links`. `docs/CONTRACT.md` §2.
//!
//! A site is the tracked property. It carries two identifiers and the difference
//! between them matters:
//!
//! - [`Site::id`] is the UUID every other Postgres table references.
//! - [`Site::site_id`] is a small serial integer that exists for exactly one
//!   reason: it is the `UInt16` leading column of every ClickHouse sort key, and
//!   a two-byte prefix is what makes a per-site read cheap across billions of
//!   rows.
//!
//! The API takes the numeric form (`?site=1`), so [`get_by_site_id`] is the
//! boundary where it becomes a UUID and an authorisation check.
//!
//! Access is org role by default, overridden per site by `site_access`. A share
//! link is the third path: a slug that grants read access with no account at
//! all, which open-source projects use to publish their own traffic.

use chrono::{DateTime, Utc};
use micaforge_core::{Error, Result};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A tracked property.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Site {
    /// UUIDv7 primary key, referenced by every child table.
    pub id: Uuid,
    /// The numeric mirror used as `UInt16` in ClickHouse.
    ///
    /// `i32` because Postgres `serial` is signed 32-bit; the value is always
    /// inside `u16`. Use [`Site::site_id_u16`] rather than casting at the call
    /// site.
    pub site_id: i32,
    /// Owning organisation.
    pub org_id: Uuid,
    /// Primary hostname, without scheme.
    pub domain: String,
    /// Display name.
    pub name: String,
    /// IANA timezone every report for this site is resolved in.
    pub timezone: String,
    /// True when the dashboard is readable with no account.
    pub public: bool,
    /// When the visitor salt last rotated, for the settings screen.
    pub salt_rotated_at: Option<DateTime<Utc>>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Per-site tracking options: excluded paths, auto-capture flags, and so on.
    pub settings: Json,
}

impl Site {
    /// The numeric id in the width ClickHouse uses.
    ///
    /// Saturating rather than wrapping: an install with more than 65,535 sites
    /// has outgrown the schema, and a wrapped id would silently write one site's
    /// events into another's partition. Saturating makes the failure loud and
    /// keeps it out of the wrong tenant's data.
    pub fn site_id_u16(&self) -> u16 {
        u16::try_from(self.site_id).unwrap_or(u16::MAX)
    }
}

/// A per-site role that overrides the organisation role.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SiteAccess {
    /// Site, by UUID.
    pub site_id: Uuid,
    /// User.
    pub user_id: Uuid,
    /// `owner` | `admin` | `member` | `viewer`.
    pub role: String,
}

/// A public or password-protected link to a site's dashboard.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ShareLink {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// The slug that appears in `/share/:slug`.
    pub slug: String,
    /// Argon2id digest when the link is password-protected.
    pub password_hash: Option<String>,
    /// When the link stops working. `None` never expires.
    pub expires_at: Option<DateTime<Utc>>,
    /// Which reports the link may read.
    pub scopes: Json,
}

impl ShareLink {
    /// True when this link may still be used at `now`.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.map(|e| e > now).unwrap_or(true)
    }
}

const SITE_COLS: &str = "id, site_id, org_id, domain, name, timezone, \"public\", \
     salt_rotated_at, created_at, settings";
const ACCESS_COLS: &str = "site_id, user_id, role";
const SHARE_COLS: &str = "id, site_id, slug, password_hash, expires_at, scopes";

// ---------------------------------------------------------------------------
// Sites
// ---------------------------------------------------------------------------

/// Create a site.
///
/// `site_id` is left to the sequence rather than passed in: it has to be dense
/// and unique across the whole install, and the only thing that can guarantee
/// that under concurrent creates is Postgres.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    org_id: Uuid,
    domain: &str,
    name: &str,
    timezone: &str,
) -> Result<Site> {
    let row = sqlx::query_as::<_, Site>(&format!(
        "INSERT INTO sites (id, org_id, domain, name, timezone, \"public\", created_at, settings)
         VALUES ($1, $2, $3, $4, $5, false, now(), '{{}}'::jsonb)
         RETURNING {SITE_COLS}"
    ))
    .bind(id)
    .bind(org_id)
    .bind(domain.trim().to_lowercase())
    .bind(name.trim())
    .bind(timezone)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by UUID.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Site> {
    let row = sqlx::query_as::<_, Site>(&format!("SELECT {SITE_COLS} FROM sites WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "site")
}

/// Fetch by the numeric id the API and ClickHouse use.
///
/// This is the boundary between the two identifier spaces. Everything that
/// arrives as `?site=1` comes through here exactly once, and everything after it
/// works in UUIDs.
pub async fn get_by_site_id(pool: &PgPool, site_id: u16) -> Result<Site> {
    let row =
        sqlx::query_as::<_, Site>(&format!("SELECT {SITE_COLS} FROM sites WHERE site_id = $1"))
            .bind(i32::from(site_id))
            .fetch_optional(pool)
            .await?;
    or_missing(row, "site")
}

/// Fetch by hostname, which is how the ingest path resolves a hit.
pub async fn get_by_domain(pool: &PgPool, domain: &str) -> Result<Option<Site>> {
    let row =
        sqlx::query_as::<_, Site>(&format!("SELECT {SITE_COLS} FROM sites WHERE domain = $1"))
            .bind(domain.trim().to_lowercase())
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

/// Every site in an organisation.
pub async fn list_for_org(pool: &PgPool, org_id: Uuid) -> Result<Vec<Site>> {
    let rows = sqlx::query_as::<_, Site>(&format!(
        "SELECT {SITE_COLS} FROM sites WHERE org_id = $1 ORDER BY name ASC"
    ))
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every site a user can see, across every organisation they belong to.
///
/// The `UNION` is the authorisation model in one query: a site is visible either
/// because the user is in its organisation, or because `site_access` names them
/// directly. `DISTINCT` matters, because both can be true at once.
pub async fn list_sites_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<Site>> {
    let rows = sqlx::query_as::<_, Site>(&format!(
        "SELECT DISTINCT {cols} FROM sites s
         WHERE s.org_id IN (SELECT org_id FROM org_members WHERE user_id = $1)
            OR s.id IN (SELECT site_id FROM site_access WHERE user_id = $1)
         ORDER BY s.name ASC",
        cols = prefixed(SITE_COLS, "s")
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every site on the install. Admin surface only.
pub async fn list_all(pool: &PgPool) -> Result<Vec<Site>> {
    let rows = sqlx::query_as::<_, Site>(&format!(
        "SELECT {SITE_COLS} FROM sites ORDER BY site_id ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update the fields the settings screen edits.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    domain: &str,
    timezone: &str,
    public: bool,
) -> Result<Site> {
    let row = sqlx::query_as::<_, Site>(&format!(
        "UPDATE sites SET name = $2, domain = $3, timezone = $4, \"public\" = $5
         WHERE id = $1 RETURNING {SITE_COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(domain.trim().to_lowercase())
    .bind(timezone)
    .bind(public)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "site")
}

/// Replace the settings document.
pub async fn update_settings(pool: &PgPool, id: Uuid, settings: &Json) -> Result<Site> {
    let row = sqlx::query_as::<_, Site>(&format!(
        "UPDATE sites SET settings = $2 WHERE id = $1 RETURNING {SITE_COLS}"
    ))
    .bind(id)
    .bind(settings)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "site")
}

/// Record that the visitor salt rotated.
///
/// Cosmetic: the salt itself lives in Redis with a 48-hour TTL and is never
/// written here. This column exists so the settings screen can say when it last
/// happened without reading the secret.
pub async fn touch_salt_rotated(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE sites SET salt_rotated_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete a site.
///
/// Postgres rows go with it. ClickHouse rows do not: they are removed by a job
/// that issues a partition-scoped delete, because dropping billions of rows
/// synchronously inside an HTTP request is not a thing that can work.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM sites WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-site access
// ---------------------------------------------------------------------------

/// Grant or change a per-site role.
pub async fn grant_access(
    pool: &PgPool,
    site_id: Uuid,
    user_id: Uuid,
    role: &str,
) -> Result<SiteAccess> {
    let row = sqlx::query_as::<_, SiteAccess>(&format!(
        "INSERT INTO site_access (site_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (site_id, user_id) DO UPDATE SET role = EXCLUDED.role
         RETURNING {ACCESS_COLS}"
    ))
    .bind(site_id)
    .bind(user_id)
    .bind(role)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// The override for one user on one site, if there is one.
pub async fn get_access(pool: &PgPool, site_id: Uuid, user_id: Uuid) -> Result<Option<SiteAccess>> {
    let row = sqlx::query_as::<_, SiteAccess>(&format!(
        "SELECT {ACCESS_COLS} FROM site_access WHERE site_id = $1 AND user_id = $2"
    ))
    .bind(site_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Every override on a site.
pub async fn list_access(pool: &PgPool, site_id: Uuid) -> Result<Vec<SiteAccess>> {
    let rows = sqlx::query_as::<_, SiteAccess>(&format!(
        "SELECT {ACCESS_COLS} FROM site_access WHERE site_id = $1"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Drop an override. The user falls back to their organisation role.
pub async fn revoke_access(pool: &PgPool, site_id: Uuid, user_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM site_access WHERE site_id = $1 AND user_id = $2")
        .bind(site_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Share links
// ---------------------------------------------------------------------------

/// Create a share link.
pub async fn create_share_link(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    slug: &str,
    password_hash: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
    scopes: &Json,
) -> Result<ShareLink> {
    let row = sqlx::query_as::<_, ShareLink>(&format!(
        "INSERT INTO share_links (id, site_id, slug, password_hash, expires_at, scopes)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING {SHARE_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(slug.trim())
    .bind(password_hash)
    .bind(expires_at)
    .bind(scopes)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Resolve `/share/:slug`.
///
/// Expiry is checked here rather than left to the caller, so an expired link
/// cannot be read by any route that forgets to look. It reports the same
/// `not_found` as a slug that never existed: telling a visitor that a link used
/// to exist is information about the site that they have no claim to.
pub async fn get_share_link_by_slug(pool: &PgPool, slug: &str) -> Result<ShareLink> {
    let row = sqlx::query_as::<_, ShareLink>(&format!(
        "SELECT {SHARE_COLS} FROM share_links WHERE slug = $1"
    ))
    .bind(slug.trim())
    .fetch_optional(pool)
    .await?;

    let link = or_missing(row, "share link")?;
    if !link.is_live(Utc::now()) {
        return Err(Error::not_found("share link"));
    }
    Ok(link)
}

/// Every link on a site, including expired ones, for the settings screen.
pub async fn list_share_links(pool: &PgPool, site_id: Uuid) -> Result<Vec<ShareLink>> {
    let rows = sqlx::query_as::<_, ShareLink>(&format!(
        "SELECT {SHARE_COLS} FROM share_links WHERE site_id = $1 ORDER BY id DESC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Revoke a link.
pub async fn delete_share_link(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM share_links WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Qualify a column list with a table alias.
///
/// `"public"` is already quoted in the constant, so the alias goes outside the
/// quotes and the result is `s."public"`, not `"s.public"`.
fn prefixed(cols: &str, alias: &str) -> String {
    cols.split(", ")
        .map(|c| format!("{alias}.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quoted_column_stays_quoted_when_qualified() {
        let q = prefixed(SITE_COLS, "s");
        assert!(q.contains("s.\"public\""));
        assert!(q.starts_with("s.id, s.site_id"));
    }

    #[test]
    fn an_out_of_range_numeric_id_saturates_rather_than_wrapping() {
        let site = Site {
            id: Uuid::nil(),
            site_id: 70_000,
            org_id: Uuid::nil(),
            domain: String::new(),
            name: String::new(),
            timezone: "UTC".into(),
            public: false,
            salt_rotated_at: None,
            created_at: Utc::now(),
            settings: Json::Null,
        };
        // Wrapping would land on 4464 and write this site's events into another
        // site's partition. Saturating is wrong loudly instead of quietly.
        assert_eq!(site.site_id_u16(), u16::MAX);
    }

    #[test]
    fn an_expired_link_is_not_live() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let link = ShareLink {
            id: Uuid::nil(),
            site_id: Uuid::nil(),
            slug: "x".into(),
            password_hash: None,
            expires_at: Some(past),
            scopes: Json::Null,
        };
        assert!(!link.is_live(Utc::now()));
        let forever = ShareLink {
            expires_at: None,
            ..link
        };
        assert!(forever.is_live(Utc::now()));
    }
}
