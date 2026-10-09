//! Short links: `/api/links*`, `/api/link-folders*`, and the redirect itself.
//!
//! A short link sends `/l/<slug>` on the API origin (and `/<slug>` on
//! `MICAFORGE_LINKS_HOST`, when one is set) to a target, counts the click
//! without a cookie, and answers a chat app's preview bot with a card instead
//! of a redirect. The management routes are ordinary authenticated JSON; the
//! redirect is the stranger-facing half and is built like ingest:
//!
//! - **It never waits on ClickHouse.** The click row goes onto its own writer
//!   lane ([`AppState::link_clicks`]) and the answer leaves at once.
//! - **It stores no address.** A click is enriched by the same functions a
//!   pageview is (the user-agent parser, the GeoIP database, the trusted
//!   edge's country) and the address is dropped.
//! - **It decides in one pure function.** [`decide`] takes the link, the time
//!   and the user agent and says what to serve. Every rule (expiry, preview
//!   bots, the app deep link, the phone targets, the UTM parameters) is tested
//!   there rather than through HTTP.
//!
//! ## Why 302 and `no-store`
//!
//! A permanent redirect would be cached by browsers and proxies, so the second
//! click would never reach the server: the count would be wrong and a changed
//! target would never take effect. Every answer here is uncacheable.
//!
//! ## One namespace
//!
//! A slug is unique on the install, so `/l/<slug>` and the links host's
//! `/<slug>` are the same link. The links host is matched on the `Host` header
//! by [`host_fallback`], the router's fallback, so it costs the API nothing:
//! a request for any other host falls through to an ordinary `404`.

use std::fmt::Write as _;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Duration, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use micaforge_core::{crypto, new_id, Error, Granularity, Result};
use micaforge_enrich::{referrer, ua};
use micaforge_query::exec::fetch_all;
use micaforge_query::sql::{bucket_millis, fmt_ts, Param};
use micaforge_store::ch::{country_code, LinkClickRow};
use micaforge_store::models::links::{self, FolderFilter, Link, LinkFields, Scope};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::audit::{self, Entry};
use crate::error::ApiError;
use crate::extract::{AuthUser, ClientAddr};
use crate::response::ok;
use crate::routes::orgs::{authorize_org, MEMBER, VIEWER};
use crate::routes::sites::{authorize_site, authorize_site_ref};
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Longest slug.
const MAX_SLUG: usize = 64;

/// Length of a generated slug. Seven characters of sixty-two is three and a
/// half trillion slugs, so a collision is a retry, not a design problem.
const GENERATED_SLUG: usize = 7;

/// Slugs that would shadow a path the links host or the API origin serves.
const RESERVED_SLUGS: &[&str] = &["api", "l", "app", "share", "status", "admin", "docs"];

/// Longest URL accepted in any link field.
const MAX_URL: usize = 2048;

/// The UTM keys a link may set, in the order they are appended.
const UTM_KEYS: &[&str] = &["source", "medium", "campaign", "term", "content"];

/// How long a phone waits for the app before the web target, in ms.
const DEEP_LINK_WAIT_MS: u32 = 1500;

/// Every authenticated short-link route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/links", get(list_links).post(create_link))
        .route("/api/links/config", get(links_config))
        .route(
            "/api/links/{id}",
            get(get_link).patch(update_link).delete(delete_link),
        )
        .route("/api/links/{id}/qr.svg", get(qr_svg))
        .route("/api/links/{id}/stats", get(link_stats))
        .route("/api/link-folders", get(list_folders).post(create_folder))
        .route(
            "/api/link-folders/{id}",
            axum::routing::patch(rename_folder).delete(delete_folder),
        )
}

/// The redirect on the API origin. Merged with the collection group: it is
/// open to everyone and answers HTML, not the API's JSON.
pub fn redirect_router() -> Router<AppState> {
    Router::new().route("/l/{slug}", get(redirect))
}

// ---------------------------------------------------------------------------
// The redirect
// ---------------------------------------------------------------------------

/// What a link request is answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The link has expired: a `410` page.
    Expired,
    /// A link-preview bot: a page of Open Graph tags, no redirect.
    Preview {
        /// The bot's name, for the click row.
        bot: &'static str,
    },
    /// A phone and an app deep link: a page that tries the app first.
    DeepLink {
        /// The custom-scheme address.
        app: String,
        /// Where to go when the app does not open.
        fallback: String,
    },
    /// A plain redirect.
    Redirect {
        /// Where to.
        to: String,
        /// `web`, `ios` or `android`, for the click row.
        target: &'static str,
    },
}

impl Decision {
    /// The `target` column of the click row.
    pub fn target(&self) -> &'static str {
        match self {
            Decision::Expired => "expired",
            Decision::Preview { .. } => "preview",
            Decision::DeepLink { .. } => "deep_link",
            Decision::Redirect { target, .. } => target,
        }
    }
}

/// Which phone a user agent is, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phone {
    /// iPhone, iPad, iPod.
    Ios,
    /// Android.
    Android,
}

/// The phone family of a request, from the same parser ingest uses.
pub fn phone_of(client: &ua::Client) -> Option<Phone> {
    match client.os.as_str() {
        "iOS" | "iPadOS" => Some(Phone::Ios),
        "Android" => Some(Phone::Android),
        _ => None,
    }
}

/// Link-preview bots: chat apps and social networks that fetch a link to draw
/// a card. Matched case-insensitively as substrings of the user agent; the
/// first match names the bot.
///
/// Telegram first: its user agent says it is "like TwitterBot".
const PREVIEW_BOTS: &[(&str, &str)] = &[
    ("telegrambot", "TelegramBot"),
    ("twitterbot", "Twitterbot"),
    ("slackbot", "Slackbot"),
    ("slack-imgproxy", "Slackbot"),
    ("facebookexternalhit", "facebookexternalhit"),
    ("facebot", "Facebot"),
    ("discordbot", "Discordbot"),
    ("linkedinbot", "LinkedInBot"),
    ("whatsapp", "WhatsApp"),
    ("skypeuripreview", "SkypeUriPreview"),
    ("pinterestbot", "Pinterestbot"),
    ("pinterest/", "Pinterestbot"),
    ("redditbot", "redditbot"),
    ("embedly", "Embedly"),
    ("iframely", "Iframely"),
    ("mastodon", "Mastodon"),
    ("cardyb", "Bluesky"),
    ("bluesky", "Bluesky"),
    ("vkshare", "vkShare"),
    ("snapchat", "Snapchat"),
    ("google-pagerenderer", "Google"),
    ("applebot", "Applebot"),
];

/// The preview bot a user agent belongs to, if any.
pub fn preview_bot(user_agent: &str) -> Option<&'static str> {
    let ua = user_agent.to_ascii_lowercase();
    PREVIEW_BOTS
        .iter()
        .find(|(needle, _)| ua.contains(needle))
        .map(|(_, name)| *name)
}

