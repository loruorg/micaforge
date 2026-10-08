//! The extractors every route is written against.
//!
//! Four questions get asked at the top of almost every handler in this server:
//! *who is calling*, *which site*, *may they see it*, and *over what window*.
//! Each one has exactly one right answer. Answering them in an extractor
//! rather than in each handler is not tidiness: an authorisation check that is
//! copied into forty handlers is an authorisation check that is missing from one
//! of them, and the one it is missing from is a cross-tenant read.
//!
//! - [`AuthUser`]: a session cookie **or** an API key resolved to a person,
//!   with the key's own grant ([`KeyGrant`]) kept beside it.
//! - [`SiteAccess`]: `?site=` resolved, and the caller's role on it asserted.
//! - [`SiteOrShare`]: the same, or a share credential in `?share=` for the
//!   handful of read endpoints a shared dashboard draws.
//! - [`Envelope`]: the standard query envelope of `docs/CONTRACT.md` §3.1,
//!   fully resolved: named ranges become timestamps, a saved segment is merged
//!   into the filter tree, and an omitted granularity is chosen.
//! - [`PublicSite`]: the unauthenticated path, for share links and sites their
//!   owner has marked public.
//! - [`ClientAddr`]: the caller's address, by the trusted-proxy rule of
//!   [`crate::client_ip`].
//!
//! ## An API key is not its owner
//!
//! A key authenticates as the organisation's owner, because every role lookup
//! in this server is written against a `user_id`. What stops that from handing
//! the key the owner's whole authority is [`KeyGrant`]: the key's organisation,
//! its optional site and its scopes. The grant is checked in three places, and
//! all three are needed:
//!
//! 1. [`key_route_policy`] refuses keys outright on the account, credential,
//!    membership and instance routes, and names the scope every other route
//!    needs from its method.
//! 2. [`key_scope`] (a layer on every API route group) holds the grant for the
//!    life of the request, so [`crate::routes::sites::effective_site_role`] and
//!    [`crate::routes::orgs::effective_org_role`] can refuse anything outside the
//!    key's organisation or site and cap the role at what the scopes allow. Every
//!    route module reaches a role through one of those two functions.
//! 3. A key presented on a route group without that layer is refused, so a
//!    future router that forgets it fails closed rather than open.
//!
//! ## Two rules that hold across all of them
//!
//! **An unauthorised site reads as absent.** Every failure to establish a role
//! on a site returns `404`, never `403`. A `403` would confirm that site 47
//! exists, and site ids are small integers anybody can count through.
//!
//! **Extractors run in argument order.** Handlers are written `State`,
//! `SiteAccess`, `Envelope` precisely so that authorisation happens before the
//! envelope does any work. [`SiteAccess`] leaves the site it resolved in the
//! request extensions, and [`Envelope`] reuses it, so the pair costs one
//! Postgres round trip rather than two.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use axum::extract::{ConnectInfo, FromRequestParts, Query, Request};
use axum::http::request::Parts;
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use axum_extra::extract::cookie::CookieJar;
use chrono::Utc;
use chrono_tz::Tz;
use serde::Deserialize;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use micaforge_core::{
    filters, time, Audience, Compare, Error, FilterGroup, Granularity, QueryEnvelope, Result,
};
use micaforge_store::models::sites::{ShareLink, Site};
use micaforge_store::models::{auth as sessions, keys, orgs, segments, users};

use crate::error::ApiError;
use crate::routes::auth::SESSION_COOKIE;
use crate::routes::orgs::{require_at_least, role_rank, MEMBER, VIEWER};
use crate::routes::sites::{effective_site_role, resolve_share, resolve_site_ref};
use crate::state::AppState;

/// The bearer prefix `docs/CONTRACT.md` §3 fixes for API keys.
const KEY_PREFIX: &str = "mfg_";

/// How much of a presented token is used as the indexed lookup key.
///
/// Must match `routes::keys::PREFIX_LEN`. It is repeated rather than imported
/// because the two are different statements: that module decides how much of a
/// *minted* token is stored, and this one decides how much of a *presented* one
/// to look up. The test at the bottom of this file pins them together.
const KEY_LOOKUP_LEN: usize = 16;

/// Largest page a list endpoint will return.
///
/// Higher than the repository ceiling for Postgres lists, because these are
/// ClickHouse reads that genuinely stream a thousand rows without complaint, and
/// a breakdown of every path on a large documentation site is a real request.
const MAX_LIMIT: u32 = 1_000;

// ---------------------------------------------------------------------------
// AuthUser
// ---------------------------------------------------------------------------

/// How a caller proved who they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// The dashboard's `mf_session` cookie. Carries the person's full authority.
    Session(Uuid),
    /// An `Authorization: Bearer mfg_…` key. Carries its organisation's
    /// authority, bounded by the key's scopes.
    ApiKey(Uuid),
}

/// What an API key was issued for: its organisation, its optional site, and its
/// scopes. The boundary every key-authenticated request is held inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyGrant {
    /// The key's own id, for audit lines.
    pub key_id: Uuid,
    /// The one organisation the key may reach.
    pub org_id: Uuid,
    /// When set, the one site the key may reach. Organisation-level objects
    /// (the organisation itself, its teams) are then out of reach entirely.
    pub site_id: Option<Uuid>,
    /// The scopes stored on the key, from `routes::keys::SCOPES`.
    pub scopes: Vec<String>,
}

