//! `chat_links` and `chat_investigation_posts` — which chat channel answers
//! for which site, and where each investigation was posted.
//!
//! A link is looked up from the platform's side on every incoming message
//! (platform, workspace, channel), and from the site's side when the settings
//! screen lists them and when an investigation is posted. Both reads are
//! indexed.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// One linked channel.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatLink {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The organisation the site belongs to.
    pub org_id: Uuid,
    /// The site the channel answers for.
    pub site_id: Uuid,
    /// `slack` | `discord` | `telegram`.
    pub platform: String,
    /// Slack team or Discord guild; empty for Telegram.
    pub workspace_id: String,
    /// The channel or chat.
    pub channel_id: String,
    /// What the settings screen shows.
    pub label: String,
    /// Who linked it.
    pub created_by: Option<Uuid>,
    /// When.
    pub created_at: DateTime<Utc>,
}

/// A link with the name of the person who made it, for the settings screen.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatLinkRow {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// `slack` | `discord` | `telegram`.
    pub platform: String,
    /// Slack team or Discord guild.
    pub workspace_id: String,
    /// The channel or chat.
    pub channel_id: String,
    /// What the settings screen shows.
    pub label: String,
    /// When.
    pub created_at: DateTime<Utc>,
    /// The linker's display name, when the account still exists.
    pub created_by_name: Option<String>,
}

const COLS: &str =
    "id, org_id, site_id, platform, workspace_id, channel_id, label, created_by, created_at";

/// Link a channel to a site, moving it if it was linked to another.
///
/// An upsert on the channel: one channel answers for one site, and linking it
/// again is how a person points it somewhere else.
#[allow(clippy::too_many_arguments)]
pub async fn link(
    pool: &PgPool,
    id: Uuid,
    org_id: Uuid,
    site_id: Uuid,
    platform: &str,
    workspace_id: &str,
    channel_id: &str,
    label: &str,
    created_by: Option<Uuid>,
) -> Result<ChatLink> {
    let row = sqlx::query_as::<_, ChatLink>(&format!(
        "INSERT INTO chat_links (id, org_id, site_id, platform, workspace_id, channel_id, label, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (platform, workspace_id, channel_id) DO UPDATE SET
             org_id = EXCLUDED.org_id, site_id = EXCLUDED.site_id, label = EXCLUDED.label,
             created_by = EXCLUDED.created_by, created_at = now()
         RETURNING {COLS}"
    ))
    .bind(id)
    .bind(org_id)
    .bind(site_id)
    .bind(platform)
    .bind(workspace_id)
    .bind(channel_id)
    .bind(label)
    .bind(created_by)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// The link for a channel, from the platform's side.
pub async fn find(
    pool: &PgPool,
    platform: &str,
    workspace_id: &str,
    channel_id: &str,
) -> Result<Option<ChatLink>> {
    let row = sqlx::query_as::<_, ChatLink>(&format!(
        "SELECT {COLS} FROM chat_links WHERE platform = $1 AND workspace_id = $2 AND channel_id = $3"
    ))
    .bind(platform)
    .bind(workspace_id)
    .bind(channel_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<ChatLink> {
    let row =
        sqlx::query_as::<_, ChatLink>(&format!("SELECT {COLS} FROM chat_links WHERE id = $1"))
            .bind(id)
            .fetch_optional(pool)
            .await?;
    or_missing(row, "chat link")
}

/// Every link of a site.
pub async fn for_site(pool: &PgPool, site_id: Uuid) -> Result<Vec<ChatLink>> {
    let rows = sqlx::query_as::<_, ChatLink>(&format!(
        "SELECT {COLS} FROM chat_links WHERE site_id = $1 ORDER BY created_at"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every link of a site with its linker's name, for the settings screen.
pub async fn list_for_site(pool: &PgPool, site_id: Uuid) -> Result<Vec<ChatLinkRow>> {
    let rows = sqlx::query_as::<_, ChatLinkRow>(
        "SELECT l.id, l.platform, l.workspace_id, l.channel_id, l.label, l.created_at,
                u.name AS created_by_name
         FROM chat_links l LEFT JOIN users u ON u.id = l.created_by
         WHERE l.site_id = $1 ORDER BY l.created_at",
    )
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Remove a link.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM chat_links WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Where an investigation was first posted in a linked channel.
pub async fn post_ref(
    pool: &PgPool,
    investigation_id: Uuid,
    link_id: Uuid,
) -> Result<Option<String>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT thread_ref FROM chat_investigation_posts WHERE investigation_id = $1 AND link_id = $2",
    )
    .bind(investigation_id)
    .bind(link_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Remember where an investigation was first posted.
pub async fn record_post(
    pool: &PgPool,
    investigation_id: Uuid,
    link_id: Uuid,
    thread_ref: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO chat_investigation_posts (investigation_id, link_id, thread_ref)
         VALUES ($1, $2, $3) ON CONFLICT (investigation_id, link_id) DO NOTHING",
    )
    .bind(investigation_id)
    .bind(link_id)
    .bind(thread_ref)
    .execute(pool)
    .await?;
    Ok(())
}
