//! `organizations`, `org_members`, `teams`, `team_members`. `docs/CONTRACT.md` §2.
//!
//! The ownership chain is: a user belongs to an organisation with a role, an
//! organisation owns sites, and a site may override the org role per user (see
//! [`crate::models::sites`]). Teams are a grouping *inside* an organisation and
//! carry no permissions of their own; they exist so an alert or a report can be
//! addressed to a group rather than to a list of addresses that goes stale.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// An organisation: the billing and ownership boundary.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Organization {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// URL-safe unique handle.
    pub slug: String,
    /// The user who owns it. Distinct from an `owner` role in `org_members`:
    /// this is the single account that cannot be removed.
    pub owner_id: Uuid,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
}

/// A user's membership of an organisation.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OrgMember {
    /// Organisation.
    pub org_id: Uuid,
    /// User.
    pub user_id: Uuid,
    /// `owner` | `admin` | `member` | `viewer`.
    pub role: String,
    /// When they joined.
    pub created_at: DateTime<Utc>,
}

/// A named group of users inside an organisation.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Team {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Owning organisation.
    pub org_id: Uuid,
    /// Display name.
    pub name: String,
}

/// Membership of a team.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TeamMember {
    /// Team.
    pub team_id: Uuid,
    /// User.
    pub user_id: Uuid,
}

const ORG_COLS: &str = "id, name, slug, owner_id, created_at";
const MEMBER_COLS: &str = "org_id, user_id, role, created_at";
const TEAM_COLS: &str = "id, org_id, name";

// ---------------------------------------------------------------------------
// Organisations
// ---------------------------------------------------------------------------