impl KeyGrant {
    /// True when this key holds `scope`.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|held| held == scope)
    }

    /// May this key act on the site `site_id` of organisation `org_id`?
    pub fn permits_site(&self, org_id: Uuid, site_id: Uuid) -> bool {
        self.org_id == org_id && self.site_id.is_none_or(|bound| bound == site_id)
    }

    /// May this key act on organisation `org_id` as a whole?
    ///
    /// A site-bound key may not: it was minted for one property, and the
    /// organisation's own objects are not part of that property.
    pub fn permits_org(&self, org_id: Uuid) -> bool {
        self.site_id.is_none() && self.org_id == org_id
    }

    /// The highest role this key may exercise.
    ///
    /// `manage:write` is documented as "everything a `member` could do", so a
    /// key never acts as `admin` or `owner`, whoever owns the organisation. A
    /// key without it is read-only.
    pub fn role_ceiling(&self) -> &'static str {
        if self.has_scope("manage:write") {
            MEMBER
        } else {
            VIEWER
        }
    }

    /// `role`, lowered to [`KeyGrant::role_ceiling`] when it sits above it.
    ///
    /// A role this build does not know is returned unchanged, so the
    /// `require_at_least` that follows still refuses it.
    pub fn cap_role(&self, role: &str) -> String {
        let ceiling = self.role_ceiling();
        match (role_rank(role), role_rank(ceiling)) {
            (Some(have), Some(max)) if have > max => ceiling.to_string(),
            _ => role.to_string(),
        }
    }
}

/// An authenticated caller.
///
/// ## Why an API key resolves to a person
///
/// `api_keys` has no `created_by` column in `docs/CONTRACT.md` §2, so a key
/// belongs to an organisation rather than to whoever minted it. Every
/// authorisation path in this server is written in terms of a `user_id`, so the
/// key is resolved to the organisation's **owner**: the one account that is
/// guaranteed to exist for the life of the organisation. The owner's authority
/// is never the key's authority, though: [`AuthUser::key`] carries the grant,
/// and the role functions bound every key request by it. See the module notes.
#[derive(Debug, Clone)]
pub struct AuthUser {
    /// The account whose memberships this request is resolved against.
    pub user: users::User,
    /// Which credential was presented.
    pub method: AuthMethod,
    /// What the credential may do. A session gets every scope; a key gets the
    /// list stored on its row.
    pub scopes: Vec<String>,
    /// The key's grant, when the caller presented a key. `None` for a session.
    pub key: Option<KeyGrant>,
}

impl AuthUser {
    /// True when this caller holds `scope`.
    ///
    /// A session cookie holds everything: the dashboard is the person, and a
    /// person's limits are their role, not a scope list.
    pub fn has_scope(&self, scope: &str) -> bool {
        matches!(self.method, AuthMethod::Session(_))
            || self.scopes.iter().any(|held| held == scope)
    }

    /// Refuse a caller who does not hold `scope`.
    pub fn require_scope(&self, scope: &str) -> Result<()> {
        if self.has_scope(scope) {
            return Ok(());
        }
        Err(Error::Forbidden(format!(
            "this credential does not hold the `{scope}` scope"
        )))
    }

    /// True when the caller presented an API key.
    pub fn is_api_key(&self) -> bool {
        self.key.is_some()
    }

    /// Refuse an API key.
    ///
    /// For the routes that act on a person or on who holds power over an
    /// organisation: the account itself, credentials, membership, ownership and
    /// the instance. [`key_route_policy`] already refuses keys on all of them;
    /// this is the same rule stated where the handler can be read.
    pub fn require_session(&self) -> Result<()> {
        if self.is_api_key() {
            return Err(Error::Forbidden(
                "API keys cannot be used on this endpoint; sign in to the dashboard".to_string(),
            ));
        }
        Ok(())
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        // A request that has already been authenticated this pass, by two
        // extractors on one handler say, is not authenticated twice.
        if let Some(cached) = parts.extensions.get::<AuthUser>() {
            return Ok(cached.clone());
        }

        let resolved = match bearer_token(parts) {
            Some(token) => authenticate_key_request(parts, state, &token).await?,
            None => authenticate_session(parts, state).await?,
        };

        parts.extensions.insert(resolved.clone());
        Ok(resolved)
    }
}

/// Authenticate a bearer key and hold it to its grant for this request.
///
/// The route is checked before the key is even looked up, so a key presented
/// where keys are never accepted costs no database read.
async fn authenticate_key_request(
    parts: &Parts,
    state: &AppState,
    token: &str,
) -> Result<AuthUser> {
    let needed = match key_route_policy(&parts.method, parts.uri.path()) {
        KeyRoute::Refused => {
            return Err(Error::Forbidden(
                "API keys cannot be used on this endpoint; sign in to the dashboard".to_string(),
            ))
        }
        KeyRoute::Needs(scope) => scope,
    };

    let resolved = authenticate_key(state, token).await?;
    let grant = KeyGrant {
        key_id: resolved.key.id,
        org_id: resolved.key.org_id,
        site_id: resolved.key.site_id,
        scopes: resolved.scopes.clone(),
    };
    if !grant.has_scope(needed) {
        return Err(Error::Forbidden(format!(
            "this key does not hold the `{needed}` scope this endpoint needs"
        )));
    }

    if !bind_key(&grant) {
        // A route group was assembled without `key_scope`, so the role
        // functions could not see this grant. Refusing is the only safe
        // answer; serving it would hand the key its owner's authority.
        tracing::error!(
            path = parts.uri.path(),
            "an API key reached a route with no key scope layer"
        );
        return Err(Error::Forbidden(
            "API keys are not accepted on this endpoint".to_string(),
        ));
    }

    Ok(AuthUser {
        user: resolved.user,
        method: AuthMethod::ApiKey(grant.key_id),
        scopes: resolved.scopes,
        key: Some(grant),
    })
}

/// Resolve the `mf_session` cookie to an account.
///
/// The stored value is a SHA-256 digest of the token, so a Postgres dump yields
/// no usable cookie, and `find_by_token_hash` filters on `expires_at > now()` so
/// a lapsed session is indistinguishable from one that never existed.
async fn authenticate_session(parts: &mut Parts, state: &AppState) -> Result<AuthUser> {
    let jar = CookieJar::from_headers(&parts.headers);
    let token = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Unauthorized("sign in to continue".to_string()))?;

    let digest = micaforge_core::crypto::token_hash(&token);
    let session = sessions::find_by_token_hash(&state.pg, &digest)
        .await?
        .ok_or_else(|| Error::Unauthorized("that session has expired".to_string()))?;

    // A session whose account was deleted is a dead session, not a `500`.
    let user = match users::get(&state.pg, session.user_id).await {
        Ok(user) => user,
        Err(Error::NotFound(_)) => {
            let _ = sessions::delete(&state.pg, session.id).await;
            return Err(Error::Unauthorized("that session has expired".to_string()));
        }
        Err(e) => return Err(e),
    };

    Ok(AuthUser {
        user,
        method: AuthMethod::Session(session.id),
        scopes: Vec::new(),
        key: None,
    })
}

