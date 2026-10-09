//! OAuth 2.1 for the MCP server, per the MCP authorization specification.
//!
//! An MCP client that meets a `401` on `/api/mcp` reads the
//! `WWW-Authenticate` header, finds the protected resource metadata
//! (RFC 9728), follows it to this server's authorization server metadata
//! (RFC 8414), registers itself (RFC 7591), and sends the person to the
//! authorization endpoint with a PKCE challenge (RFC 7636) and the resource it
//! wants a token for (RFC 8707). The person approves in the dashboard, choosing
//! the organisation and the scopes; the client trades the code for tokens.
//!
//! ## What is deliberately strict
//!
//! - **PKCE with S256, always.** OAuth 2.1 requires it for every client, and
//!   `plain` proves nothing a network observer could not repeat.
//! - **Exact redirect matching.** A registered address is matched as a whole
//!   string, with one exception RFC 8252 makes for native apps: a loopback
//!   `http` address may use any port, because the port is chosen at run time.
//! - **One audience.** Every token is bound to `{public_url}/api/mcp` and works
//!   nowhere else ([`crate::extract`] refuses it on every other route).
//! - **Rotating refresh tokens.** Each refresh issues a new pair and retires
//!   the old refresh token; presenting a retired one is treated as theft and
//!   revokes the whole grant, which is the reuse detection OAuth 2.1 asks of
//!   public clients.
//! - **Nothing usable at rest.** Codes live ten minutes in Valkey under a digest
//!   and are consumed with a compare-and-swap; tokens and client secrets are
//!   stored as SHA-256 digests.
//!
//! ## Two groups of routes
//!
//! [`public`] is what a client calls with no session: the metadata, the
//! registration, the token and revocation endpoints, and the authorization
//! redirect. It answers any origin, without credentials, because a browser
//! based MCP client must be able to read it. [`router`] is the dashboard's: the
//! consent screen's reads and writes and the "Connected apps" list, behind the
//! session cookie and the ordinary origin allow-list.

use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use chrono::{Duration, Utc};
use micaforge_core::{crypto, new_id, Error, Result};
use micaforge_store::models::oauth::{self, Client, Grant, TokenPair};
use micaforge_store::models::{orgs, users};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, ClientAddr};
use crate::response::ok;
use crate::state::AppState;

/// What every dashboard handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Access tokens start with this. [`crate::extract`] routes on it.
pub const ACCESS_PREFIX: &str = "mfo_";
/// Refresh tokens start with this.
pub const REFRESH_PREFIX: &str = "mfr_";
/// Public client identifiers start with this.
const CLIENT_ID_PREFIX: &str = "mfc_";
/// Client secrets start with this.
const CLIENT_SECRET_PREFIX: &str = "mfcs_";

/// How much of an access token is the indexed lookup key, as for API keys.
const LOOKUP_LEN: usize = 16;

/// An access token's life. Short, because it is the bearer credential sent on
/// every call; the refresh token is the long-lived half.
const ACCESS_TTL_SECS: i64 = 60 * 60;
/// A refresh token's life, renewed on every rotation.
const REFRESH_TTL_DAYS: i64 = 30;
/// An authorization code's life.
const CODE_TTL_SECS: u32 = 10 * 60;

/// Most redirect addresses one registration may list.
const MAX_REDIRECTS: usize = 10;
/// Registrations per address per hour.
const REGISTER_LIMIT: u32 = 20;

/// The scopes a person may grant, and what each means in words.
pub const SCOPES: &[(&str, &str, &str, bool)] = &[
    (
        "stats:read",
        "Read your analytics",
        "Read reports for the organisation's sites, ask the AI analyst, and send feedback.",
        true,
    ),
    (
        "manage:write",
        "Create goals and annotations",
        "Create goals, and create and delete chart annotations. Never more than a member can do.",
        false,
    ),
];

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// The authorization server's issuer: the API's public origin.
pub fn issuer(st: &AppState) -> String {
    st.cfg.public_url.trim_end_matches('/').to_string()
}

/// The one resource tokens are issued for.
pub fn mcp_resource(st: &AppState) -> String {
    format!("{}/api/mcp", issuer(st))
}

/// Where the protected resource metadata for the MCP endpoint lives.
pub fn resource_metadata_url(st: &AppState) -> String {
    format!(
        "{}/.well-known/oauth-protected-resource/api/mcp",
        issuer(st)
    )
}

/// The `WWW-Authenticate` value a `401` from the MCP endpoint carries.
pub fn www_authenticate(st: &AppState) -> String {
    format!(
        "Bearer resource_metadata=\"{}\", scope=\"stats:read\"",
        resource_metadata_url(st)
    )
}

// ---------------------------------------------------------------------------
// Routers
// ---------------------------------------------------------------------------

/// The dashboard's routes: consent and connected apps.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/oauth/consent",
            get(consent_preview).post(consent_decide),
        )
        .route("/api/oauth/grants", get(list_grants))
        .route("/api/oauth/grants/{id}", delete(revoke_grant))
}

/// The routes a client calls without a session.
pub fn public() -> Router<AppState> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/api/mcp",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server),
        )
        .route("/api/oauth/register", post(register))
        .route("/api/oauth/authorize", get(authorize))
        .route("/api/oauth/token", post(token))
        .route("/api/oauth/revoke", post(revoke))
}

// ---------------------------------------------------------------------------
// OAuth's own error shape
// ---------------------------------------------------------------------------

/// An error in OAuth's dialect: `{ "error", "error_description" }`.
///
/// The token, registration and revocation endpoints answer with this rather
/// than the API's envelope, because the clients calling them are generic OAuth
/// libraries that read `error`.
#[derive(Debug)]
struct OAuthError {
    status: StatusCode,
    code: &'static str,
    description: String,
}

impl OAuthError {
    fn new(code: &'static str, description: impl Into<String>) -> OAuthError {
        let status = if code == "invalid_client" {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::BAD_REQUEST
        };
        OAuthError {
            status,
            code,
            description: description.into(),
        }
    }

    fn server(e: &Error) -> OAuthError {
        tracing::error!(error = %e, "oauth endpoint failed");
        OAuthError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "server_error",
            description: "the server could not complete the request".to_string(),
        }
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(json!({ "error": self.code, "error_description": self.description })),
        )
            .into_response();
        no_store(response.headers_mut());
        response
    }
}

/// Token responses must not be cached by anything between here and the client.
fn no_store(headers: &mut HeaderMap) {
    headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    headers.insert(header::PRAGMA, "no-cache".parse().expect("static header"));
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// `GET /.well-known/oauth-protected-resource[/api/mcp]` (RFC 9728).
async fn protected_resource(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "resource": mcp_resource(&st),
        "authorization_servers": [issuer(&st)],
        "scopes_supported": scope_names(),
        "bearer_methods_supported": ["header"],
        "resource_name": "Micaforge MCP server",
        "resource_documentation": format!("{}/docs/api/mcp-server", st.cfg.dashboard_url.trim_end_matches('/')),
    }))
}

