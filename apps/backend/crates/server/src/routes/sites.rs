//! `/api/sites/*` and `/api/share/*`.
//!
//! A site is the tracked property, and it carries two identifiers that are not
//! interchangeable: `id`, the UUID every other Postgres table references, and
//! `site_id`, the small integer that is the leading column of every ClickHouse
//! sort key. The dashboard addresses sites by the numeric form (`/app/3`) and
//! the API's own object graph uses the UUID, so [`resolve_site_ref`] accepts
//! either in a path segment. A UUID can never parse as a `u16`, so there is no
//! ambiguity to resolve.
//!
//! ## The ingest key
//!
//! `POST /api/log/edge` is signed with a per-site key, and that key is a secret,
//! so `docs/CONTRACT.md` §2 requires it sealed at rest. It lives in the site's
//! own `settings` document as `ingest_key_sealed`: XChaCha20-Poly1305, keyed off
//! `MICAFORGE_SECRET_KEY_BASE` for the `ingest` purpose, with the site's UUID as
//! additional authenticated data so the blob is worthless in another site's row.
//! It is minted lazily on first read, which means an install that never uses the
//! server-side ingest path never generates one.
//!
//! [`ingest_key`] is public because the ingest path has to verify against the
//! same value this screen shows the operator.

use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Datelike, TimeZone, Utc};
use micaforge_core::{crypto, new_id, Config, Error, Result};
use micaforge_store::models::sites::Site;
use micaforge_store::models::{orgs, sites};
use micaforge_store::pg::PgPool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, ClientAddr, PublicSite, SiteAccess};
use crate::response::ok;
use crate::routes::auth::{hash_password_blocking, verify_password_blocking};
use crate::routes::orgs::{clean_name, require_at_least, ADMIN, MEMBER, OWNER, VIEWER};
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Where the sealed ingest key lives inside `sites.settings`.
const INGEST_KEY_FIELD: &str = "ingest_key_sealed";

/// Ingest keys are prefixed so a leaked one is recognisable in a log or a paste.
const INGEST_KEY_PREFIX: &str = "mfi_";

/// Key derivation purpose for the ingest key. Changing it invalidates every
/// site's key at once.
const INGEST_PURPOSE: &str = "ingest";

/// Longest accepted hostname.
const MAX_DOMAIN: usize = 253;

/// Characters in a generated share slug. Sixteen bytes of entropy, url-safe.
const SHARE_SLUG_BYTES: usize = 12;

/// Where a public site's share slug lives inside `sites.settings`. Written by
/// the server only, like the ingest key.
const PUBLIC_SLUG_FIELD: &str = "public_slug";

/// The share scope vocabulary, in the order the settings screen lists them.
///
/// `locations` and `devices` are accepted and stored, but the shared view does
/// not draw those panels yet, so they open no endpoint (see
/// [`crate::extract::share_scope_for`]).
pub const SHARE_SCOPES: &[&str] = &[
    "overview",
    "agents",
    "citation_gap",
    "pages",
    "sources",
    "locations",
    "devices",
];

/// What a link publishes when it names nothing: the overview, never more.
const SHARE_DEFAULT_SCOPE: &str = "overview";

/// Every unlock token starts with this, which is also how `?share=` tells a
/// token from a slug. Refused as the start of a slug for that reason.
pub const SHARE_TOKEN_PREFIX: &str = "mfs_";

/// Entropy in an unlock token.
const SHARE_TOKEN_BYTES: usize = 32;

/// How long an unlock token works. An hour: a reading session, not a standing
/// credential. The reader enters the password again after it.
const SHARE_TOKEN_TTL_SECS: u32 = 60 * 60;

/// Unlock attempts one link accepts per window, from everyone together.
const UNLOCK_LINK_LIMIT: u32 = 10;

/// Unlock attempts one address may make per window, across every link.
const UNLOCK_IP_LIMIT: u32 = 30;

/// The unlock window, in seconds.
const UNLOCK_WINDOW_SECS: u32 = 15 * 60;

// ---------------------------------------------------------------------------
// Resolution and authorisation
// ---------------------------------------------------------------------------

/// Resolve a path segment that is either a site UUID or the numeric `site_id`.
///
/// The dashboard's own URLs carry the numeric form because that is what a person
/// sees in the tracking snippet; the API's object graph carries UUIDs. Accepting
/// both here is what stops every caller from having to know which one it holds.
pub async fn resolve_site_ref(pg: &PgPool, reference: &str) -> Result<Site> {
    let reference = reference.trim();
    if let Ok(id) = reference.parse::<Uuid>() {
        return sites::get(pg, id).await;
    }
    if let Ok(numeric) = reference.parse::<u16>() {
        return sites::get_by_site_id(pg, numeric).await;
    }
    Err(Error::not_found("site"))
}

/// The caller's effective role on one site.
///
/// Organisation role, overridden by `site_access` when a row names this user.
/// The override wins in **both** directions: it can narrow an org `admin` to a
/// `viewer` on one sensitive property, and it can widen an org `viewer` to a
/// `member` on the one site they actually run.
///
/// A caller with neither gets `not_found`, not `forbidden`, so walking site ids
/// reveals nothing.
///
/// This is the **caller's** role, and it is where an API key is held to its
/// grant (see [`crate::extract`]): a site outside the key's organisation, or
/// any site but its own for a site-bound key, reads as absent, and the role is
/// capped at what the key's scopes allow. To ask what role somebody *else*
/// holds, use [`site_role_of`].
pub async fn effective_site_role(pg: &PgPool, site: &Site, user_id: Uuid) -> Result<String> {
    let key = crate::extract::current_key();
    if let Some(grant) = key.as_ref() {
        if !grant.permits_site(site.org_id, site.id) {
            return Err(Error::not_found("site"));
        }
    }

    let role = site_role_of(pg, site, user_id)
        .await?
        .ok_or_else(|| Error::not_found("site"))?;
    Ok(match key {
        Some(grant) => grant.cap_role(&role),
        None => role,
    })
}

/// The role `user_id` holds on `site`, with no caller in the picture.
///
/// `None` when they hold none. For decisions about a *target* account, such as
/// whether a grant may touch them; never for authorising the caller.
pub async fn site_role_of(pg: &PgPool, site: &Site, user_id: Uuid) -> Result<Option<String>> {
    let org_role = orgs::get_member(pg, site.org_id, user_id)
        .await?
        .map(|m| m.role);
    let override_role = sites::get_access(pg, site.id, user_id)
        .await?
        .map(|a| a.role);
    Ok(override_role.or(org_role))
}

