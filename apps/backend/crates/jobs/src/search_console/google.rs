//! Talking to Google: the OAuth token endpoints and the Search Console API.
//!
//! Every call here is one documented endpoint, and every answer is parsed by a
//! pure function that the tests feed fixtures written to Google's published
//! schema, so the shape this module expects is pinned without a network:
//!
//! - `POST https://oauth2.googleapis.com/token`, `grant_type=authorization_code`
//!   with the PKCE verifier, and `grant_type=refresh_token`.
//! - `POST https://oauth2.googleapis.com/revoke`.
//! - `GET  https://www.googleapis.com/webmasters/v3/sites` (`sites.list`).
//! - `POST https://www.googleapis.com/webmasters/v3/sites/{siteUrl}/searchAnalytics/query`.
//!
//! The base addresses live in [`Endpoints`] so a test can point them at a
//! local stand-in. Nothing here logs a token, and no error text carries one:
//! Google's error bodies are reduced to their `error` code before they travel.

use micaforge_core::config::OAuthClient;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The one scope asked for: read the Search Console data of the properties
/// the person can see. Nothing that writes, and nothing about the person.
pub const SCOPE: &str = "https://www.googleapis.com/auth/webmasters.readonly";

/// Where Google's endpoints are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The consent screen the browser is sent to.
    pub authorize: String,
    /// The token endpoint.
    pub token: String,
    /// The revocation endpoint.
    pub revoke: String,
    /// The Search Console API, without a trailing slash.
    pub api: String,
}

impl Endpoints {
    /// Google's documented addresses.
    pub fn google() -> Endpoints {
        Endpoints {
            authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            api: "https://www.googleapis.com/webmasters/v3".into(),
        }
    }
}

/// Why a call to Google did not give us what we needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleError {
    /// Google refused the refresh token (`invalid_grant`): the person revoked
    /// access, the token expired, or the account was closed. Only connecting
    /// again helps.
    Revoked,
    /// The access token was refused (`401`). One fresh token is worth trying.
    Unauthorized,
    /// The account no longer has access to the property (`403`), or never had
    /// the scope.
    Forbidden,
    /// Too many requests, a Google outage or a network failure: worth trying
    /// again later.
    Transient(String),
    /// An answer this module could not read.
    Malformed(String),
}

impl GoogleError {
    /// A sentence safe to show a site admin. Carries no token and no body.
    pub fn sentence(&self) -> String {
        match self {
            GoogleError::Revoked => "Google no longer accepts this connection. It was revoked or \
                it expired; connect Search Console again."
                .into(),
            GoogleError::Unauthorized => {
                "Google refused the access token for this connection.".into()
            }
            GoogleError::Forbidden => "The Google account behind this connection can no longer \
                read this property. Check its access in Search Console, or connect with another \
                account."
                .into(),
            GoogleError::Transient(why) => {
                format!("Google did not answer: {why}. The next sync tries again.")
            }
            GoogleError::Malformed(why) => {
                format!("Google sent an answer Micaforge could not read: {why}.")
            }
        }
    }
}

impl From<GoogleError> for micaforge_core::Error {
    fn from(e: GoogleError) -> Self {
        match e {
            GoogleError::Revoked | GoogleError::Unauthorized | GoogleError::Forbidden => {
                micaforge_core::Error::Forbidden(e.sentence())
            }
            GoogleError::Transient(_) | GoogleError::Malformed(_) => {
                micaforge_core::Error::Upstream(e.sentence())
            }
        }
    }
}

/// What the code exchange hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// A short-lived access token, used at once to list the properties.
    pub access_token: String,
    /// The long-lived token the sync mints access tokens from. Sealed before
    /// it is stored.
    pub refresh_token: String,
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Trade an authorisation code for tokens, proving the flow with the PKCE
/// verifier.
pub async fn exchange_code(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    client: &OAuthClient,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> Result<Grant, GoogleError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect),
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
        ("code_verifier", verifier),
    ];
    let (status, body) = post_form(http, &endpoints.token, &form).await?;
    parse_grant(status, &body)
}