/// The `Authorization: Bearer mfg_…` token, when one was presented.
///
/// Only a token with the product's own prefix is treated as an API key. Anything
/// else, a JWT from a reverse proxy or a stray `Bearer undefined` from a
/// half-configured client, falls through to the cookie path, which produces a
/// far more useful error than "that key does not exist".
fn bearer_token(parts: &Parts) -> Option<String> {
    let raw = parts.headers.get("authorization")?.to_str().ok()?.trim();
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?
        .trim();
    token.starts_with(KEY_PREFIX).then(|| token.to_string())
}

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

/// A presented key, verified, with the account it resolves against.
struct ResolvedKey {
    /// The key row. Its `hash` is a digest and is never the presented token.
    key: keys::ApiKey,
    /// The organisation owner the key resolves against. See [`AuthUser`].
    user: users::User,
    /// The scopes stored on the key.
    scopes: Vec<String>,
}

/// What a route asks of an API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRoute {
    /// Keys are never accepted here, whatever their scopes.
    Refused,
    /// A key holding this scope may proceed, inside its grant.
    Needs(&'static str),
}

/// Which scope a key needs for `method` on `path`, or that it is refused.
///
/// Read scopes for `GET`, `manage:write` for every mutation. Refused outright,
/// whatever the scopes: the account itself (`/api/auth/*`), credentials
/// (`/api/keys*`), the instance (`/api/admin/*`), invitations, membership and
/// per-site role management, ownership transfer, creating and deleting an
/// organisation. Those either act on a person or decide who holds power over an
/// organisation, and a credential that can change who holds power is one that
/// can mint its own successor.
///
/// Anything this table does not name is refused, so a new route module is closed
/// to keys until somebody decides what it needs.
pub fn key_route_policy(method: &Method, path: &str) -> KeyRoute {
    let read = *method == Method::GET || *method == Method::HEAD;
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let Some(("api", rest)) = segments.split_first().map(|(head, rest)| (*head, rest)) else {
        return KeyRoute::Refused;
    };

    match rest {
        ["auth", ..] | ["keys", ..] | ["admin", ..] | ["invitations", ..] => KeyRoute::Refused,
        ["orgs"] if !read => KeyRoute::Refused,
        ["orgs", _] if *method == Method::DELETE => KeyRoute::Refused,
        ["orgs", _, "owner" | "members" | "invitations", ..] => KeyRoute::Refused,
        ["sites", _, "access", ..] => KeyRoute::Refused,
        ["teams", _, "members", ..] => KeyRoute::Refused,
        // Every MCP tool is a read, so the transport needs the read scope even
        // though it is a `POST`.
        ["mcp"] => KeyRoute::Needs("stats:read"),
        ["stats" | "agents" | "citation-gap" | "policy" | "content" | "replay", ..] => {
            if read {
                KeyRoute::Needs("stats:read")
            } else {
                KeyRoute::Needs("manage:write")
            }
        }
        ["sites" | "orgs" | "teams" | "goals" | "funnels" | "segments" | "dashboards" | "flags"
        | "experiments" | "alerts" | "reports" | "imports" | "share", ..] => {
            if read {
                KeyRoute::Needs("manage:read")
            } else {
                KeyRoute::Needs("manage:write")
            }
        }
        _ => KeyRoute::Refused,
    }
}

tokio::task_local! {
    /// The key grant of the request being served, set once by [`AuthUser`].
    static KEY_SCOPE: OnceLock<KeyGrant>;
}

/// Hold a slot for the request's key grant around everything below it.
///
/// Applied with `axum::middleware::from_fn` to every API route group in
/// [`crate::routes::router`]. It costs nothing on a session request: the slot is
/// simply never filled.
pub async fn key_scope(req: Request, next: Next) -> Response {
    KEY_SCOPE.scope(OnceLock::new(), next.run(req)).await
}

/// The key grant of the request being served, when it was made with a key.
///
/// `None` for a session, and `None` outside a request. Read by the two role
/// functions every authorisation path ends in.
pub fn current_key() -> Option<KeyGrant> {
    KEY_SCOPE
        .try_with(|slot| slot.get().cloned())
        .ok()
        .flatten()
}

/// Record the grant for the rest of this request.
///
/// False when there is no slot, which means the route group was assembled
/// without [`key_scope`], or when the slot already holds a different grant.
fn bind_key(grant: &KeyGrant) -> bool {
    KEY_SCOPE
        .try_with(|slot| {
            let _ = slot.set(grant.clone());
            slot.get() == Some(grant)
        })
        .unwrap_or(false)
}

/// Verify a presented API key.
///
/// The prefix is stored in the clear and indexed; the digest is what
/// authenticates. Comparing digests in constant time is not theatre here: the
/// candidate set is public knowledge (anybody can read a prefix off a key they
/// hold) and a timing oracle on the remaining bytes would be a real one.
async fn authenticate_key(state: &AppState, token: &str) -> Result<ResolvedKey> {
    let refused = || Error::Unauthorized("that API key is not valid".to_string());

    let prefix: String = token.chars().take(KEY_LOOKUP_LEN).collect();
    let candidates = keys::find_by_prefix(&state.pg, &prefix).await?;
    let digest = micaforge_core::crypto::token_hash(token);

    let now = Utc::now();
    let mut matched: Option<keys::ApiKey> = None;
    for candidate in candidates {
        let same: bool = candidate.hash.as_bytes().ct_eq(digest.as_bytes()).into();
        if same && candidate.is_live(now) {
            matched = Some(candidate);
            break;
        }
    }
    let key = matched.ok_or_else(refused)?;

    // The organisation, and through it the account this key acts as. An
    // organisation that has been deleted leaves its keys unusable rather than
    // erroring: the credential genuinely no longer names anything.
    let org = orgs::get(&state.pg, key.org_id)
        .await
        .map_err(|_| refused())?;
    let user = users::get(&state.pg, org.owner_id)
        .await
        .map_err(|_| refused())?;

    // Best effort. Failing a request because a usage timestamp could not be
    // written would be an odd way to report that Postgres is busy.
    if let Err(e) = keys::touch_last_used(&state.pg, key.id).await {
        tracing::warn!(key = %key.id, error = %e, "could not record api key usage");
    }

    let scopes = string_list(&key.scopes);
    Ok(ResolvedKey { key, user, scopes })
}

