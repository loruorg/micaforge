//! The GitHub REST calls the App makes, and the credentials behind them.
//!
//! Three credentials, each used for exactly what GitHub documents it for:
//!
//! - **The app JWT** (RS256, signed with `MICAFORGE_GITHUB_APP_PRIVATE_KEY`,
//!   `iss` the App ID, `iat` sixty seconds in the past for clock drift, `exp`
//!   nine minutes ahead, inside GitHub's ten-minute ceiling). It reads an
//!   installation, mints installation tokens, and deletes an installation.
//!   Signed per call; it is never stored.
//! - **An installation token**, minted with the JWT, an hour long, scoped to
//!   one installation's repositories and the App's permissions. It lists
//!   repositories, reads one, and opens issues. It is cached in Valkey until
//!   five minutes before it expires, **sealed** with a key derived from
//!   `MICAFORGE_SECRET_KEY_BASE` and bound to the installation id, so a Valkey
//!   dump hands over nothing usable.
//! - **A user token**, from the OAuth code GitHub returns during an install.
//!   It answers one question, "can this person reach installation N?", and is
//!   dropped at the end of the callback.
//!
//! No token reaches a log line. Errors name the call and GitHub's status,
//! never a header; reqwest's own errors carry URLs, and no URL here holds a
//! credential.
//!
//! [`Endpoints`] says where GitHub is. Production uses
//! [`Endpoints::github_com`]; the tests point it at a server of their own,
//! which is how every call below is exercised without reaching GitHub.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use micaforge_core::config::GitHubApp;
use micaforge_core::{crypto, Error, Result};
use micaforge_store::Kv;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::AppState;

/// Sent with every request; GitHub refuses a request without a user agent.
const USER_AGENT: &str = "Micaforge GitHub App";

/// The REST API version these calls are written against.
const API_VERSION: &str = "2022-11-28";

/// The ceiling on one call to GitHub.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Pages of 100 read at most when listing. A thousand repositories, or a
/// thousand installations for one person, is past what a picker can show.
const MAX_PAGES: u32 = 10;

/// What the cached installation token is sealed under.
const TOKEN_PURPOSE: &str = "github-installation-token";

/// How long before its expiry a cached token stops being used.
const TOKEN_MARGIN_SECS: i64 = 5 * 60;

/// Where GitHub is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The REST API, without a trailing slash.
    pub api: String,
    /// The web origin, where an install starts and an OAuth code is exchanged.
    pub web: String,
}

impl Endpoints {
    /// github.com.
    pub fn github_com() -> Endpoints {
        Endpoints {
            api: "https://api.github.com".to_string(),
            web: "https://github.com".to_string(),
        }
    }
}

/// The claims of an app JWT, as GitHub documents them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppClaims {
    /// Issued at, sixty seconds in the past so a slow clock is not "in the future".
    pub iat: i64,
    /// Expiry: nine minutes ahead, inside GitHub's ten-minute ceiling.
    pub exp: i64,
    /// The App ID.
    pub iss: String,
}

/// The claims for a JWT signed at `now` (unix seconds).
pub fn app_claims(app_id: u64, now: i64) -> AppClaims {
    AppClaims {
        iat: now - 60,
        exp: now + 9 * 60,
        iss: app_id.to_string(),
    }
}

/// The App's private key, parsed.
fn signing_key(app: &GitHubApp) -> Result<EncodingKey> {
    EncodingKey::from_rsa_pem(app.private_key.as_bytes()).map_err(|_| {
        Error::validation(
            "MICAFORGE_GITHUB_APP_PRIVATE_KEY",
            "is not an RSA private key in PEM form; paste the .pem file GitHub generated",
        )
    })
}

/// Refuse a private key that cannot sign. Called at boot.
pub fn check_key(app: &GitHubApp) -> Result<()> {
    signing_key(app).map(|_| ())
}

/// An app JWT signed at `now`.
pub fn app_jwt(app: &GitHubApp, now: i64) -> Result<String> {
    let key = signing_key(app)?;
    jsonwebtoken::encode(
        &Header::new(Algorithm::RS256),
        &app_claims(app.app_id, now),
        &key,
    )
    .map_err(|_| Error::internal("the GitHub App token could not be signed"))
}