/// Mint an access token from a refresh token.
pub async fn refresh(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    client: &OAuthClient,
    refresh_token: &str,
) -> Result<String, GoogleError> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
    ];
    let (status, body) = post_form(http, &endpoints.token, &form).await?;
    parse_refresh(status, &body)
}

/// Ask Google to revoke a token. Best effort: the caller deletes its copy
/// whatever this answers, and an already-revoked token is a success here.
pub async fn revoke(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    token: &str,
) -> Result<(), GoogleError> {
    let (status, body) = post_form(http, &endpoints.revoke, &[("token", token)]).await?;
    if status.is_success() || error_code(&body).as_deref() == Some("invalid_token") {
        Ok(())
    } else {
        Err(classify(status, &body))
    }
}

/// The token endpoint's answer to a code exchange.
///
/// A grant without a refresh token is refused: without one the sync could run
/// for an hour and then never again. Google sends it because the consent URL
/// asks for `access_type=offline` with `prompt=consent`. A grant whose
/// `scope` lacks [`SCOPE`] is refused too: Google lets a person untick a scope
/// on the consent screen, and a token that cannot read Search Console is not
/// a connection.
pub fn parse_grant(status: reqwest::StatusCode, body: &Value) -> Result<Grant, GoogleError> {
    if !status.is_success() {
        return Err(classify(status, body));
    }
    let text = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let access_token =
        text("access_token").ok_or_else(|| GoogleError::Malformed("no access token".into()))?;
    let refresh_token = text("refresh_token").ok_or_else(|| {
        GoogleError::Malformed(
            "no refresh token; remove the app's access at Google and connect again".into(),
        )
    })?;
    if let Some(scope) = text("scope") {
        if !scope.split_whitespace().any(|s| s == SCOPE) {
            return Err(GoogleError::Forbidden);
        }
    }
    Ok(Grant {
        access_token,
        refresh_token,
    })
}

/// The token endpoint's answer to a refresh.
pub fn parse_refresh(status: reqwest::StatusCode, body: &Value) -> Result<String, GoogleError> {
    if !status.is_success() {
        return Err(classify(status, body));
    }
    body.get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| GoogleError::Malformed("no access token".into()))
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// One Search Console property the account can read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Property {
    /// `sc-domain:example.com` or a URL prefix such as `https://example.com/`.
    pub site_url: String,
    /// `siteOwner`, `siteFullUser` or `siteRestrictedUser`.
    pub permission: String,
    /// `domain` or `url_prefix`.
    pub kind: String,
}

/// `sites.list`: the properties the account can read.
pub async fn list_properties(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    access_token: &str,
) -> Result<Vec<Property>, GoogleError> {
    let response = http
        .get(format!("{}/sites", endpoints.api))
        .bearer_auth(access_token)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| GoogleError::Transient(network(&e)))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    parse_properties(status, &body)
}