/// Read a JSON array of strings, ignoring anything that is not one.
pub(crate) fn string_list(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// SiteAccess
// ---------------------------------------------------------------------------

/// A site the caller is allowed to read, and what they may do with it.
///
/// The role is not enforced beyond "holds one": handlers assert their own floor
/// with `require_at_least(access.role.as_str(), MEMBER)`, because the floor is
/// per-endpoint (listing goals is `viewer`, creating one is `member`) and an
/// extractor cannot know which it is being used for.
#[derive(Debug, Clone)]
pub struct SiteAccess {
    /// The numeric id every ClickHouse query keys on.
    pub site_id: u16,
    /// The full site row, including its timezone and settings.
    pub site: Site,
    /// The caller's effective role: their organisation role, overridden by
    /// `site_access` when a row names them.
    pub role: String,
}

impl FromRequestParts<AppState> for SiteAccess {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let auth = AuthUser::from_request_parts(parts, state).await?;

        let reference =
            query_param(parts, "site").ok_or_else(|| Error::validation("site", "is required"))?;
        let site = resolve_site_ref(&state.pg, &reference).await?;

        let role = effective_site_role(&state.pg, &site, auth.user.id).await?;
        // Every role clears `viewer`, so this only rejects a value that is not a
        // role at all, a hand-edited `site_access` row say. It is here so the
        // vocabulary is checked in one place rather than in forty handlers.
        require_at_least(&role, VIEWER)?;

        let resolved = SiteAccess {
            site_id: site.site_id_u16(),
            site,
            role,
        };

        // Left for `Envelope`, which needs the timezone and would otherwise read
        // the same row again.
        parts.extensions.insert(Arc::new(resolved.site.clone()));
        Ok(resolved)
    }
}

// ---------------------------------------------------------------------------
// SiteOrShare
// ---------------------------------------------------------------------------

/// [`SiteAccess`], or a share credential, for the reads a shared dashboard draws.
///
/// Opt-in per handler, and only on the endpoints `/share/:slug` renders. With no
/// `?share=` this is exactly [`SiteAccess`]. With one, the credential alone
/// decides: any cookie or key on the same request is ignored, so a signed-in
/// operator looking at a share link sees what the link publishes and no more.
///
/// The shared path is a `viewer` on the link's site and nothing else. Which
/// endpoint, which dimension, which metric and which audience it may ask for
/// is decided by [`share_scope_for`] against the link's own scopes, and filters,
/// saved segments and goals are refused outright: an aggregate filtered down to
/// one visitor id is that visitor's history.
#[derive(Debug, Clone)]
pub struct SiteOrShare(pub SiteAccess);

impl FromRequestParts<AppState> for SiteOrShare {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let Some(credential) = query_param(parts, "share") else {
            return Ok(SiteOrShare(
                SiteAccess::from_request_parts(parts, state).await?,
            ));
        };

        let public = resolve_share(state, &credential).await?;
        let needed = share_scope_for(
            &parts.method,
            parts.uri.path(),
            &ShareQuery::from_parts(parts),
        )?;
        if !public.scopes.iter().any(|held| held == needed) {
            return Err(Error::Forbidden(format!(
                "this shared dashboard does not publish `{needed}`"
            ))
            .into());
        }

        // The envelope still names a site, and it has to be the link's own.
        if let Some(reference) = query_param(parts, "site") {
            if !matches_reference(&public.site, &reference) {
                return Err(Error::not_found("dashboard").into());
            }
        }

        parts.extensions.insert(Arc::new(public.site.clone()));
        Ok(SiteOrShare(SiteAccess {
            site_id: public.site_id,
            site: public.site,
            role: VIEWER.to_string(),
        }))
    }
}

/// The metrics a shared panel may be drawn in: the overview's own six.
///
/// Revenue, conversions and the agent metrics each belong to another panel, or
/// to none a share link can open.
const SHARE_METRICS: &[&str] = &[
    "visitors",
    "sessions",
    "pageviews",
    "bounce_rate",
    "avg_duration",
    "views_per_session",
];

/// The query parameters of a shared read that decide which panel it belongs to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShareQuery {
    /// `?dimension=`, for a breakdown.
    pub dimension: Option<String>,
    /// `?metric=`.
    pub metric: Option<String>,
    /// `?metrics=`, comma separated.
    pub metrics: Option<String>,
    /// `?audience=`.
    pub audience: Option<String>,
    /// Whether `?filters=` was sent.
    pub filters: bool,
    /// Whether `?segment=` was sent.
    pub segment: bool,
    /// Whether `?goal=` was sent.
    pub goal: bool,
}

impl ShareQuery {
    /// Read the parameters off a request.
    fn from_parts(parts: &Parts) -> Self {
        ShareQuery {
            dimension: query_param(parts, "dimension"),
            metric: query_param(parts, "metric"),
            metrics: query_param(parts, "metrics"),
            audience: query_param(parts, "audience"),
            filters: query_param(parts, "filters").is_some_and(|f| f != "[]"),
            segment: query_param(parts, "segment").is_some(),
            goal: query_param(parts, "goal").is_some(),
        }
    }
}

