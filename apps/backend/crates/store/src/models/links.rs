//! `links` and `link_folders`.
//!
//! A short link belongs to an organisation and, optionally, to one of its
//! sites; a folder belongs to the organisation. The install has one link
//! namespace: `/l/<slug>` on the API origin and `/<slug>` on the links host
//! name the same link, so `slug` is unique on the install and
//! [`get_by_slug`] is the redirect's one lookup.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A short link.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Link {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Owning organisation.
    pub org_id: Uuid,
    /// The site it belongs to, `None` for an organisation-wide link.
    pub site_id: Option<Uuid>,
    /// Its folder.
    pub folder_id: Option<Uuid>,
    /// The short path.
    pub slug: String,
    /// Where it sends people.
    pub target_url: String,
    /// `{ source, medium, campaign, term, content }`.
    pub utm: Json,
    /// Preview card title.
    pub title: Option<String>,
    /// Preview card description.
    pub description: Option<String>,
    /// Preview card image.
    pub image_url: Option<String>,
    /// Target for iPhone, iPad and iPod.
    pub ios_url: Option<String>,
    /// Target for Android.
    pub android_url: Option<String>,
    /// An app's custom-scheme address, tried first on a phone.
    pub deep_link: Option<String>,
    /// When the link stops redirecting.
    pub expires_at: Option<DateTime<Utc>>,
    /// Who made it.
    pub created_by: Option<Uuid>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Last edit.
    pub updated_at: DateTime<Utc>,
}

impl Link {
    /// True once the expiry has passed.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }
}

/// The fields an operator sets.
#[derive(Debug, Clone)]
pub struct LinkFields {
    /// The short path.
    pub slug: String,
    /// Folder.
    pub folder_id: Option<Uuid>,
    /// Where it sends people.
    pub target_url: String,
    /// The UTM document.
    pub utm: Json,
    /// Preview title.
    pub title: Option<String>,
    /// Preview description.
    pub description: Option<String>,
    /// Preview image.
    pub image_url: Option<String>,
    /// iOS target.
    pub ios_url: Option<String>,
    /// Android target.
    pub android_url: Option<String>,
    /// App deep link.
    pub deep_link: Option<String>,
    /// Expiry.
    pub expires_at: Option<DateTime<Utc>>,
}

/// A folder, with how many links it holds.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Folder {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Owning organisation.
    pub org_id: Uuid,
    /// Name, unique within the organisation.
    pub name: String,
    /// Links filed in it.
    pub links: i64,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
}

/// Which links a list asks for.
#[derive(Debug, Clone, Copy)]
pub enum Scope {
    /// The links of one site.
    Site(Uuid),
    /// The organisation-wide links of one organisation.
    Org(Uuid),
}

/// Which folder a list asks for.
#[derive(Debug, Clone, Copy)]
pub enum FolderFilter {
    /// Every folder.
    Any,
    /// Links in no folder.
    None,
    /// One folder.
    One(Uuid),
}

const LINK_COLS: &str = "id, org_id, site_id, folder_id, slug, target_url, utm, title, \
     description, image_url, ios_url, android_url, deep_link, expires_at, created_by, \
     created_at, updated_at";