/// What GitHub says about an installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationInfo {
    /// GitHub's installation id.
    pub id: i64,
    /// The account it is installed on.
    pub account_login: String,
    /// `User` or `Organization`.
    pub account_type: String,
    /// `all` or `selected`.
    pub repository_selection: Option<String>,
    /// When the account suspended the App, if it has.
    pub suspended_at: Option<DateTime<Utc>>,
}

/// One repository an installation can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repo {
    /// GitHub's repository id.
    pub id: i64,
    /// `owner/name`.
    pub full_name: String,
    /// Whether it is private.
    pub private: bool,
    /// Its default branch.
    pub default_branch: String,
    /// Its page on GitHub.
    pub html_url: String,
}

/// An issue GitHub opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Its number in the repository.
    pub number: i64,
    /// Its page on GitHub.
    pub html_url: String,
}

/// The App's calls, bound to its configuration and where GitHub is.
///
/// The calls that act inside an installation take its token as an argument
/// rather than fetching it themselves, so the one place a token is cached
/// ([`GitHub::installation_token`]) is visible at every call site.
pub struct GitHub<'a> {
    pub(crate) http: &'a reqwest::Client,
    pub(crate) app: &'a GitHubApp,
    pub(crate) at: Endpoints,
    pub(crate) seal_key: [u8; 32],
}