/// The share scope a shared read needs, or why it may not be made at all.
///
/// The whole public surface, in one table:
///
/// | endpoint | scope |
/// |---|---|
/// | `GET /api/stats/overview` (human) | `overview` |
/// | `GET /api/stats/timeseries` (human, overview metrics) | `overview` |
/// | `GET /api/stats/breakdown?dimension=pathname` (human) | `pages` |
/// | `GET /api/stats/breakdown?dimension=channel` (human) | `sources` |
/// | `GET /api/agents/overview`, `GET /api/agents/timeseries` | `agents` |
/// | `GET /api/citation-gap/summary` | `citation_gap` |
///
/// Everything else, and every request carrying filters, a saved segment or a
/// goal, is refused.
pub fn share_scope_for(method: &Method, path: &str, query: &ShareQuery) -> Result<&'static str> {
    let refused = |what: &str| Error::Forbidden(format!("a shared dashboard cannot {what}"));

    if *method != Method::GET {
        return Err(refused("change anything"));
    }
    if query.filters || query.segment || query.goal {
        return Err(refused("apply filters, saved segments or goals"));
    }

    let human_only = || match query.audience.as_deref() {
        None | Some("human") => Ok(()),
        Some(_) => Err(refused("read another audience on this panel")),
    };
    let overview_metrics = || {
        let named = [query.metric.as_deref(), query.metrics.as_deref()];
        for list in named.into_iter().flatten() {
            for metric in list.split(',').map(str::trim).filter(|m| !m.is_empty()) {
                if !SHARE_METRICS.contains(&metric) {
                    return Err(refused("draw that metric"));
                }
            }
        }
        Ok(())
    };

    match path {
        "/api/stats/overview" => {
            human_only()?;
            Ok("overview")
        }
        "/api/stats/timeseries" => {
            human_only()?;
            overview_metrics()?;
            Ok("overview")
        }
        "/api/stats/breakdown" => {
            human_only()?;
            overview_metrics()?;
            match query.dimension.as_deref().map(str::trim) {
                Some("pathname") => Ok("pages"),
                Some("channel") => Ok("sources"),
                _ => Err(refused("break down by that dimension")),
            }
        }
        "/api/agents/overview" | "/api/agents/timeseries" => Ok("agents"),
        "/api/citation-gap/summary" => Ok("citation_gap"),
        _ => Err(Error::not_found("dashboard")),
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// The standard query envelope, resolved.
///
/// Newtype rather than a bare [`QueryEnvelope`] so that `QueryEnvelope` stays a
/// plain data type in `micaforge-core` with no framework in it, and so handlers
/// destructure it explicitly: `Envelope(env): Envelope`.
#[derive(Debug, Clone)]
pub struct Envelope(pub QueryEnvelope);

/// The raw query string, before any of it means anything.
///
/// Everything is `Option<String>` including the numbers. A caller that sends
/// `limit=` (which is what an empty form field produces) should get the default,
/// not a deserialisation failure naming a field they did not think they set.
#[derive(Debug, Default, Deserialize)]
struct RawEnvelope {
    site: Option<String>,
    start: Option<String>,
    end: Option<String>,
    range: Option<String>,
    compare: Option<String>,
    tz: Option<String>,
    granularity: Option<String>,
    filters: Option<String>,
    segment: Option<String>,
    audience: Option<String>,
    limit: Option<String>,
    page: Option<String>,
}

impl FromRequestParts<AppState> for Envelope {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let Query(raw) = Query::<RawEnvelope>::from_request_parts(parts, state)
            .await
            .map_err(|e| Error::BadRequest(format!("the query string could not be read: {e}")))?;

        let reference =
            trimmed(raw.site.as_deref()).ok_or_else(|| Error::validation("site", "is required"))?;

        // Reuse the row `SiteAccess` already resolved when it ran first, which
        // it does on every analytics handler in this server.
        let site = match parts.extensions.get::<Arc<Site>>() {
            Some(cached) if matches_reference(cached, &reference) => Arc::clone(cached),
            _ => Arc::new(resolve_site_ref(&state.pg, &reference).await?),
        };

        let tz = match trimmed(raw.tz.as_deref()) {
            Some(name) => Tz::from_str(&name)
                .map_err(|_| Error::validation("tz", "is not an IANA time zone name"))?,
            // The site's own zone, falling back to UTC for a row whose zone was
            // written before the validation in `routes::sites` existed.
            None => Tz::from_str(&site.timezone).unwrap_or(chrono_tz::UTC),
        };

        let range = time::resolve_range(
            trimmed(raw.range.as_deref()).as_deref(),
            trimmed(raw.start.as_deref()).as_deref(),
            trimmed(raw.end.as_deref()).as_deref(),
            tz,
        )?;

        let granularity = match trimmed(raw.granularity.as_deref()) {
            // A fine granularity over a long range is coarsened until the series
            // fits the bucket cap (docs/CONTRACT.md 3.1), rather than refused.
            Some(name) => time::fit_granularity(&range, Granularity::parse(&name)?),
            None => time::auto_granularity(&range),
        };

        let compare = match trimmed(raw.compare.as_deref()) {
            Some(name) => Compare::parse(&name)?,
            None => Compare::None,
        };

        let audience = match trimmed(raw.audience.as_deref()) {
            Some(name) => Audience::parse(&name)?,
            None => Audience::Human,
        };

        let requested = match trimmed(raw.filters.as_deref()) {
            Some(json) => Some(filters::parse_filters(&json)?),
            None => None,
        };
        let saved = load_segment(state, &site, raw.segment.as_deref()).await?;
        let merged = filters::merge(requested, saved);
        if let Some(group) = merged.as_ref() {
            group.validate()?;
        }

        Ok(Envelope(QueryEnvelope {
            site: site.site_id_u16(),
            range,
            compare,
            granularity,
            filters: merged,
            audience,
            limit: number(raw.limit.as_deref(), "limit", 100)?.clamp(1, MAX_LIMIT),
            page: number(raw.page.as_deref(), "page", 1)?.max(1),
        }))
    }
}

/// Load and validate a saved segment named by `?segment=`.
///
/// The site check is the point. A segment id is a UUID nobody can guess, but
/// "unguessable" is not an authorisation model, and applying another site's
/// saved filters would silently answer a question about the wrong property.
async fn load_segment(
    state: &AppState,
    site: &Site,
    reference: Option<&str>,
) -> Result<Option<FilterGroup>> {
    let Some(reference) = trimmed(reference) else {
        return Ok(None);
    };
    let id = Uuid::parse_str(&reference)
        .map_err(|_| Error::validation("segment", "must be a saved segment id"))?;

    let segment = segments::get(&state.pg, id).await?;
    if segment.site_id != site.id {
        // Reported as missing, not forbidden: a `403` would confirm the id.
        return Err(Error::not_found("segment"));
    }

    let parsed = filters::parse_filters(&segment.filters.to_string()).map_err(|e| {
        // The stored document is ours, not the caller's, so a broken one is an
        // internal fault and the caller cannot fix it by changing their request.
        tracing::error!(segment = %segment.id, error = %e, "a saved segment does not parse");
        Error::internal("that saved segment is stored in a form this build cannot read")
    })?;
    Ok(Some(parsed))
}