/// Load a site by UUID and assert the caller holds at least `needed`.
///
/// The entry point every route module that addresses an object by its own id
/// uses: a goal knows its `site_id`, and this turns that into a permission.
pub async fn authorize_site(
    pg: &PgPool,
    site_id: Uuid,
    user_id: Uuid,
    needed: &str,
) -> Result<Site> {
    let site = sites::get(pg, site_id).await?;
    let role = effective_site_role(pg, &site, user_id).await?;
    require_at_least(&role, needed)?;
    Ok(site)
}

/// The same, for a path segment in either identifier form.
pub async fn authorize_site_ref(
    st: &AppState,
    reference: &str,
    user_id: Uuid,
    needed: &str,
) -> Result<Site> {
    let site = resolve_site_ref(&st.pg, reference).await?;
    let role = effective_site_role(&st.pg, &site, user_id).await?;
    require_at_least(&role, needed)?;
    Ok(site)
}

// ---------------------------------------------------------------------------
// The ingest key
// ---------------------------------------------------------------------------

/// The site's server-side ingest key, minting one if it has none.
///
/// Returns the plaintext, so this is only ever called behind an `admin` check or
/// from the ingest path verifying a signature. The stored form is sealed; see
/// the module documentation for why and how.
pub async fn ingest_key(pg: &PgPool, cfg: &Config, site: &Site) -> Result<String> {
    let key = cfg.derive_key(INGEST_PURPOSE);

    if let Some(encoded) = site
        .settings
        .get(INGEST_KEY_FIELD)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let sealed = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| Error::internal("the stored ingest key is not valid base64"))?;
        let plaintext = crypto::open(&key, site.id.as_bytes(), &sealed)?;
        return String::from_utf8(plaintext)
            .map_err(|_| Error::internal("the stored ingest key is not valid utf-8"));
    }

    rotate_ingest_key(pg, cfg, site).await
}

/// Mint a fresh ingest key and store it sealed, replacing any predecessor.
///
/// Rotation is immediate and total: the previous key stops verifying the moment
/// this returns. That is the behaviour a rotation button has to have — a grace
/// period would mean a leaked key kept working after the operator revoked it.
pub async fn rotate_ingest_key(pg: &PgPool, cfg: &Config, site: &Site) -> Result<String> {
    let token = format!("{INGEST_KEY_PREFIX}{}", crypto::random_token(32));
    let sealed = crypto::seal(
        &cfg.derive_key(INGEST_PURPOSE),
        site.id.as_bytes(),
        token.as_bytes(),
    )?;

    let mut settings = site.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    if let Some(object) = settings.as_object_mut() {
        object.insert(
            INGEST_KEY_FIELD.to_string(),
            json!(URL_SAFE_NO_PAD.encode(sealed)),
        );
    }
    sites::update_settings(pg, site.id, &settings).await?;

    Ok(token)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every site and share-link route. Paths are absolute from the app root.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/sites", get(list_sites).post(create_site))
        .route(
            "/api/sites/{site}",
            get(get_site).patch(update_site).delete(delete_site),
        )
        .route("/api/sites/{site}/tracking", get(tracking))
        .route("/api/sites/{site}/usage", get(usage))
        .route("/api/sites/{site}/has-data", get(has_data))
        .route("/api/sites/{site}/rotate-key", post(rotate_key))
        .route("/api/share", get(list_share).post(create_share))
        .route(
            "/api/share/{link}",
            get(get_share).patch(update_share).delete(delete_share),
        )
        .route("/api/share/{link}/unlock", post(unlock_share))
}

// ---------------------------------------------------------------------------
// Site CRUD
// ---------------------------------------------------------------------------

/// `GET /api/sites` — every site the caller can reach, with their role on each.
async fn list_sites(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    let rows = sites::list_sites_for_user(&st.pg, auth.user.id).await?;

    // A key lists its own organisation's sites, or its one site, and not
    // every site its owner happens to belong to.
    let visible: Vec<Site> = rows
        .into_iter()
        .filter(|site| {
            auth.key
                .as_ref()
                .is_none_or(|grant| grant.permits_site(site.org_id, site.id))
        })
        .collect();

    // Which of them have ever reported, asked once for the whole list: the site
    // chooser renders a site that has never reported as its install steps, so
    // a list without this marks every site "not reporting yet". If ClickHouse
    // cannot answer, the list still loads and simply omits the field.
    let ids: Vec<u16> = visible.iter().map(|s| s.site_id_u16()).collect();
    let reporting = match sites_with_data(&st, &ids).await {
        Ok(found) => Some(found),
        Err(e) => {
            tracing::warn!(error = %e, "could not read which sites have reported");
            None
        }
    };

    let mut out = Vec::with_capacity(visible.len());
    for site in visible {
        // The list query already proved visibility; this resolves *which* role,
        // which the dashboard needs to decide what to render as editable.
        let role = effective_site_role(&st.pg, &site, auth.user.id).await?;
        let mut row = site_json(&site, Some(&role));
        if let Some(found) = &reporting {
            row["has_data"] = json!(found.contains(&site.site_id_u16()));
        }
        out.push(row);
    }
    Ok(ok(out))
}

/// One site id, from the reporting check.
#[derive(Debug, clickhouse::Row, serde::Deserialize)]
struct SiteIdRow {
    site_id: u16,
}

/// The subset of `ids` with at least one human or agent row, in one query.
///
/// `DISTINCT ... LIMIT n` lets ClickHouse stop reading as soon as every listed
/// site has been seen, and `site_id` leads both tables' sort keys, so a site
/// with no rows costs an index lookup rather than a scan.
async fn sites_with_data(st: &AppState, ids: &[u16]) -> Result<std::collections::HashSet<u16>> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let rows = st
        .ch
        .client()
        .query(
            "SELECT DISTINCT site_id FROM (
                 SELECT site_id FROM events WHERE has(?, site_id)
                 UNION ALL
                 SELECT site_id FROM agent_events WHERE has(?, site_id)
             ) LIMIT ?",
        )
        .bind(ids)
        .bind(ids)
        .bind(ids.len() as u64)
        .fetch_all::<SiteIdRow>()
        .await
        .map_err(|e| Error::Database(format!("clickhouse sites-with-data: {e}")))?;
    Ok(rows.into_iter().map(|r| r.site_id).collect())
}

/// What a new site needs.
#[derive(Debug, Deserialize)]
struct NewSite {
    org_id: Uuid,
    domain: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
}

/// `POST /api/sites` — add a property to an organisation.
async fn create_site(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<NewSite>,
) -> ApiResult<Json<Value>> {
    let (org, _) =
        crate::routes::orgs::authorize_org(&st.pg, body.org_id, auth.user.id, ADMIN).await?;

    let domain = clean_domain(&body.domain)?;
    let name = match body.name.as_deref() {
        Some(raw) => clean_name(raw, "name")?,
        None => domain.clone(),
    };
    let timezone = clean_timezone(body.timezone.as_deref().unwrap_or("UTC"))?;

    // One property per hostname across the install. Two sites collecting the
    // same domain would split every number in half with no way to tell which
    // half was which.
    if sites::get_by_domain(&st.pg, &domain).await?.is_some() {
        return Err(Error::Conflict(format!("{domain} is already being tracked")).into());
    }

    let site = sites::create(&st.pg, new_id(), org.id, &domain, &name, &timezone).await?;
    Ok(ok(site_json(&site, Some(ADMIN))))
}

