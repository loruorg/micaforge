//! `/api/orgs/*`, `/api/teams/*`, `/api/invitations/*` and per-site access.
//!
//! This module owns the **role ladder**, and every other management route asks
//! it the same question: may this caller do this to this object?
//!
//! ```text
//! owner (3) > admin (2) > member (1) > viewer (0)
//! ```
//!
//! Two rules hold everywhere and are enforced here rather than repeated:
//!
//! 1. **A caller can never grant a role above their own.** Without that, an
//!    `admin` could promote themselves to `owner` through a second account and
//!    the ladder would be decoration.
//! 2. **An object the caller cannot see reports `404`, never `403`.** A `403`
//!    on an organisation id is a confirmation that the id exists, which turns
//!    every list endpoint into an enumeration oracle.
//!
//! ## Invitations live in Redis, and that is a deliberate compromise
//!
//! `docs/CONTRACT.md` §2 has no `invitations` table, and inventing one here
//! would be a schema deviation this module is not allowed to make. So a pending
//! invitation is a Redis entry with a seven-day TTL, keyed by the digest of the
//! token rather than the token, so a Redis dump is not a set of live invites.
//! The cost is honest and bounded: a Redis flush loses *pending* invitations and
//! nothing else, because acceptance writes a real `org_members` row. The
//! operator re-sends. If the contract later grows the table, the functions in
//! the invitations section below are the ones that move.
//!
//! ## An invitation is also a door
//!
//! Accounts have no password: a person signs in with a code sent to their
//! address (`routes::auth`). On an install with registration closed, an
//! address that has no account may still be sent a code when an invitation is
//! waiting for it, so the person invited can create the account the invitation
//! is for. [`pending_invites_for`] answers that question, from a third Redis
//! entry that lists the invitations sent to one address, keyed by a digest of
//! the address rather than the address.

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use micaforge_core::{crypto, new_id, Config, Error, Result};
use micaforge_store::models::{orgs, sites, users};
use micaforge_store::pg::PgPool;
use micaforge_store::Kv;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::response::ok;
use crate::routes::auth::address_digest;
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The organisation's single owner. Can do everything, including delete.
pub const OWNER: &str = "owner";
/// Manages members, sites, keys and configuration. Cannot delete the org.
pub const ADMIN: &str = "admin";
/// Edits configuration (goals, funnels, alerts) but not people or credentials.
pub const MEMBER: &str = "member";
/// Reads. Writes nothing, anywhere.
pub const VIEWER: &str = "viewer";

/// How long a pending invitation stays valid.
const INVITE_TTL_SECS: u32 = 7 * 24 * 60 * 60;

/// Longest accepted display name, for an organisation or a team.
const MAX_NAME: usize = 120;

// ---------------------------------------------------------------------------
// The role ladder
// ---------------------------------------------------------------------------

/// Rank a role string. `None` means the value is not a role at all.
///
/// Returning `Option` rather than defaulting an unknown string to `viewer` is
/// the point: a row carrying a role this build does not know is a row from a
/// newer release, and quietly granting it read access would be a guess made in
/// the wrong direction.
pub fn role_rank(role: &str) -> Option<u8> {
    match role.trim() {
        OWNER => Some(3),
        ADMIN => Some(2),
        MEMBER => Some(1),
        VIEWER => Some(0),
        _ => None,
    }
}

/// Refuse unless `actual` sits at or above `needed` on the ladder.
///
/// The message names both roles. An operator who cannot do something should be
/// told what would let them, not simply told no.
pub fn require_at_least(actual: &str, needed: &str) -> Result<()> {
    let have = role_rank(actual)
        .ok_or_else(|| Error::Forbidden(format!("`{actual}` is not a role this build knows")))?;
    // An unrecognised requirement is treated as the top of the ladder rather
    // than the bottom, so a typo in a call site fails closed.
    let want = role_rank(needed).unwrap_or(3);
    if have < want {
        return Err(Error::Forbidden(format!(
            "this action needs the `{needed}` role; you have `{actual}`"
        )));
    }
    Ok(())
}

/// Validate a role arriving from a request body.
///
/// Returns the canonical `&'static str` so what reaches Postgres is one of four
/// known values and never the caller's own spelling.
pub fn parse_role(raw: &str) -> Result<&'static str> {
    match raw.trim() {
        OWNER => Ok(OWNER),
        ADMIN => Ok(ADMIN),
        MEMBER => Ok(MEMBER),
        VIEWER => Ok(VIEWER),
        other => Err(Error::validation(
            "role",
            format!("`{other}` is not a role; expected owner, admin, member or viewer"),
        )),
    }
}

/// Refuse to hand out a role the granter does not themselves hold.
pub fn require_can_grant(granter: &str, granted: &str) -> Result<()> {
    let mine = role_rank(granter)
        .ok_or_else(|| Error::Forbidden(format!("`{granter}` is not a role this build knows")))?;
    let theirs = role_rank(granted).ok_or_else(|| Error::validation("role", "unknown role"))?;
    if theirs > mine {
        return Err(Error::Forbidden(format!(
            "you cannot grant `{granted}` while you are `{granter}`"
        )));
    }
    Ok(())
}

