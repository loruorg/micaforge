//! The writes behind the danger-zone and access screens that touch more than
//! one table: moving a site between organisations, and reading one member's
//! per-site overrides across an organisation.
//!
//! Kept apart from [`crate::models::sites`] because each of these spans
//! `sites`, `site_access` and `api_keys` at once, and the transfer has to be
//! one transaction: a site that moved but kept the old organisation's
//! site-bound keys and overrides would be reachable by people who no longer
//! have any claim on it.

use micaforge_core::Result;
use uuid::Uuid;

use crate::pg::PgPool;

/// What a transfer did besides moving the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transferred {
    /// Per-site role overrides dropped: they named members of the old
    /// organisation.
    pub access_dropped: u64,
    /// API keys bound to this site and minted by the old organisation. A key
    /// belongs to one organisation, so after the move it could reach nothing;
    /// it is revoked rather than left as a credential that names nothing.
    pub keys_revoked: u64,
}

/// One per-site override, with the site it is on.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemberSiteAccess {
    /// The site's UUID.
    pub site_id: Uuid,
    /// The site's numeric id.
    pub site_number: i32,
    /// The site's domain.
    pub domain: String,
    /// The site's name.
    pub name: String,
    /// The override.
    pub role: String,
}

/// Move a site into another organisation, in one transaction.
pub async fn transfer_site(
    pool: &PgPool,
    site_id: Uuid,
    from_org: Uuid,
    to_org: Uuid,
) -> Result<Transferred> {
    let mut tx = pool.begin().await?;

    let access = sqlx::query("DELETE FROM site_access WHERE site_id = $1")
        .bind(site_id)
        .execute(&mut *tx)
        .await?;
    let keys = sqlx::query("DELETE FROM api_keys WHERE site_id = $1 AND org_id = $2")
        .bind(site_id)
        .bind(from_org)
        .execute(&mut *tx)
        .await?;
    let moved = sqlx::query("UPDATE sites SET org_id = $3 WHERE id = $1 AND org_id = $2")
        .bind(site_id)
        .bind(from_org)
        .bind(to_org)
        .execute(&mut *tx)
        .await?;
    if moved.rows_affected() != 1 {
        // The site moved, or was deleted, between the check and now. Nothing
        // is committed, and the caller reports the site as missing.
        return Err(micaforge_core::Error::not_found("site"));
    }

    tx.commit().await?;
    Ok(Transferred {
        access_dropped: access.rows_affected(),
        keys_revoked: keys.rows_affected(),
    })
}

/// One member's per-site overrides on the sites of one organisation.
pub async fn access_of_member(
    pool: &PgPool,
    org_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<MemberSiteAccess>> {
    let rows = sqlx::query_as::<_, MemberSiteAccess>(
        "SELECT s.id AS site_id, s.site_id AS site_number, s.domain, s.name, a.role
         FROM site_access a JOIN sites s ON s.id = a.site_id
         WHERE s.org_id = $1 AND a.user_id = $2
         ORDER BY s.domain ASC",
    )
    .bind(org_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