impl<'a> GitHub<'a> {
    /// The App, when this server has one configured.
    pub fn of(st: &'a AppState) -> Result<GitHub<'a>> {
        let app = st.cfg.github_app.as_ref().ok_or_else(not_configured)?;
        Ok(GitHub {
            http: &st.http,
            app,
            at: Endpoints::github_com(),
            seal_key: st.cfg.derive_key(TOKEN_PURPOSE),
        })
    }

    /// Where an install of this App starts, carrying `state` back to us.
    pub fn install_url(&self, state: &str) -> String {
        format!(
            "{}/apps/{}/installations/new?state={state}",
            self.at.web, self.app.slug
        )
    }

    /// A request to the API with the headers GitHub asks for.
    fn api(&self, method: Method, path: &str, bearer: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.at.api))
            .timeout(TIMEOUT)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", API_VERSION)
            .header("user-agent", USER_AGENT)
            .bearer_auth(bearer)
    }

    /// A request authenticated as the App itself.
    fn as_app(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let jwt = app_jwt(self.app, Utc::now().timestamp())?;
        Ok(self.api(method, path, &jwt))
    }

    /// Read an installation, as the App.
    pub async fn installation(&self, installation_id: i64) -> Result<InstallationInfo> {
        let request = self.as_app(
            Method::GET,
            &format!("/app/installations/{installation_id}"),
        )?;
        let body = send(request, "read the installation").await?;
        read_installation(&body)
    }

    /// Delete an installation on GitHub, as the App: the App leaves the account.
    pub async fn delete_installation(&self, installation_id: i64) -> Result<()> {
        let request = self.as_app(
            Method::DELETE,
            &format!("/app/installations/{installation_id}"),
        )?;
        send(request, "uninstall the app").await.map(|_| ())
    }

    /// Mint an installation token, as the App, with its expiry.
    pub async fn mint_installation_token(
        &self,
        installation_id: i64,
    ) -> Result<(String, Option<DateTime<Utc>>)> {
        let request = self.as_app(
            Method::POST,
            &format!("/app/installations/{installation_id}/access_tokens"),
        )?;
        let body = send(request, "mint an installation token").await?;
        let token = body["token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| Error::Upstream("github: the token answer had no token".into()))?
            .to_string();
        let expires = body["expires_at"]
            .as_str()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|at| at.with_timezone(&Utc));
        Ok((token, expires))
    }

    /// An installation token, from the sealed cache in Valkey or freshly
    /// minted and cached there.
    pub async fn installation_token(&self, kv: &Kv, installation_id: i64) -> Result<String> {
        let key = token_key(installation_id);
        let aad = installation_id.to_be_bytes();
        if let Some(cached) = kv.get_json::<String>(&key).await? {
            // A token that will not open (a rotated secret base, say) is simply
            // minted again.
            if let Some(token) = STANDARD
                .decode(cached)
                .ok()
                .and_then(|sealed| crypto::open(&self.seal_key, &aad, &sealed).ok())
                .and_then(|plain| String::from_utf8(plain).ok())
            {
                return Ok(token);
            }
        }

        let (token, expires) = self.mint_installation_token(installation_id).await?;
        let ttl = cache_ttl(expires, Utc::now());
        if ttl > 0 {
            let sealed = crypto::seal(&self.seal_key, &aad, token.as_bytes())?;
            kv.set_json(&key, &STANDARD.encode(sealed), Some(ttl))
                .await?;
        }
        Ok(token)
    }

    /// Exchange the OAuth code GitHub returned during an install for a user
    /// token. The token is the caller's to drop.
    pub async fn user_token(&self, code: &str) -> Result<String> {
        let response = self
            .http
            .post(format!("{}/login/oauth/access_token", self.at.web))
            .timeout(TIMEOUT)
            .header("accept", "application/json")
            .header("user-agent", USER_AGENT)
            .json(&json!({
                "client_id": self.app.client_id,
                "client_secret": self.app.client_secret,
                "code": code,
            }))
            .send()
            .await
            .map_err(|e| Error::Upstream(format!("github: exchange the code: {e}")))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        match body["access_token"].as_str().filter(|t| !t.is_empty()) {
            Some(token) if status.is_success() => Ok(token.to_string()),
            // GitHub answers a bad or used code with 200 and an `error` field.
            _ => Err(Error::Unauthorized(format!(
                "GitHub did not accept the sign-in code ({})",
                body["error"].as_str().unwrap_or("no reason given")
            ))),
        }
    }

    /// The installations of this App a person can reach, by their user token.
    pub async fn user_installation_ids(&self, user_token: &str) -> Result<Vec<i64>> {
        let mut ids = Vec::new();
        for page in 1..=MAX_PAGES {
            let request = self.api(
                Method::GET,
                &format!("/user/installations?per_page=100&page={page}"),
                user_token,
            );
            let body = send(request, "list your installations").await?;
            let batch = body["installations"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            ids.extend(batch.iter().filter_map(|i| i["id"].as_i64()));
            if batch.len() < 100 {
                break;
            }
        }
        Ok(ids)
    }

    /// The repositories an installation token reaches, and whether the list
    /// was cut at [`MAX_PAGES`].
    pub async fn installation_repos(&self, token: &str) -> Result<(Vec<Repo>, bool)> {
        let mut repos = Vec::new();
        for page in 1..=MAX_PAGES {
            let request = self.api(
                Method::GET,
                &format!("/installation/repositories?per_page=100&page={page}"),
                token,
            );
            let body = send(request, "list the installation's repositories").await?;
            let batch = body["repositories"].as_array().cloned().unwrap_or_default();
            repos.extend(batch.iter().filter_map(read_repo));
            if batch.len() < 100 {
                return Ok((repos, false));
            }
        }
        Ok((repos, true))
    }

    /// One repository, read with an installation token: the proof that the
    /// installation can reach it.
    pub async fn repo(&self, token: &str, full_name: &str) -> Result<Repo> {
        let path = repo_path(full_name)?;
        let body = send(
            self.api(Method::GET, &format!("/repos/{path}"), token),
            "read the repository",
        )
        .await?;
        read_repo(&body)
            .ok_or_else(|| Error::Upstream("github: the repository answer was not one".into()))
    }

    /// Open an issue in a repository an installation token reaches.
    pub async fn create_issue(
        &self,
        token: &str,
        full_name: &str,
        title: &str,
        body: &str,
    ) -> Result<Issue> {
        let path = repo_path(full_name)?;
        let request = self
            .api(Method::POST, &format!("/repos/{path}/issues"), token)
            .json(&json!({ "title": title, "body": body }));
        let answer = send(request, "open the issue").await?;
        match (answer["number"].as_i64(), answer["html_url"].as_str()) {
            (Some(number), Some(url)) if url.starts_with("https://") => Ok(Issue {
                number,
                html_url: url.to_string(),
            }),
            _ => Err(Error::Upstream(
                "github: the issue answer had no number or address".into(),
            )),
        }
    }
}

/// Forget an installation's cached token, when the installation is gone.
pub async fn forget_token(kv: &Kv, installation_id: i64) -> Result<()> {
    kv.del(&token_key(installation_id)).await
}

/// The error every route returns when the App is not configured.
pub fn not_configured() -> Error {
    Error::NotFound(
        "the GitHub App is not configured on this server; its operator sets \
         MICAFORGE_GITHUB_APP_* to switch it on"
            .to_string(),
    )
}