/// The caller's role in one organisation.
///
/// Reports `not_found` rather than `forbidden` when they are not a member, so
/// that iterating over organisation ids tells an attacker nothing.
///
/// This is where an API key is held to its grant at the organisation level
/// (see [`crate::extract`]): any organisation but the key's own reads as
/// absent, a site-bound key reaches no organisation-level object at all, and the
/// role is capped at what the key's scopes allow.
pub async fn effective_org_role(pg: &PgPool, org_id: Uuid, user_id: Uuid) -> Result<String> {
    let key = crate::extract::current_key();
    if let Some(grant) = key.as_ref() {
        if !grant.permits_org(org_id) {
            return Err(Error::not_found("organization"));
        }
    }

    match orgs::get_member(pg, org_id, user_id).await? {
        Some(member) => Ok(match key {
            Some(grant) => grant.cap_role(&member.role),
            None => member.role,
        }),
        None => Err(Error::not_found("organization")),
    }
}

/// Refuse to touch a member who stands at or above the caller.
///
/// For per-site grants and revocations. `target` is the target's standing in
/// the organisation: the higher of their organisation role and any per-site
/// override they already hold. The owner is always out of reach, and so is
/// the caller's own row, which is what stops an `admin` from pinning the owner
/// to `viewer` on a site, or from undoing an override the owner placed on them.
pub fn require_outranks(caller: &str, target: Option<&str>, target_is_owner: bool) -> Result<()> {
    if target_is_owner {
        return Err(Error::Forbidden(
            "the organisation's owner has no per-site role to change".into(),
        ));
    }
    let mine = role_rank(caller)
        .ok_or_else(|| Error::Forbidden(format!("`{caller}` is not a role this build knows")))?;
    // A target whose role this build does not know is treated as the top of the
    // ladder, so the check fails closed.
    let theirs = target.map(|role| role_rank(role).unwrap_or(3));
    if theirs.is_some_and(|rank| rank >= mine) {
        return Err(Error::Forbidden(
            "you can only change the access of members below your own role".into(),
        ));
    }
    Ok(())
}

/// Load an organisation and assert the caller holds at least `needed`.
pub async fn authorize_org(
    pg: &PgPool,
    org_id: Uuid,
    user_id: Uuid,
    needed: &str,
) -> Result<(orgs::Organization, String)> {
    let role = effective_org_role(pg, org_id, user_id).await?;
    require_at_least(&role, needed)?;
    let org = orgs::get(pg, org_id).await?;
    Ok((org, role))
}

// ---------------------------------------------------------------------------
// Slugs
// ---------------------------------------------------------------------------

/// Turn a display name into a URL handle.
///
/// Lower-case, ASCII alphanumerics and single hyphens. Anything that reduces to
/// nothing (a name written entirely in a non-Latin script, an emoji) falls back
/// to `org`, and [`unique_slug`] then makes it distinct.
pub fn slugify(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = true;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "org".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}

/// A slug nothing else is using.
///
/// Tries the bare form first so the common case is the readable one, then
/// appends random characters. Bounded rather than looping forever: after a few
/// attempts the contention is not a coincidence and the caller deserves an
/// error rather than a hung request.
pub async fn unique_slug(pg: &PgPool, base: &str) -> Result<String> {
    let base = slugify(base);
    for attempt in 0..6 {
        let candidate = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{}", crypto::random_token(4).to_lowercase())
        };
        match orgs::get_by_slug(pg, &candidate).await {
            Err(Error::NotFound(_)) => return Ok(candidate),
            Err(e) => return Err(e),
            Ok(_) => continue,
        }
    }
    Err(Error::Conflict(
        "could not find a free organisation handle".into(),
    ))
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every organisation, team, invitation and per-site access route.
///
/// Paths are absolute from the application root, so the caller merges this
/// router rather than nesting it under a prefix.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/orgs", get(list_orgs).post(create_org))
        .route(
            "/api/orgs/{org}",
            get(get_org).patch(update_org).delete(delete_org),
        )
        .route("/api/orgs/{org}/owner", post(transfer_owner))
        .route("/api/orgs/{org}/members", get(list_members))
        .route(
            "/api/orgs/{org}/members/{user}",
            axum::routing::patch(update_member).delete(remove_member),
        )
        .route(
            "/api/orgs/{org}/invitations",
            get(list_invites).post(create_invite),
        )
        .route(
            "/api/orgs/{org}/invitations/{invite}",
            delete(revoke_invite),
        )
        .route("/api/invitations/preview", post(preview_invite))
        .route("/api/invitations/accept", post(accept_invite))
        .route("/api/orgs/{org}/teams", get(list_teams).post(create_team))
        .route(
            "/api/teams/{team}",
            get(get_team).patch(update_team).delete(delete_team),
        )
        .route(
            "/api/teams/{team}/members",
            get(list_team_members).post(add_team_member),
        )
        .route(
            "/api/teams/{team}/members/{user}",
            delete(remove_team_member),
        )
        .route(
            "/api/sites/{site}/access",
            get(list_site_access).post(grant_site_access),
        )
        .route(
            "/api/sites/{site}/access/{user}",
            delete(revoke_site_access),
        )
}

// ---------------------------------------------------------------------------
// Organisations
// ---------------------------------------------------------------------------

/// `GET /api/orgs` — every organisation the caller belongs to.
async fn list_orgs(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    let rows = orgs::list_for_user(&st.pg, auth.user.id).await?;

    let mut out = Vec::with_capacity(rows.len());
    for org in rows {
        // A key lists its own organisation, not every one its owner is in.
        if auth
            .key
            .as_ref()
            .is_some_and(|grant| !grant.permits_org(org.id))
        {
            continue;
        }
        let role = effective_org_role(&st.pg, org.id, auth.user.id).await?;
        let sites = sites::list_for_org(&st.pg, org.id).await?;
        out.push(org_json(&org, Some(&role), Some(sites.len())));
    }
    Ok(ok(out))
}

