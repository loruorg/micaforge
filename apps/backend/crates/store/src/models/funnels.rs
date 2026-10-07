//! `funnels` — ordered step sequences. `docs/CONTRACT.md` §2.
//!
//! `steps` is an ordered array of filter groups, one per step, and
//! `window_hours` bounds how long a visitor has to complete the whole sequence.
//! The window is stored with the funnel rather than passed per query so that two
//! people reading the same funnel see the same number.

use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A configured funnel.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Funnel {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// Display name.
    pub name: String,
    /// Ordered array of step definitions.
    pub steps: Json,
    /// How long a visitor has to finish, in hours.
    pub window_hours: i32,
}

const COLS: &str = "id, site_id, name, steps, window_hours";

/// Create a funnel.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    name: &str,
    steps: &Json,
    window_hours: i32,
) -> Result<Funnel> {
    let row = sqlx::query_as::<_, Funnel>(&format!(
        "INSERT INTO funnels (id, site_id, name, steps, window_hours)
         VALUES ($1, $2, $3, $4, $5) RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(name.trim())
    .bind(steps)
    .bind(window_hours.max(1))
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Funnel> {
    let row = sqlx::query_as::<_, Funnel>(&format!("SELECT {COLS} FROM funnels WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "funnel")
}

/// Every funnel on a site.
pub async fn list(pool: &PgPool, site_id: Uuid) -> Result<Vec<Funnel>> {
    let rows = sqlx::query_as::<_, Funnel>(&format!(
        "SELECT {COLS} FROM funnels WHERE site_id = $1 ORDER BY name ASC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update a funnel.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    steps: &Json,
    window_hours: i32,
) -> Result<Funnel> {
    let row = sqlx::query_as::<_, Funnel>(&format!(
        "UPDATE funnels SET name = $2, steps = $3, window_hours = $4
         WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(steps)
    .bind(window_hours.max(1))
    .fetch_optional(pool)
    .await?;
    or_missing(row, "funnel")
}

/// Delete a funnel.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM funnels WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