/// Where an installation's sealed token is cached.
fn token_key(installation_id: i64) -> String {
    format!("mf:github:token:{installation_id}")
}

/// How long to cache a token that expires at `expires`, in seconds; `0` for
/// not at all. Tokens live an hour; one with no readable expiry is kept half
/// that, and none is kept past five minutes before its end.
fn cache_ttl(expires: Option<DateTime<Utc>>, now: DateTime<Utc>) -> u32 {
    let left = match expires {
        Some(at) => (at - now).num_seconds() - TOKEN_MARGIN_SECS,
        None => 30 * 60,
    };
    left.clamp(0, 55 * 60) as u32
}

/// `owner/name`, checked to be exactly that before it goes into a path.
///
/// GitHub's own rules: an owner is letters, digits and `-`; a repository name
/// adds `_` and `.`. Anything else here would be a path traversal, not a
/// repository.
pub fn repo_path(full_name: &str) -> Result<String> {
    let refused = || Error::validation("repo", "must be a repository as owner/name");
    let (owner, name) = full_name.trim().split_once('/').ok_or_else(refused)?;
    let owner_ok = (1..=39).contains(&owner.len())
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let name_ok = (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if owner_ok && name_ok {
        Ok(format!("{owner}/{name}"))
    } else {
        Err(refused())
    }
}

/// Send a request and read a JSON answer, turning GitHub's refusals into
/// sentences a person can act on. `what` names the call: "open the issue".
async fn send(request: RequestBuilder, what: &str) -> Result<Value> {
    let response = request
        .send()
        .await
        .map_err(|e| Error::Upstream(format!("github: {what}: {e}")))?;
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return Ok(Value::Null);
    }
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    Err(refusal(status, what, body["message"].as_str()))
}

/// GitHub's answer to a call it refused, as one of our errors.
///
/// The refusals a site admin can fix (a permission the App lacks, a
/// repository left out of the installation, issues turned off) become public
/// sentences; GitHub refusing the App's own credentials, or failing, is the
/// operator's to read in the log, so it stays an upstream error.
pub(crate) fn refusal(status: StatusCode, what: &str, message: Option<&str>) -> Error {
    let said = message.unwrap_or("no reason given");
    match status.as_u16() {
        403 | 404 => Error::Conflict(format!(
            "GitHub would not let the app {what} ({said}). Check that the app is installed on \
             the repository's account, that the repository is one the installation includes, and \
             that the app has the permission this needs."
        )),
        410 => Error::Conflict(format!("GitHub says this is turned off: {said}.")),
        422 => Error::BadRequest(format!("GitHub refused to {what}: {said}.")),
        _ => Error::Upstream(format!("github: {what}: status {status}: {said}")),
    }
}

/// An installation, from `GET /app/installations/{id}` or a webhook payload.
pub fn read_installation(body: &Value) -> Result<InstallationInfo> {
    let id = body["id"]
        .as_i64()
        .ok_or_else(|| Error::Upstream("github: the installation answer had no id".into()))?;
    Ok(InstallationInfo {
        id,
        account_login: body["account"]["login"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        account_type: body["account"]["type"]
            .as_str()
            .or_else(|| body["target_type"].as_str())
            .unwrap_or("User")
            .to_string(),
        repository_selection: body["repository_selection"].as_str().map(str::to_string),
        suspended_at: body["suspended_at"]
            .as_str()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|at| at.with_timezone(&Utc)),
    })
}