/// `GET /api/sites/{site}` — one site.
async fn get_site(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, VIEWER).await?;
    let role = effective_site_role(&st.pg, &site, auth.user.id).await?;
    Ok(ok(site_json(&site, Some(&role))))
}

/// Editable site fields. Everything is optional; absent means unchanged.
#[derive(Debug, Deserialize)]
struct SitePatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    public: Option<bool>,
    /// Tracking options: excluded paths, auto-capture switches.
    #[serde(default)]
    settings: Option<Value>,
}

/// `PATCH /api/sites/{site}` — edit the settings screen's fields.
async fn update_site(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
    Json(body): Json<SitePatch>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;

    let name = match body.name.as_deref() {
        Some(raw) => clean_name(raw, "name")?,
        None => site.name.clone(),
    };
    let domain = match body.domain.as_deref() {
        Some(raw) => clean_domain(raw)?,
        None => site.domain.clone(),
    };
    let timezone = match body.timezone.as_deref() {
        Some(raw) => clean_timezone(raw)?,
        None => site.timezone.clone(),
    };
    let public = body.public.unwrap_or(site.public);

    if domain != site.domain {
        if let Some(existing) = sites::get_by_domain(&st.pg, &domain).await? {
            if existing.id != site.id {
                return Err(Error::Conflict(format!("{domain} is already being tracked")).into());
            }
        }
    }

    let mut updated = sites::update(&st.pg, site.id, &name, &domain, &timezone, public).await?;

    if let Some(patch) = body.settings {
        // The sealed ingest key lives in this same document, so a settings write
        // merges rather than replaces. A dashboard that round-tripped the object
        // it was given would otherwise revoke the site's ingest key by accident.
        let merged = merge_settings(&updated.settings, &patch)?;
        updated = sites::update_settings(&st.pg, site.id, &merged).await?;
    }

    // A public site is read at `/share/<public slug>`, so publishing issues the
    // address. It survives unpublishing (the gate is `public` itself), which is
    // what keeps an open-source project's public link stable across a toggle.
    if updated.public {
        let (_, reloaded) = ensure_public_slug(&st.pg, &updated).await?;
        updated = reloaded;
    }

    let role = effective_site_role(&st.pg, &updated, auth.user.id).await?;
    Ok(ok(site_json(&updated, Some(&role))))
}

/// `DELETE /api/sites/{site}` — remove the property.
///
/// Postgres rows go immediately. The ClickHouse rows are removed by the
/// retention job's partition-scoped delete, because dropping billions of rows
/// inside an HTTP request is not something that can work.
async fn delete_site(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, OWNER).await?;
    sites::delete(&st.pg, site.id).await?;
    Ok(ok(json!({
        "deleted": site.id,
        "site_id": site.site_id,
        "events_removed": false,
        "note": "event rows are removed by the retention job, not synchronously",
    })))
}

// ---------------------------------------------------------------------------
// Tracking, usage, data presence
// ---------------------------------------------------------------------------

/// `GET /api/sites/{site}/tracking` — the snippet and the ingest key.
///
/// `admin` and above, because the ingest key is in the response. A `member` who
/// only configures goals has no need for a credential that can write events.
async fn tracking(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let key = ingest_key(&st.pg, &st.cfg, &site).await?;
    let base = st.cfg.public_url.trim_end_matches('/');
    let numeric = site.site_id_u16();

    Ok(ok(json!({
        "site_id": numeric,
        "domain": site.domain,
        "script_url": format!("{base}/mf.js"),
        "snippet": format!(
            "<script defer data-site=\"{numeric}\" src=\"{base}/mf.js\"></script>"
        ),
        "pixel_url": format!("{base}/api/track/pixel.gif?s={numeric}"),
        "edge_endpoint": format!("{base}/api/log/edge"),
        "ingest_key": key,
        "ingest_key_header": "X-Micaforge-Signature",
    })))
}

/// One site's row counts, as ClickHouse reports them.
#[derive(Debug, clickhouse::Row, serde::Deserialize)]
struct UsageRow {
    /// Rows in the current calendar month, in the site's own timezone.
    in_month: u64,
    /// Rows ever recorded for this site.
    total: u64,
    /// Epoch milliseconds of the oldest row. Meaningless when `total` is zero.
    first_ms: i64,
    /// Epoch milliseconds of the newest row.
    last_ms: i64,
}

/// `GET /api/sites/{site}/usage` — how much of the store this site occupies.
///
/// Human events and agent events are reported separately and never summed. A
/// single "events" number that folded crawler fetches into pageviews would be
/// the exact mistake this product exists to correct.
async fn usage(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, VIEWER).await?;
    let numeric = site.site_id_u16();
    let month_start = month_start(&site.timezone);

    let events = usage_for(&st, "events", numeric, month_start).await?;
    let agents = usage_for(&st, "agent_events", numeric, month_start).await?;

    Ok(ok(json!({
        "site_id": numeric,
        "month_start": month_start,
        "timezone": site.timezone,
        "retention_days": st.cfg.retention_days,
        "events": usage_json(&events),
        "agent_events": usage_json(&agents),
    })))
}

/// Count one table for one site.
///
/// The table name is a literal chosen by the caller from a fixed set, never a
/// value off the wire: ClickHouse has no bind form for an identifier, so the
/// only safe way to vary one is not to vary it by input at all.
async fn usage_for(
    st: &AppState,
    table: &'static str,
    site: u16,
    month_start: DateTime<Utc>,
) -> Result<UsageRow> {
    let sql = format!(
        "SELECT
             countIf(timestamp >= toDateTime64(?, 3, 'UTC')) AS in_month,
             count() AS total,
             toUnixTimestamp64Milli(min(timestamp)) AS first_ms,
             toUnixTimestamp64Milli(max(timestamp)) AS last_ms
         FROM {table}
         WHERE site_id = ?"
    );

    st.ch
        .client()
        .query(&sql)
        .bind(micaforge_query::sql::fmt_ts(month_start))
        .bind(site)
        .fetch_one::<UsageRow>()
        .await
        .map_err(|e| Error::Database(format!("clickhouse usage: {e}")))
}

/// Shape a usage row, suppressing the epoch when the table holds nothing.
fn usage_json(row: &UsageRow) -> Value {
    let (first, last) = if row.total == 0 {
        (None, None)
    } else {
        (
            Utc.timestamp_millis_opt(row.first_ms).single(),
            Utc.timestamp_millis_opt(row.last_ms).single(),
        )
    };
    json!({
        "this_month": row.in_month,
        "total": row.total,
        "first_seen": first,
        "last_seen": last,
    })
}