/// What to serve for `link` to this client at `now`.
///
/// The rules, in order: an expired link is gone for everyone, bots included;
/// a preview bot gets the card; a phone with an app deep link gets the page
/// that tries the app, falling back to its own target or the web one; a phone
/// gets its own target when the link has one; everyone else gets the target
/// with the UTM parameters appended.
pub fn decide(link: &Link, now: DateTime<Utc>, user_agent: &str, client: &ua::Client) -> Decision {
    if link.is_expired(now) {
        return Decision::Expired;
    }
    if let Some(bot) = preview_bot(user_agent) {
        return Decision::Preview { bot };
    }
    let web = final_url(&link.target_url, &link.utm);
    let phone = phone_of(client);
    let phone_target = match phone {
        Some(Phone::Ios) => link.ios_url.as_deref().map(|u| (u, "ios")),
        Some(Phone::Android) => link.android_url.as_deref().map(|u| (u, "android")),
        None => None,
    };
    if let (Some(_), Some(app)) = (phone, link.deep_link.as_deref()) {
        return Decision::DeepLink {
            app: app.to_string(),
            fallback: phone_target
                .map(|(u, _)| u.to_string())
                .unwrap_or_else(|| web.clone()),
        };
    }
    match phone_target {
        Some((to, target)) => Decision::Redirect {
            to: to.to_string(),
            target,
        },
        None => Decision::Redirect {
            to: web,
            target: "web",
        },
    }
}

/// The target with the link's UTM parameters appended.
///
/// The target's own query is kept in its order; a `utm_*` the link sets
/// replaces one the target already carries, so the link's tagging is the one
/// that counts. Values are form-encoded by the URL library.
pub fn final_url(target: &str, utm: &Value) -> String {
    let Ok(mut url) = url::Url::parse(target) else {
        return target.to_string();
    };
    let set: Vec<(String, String)> = UTM_KEYS
        .iter()
        .filter_map(|key| {
            utm.get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| (format!("utm_{key}"), v.to_string()))
        })
        .collect();
    if set.is_empty() {
        return url.to_string();
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !set.iter().any(|(s, _)| s == k))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (k, v) in kept.iter().chain(set.iter()) {
            pairs.append_pair(k, v);
        }
    }
    url.to_string()
}

/// `GET /l/{slug}` on the API origin.
async fn redirect(
    State(st): State<AppState>,
    Path(slug): Path<String>,
    ClientAddr(ip): ClientAddr,
    method: Method,
    headers: HeaderMap,
) -> Response {
    serve(&st, &slug, ip, &method, &headers).await
}

/// The router's fallback: a short link on the links host, or the ordinary
/// `404` for anything else.
pub async fn host_fallback(State(st): State<AppState>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let on_links_host =
        st.cfg.links_host.as_deref().is_some_and(|links| {
            request_host(&parts.headers, &parts.uri).as_deref() == Some(links)
        });
    let reading = parts.method == Method::GET || parts.method == Method::HEAD;
    if on_links_host && reading {
        let path = parts.uri.path().trim_start_matches('/');
        if !path.is_empty() && !path.contains('/') {
            let peer = parts
                .extensions
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|axum::extract::ConnectInfo(addr)| addr.ip());
            let ip = crate::client_ip::client_ip(&parts.headers, peer);
            return serve(&st, path, ip, &parts.method, &parts.headers).await;
        }
        return html(StatusCode::NOT_FOUND, &not_found_page());
    }
    ApiError(Error::not_found("route")).into_response()
}

/// The host a request was made to, lower-cased and without a port.
fn request_host(headers: &HeaderMap, uri: &axum::http::Uri) -> Option<String> {
    let raw = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| uri.host())?;
    let host = raw.trim().to_ascii_lowercase();
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name.to_string(),
        _ => host,
    };
    Some(host)
}

/// Resolve and answer one short-link request.
async fn serve(
    st: &AppState,
    slug: &str,
    ip: std::net::IpAddr,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    if check_slug(slug).is_err() {
        return html(StatusCode::NOT_FOUND, &not_found_page());
    }
    let resolved = match links::get_by_slug(&st.pg, slug).await {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return html(StatusCode::NOT_FOUND, &not_found_page()),
        Err(e) => {
            tracing::error!(error = %e, "could not resolve a short link");
            return html(
                StatusCode::SERVICE_UNAVAILABLE,
                &page(
                    "This link is not available right now",
                    "The link service could not look this link up. Try again in a minute.",
                    None,
                ),
            );
        }
    };
    let link = &resolved.link;

    let text = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let user_agent = text("user-agent").unwrap_or_default();
    let client = ua::parse(
        user_agent,
        text("sec-ch-ua"),
        text("sec-ch-ua-platform"),
        text("sec-ch-ua-mobile"),
    );
    let decision = decide(link, Utc::now(), user_agent, &client);

    // A HEAD is a link checker asking whether the link works, not a click.
    if method == Method::GET {
        record_click(st, &resolved, ip, headers, user_agent, &client, &decision);
    }

    let short = short_url(st, &link.slug);
    match decision {
        Decision::Expired => html(StatusCode::GONE, &expired_page()),
        Decision::Preview { .. } => html(
            StatusCode::OK,
            &preview_page(link, &short, &final_url(&link.target_url, &link.utm)),
        ),
        Decision::DeepLink { app, fallback } => {
            let nonce = crypto::random_token(16);
            let mut response = html(StatusCode::OK, &deep_link_page(&app, &fallback, &nonce));
            if let Ok(value) = HeaderValue::from_str(&format!(
                "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; \
                 base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
            )) {
                response
                    .headers_mut()
                    .insert(header::CONTENT_SECURITY_POLICY, value);
            }
            response
        }
        Decision::Redirect { to, .. } => found(&to),
    }
}

/// Queue the click row. Returns at once.
fn record_click(
    st: &AppState,
    resolved: &links::Resolved,
    ip: std::net::IpAddr,
    headers: &HeaderMap,
    user_agent: &str,
    client: &ua::Client,
    decision: &Decision,
) {
    let mut location = st.geo.lookup(ip);
    if location.country.is_empty() {
        if let Some(edge) = crate::routes::ingest::edge_location(headers) {
            location.country = edge.country;
            location.region = edge.region;
        }
    }

    let referrer_text: String = headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .chars()
        .take(512)
        .collect();
    let referrer_host = referrer::referrer_host(&referrer_text).unwrap_or_default();

    let (is_bot, bot_name) = match decision {
        Decision::Preview { bot } => (1, (*bot).to_string()),
        _ => match micaforge_agents::classify(
            user_agent,
            &micaforge_agents::HeaderView {
                accept: text_of(headers, "accept"),
                accept_language: text_of(headers, "accept-language"),
                sec_fetch_mode: text_of(headers, "sec-fetch-mode"),
                sec_ch_ua: text_of(headers, "sec-ch-ua"),
                from: text_of(headers, "from"),
                signature_agent: text_of(headers, "signature-agent"),
            },
            None,
        ) {
            micaforge_agents::Classification::Human => (0, String::new()),
            micaforge_agents::Classification::Agent(def) => (1, def.slug.to_string()),
            micaforge_agents::Classification::SuspectedBot(_) => (1, String::new()),
        },
    };

    st.link_clicks.push(LinkClickRow {
        site_id: resolved
            .site_number
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(0),
        timestamp: Utc::now(),
        link_id: resolved.link.id,
        org_id: resolved.link.org_id,
        referrer: referrer_text,
        referrer_host,
        country: country_code(&location.country),
        region: location.region,
        device_type: client.device_type.clone(),
        browser: client.browser.clone(),
        os: client.os.clone(),
        is_bot,
        bot_name,
        target: decision.target().to_string(),
    });
}

