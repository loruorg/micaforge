//! `goals` — what counts as a conversion. `docs/CONTRACT.md` §2.
//!
//! A goal is a named predicate over the event stream, stored rather than
//! hard-coded so the query engine can resolve `Metric::Conversions` against
//! whichever goal a report asks for. `conditions` is the same filter shape the
//! API takes (`docs/CONTRACT.md` §3.2), which is what lets a goal and an ad-hoc
//! filter compile to the same SQL.

use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A configured conversion.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Goal {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// Display name.
    pub name: String,
    /// `path` | `event` | `outbound` | `file` | `revenue`.
    pub kind: String,
    /// A `FilterGroup`, in the wire shape from §3.2.
    pub conditions: Json,
    /// Money assigned to one conversion, in minor units.
    ///
    /// Integer cents rather than a float: a goal value is multiplied by a
    /// conversion count, and float money that is multiplied is float money that
    /// is wrong.
    pub value_cents: i64,
}

const COLS: &str = "id, site_id, name, kind, conditions, value_cents";

/// Create a goal.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    name: &str,
    kind: &str,
    conditions: &Json,
    value_cents: i64,
) -> Result<Goal> {
    let row = sqlx::query_as::<_, Goal>(&format!(
        "INSERT INTO goals (id, site_id, name, kind, conditions, value_cents)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(name.trim())
    .bind(kind)
    .bind(conditions)
    .bind(value_cents)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Goal> {
    let row = sqlx::query_as::<_, Goal>(&format!("SELECT {COLS} FROM goals WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "goal")
}

/// Every goal on a site.
pub async fn list(pool: &PgPool, site_id: Uuid) -> Result<Vec<Goal>> {
    let rows = sqlx::query_as::<_, Goal>(&format!(
        "SELECT {COLS} FROM goals WHERE site_id = $1 ORDER BY name ASC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update a goal.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    kind: &str,
    conditions: &Json,
    value_cents: i64,
) -> Result<Goal> {
    let row = sqlx::query_as::<_, Goal>(&format!(
        "UPDATE goals SET name = $2, kind = $3, conditions = $4, value_cents = $5
         WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(kind)
    .bind(conditions)
    .bind(value_cents)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "goal")
}

/// Delete a goal.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM goals WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
