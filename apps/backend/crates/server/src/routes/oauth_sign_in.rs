//! `/api/auth/oauth/*` — "Continue with Google" and "Continue with GitHub".
//!
//! An alternative to the emailed code, with the same outcome and the same
//! rules: a person proves control of an address (here, by the provider saying
//! the address is verified), and that address is the account. There is still
//! no password anywhere. Each provider is off until its client id and secret
//! are configured (`MICAFORGE_GOOGLE_CLIENT_ID`/`_SECRET`,
//! `MICAFORGE_GITHUB_CLIENT_ID`/`_SECRET`), and `GET /api/config` lists only
//! the configured ones, so the sign-in screen shows a button only when it
//! works. An install with neither configured never calls Google or GitHub.
//!
//! ## The flow
//!
//! 1. `GET /api/auth/oauth/{provider}/start?next=` mints a `state` and a PKCE
//!    verifier (RFC 7636, `S256`), keeps both in Valkey for ten minutes under
//!    a keyed digest of the state, puts the state in an `HttpOnly` cookie
//!    scoped to these routes, and sends the browser to the provider.
//! 2. `GET /api/auth/oauth/{provider}/callback` refuses unless the `state` the
//!    provider hands back equals the cookie's (compared in constant time) and
//!    names a live record for the same provider, which it then consumes with a
//!    compare-and-swap: a state works once, in the browser that started it.
//!    That pairing is what stops a login CSRF (somebody else's code planted in
//!    your browser). The code is exchanged with the verifier, so a code stolen
//!    from the redirect is useless without it.
//! 3. The verified address is read. Google: the userinfo endpoint, and
//!    `email_verified` must be true. GitHub: `/user/emails`, and the address
//!    used is the one marked primary *and* verified; a GitHub account whose
//!    primary address is not verified is refused rather than signed in with a
//!    secondary one.
//! 4. The account is found by the provider's identity, or else by the
//!    address (and the identity linked to it), or else created, through the
//!    same door the code flow uses ([`super::auth::open_account`]): an
//!    existing account, the first account, open registration, or an
//!    invitation. Anybody else is turned away.
//! 5. An account with two-factor on is not signed in yet: a short-lived,
//!    single-use pending record (in an `HttpOnly` cookie, never the URL) waits
//!    for `POST /api/auth/oauth/totp`, exactly as the code flow answers
//!    `totp_required`.
//!
//! Every refusal sends the browser back to the sign-in screen with one code
//! (`?oauth_error=state|denied|unverified_email|closed|linked|provider|disabled`)
//! and nothing else. The provider's access token is used inside the callback
//! and dropped; nothing the provider issued is stored.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Redirect;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use micaforge_core::config::OAuthClient;
use micaforge_core::{crypto, Config, Error, Result};
use micaforge_store::models::{oauth_identities as identities, users};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::audit::{self, Actor, Entry};
use crate::client_ip::address_tag;
use crate::error::ApiError;
use crate::extract::{AuthUser, ClientAddr};
use crate::response::ok;
use crate::routes::auth::{
    open_account, open_session, user_json, verify_totp_secret, NewAccount, Opened,
};
use crate::routes::orgs::clean_email;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The cookie that binds a flow to the browser that started it.
const STATE_COOKIE: &str = "mf_oauth_state";

/// The cookie that carries a sign-in waiting for its authenticator code.
const PENDING_COOKIE: &str = "mf_oauth_pending";

/// Both cookies are sent only to these routes.
const COOKIE_PATH: &str = "/api/auth/oauth";

/// How long a started flow, and a pending two-factor step, stay usable.
const FLOW_TTL_SECS: u32 = 10 * 60;

/// Wrong authenticator codes one pending sign-in survives.
const PENDING_MAX_MISSES: u32 = 5;

/// Authenticator attempts one client address may make per window.
const TOTP_IP_LIMIT: u32 = 30;

/// That window, in seconds.
const TOTP_IP_WINDOW: u32 = 10 * 60;

/// The user agent sent to the providers; GitHub refuses a request without one.
const USER_AGENT: &str = "Micaforge sign-in";

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