fn text_of<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// A `302` that no cache keeps.
fn found(to: &str) -> Response {
    let Ok(location) = HeaderValue::from_str(to) else {
        return html(StatusCode::NOT_FOUND, &not_found_page());
    };
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::FOUND;
    let h = response.headers_mut();
    h.insert(header::LOCATION, location);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    response
}

/// An HTML answer that no cache keeps.
fn html(status: StatusCode, body: &str) -> Response {
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'none'; img-src https:; style-src 'unsafe-inline'; \
                 base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            ),
        );
    }
    response
}

/// Escape text for HTML content and double-quoted attributes.
pub fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// The one small page every non-redirect answer is drawn in: light, plain,
/// and complete without script.
fn page(title: &str, sentence: &str, link: Option<(&str, &str)>) -> String {
    let action = link
        .map(|(href, label)| format!("<p><a href=\"{}\">{}</a></p>", escape(href), escape(label)))
        .unwrap_or_default();
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex\"><title>{t}</title>\
         <style>body{{margin:0;font:15px/1.6 system-ui,sans-serif;background:#fafafa;color:#18181b}}\
         main{{max-width:34rem;margin:15vh auto;padding:0 1.25rem}}\
         h1{{font-size:1.375rem;font-weight:600;letter-spacing:-0.02em;color:#09090b;margin:0 0 .5rem}}\
         p{{color:#52525b;margin:0 0 1rem}}a{{color:#1d4fd7}}</style></head>\
         <body><main><h1>{t}</h1><p>{s}</p>{action}</main></body></html>",
        t = escape(title),
        s = escape(sentence),
    )
}

fn not_found_page() -> String {
    page(
        "There is no link at this address",
        "Check the address for a typing mistake. The link may also have been deleted.",
        None,
    )
}

fn expired_page() -> String {
    page(
        "This link has expired",
        "Whoever shared this link set it to stop working after a date, and that date has passed.",
        None,
    )
}

/// The card a preview bot reads. No redirect, so the bot describes this link
/// rather than whatever page the target happens to be.
fn preview_page(link: &Link, short: &str, target: &str) -> String {
    let host = url::Url::parse(target)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let title = link
        .title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(&host);
    let description = link.description.as_deref().unwrap_or_default();
    let mut tags = String::new();
    // Open Graph tags are `property`, Twitter's are `name`: each reader looks
    // for its own attribute.
    let mut meta = |key: &str, content: &str| {
        if !content.is_empty() {
            let attribute = if key.starts_with("og:") {
                "property"
            } else {
                "name"
            };
            let _ = write!(
                tags,
                "<meta {attribute}=\"{k}\" content=\"{c}\">",
                k = escape(key),
                c = escape(content)
            );
        }
    };
    meta("og:type", "website");
    meta("og:url", short);
    meta("og:title", title);
    meta("og:description", description);
    meta("twitter:title", title);
    meta("twitter:description", description);
    match link.image_url.as_deref() {
        Some(image) => {
            meta("og:image", image);
            meta("twitter:image", image);
            meta("twitter:card", "summary_large_image");
        }
        None => meta("twitter:card", "summary"),
    }
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>{t}</title>{tags}<meta name=\"description\" content=\"{d}\"></head>\
         <body><h1>{t}</h1><p>{d}</p><p><a href=\"{href}\">{host}</a></p></body></html>",
        t = escape(title),
        d = escape(description),
        href = escape(target),
        host = escape(&host),
    )
}

/// The page that tries an app first on a phone.
///
/// The script is the only one this server writes, and it carries a fresh
/// nonce that the response's CSP names, so nothing else on the page could
/// run. Without script the reader still has both links.
fn deep_link_page(app: &str, fallback: &str, nonce: &str) -> String {
    let app_json = serde_json::to_string(app).unwrap_or_else(|_| "\"\"".to_string());
    let web_json = serde_json::to_string(fallback).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex\"><title>Opening the app</title>\
         <style>body{{margin:0;font:15px/1.6 system-ui,sans-serif;background:#fafafa;color:#18181b}}\
         main{{max-width:34rem;margin:15vh auto;padding:0 1.25rem}}a{{color:#1d4fd7}}</style></head>\
         <body><main><p>Opening the app. If nothing happens, \
         <a href=\"{web}\">continue in the browser</a> or <a href=\"{app_href}\">open the app</a>.</p></main>\
         <script nonce=\"{nonce}\">(function(){{var a={app_json},w={web_json};\
         var t=setTimeout(function(){{location.replace(w)}},{wait});\
         document.addEventListener('visibilitychange',function(){{if(document.hidden)clearTimeout(t)}});\
         location.href=a;}})();</script></body></html>",
        web = escape(fallback),
        app_href = escape(app),
        wait = DEEP_LINK_WAIT_MS,
        app_json = app_json.replace('<', "\\u003c"),
        web_json = web_json.replace('<', "\\u003c"),
    )
}

// ---------------------------------------------------------------------------
// QR codes
// ---------------------------------------------------------------------------

/// An SVG QR code of `text`, at least `size` pixels square.
///
/// Error correction level M (about 15% of the code can be damaged or covered
/// and it still scans), with the standard four-module quiet zone, black on
/// white: the combination every phone camera reads first time.
pub fn qr(text: &str, size: u32) -> Result<String> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)
        .map_err(|e| Error::validation("url", format!("cannot be drawn as a QR code: {e}")))?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(size, size)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build())
}

/// `?size=`
#[derive(Debug, Deserialize)]
struct QrQuery {
    #[serde(default)]
    size: Option<u32>,
}

/// `GET /api/links/{id}/qr.svg?size=256`
async fn qr_svg(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<QrQuery>,
) -> ApiResult<Response> {
    let link = reachable(&st, id, auth.user.id, VIEWER).await?;
    let svg = qr(
        &short_url(&st, &link.slug),
        q.size.unwrap_or(256).clamp(64, 2048),
    )?;
    let mut response = Response::new(Body::from(svg));
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/svg+xml"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("inline; filename=\"{}.svg\"", link.slug)) {
        h.insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

// ---------------------------------------------------------------------------
// Management
// ---------------------------------------------------------------------------

/// The address a slug is served at.
pub fn short_url(st: &AppState, slug: &str) -> String {
    format!("{}/{slug}", short_base(st))
}

/// The base every short address starts with.
fn short_base(st: &AppState) -> String {
    match st.cfg.links_host.as_deref() {
        Some(host) => {
            let scheme = if st.cfg.public_url.starts_with("https://") {
                "https"
            } else {
                "http"
            };
            format!("{scheme}://{host}")
        }
        None => format!("{}/l", st.cfg.public_url.trim_end_matches('/')),
    }
}

/// A slug: 1 to 64 of `[A-Za-z0-9_-]`, not one of [`RESERVED_SLUGS`].
fn check_slug(slug: &str) -> Result<()> {
    let ok = !slug.is_empty()
        && slug.len() <= MAX_SLUG
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        return Err(Error::validation(
            "slug",
            format!("must be 1 to {MAX_SLUG} letters, digits, `-` or `_`"),
        ));
    }
    if RESERVED_SLUGS.contains(&slug.to_ascii_lowercase().as_str()) {
        return Err(Error::validation("slug", "is reserved; choose another"));
    }
    Ok(())
}