/// What a new organisation needs.
#[derive(Debug, Deserialize)]
struct NewOrg {
    name: String,
    /// Optional handle. Derived from the name when absent, and made unique
    /// either way.
    #[serde(default)]
    slug: Option<String>,
}

/// `POST /api/orgs` — create one, with the caller as its owner.
async fn create_org(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<NewOrg>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let name = clean_name(&body.name, "name")?;

    // The per-account cap a shared install sets (`MICAFORGE_MAX_ORGS_PER_USER`).
    let cap = st.cfg.max_orgs_per_user;
    if cap > 0 {
        let owned = orgs::list_for_user(&st.pg, auth.user.id)
            .await?
            .into_iter()
            .filter(|o| o.owner_id == auth.user.id)
            .count();
        if owned >= cap as usize
            && !crate::routes::admin::is_instance_admin(&st, &auth.user).await?
        {
            return Err(Error::Forbidden(format!(
                "this account already owns {cap} organisations, the most this install allows"
            ))
            .into());
        }
    }

    let slug = unique_slug(&st.pg, body.slug.as_deref().unwrap_or(&name)).await?;

    let org = orgs::create(&st.pg, new_id(), &name, &slug, auth.user.id).await?;
    orgs::add_member(&st.pg, org.id, auth.user.id, OWNER).await?;

    Ok(ok(org_json(&org, Some(OWNER), Some(0))))
}

/// `GET /api/orgs/{org}` — one organisation, with the caller's own role on it.
async fn get_org(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (org, role) = authorize_org(&st.pg, org_id, auth.user.id, VIEWER).await?;
    let sites = sites::list_for_org(&st.pg, org.id).await?;
    Ok(ok(org_json(&org, Some(&role), Some(sites.len()))))
}

/// Editable organisation fields.
#[derive(Debug, Deserialize)]
struct OrgPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
}

/// `PATCH /api/orgs/{org}` — rename, or move to a new handle.
async fn update_org(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
    Json(body): Json<OrgPatch>,
) -> ApiResult<Json<Value>> {
    let (org, role) = authorize_org(&st.pg, org_id, auth.user.id, ADMIN).await?;

    let name = match body.name.as_deref() {
        Some(raw) => clean_name(raw, "name")?,
        None => org.name.clone(),
    };
    // A handle change is not automatic on a rename: existing links carry the
    // old slug, and silently breaking them would be a surprise.
    let slug = match body.slug.as_deref() {
        Some(raw) => {
            let candidate = slugify(raw);
            if candidate == org.slug {
                candidate
            } else {
                unique_slug(&st.pg, &candidate).await?
            }
        }
        None => org.slug.clone(),
    };

    let updated = orgs::update(&st.pg, org.id, &name, &slug).await?;
    Ok(ok(org_json(&updated, Some(&role), None)))
}

/// `DELETE /api/orgs/{org}` — remove it and everything under it.
async fn delete_org(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, OWNER).await?;
    orgs::delete(&st.pg, org.id).await?;
    Ok(ok(json!({ "deleted": org.id })))
}

/// Who the organisation is being handed to.
#[derive(Debug, Deserialize)]
struct TransferOwner {
    user_id: Uuid,
}

/// `POST /api/orgs/{org}/owner` — hand the organisation to another member.
///
/// The new owner has to already be a member. Transferring to a stranger would
/// hand somebody an organisation they never agreed to hold.
async fn transfer_owner(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
    Json(body): Json<TransferOwner>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, OWNER).await?;

    // Transferring to yourself is a no-op that must not fall through: the
    // demotion below would otherwise take the owner's own role down to `admin`
    // and leave the organisation with no owner in `org_members`.
    if body.user_id == auth.user.id {
        return Ok(ok(org_json(&org, Some(OWNER), None)));
    }

    if orgs::get_member(&st.pg, org.id, body.user_id)
        .await?
        .is_none()
    {
        return Err(Error::validation("user_id", "is not a member of this organisation").into());
    }

    let updated = orgs::set_owner(&st.pg, org.id, body.user_id).await?;
    orgs::add_member(&st.pg, org.id, body.user_id, OWNER).await?;
    // The previous owner keeps administrative rights but is no longer the one
    // account that cannot be removed.
    orgs::add_member(&st.pg, org.id, auth.user.id, ADMIN).await?;

    Ok(ok(org_json(&updated, Some(ADMIN), None)))
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

/// `GET /api/orgs/{org}/members` — everyone, with the identity behind each row.
async fn list_members(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, VIEWER).await?;
    let members = orgs::list_members(&st.pg, org.id).await?;

    let mut out = Vec::with_capacity(members.len());
    for member in members {
        // A membership whose user row is gone is skipped rather than rendered
        // as a blank: the join is not enforced by this query, and a half-row in
        // a members table reads as data loss.
        let Ok(user) = users::get(&st.pg, member.user_id).await else {
            continue;
        };
        out.push(json!({
            "user_id": member.user_id,
            "email": user.email,
            "name": user.name,
            "avatar_url": user.avatar_url,
            "role": member.role,
            "is_owner": org.owner_id == member.user_id,
            "joined_at": member.created_at,
        }));
    }
    Ok(ok(out))
}