/// A repository object, when it has what a link needs.
pub fn read_repo(v: &Value) -> Option<Repo> {
    let html_url = v["html_url"].as_str()?;
    if !html_url.starts_with("https://") {
        return None;
    }
    Some(Repo {
        id: v["id"].as_i64()?,
        full_name: v["full_name"].as_str()?.to_string(),
        private: v["private"].as_bool().unwrap_or(false),
        default_branch: v["default_branch"].as_str().unwrap_or("main").to_string(),
        html_url: html_url.to_string(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::extract::{Path, Query};
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use jsonwebtoken::{decode, DecodingKey, Validation};
    use std::collections::HashMap;

    /// A 2048-bit RSA key made for these tests only (`openssl genrsa 2048`).
    /// It was never registered with GitHub and signs nothing outside them.
    const TEST_KEY: &str = include_str!("testdata/app-key.pem");
    /// Its public half, to check what the private half signed.
    const TEST_PUBLIC: &str = include_str!("testdata/app-key.pub.pem");

    /// An App configured with the test key and GitHub's documented webhook
    /// secret from its signature example.
    pub(crate) fn app() -> GitHubApp {
        GitHubApp {
            app_id: 123_456,
            slug: "micaforge-test".into(),
            private_key: TEST_KEY.into(),
            webhook_secret: "It's a Secret to Everybody".into(),
            client_id: "Iv23liTest".into(),
            client_secret: "client-secret".into(),
        }
    }

    #[test]
    fn the_jwt_claims_are_the_shape_github_documents() {
        let now = 1_760_000_000;
        let claims = app_claims(123_456, now);
        assert_eq!(claims.iat, now - 60, "issued sixty seconds in the past");
        assert!(claims.exp > now);
        assert!(claims.exp - now <= 600, "at most ten minutes ahead");
        assert!(
            claims.exp - claims.iat <= 600,
            "a life of at most ten minutes"
        );
        assert_eq!(claims.iss, "123456", "the issuer is the App ID");
    }

    #[test]
    fn the_jwt_is_rs256_and_verifies_with_the_public_key() {
        let now = Utc::now().timestamp();
        let token = app_jwt(&app(), now).unwrap();
        let header = jsonwebtoken::decode_header(&token).unwrap();
        assert_eq!(header.alg, Algorithm::RS256);

        let key = DecodingKey::from_rsa_pem(TEST_PUBLIC.as_bytes()).unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_required_spec_claims(&["exp", "iat", "iss"]);
        validation.set_issuer(&["123456"]);
        let read = decode::<AppClaims>(&token, &key, &validation).unwrap();
        assert_eq!(read.claims, app_claims(123_456, now));
    }

    #[test]
    fn a_key_that_is_not_rsa_pem_is_refused_by_name() {
        let mut broken = app();
        broken.private_key =
            "-----BEGIN RSA PRIVATE KEY-----\nnot a key\n-----END RSA PRIVATE KEY-----".into();
        assert_eq!(
            check_key(&broken).unwrap_err().field(),
            Some("MICAFORGE_GITHUB_APP_PRIVATE_KEY")
        );
        assert!(check_key(&app()).is_ok());
    }

    #[test]
    fn only_owner_slash_name_reaches_a_path() {
        assert_eq!(repo_path(" acme/site.web_1 ").unwrap(), "acme/site.web_1");
        for bad in [
            "acme",
            "acme/",
            "/site",
            "acme/site/issues",
            "acme/..",
            "ac me/site",
            "acme/site?x=1",
            "acme/%2e%2e",
        ] {
            assert!(repo_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_token_is_cached_until_five_minutes_before_it_ends() {
        let now = Utc::now();
        assert_eq!(
            cache_ttl(Some(now + chrono::Duration::minutes(60)), now),
            55 * 60
        );
        assert_eq!(
            cache_ttl(Some(now + chrono::Duration::minutes(10)), now),
            5 * 60
        );
        assert_eq!(cache_ttl(Some(now + chrono::Duration::minutes(3)), now), 0);
        assert_eq!(cache_ttl(None, now), 30 * 60);
    }

    #[test]
    fn refusals_a_site_admin_can_fix_are_said_plainly() {
        let missing = refusal(StatusCode::NOT_FOUND, "open the issue", Some("Not Found"));
        assert!(missing.is_public());
        assert!(missing.to_string().contains("open the issue"));
        assert!(refusal(
            StatusCode::GONE,
            "open the issue",
            Some("Issues are disabled")
        )
        .is_public());
        let broken = refusal(StatusCode::UNAUTHORIZED, "read the installation", None);
        assert!(
            !broken.is_public(),
            "bad app credentials are the operator's to read"
        );
    }

    #[test]
    fn installations_and_repositories_are_read_from_githubs_shapes() {
        let installation = read_installation(&json!({
            "id": 99, "account": { "login": "acme", "type": "Organization" },
            "repository_selection": "selected", "suspended_at": null
        }))
        .unwrap();
        assert_eq!(installation.id, 99);
        assert_eq!(installation.account_type, "Organization");
        assert_eq!(installation.suspended_at, None);

        let repo = read_repo(&json!({
            "id": 7, "full_name": "acme/site", "private": true,
            "default_branch": "trunk", "html_url": "https://github.com/acme/site"
        }))
        .unwrap();
        assert_eq!(repo.default_branch, "trunk");
        assert!(
            read_repo(&json!({ "id": 7, "full_name": "x/y", "html_url": "javascript:x" }))
                .is_none()
        );
    }

    #[test]
    fn the_install_address_carries_the_state() {
        let config = app();
        let http = reqwest::Client::new();
        let github = GitHub {
            http: &http,
            app: &config,
            at: Endpoints::github_com(),
            seal_key: [0u8; 32],
        };
        assert_eq!(
            github.install_url("abc.def"),
            "https://github.com/apps/micaforge-test/installations/new?state=abc.def"
        );
    }

    /// The bearer token a request carried.
    fn bearer(headers: &HeaderMap) -> String {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_string()
    }

    /// Whether a request carried a JWT the test key signed.
    fn is_app_jwt(headers: &HeaderMap) -> bool {
        let key = DecodingKey::from_rsa_pem(TEST_PUBLIC.as_bytes()).unwrap();
        decode::<AppClaims>(&bearer(headers), &key, &Validation::new(Algorithm::RS256)).is_ok()
    }

    fn status(code: u16, body: Value) -> Response {
        (axum::http::StatusCode::from_u16(code).unwrap(), Json(body)).into_response()
    }

    /// A server that answers the App's calls as GitHub documents them, and
    /// refuses any call that carries the wrong credential.
    pub(crate) async fn fake_github() -> Endpoints {
        let routes = Router::new()
            .route(
                "/app/installations/{id}/access_tokens",
                post(|headers: HeaderMap, Path(id): Path<i64>| async move {
                    if !is_app_jwt(&headers) || id != 42 {
                        return status(401, json!({ "message": "Bad credentials" }));
                    }
                    let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
                    status(201, json!({ "token": "ghs_installation", "expires_at": expires }))
                }),
            )
            .route(
                "/app/installations/{id}",
                get(|headers: HeaderMap, Path(id): Path<i64>| async move {
                    if !is_app_jwt(&headers) {
                        return status(401, json!({ "message": "Bad credentials" }));
                    }
                    status(
                        200,
                        json!({ "id": id, "account": { "login": "acme", "type": "Organization" },
                                "repository_selection": "all", "suspended_at": null }),
                    )
                })
                .delete(|headers: HeaderMap| async move {
                    if is_app_jwt(&headers) {
                        axum::http::StatusCode::NO_CONTENT.into_response()
                    } else {
                        status(401, json!({ "message": "Bad credentials" }))
                    }
                }),
            )
            .route(
                "/user/installations",
                get(|headers: HeaderMap| async move {
                    if bearer(&headers) != "ghu_person" {
                        return status(401, json!({ "message": "Bad credentials" }));
                    }
                    status(
                        200,
                        json!({ "total_count": 2, "installations": [{ "id": 42 }, { "id": 43 }] }),
                    )
                }),
            )
            .route(
                "/installation/repositories",
                get(
                    |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                        if bearer(&headers) != "ghs_installation"
                            || q.get("per_page").map(String::as_str) != Some("100")
                        {
                            return status(401, json!({ "message": "Bad credentials" }));
                        }
                        status(
                            200,
                            json!({ "total_count": 1, "repositories": [{
                                "id": 7, "full_name": "acme/site", "private": false,
                                "default_branch": "main", "html_url": "https://github.com/acme/site" }] }),
                        )
                    },
                ),
            )
            .route(
                "/repos/{owner}/{name}",
                get(
                    |headers: HeaderMap, Path((owner, name)): Path<(String, String)>| async move {
                        if bearer(&headers) != "ghs_installation" || (owner, name) != ("acme".into(), "site".into()) {
                            return status(404, json!({ "message": "Not Found" }));
                        }
                        status(
                            200,
                            json!({ "id": 7, "full_name": "acme/site", "private": false,
                                    "default_branch": "main", "html_url": "https://github.com/acme/site" }),
                        )
                    },
                ),
            )
            .route(
                "/repos/{owner}/{name}/issues",
                post(
                    |headers: HeaderMap,
                     Path((owner, name)): Path<(String, String)>,
                     Json(body): Json<Value>| async move {
                        if bearer(&headers) != "ghs_installation" || (owner, name) != ("acme".into(), "site".into()) {
                            return status(404, json!({ "message": "Not Found" }));
                        }
                        if body["title"].as_str().is_none_or(str::is_empty) {
                            return status(422, json!({ "message": "Validation Failed" }));
                        }
                        status(
                            201,
                            json!({ "number": 12, "html_url": "https://github.com/acme/site/issues/12" }),
                        )
                    },
                ),
            )
            .route(
                "/login/oauth/access_token",
                post(|Json(body): Json<Value>| async move {
                    if body["code"] == "good-code" && body["client_secret"] == "client-secret" {
                        Json(json!({ "access_token": "ghu_person", "token_type": "bearer" }))
                    } else {
                        Json(json!({ "error": "bad_verification_code" }))
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        Endpoints {
            api: base.clone(),
            web: base,
        }
    }

    #[tokio::test]
    async fn the_calls_carry_the_right_credentials_and_read_githubs_answers() {
        let at = fake_github().await;
        let http = reqwest::Client::new();
        let config = app();
        let github = GitHub {
            http: &http,
            app: &config,
            at,
            seal_key: [7u8; 32],
        };

        assert!(github.user_token("stolen-code").await.is_err());
        let person = github.user_token("good-code").await.unwrap();
        assert_eq!(
            github.user_installation_ids(&person).await.unwrap(),
            [42, 43]
        );
        assert!(github
            .user_installation_ids("ghu_someone_else")
            .await
            .is_err());

        let found = github.installation(42).await.unwrap();
        assert_eq!(found.account_login, "acme");
        assert_eq!(found.repository_selection.as_deref(), Some("all"));

        let (token, expires) = github.mint_installation_token(42).await.unwrap();
        assert_eq!(token, "ghs_installation");
        assert!(expires.is_some());
        assert!(github.mint_installation_token(41).await.is_err());

        let (repos, cut) = github.installation_repos(&token).await.unwrap();
        assert!(!cut);
        assert_eq!(repos[0].full_name, "acme/site");
        assert_eq!(github.repo(&token, "acme/site").await.unwrap().id, 7);
        let hidden = github.repo(&token, "acme/secret").await.unwrap_err();
        assert!(matches!(hidden, Error::Conflict(_)), "{hidden:?}");

        let issue = github
            .create_issue(&token, "acme/site", "The chart is empty", "body")
            .await
            .unwrap();
        assert_eq!(issue.number, 12);
        assert_eq!(issue.html_url, "https://github.com/acme/site/issues/12");
        let refused = github
            .create_issue(&token, "acme/other", "The chart is empty", "body")
            .await
            .unwrap_err();
        assert!(matches!(refused, Error::Conflict(_)), "{refused:?}");

        github.delete_installation(42).await.unwrap();
    }

    /// The token cache, against a real Valkey: sealed at rest, reused after.
    #[tokio::test]
    #[ignore = "needs Valkey"]
    async fn an_installation_token_is_cached_sealed() {
        let redis_url = std::env::var("MICAFORGE_TEST_REDIS_URL")
            .expect("set MICAFORGE_TEST_REDIS_URL to run this test");
        let kv = Kv::connect(&redis_url).await.unwrap();
        forget_token(&kv, 42).await.unwrap();
        let at = fake_github().await;
        let http = reqwest::Client::new();
        let config = app();
        let github = GitHub {
            http: &http,
            app: &config,
            at,
            seal_key: [7u8; 32],
        };

        assert_eq!(
            github.installation_token(&kv, 42).await.unwrap(),
            "ghs_installation"
        );
        let cached: String = kv.get_json(&token_key(42)).await.unwrap().unwrap();
        assert!(
            !cached.contains("ghs_"),
            "the token is never in Valkey in the clear"
        );
        assert_eq!(
            github.installation_token(&kv, 42).await.unwrap(),
            "ghs_installation"
        );

        // Sealed under another key, the entry does not open and a new token
        // is minted instead of failing.
        let other = GitHub {
            seal_key: [8u8; 32],
            ..github
        };
        assert_eq!(
            other.installation_token(&kv, 42).await.unwrap(),
            "ghs_installation"
        );
        forget_token(&kv, 42).await.unwrap();
    }
}