/// A fresh random slug of letters and digits.
fn generated_slug() -> String {
    loop {
        let candidate: String = crypto::random_token(16)
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(GENERATED_SLUG)
            .collect();
        if candidate.len() == GENERATED_SLUG {
            return candidate;
        }
    }
}

/// A link as the dashboard sends it, and as the MCP link tools build it.
#[derive(Debug, Deserialize)]
pub(crate) struct LinkBody {
    #[serde(default)]
    slug: Option<String>,
    target_url: String,
    #[serde(default)]
    folder_id: Option<Uuid>,
    #[serde(default)]
    utm: Option<Value>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    ios_url: Option<String>,
    #[serde(default)]
    android_url: Option<String>,
    #[serde(default)]
    deep_link: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

/// A web address for a link field: `http` or `https`, or for the phone
/// targets also an app-store scheme.
fn link_url(raw: &str, field: &str, store_schemes: bool) -> Result<String> {
    let raw = raw.trim();
    if raw.len() > MAX_URL {
        return Err(Error::validation(
            field,
            format!("must be at most {MAX_URL} characters"),
        ));
    }
    let url =
        url::Url::parse(raw).map_err(|_| Error::validation(field, "is not an absolute URL"))?;
    let scheme_ok = match url.scheme() {
        "http" | "https" => url.host_str().is_some(),
        "itms-apps" | "market" => store_schemes,
        _ => false,
    };
    if !scheme_ok {
        return Err(Error::validation(
            field,
            if store_schemes {
                "must be an http, https, itms-apps or market address"
            } else {
                "must be an http or https address"
            },
        ));
    }
    Ok(url.to_string())
}

/// An app deep link: a custom scheme, never a web or script one.
fn deep_link(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.len() > MAX_URL {
        return Err(Error::validation(
            "deep_link",
            format!("must be at most {MAX_URL} characters"),
        ));
    }
    let scheme = raw.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    let valid = scheme.as_deref().is_some_and(|s| {
        s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && !matches!(
                s,
                "http" | "https" | "javascript" | "data" | "file" | "vbscript" | "blob" | "about"
            )
    });
    if !valid || raw.chars().any(char::is_whitespace) {
        return Err(Error::validation(
            "deep_link",
            "must be an app address with its own scheme, such as myapp://item/42",
        ));
    }
    Ok(raw.to_string())
}

/// An optional text field, trimmed, empty as absent, held to `max` characters.
fn optional_text(value: Option<&str>, field: &str, max: usize) -> Result<Option<String>> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) if v.chars().count() > max => Err(Error::validation(
            field,
            format!("must be at most {max} characters"),
        )),
        Some(v) => Ok(Some(v.to_string())),
    }
}

/// The UTM document: only the five keys, each a short string.
fn utm_of(value: Option<&Value>) -> Result<Value> {
    let mut out = Map::new();
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Value::Object(out));
    };
    let object = value
        .as_object()
        .ok_or_else(|| Error::validation("utm", "must be an object"))?;
    for (key, raw) in object {
        if !UTM_KEYS.contains(&key.as_str()) {
            return Err(Error::validation(
                "utm",
                format!("`{key}` is not one of source, medium, campaign, term, content"),
            ));
        }
        if let Some(text) = optional_text(raw.as_str(), "utm", 200)? {
            out.insert(key.clone(), Value::String(text));
        }
    }
    Ok(Value::Object(out))
}

/// Validate a body. `slug` is the one to use when the body leaves it empty.
fn link_fields(body: &LinkBody, slug: String) -> Result<LinkFields> {
    check_slug(&slug)?;
    let image_url = match optional_text(body.image_url.as_deref(), "image_url", MAX_URL)? {
        Some(raw) => {
            let url = link_url(&raw, "image_url", false)?;
            if !url.starts_with("https://") {
                return Err(Error::validation("image_url", "must be an https address"));
            }
            Some(url)
        }
        None => None,
    };
    let phone = |raw: Option<&str>, field: &str| -> Result<Option<String>> {
        optional_text(raw, field, MAX_URL)?
            .map(|r| link_url(&r, field, true))
            .transpose()
    };
    Ok(LinkFields {
        slug,
        folder_id: body.folder_id,
        target_url: link_url(&body.target_url, "target_url", false)?,
        utm: utm_of(body.utm.as_ref())?,
        title: optional_text(body.title.as_deref(), "title", 200)?,
        description: optional_text(body.description.as_deref(), "description", 500)?,
        image_url,
        ios_url: phone(body.ios_url.as_deref(), "ios_url")?,
        android_url: phone(body.android_url.as_deref(), "android_url")?,
        deep_link: optional_text(body.deep_link.as_deref(), "deep_link", MAX_URL)?
            .map(|r| deep_link(&r))
            .transpose()?,
        expires_at: body.expires_at,
    })
}

/// One link on the wire.
fn link_json(st: &AppState, link: &Link, clicks_30d: u64) -> Value {
    json!({
        "id": link.id,
        "org_id": link.org_id,
        "site_id": link.site_id,
        "folder_id": link.folder_id,
        "slug": link.slug,
        "short_url": short_url(st, &link.slug),
        "target_url": link.target_url,
        "final_url": final_url(&link.target_url, &link.utm),
        "utm": link.utm,
        "title": link.title,
        "description": link.description,
        "image_url": link.image_url,
        "ios_url": link.ios_url,
        "android_url": link.android_url,
        "deep_link": link.deep_link,
        "expires_at": link.expires_at,
        "expired": link.is_expired(Utc::now()),
        "clicks_30d": clicks_30d,
        "created_at": link.created_at,
        "updated_at": link.updated_at,
    })
}

/// The owner a list or a create addresses: `?site=` or `?org=`.
#[derive(Debug, Deserialize)]
struct OwnerQuery {
    #[serde(default)]
    site: Option<String>,
    #[serde(default)]
    org: Option<Uuid>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

/// Who owns what a request addresses.
struct Owner {
    org_id: Uuid,
    site: Option<micaforge_store::models::sites::Site>,
}

/// Resolve `?site=` or `?org=` and assert the caller's role on it.
async fn owner(st: &AppState, user: Uuid, q: &OwnerQuery, needed: &str) -> Result<Owner> {
    if let Some(site) = q.site.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let site = authorize_site_ref(st, site, user, needed).await?;
        return Ok(Owner {
            org_id: site.org_id,
            site: Some(site),
        });
    }
    if let Some(org) = q.org {
        let (org, _) = authorize_org(&st.pg, org, user, needed).await?;
        return Ok(Owner {
            org_id: org.id,
            site: None,
        });
    }
    Err(Error::validation(
        "site",
        "is required, or `org` for organisation links",
    ))
}