/// The one field a membership has.
#[derive(Debug, Deserialize)]
struct MemberPatch {
    role: String,
}

/// `PATCH /api/orgs/{org}/members/{user}` — change somebody's role.
async fn update_member(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<MemberPatch>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, mine) = authorize_org(&st.pg, org_id, auth.user.id, ADMIN).await?;
    let role = parse_role(&body.role)?;
    require_can_grant(&mine, role)?;

    // The owner's row is set by the transfer endpoint, which also moves
    // `organizations.owner_id`. Editing it here would leave the two disagreeing
    // about who owns the organisation.
    if org.owner_id == user_id {
        return Err(Error::Conflict(
            "the owner's role changes by transferring the organisation".into(),
        )
        .into());
    }

    let updated = orgs::update_member_role(&st.pg, org.id, user_id, role).await?;
    Ok(ok(json!({
        "user_id": updated.user_id,
        "role": updated.role,
        "joined_at": updated.created_at,
    })))
}

/// `DELETE /api/orgs/{org}/members/{user}` — remove somebody.
///
/// Also drops their per-site overrides. Leaving those behind would keep a
/// removed member's access to a site alive through `site_access`, which is
/// exactly the kind of quiet leftover an offboarding is meant to close.
async fn remove_member(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    // Leaving on your own account needs no privilege; removing somebody else
    // does.
    let needed = if user_id == auth.user.id {
        VIEWER
    } else {
        ADMIN
    };
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, needed).await?;

    if org.owner_id == user_id {
        return Err(Error::Conflict(
            "the owner cannot be removed; transfer the organisation first".into(),
        )
        .into());
    }

    for site in sites::list_for_org(&st.pg, org.id).await? {
        sites::revoke_access(&st.pg, site.id, user_id).await?;
    }
    orgs::remove_member(&st.pg, org.id, user_id).await?;

    Ok(ok(json!({ "removed": user_id })))
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

/// A pending invitation, as it is held in Redis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Invite {
    /// Public identifier, used to revoke without knowing the token.
    id: Uuid,
    /// The organisation the invitation is into.
    pub(crate) org_id: Uuid,
    /// Lower-cased. Acceptance requires the signed-in address to match.
    email: String,
    role: String,
    invited_by: Uuid,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    /// SHA-256 of the token. The token itself is never stored.
    token_hash: String,
}

/// `mf:invite:tok:<digest>` — the token lookup.
fn invite_token_key(token_hash: &str) -> String {
    format!("mf:invite:tok:{token_hash}")
}

/// `mf:invite:org:<uuid>` — the list an admin sees.
fn invite_index_key(org_id: Uuid) -> String {
    format!("mf:invite:org:{org_id}")
}

/// `mf:invite:addr:<digest of the address>` — the token digests of every
/// invitation sent to one address, across organisations.
///
/// A hint and not the record: an entry counts only while the token lookup it
/// names still holds a live invitation for this address, so a revoked or
/// accepted invitation stops counting the moment its token key is deleted, and
/// nothing else has to remember to clean this list.
fn invite_address_key(cfg: &Config, email: &str) -> String {
    format!("mf:invite:addr:{}", address_digest(cfg, email))
}

/// Read one organisation's pending invitations, dropping any that have lapsed.
async fn index_invites(kv: &Kv, org_id: Uuid) -> Result<Vec<Invite>> {
    let now = Utc::now();
    let all: Vec<Invite> = kv
        .get_json(&invite_index_key(org_id))
        .await?
        .unwrap_or_default();
    Ok(all.into_iter().filter(|i| i.expires_at > now).collect())
}

/// Write the index back, with the same TTL as the tokens it describes.
async fn put_index(kv: &Kv, org_id: Uuid, invites: &[Invite]) -> Result<()> {
    kv.set_json(&invite_index_key(org_id), &invites, Some(INVITE_TTL_SECS))
        .await
}

/// A token digest's invitation, if it is still live.
async fn live_invite(kv: &Kv, token_hash: &str) -> Result<Option<Invite>> {
    let invite: Option<Invite> = kv.get_json(&invite_token_key(token_hash)).await?;
    Ok(invite.filter(|i| i.expires_at > Utc::now()))
}

/// Every live invitation sent to `email`, oldest first.
///
/// What the sign-in code path asks before it sends a code to an address that
/// has no account on an install with registration closed, and what it redeems
/// once that account exists.
pub(crate) async fn pending_invites_for(kv: &Kv, cfg: &Config, email: &str) -> Result<Vec<Invite>> {
    let digests: Vec<String> = kv
        .get_json(&invite_address_key(cfg, email))
        .await?
        .unwrap_or_default();
    let mut live = Vec::new();
    for digest in digests {
        if let Some(invite) = live_invite(kv, &digest).await? {
            if invite.email.eq_ignore_ascii_case(email) {
                live.push(invite);
            }
        }
    }
    Ok(live)
}

/// Add `token_hash` to the address list, dropping entries that no longer
/// name a live invitation so the list stays as short as what it describes.
async fn remember_for_address(kv: &Kv, cfg: &Config, email: &str, token_hash: &str) -> Result<()> {
    let key = invite_address_key(cfg, email);
    let mut digests: Vec<String> = Vec::new();
    for invite in pending_invites_for(kv, cfg, email).await? {
        digests.push(invite.token_hash);
    }
    digests.push(token_hash.to_string());
    kv.set_json(&key, &digests, Some(INVITE_TTL_SECS)).await
}