/// `GET /.well-known/oauth-authorization-server` (RFC 8414).
async fn authorization_server(State(st): State<AppState>) -> Json<Value> {
    let base = issuer(&st);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/api/oauth/authorize"),
        "token_endpoint": format!("{base}/api/oauth/token"),
        "registration_endpoint": format!("{base}/api/oauth/register"),
        "revocation_endpoint": format!("{base}/api/oauth/revoke"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
        "revocation_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
        "scopes_supported": scope_names(),
        "authorization_response_iss_parameter_supported": true,
        "service_documentation": format!("{}/docs/api/mcp-server", st.cfg.dashboard_url.trim_end_matches('/')),
    }))
}

fn scope_names() -> Vec<&'static str> {
    SCOPES.iter().map(|(name, ..)| *name).collect()
}

// ---------------------------------------------------------------------------
// Registration (RFC 7591)
// ---------------------------------------------------------------------------

/// A registration request. Unknown fields are ignored, as the RFC asks.
#[derive(Debug, Deserialize)]
struct Registration {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
    #[serde(default)]
    client_uri: Option<String>,
}

/// `POST /api/oauth/register`.
async fn register(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    body: Bytes,
) -> std::result::Result<Response, OAuthError> {
    let allowed = st
        .kv
        .rate_limit(&format!("oauth:register:{ip}"), REGISTER_LIMIT, 3600)
        .await
        .map_err(|e| OAuthError::server(&e))?;
    if !allowed {
        return Err(OAuthError::new(
            "invalid_request",
            "too many registrations from this address; try again later",
        ));
    }

    let request: Registration = serde_json::from_slice(&body).map_err(|e| {
        OAuthError::new(
            "invalid_client_metadata",
            format!("the body is not a registration: {e}"),
        )
    })?;
    if request.redirect_uris.is_empty() || request.redirect_uris.len() > MAX_REDIRECTS {
        return Err(OAuthError::new(
            "invalid_redirect_uri",
            format!("between 1 and {MAX_REDIRECTS} redirect_uris are required"),
        ));
    }
    for uri in &request.redirect_uris {
        if !redirect_uri_allowed(uri) {
            return Err(OAuthError::new(
                "invalid_redirect_uri",
                format!("`{uri}` must be https, http on a loopback address, or a private-use scheme, with no fragment"),
            ));
        }
    }
    let method = request
        .token_endpoint_auth_method
        .as_deref()
        .unwrap_or("none")
        .to_string();
    if !["none", "client_secret_post", "client_secret_basic"].contains(&method.as_str()) {
        return Err(OAuthError::new(
            "invalid_client_metadata",
            "token_endpoint_auth_method must be none, client_secret_post or client_secret_basic",
        ));
    }
    if let Some(grants) = &request.grant_types {
        if grants
            .iter()
            .any(|g| g != "authorization_code" && g != "refresh_token")
        {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "only the authorization_code and refresh_token grants are supported",
            ));
        }
    }
    if let Some(types) = &request.response_types {
        if types.iter().any(|t| t != "code") {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "only the code response type is supported",
            ));
        }
    }
    let name: String = request
        .client_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Unnamed MCP client")
        .chars()
        .take(80)
        .collect();
    let client_uri = request
        .client_uri
        .as_deref()
        .filter(|u| u.starts_with("https://") && u.len() <= 300);

    let client_id = format!("{CLIENT_ID_PREFIX}{}", crypto::random_token(18));
    let secret =
        (method != "none").then(|| format!("{CLIENT_SECRET_PREFIX}{}", crypto::random_token(32)));
    let client = oauth::create_client(
        &st.pg,
        new_id(),
        &client_id,
        &name,
        &json!(request.redirect_uris),
        &method,
        secret.as_deref().map(crypto::token_hash).as_deref(),
        client_uri,
    )
    .await
    .map_err(|e| OAuthError::server(&e))?;

    let mut answer = json!({
        "client_id": client.client_id,
        "client_id_issued_at": client.created_at.timestamp(),
        "client_name": client.client_name,
        "redirect_uris": client.redirect_uris,
        "token_endpoint_auth_method": client.token_endpoint_auth_method,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "scope": scope_names().join(" "),
    });
    if let Some(secret) = secret {
        answer["client_secret"] = json!(secret);
        answer["client_secret_expires_at"] = json!(0);
    }
    let mut response = (StatusCode::CREATED, Json(answer)).into_response();
    no_store(response.headers_mut());
    Ok(response)
}

/// Whether a redirect address may be registered.
///
/// `https` anywhere; `http` only on a loopback address (a native app's local
/// listener, RFC 8252 §7.3); or a private-use scheme such as `cursor:` or
/// `com.example.app:` (RFC 8252 §7.1). Never a fragment, and never a scheme a
/// browser would run (`javascript:`, `data:`) or read locally (`file:`).
pub fn redirect_uri_allowed(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    if url.fragment().is_some() || raw.len() > 2000 {
        return false;
    }
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => is_loopback(&url),
        "javascript" | "data" | "file" | "blob" | "vbscript" | "about" | "ftp" | "ws" | "wss"
        | "mailto" | "tel" => false,
        _ => true,
    }
}

/// True for `http://localhost`, `127.0.0.0/8` and `[::1]`.
fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Whether a presented redirect address matches a registered one.
///
/// Whole-string equality, except that two loopback `http` addresses that
/// differ only in their port match (RFC 8252 §7.3).
pub fn redirect_matches(registered: &str, presented: &str) -> bool {
    if registered == presented {
        return true;
    }
    let (Ok(a), Ok(b)) = (url::Url::parse(registered), url::Url::parse(presented)) else {
        return false;
    };
    a.scheme() == "http"
        && b.scheme() == "http"
        && is_loopback(&a)
        && is_loopback(&b)
        && a.host() == b.host()
        && a.path() == b.path()
        && a.query() == b.query()
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

/// The parameters of an authorization request.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizeRequest {
    #[serde(default)]
    response_type: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
    #[serde(default)]
    resource: Option<String>,
}

/// An authorization request that passed every check.
#[derive(Debug, Clone)]
struct Checked {
    client: Client,
    redirect_uri: String,
    scopes: Vec<String>,
    state: Option<String>,
    challenge: String,
    resource: String,
}

/// A failed authorization request, and whether it may be reported to the
/// client's redirect address.
///
/// An unknown client or an unregistered redirect address must never be
/// redirected to: that is how an authorization server becomes an open
/// redirector. Those failures are shown to the person in the dashboard. Every
/// other failure goes back to the client, which is what its user is waiting on.
#[derive(Debug)]
enum Refusal {
    ToPerson(String),
    ToClient {
        redirect: String,
        code: &'static str,
        description: String,
        state: Option<String>,
    },
}

