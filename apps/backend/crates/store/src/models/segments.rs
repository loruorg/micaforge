//! `segments` — saved filter sets. `docs/CONTRACT.md` §2.
//!
//! A segment is a `FilterGroup` with a name. The API merges it into the query
//! envelope's own filters (`?segment=…`), which is why the stored shape is
//! exactly the wire shape and not a normalised table of conditions: a segment
//! and a hand-built filter have to compile to the same SQL or the same question
//! would get two answers.

use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A saved filter set.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Segment {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// Display name.
    pub name: String,
    /// A `FilterGroup`, in the wire shape from §3.2.
    pub filters: Json,
    /// True when everyone on the site can use it, false when it is personal.
    pub shared: bool,
}

const COLS: &str = "id, site_id, name, filters, shared";

/// Create a segment.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    name: &str,
    filters: &Json,
    shared: bool,
) -> Result<Segment> {
    let row = sqlx::query_as::<_, Segment>(&format!(
        "INSERT INTO segments (id, site_id, name, filters, shared)
         VALUES ($1, $2, $3, $4, $5) RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(name.trim())
    .bind(filters)
    .bind(shared)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Segment> {
    let row = sqlx::query_as::<_, Segment>(&format!("SELECT {COLS} FROM segments WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "segment")
}

/// Every segment on a site.
pub async fn list(pool: &PgPool, site_id: Uuid) -> Result<Vec<Segment>> {
    let rows = sqlx::query_as::<_, Segment>(&format!(
        "SELECT {COLS} FROM segments WHERE site_id = $1 ORDER BY name ASC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Only the segments everyone on the site can use.
pub async fn list_shared(pool: &PgPool, site_id: Uuid) -> Result<Vec<Segment>> {
    let rows = sqlx::query_as::<_, Segment>(&format!(
        "SELECT {COLS} FROM segments WHERE site_id = $1 AND shared = true ORDER BY name ASC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update a segment.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    filters: &Json,
    shared: bool,
) -> Result<Segment> {
    let row = sqlx::query_as::<_, Segment>(&format!(
        "UPDATE segments SET name = $2, filters = $3, shared = $4
         WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(filters)
    .bind(shared)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "segment")
}

/// Delete a segment.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM segments WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