/// Create a link.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    org_id: Uuid,
    site_id: Option<Uuid>,
    created_by: Option<Uuid>,
    f: &LinkFields,
) -> Result<Link> {
    let row = sqlx::query_as::<_, Link>(&format!(
        "INSERT INTO links (id, org_id, site_id, folder_id, slug, target_url, utm, title,
             description, image_url, ios_url, android_url, deep_link, expires_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
         RETURNING {LINK_COLS}"
    ))
    .bind(id)
    .bind(org_id)
    .bind(site_id)
    .bind(f.folder_id)
    .bind(&f.slug)
    .bind(&f.target_url)
    .bind(&f.utm)
    .bind(&f.title)
    .bind(&f.description)
    .bind(&f.image_url)
    .bind(&f.ios_url)
    .bind(&f.android_url)
    .bind(&f.deep_link)
    .bind(f.expires_at)
    .bind(created_by)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Link> {
    let row = sqlx::query_as::<_, Link>(&format!("SELECT {LINK_COLS} FROM links WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "link")
}

/// A link as the redirect needs it: the row, and the numeric id of its site,
/// which is what the click row in ClickHouse is keyed by.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Resolved {
    /// The link.
    #[sqlx(flatten)]
    pub link: Link,
    /// `sites.site_id` of the link's site, `None` for an organisation link.
    pub site_number: Option<i32>,
}

/// The redirect's lookup: one indexed read, with the site's numeric id joined
/// in so a click costs no second query. `None` for a slug nobody holds.
pub async fn get_by_slug(pool: &PgPool, slug: &str) -> Result<Option<Resolved>> {
    let row = sqlx::query_as::<_, Resolved>(&format!(
        "SELECT {cols}, s.site_id AS site_number
         FROM links l LEFT JOIN sites s ON s.id = l.site_id
         WHERE l.slug = $1",
        cols = LINK_COLS
            .split(',')
            .map(|c| format!("l.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", "),
    ))
    .bind(slug)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// True when a slug is taken.
pub async fn slug_taken(pool: &PgPool, slug: &str) -> Result<bool> {
    let (taken,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM links WHERE slug = $1)")
        .bind(slug)
        .fetch_one(pool)
        .await?;
    Ok(taken)
}

/// A page of links, newest first.
///
/// `search` matches the slug, the target and the title, case-insensitively.
/// It is bound as a parameter and its `LIKE` wildcards are escaped, so a
/// search for `50%` finds `50%` rather than everything.
pub async fn list(
    pool: &PgPool,
    scope: Scope,
    folder: FolderFilter,
    search: Option<&str>,
    limit: i64,
) -> Result<Vec<Link>> {
    // The two filters are chosen with bound flags rather than by assembling
    // SQL, so the statement text is one constant and every parameter is
    // referenced whichever filter applies.
    let (by_site, scope_id) = match scope {
        Scope::Site(id) => (true, id),
        Scope::Org(id) => (false, id),
    };
    let (folder_mode, folder_id) = match folder {
        FolderFilter::Any => ("any", None),
        FolderFilter::None => ("none", None),
        FolderFilter::One(id) => ("one", Some(id)),
    };
    let pattern = search
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", escape_like(s)));

    let rows = sqlx::query_as::<_, Link>(&format!(
        "SELECT {LINK_COLS} FROM links
         WHERE (($1 AND site_id = $2) OR (NOT $1 AND org_id = $2 AND site_id IS NULL))
           AND ($3 = 'any' OR ($3 = 'none' AND folder_id IS NULL)
                OR ($3 = 'one' AND folder_id = $4))
           AND ($5::text IS NULL OR slug ILIKE $5 OR target_url ILIKE $5
                OR coalesce(title, '') ILIKE $5)
         ORDER BY created_at DESC, id DESC
         LIMIT $6"
    ))
    .bind(by_site)
    .bind(scope_id)
    .bind(folder_mode)
    .bind(folder_id)
    .bind(pattern)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Replace the operator's fields.
pub async fn update(pool: &PgPool, id: Uuid, f: &LinkFields) -> Result<Link> {
    let row = sqlx::query_as::<_, Link>(&format!(
        "UPDATE links SET slug = $2, folder_id = $3, target_url = $4, utm = $5, title = $6,
             description = $7, image_url = $8, ios_url = $9, android_url = $10,
             deep_link = $11, expires_at = $12, updated_at = now()
         WHERE id = $1 RETURNING {LINK_COLS}"
    ))
    .bind(id)
    .bind(&f.slug)
    .bind(f.folder_id)
    .bind(&f.target_url)
    .bind(&f.utm)
    .bind(&f.title)
    .bind(&f.description)
    .bind(&f.image_url)
    .bind(&f.ios_url)
    .bind(&f.android_url)
    .bind(&f.deep_link)
    .bind(f.expires_at)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "link")
}

/// Delete a link. Its clicks stay in ClickHouse until retention takes them.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM links WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------

const FOLDER_SELECT: &str = "SELECT f.id, f.org_id, f.name, \
     (SELECT count(*) FROM links l WHERE l.folder_id = f.id) AS links, f.created_at \
     FROM link_folders f";

/// Create a folder.
pub async fn create_folder(pool: &PgPool, id: Uuid, org_id: Uuid, name: &str) -> Result<Folder> {
    sqlx::query("INSERT INTO link_folders (id, org_id, name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(org_id)
        .bind(name)
        .execute(pool)
        .await?;
    get_folder(pool, id).await
}

/// Fetch a folder.
pub async fn get_folder(pool: &PgPool, id: Uuid) -> Result<Folder> {
    let row = sqlx::query_as::<_, Folder>(&format!("{FOLDER_SELECT} WHERE f.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "folder")
}

/// An organisation's folders, by name.
pub async fn list_folders(pool: &PgPool, org_id: Uuid) -> Result<Vec<Folder>> {
    let rows = sqlx::query_as::<_, Folder>(&format!(
        "{FOLDER_SELECT} WHERE f.org_id = $1 ORDER BY lower(f.name), f.id"
    ))
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Rename a folder.
pub async fn rename_folder(pool: &PgPool, id: Uuid, name: &str) -> Result<Folder> {
    let done = sqlx::query("UPDATE link_folders SET name = $2 WHERE id = $1")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await?;
    if done.rows_affected() == 0 {
        return Err(micaforge_core::Error::not_found("folder"));
    }
    get_folder(pool, id).await
}

/// Delete a folder. Its links stay, in no folder.
pub async fn delete_folder(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM link_folders WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Escape `%`, `_` and the escape character itself for a `LIKE` pattern.
fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_wildcards_in_a_search_are_literal() {
        assert_eq!(escape_like("50%_off\\"), "50\\%\\_off\\\\");
        assert_eq!(escape_like("plain"), "plain");
    }

    #[test]
    fn a_link_expires_at_its_expiry_and_not_before() {
        let now = Utc::now();
        let link = Link {
            id: Uuid::nil(),
            org_id: Uuid::nil(),
            site_id: None,
            folder_id: None,
            slug: "x".into(),
            target_url: "https://example.com".into(),
            utm: Json::Null,
            title: None,
            description: None,
            image_url: None,
            ios_url: None,
            android_url: None,
            deep_link: None,
            expires_at: Some(now),
            created_by: None,
            created_at: now,
            updated_at: now,
        };
        assert!(link.is_expired(now));
        assert!(!link.is_expired(now - chrono::Duration::seconds(1)));
        let forever = Link {
            expires_at: None,
            ..link
        };
        assert!(!forever.is_expired(now));
    }
}
