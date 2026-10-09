//! `feature_flags` and `experiments`. `docs/CONTRACT.md` §2.
//!
//! The two are separate tables because they answer different questions. A flag
//! is a runtime switch: is this on, for whom, at what percentage. An experiment
//! is a measurement: this hypothesis, these variants, this goal, this window.
//! A flag can exist without an experiment, and an experiment names the goal it
//! is judged against so the result cannot be chosen after the fact.
//!
//! Exposures are recorded on the event row (`flags`, `experiment`, `variant` in
//! `docs/CONTRACT.md` §1.1) and, counted by the SDKs, in the ClickHouse
//! `flag_events` table, not here. Nothing in this module is on the read path
//! of a report.
//!
//! A flag's target groups and its change history live beside it in
//! [`crate::models::flag_rules`].

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A runtime feature switch.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FeatureFlag {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// The key the SDK asks for.
    pub key: String,
    /// A readable name for the dashboard. Empty means "use the key".
    pub name: String,
    /// What the flag is for, in the operator's own words.
    pub description: String,
    /// Master switch. False means off regardless of rollout or conditions.
    pub enabled: bool,
    /// Percentage of matching visitors the flag is on for, 0-100.
    pub rollout_pct: i32,
    /// Named variants and their weights, for a multivariate flag.
    pub variants: Json,
    /// The flag's own condition document (`micaforge_core::flags`).
    pub conditions: Json,
    /// Ids of the target groups the flag reaches, as a JSON array of strings.
    pub target_groups: Json,
    /// Prerequisites: `[{ flag, enabled, variant }]`.
    pub dependencies: Json,
    /// Per-environment overrides: `{ name: { enabled?, rollout_pct? } }`.
    pub environments: Json,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Last change, of any kind.
    pub updated_at: DateTime<Utc>,
}

/// Everything about a flag that a person edits, as one value.
///
/// One struct rather than ten arguments, because create, update and revert
/// all write the same set and a forgotten field would be a silent reset.
#[derive(Debug, Clone, PartialEq)]
pub struct FlagFields {
    /// The key the SDK asks for.
    pub key: String,
    /// The readable name.
    pub name: String,
    /// What it is for.
    pub description: String,
    /// Master switch.
    pub enabled: bool,
    /// 0-100.
    pub rollout_pct: i32,
    /// `[{ key, weight }]`.
    pub variants: Json,
    /// The condition document, in group form.
    pub conditions: Json,
    /// Target group ids.
    pub target_groups: Json,
    /// `[{ flag, enabled, variant }]`.
    pub dependencies: Json,
    /// `{ name: { enabled?, rollout_pct? } }`.
    pub environments: Json,
}

impl FlagFields {
    /// The editable fields of a stored flag.
    pub fn of(flag: &FeatureFlag) -> FlagFields {
        FlagFields {
            key: flag.key.clone(),
            name: flag.name.clone(),
            description: flag.description.clone(),
            enabled: flag.enabled,
            rollout_pct: flag.rollout_pct,
            variants: flag.variants.clone(),
            conditions: flag.conditions.clone(),
            target_groups: flag.target_groups.clone(),
            dependencies: flag.dependencies.clone(),
            environments: flag.environments.clone(),
        }
    }
}

/// A running or finished A/B test.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Experiment {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// The key written to `experiment` on every exposed event.
    pub key: String,
    /// What the experiment is testing, in the operator's own words.
    pub hypothesis: String,
    /// Variant names and weights.
    pub variants: Json,
    /// The goal that decides the outcome. Named up front on purpose: choosing
    /// the metric after seeing the data is how experiments lie.
    pub goal_id: Option<Uuid>,
    /// `draft` | `running` | `paused` | `finished`.
    pub status: String,
    /// When it started collecting.
    pub started_at: Option<DateTime<Utc>>,
    /// When it stopped.
    pub ended_at: Option<DateTime<Utc>>,
}

const FLAG_COLS: &str = "id, site_id, key, name, description, enabled, rollout_pct, variants, \
     conditions, target_groups, dependencies, environments, created_at, updated_at";
const EXP_COLS: &str =
    "id, site_id, key, hypothesis, variants, goal_id, status, started_at, ended_at";