/// Where one provider's endpoints are.
#[derive(Debug, Clone)]
pub(crate) struct Endpoints {
    authorize: String,
    token: String,
    profile: String,
    /// GitHub only: the list of addresses with their verified flags.
    emails: Option<String>,
    scope: &'static str,
}

impl Endpoints {
    /// The documented endpoints of a provider this build knows.
    fn of(provider: &str) -> Option<Endpoints> {
        match provider {
            "google" => Some(Endpoints {
                authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token: "https://oauth2.googleapis.com/token".into(),
                profile: "https://openidconnect.googleapis.com/v1/userinfo".into(),
                emails: None,
                scope: "openid email profile",
            }),
            "github" => Some(Endpoints {
                authorize: "https://github.com/login/oauth/authorize".into(),
                token: "https://github.com/login/oauth/access_token".into(),
                profile: "https://api.github.com/user".into(),
                emails: Some("https://api.github.com/user/emails".into()),
                scope: "read:user user:email",
            }),
            _ => None,
        }
    }
}

/// Why a sign-in through a provider was turned away. The wire code is the only
/// thing the browser is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The state did not match, expired, or was used.
    State,
    /// The person cancelled at the provider.
    Denied,
    /// No verified address to sign in with.
    UnverifiedEmail,
    /// The install is closed to this address.
    Closed,
    /// The account is already linked to another account at this provider.
    Linked,
    /// The provider answered badly, or not at all.
    Provider,
    /// This provider is not configured here.
    Disabled,
}

impl Refusal {
    fn code(self) -> &'static str {
        match self {
            Refusal::State => "state",
            Refusal::Denied => "denied",
            Refusal::UnverifiedEmail => "unverified_email",
            Refusal::Closed => "closed",
            Refusal::Linked => "linked",
            Refusal::Provider => "provider",
            Refusal::Disabled => "disabled",
        }
    }
}

/// What a provider says about the person, once checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Profile {
    /// The provider's stable id.
    subject: String,
    /// A verified address, lower-cased.
    email: String,
    /// A display name, when the provider has one.
    name: Option<String>,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every OAuth route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/oauth/{provider}/start", get(start))
        .route("/api/auth/oauth/{provider}/callback", get(callback))
        .route("/api/auth/oauth/totp", post(totp))
        .route("/api/auth/oauth/identities", get(list_identities))
        .route("/api/auth/oauth/identities/{provider}", delete(unlink))
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

/// Where to go after signing in.
#[derive(Debug, Default, Deserialize)]
struct StartQuery {
    #[serde(default)]
    next: Option<String>,
}

/// One started flow, as Valkey holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FlowRecord {
    provider: String,
    verifier: String,
    next: String,
}

/// `GET /api/auth/oauth/{provider}/start`
async fn start(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Redirect)> {
    let (Some(client), Some(endpoints)) =
        (st.cfg.oauth_client(&provider), Endpoints::of(&provider))
    else {
        return Ok((jar, refuse(&st.cfg, Refusal::Disabled)));
    };

    let state = crypto::random_token(32);
    let verifier = crypto::random_token(32);
    let record = FlowRecord {
        provider: provider.clone(),
        verifier: verifier.clone(),
        next: safe_next(q.next.as_deref()),
    };
    st.kv
        .set_json(&flow_key(&st.cfg, &state), &record, Some(FLOW_TTL_SECS))
        .await?;

    let url = authorize_url(
        &endpoints,
        client,
        &redirect_uri(&st.cfg, &provider),
        &state,
        &pkce_challenge(&verifier),
    );
    let jar = jar.add(flow_cookie(&st.cfg, STATE_COOKIE, &state, FLOW_TTL_SECS)?);
    Ok((jar, Redirect::to(&url)))
}

/// The address the provider sends the browser back to. Registered with the
/// provider exactly as written here.
fn redirect_uri(cfg: &Config, provider: &str) -> String {
    format!("{}/api/auth/oauth/{provider}/callback", cfg.public_url)
}

/// The provider's authorisation address, with every parameter the flow needs.
fn authorize_url(
    endpoints: &Endpoints,
    client: &OAuthClient,
    redirect: &str,
    state: &str,
    challenge: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", client.client_id.as_str()),
        ("redirect_uri", redirect),
        ("scope", endpoints.scope),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ];
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect();
    format!("{}?{}", endpoints.authorize, query.join("&"))
}