/// Whether a site has ever recorded anything.
#[derive(Debug, clickhouse::Row, serde::Deserialize)]
struct PresenceRow {
    human: u64,
    agent: u64,
}

/// `GET /api/sites/{site}/has-data` — has anything ever arrived?
///
/// This is what the setup screen polls while the operator installs the snippet,
/// so it must stay cheap however large the table is. Each half is a `LIMIT 1`
/// inside a subquery: ClickHouse stops at the first matching row rather than
/// counting the site's whole partition set.
async fn has_data(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, VIEWER).await?;
    let numeric = site.site_id_u16();

    let row = st
        .ch
        .client()
        .query(
            "SELECT
                 (SELECT count() FROM (SELECT 1 FROM events WHERE site_id = ? LIMIT 1)) AS human,
                 (SELECT count() FROM (SELECT 1 FROM agent_events WHERE site_id = ? LIMIT 1)) AS agent",
        )
        .bind(numeric)
        .bind(numeric)
        .fetch_one::<PresenceRow>()
        .await
        .map_err(|e| Error::Database(format!("clickhouse has-data: {e}")))?;

    let (live_humans, live_agents) = st.kv.realtime_count(numeric).await.unwrap_or((0, 0));

    Ok(ok(json!({
        "site_id": numeric,
        "has_data": row.human > 0 || row.agent > 0,
        "has_human_data": row.human > 0,
        "has_agent_data": row.agent > 0,
        "live_visitors": live_humans,
        "live_agents": live_agents,
    })))
}

/// `POST /api/sites/{site}/rotate-key` — replace the ingest key now.
async fn rotate_key(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let key = rotate_ingest_key(&st.pg, &st.cfg, &site).await?;

    tracing::info!(site = %site.id, actor = %auth.user.id, "ingest key rotated");
    Ok(ok(json!({
        "site_id": site.site_id_u16(),
        "ingest_key": key,
        "rotated_at": Utc::now(),
        "note": "the previous key stopped verifying immediately",
    })))
}

// ---------------------------------------------------------------------------
// Share links: management
// ---------------------------------------------------------------------------

/// `GET /api/share?site=…`: the site's public address and every link on it,
/// including expired ones.
///
/// One object rather than a bare list, because the share screen manages both
/// ways of sharing at once: `public` and `public_slug` for the public
/// dashboard, `links` for the private links.
async fn list_share(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), ADMIN)?;
    let links = sites::list_share_links(&st.pg, access.site.id).await?;
    let base = st.cfg.dashboard_url.trim_end_matches('/');

    // A site published before public slugs existed gets its address here, the
    // first time somebody looks for it.
    let public_slug = if access.site.public {
        Some(ensure_public_slug(&st.pg, &access.site).await?.0)
    } else {
        None
    };

    let out: Vec<Value> = links.iter().map(|l| share_json(l, base)).collect();
    Ok(ok(json!({
        "public": access.site.public,
        "public_slug": public_slug,
        "public_url": public_slug.as_ref().map(|slug| format!("{base}/share/{slug}")),
        "links": out,
    })))
}

/// What a new share link may specify.
#[derive(Debug, Deserialize)]
struct NewShare {
    /// A readable slug. Generated when absent.
    #[serde(default)]
    slug: Option<String>,
    /// Optional password. Stored as an Argon2id digest, never in the clear.
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
    /// Which panels the link may read, from [`SHARE_SCOPES`]. Defaults to the
    /// overview only.
    #[serde(default)]
    scopes: Option<Value>,
}

/// `POST /api/share?site=…` — publish a read-only dashboard.
async fn create_share(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<NewShare>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), ADMIN)?;

    let slug = match body.slug.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(raw) => clean_slug(raw)?,
        None => fresh_slug(&st.pg).await?,
    };
    // Reported as a conflict on the slug alone. Naming the site that holds it
    // would leak one operator's configuration to another.
    if slug_taken(&st.pg, &slug).await? {
        return Err(Error::Conflict("that share slug is already in use".into()).into());
    }

    let scopes = clean_share_scopes(body.scopes.as_ref())?;
    if let Some(expiry) = body.expires_at {
        if expiry <= Utc::now() {
            return Err(Error::validation("expires_at", "is already in the past").into());
        }
    }
    let password_hash = match body.password.filter(|p| !p.is_empty()) {
        Some(p) => Some(hash_password_blocking(p).await?),
        None => None,
    };

    let link = sites::create_share_link(
        &st.pg,
        new_id(),
        access.site.id,
        &slug,
        password_hash.as_deref(),
        body.expires_at,
        &json!(scopes),
    )
    .await?;

    Ok(ok(share_json(
        &link,
        st.cfg.dashboard_url.trim_end_matches('/'),
    )))
}

/// Every column of `share_links`, matching the repository's own list.
///
/// Written out here because the reads this module needs (one link by id, one by
/// slug whatever its state, and an update in place) have no repository form,
/// and `SELECT *` would let a new column silently change what they return.
const SHARE_COLS: &str = "id, site_id, slug, password_hash, expires_at, scopes";