/// Make `user_id` a member on the terms of `invite`, and use the invitation up.
///
/// The one place an invitation is redeemed: from the link, by a signed-in
/// account ([`accept_invite`]), and at the creation of the account the
/// invitation was for, by the sign-in code path. The caller has already
/// established that the account's address is the invited one.
pub(crate) async fn redeem_invite(
    pg: &PgPool,
    kv: &Kv,
    invite: &Invite,
    user_id: Uuid,
) -> Result<orgs::OrgMember> {
    let role = parse_role(&invite.role)?;
    let member = orgs::add_member(pg, invite.org_id, user_id, role).await?;

    kv.del(&invite_token_key(&invite.token_hash)).await?;
    let mut index = index_invites(kv, invite.org_id).await?;
    index.retain(|i| i.id != invite.id);
    put_index(kv, invite.org_id, &index).await?;
    Ok(member)
}

/// `GET /api/orgs/{org}/invitations` — what is outstanding.
async fn list_invites(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, ADMIN).await?;
    let invites = index_invites(&st.kv, org.id).await?;

    let out: Vec<Value> = invites.iter().map(invite_json).collect();
    Ok(ok(out))
}

/// Who to invite, and as what.
#[derive(Debug, Deserialize)]
struct NewInvite {
    email: String,
    #[serde(default)]
    role: Option<String>,
}

/// `POST /api/orgs/{org}/invitations` — invite an address.
///
/// The link is returned in the response as well as mailed. On an install with
/// no mail transport that link is the only way an invitation works at all, and
/// [`micaforge_mail::Mailer::is_sending`] is how the dashboard knows to show it.
///
/// Inviting an address that has no account is what lets it sign up by code on
/// an install whose registration is closed: see the module docs.
async fn create_invite(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
    Json(body): Json<NewInvite>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, mine) = authorize_org(&st.pg, org_id, auth.user.id, ADMIN).await?;
    let email = clean_email(&body.email)?;
    let role = parse_role(body.role.as_deref().unwrap_or(MEMBER))?;
    require_can_grant(&mine, role)?;

    // Somebody who is already in is not invited again; that would send a link
    // whose only effect is to silently change their role.
    if let Some(existing) = users::find_by_email(&st.pg, &email).await? {
        if orgs::get_member(&st.pg, org.id, existing.id)
            .await?
            .is_some()
        {
            return Err(Error::Conflict("that address is already a member".into()).into());
        }
    }

    let token = crypto::random_token(32);
    let invite = Invite {
        id: new_id(),
        org_id: org.id,
        email: email.clone(),
        role: role.to_string(),
        invited_by: auth.user.id,
        created_at: Utc::now(),
        expires_at: Utc::now() + Duration::seconds(i64::from(INVITE_TTL_SECS)),
        token_hash: crypto::token_hash(&token),
    };

    st.kv
        .set_json(
            &invite_token_key(&invite.token_hash),
            &invite,
            Some(INVITE_TTL_SECS),
        )
        .await?;

    // A re-invite replaces the earlier one outright. Dropping it from the index
    // alone would leave its token redeemable and no longer listed, so nobody
    // could see it, let alone revoke it.
    let mut index = index_invites(&st.kv, org.id).await?;
    for replaced in index.iter().filter(|i| i.email == email) {
        st.kv.del(&invite_token_key(&replaced.token_hash)).await?;
    }
    index.retain(|i| i.email != email);
    index.push(invite.clone());
    put_index(&st.kv, org.id, &index).await?;
    remember_for_address(&st.kv, &st.cfg, &email, &invite.token_hash).await?;

    // A relay that refuses the message must not lose the invitation: the link is
    // already in the response and the admin can hand it over.
    if let Err(e) = st.mailer.send_invitation(&email, &org.name, &token).await {
        tracing::warn!(org = %org.id, error = %e, "invitation email could not be sent");
    }

    let mut payload = invite_json(&invite);
    payload["link"] = json!(micaforge_mail::templates::invitation_link(
        st.mailer.base_url(),
        &token
    ));
    payload["emailed"] = json!(st.mailer.is_sending());
    Ok(ok(payload))
}