/// Check an authorization request against its client.
async fn check(
    st: &AppState,
    req: &AuthorizeRequest,
) -> Result<std::result::Result<Checked, Refusal>> {
    let Some(client_id) = req.client_id.as_deref().filter(|c| !c.is_empty()) else {
        return Ok(Err(Refusal::ToPerson(
            "the request names no client".to_string(),
        )));
    };
    let Some(client) = oauth::find_client(&st.pg, client_id).await? else {
        return Ok(Err(Refusal::ToPerson(
            "this client is not registered here; ask it to register again".to_string(),
        )));
    };
    let registered: Vec<String> =
        serde_json::from_value(client.redirect_uris.clone()).unwrap_or_default();
    let redirect = match req.redirect_uri.as_deref() {
        Some(presented) => match registered.iter().find(|r| redirect_matches(r, presented)) {
            Some(_) => presented.to_string(),
            None => {
                return Ok(Err(Refusal::ToPerson(
                    "the redirect address is not one this client registered".to_string(),
                )))
            }
        },
        // With exactly one registered address it may be left out.
        None if registered.len() == 1 => registered[0].clone(),
        None => {
            return Ok(Err(Refusal::ToPerson(
                "the request must name its redirect address".to_string(),
            )))
        }
    };
    let to_client = |code: &'static str, description: &str| {
        Ok(Err(Refusal::ToClient {
            redirect: redirect.clone(),
            code,
            description: description.to_string(),
            state: req.state.clone(),
        }))
    };

    if req.response_type.as_deref() != Some("code") {
        return to_client("unsupported_response_type", "response_type must be code");
    }
    let Some(challenge) = req.code_challenge.as_deref().filter(|c| valid_pkce_text(c)) else {
        return to_client("invalid_request", "a PKCE code_challenge is required");
    };
    if req.code_challenge_method.as_deref() != Some("S256") {
        return to_client("invalid_request", "code_challenge_method must be S256");
    }
    let resource = mcp_resource(st);
    if let Some(asked) = req.resource.as_deref() {
        if asked.trim_end_matches('/') != resource {
            return to_client(
                "invalid_target",
                "this server issues tokens only for its MCP endpoint",
            );
        }
    }
    let scopes = match parse_scope(req.scope.as_deref()) {
        Some(scopes) => scopes,
        None => {
            return to_client(
                "invalid_scope",
                "unknown scope; use stats:read and manage:write",
            )
        }
    };

    Ok(Ok(Checked {
        client,
        redirect_uri: redirect,
        scopes,
        state: req.state.clone(),
        challenge: challenge.to_string(),
        resource,
    }))
}

/// The requested scopes, defaulting to `stats:read`; `None` when one is unknown.
pub fn parse_scope(raw: Option<&str>) -> Option<Vec<String>> {
    let mut scopes: Vec<String> = Vec::new();
    for scope in raw.unwrap_or("").split_whitespace() {
        if !SCOPES.iter().any(|(name, ..)| *name == scope) {
            return None;
        }
        if !scopes.iter().any(|s| s == scope) {
            scopes.push(scope.to_string());
        }
    }
    if !scopes.iter().any(|s| s == "stats:read") {
        scopes.insert(0, "stats:read".to_string());
    }
    Some(scopes)
}

/// A PKCE verifier or challenge: 43 to 128 characters of the unreserved set.
fn valid_pkce_text(text: &str) -> bool {
    (43..=128).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// The S256 challenge of a verifier: `base64url(sha256(verifier))`.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Does `verifier` answer `challenge`? Constant time.
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    valid_pkce_text(verifier)
        && bool::from(
            pkce_challenge(verifier)
                .as_bytes()
                .ct_eq(challenge.as_bytes()),
        )
}

/// Append query parameters to a redirect address that may already have some.
fn with_params(redirect: &str, params: &[(&str, &str)]) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in params {
        query.append_pair(key, value);
    }
    let sep = if redirect.contains('?') { '&' } else { '?' };
    format!("{redirect}{sep}{}", query.finish())
}

/// `GET /api/oauth/authorize` — hand the person to the dashboard's consent screen.
///
/// Nothing is decided here. The consent screen reads the same query through
/// [`consent_preview`], which checks it with the person signed in, so this
/// endpoint needs no session and sets no cookie.
async fn authorize(State(st): State<AppState>, RawQuery(query): RawQuery) -> Redirect {
    let base = st.cfg.dashboard_url.trim_end_matches('/');
    match query.filter(|q| !q.is_empty()) {
        Some(q) => Redirect::to(&format!("{base}/app/oauth/authorize?{q}")),
        None => Redirect::to(&format!("{base}/app/oauth/authorize")),
    }
}

/// `GET /api/oauth/consent?…` — what the consent screen shows.
async fn consent_preview(
    State(st): State<AppState>,
    auth: AuthUser,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let req: AuthorizeRequest = serde_urlencoded::from_str(query.as_deref().unwrap_or(""))
        .map_err(|e| {
            Error::BadRequest(format!("the authorization request could not be read: {e}"))
        })?;
    let checked = match check(&st, &req).await? {
        Ok(checked) => checked,
        Err(Refusal::ToPerson(message)) => return Err(Error::validation("request", message).into()),
        Err(Refusal::ToClient { description, .. }) => {
            return Err(Error::validation("request", description).into())
        }
    };

    let memberships = orgs::list_for_user(&st.pg, auth.user.id).await?;
    let mut organisations = Vec::with_capacity(memberships.len());
    for org in memberships {
        let role = orgs::get_member(&st.pg, org.id, auth.user.id)
            .await?
            .map(|m| m.role)
            .unwrap_or_default();
        organisations.push(json!({ "id": org.id, "name": org.name, "role": role }));
    }

    Ok(ok(json!({
        "client": {
            "client_id": checked.client.client_id,
            "name": checked.client.client_name,
            "redirect_uri": checked.redirect_uri,
            "registered_at": checked.client.created_at,
        },
        "scopes_requested": checked.scopes,
        "scopes_available": SCOPES.iter().map(|(scope, label, description, required)| json!({
            "scope": scope, "label": label, "description": description, "required": required,
        })).collect::<Vec<_>>(),
        "organisations": organisations,
        "resource": checked.resource,
    })))
}

/// The consent screen's decision.
#[derive(Debug, Deserialize)]
struct Decision {
    /// The authorization request's query string, as received.
    query: String,
    approve: bool,
    #[serde(default)]
    org_id: Option<Uuid>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

/// What a code stands for while it waits to be exchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodeGrant {
    client: Uuid,
    user: Uuid,
    org: Uuid,
    scopes: Vec<String>,
    redirect_uri: String,
    challenge: String,
    resource: String,
}

fn code_key(code: &str) -> String {
    format!("mf:oauth:code:{}", crypto::token_hash(code))
}