/// Create an organisation.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    slug: &str,
    owner_id: Uuid,
) -> Result<Organization> {
    let row = sqlx::query_as::<_, Organization>(&format!(
        "INSERT INTO organizations (id, name, slug, owner_id, created_at)
         VALUES ($1, $2, $3, $4, now())
         RETURNING {ORG_COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(slug.trim().to_lowercase())
    .bind(owner_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Organization> {
    let row = sqlx::query_as::<_, Organization>(&format!(
        "SELECT {ORG_COLS} FROM organizations WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "organization")
}

/// Fetch by URL handle.
pub async fn get_by_slug(pool: &PgPool, slug: &str) -> Result<Organization> {
    let row = sqlx::query_as::<_, Organization>(&format!(
        "SELECT {ORG_COLS} FROM organizations WHERE slug = $1"
    ))
    .bind(slug.trim().to_lowercase())
    .fetch_optional(pool)
    .await?;
    or_missing(row, "organization")
}

/// Every organisation a user is a member of, oldest first.
pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<Organization>> {
    let rows = sqlx::query_as::<_, Organization>(&format!(
        "SELECT {} FROM organizations o
         JOIN org_members m ON m.org_id = o.id
         WHERE m.user_id = $1
         ORDER BY o.created_at ASC",
        prefixed(ORG_COLS, "o")
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Rename an organisation, or move it to a new handle.
pub async fn update(pool: &PgPool, id: Uuid, name: &str, slug: &str) -> Result<Organization> {
    let row = sqlx::query_as::<_, Organization>(&format!(
        "UPDATE organizations SET name = $2, slug = $3 WHERE id = $1 RETURNING {ORG_COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .bind(slug.trim().to_lowercase())
    .fetch_optional(pool)
    .await?;
    or_missing(row, "organization")
}

/// Hand an organisation to a different owner.
pub async fn set_owner(pool: &PgPool, id: Uuid, owner_id: Uuid) -> Result<Organization> {
    let row = sqlx::query_as::<_, Organization>(&format!(
        "UPDATE organizations SET owner_id = $2 WHERE id = $1 RETURNING {ORG_COLS}"
    ))
    .bind(id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "organization")
}

/// Delete an organisation and, by cascade, everything under it.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

/// Add a user to an organisation, or change the role they already have.
///
/// An upsert rather than an insert: re-inviting an existing member is a role
/// change, and answering it with a unique-violation conflict would be a worse
/// answer than doing the obvious thing.
pub async fn add_member(
    pool: &PgPool,
    org_id: Uuid,
    user_id: Uuid,
    role: &str,
) -> Result<OrgMember> {
    let row = sqlx::query_as::<_, OrgMember>(&format!(
        "INSERT INTO org_members (org_id, user_id, role, created_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (org_id, user_id) DO UPDATE SET role = EXCLUDED.role
         RETURNING {MEMBER_COLS}"
    ))
    .bind(org_id)
    .bind(user_id)
    .bind(role)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// One membership, if it exists.
///
/// This is the authorisation lookup. It returns `Option` rather than a `404`
/// because "not a member" is the answer the caller is asking for, not a failure.
pub async fn get_member(pool: &PgPool, org_id: Uuid, user_id: Uuid) -> Result<Option<OrgMember>> {
    let row = sqlx::query_as::<_, OrgMember>(&format!(
        "SELECT {MEMBER_COLS} FROM org_members WHERE org_id = $1 AND user_id = $2"
    ))
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Everyone in an organisation.
pub async fn list_members(pool: &PgPool, org_id: Uuid) -> Result<Vec<OrgMember>> {
    let rows = sqlx::query_as::<_, OrgMember>(&format!(
        "SELECT {MEMBER_COLS} FROM org_members WHERE org_id = $1 ORDER BY created_at ASC"
    ))
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Change a member's role.
pub async fn update_member_role(
    pool: &PgPool,
    org_id: Uuid,
    user_id: Uuid,
    role: &str,
) -> Result<OrgMember> {
    let row = sqlx::query_as::<_, OrgMember>(&format!(
        "UPDATE org_members SET role = $3 WHERE org_id = $1 AND user_id = $2 RETURNING {MEMBER_COLS}"
    ))
    .bind(org_id)
    .bind(user_id)
    .bind(role)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "org member")
}

/// Remove a member.
pub async fn remove_member(pool: &PgPool, org_id: Uuid, user_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM org_members WHERE org_id = $1 AND user_id = $2")
        .bind(org_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------

/// Create a team inside an organisation.
pub async fn create_team(pool: &PgPool, id: Uuid, org_id: Uuid, name: &str) -> Result<Team> {
    let row = sqlx::query_as::<_, Team>(&format!(
        "INSERT INTO teams (id, org_id, name) VALUES ($1, $2, $3) RETURNING {TEAM_COLS}"
    ))
    .bind(id)
    .bind(org_id)
    .bind(name.trim())
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch a team.
pub async fn get_team(pool: &PgPool, id: Uuid) -> Result<Team> {
    let row = sqlx::query_as::<_, Team>(&format!("SELECT {TEAM_COLS} FROM teams WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "team")
}

/// Every team in an organisation.
pub async fn list_teams(pool: &PgPool, org_id: Uuid) -> Result<Vec<Team>> {
    let rows = sqlx::query_as::<_, Team>(&format!(
        "SELECT {TEAM_COLS} FROM teams WHERE org_id = $1 ORDER BY name ASC"
    ))
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Rename a team.
pub async fn update_team(pool: &PgPool, id: Uuid, name: &str) -> Result<Team> {
    let row = sqlx::query_as::<_, Team>(&format!(
        "UPDATE teams SET name = $2 WHERE id = $1 RETURNING {TEAM_COLS}"
    ))
    .bind(id)
    .bind(name.trim())
    .fetch_optional(pool)
    .await?;
    or_missing(row, "team")
}

/// Delete a team. Its members keep their organisation membership.
pub async fn delete_team(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM teams WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Put a user in a team. Adding someone twice is not an error.
pub async fn add_team_member(pool: &PgPool, team_id: Uuid, user_id: Uuid) -> Result<()> {
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(team_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Everyone in a team.
pub async fn list_team_members(pool: &PgPool, team_id: Uuid) -> Result<Vec<TeamMember>> {
    let rows = sqlx::query_as::<_, TeamMember>(
        "SELECT team_id, user_id FROM team_members WHERE team_id = $1",
    )
    .bind(team_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every team a user belongs to, across one organisation.
pub async fn list_teams_for_user(pool: &PgPool, org_id: Uuid, user_id: Uuid) -> Result<Vec<Team>> {
    let rows = sqlx::query_as::<_, Team>(&format!(
        "SELECT {} FROM teams t
         JOIN team_members tm ON tm.team_id = t.id
         WHERE t.org_id = $1 AND tm.user_id = $2
         ORDER BY t.name ASC",
        prefixed(TEAM_COLS, "t")
    ))
    .bind(org_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Take a user out of a team.
pub async fn remove_team_member(pool: &PgPool, team_id: Uuid, user_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND user_id = $2")
        .bind(team_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Qualify a column list with a table alias, for the joined reads above.
///
/// A join brings two tables into scope and both have an `id`, so an unqualified
/// list would be ambiguous. Building the qualified form from the same constant
/// keeps one definition of what the columns are.
fn prefixed(cols: &str, alias: &str) -> String {
    cols.split(", ")
        .map(|c| format!("{alias}.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_lists_qualify_cleanly() {
        assert_eq!(prefixed("id, name", "o"), "o.id, o.name");
        assert_eq!(prefixed(TEAM_COLS, "t"), "t.id, t.org_id, t.name");
    }
}