/// One link by id, expired or not.
async fn load_share_link(pg: &PgPool, id: Uuid) -> Result<Option<sites::ShareLink>> {
    let link = sqlx::query_as::<_, sites::ShareLink>(&format!(
        "SELECT {SHARE_COLS} FROM share_links WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pg)
    .await?;
    Ok(link)
}

/// One link by slug, expired or not.
///
/// Unlike [`micaforge_store::models::sites::get_share_link_by_slug`], this does
/// not hide an expired link, so the shared view can say "this link has expired"
/// rather than "nothing is here". Every caller checks `is_live` itself before
/// it publishes anything.
async fn load_share_link_by_slug(pg: &PgPool, slug: &str) -> Result<Option<sites::ShareLink>> {
    let link = sqlx::query_as::<_, sites::ShareLink>(&format!(
        "SELECT {SHARE_COLS} FROM share_links WHERE slug = $1"
    ))
    .bind(slug.trim())
    .fetch_optional(pg)
    .await?;
    Ok(link)
}

/// Load a share link and assert the caller may administer its site.
///
/// Does **not** hide an expired link: the settings screen has to be able to see
/// the link that lapsed in order to extend or delete it.
async fn authorize_share(st: &AppState, id: Uuid, user_id: Uuid) -> Result<sites::ShareLink> {
    let link = load_share_link(&st.pg, id)
        .await?
        .ok_or_else(|| Error::not_found("share link"))?;

    // Authorising against the link's own site is what stops one operator
    // editing another's published dashboard by guessing a UUID.
    authorize_site(&st.pg, link.site_id, user_id, ADMIN).await?;
    Ok(link)
}

/// `GET /api/share/{link}`: one link, by id or by slug.
///
/// Two readers share this path, told apart by the shape of the segment:
///
/// - **A UUID** is the management read of one link, for a signed-in `admin`
///   of its site. A slug can never be a UUID; [`clean_slug`] refuses one.
/// - **A slug** is the shared view's first question: what does this link
///   publish? Answered without an account. A password-protected link answers
///   `password_required: true` and nothing about its site until `?share=`
///   carries an unlock token for it; an expired one answers `expired: true`.
async fn get_share(
    State(st): State<AppState>,
    Path(reference): Path<String>,
    mut parts: Parts,
) -> ApiResult<Json<Value>> {
    if let Ok(id) = reference.trim().parse::<Uuid>() {
        let auth = AuthUser::from_request_parts(&mut parts, &st).await?;
        let link = authorize_share(&st, id, auth.user.id).await?;
        return Ok(ok(share_json(
            &link,
            st.cfg.dashboard_url.trim_end_matches('/'),
        )));
    }

    let token = crate::extract::query_param(&parts, "share");
    Ok(ok(share_meta(&st, &reference, token.as_deref()).await?))
}

/// What a share link update may change.
#[derive(Debug, Deserialize)]
struct SharePatch {
    /// `Some(Some(pw))` sets a password, `Some(None)` clears it, absent leaves
    /// it alone. The double option is what makes "remove the password" a thing
    /// the API can express at all.
    #[serde(default, deserialize_with = "double_option")]
    password: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    expires_at: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    scopes: Option<Value>,
}

/// Distinguish an absent field from an explicit `null`.
fn double_option<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(d).map(Some)
}

/// `PATCH /api/share/{id}` — change the password, expiry or scopes.
///
/// The slug is deliberately not editable. It is the whole address of a published
/// dashboard, and moving it would break every link already handed out; a new
/// address is a new link, which is what `POST` is for.
///
/// Changing or clearing the password ends every unlock token issued against the
/// old one, because a token remembers the password it was issued under.
async fn update_share(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SharePatch>,
) -> ApiResult<Json<Value>> {
    let link = authorize_share(&st, id, auth.user.id).await?;

    let password_hash = match body.password {
        Some(Some(pw)) if !pw.is_empty() => Some(hash_password_blocking(pw).await?),
        Some(_) => None,
        None => link.password_hash.clone(),
    };
    let expires_at = match body.expires_at {
        Some(value) => value,
        None => link.expires_at,
    };
    let scopes = match body.scopes.as_ref() {
        Some(raw) => json!(clean_share_scopes(Some(raw))?),
        None => link.scopes.clone(),
    };

    let updated = sqlx::query_as::<_, sites::ShareLink>(&format!(
        "UPDATE share_links SET password_hash = $2, expires_at = $3, scopes = $4
         WHERE id = $1 RETURNING {SHARE_COLS}"
    ))
    .bind(link.id)
    .bind(password_hash.as_deref())
    .bind(expires_at)
    .bind(&scopes)
    .fetch_optional(&st.pg)
    .await
    .map_err(Error::from)?
    .ok_or_else(|| Error::not_found("share link"))?;

    Ok(ok(share_json(
        &updated,
        st.cfg.dashboard_url.trim_end_matches('/'),
    )))
}

/// `DELETE /api/share/{id}` — revoke a published dashboard.
///
/// Immediate for unlock tokens too: a token names its link by id, and a link
/// that no longer exists resolves nothing.
async fn delete_share(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let link = authorize_share(&st, id, auth.user.id).await?;
    sites::delete_share_link(&st.pg, link.id).await?;
    Ok(ok(json!({ "revoked": link.id })))
}

// ---------------------------------------------------------------------------
// Share links: the public side
// ---------------------------------------------------------------------------

/// An unlock, as it is held in Redis under the digest of its token.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShareUnlock {
    /// The link this token opens, by id, so deleting the link ends it.
    link_id: Uuid,
    /// A digest of the password digest the token was issued under, so changing
    /// the password ends it.
    password_fingerprint: String,
    /// Belt and braces beside the Redis TTL.
    expires_at: DateTime<Utc>,
}

/// `mf:share:tok:<digest>`: an unlock token's record. The token itself is
/// never stored.
fn share_token_key(token_digest: &str) -> String {
    format!("mf:share:tok:{token_digest}")
}

/// What an unlock token remembers about the password it was issued under.
fn password_fingerprint(password_hash: Option<&str>) -> String {
    crypto::token_hash(password_hash.unwrap_or(""))
}

/// The live link an unlock token opens, when it still opens one.
async fn unlocked_link(st: &AppState, token: &str) -> Result<Option<sites::ShareLink>> {
    if !token.starts_with(SHARE_TOKEN_PREFIX) {
        return Ok(None);
    }
    let record: Option<ShareUnlock> = st
        .kv
        .get_json(&share_token_key(&crypto::token_hash(token)))
        .await?;
    let now = Utc::now();
    let Some(record) = record.filter(|r| r.expires_at > now) else {
        return Ok(None);
    };
    let Some(link) = load_share_link(&st.pg, record.link_id).await? else {
        return Ok(None);
    };
    let same_password =
        password_fingerprint(link.password_hash.as_deref()) == record.password_fingerprint;
    Ok((link.is_live(now) && same_password).then_some(link))
}

/// The public site whose public slug this is, while it is still public.
async fn public_site_by_slug(pg: &PgPool, slug: &str) -> Result<Option<Site>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sites WHERE \"public\" AND settings->>'public_slug' = $1 LIMIT 1",
    )
    .bind(slug.trim())
    .fetch_optional(pg)
    .await?;
    match id {
        Some(id) => Ok(Some(sites::get(pg, id).await?)),
        None => Ok(None),
    }
}

/// Resolve a `?share=` credential to the site it opens and what it may read.
///
/// Three forms, tried in this order: an unlock token (`mfs_…`), a share link's
/// slug, a public site's slug. Anything that does not resolve, has expired, or
/// was revoked reads as absent. A password-protected link presented by its slug
/// is `401`: the reader holds the right address and needs to unlock it.
pub async fn resolve_share(st: &AppState, credential: &str) -> Result<PublicSite> {
    let missing = || Error::not_found("dashboard");
    let credential = credential.trim();

    if credential.starts_with(SHARE_TOKEN_PREFIX) {
        let link = unlocked_link(st, credential).await?.ok_or_else(missing)?;
        return public_from_link(st, link).await;
    }

    if let Some(link) = load_share_link_by_slug(&st.pg, credential).await? {
        if !link.is_live(Utc::now()) {
            return Err(missing());
        }
        if link.password_hash.is_some() {
            return Err(Error::Unauthorized(
                "this shared dashboard is password protected; unlock it first".to_string(),
            ));
        }
        return public_from_link(st, link).await;
    }

    let site = public_site_by_slug(&st.pg, credential)
        .await?
        .ok_or_else(missing)?;
    Ok(PublicSite {
        site_id: site.site_id_u16(),
        site,
        link: None,
        scopes: SHARE_SCOPES.iter().map(|s| s.to_string()).collect(),
    })
}