/// `POST /api/oauth/consent` — approve or deny; answers where to send the person.
async fn consent_decide(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(decision): Json<Decision>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let req: AuthorizeRequest = serde_urlencoded::from_str(decision.query.trim_start_matches('?'))
        .map_err(|e| {
            Error::BadRequest(format!("the authorization request could not be read: {e}"))
        })?;
    let checked = match check(&st, &req).await? {
        Ok(checked) => checked,
        Err(Refusal::ToPerson(message)) => return Err(Error::validation("request", message).into()),
        Err(Refusal::ToClient {
            redirect,
            code,
            description,
            state,
        }) => {
            let mut params = vec![("error", code), ("error_description", description.as_str())];
            if let Some(state) = state.as_deref() {
                params.push(("state", state));
            }
            return Ok(ok(
                json!({ "redirect_to": with_params(&redirect, &params) }),
            ));
        }
    };

    let iss = issuer(&st);
    let mut params: Vec<(&str, &str)> = Vec::new();
    if let Some(state) = checked.state.as_deref() {
        params.push(("state", state));
    }
    params.push(("iss", &iss));

    if !decision.approve {
        let mut denied = vec![
            ("error", "access_denied"),
            ("error_description", "the person declined"),
        ];
        denied.extend(params);
        return Ok(ok(
            json!({ "redirect_to": with_params(&checked.redirect_uri, &denied) }),
        ));
    }

    let org_id = decision
        .org_id
        .ok_or_else(|| Error::validation("org_id", "choose the organisation to connect"))?;
    if orgs::get_member(&st.pg, org_id, auth.user.id)
        .await?
        .is_none()
    {
        return Err(Error::not_found("organisation").into());
    }
    let chosen = match decision.scopes {
        Some(list) => parse_scope(Some(&list.join(" ")))
            .ok_or_else(|| Error::validation("scopes", "unknown scope"))?,
        None => checked.scopes.clone(),
    };

    let code = crypto::random_token(32);
    let grant = CodeGrant {
        client: checked.client.id,
        user: auth.user.id,
        org: org_id,
        scopes: chosen,
        redirect_uri: checked.redirect_uri.clone(),
        challenge: checked.challenge.clone(),
        resource: checked.resource.clone(),
    };
    st.kv
        .set_json(&code_key(&code), &grant, Some(CODE_TTL_SECS))
        .await?;

    let mut approved = vec![("code", code.as_str())];
    approved.extend(params);
    Ok(ok(
        json!({ "redirect_to": with_params(&checked.redirect_uri, &approved) }),
    ))
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// The token and revocation endpoints' form, every field optional so a missing
/// one is reported in OAuth's words.
#[derive(Debug, Default, Deserialize)]
struct TokenForm {
    #[serde(default)]
    grant_type: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

/// Identify the calling client: Basic credentials, or `client_id` in the form.
async fn client_of(
    st: &AppState,
    headers: &HeaderMap,
    form: &TokenForm,
) -> std::result::Result<Client, OAuthError> {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Basic ")
                .or_else(|| v.strip_prefix("basic "))
        })
        .and_then(|b64| STANDARD.decode(b64.trim()).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|pair| {
            pair.split_once(':')
                .map(|(id, secret)| (url_decode(id), url_decode(secret)))
        });
    let (client_id, secret, via_basic) = match basic {
        Some((id, secret)) => (id, Some(secret), true),
        None => (
            form.client_id.clone().unwrap_or_default(),
            form.client_secret.clone(),
            false,
        ),
    };
    let unknown = || OAuthError::new("invalid_client", "client authentication failed");
    let client = oauth::find_client(&st.pg, &client_id)
        .await
        .map_err(|e| OAuthError::server(&e))?
        .ok_or_else(unknown)?;

    match client.token_endpoint_auth_method.as_str() {
        "none" => Ok(client),
        method => {
            let presented = secret.ok_or_else(unknown)?;
            let expected = client.client_secret_hash.as_deref().unwrap_or("");
            let right_channel = (method == "client_secret_basic") == via_basic;
            if right_channel && crypto::digests_match(expected, &crypto::token_hash(&presented)) {
                Ok(client)
            } else {
                Err(unknown())
            }
        }
    }
}

