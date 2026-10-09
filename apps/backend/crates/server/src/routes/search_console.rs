//! Google Search Console: `/api/search-console*` to connect a site, and
//! `/api/stats/search/*` to read what it reports.
//!
//! Off until the operator sets `MICAFORGE_SEARCH_CONSOLE_CLIENT_ID` and
//! `_SECRET`. Until then the status route says `configured: false`, connecting
//! answers `503 search_console_not_configured`, and nothing calls Google.
//! (Why a client of its own rather than the sign-in one: see
//! [`micaforge_core::Config::search_console`].)
//!
//! ## Connecting a site
//!
//! 1. `POST /api/search-console/connect?site=` (site admin) mints a state and
//!    a PKCE verifier (RFC 7636, `S256`) and answers the Google consent address
//!    for the dashboard to open. The state is **signed and expiring**: an HMAC
//!    over the site, the admin and a nonce, valid ten minutes. The verifier is
//!    kept in Valkey under a keyed digest of the nonce, for the same ten
//!    minutes. The consent asks for one scope, `webmasters.readonly`, with
//!    `access_type=offline` and `prompt=consent` so Google issues a refresh
//!    token every time.
//! 2. `GET /api/search-console/callback` is where Google sends the browser
//!    back. It refuses unless the state's signature holds, it has not expired,
//!    it names the admin whose session the browser carries (so nobody can
//!    plant their grant on someone else's site), that admin still holds the
//!    site, and its verifier record is still there, which it then consumes:
//!    a state works once. The code is exchanged with the verifier, so a code
//!    copied from the redirect is useless without it. The refresh token is
//!    sealed with the site's id as additional data
//!    (`micaforge_jobs::search_console::seal_token`) and stored; the access
//!    token is dropped. Every outcome lands on the dashboard's Search settings
//!    with one code (`?gsc=connected`, or `?gsc_error=state|denied|forbidden|
//!    scope|disabled|provider`).
//! 3. `GET /api/search-console/properties?site=` lists the Search Console
//!    properties the Google account can read (domain and URL-prefix
//!    properties), and `PUT /api/search-console/property?site=` chooses one,
//!    which must be on that list. Choosing starts the first sync at once.
//!
//! `DELETE /api/search-console?site=` asks Google to revoke the token (best
//! effort) and deletes the connection. The rows already synced stay, keyed by
//! property, and reappear if the same property is connected again.
//!
//! Nothing here is a sign-in. The grant belongs to a site, whoever approved
//! it, no profile is requested, and `oauth_identities` is never touched.
//!
//! ## Reading
//!
//! The six `/api/stats/search/*` reads take the standard envelope and answer
//! from `micaforge_query::search`, scoped to the property the site chose. They
//! are analytics reads, so an API key with `stats:read` may call them; the
//! management routes above refuse keys, because connecting an account is a
//! person's act.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::Utc;
use micaforge_core::{crypto, Config, Error, Result};
use micaforge_jobs::search_console::{self as gsc, google, plan, Endpoints, GoogleError};
use micaforge_jobs::Kind;
use micaforge_query::search::{self, DateWindow, ListQuery, QueryFilter, SearchScope, SortBy};
use micaforge_store::models::search_console::{self as store, Connection};
use micaforge_store::models::sites::{self, Site};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit::{self, Entry};
use crate::error::ApiError;
use crate::extract::{AuthMethod, AuthUser, Envelope, SiteAccess};
use crate::response::{ok, ok_meta};
use crate::routes::oauth_sign_in::pkce_challenge;
use crate::routes::orgs::{require_at_least, ADMIN, MEMBER, VIEWER};
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// How long a started connection stays usable.
const FLOW_TTL_SECS: u32 = 10 * 60;

/// The key-derivation purpose of the state signature.
const STATE_PURPOSE: &str = "search-console-state";

/// "Sync now" presses one site may make per window.
const SYNC_NOW_LIMIT: u32 = 4;

/// That window, in seconds.
const SYNC_NOW_WINDOW: u32 = 60 * 60;

/// Rows a dashboard list shows when the caller did not ask.
const DEFAULT_ROWS: u32 = 50;

/// Every route. Paths are absolute.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/search-console", get(status).delete(disconnect))
        .route("/api/search-console/connect", post(connect))
        .route("/api/search-console/callback", get(callback))
        .route("/api/search-console/properties", get(properties))
        .route("/api/search-console/property", put(choose_property))
        .route("/api/search-console/sync", post(sync_now))
        .route("/api/stats/search/overview", get(overview))
        .route("/api/stats/search/timeseries", get(timeseries))
        .route("/api/stats/search/queries", get(queries))
        .route("/api/stats/search/pages", get(pages))
        .route("/api/stats/search/countries", get(countries))
        .route("/api/stats/search/devices", get(devices))
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The sentence every surface says when Search Console is off.
pub fn not_configured() -> String {
    "Google Search Console is not configured on this server. The operator sets \
     MICAFORGE_SEARCH_CONSOLE_CLIENT_ID and MICAFORGE_SEARCH_CONSOLE_CLIENT_SECRET to switch it on."
        .to_string()
}