// ---------------------------------------------------------------------------
// Feature flags
// ---------------------------------------------------------------------------

/// Create a flag.
///
/// Takes any executor, so the route can write the flag and its first history
/// entry in one transaction.
pub async fn create_flag<'c>(
    ex: impl sqlx::PgExecutor<'c>,
    id: Uuid,
    site_id: Uuid,
    fields: &FlagFields,
) -> Result<FeatureFlag> {
    let row = sqlx::query_as::<_, FeatureFlag>(&format!(
        "INSERT INTO feature_flags
             (id, site_id, key, name, description, enabled, rollout_pct, variants, conditions,
              target_groups, dependencies, environments, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now(), now())
         RETURNING {FLAG_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(fields.key.trim())
    .bind(fields.name.trim())
    .bind(fields.description.trim())
    .bind(fields.enabled)
    .bind(fields.rollout_pct.clamp(0, 100))
    .bind(&fields.variants)
    .bind(&fields.conditions)
    .bind(&fields.target_groups)
    .bind(&fields.dependencies)
    .bind(&fields.environments)
    .fetch_one(ex)
    .await?;
    Ok(row)
}

/// Fetch a flag by primary key.
pub async fn get_flag(pool: &PgPool, id: Uuid) -> Result<FeatureFlag> {
    let row = sqlx::query_as::<_, FeatureFlag>(&format!(
        "SELECT {FLAG_COLS} FROM feature_flags WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "feature flag")
}

/// Fetch a flag by the key the SDK sends.
pub async fn get_flag_by_key(
    pool: &PgPool,
    site_id: Uuid,
    key: &str,
) -> Result<Option<FeatureFlag>> {
    let row = sqlx::query_as::<_, FeatureFlag>(&format!(
        "SELECT {FLAG_COLS} FROM feature_flags WHERE site_id = $1 AND key = $2"
    ))
    .bind(site_id)
    .bind(key.trim())
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Every flag on a site.
///
/// The SDK's bootstrap call reads this whole list in one go, so it is ordered by
/// key and not paginated: a site with enough flags to need a page has a bigger
/// problem than pagination.
pub async fn list_flags(pool: &PgPool, site_id: Uuid) -> Result<Vec<FeatureFlag>> {
    let rows = sqlx::query_as::<_, FeatureFlag>(&format!(
        "SELECT {FLAG_COLS} FROM feature_flags WHERE site_id = $1 ORDER BY key ASC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The flags on a site that name `key` as a prerequisite.
///
/// jsonb containment: `[{"flag":"x","enabled":true}] @> [{"flag":"x"}]` holds,
/// so this finds a dependency whatever state it asks the prerequisite for.
pub async fn list_dependents(pool: &PgPool, site_id: Uuid, key: &str) -> Result<Vec<FeatureFlag>> {
    let rows = sqlx::query_as::<_, FeatureFlag>(&format!(
        "SELECT {FLAG_COLS} FROM feature_flags
         WHERE site_id = $1
           AND dependencies @> jsonb_build_array(jsonb_build_object('flag', $2::text))
         ORDER BY key ASC"
    ))
    .bind(site_id)
    .bind(key.trim())
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The flags on a site that reach one target group.
pub async fn list_with_group(
    pool: &PgPool,
    site_id: Uuid,
    group_id: Uuid,
) -> Result<Vec<FeatureFlag>> {
    let rows = sqlx::query_as::<_, FeatureFlag>(&format!(
        "SELECT {FLAG_COLS} FROM feature_flags
         WHERE site_id = $1 AND target_groups @> jsonb_build_array($2::text)
         ORDER BY key ASC"
    ))
    .bind(site_id)
    .bind(group_id.to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Replace a flag's editable fields.
pub async fn update_flag<'c>(
    ex: impl sqlx::PgExecutor<'c>,
    id: Uuid,
    fields: &FlagFields,
) -> Result<FeatureFlag> {
    let row = sqlx::query_as::<_, FeatureFlag>(&format!(
        "UPDATE feature_flags
         SET key = $2, name = $3, description = $4, enabled = $5, rollout_pct = $6,
             variants = $7, conditions = $8, target_groups = $9, dependencies = $10,
             environments = $11, updated_at = now()
         WHERE id = $1 RETURNING {FLAG_COLS}"
    ))
    .bind(id)
    .bind(fields.key.trim())
    .bind(fields.name.trim())
    .bind(fields.description.trim())
    .bind(fields.enabled)
    .bind(fields.rollout_pct.clamp(0, 100))
    .bind(&fields.variants)
    .bind(&fields.conditions)
    .bind(&fields.target_groups)
    .bind(&fields.dependencies)
    .bind(&fields.environments)
    .fetch_optional(ex)
    .await?;
    or_missing(row, "feature flag")
}

/// Flip a flag without touching its rollout or conditions.
pub async fn set_flag_enabled<'c>(
    ex: impl sqlx::PgExecutor<'c>,
    id: Uuid,
    enabled: bool,
) -> Result<FeatureFlag> {
    let row = sqlx::query_as::<_, FeatureFlag>(&format!(
        "UPDATE feature_flags SET enabled = $2, updated_at = now()
         WHERE id = $1 RETURNING {FLAG_COLS}"
    ))
    .bind(id)
    .bind(enabled)
    .fetch_optional(ex)
    .await?;
    or_missing(row, "feature flag")
}

/// Delete a flag. Its change history goes with it; the audit log keeps the
/// record of the deletion.
pub async fn delete_flag(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM feature_flags WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Experiments
// ---------------------------------------------------------------------------

/// Create an experiment. It starts in whatever status the caller names, which is
/// `draft` for anything created from the dashboard.
#[allow(clippy::too_many_arguments)]
pub async fn create_experiment(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    key: &str,
    hypothesis: &str,
    variants: &Json,
    goal_id: Option<Uuid>,
    status: &str,
) -> Result<Experiment> {
    let row = sqlx::query_as::<_, Experiment>(&format!(
        "INSERT INTO experiments (id, site_id, key, hypothesis, variants, goal_id, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {EXP_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(key.trim())
    .bind(hypothesis.trim())
    .bind(variants)
    .bind(goal_id)
    .bind(status)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch an experiment by primary key.
pub async fn get_experiment(pool: &PgPool, id: Uuid) -> Result<Experiment> {
    let row = sqlx::query_as::<_, Experiment>(&format!(
        "SELECT {EXP_COLS} FROM experiments WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "experiment")
}

/// Every experiment on a site, newest first.
pub async fn list_experiments(pool: &PgPool, site_id: Uuid) -> Result<Vec<Experiment>> {
    let rows = sqlx::query_as::<_, Experiment>(&format!(
        "SELECT {EXP_COLS} FROM experiments WHERE site_id = $1 ORDER BY id DESC"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update an experiment's definition.
pub async fn update_experiment(
    pool: &PgPool,
    id: Uuid,
    key: &str,
    hypothesis: &str,
    variants: &Json,
    goal_id: Option<Uuid>,
) -> Result<Experiment> {
    let row = sqlx::query_as::<_, Experiment>(&format!(
        "UPDATE experiments SET key = $2, hypothesis = $3, variants = $4, goal_id = $5
         WHERE id = $1 RETURNING {EXP_COLS}"
    ))
    .bind(id)
    .bind(key.trim())
    .bind(hypothesis.trim())
    .bind(variants)
    .bind(goal_id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "experiment")
}

/// Move an experiment through its lifecycle.
///
/// The timestamps are set from the status rather than passed in, and `started_at`
/// is only ever written once. An experiment whose start moved would have a
/// window that no longer matches the data collected in it.
pub async fn set_experiment_status(pool: &PgPool, id: Uuid, status: &str) -> Result<Experiment> {
    let row = sqlx::query_as::<_, Experiment>(&format!(
        "UPDATE experiments SET
            status = $2,
            started_at = CASE WHEN $2 = 'running' AND started_at IS NULL THEN now() ELSE started_at END,
            ended_at   = CASE WHEN $2 = 'finished' THEN now() ELSE ended_at END
         WHERE id = $1 RETURNING {EXP_COLS}"
    ))
    .bind(id)
    .bind(status)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "experiment")
}

/// Delete an experiment. Its exposures stay on the event rows.
pub async fn delete_experiment(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM experiments WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