/// Does the cached site row answer this `?site=` reference?
///
/// Guards against a handler that resolved one site through `SiteAccess` and then
/// received a different `?site=` on the same request, which cannot happen today
/// (both read the same parameter) and must not start silently working if it ever
/// can.
fn matches_reference(site: &Site, reference: &str) -> bool {
    reference
        .parse::<u16>()
        .map(|numeric| numeric == site.site_id_u16())
        .unwrap_or_else(|_| {
            reference
                .parse::<Uuid>()
                .map(|id| id == site.id)
                .unwrap_or(false)
        })
}

// ---------------------------------------------------------------------------
// PublicSite
// ---------------------------------------------------------------------------

/// A site reachable without signing in.
///
/// Reached through one query parameter, `?share=`, which carries one of three
/// things (resolved by [`crate::routes::sites::resolve_share`]):
///
/// - **A share link's slug.** Carries the link's own scopes and expiry, and is
///   revoked by deleting one row. Refused when the link has a password.
/// - **An unlock token** (`mfs_…`) from `POST /api/share/{slug}/unlock`, for a
///   password-protected link. Short-lived, and dead the moment the link is
///   deleted, expires, or has its password changed.
/// - **A public site's slug**, issued when the owner set `sites.public`.
///   Carries every share scope, and stops answering when `public` is cleared.
///
/// A password is never accepted here. Verifying one is a `POST` with a body and
/// a rate limit, so the exchange lives in that handler and mints the token.
#[derive(Debug, Clone)]
pub struct PublicSite {
    /// The numeric id ClickHouse keys on.
    pub site_id: u16,
    /// The site row.
    pub site: Site,
    /// The link this was reached through, when it was a link.
    pub link: Option<ShareLink>,
    /// The share scopes the caller may read (`routes::sites::SHARE_SCOPES`).
    pub scopes: Vec<String>,
}

impl FromRequestParts<AppState> for PublicSite {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let credential =
            query_param(parts, "share").ok_or_else(|| Error::not_found("dashboard"))?;
        Ok(resolve_share(state, &credential).await?)
    }
}

// ---------------------------------------------------------------------------
// ClientAddr
// ---------------------------------------------------------------------------

/// The address a request is attributed to, by [`crate::client_ip::client_ip`].
///
/// The same rule the access log and the global rate limiter use, so a per-route
/// limit cannot be escaped by writing a header the global one ignores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAddr(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientAddr {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip());
        Ok(ClientAddr(crate::client_ip::client_ip(
            &parts.headers,
            peer,
        )))
    }
}

// ---------------------------------------------------------------------------
// Query-string helpers
// ---------------------------------------------------------------------------

