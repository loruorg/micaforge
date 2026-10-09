//! `flag_target_groups` and `flag_changes`: what a flag is aimed at, and what
//! was done to it.
//!
//! A **target group** is an audience defined once (beta users, staff, one
//! country) and attached to many flags, so that adding a person to the beta
//! is one edit rather than one per flag. Its condition document is the same
//! shape a flag's own conditions take, validated by
//! `micaforge_core::flags::check_conditions` before it is written.
//!
//! A **change** is one write to a flag, with the whole editable definition
//! before and after it. The flag screen draws a field-by-field diff from the
//! pair and can put any earlier state back; a revert is itself a change, so
//! undoing a revert is just reverting again.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::{clamp_limit, or_missing};
use crate::pg::PgPool;

/// A reusable audience for flags.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TargetGroup {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// Unique per site.
    pub name: String,
    /// What the audience is, in the operator's words.
    pub description: String,
    /// The condition document, in group form.
    pub conditions: Json,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Last edit.
    pub updated_at: DateTime<Utc>,
}

/// One recorded write to a flag.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FlagChange {
    /// UUIDv7 primary key; orders by time.
    pub id: Uuid,
    /// The flag.
    pub flag_id: Uuid,
    /// The flag's site, so a site's history can be read without a join.
    pub site_id: Uuid,
    /// `created` | `updated` | `toggled` | `reverted`.
    pub action: String,
    /// `user` | `api_key` | `system`.
    pub actor_kind: String,
    /// The user or key, when there was one.
    pub actor_id: Option<Uuid>,
    /// Who, as it read at the time: an address, or a key's name.
    pub actor_label: String,
    /// The editable definition before the write. `None` for a creation.
    pub before: Option<Json>,
    /// The editable definition after it.
    pub after: Option<Json>,
    /// When.
    pub created_at: DateTime<Utc>,
}

/// Who made a change, as the change rows store it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeActor {
    /// `user` | `api_key` | `system`.
    pub kind: String,
    /// The user or key id.
    pub id: Option<Uuid>,
    /// A readable label.
    pub label: String,
}

const GROUP_COLS: &str = "id, site_id, name, description, conditions, created_at, updated_at";
const CHANGE_COLS: &str = "id, flag_id, site_id, action, actor_kind, actor_id, actor_label, \
     before, after, created_at";

// ---------------------------------------------------------------------------
// Target groups
// ---------------------------------------------------------------------------

/// Create a target group. A second group with the same name on the site is a
/// [`micaforge_core::Error::Conflict`] from the unique index.
pub async fn create_group(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    name: &str,
    description: &str,
    conditions: &Json,
) -> Result<TargetGroup> {
    let row = sqlx::query_as::<_, TargetGroup>(&format!(
        "INSERT INTO flag_target_groups (id, site_id, name, description, conditions)
         VALUES ($1, $2, $3, $4, $5) RETURNING {GROUP_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(name.trim())
    .bind(description.trim())
    .bind(conditions)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// One target group.
pub async fn get_group(pool: &PgPool, id: Uuid) -> Result<TargetGroup> {
    let row = sqlx::query_as::<_, TargetGroup>(&format!(
        "SELECT {GROUP_COLS} FROM flag_target_groups WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "target group")
}

/// Every target group on a site, by name.
pub async fn list_groups(pool: &PgPool, site_id: Uuid) -> Result<Vec<TargetGroup>> {
    let rows = sqlx::query_as::<_, TargetGroup>(&format!(
        "SELECT {GROUP_COLS} FROM flag_target_groups WHERE site_id = $1 ORDER BY name ASC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Replace a group's name, description and conditions.
pub async fn update_group(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    description: &str,
    conditions: &Json,
) -> Result<TargetGroup> {
    let row = sqlx::query_as::<_, TargetGroup>(&format!(
        "UPDATE flag_target_groups
         SET name = $2, description = $3, conditions = $4, updated_at = now()
         WHERE id = $1 RETURNING {GROUP_COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(description.trim())
    .bind(conditions)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "target group")
}

/// Delete a group. The caller detaches it from its flags first, through the
/// flag write path, so each detachment is a recorded change.
pub async fn delete_group(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM flag_target_groups WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

/// One write to a flag, about to be recorded.
#[derive(Debug, Clone)]
pub struct NewChange<'a> {
    /// The flag.
    pub flag_id: Uuid,
    /// Its site.
    pub site_id: Uuid,
    /// `created` | `updated` | `toggled` | `reverted`.
    pub action: &'a str,
    /// Who.
    pub actor: &'a ChangeActor,
    /// The definition before, `None` for a creation.
    pub before: Option<&'a Json>,
    /// The definition after.
    pub after: Option<&'a Json>,
}

/// Record one write to a flag. Takes any executor, so it commits or rolls back
/// with the write it describes.
pub async fn record_change<'c>(
    ex: impl sqlx::PgExecutor<'c>,
    id: Uuid,
    change: &NewChange<'_>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO flag_changes
             (id, flag_id, site_id, action, actor_kind, actor_id, actor_label, before, after)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(change.flag_id)
    .bind(change.site_id)
    .bind(change.action)
    .bind(&change.actor.kind)
    .bind(change.actor.id)
    .bind(&change.actor.label)
    .bind(change.before)
    .bind(change.after)
    .execute(ex)
    .await?;
    Ok(())
}

/// A flag's changes, newest first.
pub async fn list_changes(
    pool: &PgPool,
    flag_id: Uuid,
    limit: Option<i64>,
) -> Result<Vec<FlagChange>> {
    let rows = sqlx::query_as::<_, FlagChange>(&format!(
        "SELECT {CHANGE_COLS} FROM flag_changes WHERE flag_id = $1 ORDER BY id DESC LIMIT $2"
    ))
    .bind(flag_id)
    .bind(clamp_limit(limit))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The latest change of every flag on a site, for the "changed by" column.
pub async fn latest_changes(pool: &PgPool, site_id: Uuid) -> Result<Vec<FlagChange>> {
    let rows = sqlx::query_as::<_, FlagChange>(&format!(
        "SELECT DISTINCT ON (flag_id) {CHANGE_COLS} FROM flag_changes
         WHERE site_id = $1 ORDER BY flag_id, id DESC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// One change.
pub async fn get_change(pool: &PgPool, id: Uuid) -> Result<FlagChange> {
    let row = sqlx::query_as::<_, FlagChange>(&format!(
        "SELECT {CHANGE_COLS} FROM flag_changes WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "flag change")
}