/// The PKCE `S256` challenge for a verifier: `BASE64URL(SHA256(verifier))`.
pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A path inside the dashboard to land on, or `/app`.
///
/// Only an absolute path under `/app` is followed. Anything else (another
/// host, a `//` scheme-relative address, a backslash some browsers read as a
/// slash) would make the sign-in an open redirect.
fn safe_next(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        Some(path)
            if (path == "/app" || path.starts_with("/app/") || path.starts_with("/app?"))
                && !path.contains("//")
                && !path.contains('\\')
                && path.len() <= 512 =>
        {
            path.to_string()
        }
        _ => "/app".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Callback
// ---------------------------------------------------------------------------

/// What the provider sends back.
#[derive(Debug, Default, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// A sign-in waiting for its authenticator code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Pending {
    user_id: Uuid,
    provider: String,
    next: String,
    #[serde(default)]
    misses: u32,
}

/// `GET /api/auth/oauth/{provider}/callback`
async fn callback(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<CallbackQuery>,
    ClientAddr(ip): ClientAddr,
    headers: HeaderMap,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Redirect)> {
    // The state cookie is single use whatever happens next.
    let cookie_state = jar.get(STATE_COOKIE).map(|c| c.value().to_string());
    let jar = jar.add(flow_cookie(&st.cfg, STATE_COOKIE, "", 0)?);

    match sign_in(&st, &provider, &q, cookie_state.as_deref()).await {
        Ok(Outcome::SignedIn { user, next, linked }) => {
            let (jar, _) = open_session(&st, jar, &user, &headers, &ip).await?;
            record_sign_in(&st, &user, &provider, linked).await;
            Ok((jar, Redirect::to(&dashboard(&st.cfg, &next))))
        }
        Ok(Outcome::NeedsTotp(pending)) => {
            let token = crypto::random_token(32);
            st.kv
                .set_json(&pending_key(&st.cfg, &token), &pending, Some(FLOW_TTL_SECS))
                .await?;
            let jar = jar.add(flow_cookie(&st.cfg, PENDING_COOKIE, &token, FLOW_TTL_SECS)?);
            Ok((
                jar,
                Redirect::to(&dashboard(&st.cfg, "/app/login?oauth=totp")),
            ))
        }
        Err(refusal) => {
            tracing::info!(
                provider,
                reason = refusal.code(),
                "a provider sign-in was refused"
            );
            Ok((jar, refuse(&st.cfg, refusal)))
        }
    }
}

/// How a callback ended, short of a refusal.
enum Outcome {
    /// Signed in. `linked` is true when this sign-in linked the identity.
    SignedIn {
        user: users::User,
        next: String,
        linked: bool,
    },
    /// The account has two-factor on.
    NeedsTotp(Pending),
}

/// Everything between the provider's redirect and a session.
async fn sign_in(
    st: &AppState,
    provider: &str,
    q: &CallbackQuery,
    cookie_state: Option<&str>,
) -> std::result::Result<Outcome, Refusal> {
    let (Some(client), Some(endpoints)) = (st.cfg.oauth_client(provider), Endpoints::of(provider))
    else {
        return Err(Refusal::Disabled);
    };
    if let Some(error) = q.error.as_deref() {
        return Err(if error == "access_denied" {
            Refusal::Denied
        } else {
            Refusal::Provider
        });
    }

    let returned = q.state.as_deref().unwrap_or("");
    let flow = consume_flow(st, returned, cookie_state, provider)
        .await
        .map_err(|_| Refusal::State)?
        .ok_or(Refusal::State)?;
    let code = q
        .code
        .as_deref()
        .filter(|c| !c.is_empty())
        .ok_or(Refusal::Provider)?;

    let token = exchange(
        &st.http,
        &endpoints,
        client,
        code,
        &flow.verifier,
        &redirect_uri(&st.cfg, provider),
    )
    .await?;
    let profile = fetch_profile(&st.http, provider, &endpoints, &token).await?;

    let (user, linked) = resolve(st, provider, &profile).await?;
    if user.has_totp() {
        return Ok(Outcome::NeedsTotp(Pending {
            user_id: user.id,
            provider: provider.to_string(),
            next: flow.next,
            misses: 0,
        }));
    }
    Ok(Outcome::SignedIn {
        user,
        next: flow.next,
        linked,
    })
}