/// Human clicks in the last thirty days, per link.
async fn clicks_30d(st: &AppState, ids: &[Uuid]) -> Result<Vec<(Uuid, u64)>> {
    #[derive(Debug, clickhouse::Row, Deserialize)]
    struct Count {
        #[serde(with = "clickhouse::serde::uuid")]
        link_id: Uuid,
        clicks: u64,
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let slots = vec!["toUUID(?)"; ids.len()].join(", ");
    let sql = format!(
        "SELECT link_id, countIf(is_bot = 0) AS clicks FROM link_clicks
         WHERE link_id IN ({slots}) AND timestamp >= toDateTime64(?, 3, 'UTC')
         GROUP BY link_id"
    );
    let mut params: Vec<Param> = ids.iter().map(|id| Param::Str(id.to_string())).collect();
    params.push(Param::Str(fmt_ts(Utc::now() - Duration::days(30))));
    let rows = fetch_all::<Count>(&st.ch, "link_clicks_30d", &sql, &params).await?;
    Ok(rows.into_iter().map(|r| (r.link_id, r.clicks)).collect())
}

/// `GET /api/links?site=|org=&folder=&q=`
async fn list_links(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(q): Query<OwnerQuery>,
) -> ApiResult<Json<Value>> {
    let owner = owner(&st, auth.user.id, &q, VIEWER).await?;
    let scope = match &owner.site {
        Some(site) => Scope::Site(site.id),
        None => Scope::Org(owner.org_id),
    };
    let folder = match q.folder.as_deref().map(str::trim) {
        None | Some("") | Some("all") => FolderFilter::Any,
        Some("none") => FolderFilter::None,
        Some(raw) => FolderFilter::One(
            raw.parse()
                .map_err(|_| Error::validation("folder", "is not a folder id"))?,
        ),
    };
    Ok(ok(links_value(&st, scope, folder, q.q.as_deref()).await?))
}

/// A list of links with their recent clicks, for the route and the MCP tool.
pub(crate) async fn links_value(
    st: &AppState,
    scope: Scope,
    folder: FolderFilter,
    search: Option<&str>,
) -> Result<Value> {
    let rows = links::list(&st.pg, scope, folder, search, 500).await?;
    let ids: Vec<Uuid> = rows.iter().map(|l| l.id).collect();
    let counts = clicks_30d(st, &ids).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|l| {
            let n = counts
                .iter()
                .find(|(id, _)| *id == l.id)
                .map(|(_, n)| *n)
                .unwrap_or(0);
            link_json(st, l, n)
        })
        .collect();
    Ok(Value::Array(out))
}

/// Refuse a folder of another organisation.
async fn check_folder(st: &AppState, folder: Option<Uuid>, org: Uuid) -> Result<()> {
    if let Some(id) = folder {
        let found = links::get_folder(&st.pg, id).await.map_err(|e| match e {
            Error::NotFound(_) => Error::validation("folder_id", "is not a folder"),
            other => other,
        })?;
        if found.org_id != org {
            return Err(Error::validation("folder_id", "is not a folder"));
        }
    }
    Ok(())
}

/// `POST /api/links?site=|org=`
async fn create_link(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(q): Query<OwnerQuery>,
    Json(body): Json<LinkBody>,
) -> ApiResult<Json<Value>> {
    let owner = owner(&st, auth.user.id, &q, MEMBER).await?;
    Ok(ok(create_in(
        &st,
        &auth,
        owner.org_id,
        owner.site.as_ref(),
        &body,
    )
    .await?))
}

/// Create a link for an organisation, and a site when there is one. The
/// caller has already asserted `member` on whichever owns it.
///
/// The one write path for the route and the MCP `create_link` tool, so both
/// validate, store and audit a link the same way.
pub(crate) async fn create_in(
    st: &AppState,
    auth: &AuthUser,
    org_id: Uuid,
    site: Option<&micaforge_store::models::sites::Site>,
    body: &LinkBody,
) -> Result<Value> {
    let chosen = body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let slug = match chosen {
        Some(slug) => slug,
        None => {
            let mut slug = generated_slug();
            while links::slug_taken(&st.pg, &slug).await? {
                slug = generated_slug();
            }
            slug
        }
    };
    let fields = link_fields(body, slug)?;
    check_folder(st, fields.folder_id, org_id).await?;
    let link = links::create(
        &st.pg,
        new_id(),
        org_id,
        site.map(|s| s.id),
        Some(auth.user.id),
        &fields,
    )
    .await
    .map_err(taken)?;
    let after = link_json(st, &link, 0);
    let entry = link_entry(st, "link.created", &link)
        .await
        .after(audited(&after));
    audit::record(st, auth, entry).await;
    Ok(after)
}

/// The audit entry for a change to `link`: the link as the target, on its
/// site when it has one and on its organisation otherwise.
async fn link_entry(st: &AppState, action: &'static str, link: &Link) -> Entry {
    let entry = Entry::new(action).target("link", link.id, &link.slug);
    let site = match link.site_id {
        Some(id) => micaforge_store::models::sites::get(&st.pg, id).await.ok(),
        None => None,
    };
    match site {
        Some(site) => entry.site(&site),
        None => entry.org(link.org_id),
    }
}