/// `DELETE /api/orgs/{org}/invitations/{invite}` — withdraw one.
async fn revoke_invite(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((org_id, invite_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, ADMIN).await?;

    let mut index = index_invites(&st.kv, org.id).await?;
    let Some(position) = index.iter().position(|i| i.id == invite_id) else {
        return Err(Error::not_found("invitation").into());
    };

    // Deleting the token key is what ends it everywhere, the address list
    // included: see `invite_address_key`.
    let invite = index.remove(position);
    st.kv.del(&invite_token_key(&invite.token_hash)).await?;
    put_index(&st.kv, org.id, &index).await?;

    Ok(ok(json!({ "revoked": invite.id })))
}

/// The token from the emailed link.
#[derive(Debug, Deserialize)]
struct InviteToken {
    token: String,
}

/// `POST /api/invitations/preview` — what an invitation is for, before anybody
/// signs in.
///
/// The invitation screen names the organisation and the address before it asks
/// for anything, and it needs the address to send the sign-in code to. The
/// token is the credential, so no session is asked for: whoever holds it read
/// the email it was sent in, which is the same proof the code will ask for.
/// It travels in the body rather than the path because the access log records
/// paths. An unknown, expired, withdrawn or used token is one `404`.
async fn preview_invite(
    State(st): State<AppState>,
    Json(body): Json<InviteToken>,
) -> ApiResult<Json<Value>> {
    let digest = crypto::token_hash(body.token.trim());
    let Some(invite) = live_invite(&st.kv, &digest).await? else {
        return Err(Error::not_found("invitation").into());
    };

    let org = orgs::get(&st.pg, invite.org_id).await?;
    // The inviter may have left since. Their name is a courtesy, not a fact
    // the invitation depends on.
    let invited_by = users::get(&st.pg, invite.invited_by)
        .await
        .ok()
        .map(|u| u.name);

    Ok(ok(json!({
        "email": invite.email,
        "org_id": org.id,
        "org_name": org.name,
        "role": invite.role,
        "invited_by": invited_by,
        "expires_at": invite.expires_at,
    })))
}

/// `POST /api/invitations/accept` — join the organisation.
///
/// Requires a signed-in account whose address is the invited one. Letting any
/// authenticated user redeem any token would turn a leaked link into access for
/// whoever found it.
async fn accept_invite(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<InviteToken>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let digest = crypto::token_hash(body.token.trim());
    let Some(invite) = live_invite(&st.kv, &digest).await? else {
        return Err(Error::not_found("invitation").into());
    };

    if !invite.email.eq_ignore_ascii_case(auth.user.email.trim()) {
        return Err(
            Error::Forbidden("this invitation was sent to a different address".into()).into(),
        );
    }

    let member = redeem_invite(&st.pg, &st.kv, &invite, auth.user.id).await?;
    let org = orgs::get(&st.pg, invite.org_id).await?;
    Ok(ok(json!({
        "organization": org_json(&org, Some(&member.role), None),
        "role": member.role,
    })))
}

// ---------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------

/// `GET /api/orgs/{org}/teams` — every team in the organisation.
async fn list_teams(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, VIEWER).await?;
    let teams = orgs::list_teams(&st.pg, org.id).await?;

    let mut out = Vec::with_capacity(teams.len());
    for team in teams {
        let members = orgs::list_team_members(&st.pg, team.id).await?;
        out.push(json!({
            "id": team.id,
            "org_id": team.org_id,
            "name": team.name,
            "member_count": members.len(),
        }));
    }
    Ok(ok(out))
}

/// A team's only field.
#[derive(Debug, Deserialize)]
struct TeamBody {
    name: String,
}

/// `POST /api/orgs/{org}/teams` — create one.
async fn create_team(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(org_id): Path<Uuid>,
    Json(body): Json<TeamBody>,
) -> ApiResult<Json<Value>> {
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, ADMIN).await?;
    let name = clean_name(&body.name, "name")?;
    let team = orgs::create_team(&st.pg, new_id(), org.id, &name).await?;
    Ok(ok(team_json(&team, 0)))
}

/// Load a team and assert the caller's role in its organisation.
async fn authorize_team(
    st: &AppState,
    team_id: Uuid,
    user_id: Uuid,
    needed: &str,
) -> Result<orgs::Team> {
    let team = orgs::get_team(&st.pg, team_id).await?;
    // Resolve the role *after* the team is loaded but report the team as
    // missing, not forbidden, when the caller is outside its organisation.
    match effective_org_role(&st.pg, team.org_id, user_id).await {
        Ok(role) => require_at_least(&role, needed)?,
        Err(Error::NotFound(_)) => return Err(Error::not_found("team")),
        Err(e) => return Err(e),
    }
    Ok(team)
}

/// `GET /api/teams/{team}` — one team and who is in it.
async fn get_team(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let team = authorize_team(&st, team_id, auth.user.id, VIEWER).await?;
    let members = orgs::list_team_members(&st.pg, team.id).await?;
    Ok(ok(team_json(&team, members.len())))
}

/// `PATCH /api/teams/{team}` — rename.
async fn update_team(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(team_id): Path<Uuid>,
    Json(body): Json<TeamBody>,
) -> ApiResult<Json<Value>> {
    let team = authorize_team(&st, team_id, auth.user.id, ADMIN).await?;
    let name = clean_name(&body.name, "name")?;
    let updated = orgs::update_team(&st.pg, team.id, &name).await?;
    let members = orgs::list_team_members(&st.pg, updated.id).await?;
    Ok(ok(team_json(&updated, members.len())))
}

/// `DELETE /api/teams/{team}` — remove it. Members keep their org membership.
async fn delete_team(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let team = authorize_team(&st, team_id, auth.user.id, ADMIN).await?;
    orgs::delete_team(&st.pg, team.id).await?;
    Ok(ok(json!({ "deleted": team.id })))
}

/// `GET /api/teams/{team}/members` — who is in it.
async fn list_team_members(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let team = authorize_team(&st, team_id, auth.user.id, VIEWER).await?;
    let members = orgs::list_team_members(&st.pg, team.id).await?;

    let mut out = Vec::with_capacity(members.len());
    for member in members {
        let Ok(user) = users::get(&st.pg, member.user_id).await else {
            continue;
        };
        out.push(json!({
            "user_id": user.id,
            "email": user.email,
            "name": user.name,
            "avatar_url": user.avatar_url,
        }));
    }
    Ok(ok(out))
}

/// Who to add to a team.
#[derive(Debug, Deserialize)]
struct TeamMemberBody {
    user_id: Uuid,
}

