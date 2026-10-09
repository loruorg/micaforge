//! `imports` — GA4, Plausible, Umami, PostHog and Simple Analytics migrations.
//! `docs/CONTRACT.md` §2.
//!
//! One row per import job, tracking status and progress so the settings screen
//! can show a bar and so a failed import says why. The events themselves land in
//! ClickHouse with `import_id` set to this row's id, which is what makes an
//! import reversible: undoing one is a delete on that single column.
//!
//! An import that reads a provider's API rather than an uploaded file also
//! carries that provider's API key, sealed, in `credentials_sealed`. The column
//! is deliberately not part of [`Import`]: nothing that lists, shows or returns
//! an import ever loads it, so it cannot be serialised by accident. It is read
//! by exactly one caller, the job, through [`credentials`], and cleared by
//! [`complete`] and [`forget_credentials`].

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
    /// `ga4` | `plausible` | `umami` | `posthog` | `simple_analytics`.
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
///
/// `credentials` is the sealed API key of an API import, written in the same
/// statement as the row so there is never an API import without its key, and
/// `None` for an import that reads a file.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    source: &str,
    credentials: Option<&[u8]>,
) -> Result<Import> {
    let row = sqlx::query_as::<_, Import>(&format!(
        "INSERT INTO imports (id, site_id, source, status, progress, credentials_sealed, created_at)
         VALUES ($1, $2, $3, 'pending', 0, $4, now()) RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(source)
    .bind(credentials)
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

/// The sealed API key of an API import, if it still has one.
///
/// `None` for a file import, and for an API import that has already finished
/// or been given up, because both of those clear it.
pub async fn credentials(pool: &PgPool, id: Uuid) -> Result<Option<Vec<u8>>> {
    let row: Option<(Option<Vec<u8>>,)> =
        sqlx::query_as("SELECT credentials_sealed FROM imports WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(or_missing(row, "import")?.0)
}

/// Drop an import's sealed API key.
///
/// Called when the import is parked for good. A finished import loses its key
/// in [`complete`] instead, in the same statement that marks it done.
pub async fn forget_credentials(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE imports SET credentials_sealed = NULL WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
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
///
/// The sealed API key goes in the same statement: a finished import will never
/// call the provider again, and a key kept after its last use is only something
/// that can leak.
pub async fn complete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE imports
         SET status = 'done', progress = 100, error = NULL, credentials_sealed = NULL
         WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark an import failed, with the reason.
///
/// The progress value is left where it stopped rather than reset, because "it
/// failed at 60%" is the useful thing to show and "it failed" is not. The sealed
/// API key stays: a failure here is usually followed by a retry that needs it.
/// [`forget_credentials`] is the call for an import that will not run again.
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
/// delete silently drop event data would be a trap. A sealed API key goes with
/// the row.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM imports WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