/// The fields of a link the audit log keeps: what a person set, not the
/// derived ones (short address, click count).
fn audited(link: &Value) -> Value {
    let keep = [
        "slug",
        "target_url",
        "utm",
        "title",
        "description",
        "image_url",
        "ios_url",
        "android_url",
        "deep_link",
        "expires_at",
        "folder_id",
    ];
    let mut out = Map::new();
    for key in keep {
        if let Some(v) = link.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

/// A link's editable fields, in the shape [`LinkBody`] reads, so the MCP
/// `update_link` tool can lay only the fields it was given over the rest.
pub(crate) fn editable(link: &Link) -> Value {
    json!({
        "slug": link.slug,
        "target_url": link.target_url,
        "folder_id": link.folder_id,
        "utm": link.utm,
        "title": link.title,
        "description": link.description,
        "image_url": link.image_url,
        "ios_url": link.ios_url,
        "android_url": link.android_url,
        "deep_link": link.deep_link,
        "expires_at": link.expires_at,
    })
}

/// A unique violation on `slug`, said as the field it is.
fn taken(e: Error) -> Error {
    match e {
        Error::Conflict(_) => Error::Conflict("that short link is taken".to_string()),
        other => other,
    }
}

/// Load a link and assert the caller's role on its site or organisation.
pub(crate) async fn reachable(st: &AppState, id: Uuid, user: Uuid, needed: &str) -> Result<Link> {
    let link = links::get(&st.pg, id).await?;
    let allowed = match link.site_id {
        Some(site) => authorize_site(&st.pg, site, user, needed).await.map(|_| ()),
        None => authorize_org(&st.pg, link.org_id, user, needed)
            .await
            .map(|_| ()),
    };
    match allowed {
        Ok(()) => Ok(link),
        Err(Error::NotFound(_)) => Err(Error::not_found("link")),
        Err(e) => Err(e),
    }
}

/// `GET /api/links/{id}`
async fn get_link(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let link = reachable(&st, id, auth.user.id, VIEWER).await?;
    let n = clicks_30d(&st, &[link.id])
        .await?
        .first()
        .map(|(_, n)| *n)
        .unwrap_or(0);
    Ok(ok(link_json(&st, &link, n)))
}

/// `PATCH /api/links/{id}`
async fn update_link(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<LinkBody>,
) -> ApiResult<Json<Value>> {
    let link = reachable(&st, id, auth.user.id, MEMBER).await?;
    Ok(ok(update_in(&st, &auth, &link, &body).await?))
}

/// Replace a link's fields. The caller has already asserted `member` on it
/// (through [`reachable`]). The one write path for the route and the MCP
/// `update_link` tool.
pub(crate) async fn update_in(
    st: &AppState,
    auth: &AuthUser,
    link: &Link,
    body: &LinkBody,
) -> Result<Value> {
    let slug = body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| link.slug.clone());
    let fields = link_fields(body, slug)?;
    check_folder(st, fields.folder_id, link.org_id).await?;
    let updated = links::update(&st.pg, link.id, &fields)
        .await
        .map_err(taken)?;
    let entry = link_entry(st, "link.updated", &updated)
        .await
        .before(audited(&link_json(st, link, 0)))
        .after(audited(&link_json(st, &updated, 0)));
    audit::record(st, auth, entry).await;
    // The change is made by now. A ClickHouse that cannot count the clicks
    // must not turn it into a reported failure, which would invite the
    // caller to make it twice; the count reads zero until the next read.
    let n = match clicks_30d(st, &[updated.id]).await {
        Ok(counts) => counts.first().map(|(_, n)| *n).unwrap_or(0),
        Err(e) => {
            tracing::warn!(link = %updated.id, error = %e, "could not count a link's clicks after an edit");
            0
        }
    };
    Ok(link_json(st, &updated, n))
}

/// `DELETE /api/links/{id}`
async fn delete_link(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let link = reachable(&st, id, auth.user.id, MEMBER).await?;
    Ok(ok(delete_in(&st, &auth, &link).await?))
}

/// Delete a link. The caller has already asserted `member` on it. The one
/// write path for the route and the MCP `delete_link` tool. Its clicks stay
/// in ClickHouse, counted under an id nothing points to any more.
pub(crate) async fn delete_in(st: &AppState, auth: &AuthUser, link: &Link) -> Result<Value> {
    links::delete(&st.pg, link.id).await?;
    let entry = link_entry(st, "link.deleted", link)
        .await
        .before(audited(&link_json(st, link, 0)));
    audit::record(st, auth, entry).await;
    Ok(json!({ "deleted": link.id }))
}

/// `GET /api/links/config`
async fn links_config(State(st): State<AppState>, _auth: AuthUser) -> ApiResult<Json<Value>> {
    Ok(ok(json!({
        "links_host": st.cfg.links_host,
        "short_base": short_base(&st),
    })))
}

// ---------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------

/// A folder body.
#[derive(Debug, Deserialize)]
struct FolderBody {
    name: String,
}

fn folder_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(Error::validation("name", "must be 1 to 80 characters"));
    }
    Ok(name.to_string())
}

fn folder_json(f: &links::Folder) -> Value {
    json!({
        "id": f.id,
        "org_id": f.org_id,
        "name": f.name,
        "links": f.links,
        "created_at": f.created_at,
    })
}

/// A folder name that is already used, said as the field it is.
fn name_taken(e: Error) -> Error {
    match e {
        Error::Conflict(_) => Error::validation("name", "is already a folder"),
        other => other,
    }
}

/// `GET /api/link-folders?site=|org=`
async fn list_folders(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(q): Query<OwnerQuery>,
) -> ApiResult<Json<Value>> {
    let owner = owner(&st, auth.user.id, &q, VIEWER).await?;
    let rows = links::list_folders(&st.pg, owner.org_id).await?;
    Ok(ok(rows.iter().map(folder_json).collect::<Vec<_>>()))
}

/// `POST /api/link-folders?site=|org=`
///
/// A folder belongs to the organisation, so making one needs `member` there,
/// whichever door the request came in by.
async fn create_folder(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(q): Query<OwnerQuery>,
    Json(body): Json<FolderBody>,
) -> ApiResult<Json<Value>> {
    let owner = owner(&st, auth.user.id, &q, MEMBER).await?;
    authorize_org(&st.pg, owner.org_id, auth.user.id, MEMBER).await?;
    let folder = links::create_folder(&st.pg, new_id(), owner.org_id, &folder_name(&body.name)?)
        .await
        .map_err(name_taken)?;
    Ok(ok(folder_json(&folder)))
}

/// `PATCH /api/link-folders/{id}`
async fn rename_folder(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<FolderBody>,
) -> ApiResult<Json<Value>> {
    let folder = links::get_folder(&st.pg, id).await?;
    authorize_org(&st.pg, folder.org_id, auth.user.id, MEMBER)
        .await
        .map_err(|e| match e {
            Error::NotFound(_) => Error::not_found("folder"),
            other => other,
        })?;
    let renamed = links::rename_folder(&st.pg, folder.id, &folder_name(&body.name)?)
        .await
        .map_err(name_taken)?;
    Ok(ok(folder_json(&renamed)))
}

/// `DELETE /api/link-folders/{id}`
async fn delete_folder(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let folder = links::get_folder(&st.pg, id).await?;
    authorize_org(&st.pg, folder.org_id, auth.user.id, MEMBER)
        .await
        .map_err(|e| match e {
            Error::NotFound(_) => Error::not_found("folder"),
            other => other,
        })?;
    links::delete_folder(&st.pg, folder.id).await?;
    Ok(ok(json!({ "deleted": folder.id })))
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// `?range=&tz=`
#[derive(Debug, Deserialize)]
struct StatsQuery {
    #[serde(default)]
    range: Option<String>,
    #[serde(default)]
    tz: Option<String>,
}

/// A stats window: its label, its length and its bucket.
fn stats_window(range: Option<&str>) -> Result<(&'static str, Duration, Granularity)> {
    match range.map(str::trim).unwrap_or("7d") {
        "24h" => Ok(("24h", Duration::hours(24), Granularity::Hour)),
        "7d" | "" => Ok(("7d", Duration::days(7), Granularity::Hour)),
        "30d" => Ok(("30d", Duration::days(30), Granularity::Day)),
        "90d" => Ok(("90d", Duration::days(90), Granularity::Day)),
        other => Err(Error::validation(
            "range",
            format!("`{other}` is not a range; expected 24h, 7d, 30d or 90d"),
        )),
    }
}

/// The start of every bucket between `start` and `end`, in epoch ms, in the
/// zone the chart is read in, so a chart can draw an empty bucket as zero
/// rather than join a line across it.
///
/// It walks the window a minute or an hour at a time and maps each instant to
/// the start of its local minute, hour, day, Monday-start week or month: the
/// same buckets [`micaforge_core::time::bucket_expr`] asks ClickHouse for,
/// which is what makes a half-hour zone and a daylight-saving day come out
/// with the keys ClickHouse returns.
pub(crate) fn bucket_starts(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    g: Granularity,
    tz: Tz,
) -> Vec<i64> {
    use chrono::Datelike;

    let step = if g == Granularity::Minute {
        Duration::minutes(1)
    } else {
        Duration::hours(1)
    };
    // Every step through the window, and the end itself: the last bucket is
    // usually partial, and stepping from a start that is not on a boundary
    // would otherwise stop one step short of it.
    let mut instants = Vec::new();
    let mut t = start;
    while t < end {
        instants.push(t);
        t += step;
    }
    instants.push(end);

    let mut out: Vec<i64> = Vec::new();
    for t in instants {
        let local = t.with_timezone(&tz);
        let date = local.date_naive();
        let floored = match g {
            Granularity::Minute => date.and_hms_opt(local.hour(), local.minute(), 0),
            Granularity::Hour => date.and_hms_opt(local.hour(), 0, 0),
            Granularity::Day => date.and_hms_opt(0, 0, 0),
            Granularity::Week => (date
                - Duration::days(i64::from(date.weekday().num_days_from_monday())))
            .and_hms_opt(0, 0, 0),
            Granularity::Month => date.with_day(1).and_then(|d| d.and_hms_opt(0, 0, 0)),
        };
        if let Some(naive) = floored {
            if let Some(at) = tz.from_local_datetime(&naive).earliest() {
                let ms = at.with_timezone(&Utc).timestamp_millis();
                if out.last() != Some(&ms) {
                    out.push(ms);
                }
            }
        }
    }
    out
}

/// One ranked value.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct Ranked {
    value: String,
    clicks: u64,
}