/// `POST /api/teams/{team}/members` — add somebody already in the organisation.
async fn add_team_member(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(team_id): Path<Uuid>,
    Json(body): Json<TeamMemberBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let team = authorize_team(&st, team_id, auth.user.id, ADMIN).await?;

    // A team is a grouping inside an organisation, so it cannot reach outside
    // one. Without this check a team would become a second, weaker way in.
    if orgs::get_member(&st.pg, team.org_id, body.user_id)
        .await?
        .is_none()
    {
        return Err(Error::validation("user_id", "is not a member of this organisation").into());
    }

    orgs::add_team_member(&st.pg, team.id, body.user_id).await?;
    Ok(ok(json!({ "team_id": team.id, "user_id": body.user_id })))
}

/// `DELETE /api/teams/{team}/members/{user}` — take somebody out.
async fn remove_team_member(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((team_id, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let team = authorize_team(&st, team_id, auth.user.id, ADMIN).await?;
    orgs::remove_team_member(&st.pg, team.id, user_id).await?;
    Ok(ok(json!({ "removed": user_id })))
}

// ---------------------------------------------------------------------------
// Per-site access
// ---------------------------------------------------------------------------

/// `GET /api/sites/{site}/access` — the overrides on one site.
async fn list_site_access(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let site =
        crate::routes::sites::authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let rows = sites::list_access(&st.pg, site.id).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Ok(user) = users::get(&st.pg, row.user_id).await else {
            continue;
        };
        out.push(json!({
            "user_id": user.id,
            "email": user.email,
            "name": user.name,
            "role": row.role,
        }));
    }
    Ok(ok(out))
}

/// A per-site grant.
#[derive(Debug, Deserialize)]
struct SiteAccessBody {
    user_id: Uuid,
    role: String,
}

/// `POST /api/sites/{site}/access` — override somebody's org role on one site.
///
/// Only downward: the target must stand below the caller (see
/// [`require_outranks`]), and never be the owner.
async fn grant_site_access(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
    Json(body): Json<SiteAccessBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let site =
        crate::routes::sites::authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let mine = crate::routes::sites::effective_site_role(&st.pg, &site, auth.user.id).await?;
    let role = parse_role(&body.role)?;
    require_can_grant(&mine, role)?;

    // The override is a *narrowing or widening within one organisation*, not a
    // way to hand a site to somebody outside it.
    let Some(member) = orgs::get_member(&st.pg, site.org_id, body.user_id).await? else {
        return Err(Error::validation(
            "user_id",
            "is not a member of the organisation that owns this site",
        )
        .into());
    };
    let standing = target_standing(&st, &site, body.user_id, Some(member.role)).await?;
    require_outranks(
        &mine,
        standing.as_deref(),
        is_owner(&st, &site, body.user_id).await?,
    )?;

    let granted = sites::grant_access(&st.pg, site.id, body.user_id, role).await?;
    Ok(ok(json!({
        "site_id": site.site_id,
        "user_id": granted.user_id,
        "role": granted.role,
    })))
}

/// `DELETE /api/sites/{site}/access/{user}` — drop the override.
async fn revoke_site_access(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((site_ref, user_id)): Path<(String, Uuid)>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let site =
        crate::routes::sites::authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let mine = crate::routes::sites::effective_site_role(&st.pg, &site, auth.user.id).await?;

    // Revoking is a grant too: it moves the target back to their organisation
    // role, so it is held to the same rule.
    let org_role = orgs::get_member(&st.pg, site.org_id, user_id)
        .await?
        .map(|m| m.role);
    let standing = target_standing(&st, &site, user_id, org_role).await?;
    require_outranks(
        &mine,
        standing.as_deref(),
        is_owner(&st, &site, user_id).await?,
    )?;

    sites::revoke_access(&st.pg, site.id, user_id).await?;
    Ok(ok(json!({ "revoked": user_id })))
}

/// The higher of a target's organisation role and their override on `site`.
///
/// The higher one, because an override that narrowed an `admin` to `viewer` on
/// one site does not make them a `viewer` in the organisation, and must not
/// make them reachable by another `admin`.
async fn target_standing(
    st: &AppState,
    site: &sites::Site,
    user_id: Uuid,
    org_role: Option<String>,
) -> Result<Option<String>> {
    let override_role = sites::get_access(&st.pg, site.id, user_id)
        .await?
        .map(|a| a.role);
    // An unknown role ranks at the top, so it can only make the target harder
    // to reach.
    let rank = |role: &str| role_rank(role).unwrap_or(3);
    Ok(match (org_role, override_role) {
        (Some(org), Some(over)) if rank(&over) > rank(&org) => Some(over),
        (Some(org), _) => Some(org),
        (None, over) => over,
    })
}

/// Is `user_id` the owner of the organisation that holds `site`?
async fn is_owner(st: &AppState, site: &sites::Site, user_id: Uuid) -> Result<bool> {
    Ok(orgs::get(&st.pg, site.org_id).await?.owner_id == user_id)
}

// ---------------------------------------------------------------------------
// Shared shaping and validation
// ---------------------------------------------------------------------------

/// One organisation on the wire.
fn org_json(org: &orgs::Organization, role: Option<&str>, site_count: Option<usize>) -> Value {
    json!({
        "id": org.id,
        "name": org.name,
        "slug": org.slug,
        "owner_id": org.owner_id,
        "created_at": org.created_at,
        "role": role,
        "site_count": site_count,
    })
}

/// One team on the wire.
fn team_json(team: &orgs::Team, member_count: usize) -> Value {
    json!({
        "id": team.id,
        "org_id": team.org_id,
        "name": team.name,
        "member_count": member_count,
    })
}

/// One invitation on the wire. Never carries the token.
fn invite_json(invite: &Invite) -> Value {
    json!({
        "id": invite.id,
        "org_id": invite.org_id,
        "email": invite.email,
        "role": invite.role,
        "invited_by": invite.invited_by,
        "created_at": invite.created_at,
        "expires_at": invite.expires_at,
    })
}

/// Trim and bound a display name.
pub fn clean_name(raw: &str, field: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(Error::validation(field, "cannot be empty"));
    }
    if name.chars().count() > MAX_NAME {
        return Err(Error::validation(
            field,
            format!("cannot be longer than {MAX_NAME} characters"),
        ));
    }
    Ok(name.to_string())
}

