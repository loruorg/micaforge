//! `imports` — GA4, Plausible and Umami migrations. `docs/CONTRACT.md` §2.
//!
//! One row per import job, tracking status and progress so the settings screen
//! can show a bar and so a failed import says why. The events themselves land in
//! ClickHouse with `import_id` set to this row's id, which is what makes an
//! import reversible: undoing one is a delete on that single column.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// An import job.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Import {
    /// UUIDv7 primary key. Also written to `events.import_id`.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// `ga4` | `plausible` | `umami`.
    pub source: String,
    /// `pending` | `running` | `done` | `failed`.
    pub status: String,
    /// Percentage complete, 0-100.
    pub progress: i32,
    /// Why it failed, when it did.
    pub error: Option<String>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
}

const COLS: &str = "id, site_id, source, status, progress, error, created_at";

/// Queue an import.
pub async fn create(pool: &PgPool, id: Uuid, site_id: Uuid, source: &str) -> Result<Import> {
    let row = sqlx::query_as::<_, Import>(&format!(
        "INSERT INTO imports (id, site_id, source, status, progress, created_at)
         VALUES ($1, $2, $3, 'pending', 0, now()) RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(source)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Import> {
    let row = sqlx::query_as::<_, Import>(&format!("SELECT {COLS} FROM imports WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "import")
}

/// Every import on a site, newest first.
pub async fn list_for_site(pool: &PgPool, site_id: Uuid) -> Result<Vec<Import>> {
    let rows = sqlx::query_as::<_, Import>(&format!(
        "SELECT {COLS} FROM imports WHERE site_id = $1 ORDER BY created_at DESC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Move the progress bar, and mark the job running.
pub async fn update_progress(pool: &PgPool, id: Uuid, progress: i32) -> Result<()> {
    sqlx::query("UPDATE imports SET progress = $2, status = 'running' WHERE id = $1")
        .bind(id)
        .bind(progress.clamp(0, 100))
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark an import finished.
pub async fn complete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE imports SET status = 'done', progress = 100, error = NULL WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark an import failed, with the reason.
///
/// The progress value is left where it stopped rather than reset, because "it
/// failed at 60%" is the useful thing to show and "it failed" is not.
pub async fn fail(pool: &PgPool, id: Uuid, error: &str) -> Result<()> {
    sqlx::query("UPDATE imports SET status = 'failed', error = $2 WHERE id = $1")
        .bind(id)
        .bind(error)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete an import record.
///
/// Does not remove the rows it wrote to ClickHouse. Undoing an import is a
/// separate, explicit job keyed on `events.import_id`; making a settings-screen
/// delete silently drop event data would be a trap.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM imports WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