/// The answer to `sites.list`, as a sorted list of readable properties.
///
/// `siteUnverifiedUser` entries are left out: Google lists a property the
/// account once added but never verified, and its data cannot be read.
/// An account with no property answers `{}`, which is an empty list.
pub fn parse_properties(
    status: reqwest::StatusCode,
    body: &Value,
) -> Result<Vec<Property>, GoogleError> {
    if !status.is_success() {
        return Err(classify(status, body));
    }
    let entries = match body.get("siteEntry") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(GoogleError::Malformed("`siteEntry` is not a list".into())),
    };
    let mut out: Vec<Property> = entries
        .iter()
        .filter_map(|e| {
            let site_url = e.get("siteUrl")?.as_str()?.trim().to_string();
            let permission = e.get("permissionLevel")?.as_str()?.to_string();
            if site_url.is_empty() || permission == "siteUnverifiedUser" {
                return None;
            }
            let kind = if site_url.starts_with("sc-domain:") {
                "domain"
            } else {
                "url_prefix"
            };
            Some(Property {
                site_url,
                permission,
                kind: kind.to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.site_url.cmp(&b.site_url));
    out.dedup_by(|a, b| a.site_url == b.site_url);
    Ok(out)
}

/// True for a property address Google could have issued: a domain property
/// (`sc-domain:` and a host) or an `http`/`https` URL prefix ending in `/`.
pub fn is_property(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.len() > 512 {
        return false;
    }
    if let Some(host) = raw.strip_prefix("sc-domain:") {
        return !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    }
    match url::Url::parse(raw) {
        Ok(url) => {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && raw.ends_with('/')
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Search analytics
// ---------------------------------------------------------------------------

/// Most rows one request may ask for; Google's own ceiling.
pub const ROW_LIMIT: u32 = 25_000;

/// One `searchAnalytics.query` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsRequest<'a> {
    /// First day, `YYYY-MM-DD`, Pacific time.
    pub start: &'a str,
    /// Last day, included.
    pub end: &'a str,
    /// The dimensions, in the order the answer's `keys` follow.
    pub dimensions: &'a [&'a str],
    /// How many rows to skip, for paging.
    pub start_row: u32,
    /// How many rows to return, at most [`ROW_LIMIT`].
    pub row_limit: u32,
}

impl AnalyticsRequest<'_> {
    /// The JSON body Google documents.
    ///
    /// `type: web` is the search Google calls "Web" in its own screens.
    /// `dataState: all` includes the newest days Google has not finalised
    /// yet, which is why the sync fetches the last three days again.
    pub fn body(&self) -> Value {
        json!({
            "startDate": self.start,
            "endDate": self.end,
            "dimensions": self.dimensions,
            "type": "web",
            "dataState": "all",
            "startRow": self.start_row,
            "rowLimit": self.row_limit.min(ROW_LIMIT),
        })
    }
}

/// One row of an answer, before it is placed in a grain.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ApiRow {
    /// The dimension values, in request order.
    #[serde(default)]
    pub keys: Vec<String>,
    /// Clicks. Google sends a double.
    #[serde(default)]
    pub clicks: f64,
    /// Impressions. Also a double.
    #[serde(default)]
    pub impressions: f64,
    /// Average position.
    #[serde(default)]
    pub position: f64,
}

/// Run one request.
pub async fn query(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    access_token: &str,
    property: &str,
    request: &AnalyticsRequest<'_>,
) -> Result<Vec<ApiRow>, GoogleError> {
    let url = format!(
        "{}/sites/{}/searchAnalytics/query",
        endpoints.api,
        encode_component(property)
    );
    let response = http
        .post(url)
        .bearer_auth(access_token)
        .header("accept", "application/json")
        .json(&request.body())
        .send()
        .await
        .map_err(|e| GoogleError::Transient(network(&e)))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    parse_rows(status, &body, request.dimensions.len())
}

/// The answer to `searchAnalytics.query`.
///
/// An empty answer has no `rows` key at all. A row whose `keys` do not match
/// the dimensions asked for is refused rather than misplaced: a country
/// filed under device would be a wrong answer, not a missing one.
pub fn parse_rows(
    status: reqwest::StatusCode,
    body: &Value,
    dimensions: usize,
) -> Result<Vec<ApiRow>, GoogleError> {
    if !status.is_success() {
        return Err(classify(status, body));
    }
    let rows = match body.get("rows") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(rows) => rows,
    };
    let rows: Vec<ApiRow> = serde_json::from_value(rows.clone())
        .map_err(|e| GoogleError::Malformed(format!("rows: {e}")))?;
    if let Some(bad) = rows.iter().find(|r| r.keys.len() != dimensions) {
        return Err(GoogleError::Malformed(format!(
            "a row has {} keys for {dimensions} dimensions",
            bad.keys.len()
        )));
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

/// Post a form and read the JSON answer, whatever the status.
async fn post_form(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, Value), GoogleError> {
    let response = http
        .post(url)
        .header("accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| GoogleError::Transient(network(&e)))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

/// Google's error code from an error body: the token endpoint's
/// `{"error": "invalid_grant"}`, or the API's `{"error": {"status": "…"}}`.
fn error_code(body: &Value) -> Option<String> {
    match body.get("error") {
        Some(Value::String(code)) => Some(code.clone()),
        Some(Value::Object(obj)) => obj
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// Sort a failed answer into what the caller can do about it.
pub fn classify(status: reqwest::StatusCode, body: &Value) -> GoogleError {
    let code = error_code(body);
    match (status.as_u16(), code.as_deref()) {
        (_, Some("invalid_grant")) => GoogleError::Revoked,
        (401, _) | (_, Some("UNAUTHENTICATED")) => GoogleError::Unauthorized,
        (403, _) | (_, Some("PERMISSION_DENIED")) => GoogleError::Forbidden,
        (429, _) | (500..=599, _) => GoogleError::Transient(format!("status {}", status.as_u16())),
        (_, Some(code)) => GoogleError::Malformed(format!("status {} ({code})", status.as_u16())),
        _ => GoogleError::Malformed(format!("status {}", status.as_u16())),
    }
}

/// A network failure, said without the URL (which can carry a property).
fn network(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the request timed out".into()
    } else if e.is_connect() {
        "could not connect".into()
    } else {
        "the request failed".into()
    }
}

/// Percent-encode a path segment: everything outside the unreserved set, so
/// `sc-domain:example.com` and `https://example.com/` each stay one segment.
pub fn encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 3);
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
    use reqwest::StatusCode;

    #[test]
    fn a_code_exchange_needs_a_refresh_token_and_the_scope() {
        // Google's documented answer to an offline code exchange.
        let ok = json!({
            "access_token": "ya29.a0-access",
            "expires_in": 3599,
            "refresh_token": "1//0g-refresh",
            "scope": "https://www.googleapis.com/auth/webmasters.readonly",
            "token_type": "Bearer"
        });
        let grant = parse_grant(StatusCode::OK, &ok).unwrap();
        assert_eq!(grant.refresh_token, "1//0g-refresh");

        let mut no_refresh = ok.clone();
        no_refresh.as_object_mut().unwrap().remove("refresh_token");
        assert!(matches!(
            parse_grant(StatusCode::OK, &no_refresh),
            Err(GoogleError::Malformed(_))
        ));

        // The person unticked the Search Console box on the consent screen.
        let mut narrowed = ok.clone();
        narrowed["scope"] = json!("openid");
        assert_eq!(
            parse_grant(StatusCode::OK, &narrowed),
            Err(GoogleError::Forbidden)
        );

        let bad_code = json!({ "error": "invalid_grant", "error_description": "Bad Request" });
        assert_eq!(
            parse_grant(StatusCode::BAD_REQUEST, &bad_code),
            Err(GoogleError::Revoked)
        );
    }

    #[test]
    fn a_revoked_refresh_token_is_told_apart_from_an_outage() {
        let revoked = json!({
            "error": "invalid_grant",
            "error_description": "Token has been expired or revoked."
        });
        assert_eq!(
            parse_refresh(StatusCode::BAD_REQUEST, &revoked),
            Err(GoogleError::Revoked)
        );
        assert!(matches!(
            parse_refresh(StatusCode::SERVICE_UNAVAILABLE, &Value::Null),
            Err(GoogleError::Transient(_))
        ));
        assert_eq!(
            parse_refresh(
                StatusCode::OK,
                &json!({ "access_token": "ya29.x", "expires_in": 3599 })
            )
            .unwrap(),
            "ya29.x"
        );
        // No error text carries a token.
        assert!(!GoogleError::Revoked.sentence().contains("ya29"));
    }

    #[test]
    fn properties_are_listed_without_the_unverified_ones() {
        // `sites.list`, as Google documents its `siteEntry` resource.
        let body = json!({
            "siteEntry": [
                { "siteUrl": "https://www.example.com/", "permissionLevel": "siteFullUser" },
                { "siteUrl": "sc-domain:example.com", "permissionLevel": "siteOwner" },
                { "siteUrl": "https://old.example.com/", "permissionLevel": "siteUnverifiedUser" },
                { "siteUrl": "sc-domain:example.com", "permissionLevel": "siteOwner" }
            ]
        });
        let list = parse_properties(StatusCode::OK, &body).unwrap();
        let urls: Vec<&str> = list.iter().map(|p| p.site_url.as_str()).collect();
        assert_eq!(urls, ["https://www.example.com/", "sc-domain:example.com"]);
        assert_eq!(list[1].kind, "domain");
        assert_eq!(list[0].kind, "url_prefix");
        // An account with no property answers an empty object.
        assert!(parse_properties(StatusCode::OK, &json!({}))
            .unwrap()
            .is_empty());
        let denied =
            json!({ "error": { "code": 403, "message": "…", "status": "PERMISSION_DENIED" } });
        assert_eq!(
            parse_properties(StatusCode::FORBIDDEN, &denied),
            Err(GoogleError::Forbidden)
        );
    }

    #[test]
    fn only_addresses_google_could_issue_are_properties() {
        assert!(is_property("sc-domain:example.com"));
        assert!(is_property("https://example.com/"));
        assert!(is_property("http://example.com/blog/"));
        assert!(
            !is_property("https://example.com"),
            "a prefix ends in a slash"
        );
        assert!(!is_property("sc-domain:"));
        assert!(!is_property("sc-domain:exa mple.com"));
        assert!(!is_property("ftp://example.com/"));
        assert!(!is_property("javascript:alert(1)//"));
    }

    #[test]
    fn analytics_rows_follow_the_dimensions_asked_for() {
        // `searchAnalytics.query`, as Google documents its response body.
        let body = json!({
            "rows": [
                {
                    "keys": ["2026-10-01", "micaforge analytics", "https://example.com/", "usa", "DESKTOP"],
                    "clicks": 12.0, "impressions": 140.0, "ctr": 0.0857, "position": 2.4
                },
                {
                    "keys": ["2026-10-01", "cookieless analytics", "https://example.com/docs", "deu", "MOBILE"],
                    "clicks": 0, "impressions": 9, "ctr": 0, "position": 18.7
                }
            ],
            "responseAggregationType": "byPage"
        });
        let rows = parse_rows(StatusCode::OK, &body, 5).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].keys[3], "usa");
        assert_eq!(rows[1].impressions, 9.0);

        // No `rows` key is an empty answer, not an error.
        let empty = json!({ "responseAggregationType": "byProperty" });
        assert!(parse_rows(StatusCode::OK, &empty, 1).unwrap().is_empty());

        // Keys that do not line up with the dimensions are refused.
        assert!(matches!(
            parse_rows(StatusCode::OK, &body, 4),
            Err(GoogleError::Malformed(_))
        ));
        assert!(matches!(
            parse_rows(StatusCode::TOO_MANY_REQUESTS, &Value::Null, 5),
            Err(GoogleError::Transient(_))
        ));
        assert_eq!(
            parse_rows(StatusCode::UNAUTHORIZED, &Value::Null, 5),
            Err(GoogleError::Unauthorized)
        );
    }

    #[test]
    fn the_request_body_is_the_documented_shape() {
        let req = AnalyticsRequest {
            start: "2026-10-01",
            end: "2026-10-01",
            dimensions: &["date", "query"],
            start_row: 25_000,
            row_limit: 99_999,
        };
        let body = req.body();
        assert_eq!(body["startDate"], "2026-10-01");
        assert_eq!(body["dimensions"], json!(["date", "query"]));
        assert_eq!(body["dataState"], "all");
        assert_eq!(body["type"], "web");
        assert_eq!(body["startRow"], 25_000);
        assert_eq!(body["rowLimit"], ROW_LIMIT, "capped at Google's ceiling");
    }

    #[test]
    fn a_property_stays_one_path_segment() {
        assert_eq!(
            encode_component("sc-domain:example.com"),
            "sc-domain%3Aexample.com"
        );
        assert_eq!(
            encode_component("https://example.com/"),
            "https%3A%2F%2Fexample.com%2F"
        );
    }

    /// The token endpoint, the API and revocation against a local stand-in
    /// for Google: the verifier and the secret reach the token endpoint, the
    /// bearer reaches the API, and the property arrives as one segment.
    #[tokio::test]
    async fn the_calls_reach_the_documented_endpoints() {
        use axum::extract::{Form, Path};
        use axum::http::HeaderMap;
        use axum::routing::{get, post};
        use axum::Json;
        use std::collections::HashMap;

        async fn token(Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            match form.get("grant_type").map(String::as_str) {
                Some("authorization_code")
                    if form.get("code_verifier").map(String::as_str) == Some("verifier-1")
                        && form.get("client_secret").map(String::as_str) == Some("s3cret") =>
                {
                    (
                        StatusCode::OK,
                        Json(json!({ "access_token": "acc-1", "refresh_token": "ref-1",
                            "scope": SCOPE, "expires_in": 3599, "token_type": "Bearer" })),
                    )
                }
                Some("refresh_token")
                    if form.get("refresh_token").map(String::as_str) == Some("ref-1") =>
                {
                    (
                        StatusCode::OK,
                        Json(json!({ "access_token": "acc-2", "expires_in": 3599 })),
                    )
                }
                _ => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "invalid_grant" })),
                ),
            }
        }
        async fn sites(headers: HeaderMap) -> (StatusCode, Json<Value>) {
            if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer acc-2") {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": { "status": "UNAUTHENTICATED" } })),
                );
            }
            (
                StatusCode::OK,
                Json(
                    json!({ "siteEntry": [{ "siteUrl": "sc-domain:example.com", "permissionLevel": "siteOwner" }] }),
                ),
            )
        }
        async fn analytics(Path(site): Path<String>, Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(site, "sc-domain:example.com");
            assert_eq!(body["dimensions"], json!(["date"]));
            Json(
                json!({ "rows": [{ "keys": ["2026-10-01"], "clicks": 3, "impressions": 30, "ctr": 0.1, "position": 4.0 }] }),
            )
        }
        async fn revoke_ok() -> StatusCode {
            StatusCode::OK
        }

        let app = axum::Router::new()
            .route("/token", post(token))
            .route("/revoke", post(revoke_ok))
            .route("/api/sites", get(sites))
            .route("/api/sites/{site}/searchAnalytics/query", post(analytics));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let endpoints = Endpoints {
            authorize: format!("{base}/authorize"),
            token: format!("{base}/token"),
            revoke: format!("{base}/revoke"),
            api: format!("{base}/api"),
        };
        let client = OAuthClient {
            client_id: "id".into(),
            client_secret: "s3cret".into(),
        };
        let http = reqwest::Client::new();

        let wrong = exchange_code(&http, &endpoints, &client, "c", "not-it", "https://x/cb").await;
        assert_eq!(wrong, Err(GoogleError::Revoked));
        let grant = exchange_code(
            &http,
            &endpoints,
            &client,
            "c",
            "verifier-1",
            "https://x/cb",
        )
        .await
        .unwrap();
        assert_eq!(grant.refresh_token, "ref-1");

        let access = refresh(&http, &endpoints, &client, &grant.refresh_token)
            .await
            .unwrap();
        assert_eq!(
            refresh(&http, &endpoints, &client, "stale").await,
            Err(GoogleError::Revoked)
        );
        let listed = list_properties(&http, &endpoints, &access).await.unwrap();
        assert_eq!(listed[0].site_url, "sc-domain:example.com");
        assert_eq!(
            list_properties(&http, &endpoints, "acc-1").await,
            Err(GoogleError::Unauthorized)
        );

        let req = AnalyticsRequest {
            start: "2026-10-01",
            end: "2026-10-01",
            dimensions: &["date"],
            start_row: 0,
            row_limit: ROW_LIMIT,
        };
        let rows = query(&http, &endpoints, &access, "sc-domain:example.com", &req)
            .await
            .unwrap();
        assert_eq!(rows[0].clicks, 3.0);
        assert!(revoke(&http, &endpoints, "ref-1").await.is_ok());
    }
}