/// Check the returned state against the cookie and consume its record.
///
/// `None` for every way it can fail: no cookie, a cookie that differs, no
/// record, a record for another provider, or a record another request
/// consumed first. They are one answer on purpose.
async fn consume_flow(
    st: &AppState,
    returned: &str,
    cookie: Option<&str>,
    provider: &str,
) -> Result<Option<FlowRecord>> {
    if !states_match(returned, cookie) {
        return Ok(None);
    }
    let key = flow_key(&st.cfg, returned);
    let Some(seen) = st.kv.get_snapshot::<FlowRecord>(&key).await? else {
        return Ok(None);
    };
    if seen.value.provider != provider {
        return Ok(None);
    }
    if !st.kv.swap_json(&key, &seen, None).await? {
        return Ok(None);
    }
    Ok(Some(seen.value))
}

/// Does the state the provider returned equal the browser's cookie?
fn states_match(returned: &str, cookie: Option<&str>) -> bool {
    match cookie {
        Some(cookie) if !returned.is_empty() && !cookie.is_empty() => {
            returned.as_bytes().ct_eq(cookie.as_bytes()).into()
        }
        _ => false,
    }
}

/// Find or create the account for a verified profile.
///
/// Returns the account and whether this sign-in linked the identity to it.
async fn resolve(
    st: &AppState,
    provider: &str,
    profile: &Profile,
) -> std::result::Result<(users::User, bool), Refusal> {
    let failed = |e: Error| {
        tracing::warn!(provider, error = %e, "a provider sign-in failed on the server");
        Refusal::Provider
    };

    if let Some(identity) = identities::find(&st.pg, provider, &profile.subject)
        .await
        .map_err(failed)?
    {
        let user = users::get(&st.pg, identity.user_id).await.map_err(failed)?;
        identities::touch(&st.pg, provider, &profile.subject, &profile.email)
            .await
            .map_err(failed)?;
        return Ok((user, false));
    }

    if let Some(user) = users::find_by_email(&st.pg, &profile.email)
        .await
        .map_err(failed)?
    {
        return match identities::link(&st.pg, provider, &profile.subject, user.id, &profile.email)
            .await
        {
            Ok(_) => Ok((user, true)),
            Err(Error::Conflict(_)) => Err(Refusal::Linked),
            Err(e) => Err(failed(e)),
        };
    }

    let account = NewAccount::from_provider(profile.name.as_deref());
    match open_account(st, &profile.email, &account, async { Ok(true) })
        .await
        .map_err(failed)?
    {
        Opened::Created { user, .. } => {
            identities::link(&st.pg, provider, &profile.subject, user.id, &profile.email)
                .await
                .map_err(failed)?;
            Ok((user, true))
        }
        Opened::Closed => Err(Refusal::Closed),
        Opened::Lost => Err(Refusal::Provider),
    }
}

/// Mark the address verified, and record the sign-in (and a new link).
async fn record_sign_in(st: &AppState, user: &users::User, provider: &str, linked: bool) {
    if let Err(e) = users::mark_email_verified(&st.pg, user.id).await {
        tracing::warn!(user = %user.id, error = %e, "could not mark the address verified");
    }
    if linked {
        audit::record_as(
            st,
            Actor::user(user),
            Entry::new("auth.oauth_linked")
                .target("account", user.id, &user.email)
                .meta(json!({ "provider": provider })),
        )
        .await;
    }
    audit::record_as(
        st,
        Actor::user(user),
        Entry::new("auth.signed_in_oauth")
            .target("account", user.id, &user.email)
            .meta(json!({ "provider": provider })),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Talking to the provider
// ---------------------------------------------------------------------------

/// Trade the code for an access token, proving the flow with the verifier.
///
/// GitHub answers a bad code with `200` and an `error` field, so the body is
/// read either way and a missing token is the failure.
pub(crate) async fn exchange(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    client: &OAuthClient,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> std::result::Result<String, Refusal> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect),
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
        ("code_verifier", verifier),
    ];
    let body: Value = http
        .post(&endpoints.token)
        .header("accept", "application/json")
        .header("user-agent", USER_AGENT)
        .form(&form)
        .send()
        .await
        .map_err(|_| Refusal::Provider)?
        .json()
        .await
        .map_err(|_| Refusal::Provider)?;
    body.get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .ok_or(Refusal::Provider)
}