/// The `503` a connect gets when Search Console is off.
fn not_configured_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": { "code": "search_console_not_configured", "message": not_configured() } })),
    )
        .into_response()
}

/// The address Google sends the browser back to. Registered with Google
/// exactly as written here, so the status route shows it to admins.
pub fn redirect_uri(cfg: &Config) -> String {
    format!("{}/api/search-console/callback", cfg.public_url)
}

// ---------------------------------------------------------------------------
// The signed state
// ---------------------------------------------------------------------------

/// What the state carries. Short keys: it rides in a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StateClaims {
    /// The site being connected.
    s: Uuid,
    /// The admin who started it.
    u: Uuid,
    /// The single-use nonce the verifier is stored under.
    n: String,
    /// Expiry, Unix seconds.
    e: i64,
}

/// `base64url(claims).hmac`. The HMAC is keyed off the server secret for this
/// purpose alone, so a state cannot be forged or replayed across purposes.
fn sign_state(cfg: &Config, claims: &StateClaims) -> Result<String> {
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?);
    let mac = crypto::keyed_digest(&cfg.derive_key(STATE_PURPOSE), &[&body]);
    Ok(format!("{body}.{mac}"))
}

/// The claims of a state whose signature holds and which has not expired.
/// `None` for every way it can fail, on purpose.
fn verify_state(cfg: &Config, raw: &str, now: i64) -> Option<StateClaims> {
    let (body, mac) = raw.split_once('.')?;
    let expected = crypto::keyed_digest(&cfg.derive_key(STATE_PURPOSE), &[body]);
    if !crypto::digests_match(&expected, mac) {
        return None;
    }
    let claims: StateClaims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
    (claims.e > now).then_some(claims)
}

/// The verifier record, as Valkey holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Flow {
    verifier: String,
    site: Uuid,
    user: Uuid,
}

/// `mf:gsc:flow:<keyed digest of the nonce>`.
fn flow_key(cfg: &Config, nonce: &str) -> String {
    format!(
        "mf:gsc:flow:{}",
        crypto::keyed_digest(&cfg.derive_key("search-console-flow"), &[nonce])
    )
}

/// Google's consent address, with every parameter the flow needs.
fn authorize_url(
    endpoints: &Endpoints,
    client_id: &str,
    redirect: &str,
    state: &str,
    challenge: &str,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("scope", google::SCOPE)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        // A refresh token, every time: the sync runs without the person.
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .finish();
    format!("{}?{query}", endpoints.authorize)
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// A connection on the wire. Never the token.
fn connection_json(c: &Connection) -> Value {
    let now = Utc::now();
    json!({
        "status": c.status,
        "property": c.property,
        "connected_by": c.connected_by,
        "connected_at": c.created_at,
        "backfill_until": c.backfill_until,
        "backfilled_from": c.backfilled_from,
        "synced_through": c.synced_through,
        "backfill_done": matches!((c.backfilled_from, c.backfill_until), (Some(from), Some(until)) if from <= until),
        "syncing": c.lease_until.is_some_and(|until| until > now),
        "last_sync_started_at": c.last_sync_started_at,
        "last_sync_finished_at": c.last_sync_finished_at,
        "last_error": c.last_error,
        "last_error_at": c.last_error_at,
        "rows_synced": c.rows_synced,
    })
}

/// `GET /api/search-console?site=`
async fn status(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let conn = store::find(&st.pg, access.site.id).await?;
    Ok(ok(json!({
        "configured": st.cfg.search_console.is_some(),
        "redirect_uri": redirect_uri(&st.cfg),
        "can_manage": require_at_least(access.role.as_str(), ADMIN).is_ok(),
        "connection": conn.as_ref().map(connection_json),
    })))
}

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

/// `POST /api/search-console/connect?site=` → `{ url }`
async fn connect(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Response> {
    auth.require_session()?;
    require_at_least(access.role.as_str(), ADMIN)?;
    let Some(client) = st.cfg.search_console.as_ref() else {
        return Ok(not_configured_response());
    };

    let nonce = crypto::random_token(24);
    let verifier = crypto::random_token(32);
    let flow = Flow {
        verifier: verifier.clone(),
        site: access.site.id,
        user: auth.user.id,
    };
    st.kv
        .set_json(&flow_key(&st.cfg, &nonce), &flow, Some(FLOW_TTL_SECS))
        .await?;
    let claims = StateClaims {
        s: access.site.id,
        u: auth.user.id,
        n: nonce,
        e: Utc::now().timestamp() + i64::from(FLOW_TTL_SECS),
    };
    let state = sign_state(&st.cfg, &claims)?;
    let url = authorize_url(
        &Endpoints::google(),
        &client.client_id,
        &redirect_uri(&st.cfg),
        &state,
        &pkce_challenge(&verifier),
    );
    Ok(ok(json!({ "url": url })).into_response())
}

/// What Google sends back.
#[derive(Debug, Default, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Why a connection was turned away. The wire code is all the browser hears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The state was forged, expired, used, or another person's.
    State,
    /// The person cancelled at Google.
    Denied,
    /// The admin no longer holds the site.
    Forbidden,
    /// The person did not grant the Search Console scope.
    Scope,
    /// Search Console is not configured.
    Disabled,
    /// Google answered badly, or not at all.
    Provider,
}

impl Refusal {
    fn code(self) -> &'static str {
        match self {
            Refusal::State => "state",
            Refusal::Denied => "denied",
            Refusal::Forbidden => "forbidden",
            Refusal::Scope => "scope",
            Refusal::Disabled => "disabled",
            Refusal::Provider => "provider",
        }
    }
}

