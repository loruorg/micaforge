//! The operations the settings screens offer that span more than one store:
//! moving a site to another organisation, deleting a site's data, rotating its
//! visitor salt, per-member and per-site access, sending an invitation again,
//! and the state of an error group.
//!
//! Each of them is an admin action (the error state aside, which a `member`
//! may set), each refuses an API key, and each is written to the audit log.
//!
//! ## Reset deletes rows, not the site
//!
//! `POST /api/sites/{site}/reset` issues one `ALTER TABLE ... DELETE WHERE
//! site_id = N` per ClickHouse table that has a `site_id` column, taken from
//! the schema's own table list so a table added later is covered without
//! anybody remembering to add it here. ClickHouse runs these as background
//! mutations over the parts that exist when they are issued: rows written after
//! the reset are never touched, and the old rows disappear from reports as the
//! mutation finishes, usually within seconds. The site row, its number, its
//! ingest key and every goal, alert and report stay.

use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::Utc;
use micaforge_core::{Error, Result};
use micaforge_store::ch::TABLES;
use micaforge_store::models::{error_states, orgs, site_ops, sites};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit::{self, Entry};
use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::response::ok;
use crate::routes::orgs::{
    authorize_org, is_owner, parse_role, require_can_grant, require_outranks, target_standing,
    ADMIN,
};
use crate::routes::sites::{authorize_site_ref, effective_site_role, site_json};
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Every route in this module. Two of them share a path with
/// `routes::orgs` (`/api/sites/{site}/access/{user}` takes `DELETE` there and
/// `PATCH` here), which axum merges by method.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/sites/{site}/transfer", post(transfer))
        .route("/api/sites/{site}/reset", post(reset))
        .route("/api/sites/{site}/rotate-salt", post(rotate_salt))
        .route("/api/sites/{site}/access/{user}", patch(change_access))
        .route("/api/orgs/{org}/members/{user}/access", get(member_access))
        .route(
            "/api/orgs/{org}/invitations/{invite}/resend",
            post(resend_invite),
        )
}

// ---------------------------------------------------------------------------
// Transfer
// ---------------------------------------------------------------------------

/// Where a site is going.
#[derive(Debug, Deserialize)]
struct TransferBody {
    org_id: Uuid,
}

/// `POST /api/sites/{site}/transfer` — move a site into another organisation.
///
/// The caller must be an `admin` of both organisations: moving a site is
/// giving it away and taking it in, and each half needs its own authority.
/// The receiving organisation's site limit applies, as it would to a new site.
/// Per-site overrides are dropped (they name members of the old organisation)
/// and API keys the old organisation bound to this site are revoked (a key
/// belongs to one organisation, so they could reach nothing afterwards).
async fn transfer(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
    Json(body): Json<TransferBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let (from, _) = authorize_org(&st.pg, site.org_id, auth.user.id, ADMIN).await?;
    if body.org_id == site.org_id {
        return Err(Error::validation("org_id", "the site is already in that organisation").into());
    }
    let (to, _) = match authorize_org(&st.pg, body.org_id, auth.user.id, ADMIN).await {
        Ok(found) => found,
        Err(Error::NotFound(_)) | Err(Error::Forbidden(_)) => {
            return Err(Error::validation(
                "org_id",
                "you can only move a site into an organisation you administer",
            )
            .into())
        }
        Err(e) => return Err(e.into()),
    };

    let cap = crate::routes::billing::site_cap(&st, to.id).await?;
    if cap > 0
        && sites::list_for_org(&st.pg, to.id).await?.len() >= cap as usize
        && !crate::routes::admin::is_instance_admin(&st, &auth.user).await?
    {
        return Err(Error::Forbidden(format!(
            "{} already has {cap} sites, the most it may hold",
            to.name
        ))
        .into());
    }

    let done = site_ops::transfer_site(&st.pg, site.id, from.id, to.id).await?;
    let moved = sites::get(&st.pg, site.id).await?;

    let summary = json!({
        "from": { "id": from.id, "name": from.name },
        "to": { "id": to.id, "name": to.name },
        "access_overrides_dropped": done.access_dropped,
        "keys_revoked": done.keys_revoked,
    });
    // Recorded in both logs: each organisation's admins should see the site
    // leave or arrive.
    for org_id in [from.id, to.id] {
        audit::record(
            &st,
            &auth,
            Entry::new("site.transferred")
                .site(&moved)
                .org(org_id)
                .before(json!({ "org_id": from.id }))
                .after(json!({ "org_id": to.id }))
                .meta(summary.clone()),
        )
        .await;
    }

    let role = effective_site_role(&st.pg, &moved, auth.user.id).await?;
    Ok(ok(json!({
        "site": site_json(&moved, Some(&role)),
        "keys_revoked": done.keys_revoked,
        "access_overrides_dropped": done.access_dropped,
    })))
}