/// One query parameter, percent-decoded, with empty treated as absent.
///
/// Written by hand rather than through a `Query<T>` extractor because the
/// callers here want a single value out of a query string that also carries a
/// dozen parameters belonging to the handler, and a typed extractor would either
/// have to know all of them or throw the rest away.
pub(crate) fn query_param(parts: &Parts, name: &str) -> Option<String> {
    let query = parts.uri.query()?;
    for pair in query.split('&') {
        let (key, value) = match pair.split_once('=') {
            Some(split) => split,
            None => (pair, ""),
        };
        if key != name {
            continue;
        }
        let decoded = percent_decode(value);
        let trimmed = decoded.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Minimal `application/x-www-form-urlencoded` decoding.
///
/// Only what a query parameter needs: `+` for space and `%XX` escapes. A
/// malformed escape is left verbatim rather than dropped, so a value that was
/// never encoded in the first place still arrives intact.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A trimmed, non-empty copy of an optional string.
fn trimmed(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Parse an optional numeric parameter, naming the field when it will not parse.
fn number(raw: Option<&str>, field: &'static str, default: u32) -> Result<u32> {
    match trimmed(raw) {
        None => Ok(default),
        Some(value) => value
            .parse::<u32>()
            .map_err(|_| Error::validation(field, "must be a whole number")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn parts_for(uri: &str) -> Parts {
        Request::builder().uri(uri).body(()).unwrap().into_parts().0
    }

    #[test]
    fn the_key_lookup_length_matches_what_is_minted() {
        // If `routes::keys::PREFIX_LEN` ever changes, every existing key stops
        // authenticating silently. This is the test that makes that loud.
        assert_eq!(KEY_LOOKUP_LEN, 16);
    }

    #[test]
    fn only_this_products_bearer_tokens_are_treated_as_keys() {
        let mut parts = parts_for("/api/stats/overview");
        parts
            .headers
            .insert("authorization", "Bearer mfg_abc123".parse().unwrap());
        assert_eq!(bearer_token(&parts).as_deref(), Some("mfg_abc123"));

        // A proxy's JWT must fall through to the cookie path, not be rejected as
        // a bad API key.
        parts
            .headers
            .insert("authorization", "Bearer eyJhbGciOi".parse().unwrap());
        assert!(bearer_token(&parts).is_none());

        parts
            .headers
            .insert("authorization", "Basic dXNlcjpwdw==".parse().unwrap());
        assert!(bearer_token(&parts).is_none());
    }

    #[test]
    fn query_parameters_are_decoded_and_empty_means_absent() {
        let parts = parts_for("/api/stats/pages?site=3&filters=%5B%5D&limit=&tz=Europe%2FLisbon");
        assert_eq!(query_param(&parts, "site").as_deref(), Some("3"));
        assert_eq!(query_param(&parts, "filters").as_deref(), Some("[]"));
        assert_eq!(query_param(&parts, "tz").as_deref(), Some("Europe/Lisbon"));
        assert!(query_param(&parts, "limit").is_none(), "empty is absent");
        assert!(query_param(&parts, "segment").is_none());
    }

    fn share_query(uri: &str) -> ShareQuery {
        ShareQuery::from_parts(&parts_for(uri))
    }

    #[test]
    fn a_shared_dashboard_reads_exactly_the_panels_it_draws() {
        let get = Method::GET;
        let scope = |uri: &str| {
            let path = uri.split('?').next().unwrap();
            share_scope_for(&get, path, &share_query(uri))
        };

        assert_eq!(
            scope("/api/stats/overview?site=1&range=30d&compare=previous").unwrap(),
            "overview"
        );
        assert_eq!(
            scope("/api/stats/timeseries?site=1&metric=visitors").unwrap(),
            "overview"
        );
        assert_eq!(
            scope("/api/stats/breakdown?dimension=pathname&limit=8").unwrap(),
            "pages"
        );
        assert_eq!(
            scope("/api/stats/breakdown?dimension=channel").unwrap(),
            "sources"
        );
        assert_eq!(
            scope("/api/agents/overview?audience=agent").unwrap(),
            "agents"
        );
        assert_eq!(
            scope("/api/agents/timeseries?audience=agent").unwrap(),
            "agents"
        );
        assert_eq!(
            scope("/api/citation-gap/summary?audience=all").unwrap(),
            "citation_gap"
        );
        // The share screen always sends an empty filter list as absent, and an
        // empty array means the same thing.
        assert!(scope("/api/stats/overview?filters=%5B%5D").is_ok());
    }

    #[test]
    fn a_shared_dashboard_refuses_everything_it_does_not_draw() {
        let get = Method::GET;
        let scope = |uri: &str| {
            let path = uri.split('?').next().unwrap();
            share_scope_for(&get, path, &share_query(uri))
        };

        // The places a person lives.
        for path in [
            "/api/stats/visitors",
            "/api/stats/sessions",
            "/api/stats/visitors/abc",
            "/api/replay",
            "/api/stats/errors",
            "/api/stats/export.csv",
            "/api/sites/1",
            "/api/goals",
        ] {
            assert!(scope(path).is_err(), "{path} must not be shared");
        }

        // A breakdown that could single somebody out, or a metric that belongs
        // to another panel.
        assert!(scope("/api/stats/breakdown?dimension=visitor_id").is_err());
        assert!(scope("/api/stats/breakdown?dimension=identified_id").is_err());
        assert!(scope("/api/stats/breakdown").is_err());
        assert!(scope("/api/stats/timeseries?metric=revenue").is_err());
        assert!(scope("/api/stats/breakdown?dimension=pathname&metrics=visitors,revenue").is_err());

        // Filters, saved segments and goals narrow an aggregate to a person.
        let filtered = "/api/stats/overview?filters=%5B%7B%22dimension%22%3A%22visitor_id%22%7D%5D";
        assert!(scope(filtered).is_err());
        assert!(scope("/api/stats/overview?segment=0190").is_err());
        assert!(scope("/api/stats/timeseries?metric=visitors&goal=0190").is_err());

        // The overview scope is the human record only.
        assert!(scope("/api/stats/overview?audience=agent").is_err());
        assert!(scope("/api/stats/overview?audience=all").is_err());

        // And nothing is ever written through a share.
        let empty = ShareQuery::default();
        assert!(share_scope_for(&Method::POST, "/api/stats/overview", &empty).is_err());
        assert!(share_scope_for(&Method::DELETE, "/api/stats/visitors/abc", &empty).is_err());
    }

    fn grant(site: Option<Uuid>, scopes: &[&str]) -> KeyGrant {
        KeyGrant {
            key_id: Uuid::from_u128(9),
            org_id: Uuid::from_u128(1),
            site_id: site,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn keys_are_refused_on_every_route_that_acts_on_a_person_or_on_power() {
        use KeyRoute::Refused;
        let cases = [
            (Method::GET, "/api/auth/me"),
            (Method::POST, "/api/auth/totp/enroll"),
            (Method::POST, "/api/auth/code/verify"),
            (Method::GET, "/api/keys"),
            (Method::POST, "/api/keys"),
            (Method::DELETE, "/api/keys/0190"),
            (Method::GET, "/api/admin/stats"),
            (Method::DELETE, "/api/admin/users/0190"),
            (Method::POST, "/api/orgs/0190/owner"),
            (Method::DELETE, "/api/orgs/0190"),
            (Method::POST, "/api/orgs"),
            (Method::GET, "/api/orgs/0190/members"),
            (Method::PATCH, "/api/orgs/0190/members/0191"),
            (Method::POST, "/api/orgs/0190/invitations"),
            (Method::POST, "/api/invitations/accept"),
            (Method::POST, "/api/invitations/preview"),
            (Method::POST, "/api/sites/3/access"),
            (Method::DELETE, "/api/sites/3/access/0191"),
            (Method::POST, "/api/teams/0190/members"),
            // A route nobody has classified is closed until somebody does.
            (Method::GET, "/api/something-new"),
            (Method::GET, "/mf.js"),
        ];
        for (method, path) in cases {
            assert_eq!(key_route_policy(&method, path), Refused, "{method} {path}");
        }
    }

    #[test]
    fn a_key_needs_the_read_scope_to_read_and_the_write_scope_to_write() {
        use KeyRoute::Needs;
        assert_eq!(
            key_route_policy(&Method::GET, "/api/stats/overview"),
            Needs("stats:read")
        );
        assert_eq!(
            key_route_policy(&Method::GET, "/api/citation-gap/summary"),
            Needs("stats:read")
        );
        assert_eq!(
            key_route_policy(&Method::POST, "/api/mcp"),
            Needs("stats:read")
        );
        // An erasure under an analytics prefix is still a write.
        assert_eq!(
            key_route_policy(&Method::DELETE, "/api/stats/visitors/abc"),
            Needs("manage:write")
        );
        assert_eq!(
            key_route_policy(&Method::DELETE, "/api/replay/s1"),
            Needs("manage:write")
        );
        assert_eq!(
            key_route_policy(&Method::POST, "/api/policy/refetch"),
            Needs("manage:write")
        );

        assert_eq!(
            key_route_policy(&Method::GET, "/api/sites"),
            Needs("manage:read")
        );
        assert_eq!(
            key_route_policy(&Method::GET, "/api/orgs/0190"),
            Needs("manage:read")
        );
        assert_eq!(
            key_route_policy(&Method::DELETE, "/api/sites/3"),
            Needs("manage:write")
        );
        assert_eq!(
            key_route_policy(&Method::POST, "/api/goals"),
            Needs("manage:write")
        );
        assert_eq!(
            key_route_policy(&Method::PATCH, "/api/share/0190"),
            Needs("manage:write")
        );
    }

    #[test]
    fn a_key_reaches_only_its_own_organisation_and_its_own_site() {
        let org = Uuid::from_u128(1);
        let other_org = Uuid::from_u128(2);
        let site = Uuid::from_u128(10);
        let other_site = Uuid::from_u128(11);

        let org_key = grant(None, &["stats:read"]);
        assert!(org_key.permits_site(org, site));
        assert!(org_key.permits_site(org, other_site));
        assert!(!org_key.permits_site(other_org, site));
        assert!(org_key.permits_org(org));
        assert!(!org_key.permits_org(other_org));

        let site_key = grant(Some(site), &["stats:read"]);
        assert!(site_key.permits_site(org, site));
        assert!(!site_key.permits_site(org, other_site));
        // The same site id under another organisation is not the same site.
        assert!(!site_key.permits_site(other_org, site));
        // A site-bound key cannot act on the organisation as a whole.
        assert!(!site_key.permits_org(org));
    }

    #[test]
    fn a_key_never_acts_above_member_and_a_read_key_never_above_viewer() {
        let read = grant(None, &["stats:read", "manage:read"]);
        assert_eq!(read.cap_role("owner"), VIEWER);
        assert_eq!(read.cap_role("admin"), VIEWER);
        assert_eq!(read.cap_role("viewer"), VIEWER);
        assert!(require_at_least(&read.cap_role("owner"), MEMBER).is_err());

        let write = grant(None, &["manage:write"]);
        assert_eq!(write.cap_role("owner"), MEMBER);
        assert_eq!(write.cap_role("admin"), MEMBER);
        // A narrower role is never widened by the cap.
        assert_eq!(write.cap_role("viewer"), VIEWER);
        assert!(require_at_least(&write.cap_role("owner"), "admin").is_err());
        assert!(require_at_least(&write.cap_role("owner"), "owner").is_err());

        // An unknown role passes through so the ladder still refuses it.
        assert_eq!(write.cap_role("archon"), "archon");
        assert!(require_at_least(&write.cap_role("archon"), VIEWER).is_err());
    }

    #[tokio::test]
    async fn the_layer_is_what_lets_a_handler_hold_a_key_to_its_grant() {
        use axum::body::Body;
        use axum::routing::get;
        use tower::ServiceExt;

        // What `AuthUser` does for a key, then what the role functions see.
        async fn probe() -> &'static str {
            let g = KeyGrant {
                key_id: Uuid::from_u128(9),
                org_id: Uuid::from_u128(1),
                site_id: None,
                scopes: vec!["stats:read".into()],
            };
            if bind_key(&g) && current_key() == Some(g) {
                "bound"
            } else {
                "refused"
            }
        }

        let call = |router: axum::Router| async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/probe")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            String::from_utf8(bytes.to_vec()).unwrap()
        };

        let guarded = axum::Router::new()
            .route("/probe", get(probe))
            .layer(axum::middleware::from_fn(key_scope));
        assert_eq!(call(guarded).await, "bound");

        // A group assembled without the layer cannot bind, so `AuthUser`
        // refuses the key instead of serving it with its owner's authority.
        let unguarded = axum::Router::new().route("/probe", get(probe));
        assert_eq!(call(unguarded).await, "refused");
    }

    #[tokio::test]
    async fn the_grant_is_visible_for_the_request_and_nowhere_else() {
        let g = grant(Some(Uuid::from_u128(10)), &["stats:read"]);

        // Outside a request there is no slot: binding fails, and the role
        // functions see a session.
        assert!(!bind_key(&g));
        assert!(current_key().is_none());

        KEY_SCOPE
            .scope(OnceLock::new(), async {
                assert!(
                    current_key().is_none(),
                    "a session request carries no grant"
                );
                assert!(bind_key(&g));
                assert_eq!(current_key(), Some(g.clone()));
                // Binding the same grant again is harmless; a different one is not.
                assert!(bind_key(&g));
                assert!(!bind_key(&grant(None, &["manage:write"])));
                assert_eq!(current_key(), Some(g.clone()));
            })
            .await;

        assert!(current_key().is_none());
    }

    #[test]
    fn a_number_that_will_not_parse_names_its_field() {
        assert_eq!(number(None, "limit", 100).unwrap(), 100);
        assert_eq!(number(Some(" 25 "), "limit", 100).unwrap(), 25);
        assert_eq!(
            number(Some("lots"), "limit", 100).unwrap_err().field(),
            Some("limit")
        );
    }

    #[test]
    fn a_session_holds_every_scope_and_a_key_holds_only_its_own() {
        let user = users::User {
            id: Uuid::nil(),
            email: "a@b.c".into(),
            name: "A".into(),
            avatar_url: None,
            email_verified_at: None,
            totp_secret_sealed: None,
            created_at: Utc::now(),
        };

        let session = AuthUser {
            user: user.clone(),
            method: AuthMethod::Session(Uuid::nil()),
            scopes: Vec::new(),
            key: None,
        };
        assert!(session.has_scope("manage:write"));
        assert!(session.require_session().is_ok());

        let key = AuthUser {
            user,
            method: AuthMethod::ApiKey(Uuid::nil()),
            scopes: vec!["stats:read".into()],
            key: Some(grant(None, &["stats:read"])),
        };
        assert!(key.has_scope("stats:read"));
        assert!(!key.has_scope("manage:write"));
        assert!(key.require_scope("manage:write").is_err());
        assert!(key.require_session().is_err());
    }
}
