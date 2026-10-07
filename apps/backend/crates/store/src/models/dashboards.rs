//! `dashboards` — user-composed report grids. `docs/CONTRACT.md` §2.
//!
//! `layout` holds the widget list and their positions. It is opaque here on
//! purpose: the dashboard editor owns that shape, and giving this crate an
//! opinion about it would mean a layout change needed a migration.

use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A saved dashboard.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Dashboard {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// Display name.
    pub name: String,
    /// Widgets and their positions.
    pub layout: Json,
}

const COLS: &str = "id, site_id, name, layout";

/// Create a dashboard.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    name: &str,
    layout: &Json,
) -> Result<Dashboard> {
    let row = sqlx::query_as::<_, Dashboard>(&format!(
        "INSERT INTO dashboards (id, site_id, name, layout)
         VALUES ($1, $2, $3, $4) RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(name.trim())
    .bind(layout)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Dashboard> {
    let row =
        sqlx::query_as::<_, Dashboard>(&format!("SELECT {COLS} FROM dashboards WHERE id = $1"))
            .bind(id)
            .fetch_optional(pool)
            .await?;
    or_missing(row, "dashboard")
}

/// Every dashboard on a site.
pub async fn list(pool: &PgPool, site_id: Uuid) -> Result<Vec<Dashboard>> {
    let rows = sqlx::query_as::<_, Dashboard>(&format!(
        "SELECT {COLS} FROM dashboards WHERE site_id = $1 ORDER BY name ASC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update a dashboard.
pub async fn update(pool: &PgPool, id: Uuid, name: &str, layout: &Json) -> Result<Dashboard> {
    let row = sqlx::query_as::<_, Dashboard>(&format!(
        "UPDATE dashboards SET name = $2, layout = $3 WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(layout)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "dashboard")
}

/// Delete a dashboard.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM dashboards WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