// ---------------------------------------------------------------------------
// Reset
// ---------------------------------------------------------------------------

/// `POST /api/sites/{site}/reset` — delete every event row of a site, and keep
/// the site. See the module docs for how the deletion runs.
async fn reset(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let numeric = site.site_id_u16();

    let tables = delete_site_rows(&st.ch, numeric).await?;
    // The state of an error group means nothing once its errors are gone.
    error_states::clear_site(&st.pg, site.id).await?;

    tracing::info!(site = %site.id, actor = %auth.user.id, "site data reset");
    audit::record(
        &st,
        &auth,
        Entry::new("site.reset")
            .site(&site)
            .meta(json!({ "tables": tables })),
    )
    .await;
    Ok(ok(json!({
        "site_id": numeric,
        "tables": tables,
        "status": "deleting",
    })))
}

/// Issue the deletion of one site's rows in every table that keys by site,
/// and name the tables. The site id is a number, formatted into the statement
/// because ClickHouse binds no values in an `ALTER`; nothing a caller typed
/// reaches it.
async fn delete_site_rows(ch: &micaforge_store::Ch, site: u16) -> Result<Vec<&'static str>> {
    let mut tables = Vec::new();
    for table in reset_tables() {
        let sql = format!(
            "ALTER TABLE {}.{table} DELETE WHERE site_id = {site}",
            ch.db
        );
        ch.exec(&sql).await?;
        tables.push(table);
    }
    Ok(tables)
}

/// Every ClickHouse table that keys rows by site.
fn reset_tables() -> Vec<&'static str> {
    TABLES
        .iter()
        .filter(|t| t.columns.iter().any(|(name, _)| *name == "site_id"))
        .map(|t| t.name)
        .collect()
}

// ---------------------------------------------------------------------------
// Salt
// ---------------------------------------------------------------------------

/// `POST /api/sites/{site}/rotate-salt` — start a new visitor salt now.
///
/// Every hit after this one hashes to a new visitor id, as if the site's day
/// had turned over: today's visitors count again, and nothing before the
/// rotation can be joined to anything after it. A process that is riding out
/// a Valkey outage on its in-memory salt keeps that salt until Valkey answers
/// again.
async fn rotate_salt(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let tz: chrono_tz::Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    let today = Utc::now().with_timezone(&tz).date_naive();

    st.kv.rotate_salt(site.site_id_u16(), today).await?;
    sites::touch_salt_rotated(&st.pg, site.id).await?;
    let updated = sites::get(&st.pg, site.id).await?;

    audit::record(&st, &auth, Entry::new("site.salt_rotated").site(&updated)).await;
    let role = effective_site_role(&st.pg, &updated, auth.user.id).await?;
    Ok(ok(site_json(&updated, Some(&role))))
}

// ---------------------------------------------------------------------------
// Access
// ---------------------------------------------------------------------------

/// `GET /api/orgs/{org}/members/{user}/access` — one member's per-site
/// overrides across the organisation's sites.
async fn member_access(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, ADMIN).await?;
    let rows = site_ops::access_of_member(&st.pg, org.id, user_id).await?;
    Ok(ok(rows
        .iter()
        .map(|r| {
            json!({
                "site_id": r.site_id,
                "site": r.site_number,
                "domain": r.domain,
                "name": r.name,
                "role": r.role,
            })
        })
        .collect::<Vec<_>>()))
}

/// The new per-site role.
#[derive(Debug, Deserialize)]
struct AccessBody {
    role: String,
}