/// `GET /api/search-console/callback`
async fn callback(
    State(st): State<AppState>,
    auth: std::result::Result<AuthUser, ApiError>,
    Query(q): Query<CallbackQuery>,
) -> Redirect {
    let returned = q
        .state
        .as_deref()
        .and_then(|raw| verify_state(&st.cfg, raw, Utc::now().timestamp()));
    let landing = |site: Option<&Site>, query: &str| {
        let path = match site {
            Some(site) => format!("/app/{}/search?tab=settings&{query}", site.site_id),
            None => format!("/app?{query}"),
        };
        Redirect::to(&format!("{}{path}", st.cfg.dashboard_url))
    };

    let site = match &returned {
        Some(claims) => sites::get(&st.pg, claims.s).await.ok(),
        None => None,
    };
    match finish_connect(&st, auth.ok(), returned, &q).await {
        Ok(()) => landing(site.as_ref(), "gsc=connected"),
        Err(refusal) => {
            tracing::info!(
                reason = refusal.code(),
                "a Search Console connection was refused"
            );
            landing(site.as_ref(), &format!("gsc_error={}", refusal.code()))
        }
    }
}

/// The checks a callback passes before any store or Google is asked:
/// Google's own error, a state whose signature held, and a browser session
/// belonging to the admin the state names.
fn admit(
    auth: Option<&AuthUser>,
    claims: Option<StateClaims>,
    q: &CallbackQuery,
) -> std::result::Result<StateClaims, Refusal> {
    if let Some(error) = q.error.as_deref() {
        return Err(if error == "access_denied" {
            Refusal::Denied
        } else {
            Refusal::Provider
        });
    }
    let claims = claims.ok_or(Refusal::State)?;
    let auth = auth.ok_or(Refusal::State)?;
    if !matches!(auth.method, AuthMethod::Session(_)) || auth.user.id != claims.u {
        return Err(Refusal::State);
    }
    Ok(claims)
}