/// A resolved link, with its site and its scopes.
async fn public_from_link(st: &AppState, link: sites::ShareLink) -> Result<PublicSite> {
    let site = sites::get(&st.pg, link.site_id)
        .await
        .map_err(|_| Error::not_found("dashboard"))?;
    Ok(PublicSite {
        site_id: site.site_id_u16(),
        site,
        scopes: share_scopes_of(&link.scopes),
        link: Some(link),
    })
}

/// The shared view's description of one slug.
///
/// Says nothing about the site behind a link until the reader may read it: a
/// locked or expired link reports `site: null` and no scopes.
async fn share_meta(st: &AppState, slug: &str, token: Option<&str>) -> Result<Value> {
    let slug = slug.trim();

    if let Some(link) = load_share_link_by_slug(&st.pg, slug).await? {
        let password_required = link.password_hash.is_some();
        let closed = |expired: bool| {
            json!({
                "slug": slug,
                "kind": "link",
                "site": Value::Null,
                "scopes": Vec::<String>::new(),
                "password_required": password_required,
                "unlocked": false,
                "expires_at": link.expires_at,
                "expired": expired,
            })
        };

        if !link.is_live(Utc::now()) {
            return Ok(closed(true));
        }
        let unlocked = match (password_required, token) {
            (false, _) => true,
            (true, Some(token)) => unlocked_link(st, token)
                .await?
                .is_some_and(|opened| opened.id == link.id),
            (true, None) => false,
        };
        if !unlocked {
            return Ok(closed(false));
        }

        let site = sites::get(&st.pg, link.site_id)
            .await
            .map_err(|_| Error::not_found("dashboard"))?;
        return Ok(json!({
            "slug": slug,
            "kind": "link",
            "site": shared_site_json(&site),
            "scopes": share_scopes_of(&link.scopes),
            "password_required": password_required,
            "unlocked": true,
            "expires_at": link.expires_at,
            "expired": false,
        }));
    }

    if let Some(site) = public_site_by_slug(&st.pg, slug).await? {
        return Ok(json!({
            "slug": slug,
            "kind": "public",
            "site": shared_site_json(&site),
            "scopes": SHARE_SCOPES,
            "password_required": false,
            "unlocked": true,
            "expires_at": Value::Null,
            "expired": false,
        }));
    }

    Err(Error::not_found("dashboard"))
}

/// What a password unlock sends.
#[derive(Debug, Deserialize)]
struct UnlockBody {
    #[serde(default)]
    password: String,
}

/// `POST /api/share/{slug}/unlock`: trade a link's password for a token.
///
/// Limited twice before the password is even looked at: per address, so one
/// client cannot walk many links, and per link, so many clients cannot share
/// the guessing on one. Both counters are incremented atomically before the
/// Argon2 check, so a parallel burst gets no more guesses than a patient
/// attacker. The check itself is Argon2id's own constant-time comparison, run
/// off the async runtime.
///
/// The token goes in `?share=` on every read the shared view makes, so the
/// password is sent once and never stored by the browser.
async fn unlock_share(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    Path(slug): Path<String>,
    Json(body): Json<UnlockBody>,
) -> ApiResult<Json<Value>> {
    let ip_key = format!(
        "share-unlock-ip:{}",
        crypto::visitor_hash(&st.cfg.derive_key("share-ip"), 0, &ip.to_string(), "")
    );
    if !st
        .kv
        .rate_limit(&ip_key, UNLOCK_IP_LIMIT, UNLOCK_WINDOW_SECS)
        .await?
    {
        return Err(Error::RateLimited {
            retry_after: u64::from(UNLOCK_WINDOW_SECS),
        }
        .into());
    }

    let link = load_share_link_by_slug(&st.pg, &slug)
        .await?
        .filter(|l| l.is_live(Utc::now()))
        .ok_or_else(|| Error::not_found("dashboard"))?;

    if !st
        .kv
        .rate_limit(
            &format!("share-unlock:{}", link.id),
            UNLOCK_LINK_LIMIT,
            UNLOCK_WINDOW_SECS,
        )
        .await?
    {
        return Err(Error::RateLimited {
            retry_after: u64::from(UNLOCK_WINDOW_SECS),
        }
        .into());
    }

    if let Some(stored) = link.password_hash.clone() {
        if !verify_password_blocking(body.password, stored).await? {
            return Err(Error::Unauthorized("that password does not open this link".into()).into());
        }
    }

    let token = format!(
        "{SHARE_TOKEN_PREFIX}{}",
        crypto::random_token(SHARE_TOKEN_BYTES)
    );
    let ttl_end = Utc::now() + chrono::Duration::seconds(i64::from(SHARE_TOKEN_TTL_SECS));
    // Never outlives the link it opens.
    let expires_at = link.expires_at.map_or(ttl_end, |end| end.min(ttl_end));
    let record = ShareUnlock {
        link_id: link.id,
        password_fingerprint: password_fingerprint(link.password_hash.as_deref()),
        expires_at,
    };
    st.kv
        .set_json(
            &share_token_key(&crypto::token_hash(&token)),
            &record,
            Some(SHARE_TOKEN_TTL_SECS),
        )
        .await?;

    Ok(ok(json!({
        "token": token,
        "expires_at": expires_at,
        "param": "share",
    })))
}

// ---------------------------------------------------------------------------
// Share slugs
// ---------------------------------------------------------------------------

/// The public slug stored on a site, when it has one.
fn stored_public_slug(site: &Site) -> Option<String> {
    site.settings
        .get(PUBLIC_SLUG_FIELD)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The site's public slug, issuing one if it has none. Returns the slug and the
/// site row as it now stands.
async fn ensure_public_slug(pg: &PgPool, site: &Site) -> Result<(String, Site)> {
    if let Some(existing) = stored_public_slug(site) {
        return Ok((existing, site.clone()));
    }

    let slug = fresh_slug(pg).await?;
    let mut settings = site.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    if let Some(object) = settings.as_object_mut() {
        object.insert(PUBLIC_SLUG_FIELD.to_string(), json!(slug));
    }
    let updated = sites::update_settings(pg, site.id, &settings).await?;
    Ok((slug, updated))
}

/// Is a slug already an address, as a link or as a public site?
async fn slug_taken(pg: &PgPool, slug: &str) -> Result<bool> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM share_links WHERE slug = $1)
             OR EXISTS (SELECT 1 FROM sites WHERE settings->>'public_slug' = $1)",
    )
    .bind(slug)
    .fetch_one(pg)
    .await?;
    Ok(taken)
}

