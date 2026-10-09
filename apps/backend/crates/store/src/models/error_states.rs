//! `error_states` — what a person decided about an error group.
//!
//! The groups themselves are ClickHouse aggregates over append-only rows. The
//! decision "resolved" or "ignored" is the one mutable fact about a group, so it
//! lives here, one row per marked `(site, fingerprint)`. No row means open.
//! Migration `0010_error_states.sql` explains the table.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::pg::PgPool;

/// A marked group.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ErrorState {
    /// The group's fingerprint, as `errors.fingerprint` holds it.
    pub fingerprint: String,
    /// `resolved` | `ignored`.
    pub status: String,
    /// When it was marked.
    pub resolved_at: DateTime<Utc>,
}

/// Every marked group on a site, by fingerprint.
///
/// A site marks tens of groups, not thousands, so the whole map is read once per
/// error list rather than one row per listed group.
pub async fn for_site(pool: &PgPool, site_id: Uuid) -> Result<HashMap<String, ErrorState>> {
    let rows = sqlx::query_as::<_, ErrorState>(
        "SELECT fingerprint, status, resolved_at FROM error_states WHERE site_id = $1",
    )
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.fingerprint.clone(), row))
        .collect())
}

/// Mark a group `resolved` or `ignored`, replacing any earlier mark, and
/// restart its clock: a group resolved again is resolved as of now.
pub async fn set(
    pool: &PgPool,
    site_id: Uuid,
    fingerprint: &str,
    status: &str,
    by: Uuid,
) -> Result<ErrorState> {
    let row = sqlx::query_as::<_, ErrorState>(
        "INSERT INTO error_states (site_id, fingerprint, status, updated_by, resolved_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (site_id, fingerprint)
         DO UPDATE SET status = EXCLUDED.status,
                       updated_by = EXCLUDED.updated_by,
                       resolved_at = EXCLUDED.resolved_at
         RETURNING fingerprint, status, resolved_at",
    )
    .bind(site_id)
    .bind(fingerprint)
    .bind(status)
    .bind(by)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Reopen a group: remove its mark.
pub async fn clear(pool: &PgPool, site_id: Uuid, fingerprint: &str) -> Result<()> {
    sqlx::query("DELETE FROM error_states WHERE site_id = $1 AND fingerprint = $2")
        .bind(site_id)
        .bind(fingerprint)
        .execute(pool)
        .await?;
    Ok(())
}

/// Remove every mark of a site, for "reset site data": the error groups are
/// gone with the rows, so a mark left behind would resolve a future error
/// nobody has seen.
pub async fn clear_site(pool: &PgPool, site_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM error_states WHERE site_id = $1")
        .bind(site_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The state a group reads as on the screen, given its mark and its latest
/// occurrence.
///
/// A resolved group that fired after it was resolved reads `open` again: the
/// fix did not hold, and that is the one thing the register must not hide. An
/// ignored group stays ignored whatever it does; ignoring is the decision not to
/// be told.
pub fn effective(mark: Option<&ErrorState>, last_seen: DateTime<Utc>) -> &'static str {
    match mark {
        Some(m) if m.status == "ignored" => "ignored",
        Some(m) if m.status == "resolved" && last_seen <= m.resolved_at => "resolved",
        _ => "open",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn a_fix_that_did_not_hold_reads_open_again() {
        let at = Utc::now();
        let mark = |status: &str| ErrorState {
            fingerprint: "abc".into(),
            status: status.into(),
            resolved_at: at,
        };
        assert_eq!(effective(None, at), "open");
        assert_eq!(
            effective(Some(&mark("resolved")), at - Duration::hours(1)),
            "resolved"
        );
        assert_eq!(
            effective(Some(&mark("resolved")), at + Duration::minutes(5)),
            "open"
        );
        assert_eq!(
            effective(Some(&mark("ignored")), at + Duration::days(3)),
            "ignored"
        );
    }
}