/// Everything between Google's redirect and a stored grant.
async fn finish_connect(
    st: &AppState,
    auth: Option<AuthUser>,
    claims: Option<StateClaims>,
    q: &CallbackQuery,
) -> std::result::Result<(), Refusal> {
    let claims = admit(auth.as_ref(), claims, q)?;
    let auth = auth.ok_or(Refusal::State)?;
    let site = crate::routes::sites::authorize_site(&st.pg, claims.s, auth.user.id, ADMIN)
        .await
        .map_err(|_| Refusal::Forbidden)?;
    let client = st.cfg.search_console.as_ref().ok_or(Refusal::Disabled)?;

    // Consume the verifier: the state works once.
    let key = flow_key(&st.cfg, &claims.n);
    let seen = st
        .kv
        .get_snapshot::<Flow>(&key)
        .await
        .map_err(|_| Refusal::Provider)?
        .ok_or(Refusal::State)?;
    if seen.value.site != claims.s || seen.value.user != claims.u {
        return Err(Refusal::State);
    }
    if !st
        .kv
        .swap_json(&key, &seen, None)
        .await
        .map_err(|_| Refusal::Provider)?
    {
        return Err(Refusal::State);
    }

    let code = q
        .code
        .as_deref()
        .filter(|c| !c.is_empty())
        .ok_or(Refusal::Provider)?;
    let grant = google::exchange_code(
        &st.http,
        &Endpoints::google(),
        client,
        code,
        &seen.value.verifier,
        &redirect_uri(&st.cfg),
    )
    .await
    .map_err(|e| match e {
        GoogleError::Forbidden => Refusal::Scope,
        _ => Refusal::Provider,
    })?;

    let failed = |e: Error| {
        tracing::warn!(error = %e, "a Search Console grant could not be stored");
        Refusal::Provider
    };
    let sealed = gsc::seal_token(&st.cfg, site.id, &grant.refresh_token).map_err(failed)?;
    store::upsert_grant(&st.pg, site.id, auth.user.id, &sealed)
        .await
        .map_err(failed)?;
    audit::record(
        st,
        &auth,
        Entry::new("search_console.connected").site(&site),
    )
    .await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// The connection, its opened refresh token, and a fresh access token.
///
/// A refresh token Google refuses marks the connection revoked here too, so
/// the settings screen and the next sync agree.
async fn google_session(st: &AppState, site: &Site) -> Result<(Connection, String, String)> {
    let client = st
        .cfg
        .search_console
        .as_ref()
        .ok_or_else(|| Error::Conflict(not_configured()))?;
    let conn = store::get(&st.pg, site.id).await?;
    let sealed = conn.refresh_token_sealed.clone().ok_or_else(|| {
        Error::Conflict(
            "Google no longer accepts this connection; connect Search Console again".into(),
        )
    })?;
    let refresh_token = gsc::open_token(&st.cfg, site.id, &sealed)?;
    match google::refresh(&st.http, &Endpoints::google(), client, &refresh_token).await {
        Ok(access) => Ok((conn, refresh_token, access)),
        Err(GoogleError::Revoked) => {
            store::mark_revoked(&st.pg, site.id, &GoogleError::Revoked.sentence()).await?;
            Err(Error::Conflict(GoogleError::Revoked.sentence()))
        }
        Err(e) => Err(e.into()),
    }
}

/// `GET /api/search-console/properties?site=`
async fn properties(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    require_at_least(access.role.as_str(), ADMIN)?;
    let (_, _, token) = google_session(&st, &access.site).await?;
    let list = google::list_properties(&st.http, &Endpoints::google(), &token)
        .await
        .map_err(Error::from)?;
    Ok(ok(list))
}

/// The property to read.
#[derive(Debug, Deserialize)]
struct PropertyBody {
    property: String,
}

/// `PUT /api/search-console/property?site=`
async fn choose_property(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
    Json(body): Json<PropertyBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    require_at_least(access.role.as_str(), ADMIN)?;
    let wanted = body.property.trim().to_string();
    if !google::is_property(&wanted) {
        return Err(Error::validation(
            "property",
            "must be a Search Console property, such as sc-domain:example.com or https://example.com/",
        )
        .into());
    }
    let (before, _, token) = google_session(&st, &access.site).await?;
    let readable = google::list_properties(&st.http, &Endpoints::google(), &token)
        .await
        .map_err(Error::from)?;
    if !readable.iter().any(|p| p.site_url == wanted) {
        return Err(Error::validation(
            "property",
            "the connected Google account cannot read that property",
        )
        .into());
    }

    let today = plan::google_today(Utc::now());
    let conn =
        store::set_property(&st.pg, access.site.id, &wanted, plan::backfill_until(today)).await?;
    micaforge_jobs::enqueue_now(
        &st.pg,
        Kind::SyncSearchConsole,
        &gsc::Payload {
            site: Some(access.site.id),
        },
    )
    .await?;
    audit::record(
        &st,
        &auth,
        Entry::new("search_console.property_chosen")
            .site(&access.site)
            .before(json!({ "property": before.property }))
            .after(json!({ "property": wanted })),
    )
    .await;
    Ok(ok(connection_json(&conn)))
}

// ---------------------------------------------------------------------------
// Sync now, and disconnect
// ---------------------------------------------------------------------------

/// `POST /api/search-console/sync?site=`
async fn sync_now(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    require_at_least(access.role.as_str(), MEMBER)?;
    if st.cfg.search_console.is_none() {
        return Err(Error::Conflict(not_configured()).into());
    }
    let conn = store::get(&st.pg, access.site.id).await?;
    if !conn.is_syncable() {
        return Err(Error::Conflict(
            "Search Console is not syncing for this site: choose a property, or connect again"
                .into(),
        )
        .into());
    }
    let key = format!("gsc-sync:{}", access.site.id);
    if !st
        .kv
        .rate_limit(&key, SYNC_NOW_LIMIT, SYNC_NOW_WINDOW)
        .await?
    {
        return Err(Error::RateLimited {
            retry_after: u64::from(SYNC_NOW_WINDOW),
        }
        .into());
    }
    micaforge_jobs::enqueue_now(
        &st.pg,
        Kind::SyncSearchConsole,
        &gsc::Payload {
            site: Some(access.site.id),
        },
    )
    .await?;
    Ok(ok(json!({ "queued": true })))
}

/// `DELETE /api/search-console?site=`
async fn disconnect(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    require_at_least(access.role.as_str(), ADMIN)?;
    let Some(removed) = store::delete(&st.pg, access.site.id).await? else {
        return Err(Error::not_found("Search Console connection").into());
    };
    // Best effort: the token is gone from here either way, and Google
    // forgets an unused refresh token on its own after six months.
    if let Some(sealed) = removed.refresh_token_sealed.as_deref() {
        match gsc::open_token(&st.cfg, access.site.id, sealed) {
            Ok(token) => {
                if let Err(e) = google::revoke(&st.http, &Endpoints::google(), &token).await {
                    tracing::info!(reason = %e.sentence(), "google did not confirm the revocation");
                }
            }
            Err(e) => {
                tracing::info!(error = %e, "a stored Search Console token could not be opened to revoke")
            }
        }
    }
    audit::record(
        &st,
        &auth,
        Entry::new("search_console.disconnected")
            .site(&access.site)
            .before(json!({ "property": removed.property })),
    )
    .await;
    Ok(ok(json!({ "disconnected": true })))
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// The property a site reads, or `404` with the reason.
pub(crate) async fn chosen_property(st: &AppState, site: &Site) -> Result<String> {
    store::find(&st.pg, site.id)
        .await?
        .and_then(|c| c.property)
        .ok_or_else(|| Error::not_found("a Search Console property for this site; a site admin connects one on the Search screen"))
}

/// The narrowing and ranking the list reads accept, besides the envelope.
///
/// `page` is the envelope's page number, so a page's path is `path`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ListParams {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    sort: Option<String>,
}

impl ListParams {
    /// Checked into the query crate's terms. `explicit_limit` is false when
    /// the envelope's limit is its own default, which a dashboard list
    /// shortens to [`DEFAULT_ROWS`].
    pub(crate) fn list(
        self,
        env: &micaforge_core::QueryEnvelope,
        explicit_limit: bool,
    ) -> Result<ListQuery> {
        Ok(ListQuery {
            filter: QueryFilter {
                query: self.q,
                page: self.path,
                country: self.country,
                device: self.device,
            }
            .checked()?,
            sort: SortBy::parse(self.sort.as_deref())?,
            limit: if explicit_limit {
                env.limit
            } else {
                DEFAULT_ROWS
            },
            page: env.page,
        })
    }
}

/// True when the request named a `limit`.
fn asked_limit(raw: Option<&str>) -> bool {
    raw.is_some_and(|q| q.split('&').any(|pair| pair.starts_with("limit=")))
}

/// What every read needs: the property, the days and the scope meta.
fn meta(property: &str, window: DateWindow) -> Value {
    json!({ "property": property, "window": window })
}

/// `GET /api/stats/search/overview`
async fn overview(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> ApiResult<Json<Value>> {
    let property = chosen_property(&st, &access.site).await?;
    let scope = SearchScope {
        site: access.site_id,
        property: &property,
    };
    let window = DateWindow::of_range(&env.range);
    let previous = search::previous_window(&env.range, env.compare);
    let report = search::overview(&st.ch, scope, window, previous).await?;
    let mut data = serde_json::to_value(&report)?;
    // Google revises the last three days; the chart marks them.
    data["provisional_from"] =
        json!(plan::google_today(Utc::now()) - chrono::Duration::days(plan::PROVISIONAL_DAYS - 1));
    Ok(ok_meta(data, meta(&property, window)))
}

/// `GET /api/stats/search/timeseries`
async fn timeseries(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> ApiResult<Json<Value>> {
    let property = chosen_property(&st, &access.site).await?;
    let scope = SearchScope {
        site: access.site_id,
        property: &property,
    };
    let window = DateWindow::of_range(&env.range);
    let points = search::timeseries(&st.ch, scope, window).await?;
    Ok(ok_meta(points, meta(&property, window)))
}

/// One ranked list read, after the shared preamble.
async fn list_read(
    st: &AppState,
    access: &SiteAccess,
    env: &micaforge_core::QueryEnvelope,
    params: ListParams,
    raw_query: Option<&str>,
    which: &str,
) -> Result<Json<Value>> {
    let property = chosen_property(st, &access.site).await?;
    let scope = SearchScope {
        site: access.site_id,
        property: &property,
    };
    let window = DateWindow::of_range(&env.range);
    let list = params.list(env, asked_limit(raw_query))?;
    let data = match which {
        "queries" => serde_json::to_value(search::queries(&st.ch, scope, window, &list).await?)?,
        "pages" => {
            let rows = search::pages(&st.ch, scope, window, &list).await?;
            let paths: Vec<String> = rows.iter().map(|r| r.pathname.clone()).collect();
            let traffic = search::page_traffic(&st.ch, access.site_id, &env.range, &paths).await?;
            serde_json::to_value(search::join_pages(rows, &traffic))?
        }
        "countries" => {
            serde_json::to_value(search::countries(&st.ch, scope, window, &list).await?)?
        }
        _ => serde_json::to_value(search::devices(&st.ch, scope, window, &list).await?)?,
    };
    Ok(ok_meta(data, meta(&property, window)))
}

/// `GET /api/stats/search/queries`
async fn queries(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    uri: axum::http::Uri,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    Ok(list_read(&st, &access, &env, params, uri.query(), "queries").await?)
}

/// `GET /api/stats/search/pages`
async fn pages(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    uri: axum::http::Uri,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    Ok(list_read(&st, &access, &env, params, uri.query(), "pages").await?)
}

/// `GET /api/stats/search/countries`
async fn countries(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    uri: axum::http::Uri,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    Ok(list_read(&st, &access, &env, params, uri.query(), "countries").await?)
}

/// `GET /api/stats/search/devices`
async fn devices(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    uri: axum::http::Uri,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    Ok(list_read(&st, &access, &env, params, uri.query(), "devices").await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        let mut cfg = crate::routes::tests::cfg();
        cfg.secret_key_base = "k".repeat(48);
        cfg
    }

    fn claims(expires: i64) -> StateClaims {
        StateClaims {
            s: Uuid::from_u128(1),
            u: Uuid::from_u128(2),
            n: "nonce-1".into(),
            e: expires,
        }
    }

    #[test]
    fn a_state_is_signed_and_expires() {
        let cfg = cfg();
        let now = 1_000_000;
        let state = sign_state(&cfg, &claims(now + 600)).unwrap();
        assert_eq!(verify_state(&cfg, &state, now), Some(claims(now + 600)));
        // Expired.
        assert_eq!(verify_state(&cfg, &state, now + 601), None);
        // Tampered: another site in the body, the old signature.
        let (_, mac) = state.split_once('.').unwrap();
        let mut forged = claims(now + 600);
        forged.s = Uuid::from_u128(99);
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
        assert_eq!(verify_state(&cfg, &format!("{body}.{mac}"), now), None);
        // Signed under another server secret.
        let mut other = cfg.clone();
        other.secret_key_base = "z".repeat(48);
        assert_eq!(verify_state(&other, &state, now), None);
        // Not a state at all.
        assert_eq!(verify_state(&cfg, "", now), None);
        assert_eq!(verify_state(&cfg, "abc", now), None);
        assert_eq!(verify_state(&cfg, "abc.def", now), None);
    }

    #[test]
    fn the_consent_address_asks_for_one_scope_offline_with_pkce() {
        let url = authorize_url(
            &Endpoints::google(),
            "client 1",
            "https://mf.example.com/api/search-console/callback",
            "st.1",
            &pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        );
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        let parsed = url::Url::parse(&url).unwrap();
        let get = |k: &str| {
            parsed
                .query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.into_owned())
        };
        assert_eq!(get("scope").as_deref(), Some(google::SCOPE));
        assert_eq!(get("access_type").as_deref(), Some("offline"));
        assert_eq!(get("prompt").as_deref(), Some("consent"));
        assert_eq!(get("code_challenge_method").as_deref(), Some("S256"));
        // RFC 7636 appendix B.
        assert_eq!(
            get("code_challenge").as_deref(),
            Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
        );
        assert_eq!(get("client_id").as_deref(), Some("client 1"));
        assert_eq!(get("state").as_deref(), Some("st.1"));
        assert!(!url.contains("secret"));
    }

    #[test]
    fn flow_keys_hold_no_nonce() {
        let cfg = cfg();
        let key = flow_key(&cfg, "nonce-1");
        assert!(key.starts_with("mf:gsc:flow:"));
        assert!(!key.contains("nonce-1"));
        assert_ne!(key, flow_key(&cfg, "nonce-2"));
    }

    #[test]
    fn refusal_codes_are_the_ones_the_screen_knows() {
        let codes: Vec<&str> = [
            Refusal::State,
            Refusal::Denied,
            Refusal::Forbidden,
            Refusal::Scope,
            Refusal::Disabled,
            Refusal::Provider,
        ]
        .iter()
        .map(|r| r.code())
        .collect();
        assert_eq!(
            codes,
            [
                "state",
                "denied",
                "forbidden",
                "scope",
                "disabled",
                "provider"
            ]
        );
    }

    fn person(id: u128, method: AuthMethod) -> AuthUser {
        AuthUser {
            user: micaforge_store::models::users::User {
                id: Uuid::from_u128(id),
                email: "ada@example.com".into(),
                name: "Ada".into(),
                avatar_url: None,
                email_verified_at: None,
                totp_secret_sealed: None,
                created_at: Utc::now(),
            },
            method,
            scopes: Vec::new(),
            key: None,
        }
    }

    #[test]
    fn a_callback_needs_the_session_of_the_admin_who_started_it() {
        let q = CallbackQuery {
            code: Some("c".into()),
            ..CallbackQuery::default()
        };
        let admin = person(2, AuthMethod::Session(Uuid::from_u128(50)));
        let good = claims(i64::MAX);
        assert_eq!(
            admit(Some(&admin), Some(good.clone()), &q),
            Ok(good.clone())
        );

        // No state that verified, no session, another person's session, and
        // a credential that is not a browser session: all the same refusal.
        assert_eq!(admit(Some(&admin), None, &q), Err(Refusal::State));
        assert_eq!(admit(None, Some(good.clone()), &q), Err(Refusal::State));
        let stranger = person(3, AuthMethod::Session(Uuid::from_u128(51)));
        assert_eq!(
            admit(Some(&stranger), Some(good.clone()), &q),
            Err(Refusal::State)
        );
        let key = person(2, AuthMethod::ApiKey(Uuid::from_u128(52)));
        assert_eq!(
            admit(Some(&key), Some(good.clone()), &q),
            Err(Refusal::State)
        );

        // A cancelled consent says so, whatever else arrived.
        let denied = CallbackQuery {
            error: Some("access_denied".into()),
            ..CallbackQuery::default()
        };
        assert_eq!(
            admit(Some(&admin), Some(good.clone()), &denied),
            Err(Refusal::Denied)
        );
        let other = CallbackQuery {
            error: Some("server_error".into()),
            ..CallbackQuery::default()
        };
        assert_eq!(
            admit(Some(&admin), Some(good), &other),
            Err(Refusal::Provider)
        );
    }

    /// The reads against a real ClickHouse: a day fetched twice reads once
    /// (the newest fetch, with a row the newer fetch dropped gone), totals
    /// come from the total grain, positions are weighted, and pages carry
    /// Micaforge's own pageviews. Opt in with `MICAFORGE_TEST_CLICKHOUSE_URL`.
    #[tokio::test]
    #[ignore = "needs a ClickHouse server"]
    async fn reads_keep_the_newest_fetch_of_each_day() {
        use crate::routes::flags::tests::{drop_clickhouse, live_clickhouse};
        use micaforge_store::ch::{insert_rows, SearchConsoleRow};

        let ch = live_clickhouse().await;
        let day = |d: &str| d.parse::<chrono::NaiveDate>().unwrap();
        let old = Utc::now() - chrono::Duration::hours(30);
        let new = Utc::now();
        let row = |grain: &str,
                   date: &str,
                   query: &str,
                   path: &str,
                   clicks: u32,
                   impressions: u32,
                   position: f64,
                   at| SearchConsoleRow {
            site_id: 4,
            property: "sc-domain:example.com".into(),
            grain: grain.into(),
            date: day(date),
            query: query.into(),
            page: if path.is_empty() {
                String::new()
            } else {
                format!("https://www.example.com{path}")
            },
            hostname: if path.is_empty() {
                String::new()
            } else {
                "www.example.com".into()
            },
            pathname: path.into(),
            country: String::new(),
            device: String::new(),
            clicks,
            impressions,
            position,
            synced_at: at,
        };
        let rows = vec![
            // 1 October, fetched twice: the first fetch is superseded whole.
            row("total", "2026-10-01", "", "", 5, 100, 4.0, old),
            row("total", "2026-10-01", "", "", 8, 120, 3.0, new),
            row("query", "2026-10-01", "micaforge", "/", 3, 40, 2.0, old),
            row("query", "2026-10-01", "dropped later", "/", 2, 10, 9.0, old),
            row("query", "2026-10-01", "micaforge", "/", 6, 50, 1.0, new),
            // 2 October, fetched once.
            row("total", "2026-10-02", "", "", 2, 100, 10.0, old),
            row(
                "query",
                "2026-10-02",
                "pricing",
                "/pricing",
                1,
                30,
                5.0,
                old,
            ),
            row("page", "2026-10-02", "", "/pricing", 1, 30, 5.0, old),
            // Another property on the same site is never read.
            SearchConsoleRow {
                property: "https://other.example/".into(),
                ..row("total", "2026-10-01", "", "", 999, 999, 1.0, new)
            },
        ];
        insert_rows(&ch, "search_console", &rows).await.unwrap();
        let ts = micaforge_core::time::resolve_range(
            None,
            Some("2026-10-02"),
            Some("2026-10-02"),
            chrono_tz::UTC,
        )
        .unwrap()
        .start
            + chrono::Duration::hours(3);
        ch.exec(&format!(
            "INSERT INTO {}.events (site_id, timestamp, event_id, session_id, visitor_id, kind, hostname, pathname, channel, source, props) \
             VALUES (4, '{}', generateUUIDv4(), 's1', 'v1', 'pageview', 'example.com', '/pricing', 'search', 'google', '{{}}')",
            ch.db,
            micaforge_query::sql::fmt_ts(ts)
        ))
        .await
        .unwrap();

        let scope = SearchScope {
            site: 4,
            property: "sc-domain:example.com",
        };
        let window = DateWindow {
            start: day("2026-10-01"),
            end: day("2026-10-02"),
        };
        let report = search::overview(&ch, scope, window, None).await.unwrap();
        assert_eq!(
            report.totals.clicks, 10,
            "8 from the newest fetch of the 1st, 2 from the 2nd"
        );
        assert_eq!(report.totals.impressions, 220);
        // (3.0 * 120 + 10.0 * 100) / 220
        assert_eq!(report.totals.position, 6.18);

        let list = ListQuery {
            limit: 10,
            page: 1,
            ..ListQuery::default()
        };
        let terms = search::queries(&ch, scope, window, &list).await.unwrap();
        let names: Vec<&str> = terms.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(names, ["micaforge", "pricing"], "the dropped row is gone");
        assert_eq!(terms[0].clicks, 6);

        let series = search::timeseries(&ch, scope, window).await.unwrap();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].clicks, 8);

        let pages = search::pages(&ch, scope, window, &list).await.unwrap();
        let range = micaforge_core::time::resolve_range(
            None,
            Some("2026-10-01"),
            Some("2026-10-02"),
            chrono_tz::UTC,
        )
        .unwrap();
        let paths: Vec<String> = pages.iter().map(|p| p.pathname.clone()).collect();
        let traffic = search::page_traffic(&ch, 4, &range, &paths).await.unwrap();
        let joined = search::join_pages(pages, &traffic);
        assert_eq!(joined[0].search.pathname, "/pricing");
        assert_eq!(joined[0].pageviews, Some(1));
        assert_eq!(joined[0].from_google, Some(1));
        drop_clickhouse(&ch).await;
    }

    /// The connection's life in Postgres: a grant, a property, one lease at a
    /// time, progress kept, and a revocation that drops the token. Opt in
    /// with `MICAFORGE_TEST_DATABASE_URL` and `MICAFORGE_TEST_REDIS_URL`.
    #[tokio::test]
    #[ignore = "needs Postgres and Valkey"]
    async fn a_connection_is_leased_once_and_revoked_cleanly() {
        use crate::routes::oauth::harness::Harness;
        use micaforge_store::models::search_console::{Progress, ACTIVE, PENDING, REVOKED};

        let h = Harness::new().await;
        let owner = h.person("owner@example.com").await;
        let site = sites::create(
            &h.st.pg,
            micaforge_core::new_id(),
            owner.org,
            "example.com",
            "Docs",
            "UTC",
        )
        .await
        .unwrap();
        let user: Uuid =
            sqlx::query_scalar("SELECT id FROM users WHERE email = 'owner@example.com'")
                .fetch_one(&h.st.pg)
                .await
                .unwrap();
        let sealed = gsc::seal_token(&h.st.cfg, site.id, "1//refresh").unwrap();
        let conn = store::upsert_grant(&h.st.pg, site.id, user, &sealed)
            .await
            .unwrap();
        assert_eq!(conn.status, PENDING);
        assert!(!conn.is_syncable(), "no property yet");
        assert!(store::take_lease(&h.st.pg, site.id, 60)
            .await
            .unwrap()
            .is_none());

        let until = "2025-06-09".parse().unwrap();
        let conn = store::set_property(&h.st.pg, site.id, "sc-domain:example.com", until)
            .await
            .unwrap();
        assert_eq!(
            (conn.status.as_str(), conn.backfill_until),
            (ACTIVE, Some(until))
        );
        assert_eq!(store::list_syncable(&h.st.pg).await.unwrap().len(), 1);

        // One lease at a time.
        let leased = store::take_lease(&h.st.pg, site.id, 60).await.unwrap();
        assert!(leased.is_some());
        assert!(store::take_lease(&h.st.pg, site.id, 60)
            .await
            .unwrap()
            .is_none());
        let progress = Progress {
            backfilled_from: Some("2026-09-10".parse().unwrap()),
            synced_through: Some("2026-10-09".parse().unwrap()),
            rows: 1234,
        };
        store::finish_ok(&h.st.pg, site.id, "sc-domain:example.com", &progress)
            .await
            .unwrap();
        let conn = store::get(&h.st.pg, site.id).await.unwrap();
        assert_eq!(conn.rows_synced, 1234);
        assert_eq!(conn.synced_through, progress.synced_through);
        assert!(conn.lease_until.is_none(), "the lease is given back");
        // A run that read another property does not stamp this one.
        store::finish_ok(
            &h.st.pg,
            site.id,
            "https://old.example/",
            &Progress {
                rows: 9,
                ..progress.clone()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            store::get(&h.st.pg, site.id).await.unwrap().rows_synced,
            1234
        );

        store::mark_revoked(&h.st.pg, site.id, "revoked")
            .await
            .unwrap();
        let conn = store::get(&h.st.pg, site.id).await.unwrap();
        assert_eq!(conn.status, REVOKED);
        assert!(conn.refresh_token_sealed.is_none(), "the token is dropped");
        assert!(store::list_syncable(&h.st.pg).await.unwrap().is_empty());
        assert!(
            store::set_property(&h.st.pg, site.id, "sc-domain:example.com", until)
                .await
                .is_err()
        );

        // Connecting again starts over; disconnecting removes the row.
        let again = store::upsert_grant(&h.st.pg, site.id, user, &sealed)
            .await
            .unwrap();
        assert_eq!(
            (again.status.as_str(), again.property.as_deref()),
            (PENDING, None)
        );
        assert!(store::delete(&h.st.pg, site.id).await.unwrap().is_some());
        assert!(store::find(&h.st.pg, site.id).await.unwrap().is_none());
        h.finish().await;
    }

    #[test]
    fn keys_read_the_search_reports_and_never_connect() {
        use crate::extract::{key_route_policy, KeyRoute};
        use axum::http::Method;
        for path in [
            "/api/stats/search/overview",
            "/api/stats/search/queries",
            "/api/stats/search/pages",
        ] {
            assert_eq!(
                key_route_policy(&Method::GET, path),
                KeyRoute::Needs("stats:read")
            );
        }
        for (method, path) in [
            (Method::GET, "/api/search-console"),
            (Method::POST, "/api/search-console/connect"),
            (Method::GET, "/api/search-console/callback"),
            (Method::GET, "/api/search-console/properties"),
            (Method::PUT, "/api/search-console/property"),
            (Method::POST, "/api/search-console/sync"),
            (Method::DELETE, "/api/search-console"),
        ] {
            assert_eq!(
                key_route_policy(&method, path),
                KeyRoute::Refused,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn only_an_explicit_limit_overrides_the_dashboard_default() {
        assert!(asked_limit(Some("site=1&limit=10")));
        assert!(!asked_limit(Some("site=1&range=7d")));
        assert!(!asked_limit(None));
    }
}
