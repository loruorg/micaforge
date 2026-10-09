//! `annotations` — notes people, scripts and agents write on a site's charts.
//!
//! A chart asks for the annotations that *overlap* its window, not the ones
//! that start inside it: a three-day outage that began before the window
//! opened is still something the reader of that window needs to see.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// One annotation, with its author's name.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Annotation {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// The moment, or the start of the range.
    pub starts_at: DateTime<Utc>,
    /// The end of the range. `None` for a moment.
    pub ends_at: Option<DateTime<Utc>>,
    /// The note.
    pub text: String,
    /// `neutral` | `green` | `red`.
    pub color: String,
    /// A free label such as `release` or `campaign`.
    pub category: Option<String>,
    /// `dashboard` | `api` | `mcp` | `github`.
    pub source: String,
    /// The object on GitHub a note from the GitHub App points at: the release,
    /// the deployed commit, the compare view of a push. `None` for the rest.
    pub url: Option<String>,
    /// Who wrote it, while the account exists.
    pub author_id: Option<Uuid>,
    /// Their name, resolved.
    pub author_name: Option<String>,
    /// When it was written.
    pub created_at: DateTime<Utc>,
    /// When it was last edited.
    pub updated_at: DateTime<Utc>,
}

/// The fields a create or an edit sets.
#[derive(Debug, Clone)]
pub struct Fields<'a> {
    /// The moment, or the start of the range.
    pub starts_at: DateTime<Utc>,
    /// The end of the range.
    pub ends_at: Option<DateTime<Utc>>,
    /// The note.
    pub text: &'a str,
    /// `neutral` | `green` | `red`.
    pub color: &'a str,
    /// A free label.
    pub category: Option<&'a str>,
}

const SELECT: &str = "SELECT a.id, a.site_id, a.starts_at, a.ends_at, a.text, a.color, \
    a.category, a.source, a.url, a.author_id, u.name AS author_name, a.created_at, a.updated_at \
    FROM annotations a LEFT JOIN users u ON u.id = a.author_id";

/// Write an annotation.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    fields: &Fields<'_>,
    source: &str,
    author_id: Option<Uuid>,
) -> Result<Annotation> {
    sqlx::query(
        "INSERT INTO annotations (id, site_id, starts_at, ends_at, text, color, category, source, author_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(site_id)
    .bind(fields.starts_at)
    .bind(fields.ends_at)
    .bind(fields.text)
    .bind(fields.color)
    .bind(fields.category)
    .bind(source)
    .bind(author_id)
    .execute(pool)
    .await?;
    get(pool, id).await
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Annotation> {
    let row = sqlx::query_as::<_, Annotation>(&format!("{SELECT} WHERE a.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "annotation")
}

/// A site's annotations overlapping `[start, end)`, oldest first.
pub async fn list_in(
    pool: &PgPool,
    site_id: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Annotation>> {
    let rows = sqlx::query_as::<_, Annotation>(&format!(
        "{SELECT} WHERE a.site_id = $1 AND a.starts_at < $3
           AND COALESCE(a.ends_at, a.starts_at) >= $2
         ORDER BY a.starts_at, a.id LIMIT $4"
    ))
    .bind(site_id)
    .bind(start)
    .bind(end)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Replace an annotation's fields.
pub async fn update(pool: &PgPool, id: Uuid, fields: &Fields<'_>) -> Result<Annotation> {
    let done = sqlx::query(
        "UPDATE annotations SET starts_at = $2, ends_at = $3, text = $4, color = $5,
             category = $6, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(fields.starts_at)
    .bind(fields.ends_at)
    .bind(fields.text)
    .bind(fields.color)
    .bind(fields.category)
    .execute(pool)
    .await?;
    if done.rows_affected() == 0 {
        return Err(micaforge_core::Error::not_found("annotation"));
    }
    get(pool, id).await
}

/// Delete an annotation.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM annotations WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