/// Read and check the person's profile.
pub(crate) async fn fetch_profile(
    http: &reqwest::Client,
    provider: &str,
    endpoints: &Endpoints,
    token: &str,
) -> std::result::Result<Profile, Refusal> {
    let get = |url: &str| {
        http.get(url)
            .bearer_auth(token)
            .header("accept", "application/json")
            .header("user-agent", USER_AGENT)
            .send()
    };
    let read = |response: std::result::Result<reqwest::Response, reqwest::Error>| async {
        let response = response.map_err(|_| Refusal::Provider)?;
        if !response.status().is_success() {
            return Err(Refusal::Provider);
        }
        response
            .json::<Value>()
            .await
            .map_err(|_| Refusal::Provider)
    };

    let user = read(get(&endpoints.profile).await).await?;
    match (provider, endpoints.emails.as_deref()) {
        ("github", Some(emails_url)) => {
            let emails = read(get(emails_url).await).await?;
            github_profile(&user, &emails)
        }
        ("google", _) => google_profile(&user),
        _ => Err(Refusal::Provider),
    }
}

/// Google's userinfo, checked: a subject, an address, and `email_verified`
/// true (Google has sent it both as a boolean and as a string).
pub(crate) fn google_profile(user: &Value) -> std::result::Result<Profile, Refusal> {
    let subject = user
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(Refusal::Provider)?;
    let verified = match user.get("email_verified") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    };
    let raw = user.get("email").and_then(Value::as_str).unwrap_or("");
    if raw.is_empty() || !verified {
        return Err(Refusal::UnverifiedEmail);
    }
    let email = clean_email(raw).map_err(|_| Refusal::UnverifiedEmail)?;
    Ok(Profile {
        subject: subject.to_string(),
        email,
        name: user.get("name").and_then(Value::as_str).map(str::to_string),
    })
}

/// GitHub's user and address list, checked: the numeric id as the subject, and
/// the address that is both primary and verified. Not any verified address:
/// a secondary address is one somebody added, and the primary is the one the
/// person signs in to GitHub with.
pub(crate) fn github_profile(
    user: &Value,
    emails: &Value,
) -> std::result::Result<Profile, Refusal> {
    let subject = user
        .get("id")
        .and_then(|id| id.as_u64().map(|n| n.to_string()))
        .ok_or(Refusal::Provider)?;
    let primary = emails
        .as_array()
        .and_then(|list| {
            list.iter().find(|e| {
                e.get("primary").and_then(Value::as_bool) == Some(true)
                    && e.get("verified").and_then(Value::as_bool) == Some(true)
            })
        })
        .and_then(|e| e.get("email"))
        .and_then(Value::as_str)
        .ok_or(Refusal::UnverifiedEmail)?;
    let email = clean_email(primary).map_err(|_| Refusal::UnverifiedEmail)?;
    let name = user
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .or_else(|| user.get("login").and_then(Value::as_str))
        .map(str::to_string);
    Ok(Profile {
        subject,
        email,
        name,
    })
}

// ---------------------------------------------------------------------------
// The two-factor step
// ---------------------------------------------------------------------------

/// The authenticator code.
#[derive(Deserialize)]
struct TotpBody {
    totp: String,
}