/// A generated slug nothing else is using.
async fn fresh_slug(pg: &PgPool) -> Result<String> {
    for _ in 0..6 {
        let candidate = crypto::random_token(SHARE_SLUG_BYTES).to_lowercase();
        if candidate.starts_with(SHARE_TOKEN_PREFIX) {
            continue;
        }
        if !slug_taken(pg, &candidate).await? {
            return Ok(candidate);
        }
    }
    Err(Error::Conflict(
        "could not find a free share address".into(),
    ))
}

/// Validate a requested scope list against [`SHARE_SCOPES`].
///
/// An unknown scope is refused rather than dropped, for the same reason
/// `routes::keys` refuses one: a link its owner believes shows more, or less,
/// than it does is found out about at the worst moment.
fn clean_share_scopes(requested: Option<&Value>) -> Result<Vec<String>> {
    let Some(raw) = requested else {
        return Ok(vec![SHARE_DEFAULT_SCOPE.to_string()]);
    };
    let Some(items) = raw.as_array() else {
        return Err(Error::validation("scopes", "must be a list of scopes"));
    };

    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let scope = item
            .as_str()
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
        if !SHARE_SCOPES.contains(&scope.as_str()) {
            return Err(Error::validation(
                "scopes",
                format!(
                    "`{scope}` is not a share scope; expected one of {}",
                    SHARE_SCOPES.join(", ")
                ),
            ));
        }
        if !out.contains(&scope) {
            out.push(scope);
        }
    }
    if out.is_empty() {
        return Err(Error::validation(
            "scopes",
            "a link with no scopes would show nothing; omit the field for the overview",
        ));
    }
    Ok(out)
}

/// The share scopes a stored link grants.
///
/// Whatever is not in [`SHARE_SCOPES`] is ignored, so a row written before the
/// vocabulary was enforced can only ever read as narrower, never wider. An empty
/// result is the overview alone: a link never widens by saying nothing.
fn share_scopes_of(stored: &Value) -> Vec<String> {
    let known: Vec<String> = crate::extract::string_list(stored)
        .into_iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| SHARE_SCOPES.contains(&s.as_str()))
        .collect();
    if known.is_empty() {
        vec![SHARE_DEFAULT_SCOPE.to_string()]
    } else {
        known
    }
}

// ---------------------------------------------------------------------------
// Shaping and validation
// ---------------------------------------------------------------------------

/// One site on the wire. Never carries the sealed ingest key.
fn site_json(site: &Site, role: Option<&str>) -> Value {
    let mut settings = site.settings.clone();
    if let Some(object) = settings.as_object_mut() {
        object.remove(INGEST_KEY_FIELD);
    }

    json!({
        "id": site.id,
        "site_id": site.site_id_u16(),
        "org_id": site.org_id,
        "domain": site.domain,
        "name": site.name,
        "timezone": site.timezone,
        "public": site.public,
        "salt_rotated_at": site.salt_rotated_at,
        "created_at": site.created_at,
        "settings": settings,
        "role": role,
        "can_write": role.map(|r| require_at_least(r, MEMBER).is_ok()),
    })
}

/// One share link on the wire. Never carries the password digest.
///
/// `has_password` and `password_protected` say the same thing; the first is what
/// the settings screen reads, the second is kept for existing API callers.
fn share_json(link: &sites::ShareLink, dashboard_url: &str) -> Value {
    json!({
        "id": link.id,
        "site_id": link.site_id,
        "slug": link.slug,
        "url": format!("{dashboard_url}/share/{}", link.slug),
        "has_password": link.password_hash.is_some(),
        "password_protected": link.password_hash.is_some(),
        "expires_at": link.expires_at,
        "live": link.is_live(Utc::now()),
        "scopes": share_scopes_of(&link.scopes),
    })
}

/// The site behind a shared view: what the page header needs and nothing else.
/// No UUID, no organisation, no settings.
fn shared_site_json(site: &Site) -> Value {
    json!({
        "site_id": site.site_id_u16(),
        "domain": site.domain,
        "name": site.name,
        "timezone": site.timezone,
    })
}