/// One chart bucket.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct ClickBucket {
    t: i64,
    humans: u64,
    bots: u64,
}

/// `GET /api/links/{id}/stats?range=`
async fn link_stats(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<Json<Value>> {
    let link = reachable(&st, id, auth.user.id, VIEWER).await?;
    Ok(ok(stats_value(
        &st,
        &link,
        q.range.as_deref(),
        q.tz.as_deref(),
    )
    .await?))
}

/// One link's clicks over a window, for the route and the MCP tool.
pub(crate) async fn stats_value(
    st: &AppState,
    link: &Link,
    range: Option<&str>,
    tz: Option<&str>,
) -> Result<Value> {
    let (label, span, granularity) = stats_window(range)?;
    let tz: Tz = match tz.map(str::trim).filter(|t| !t.is_empty()) {
        Some(name) => name
            .parse()
            .map_err(|_| Error::validation("tz", "is not an IANA time zone name"))?,
        None => chrono_tz::UTC,
    };
    let now = Utc::now();
    let start = now - span;
    let scope = "link_id = toUUID(?) AND timestamp >= toDateTime64(?, 3, 'UTC')";
    let base = || vec![Param::Str(link.id.to_string()), Param::Str(fmt_ts(start))];

    let series_sql = format!(
        "SELECT {t} AS t, countIf(is_bot = 0) AS humans, countIf(is_bot = 1) AS bots
         FROM link_clicks WHERE {scope} GROUP BY t ORDER BY t",
        t = bucket_millis(granularity, tz, "timestamp"),
    );
    let buckets = fetch_all::<ClickBucket>(&st.ch, "link_series", &series_sql, &base()).await?;
    let series: Vec<Value> = bucket_starts(start, now, granularity, tz)
        .into_iter()
        .map(|t| {
            let b = buckets.iter().find(|b| b.t == t);
            json!({
                "t": t,
                "humans": b.map(|b| b.humans).unwrap_or(0),
                "bots": b.map(|b| b.bots).unwrap_or(0),
            })
        })
        .collect();

    let ranked = |column: &'static str, humans_only: bool, report: &'static str| {
        let params = base();
        let sql = format!(
            "SELECT {column} AS value, count() AS clicks FROM link_clicks
             WHERE {scope}{human} GROUP BY value ORDER BY clicks DESC, value LIMIT 10",
            human = if humans_only { " AND is_bot = 0" } else { "" },
        );
        async move {
            let rows = fetch_all::<Ranked>(&st.ch, report, &sql, &params).await?;
            Ok::<Vec<Value>, Error>(
                rows.into_iter()
                    .map(|r| json!({ "value": r.value, "clicks": r.clicks }))
                    .collect(),
            )
        }
    };
    let (referrers, countries, devices, browsers, targets) = tokio::try_join!(
        ranked("referrer_host", true, "link_referrers"),
        ranked("trimBoth(toString(country))", true, "link_countries"),
        ranked("toString(device_type)", true, "link_devices"),
        ranked("toString(browser)", true, "link_browsers"),
        ranked("toString(target)", false, "link_targets"),
    )?;

    let humans: u64 = buckets.iter().map(|b| b.humans).sum();
    let bots: u64 = buckets.iter().map(|b| b.bots).sum();
    Ok(json!({
        "range": label,
        "total": humans + bots,
        "humans": humans,
        "bots": bots,
        "series": series,
        "referrers": referrers,
        "countries": countries,
        "devices": devices,
        "browsers": browsers,
        "targets": targets,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IPHONE: &str =
        "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 \
        (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";
    const ANDROID: &str = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 \
        (KHTML, like Gecko) Chrome/124.0.0.0 Mobile Safari/537.36";
    const DESKTOP: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
        (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
    const SLACK: &str = "Slackbot-LinkExpanding 1.0 (+https://api.slack.com/robots)";

    fn link() -> Link {
        let now = Utc::now();
        Link {
            id: Uuid::nil(),
            org_id: Uuid::nil(),
            site_id: None,
            folder_id: None,
            slug: "launch".into(),
            target_url: "https://example.com/pricing?plan=pro".into(),
            utm: json!({ "source": "newsletter", "campaign": "launch week" }),
            title: Some("Pricing".into()),
            description: Some("Plans & prices".into()),
            image_url: None,
            ios_url: None,
            android_url: None,
            deep_link: None,
            expires_at: None,
            created_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn decide_for(link: &Link, user_agent: &str) -> Decision {
        let client = ua::parse(user_agent, None, None, None);
        decide(link, Utc::now(), user_agent, &client)
    }

    #[test]
    fn the_utm_parameters_are_appended_and_replace_their_namesakes() {
        assert_eq!(
            final_url(
                "https://example.com/p?plan=pro&utm_source=old",
                &json!({ "source": "newsletter", "campaign": "launch week", "term": " " })
            ),
            "https://example.com/p?plan=pro&utm_source=newsletter&utm_campaign=launch+week"
        );
        assert_eq!(
            final_url("https://example.com/p#top", &json!({})),
            "https://example.com/p#top"
        );
    }

    #[test]
    fn a_desktop_gets_the_web_target_with_its_utm() {
        let d = decide_for(&link(), DESKTOP);
        assert_eq!(
            d,
            Decision::Redirect {
                to: "https://example.com/pricing?plan=pro&utm_source=newsletter&utm_campaign=launch+week"
                    .into(),
                target: "web"
            }
        );
    }

    #[test]
    fn phones_get_their_own_target_when_the_link_has_one() {
        let mut l = link();
        l.ios_url = Some("https://apps.apple.com/app/id1".into());
        l.android_url = Some("https://play.google.com/store/apps/details?id=x".into());
        assert_eq!(decide_for(&l, IPHONE).target(), "ios");
        assert_eq!(decide_for(&l, ANDROID).target(), "android");
        assert_eq!(decide_for(&l, DESKTOP).target(), "web");
        // A phone with no target of its own gets the web one.
        l.android_url = None;
        assert_eq!(decide_for(&l, ANDROID).target(), "web");
    }

    #[test]
    fn a_phone_tries_the_app_first_and_falls_back() {
        let mut l = link();
        l.deep_link = Some("myapp://pricing".into());
        l.ios_url = Some("https://apps.apple.com/app/id1".into());
        match decide_for(&l, IPHONE) {
            Decision::DeepLink { app, fallback } => {
                assert_eq!(app, "myapp://pricing");
                assert_eq!(fallback, "https://apps.apple.com/app/id1");
            }
            other => panic!("expected a deep link, got {other:?}"),
        }
        match decide_for(&l, ANDROID) {
            Decision::DeepLink { fallback, .. } => {
                assert!(fallback.starts_with("https://example.com/pricing"))
            }
            other => panic!("expected a deep link, got {other:?}"),
        }
        // A desktop never tries the app.
        assert_eq!(decide_for(&l, DESKTOP).target(), "web");
    }

    #[test]
    fn preview_bots_get_the_card_and_expiry_beats_everything() {
        assert_eq!(
            decide_for(&link(), SLACK),
            Decision::Preview { bot: "Slackbot" }
        );
        for (ua, name) in [
            ("Twitterbot/1.0", "Twitterbot"),
            ("facebookexternalhit/1.1", "facebookexternalhit"),
            ("Mozilla/5.0 (compatible; Discordbot/2.0)", "Discordbot"),
            ("LinkedInBot/1.0", "LinkedInBot"),
            ("TelegramBot (like TwitterBot)", "TelegramBot"),
            ("WhatsApp/2.23.20.0", "WhatsApp"),
        ] {
            assert_eq!(preview_bot(ua), Some(name), "{ua}");
        }
        assert_eq!(preview_bot(DESKTOP), None);

        let mut expired = link();
        expired.expires_at = Some(Utc::now() - Duration::minutes(1));
        assert_eq!(decide_for(&expired, SLACK), Decision::Expired);
        assert_eq!(decide_for(&expired, DESKTOP), Decision::Expired);
        assert_eq!(Decision::Expired.target(), "expired");
    }

    #[test]
    fn the_preview_page_escapes_everything_it_prints() {
        let mut l = link();
        l.title = Some("<script>alert(1)</script>".into());
        l.image_url = Some("https://cdn.example.com/card.png?a=1&b=2".into());
        let html = preview_page(&l, "https://go.example.com/launch", "https://example.com/");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(
            html.contains("og:image\" content=\"https://cdn.example.com/card.png?a=1&amp;b=2\"")
        );
        assert!(html.contains("summary_large_image"));
        assert!(html.contains("<meta property=\"og:title\""));
        assert!(html.contains("<meta name=\"twitter:card\""));
        assert!(!html.contains("<meta name=\"og:"));
        assert!(html.contains("og:url\" content=\"https://go.example.com/launch\""));
    }

    #[test]
    fn the_deep_link_page_cannot_be_broken_out_of() {
        let html = deep_link_page(
            "myapp://x</script><script>alert(1)",
            "https://example.com/",
            "n0nce",
        );
        assert_eq!(html.matches("<script").count(), 1, "{html}");
        assert!(html.contains("nonce=\"n0nce\""));
    }

    #[test]
    fn slugs_and_link_fields_are_validated() {
        assert!(check_slug("Launch_2026-q4").is_ok());
        for bad in ["", "a b", "a/b", "api", "API", "status", &"x".repeat(65)] {
            assert!(check_slug(bad).is_err(), "{bad:?}");
        }
        let slug = generated_slug();
        assert_eq!(slug.len(), GENERATED_SLUG);
        assert!(slug.chars().all(|c| c.is_ascii_alphanumeric()));

        let body = |v: Value| serde_json::from_value::<LinkBody>(v).unwrap();
        let ok = link_fields(
            &body(json!({
                "target_url": "https://example.com/",
                "utm": { "source": "x" },
                "ios_url": "itms-apps://apps.apple.com/app/id1",
                "deep_link": "myapp://open"
            })),
            "abc".into(),
        )
        .unwrap();
        assert_eq!(ok.utm, json!({ "source": "x" }));

        for (v, field) in [
            (json!({ "target_url": "javascript:alert(1)" }), "target_url"),
            (json!({ "target_url": "/relative" }), "target_url"),
            (
                json!({ "target_url": "https://e.com", "deep_link": "https://e.com" }),
                "deep_link",
            ),
            (
                json!({ "target_url": "https://e.com", "deep_link": "javascript:alert(1)" }),
                "deep_link",
            ),
            (
                json!({ "target_url": "https://e.com", "image_url": "http://e.com/a.png" }),
                "image_url",
            ),
            (
                json!({ "target_url": "https://e.com", "utm": { "id": "x" } }),
                "utm",
            ),
            (
                json!({ "target_url": "https://e.com", "android_url": "intent://x" }),
                "android_url",
            ),
        ] {
            let e = link_fields(&body(v.clone()), "abc".into()).unwrap_err();
            assert_eq!(e.field(), Some(field), "{v}");
        }
    }

    #[test]
    fn a_qr_code_is_an_svg_of_the_requested_size() {
        let svg = qr("https://go.example.com/launch", 256).unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.ends_with("</svg>"));
        let width: u32 = svg
            .split("width=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .and_then(|w| w.parse().ok())
            .unwrap();
        assert!(width >= 256, "{width}");
        // Black modules on white, and the same input draws the same code.
        assert!(svg.contains("fill=\"#000000\""));
        assert_eq!(svg, qr("https://go.example.com/launch", 256).unwrap());
        assert_ne!(svg, qr("https://go.example.com/other", 256).unwrap());
    }

    #[test]
    fn the_host_comparison_ignores_case_and_port() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("Go.Example.com:443"));
        let uri: axum::http::Uri = "/launch".parse().unwrap();
        assert_eq!(
            request_host(&headers, &uri).as_deref(),
            Some("go.example.com")
        );
    }

    #[test]
    fn buckets_follow_the_readers_zone() {
        let start = Utc.with_ymd_and_hms(2026, 3, 28, 12, 0, 0).unwrap();
        let end = start + Duration::days(3);
        let lisbon: Tz = "Europe/Lisbon".parse().unwrap();
        let days = bucket_starts(start, end, Granularity::Day, lisbon);
        assert_eq!(days.len(), 4, "the 28th to the 31st");
        // The spring-forward day is 23 hours long.
        assert_eq!((days[2] - days[1]) / 3_600_000, 23);
        // The last, partial bucket is there even when the window does not
        // start on a boundary.
        let late = Utc.with_ymd_and_hms(2026, 10, 8, 22, 8, 30).unwrap();
        let hours = bucket_starts(
            late - Duration::hours(24),
            late,
            Granularity::Hour,
            chrono_tz::UTC,
        );
        assert_eq!(hours.len(), 25);
        assert_eq!(
            hours.last().copied(),
            Some(
                Utc.with_ymd_and_hms(2026, 10, 8, 22, 0, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
        let kolkata: Tz = "Asia/Kolkata".parse().unwrap();
        let hours = bucket_starts(
            start,
            start + Duration::hours(2),
            Granularity::Hour,
            kolkata,
        );
        assert!(
            hours.iter().all(|ms| (ms / 60_000) % 60 == 30),
            "half-hour zone"
        );
    }
}