/// `POST /api/auth/oauth/totp` — finish a provider sign-in on an account with
/// two-factor on. Answers like `POST /api/auth/code/verify` does on success,
/// plus `next`.
async fn totp(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<TotpBody>,
) -> ApiResult<(CookieJar, Json<Value>)> {
    let expired = || Error::Unauthorized("that sign-in has expired; start it again".into());
    if !st
        .kv
        .rate_limit(
            &format!("oauth-totp:ip:{}", address_tag(ip)),
            TOTP_IP_LIMIT,
            TOTP_IP_WINDOW,
        )
        .await?
    {
        return Err(Error::RateLimited {
            retry_after: u64::from(TOTP_IP_WINDOW),
        }
        .into());
    }

    let token = jar
        .get(PENDING_COOKIE)
        .map(|c| c.value().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(expired)?;
    let key = pending_key(&st.cfg, &token);
    let Some(seen) = st.kv.get_snapshot::<Pending>(&key).await? else {
        return Err(expired().into());
    };
    let user = users::get(&st.pg, seen.value.user_id).await?;

    if let Some(sealed) = user.totp_secret_sealed.as_deref() {
        if !verify_totp_secret(&st.cfg, &user, sealed, body.totp.trim())? {
            let misses = seen.value.misses + 1;
            let next = (misses < PENDING_MAX_MISSES).then(|| Pending {
                misses,
                ..seen.value.clone()
            });
            st.kv.swap_json(&key, &seen, next.as_ref()).await?;
            return Err(Error::Unauthorized("that authenticator code is not right".into()).into());
        }
    }
    if !st.kv.swap_json(&key, &seen, None).await? {
        return Err(expired().into());
    }

    let jar = jar.add(flow_cookie(&st.cfg, PENDING_COOKIE, "", 0)?);
    let (jar, session) = open_session(&st, jar, &user, &headers, &ip).await?;
    record_sign_in(&st, &user, &seen.value.provider, false).await;
    Ok((
        jar,
        ok(json!({
            "status": "ok",
            "user": user_json(&user, false),
            "expires_at": session.expires_at,
            "created": false,
            "joined": Vec::<Uuid>::new(),
            "next": seen.value.next,
        })),
    ))
}

// ---------------------------------------------------------------------------
// Linked identities
// ---------------------------------------------------------------------------

/// `GET /api/auth/oauth/identities` — which providers this account is linked
/// to.
async fn list_identities(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let rows = identities::list_for_user(&st.pg, auth.user.id).await?;
    Ok(ok(rows
        .iter()
        .map(|r| {
            json!({
                "provider": r.provider,
                "email": r.email,
                "created_at": r.created_at,
                "last_used_at": r.last_used_at,
            })
        })
        .collect::<Vec<_>>()))
}

/// `DELETE /api/auth/oauth/identities/{provider}` — unlink a provider.
///
/// Always safe: the emailed code keeps working for the account's address, so
/// unlinking can never lock anybody out.
async fn unlink(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    if Endpoints::of(&provider).is_none() {
        return Err(Error::not_found("linked account").into());
    }
    if !identities::unlink(&st.pg, auth.user.id, &provider).await? {
        return Err(Error::not_found("linked account").into());
    }
    audit::record(
        &st,
        &auth,
        Entry::new("auth.oauth_unlinked")
            .target("account", auth.user.id, &auth.user.email)
            .meta(json!({ "provider": provider })),
    )
    .await;
    Ok(ok(json!({ "unlinked": provider })))
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

/// `mf:oauth:flow:<keyed digest of the state>`.
fn flow_key(cfg: &Config, state: &str) -> String {
    format!(
        "mf:oauth:flow:{}",
        crypto::keyed_digest(&cfg.derive_key("oauth-state"), &[state])
    )
}

/// `mf:oauth:pending:<keyed digest of the token>`.
fn pending_key(cfg: &Config, token: &str) -> String {
    format!(
        "mf:oauth:pending:{}",
        crypto::keyed_digest(&cfg.derive_key("oauth-pending"), &[token])
    )
}

/// A short-lived cookie for these routes only. `SameSite=Lax`, because the
/// provider's redirect back is a top-level navigation from another site, and
/// `Strict` would leave the cookie behind.
fn flow_cookie(cfg: &Config, name: &str, value: &str, max_age: u32) -> Result<Cookie<'static>> {
    let secure = if cfg.cookies_are_secure() {
        "; Secure"
    } else {
        ""
    };
    Cookie::parse(format!(
        "{name}={value}; Path={COOKIE_PATH}; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .map_err(|e| Error::internal(format!("could not build the sign-in cookie: {e}")))
}

/// A dashboard address.
fn dashboard(cfg: &Config, path: &str) -> String {
    format!("{}{path}", cfg.dashboard_url)
}

/// Send the browser back to the sign-in screen with a refusal code.
fn refuse(cfg: &Config, refusal: Refusal) -> Redirect {
    Redirect::to(&dashboard(
        cfg,
        &format!("/app/login?oauth_error={}", refusal.code()),
    ))
}

/// Percent-encode everything outside the unreserved set.
fn encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pkce_challenge_matches_rfc_7636() {
        // Appendix B of RFC 7636.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        // A verifier as minted here is within the RFC's alphabet and length.
        let verifier = crypto::random_token(32);
        assert!((43..=128).contains(&verifier.len()));
        assert!(verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));
    }

    #[test]
    fn the_state_must_equal_the_cookie_exactly() {
        assert!(states_match("abc", Some("abc")));
        assert!(!states_match("abc", Some("abd")));
        assert!(!states_match("abc", None), "no cookie, no sign-in");
        assert!(!states_match("", Some("")), "empty is never a state");
        assert!(!states_match("abc", Some("abcd")));
    }

    #[test]
    fn the_authorize_url_carries_state_and_challenge() {
        let endpoints = Endpoints::of("github").unwrap();
        let client = OAuthClient {
            client_id: "client 1".into(),
            client_secret: "secret".into(),
        };
        let url = authorize_url(
            &endpoints,
            &client,
            "https://mf.example.com/api/auth/oauth/github/callback",
            "st-1",
            "ch-1",
        );
        assert!(url.starts_with("https://github.com/login/oauth/authorize?"));
        assert!(url.contains("client_id=client%201"));
        assert!(url.contains("state=st-1"));
        assert!(url.contains("code_challenge=ch-1&code_challenge_method=S256"));
        assert!(url.contains(
            "redirect_uri=https%3A%2F%2Fmf.example.com%2Fapi%2Fauth%2Foauth%2Fgithub%2Fcallback"
        ));
        // The secret never travels through the browser.
        assert!(!url.contains("secret"));
    }

    #[test]
    fn only_a_dashboard_path_is_followed_after_sign_in() {
        assert_eq!(safe_next(Some("/app/3/agents")), "/app/3/agents");
        assert_eq!(safe_next(Some("/app")), "/app");
        for bad in [
            "https://evil.example",
            "//evil.example/app",
            "/app//evil.example",
            "/\\evil.example",
            "/application",
            "/share/x",
            "",
        ] {
            assert_eq!(safe_next(Some(bad)), "/app", "{bad}");
        }
        assert_eq!(safe_next(None), "/app");
    }

    #[test]
    fn google_needs_a_verified_address() {
        let ok = google_profile(&json!({
            "sub": "1234", "email": "Ada@Example.com", "email_verified": true, "name": "Ada"
        }))
        .unwrap();
        assert_eq!(ok.email, "ada@example.com");
        assert_eq!(ok.subject, "1234");

        let as_string = google_profile(&json!({
            "sub": "1", "email": "a@example.com", "email_verified": "true"
        }));
        assert!(as_string.is_ok());

        assert_eq!(
            google_profile(
                &json!({ "sub": "1", "email": "a@example.com", "email_verified": false })
            ),
            Err(Refusal::UnverifiedEmail)
        );
        assert_eq!(
            google_profile(&json!({ "sub": "1", "email": "a@example.com" })),
            Err(Refusal::UnverifiedEmail)
        );
        assert_eq!(
            google_profile(&json!({ "email": "a@example.com", "email_verified": true })),
            Err(Refusal::Provider)
        );
    }

    #[test]
    fn github_uses_the_primary_verified_address_only() {
        let user = json!({ "id": 42, "login": "ada", "name": null });
        let emails = json!([
            { "email": "work@example.com", "primary": false, "verified": true },
            { "email": "Ada@Example.com", "primary": true, "verified": true }
        ]);
        let profile = github_profile(&user, &emails).unwrap();
        assert_eq!(profile.email, "ada@example.com");
        assert_eq!(profile.subject, "42");
        assert_eq!(
            profile.name.as_deref(),
            Some("ada"),
            "the login when there is no name"
        );

        // A verified secondary is not used when the primary is unverified.
        let unverified_primary = json!([
            { "email": "work@example.com", "primary": false, "verified": true },
            { "email": "ada@example.com", "primary": true, "verified": false }
        ]);
        assert_eq!(
            github_profile(&user, &unverified_primary),
            Err(Refusal::UnverifiedEmail)
        );
        assert_eq!(
            github_profile(&user, &json!([])),
            Err(Refusal::UnverifiedEmail)
        );
        assert_eq!(
            github_profile(&json!({ "login": "ada" }), &emails),
            Err(Refusal::Provider)
        );
    }

    #[test]
    fn refusal_codes_are_the_ones_the_screen_knows() {
        let codes: Vec<&str> = [
            Refusal::State,
            Refusal::Denied,
            Refusal::UnverifiedEmail,
            Refusal::Closed,
            Refusal::Linked,
            Refusal::Provider,
            Refusal::Disabled,
        ]
        .iter()
        .map(|r| r.code())
        .collect();
        assert_eq!(
            codes,
            vec![
                "state",
                "denied",
                "unverified_email",
                "closed",
                "linked",
                "provider",
                "disabled"
            ]
        );
    }

    /// The exchange and the profile reads against a local stand-in for GitHub:
    /// the verifier and the secret reach the token endpoint, the token reaches
    /// the profile endpoints, and the primary verified address comes back.
    #[tokio::test]
    async fn the_code_is_exchanged_with_its_verifier() {
        use axum::extract::Form;
        use std::collections::HashMap;

        async fn token(Form(form): Form<HashMap<String, String>>) -> Json<Value> {
            let good = form.get("code_verifier").map(String::as_str) == Some("the-verifier")
                && form.get("client_secret").map(String::as_str) == Some("s3cret")
                && form.get("grant_type").map(String::as_str) == Some("authorization_code");
            if good {
                Json(json!({ "access_token": "tok-1", "token_type": "bearer" }))
            } else {
                // GitHub's way of saying no: a 200 with an error.
                Json(json!({ "error": "bad_verification_code" }))
            }
        }
        async fn user(headers: HeaderMap) -> axum::response::Response {
            use axum::response::IntoResponse;
            if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer tok-1") {
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
            Json(json!({ "id": 7, "login": "octo", "name": "Octo Cat" })).into_response()
        }
        async fn emails() -> Json<Value> {
            Json(json!([{ "email": "octo@example.com", "primary": true, "verified": true }]))
        }

        let app = axum::Router::new()
            .route("/token", axum::routing::post(token))
            .route("/user", get(user))
            .route("/emails", get(emails));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let endpoints = Endpoints {
            authorize: format!("{base}/authorize"),
            token: format!("{base}/token"),
            profile: format!("{base}/user"),
            emails: Some(format!("{base}/emails")),
            scope: "read:user user:email",
        };
        let client = OAuthClient {
            client_id: "id".into(),
            client_secret: "s3cret".into(),
        };
        let http = reqwest::Client::new();

        let wrong = exchange(&http, &endpoints, &client, "code", "not-it", "https://x/cb").await;
        assert_eq!(wrong, Err(Refusal::Provider));

        let tok = exchange(
            &http,
            &endpoints,
            &client,
            "code",
            "the-verifier",
            "https://x/cb",
        )
        .await
        .unwrap();
        let profile = fetch_profile(&http, "github", &endpoints, &tok)
            .await
            .unwrap();
        assert_eq!(profile.email, "octo@example.com");
        assert_eq!(profile.subject, "7");
        assert_eq!(profile.name.as_deref(), Some("Octo Cat"));

        let refused = fetch_profile(&http, "github", &endpoints, "stolen").await;
        assert_eq!(refused, Err(Refusal::Provider));
    }

    #[test]
    fn flow_keys_hold_no_state_and_differ_by_purpose() {
        let mut cfg = crate::routes::tests::cfg();
        cfg.secret_key_base = "k".repeat(48);
        let flow = flow_key(&cfg, "state-1");
        assert!(!flow.contains("state-1"));
        assert_ne!(
            flow.trim_start_matches("mf:oauth:flow:"),
            pending_key(&cfg, "state-1").trim_start_matches("mf:oauth:pending:")
        );
    }
}