/// Merge a settings patch over the stored document, one key at a time.
///
/// Shallow on purpose. A deep merge would make it impossible to remove a nested
/// key, and the settings document is one level of switches, not a tree.
fn merge_settings(current: &Value, patch: &Value) -> Result<Value> {
    let Some(incoming) = patch.as_object() else {
        return Err(Error::validation("settings", "must be a JSON object"));
    };

    let mut merged = current.clone();
    if !merged.is_object() {
        merged = json!({});
    }
    let Some(target) = merged.as_object_mut() else {
        return Err(Error::internal("settings did not become an object"));
    };

    for (key, value) in incoming {
        // The sealed ingest key is not a setting and is never writable from a
        // request body. Allowing it would let a caller install a key of their
        // own choosing and sign whatever they liked. The public slug is an
        // address the server issues: a chosen one could claim another site's.
        if key == INGEST_KEY_FIELD || key == PUBLIC_SLUG_FIELD {
            continue;
        }
        if value.is_null() {
            target.remove(key);
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(merged)
}

/// Reduce whatever the operator typed to a bare hostname.
///
/// People paste `https://example.com/docs/` into a domain field constantly, so
/// the scheme, the path, the port and a leading `www.` are all stripped rather
/// than rejected. What is refused is a value that is not a hostname at all.
pub fn clean_domain(raw: &str) -> Result<String> {
    let mut host = raw.trim().to_lowercase();
    if let Some(rest) = host.split("://").nth(1) {
        host = rest.to_string();
    }
    host = host
        .split('/')
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_matches('.')
        .to_string();
    if let Some(stripped) = host.strip_prefix("www.") {
        host = stripped.to_string();
    }

    let plausible = !host.is_empty()
        && host.len() <= MAX_DOMAIN
        && host.contains('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !host.starts_with('-')
        && !host.ends_with('-');

    if !plausible {
        return Err(Error::validation(
            "domain",
            "must be a hostname such as example.com",
        ));
    }
    Ok(host)
}

/// Refuse a timezone `chrono-tz` does not know.
///
/// A site's whole calendar — the day a report covers, the midnight its visitor
/// salt rotates at — is resolved in this zone, so a typo here would quietly
/// shift every number by hours.
pub fn clean_timezone(raw: &str) -> Result<String> {
    let name = raw.trim();
    name.parse::<chrono_tz::Tz>()
        .map_err(|_| Error::validation("timezone", format!("`{name}` is not an IANA timezone")))?;
    Ok(name.to_string())
}

/// Bound a caller-chosen share slug to what can sit safely in a URL.
///
/// Two shapes are reserved even though the alphabet allows them: a UUID, which
/// `GET /api/share/{link}` reads as a management id, and the unlock-token
/// prefix, which `?share=` reads as a token.
fn clean_slug(raw: &str) -> Result<String> {
    let slug = raw.trim().to_lowercase();
    let usable = (3..=64).contains(&slug.len())
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !usable {
        return Err(Error::validation(
            "slug",
            "must be 3 to 64 characters of letters, digits, hyphens or underscores",
        ));
    }
    if slug.parse::<Uuid>().is_ok() || slug.starts_with(SHARE_TOKEN_PREFIX) {
        return Err(Error::validation(
            "slug",
            "is a reserved shape; choose another",
        ));
    }
    Ok(slug)
}

/// The first instant of the current calendar month, in a site's own zone.
///
/// Falls back to UTC for a zone that will not parse, which cannot happen through
/// [`clean_timezone`] but can happen to a row written before that check existed.
fn month_start(timezone: &str) -> DateTime<Utc> {
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
    let now = Utc::now().with_timezone(&tz);
    now.date_naive()
        .with_day(1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|naive| tz.from_local_datetime(&naive).single())
        .map(|dt| dt.with_timezone(&Utc))
        // A month that has no valid midnight in this zone is a daylight-saving
        // artefact; falling back a day keeps the window slightly wide rather
        // than reporting nothing.
        .unwrap_or_else(|| Utc::now() - chrono::Duration::days(30))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_url_becomes_a_hostname() {
        assert_eq!(
            clean_domain("https://www.Example.com/docs/?a=1").unwrap(),
            "example.com"
        );
        assert_eq!(clean_domain("  example.co.uk  ").unwrap(), "example.co.uk");
        assert_eq!(clean_domain("example.com:8443").unwrap(), "example.com");
        assert!(
            clean_domain("localhost").is_err(),
            "a bare label is not a site"
        );
        assert!(clean_domain("").is_err());
        assert!(clean_domain("exa mple.com").is_err());
    }

    #[test]
    fn timezones_are_checked_against_the_real_database() {
        assert_eq!(clean_timezone(" Europe/Lisbon ").unwrap(), "Europe/Lisbon");
        assert!(clean_timezone("Mars/Olympus").is_err());
    }

    #[test]
    fn share_slugs_stay_url_safe() {
        assert_eq!(clean_slug(" My-Docs_1 ").unwrap(), "my-docs_1");
        assert!(clean_slug("ab").is_err());
        assert!(clean_slug("has/slash").is_err());
    }

    #[test]
    fn a_slug_can_never_be_read_as_an_id_or_a_token() {
        // `GET /api/share/{link}` reads a UUID as a management id.
        assert!(clean_slug("0190a5f2-3c4d-7e8f-9a0b-1c2d3e4f5a6b").is_err());
        assert!(clean_slug("0190a5f23c4d7e8f9a0b1c2d3e4f5a6b").is_err());
        // `?share=` reads this prefix as an unlock token.
        assert!(clean_slug("mfs_docs").is_err());
        assert!(clean_slug("MFS_docs").is_err());
    }

    #[test]
    fn share_scopes_are_checked_against_the_vocabulary() {
        assert_eq!(clean_share_scopes(None).unwrap(), vec!["overview"]);
        assert_eq!(
            clean_share_scopes(Some(&json!([" Pages ", "pages", "agents"]))).unwrap(),
            vec!["pages", "agents"]
        );
        assert!(clean_share_scopes(Some(&json!(["stats:read"]))).is_err());
        assert!(clean_share_scopes(Some(&json!(["*"]))).is_err());
        assert!(clean_share_scopes(Some(&json!([]))).is_err());
        assert!(clean_share_scopes(Some(&json!("overview"))).is_err());
    }

    #[test]
    fn a_stored_link_only_ever_reads_as_narrower() {
        // A row from before the vocabulary was enforced cannot widen itself.
        assert_eq!(
            share_scopes_of(&json!(["*", "stats:read"])),
            vec!["overview"]
        );
        assert_eq!(share_scopes_of(&json!(null)), vec!["overview"]);
        assert_eq!(
            share_scopes_of(&json!(["pages", "visitors", "citation_gap"])),
            vec!["pages", "citation_gap"]
        );
    }

    #[test]
    fn a_settings_patch_can_never_write_the_public_slug() {
        let stored = json!({ PUBLIC_SLUG_FIELD: "issued" });
        let patch = json!({ PUBLIC_SLUG_FIELD: "another-sites-slug" });
        let merged = merge_settings(&stored, &patch).unwrap();
        assert_eq!(merged[PUBLIC_SLUG_FIELD], json!("issued"));

        let cleared = merge_settings(&stored, &json!({ PUBLIC_SLUG_FIELD: null })).unwrap();
        assert_eq!(cleared[PUBLIC_SLUG_FIELD], json!("issued"));
    }

    #[test]
    fn an_unlock_token_dies_with_the_password_it_was_issued_under() {
        let before = password_fingerprint(Some("$argon2id$v=19$m=19456,t=2,p=1$aaa$bbb"));
        let after = password_fingerprint(Some("$argon2id$v=19$m=19456,t=2,p=1$ccc$ddd"));
        let cleared = password_fingerprint(None);
        assert_ne!(before, after);
        assert_ne!(before, cleared);
        // And the record never holds the digest itself.
        assert!(!before.contains("argon2"));
    }

    #[test]
    fn a_settings_patch_can_never_write_the_ingest_key() {
        let stored = json!({ INGEST_KEY_FIELD: "sealed", "exclude": ["/admin"] });
        let patch = json!({ INGEST_KEY_FIELD: "attacker-chosen", "exclude": ["/private"] });
        let merged = merge_settings(&stored, &patch).unwrap();

        assert_eq!(merged[INGEST_KEY_FIELD], json!("sealed"));
        assert_eq!(merged["exclude"], json!(["/private"]));
    }

    #[test]
    fn a_null_in_a_settings_patch_removes_the_key() {
        let stored = json!({ "respect_dnt": true });
        let merged = merge_settings(&stored, &json!({ "respect_dnt": null })).unwrap();
        assert!(merged.get("respect_dnt").is_none());
    }

    #[test]
    fn a_site_never_serialises_its_sealed_key() {
        let site = Site {
            id: Uuid::nil(),
            site_id: 3,
            org_id: Uuid::nil(),
            domain: "example.com".into(),
            name: "Example".into(),
            timezone: "UTC".into(),
            public: false,
            salt_rotated_at: None,
            created_at: Utc::now(),
            settings: json!({ INGEST_KEY_FIELD: "sealed", "respect_dnt": true }),
        };
        let shaped = site_json(&site, Some(VIEWER));

        assert!(shaped["settings"].get(INGEST_KEY_FIELD).is_none());
        assert_eq!(shaped["settings"]["respect_dnt"], json!(true));
        assert_eq!(shaped["can_write"], json!(false));
    }
}