/// Decode `application/x-www-form-urlencoded` in Basic credentials (RFC 6749 §2.3.1).
fn url_decode(raw: &str) -> String {
    url::form_urlencoded::parse(format!("x={raw}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// `POST /api/oauth/token`.
async fn token(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, OAuthError> {
    let form: TokenForm = serde_urlencoded::from_bytes(&body)
        .map_err(|_| OAuthError::new("invalid_request", "the body must be a form"))?;
    let client = client_of(&st, &headers, &form).await?;

    let grant = match form.grant_type.as_deref() {
        Some("authorization_code") => exchange_code(&st, &client, &form).await?,
        Some("refresh_token") => refresh(&st, &client, &form).await?,
        Some(_) => {
            return Err(OAuthError::new(
                "unsupported_grant_type",
                "use authorization_code or refresh_token",
            ))
        }
        None => return Err(OAuthError::new("invalid_request", "grant_type is required")),
    };

    let mut response = (StatusCode::OK, Json(grant)).into_response();
    no_store(response.headers_mut());
    Ok(response)
}

/// Mint a fresh access and refresh token pair, returning the plaintexts.
fn mint() -> (String, String) {
    (
        format!("{ACCESS_PREFIX}{}", crypto::random_token(32)),
        format!("{REFRESH_PREFIX}{}", crypto::random_token(32)),
    )
}

/// The token response body.
fn token_body(access: &str, refresh: &str, scopes: &[String]) -> Value {
    json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL_SECS,
        "refresh_token": refresh,
        "scope": scopes.join(" "),
    })
}

/// `grant_type=authorization_code`.
async fn exchange_code(
    st: &AppState,
    client: &Client,
    form: &TokenForm,
) -> std::result::Result<Value, OAuthError> {
    let invalid = |why: &str| OAuthError::new("invalid_grant", why.to_string());
    let code = form
        .code
        .as_deref()
        .filter(|c| !c.is_empty())
        .ok_or_else(|| OAuthError::new("invalid_request", "code is required"))?;
    let verifier = form
        .code_verifier
        .as_deref()
        .ok_or_else(|| OAuthError::new("invalid_request", "code_verifier is required"))?;

    // Consume first, then check: a code that fails a check is spent anyway,
    // so a guessed verifier cannot be retried against the same code.
    let key = code_key(code);
    let snapshot = st
        .kv
        .get_snapshot::<CodeGrant>(&key)
        .await
        .map_err(|e| OAuthError::server(&e))?
        .ok_or_else(|| invalid("the code is unknown, expired or already used"))?;
    let consumed = st
        .kv
        .swap_json::<CodeGrant>(&key, &snapshot, None)
        .await
        .map_err(|e| OAuthError::server(&e))?;
    if !consumed {
        return Err(invalid("the code is unknown, expired or already used"));
    }
    let grant = snapshot.value;

    if grant.client != client.id {
        return Err(invalid("the code was issued to another client"));
    }
    match form.redirect_uri.as_deref() {
        Some(presented) if presented == grant.redirect_uri => {}
        _ => {
            return Err(invalid(
                "redirect_uri does not match the authorization request",
            ))
        }
    }
    if !pkce_matches(verifier, &grant.challenge) {
        return Err(invalid(
            "the code_verifier does not match the code_challenge",
        ));
    }
    if let Some(resource) = form.resource.as_deref() {
        if resource.trim_end_matches('/') != grant.resource {
            return Err(OAuthError::new(
                "invalid_target",
                "the resource does not match the authorization",
            ));
        }
    }
    // The person may have left the organisation in the ten minutes since.
    if orgs::get_member(&st.pg, grant.org, grant.user)
        .await
        .map_err(|e| OAuthError::server(&e))?
        .is_none()
    {
        return Err(invalid(
            "the approving account no longer belongs to that organisation",
        ));
    }

    let (access, refresh) = mint();
    let now = Utc::now();
    let access_hash = crypto::token_hash(&access);
    let refresh_hash = crypto::token_hash(&refresh);
    let prefix: String = access.chars().take(LOOKUP_LEN).collect();
    oauth::create_grant(
        &st.pg,
        new_id(),
        client.id,
        grant.user,
        grant.org,
        &json!(grant.scopes),
        &grant.resource,
        &TokenPair {
            access_prefix: &prefix,
            access_hash: &access_hash,
            access_expires_at: now + Duration::seconds(ACCESS_TTL_SECS),
            refresh_hash: &refresh_hash,
            refresh_expires_at: now + Duration::days(REFRESH_TTL_DAYS),
        },
    )
    .await
    .map_err(|e| OAuthError::server(&e))?;

    Ok(token_body(&access, &refresh, &grant.scopes))
}

/// `grant_type=refresh_token`, with rotation and reuse detection.
async fn refresh(
    st: &AppState,
    client: &Client,
    form: &TokenForm,
) -> std::result::Result<Value, OAuthError> {
    let invalid = |why: &str| OAuthError::new("invalid_grant", why.to_string());
    let presented = form
        .refresh_token
        .as_deref()
        .ok_or_else(|| OAuthError::new("invalid_request", "refresh_token is required"))?;
    let hash = crypto::token_hash(presented);

    let Some(grant) = oauth::find_by_refresh(&st.pg, &hash)
        .await
        .map_err(|e| OAuthError::server(&e))?
    else {
        // A retired refresh token: somebody kept a copy. Revoke the grant so
        // the copy and the original both stop working.
        if let Some(stolen) = oauth::find_by_previous_refresh(&st.pg, &hash)
            .await
            .map_err(|e| OAuthError::server(&e))?
        {
            tracing::warn!(grant = %stolen.id, "a retired refresh token was presented; the grant is revoked");
            oauth::revoke(&st.pg, stolen.id)
                .await
                .map_err(|e| OAuthError::server(&e))?;
        }
        return Err(invalid("the refresh token is unknown, used or revoked"));
    };
    if grant.client_id != client.id {
        return Err(invalid("the refresh token was issued to another client"));
    }
    if grant.refresh_expires_at <= Utc::now() {
        return Err(invalid("the refresh token has expired; authorize again"));
    }
    if let Some(resource) = form.resource.as_deref() {
        if resource.trim_end_matches('/') != grant.resource {
            return Err(OAuthError::new(
                "invalid_target",
                "the resource does not match the grant",
            ));
        }
    }
    let granted = crate::extract::string_list(&grant.scopes);
    let scopes = match form.scope.as_deref() {
        // A refresh may narrow the scopes, never widen them.
        Some(asked) => {
            let asked = parse_scope(Some(asked))
                .ok_or_else(|| OAuthError::new("invalid_scope", "unknown scope"))?;
            if asked.iter().any(|s| !granted.contains(s)) {
                return Err(OAuthError::new(
                    "invalid_scope",
                    "a refresh cannot widen the scopes",
                ));
            }
            asked
        }
        None => granted,
    };

    let (access, refresh) = mint();
    let now = Utc::now();
    let access_hash = crypto::token_hash(&access);
    let refresh_hash = crypto::token_hash(&refresh);
    let prefix: String = access.chars().take(LOOKUP_LEN).collect();
    let rotated = oauth::rotate(
        &st.pg,
        grant.id,
        &hash,
        &TokenPair {
            access_prefix: &prefix,
            access_hash: &access_hash,
            access_expires_at: now + Duration::seconds(ACCESS_TTL_SECS),
            refresh_hash: &refresh_hash,
            refresh_expires_at: now + Duration::days(REFRESH_TTL_DAYS),
        },
    )
    .await
    .map_err(|e| OAuthError::server(&e))?;
    if rotated.is_none() {
        return Err(invalid("the refresh token was used by another request"));
    }
    Ok(token_body(&access, &refresh, &scopes))
}

/// `POST /api/oauth/revoke` (RFC 7009).
///
/// Answers `200` whether or not the token existed, as the RFC requires, so the
/// endpoint cannot be used to test tokens.
async fn revoke(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, OAuthError> {
    let form: TokenForm = serde_urlencoded::from_bytes(&body)
        .map_err(|_| OAuthError::new("invalid_request", "the body must be a form"))?;
    let client = client_of(&st, &headers, &form).await?;
    let token = form
        .token
        .as_deref()
        .ok_or_else(|| OAuthError::new("invalid_request", "token is required"))?;

    let found = find_grant_by_token(&st, token)
        .await
        .map_err(|e| OAuthError::server(&e))?;
    if let Some(grant) = found.filter(|g| g.client_id == client.id) {
        oauth::revoke(&st.pg, grant.id)
            .await
            .map_err(|e| OAuthError::server(&e))?;
    }
    let mut response = StatusCode::OK.into_response();
    no_store(response.headers_mut());
    Ok(response)
}

/// The grant an access or refresh token belongs to.
async fn find_grant_by_token(st: &AppState, token: &str) -> Result<Option<Grant>> {
    let hash = crypto::token_hash(token);
    if token.starts_with(REFRESH_PREFIX) {
        return oauth::find_by_refresh(&st.pg, &hash).await;
    }
    let prefix: String = token.chars().take(LOOKUP_LEN).collect();
    Ok(oauth::find_by_access_prefix(&st.pg, &prefix)
        .await?
        .into_iter()
        .find(|g| crypto::digests_match(&g.access_hash, &hash)))
}

/// Verify an access token presented to the MCP endpoint.
///
/// Returns the grant and the person it acts for. Every refusal is the same
/// `401`, so a token's state (expired, revoked, unknown) is not disclosed.
pub async fn authenticate_access(st: &AppState, token: &str) -> Result<(Grant, users::User)> {
    let refused = || Error::Unauthorized("that access token is not valid".to_string());
    let grant = find_grant_by_token(st, token).await?.ok_or_else(refused)?;
    if grant.revoked_at.is_some()
        || grant.access_expires_at <= Utc::now()
        || grant.resource != mcp_resource(st)
    {
        return Err(refused());
    }
    let user = users::get(&st.pg, grant.user_id)
        .await
        .map_err(|_| refused())?;
    if let Err(e) = oauth::touch(&st.pg, grant.id).await {
        tracing::warn!(grant = %grant.id, error = %e, "could not record oauth token use");
    }
    Ok((grant, user))
}

// ---------------------------------------------------------------------------
// Connected apps
// ---------------------------------------------------------------------------

/// `GET /api/oauth/grants` — the signed-in person's connected MCP clients.
async fn list_grants(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let rows = oauth::list_for_user(&st.pg, auth.user.id).await?;
    Ok(ok(rows
        .iter()
        .map(|g| {
            json!({
                "id": g.id,
                "client_id": g.client_id,
                "client_name": g.client_name,
                "org": { "id": g.org_id, "name": g.org_name },
                "scopes": g.scopes,
                "created_at": g.created_at,
                "last_used_at": g.last_used_at,
                "expires_at": g.refresh_expires_at,
            })
        })
        .collect::<Vec<_>>()))
}

/// `DELETE /api/oauth/grants/{id}` — disconnect a client.
async fn revoke_grant(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let grant = oauth::get_grant(&st.pg, id).await?;
    if grant.user_id != auth.user.id {
        return Err(Error::not_found("connected app").into());
    }
    oauth::revoke(&st.pg, grant.id).await?;
    Ok(ok(json!({ "deleted": grant.id })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_7636_vector_verifies() {
        // RFC 7636, Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(pkce_challenge(verifier), challenge);
        assert!(pkce_matches(verifier, challenge));
        assert!(!pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXX",
            challenge
        ));
        // Too short to be a verifier at all, whatever it hashes to.
        assert!(!pkce_matches("short", &pkce_challenge("short")));
    }

    #[test]
    fn redirect_addresses_follow_rfc_8252() {
        assert!(redirect_uri_allowed("https://client.example.com/callback"));
        assert!(redirect_uri_allowed("http://127.0.0.1:33418/callback"));
        assert!(redirect_uri_allowed("http://localhost/callback"));
        assert!(redirect_uri_allowed("http://[::1]:9000/cb"));
        assert!(redirect_uri_allowed(
            "cursor://anysphere.cursor-mcp/oauth/callback"
        ));
        assert!(redirect_uri_allowed("com.example.app:/oauth2redirect"));

        assert!(!redirect_uri_allowed("http://client.example.com/callback"));
        assert!(!redirect_uri_allowed("https://client.example.com/cb#frag"));
        assert!(!redirect_uri_allowed("javascript:alert(1)"));
        assert!(!redirect_uri_allowed("data:text/html,hi"));
        assert!(!redirect_uri_allowed("file:///etc/passwd"));
        assert!(!redirect_uri_allowed("not a url"));
    }

    #[test]
    fn a_loopback_redirect_may_change_its_port_and_nothing_else() {
        let registered = "http://127.0.0.1:5000/callback";
        assert!(redirect_matches(
            registered,
            "http://127.0.0.1:61234/callback"
        ));
        assert!(!redirect_matches(
            registered,
            "http://127.0.0.1:61234/other"
        ));
        assert!(!redirect_matches(
            registered,
            "http://localhost:5000/callback"
        ));
        let https = "https://client.example.com/cb";
        assert!(redirect_matches(https, https));
        assert!(!redirect_matches(
            https,
            "https://client.example.com:8443/cb"
        ));
        assert!(!redirect_matches(
            https,
            "https://client.example.com/cb?x=1"
        ));
    }

    #[test]
    fn scopes_default_to_read_and_refuse_the_unknown() {
        assert_eq!(parse_scope(None).unwrap(), vec!["stats:read"]);
        assert_eq!(
            parse_scope(Some("manage:write")).unwrap(),
            vec!["stats:read", "manage:write"]
        );
        assert_eq!(
            parse_scope(Some("stats:read stats:read")).unwrap(),
            vec!["stats:read"]
        );
        assert!(parse_scope(Some("admin")).is_none());
    }

    #[test]
    fn redirect_parameters_are_appended_and_encoded() {
        assert_eq!(
            with_params("https://c.example/cb", &[("code", "a b"), ("state", "x&y")]),
            "https://c.example/cb?code=a+b&state=x%26y"
        );
        assert_eq!(
            with_params("https://c.example/cb?keep=1", &[("code", "z")]),
            "https://c.example/cb?keep=1&code=z"
        );
    }

    #[test]
    fn basic_credentials_are_form_decoded() {
        assert_eq!(url_decode("mfc_a%2Bb"), "mfc_a+b");
        assert_eq!(url_decode("plain"), "plain");
    }

    #[test]
    fn oauth_errors_use_oauth_status_codes() {
        assert_eq!(
            OAuthError::new("invalid_client", "x").status,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            OAuthError::new("invalid_grant", "x").status,
            StatusCode::BAD_REQUEST
        );
    }
}

/// A real Postgres and a real Valkey, for the flows whose guarantees live in
/// the stores: single-use codes, refresh rotation, revocation.
///
/// ```text
/// MICAFORGE_TEST_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/postgres \
/// MICAFORGE_TEST_REDIS_URL=redis://127.0.0.1:6379 \
/// cargo test -p micaforge-server -- --ignored
/// ```
///
/// Each harness creates a database of its own (the role needs `CREATEDB`),
/// migrates it, and drops it at the end.
#[cfg(test)]
pub(crate) mod harness {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use micaforge_agents::verify::RangeStore;
    use micaforge_core::config::MailTransport;
    use micaforge_core::{crypto, new_id, Config};
    use micaforge_enrich::Geo;
    use micaforge_mail::Mailer;
    use micaforge_store::models::{auth as sessions, orgs, users};
    use micaforge_store::{Ch, EventWriter, Kv, WriterCfg};
    use serde_json::Value;
    use tokio::sync::RwLock;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::state::AppState;

    /// One test's stores, and the database to drop afterwards.
    pub(crate) struct Harness {
        pub(crate) st: AppState,
        admin_url: String,
        database: String,
    }

    /// A signed-in person with an organisation of their own.
    pub(crate) struct Person {
        pub(crate) org: Uuid,
        pub(crate) cookie: String,
    }

    fn config() -> Config {
        Config {
            bind_addr: "127.0.0.1:0".into(),
            database_url: String::new(),
            clickhouse_url: "http://127.0.0.1:9".into(),
            clickhouse_db: "micaforge".into(),
            clickhouse_user: "default".into(),
            clickhouse_password: String::new(),
            redis_url: String::new(),
            public_url: "https://mf.example.com".into(),
            dashboard_url: "https://app.example.com".into(),
            secret_key_base: crypto::random_token(48),
            geoip_city_path: None,
            geoip_asn_path: None,
            retention_days: 0,
            replay_retention_days: 30,
            disable_registration: false,
            max_sites_per_org: 0,
            max_orgs_per_user: 0,
            self_hosted: true,
            smtp_url: None,
            cloudflare_email_token: None,
            cloudflare_account_id: None,
            mail_from: "Micaforge <noreply@localhost>".into(),
            log_format: "text".into(),
            worker_concurrency: 2,
            llm: None,
            chat: Default::default(),
            feedback_channel: Default::default(),
            region: "default".into(),
            links_host: None,
            google_oauth: None,
            github_oauth: None,
            billing_enabled: false,
            billing_plans: Vec::new(),
            stripe: None,
            search_console: None,
            github_app: None,
        }
    }

    impl Harness {
        pub(crate) async fn new() -> Harness {
            let admin_url = std::env::var("MICAFORGE_TEST_DATABASE_URL")
                .expect("set MICAFORGE_TEST_DATABASE_URL to run this test");
            let redis_url = std::env::var("MICAFORGE_TEST_REDIS_URL")
                .expect("set MICAFORGE_TEST_REDIS_URL to run this test");
            let database = format!("mf_test_{}", crypto::numeric_code(9));
            let admin = micaforge_store::pg::connect(&admin_url).await.unwrap();
            sqlx::query(&format!("CREATE DATABASE {database}"))
                .execute(&admin)
                .await
                .unwrap();
            admin.close().await;
            let (server, _) = admin_url.rsplit_once('/').unwrap();
            let pg = micaforge_store::pg::connect(&format!("{server}/{database}"))
                .await
                .unwrap();
            micaforge_store::pg::migrate(&pg).await.unwrap();
            let kv = Kv::connect(&redis_url).await.unwrap();
            let cfg = config();
            let ch = Ch::connect("http://127.0.0.1:9", "micaforge", "default", "").unwrap();
            let mailer =
                Mailer::new(MailTransport::Log, &cfg.mail_from, &cfg.dashboard_url).unwrap();
            let st = AppState {
                cfg: Arc::new(cfg),
                pg,
                writer: EventWriter::spawn(ch.clone(), WriterCfg::default()),
                ch,
                kv,
                geo: Arc::new(Geo::open(None, None)),
                ranges: Arc::new(RwLock::new(RangeStore::new())),
                mailer: Arc::new(mailer),
                http: reqwest::Client::new(),
                link_clicks: micaforge_store::TableWriter::spawn(
                    Ch::connect("http://127.0.0.1:9", "micaforge", "default", "").unwrap(),
                    "link_clicks",
                    WriterCfg::default(),
                ),
                mcp_calls: micaforge_store::TableWriter::spawn(
                    Ch::connect("http://127.0.0.1:9", "micaforge", "default", "").unwrap(),
                    "mcp_calls",
                    WriterCfg::default(),
                ),
            };
            Harness {
                st,
                admin_url,
                database,
            }
        }

        pub(crate) async fn finish(self) {
            self.st.pg.close().await;
            let admin = micaforge_store::pg::connect(&self.admin_url).await.unwrap();
            let _ = sqlx::query(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.database
            ))
            .execute(&admin)
            .await;
        }

        /// A new account, signed in, owning an organisation.
        pub(crate) async fn person(&self, email: &str) -> Person {
            let user = users::create(&self.st.pg, new_id(), email, "Test person")
                .await
                .unwrap();
            users::mark_email_verified(&self.st.pg, user.id)
                .await
                .unwrap();
            let org = orgs::create(
                &self.st.pg,
                new_id(),
                "Test org",
                &format!("org-{}", crypto::numeric_code(8)),
                user.id,
            )
            .await
            .unwrap();
            orgs::add_member(&self.st.pg, org.id, user.id, "owner")
                .await
                .unwrap();
            let token = crypto::random_token(32);
            sessions::create(
                &self.st.pg,
                new_id(),
                user.id,
                &crypto::token_hash(&token),
                None,
                None,
                chrono::Utc::now() + chrono::Duration::hours(1),
            )
            .await
            .unwrap();
            Person {
                org: org.id,
                cookie: format!("mf_session={token}"),
            }
        }

        /// Send one request through the whole router.
        pub(crate) async fn send(
            &self,
            request: Request<Body>,
        ) -> (u16, axum::http::HeaderMap, Value) {
            let app = crate::routes::router(&self.st.cfg).with_state(self.st.clone());
            let response = app.oneshot(request).await.unwrap();
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, headers, body)
        }
    }
}

#[cfg(test)]
mod live {
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::{json, Value};

    use super::harness::{Harness, Person};
    use super::*;

    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

    fn form(pairs: &[(&str, &str)]) -> String {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in pairs {
            s.append_pair(k, v);
        }
        s.finish()
    }

    fn post_form(path: &str, body: String) -> Request<Body> {
        Request::post(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    }

    /// Register a client and walk a person through consent; returns the
    /// client id, the code, and the redirect address used.
    async fn authorize(
        h: &Harness,
        who: &Person,
        client_id: Option<&str>,
    ) -> (String, String, String) {
        let client_id = match client_id {
            Some(id) => id.to_string(),
            None => {
                let (status, _, body) = h
                    .send(
                        Request::post("/api/oauth/register")
                            .header("content-type", "application/json")
                            .body(Body::from(
                                json!({
                                    "client_name": "Test client",
                                    "redirect_uris": ["http://127.0.0.1:33418/callback"],
                                    "token_endpoint_auth_method": "none"
                                })
                                .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await;
                assert_eq!(status, 201, "{body}");
                body["client_id"].as_str().unwrap().to_string()
            }
        };
        // A loopback redirect may use another port at request time.
        let redirect = "http://127.0.0.1:50123/callback".to_string();
        let query = form(&[
            ("response_type", "code"),
            ("client_id", &client_id),
            ("redirect_uri", &redirect),
            ("code_challenge", &pkce_challenge(VERIFIER)),
            ("code_challenge_method", "S256"),
            ("state", "xyz"),
            ("scope", "stats:read manage:write"),
            ("resource", "https://mf.example.com/api/mcp"),
        ]);

        let (status, headers, _) = h
            .send(
                Request::get(format!("/api/oauth/authorize?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert!((300..400).contains(&status));
        let location = headers.get("location").unwrap().to_str().unwrap();
        assert!(location.starts_with("https://app.example.com/app/oauth/authorize?"));

        let (status, _, preview) = h
            .send(
                Request::get(format!("/api/oauth/consent?{query}"))
                    .header("cookie", &who.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 200, "{preview}");
        assert_eq!(preview["data"]["client"]["name"], "Test client");
        assert_eq!(preview["data"]["organisations"][0]["id"], json!(who.org));

        let (status, _, decided) = h
            .send(
                Request::post("/api/oauth/consent")
                    .header("cookie", &who.cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "query": query, "approve": true, "org_id": who.org }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 200, "{decided}");
        let to = url::Url::parse(decided["data"]["redirect_to"].as_str().unwrap()).unwrap();
        let params: std::collections::HashMap<String, String> =
            to.query_pairs().into_owned().collect();
        assert_eq!(params["state"], "xyz");
        assert_eq!(params["iss"], "https://mf.example.com");
        (client_id, params["code"].clone(), redirect)
    }

    async fn exchange(
        h: &Harness,
        client: &str,
        code: &str,
        redirect: &str,
        verifier: &str,
    ) -> (u16, Value) {
        let (status, headers, body) = h
            .send(post_form(
                "/api/oauth/token",
                form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", client),
                    ("code", code),
                    ("redirect_uri", redirect),
                    ("code_verifier", verifier),
                ]),
            ))
            .await;
        assert_eq!(headers.get("cache-control").unwrap(), "no-store");
        (status, body)
    }

    #[tokio::test]
    #[ignore = "needs Postgres and Valkey"]
    async fn the_whole_oauth_flow_with_pkce_rotation_and_revocation() {
        let h = Harness::new().await;
        let ada = h.person("ada@example.com").await;

        // Discovery.
        let (status, _, meta) = h
            .send(
                Request::get("/.well-known/oauth-authorization-server")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(meta["code_challenge_methods_supported"], json!(["S256"]));
        let (_, _, resource) = h
            .send(
                Request::get("/.well-known/oauth-protected-resource/api/mcp")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(resource["resource"], "https://mf.example.com/api/mcp");

        // Without a credential the MCP endpoint points at the metadata.
        let (status, headers, _) = h
            .send(Request::post("/api/mcp").body(Body::from("{}")).unwrap())
            .await;
        assert_eq!(status, 401);
        assert!(headers
            .get("www-authenticate")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("/.well-known/oauth-protected-resource/api/mcp"));

        // A wrong verifier spends the code; the right one cannot use it after.
        let (client, code, redirect) = authorize(&h, &ada, None).await;
        let (status, body) = exchange(&h, &client, &code, &redirect, &"x".repeat(43)).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (400, Some("invalid_grant"))
        );
        let (status, _) = exchange(&h, &client, &code, &redirect, VERIFIER).await;
        assert_eq!(status, 400, "a code works once");

        // A fresh code and the right verifier.
        let (_, code, redirect) = authorize(&h, &ada, Some(&client)).await;
        let (status, tokens) = exchange(&h, &client, &code, &redirect, VERIFIER).await;
        assert_eq!(status, 200, "{tokens}");
        let access = tokens["access_token"].as_str().unwrap().to_string();
        let refresh = tokens["refresh_token"].as_str().unwrap().to_string();
        assert!(access.starts_with(ACCESS_PREFIX));
        assert!(refresh.starts_with(REFRESH_PREFIX));
        assert_eq!(tokens["expires_in"], ACCESS_TTL_SECS);
        assert_eq!(tokens["scope"], "stats:read manage:write");

        // The access token works on the MCP endpoint, and opens a session.
        let (status, headers, init) = h
            .send(
                Request::post("/api/mcp")
                    .header("authorization", format!("Bearer {access}"))
                    .body(Body::from(
                        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                                "params": { "protocolVersion": "2025-06-18", "clientInfo": { "name": "Test client" } } })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 200, "{init}");
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(headers.get("mcp-session-id").is_some());

        // ... and nowhere else.
        let (status, _, _) = h
            .send(
                Request::get("/api/sites")
                    .header("authorization", format!("Bearer {access}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 403);

        // Rotation: the refresh token buys a new pair once.
        let refreshed = |token: String| {
            post_form(
                "/api/oauth/token",
                form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", &client),
                    ("refresh_token", &token),
                ]),
            )
        };
        let (status, _, rotated) = h.send(refreshed(refresh.clone())).await;
        assert_eq!(status, 200, "{rotated}");
        let new_access = rotated["access_token"].as_str().unwrap().to_string();
        assert!(authenticate_access(&h.st, &new_access).await.is_ok());

        // Replaying the retired refresh token revokes the whole grant.
        let (status, _, replay) = h.send(refreshed(refresh)).await;
        assert_eq!(
            (status, replay["error"].as_str()),
            (400, Some("invalid_grant"))
        );
        assert!(authenticate_access(&h.st, &new_access).await.is_err());

        // Revocation by the client, and by the person in "Connected apps".
        let (_, code, redirect) = authorize(&h, &ada, Some(&client)).await;
        let (_, tokens) = exchange(&h, &client, &code, &redirect, VERIFIER).await;
        let access = tokens["access_token"].as_str().unwrap().to_string();
        let (status, _, _) = h
            .send(post_form(
                "/api/oauth/revoke",
                form(&[("client_id", &client), ("token", &access)]),
            ))
            .await;
        assert_eq!(status, 200);
        assert!(authenticate_access(&h.st, &access).await.is_err());

        let (_, code, redirect) = authorize(&h, &ada, Some(&client)).await;
        let (_, tokens) = exchange(&h, &client, &code, &redirect, VERIFIER).await;
        let access = tokens["access_token"].as_str().unwrap().to_string();
        let (_, _, listed) = h
            .send(
                Request::get("/api/oauth/grants")
                    .header("cookie", &ada.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let grant = listed["data"][0]["id"].as_str().unwrap().to_string();
        let (status, _, _) = h
            .send(
                Request::delete(format!("/api/oauth/grants/{grant}"))
                    .header("cookie", &ada.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 200);
        assert!(authenticate_access(&h.st, &access).await.is_err());

        // Another person cannot revoke Ada's connection.
        let bob = h.person("bob@example.com").await;
        let (status, _, _) = h
            .send(
                Request::delete(format!("/api/oauth/grants/{grant}"))
                    .header("cookie", &bob.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 404);

        h.finish().await;
    }

    #[tokio::test]
    #[ignore = "needs Postgres and Valkey"]
    async fn consent_refuses_what_it_must_not_redirect_and_reports_the_rest() {
        let h = Harness::new().await;
        let ada = h.person("ada@example.com").await;
        let (client, _, _) = authorize(&h, &ada, None).await;

        // An unregistered redirect address is shown to the person, never followed.
        let evil = form(&[
            ("response_type", "code"),
            ("client_id", &client),
            ("redirect_uri", "https://evil.example.com/cb"),
            ("code_challenge", &pkce_challenge(VERIFIER)),
            ("code_challenge_method", "S256"),
        ]);
        let (status, _, body) = h
            .send(
                Request::post("/api/oauth/consent")
                    .header("cookie", &ada.cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "query": evil, "approve": true, "org_id": ada.org }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 422, "{body}");

        // A missing PKCE challenge goes back to the client as an error.
        let no_pkce = form(&[
            ("response_type", "code"),
            ("client_id", &client),
            ("redirect_uri", "http://127.0.0.1:33418/callback"),
            ("state", "s"),
        ]);
        let (status, _, body) = h
            .send(
                Request::post("/api/oauth/consent")
                    .header("cookie", &ada.cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "query": no_pkce, "approve": true, "org_id": ada.org }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 200);
        let to = body["data"]["redirect_to"].as_str().unwrap();
        assert!(to.contains("error=invalid_request") && to.contains("state=s"));

        // Consent for an organisation the person is not in is refused.
        let bob = h.person("bob@example.com").await;
        let good = form(&[
            ("response_type", "code"),
            ("client_id", &client),
            ("redirect_uri", "http://127.0.0.1:33418/callback"),
            ("code_challenge", &pkce_challenge(VERIFIER)),
            ("code_challenge_method", "S256"),
        ]);
        let (status, _, _) = h
            .send(
                Request::post("/api/oauth/consent")
                    .header("cookie", &ada.cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "query": good, "approve": true, "org_id": bob.org }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 404);

        // Denial goes back to the client as access_denied.
        let (_, _, denied) = h
            .send(
                Request::post("/api/oauth/consent")
                    .header("cookie", &ada.cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "query": good, "approve": false }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert!(denied["data"]["redirect_to"]
            .as_str()
            .unwrap()
            .contains("error=access_denied"));

        h.finish().await;
    }
}