/// Lower-case and sanity-check an address.
///
/// Deliberately not a full RFC 5322 parse. The only thing that proves an address
/// is real is mail arriving at it, so this rejects what is obviously not an
/// address and leaves the rest to the sign-in code.
pub fn clean_email(raw: &str) -> Result<String> {
    let email = raw.trim().to_lowercase();
    let looks_addressable = email.len() >= 3
        && email.len() <= 254
        && !email.contains(char::is_whitespace)
        && email.match_indices('@').count() == 1
        && email.split('@').all(|part| !part.is_empty())
        && email
            .split('@')
            .nth(1)
            .is_some_and(|host| host.contains('.'));

    if !looks_addressable {
        return Err(Error::validation("email", "is not a valid email address"));
    }
    Ok(email)
}

/// What the live tests of other modules need from this one.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// Invite `email` into `org_id` as `auth`, through the real handler, so a
    /// test elsewhere gets an invitation exactly as an admin would make one.
    /// Answers what the handler answered, link included.
    pub(crate) async fn invite(st: &AppState, auth: AuthUser, org_id: Uuid, email: &str) -> Value {
        create_invite(
            State(st.clone()),
            auth,
            Path(org_id),
            Json(NewInvite {
                email: email.to_string(),
                role: None,
            }),
        )
        .await
        .expect("an admin may invite an address")
        .0["data"]
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_orders_the_four_roles() {
        assert!(role_rank(OWNER) > role_rank(ADMIN));
        assert!(role_rank(ADMIN) > role_rank(MEMBER));
        assert!(role_rank(MEMBER) > role_rank(VIEWER));
        assert_eq!(role_rank("superuser"), None);
    }

    #[test]
    fn viewer_is_read_only_everywhere() {
        assert!(require_at_least(VIEWER, VIEWER).is_ok());
        assert!(require_at_least(VIEWER, MEMBER).is_err());
        assert!(require_at_least(ADMIN, MEMBER).is_ok());
        // A requirement this build does not recognise fails closed.
        assert!(require_at_least(ADMIN, "archon").is_err());
    }

    #[test]
    fn nobody_grants_a_role_above_their_own() {
        assert!(require_can_grant(ADMIN, MEMBER).is_ok());
        assert!(require_can_grant(ADMIN, ADMIN).is_ok());
        assert!(require_can_grant(ADMIN, OWNER).is_err());
        assert!(require_can_grant(MEMBER, ADMIN).is_err());
    }

    #[test]
    fn a_site_grant_only_reaches_members_below_the_caller() {
        // The owner is out of reach whatever the caller holds.
        assert!(require_outranks(OWNER, Some(OWNER), true).is_err());
        assert!(require_outranks(ADMIN, Some(OWNER), true).is_err());
        // An admin cannot touch another admin, or themselves.
        assert!(require_outranks(ADMIN, Some(ADMIN), false).is_err());
        // An admin can manage members and viewers.
        assert!(require_outranks(ADMIN, Some(MEMBER), false).is_ok());
        assert!(require_outranks(ADMIN, Some(VIEWER), false).is_ok());
        // The owner can manage an admin.
        assert!(require_outranks(OWNER, Some(ADMIN), false).is_ok());
        // An unknown role on the target fails closed.
        assert!(require_outranks(ADMIN, Some("archon"), false).is_err());
        // A stale override on somebody who has left the organisation.
        assert!(require_outranks(ADMIN, None, false).is_ok());
    }

    #[test]
    fn slugs_are_url_safe_and_never_empty() {
        assert_eq!(slugify("  Acme Analytics!  "), "acme-analytics");
        assert_eq!(slugify("a//b"), "a-b");
        assert_eq!(slugify("日本語"), "org");
        assert!(!slugify("---").is_empty());
        assert!(slugify(&"x".repeat(200)).len() <= 48);
    }

    #[test]
    fn addresses_are_checked_only_as_far_as_is_honest() {
        assert_eq!(
            clean_email("  Owner@Example.COM ").unwrap(),
            "owner@example.com"
        );
        assert!(clean_email("owner@localhost").is_err());
        assert!(clean_email("owner@@example.com").is_err());
        assert!(clean_email("not an address").is_err());
        assert!(clean_email("@example.com").is_err());
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(clean_name("  Acme  ", "name").unwrap(), "Acme");
        assert!(clean_name("   ", "name").is_err());
        assert!(clean_name(&"x".repeat(MAX_NAME + 1), "name").is_err());
    }
}