/// `PATCH /api/sites/{site}/access/{user}` — set somebody's role on one site.
///
/// The same rules as granting one (`POST /api/sites/{site}/access`): the
/// target must be a member of the site's organisation, stand below the caller
/// and not be the owner, and the caller cannot hand out a role above their own.
async fn change_access(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((site_ref, user_id)): Path<(String, Uuid)>,
    Json(body): Json<AccessBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let mine = effective_site_role(&st.pg, &site, auth.user.id).await?;
    let role = parse_role(&body.role)?;
    require_can_grant(&mine, role)?;

    let Some(member) = orgs::get_member(&st.pg, site.org_id, user_id).await? else {
        return Err(Error::not_found("member").into());
    };
    let previous = sites::get_access(&st.pg, site.id, user_id)
        .await?
        .map(|a| a.role);
    let standing = target_standing(&st, &site, user_id, Some(member.role)).await?;
    require_outranks(
        &mine,
        standing.as_deref(),
        is_owner(&st, &site, user_id).await?,
    )?;

    let granted = sites::grant_access(&st.pg, site.id, user_id, role).await?;
    audit::record(
        &st,
        &auth,
        Entry::new("site.access_changed")
            .site(&site)
            .target("member", user_id, &user_id.to_string())
            .before(json!({ "role": previous }))
            .after(json!({ "role": granted.role })),
    )
    .await;
    Ok(ok(json!({
        "site_id": site.id,
        "user_id": granted.user_id,
        "role": granted.role,
    })))
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

/// `POST /api/orgs/{org}/invitations/{invite}/resend` — send an invitation
/// again, with a new link. The old link stops working.
async fn resend_invite(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((org_id, invite_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, ADMIN).await?;
    let reissued = crate::routes::orgs::reissue_invite(&st, org.id, invite_id).await?;

    // A relay that refuses the message must not lose the invitation: the link
    // is in the response and the admin can hand it over.
    if let Err(e) = st
        .mailer
        .send_invitation(&reissued.email, &org.name, &reissued.token)
        .await
    {
        tracing::warn!(org = %org.id, error = %e, "invitation email could not be sent again");
    }

    audit::record(
        &st,
        &auth,
        Entry::new("invite.resent")
            .org(org.id)
            .target("invitation", invite_id, &reissued.email),
    )
    .await;

    let mut payload = reissued.invite;
    payload["link"] = json!(micaforge_mail::templates::invitation_link(
        st.mailer.base_url(),
        &reissued.token
    ));
    payload["emailed"] = json!(st.mailer.is_sending());
    Ok(ok(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_reaches_every_table_that_keys_by_site() {
        let tables = reset_tables();
        for name in [
            "events",
            "agent_events",
            "bot_events",
            "errors",
            "flag_events",
        ] {
            assert!(tables.contains(&name), "{name} must be reset");
        }
        assert_eq!(
            tables.len(),
            TABLES
                .iter()
                .filter(|t| t.columns.iter().any(|(n, _)| *n == "site_id"))
                .count()
        );
    }

    /// The reset against a real ClickHouse: one site's rows go from every
    /// table, another site's stay. See `routes::flags::tests::live_clickhouse`
    /// for how to opt in.
    #[tokio::test]
    #[ignore = "needs a ClickHouse server"]
    async fn a_reset_deletes_one_sites_rows_and_no_other() {
        use crate::routes::flags::tests::{drop_clickhouse, live_clickhouse};
        use micaforge_store::ch::EventRow;

        let ch = live_clickhouse().await;
        let mut insert = ch.client().insert::<EventRow>("events").unwrap();
        for site_id in [5u16, 5, 6] {
            insert
                .write(&EventRow {
                    site_id,
                    timestamp: Utc::now(),
                    ..EventRow::default()
                })
                .await
                .unwrap();
        }
        insert.end().await.unwrap();

        let tables = delete_site_rows(&ch, 5).await.unwrap();
        assert!(tables.contains(&"events"));

        // The mutation runs in the background; give it a few seconds.
        let count = |site: u16| {
            let ch = ch.clone();
            async move {
                ch.client()
                    .query("SELECT count() FROM events WHERE site_id = ?")
                    .bind(site)
                    .fetch_one::<u64>()
                    .await
                    .unwrap()
            }
        };
        let mut left = count(5).await;
        for _ in 0..50 {
            if left == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            left = count(5).await;
        }
        assert_eq!(left, 0, "the reset site's rows are gone");
        assert_eq!(count(6).await, 1, "the other site keeps its rows");
        drop_clickhouse(&ch).await;
    }
}
