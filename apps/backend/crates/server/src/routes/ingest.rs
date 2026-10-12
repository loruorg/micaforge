//! Collection: the routes a tracker, a pixel and a log shipper write into.
//!
//! This is the hot path and it is also the product's integrity boundary, so the
//! rules it follows are stricter than anywhere else in the server.
//!
//! **It never breaks a customer's page.** Every browser-facing handler answers
//! `204 No Content` with an empty body. A truncated beacon, a payload with a
//! field of the wrong type, a Redis that is down: none of them produce a status
//! a browser console will complain about, because a page owner debugging their
//! own site must never be sent looking at our error instead of theirs. The one
//! exception is a domain that does not belong to the site, which is answered
//! `403` on purpose — see [`check_origin`].
//!
//! **It never awaits a store it does not have to.** ClickHouse is written
//! through [`micaforge_store::EventWriter`], which takes the row on a channel
//! and returns. Postgres is read through a process-local cache with a short TTL,
//! so the steady state of a busy site is zero database round trips per hit, and
//! a Postgres outage is ridden out on the cached answers rather than turned into
//! a collection outage. Only Redis is on the path of every request, the salt and
//! limiter reads have short budgets of their own, and the whole request has the
//! budget `middleware::timeout` gives it.
//!
//! **It never weakens the visitor digest.** When the daily salt cannot be read
//! from Redis, the hit is hashed with a random salt held in process memory for
//! the current UTC day ([`daily_salt`]), never with an empty or a fixed one. An
//! unsalted digest of an IPv4 address and a user agent is reversible by brute
//! force and never rotates, which would make every row written during an outage
//! a permanent identifier.
//!
//! **It never persists an address.** The client IP is read, resolved to a coarse
//! location, folded into the visitor digest, and dropped. `docs/CONTRACT.md` §7.1
//! is not a policy this module implements; it is the shape of the code.
//!
//! ## The three-way split
//!
//! [`micaforge_agents::classify`] decides which of the three tables a hit lands
//! in, and the browser path narrows that decision further: on `/api/track`, a
//! *named* agent is written to `bot_events`, not `agent_events`. A crawler does
//! not run JavaScript, so a beacon carrying GPTBot's user agent is either a
//! spoof or a headless browser wearing a costume. Filing it as a genuine agent
//! fetch would put invented crawl volume into the Citation Gap, which is the one
//! number this product cannot afford to inflate. Real agent rows come from
//! [`edge`], where a server observed the fetch.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use axum::body::to_bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value as Json};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use url::Url;
use uuid::Uuid;

use micaforge_agents::verify::normalize_ip;
use micaforge_agents::{catalog::AgentDef, policy, robots, Classification, HeaderView, LlmsTxt};
use micaforge_core::{crypto, EventKind};
use micaforge_enrich::{geo::Location, identity, referrer, ua, url_norm};
use micaforge_store::ch::{
    country_code, decimal64_4, AgentEventRow, BotEventRow, ErrorRow, EventRow,
};
use micaforge_store::models::{catalog as catalog_model, sites as sites_model};

use crate::client_ip::address_tag;
use crate::ingest_guard::{self, Blocked, Guard};
use crate::middleware::{LogGate, LIMITER_BUDGET};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Largest accepted browser beacon.
///
/// `sendBeacon` itself is capped at 64 KB by the platform, so anything larger
/// did not come from the tracker.
const MAX_TRACK_BODY: usize = 64 * 1024;

/// Largest accepted server-side batch. Fifty rows of a hundred columns, with
/// room for long user-agent strings, fits inside this several times over.
const MAX_EDGE_BODY: usize = 2 * 1024 * 1024;

/// Most payloads accepted from one `POST /api/track/batch` call.
const MAX_BATCH_ROWS: usize = 200;

/// Most rows accepted from one `POST /api/log/edge` call, per array.
const MAX_EDGE_ROWS: usize = 500;

/// Hits per minute per (site, address). Deliberately generous: a documentation
/// site behind one corporate NAT is a legitimate source of hundreds of hits a
/// minute, and throttling it would delete real readers. This exists to bound a
/// broken loop, not to police traffic.
const RATE_LIMIT_PER_MIN: u32 = 600;

/// Signed edge batches per minute per site. Higher than the per-address limit
/// because one site's shippers (every Worker isolate, every server) share it.
const EDGE_SITE_LIMIT_PER_MIN: u32 = 6_000;

/// The rate-limit window.
const RATE_WINDOW_SECS: u32 = 60;

/// How long a resolved site is held in process before it is read again.
///
/// A minute is short enough that adding a domain in settings takes effect while
/// the operator is still on the page, and long enough that a busy site costs no
/// Postgres round trips at all.
const SITE_CACHE_TTL: Duration = Duration::from_secs(60);

/// How long a resolved *absence* is held: an id Postgres says has no site.
///
/// Shorter than [`SITE_CACHE_TTL`] so a site created a moment ago starts
/// collecting quickly, and long enough that a stranger probing ids costs one
/// query per id per half minute rather than one per request.
const NEGATIVE_TTL: Duration = Duration::from_secs(30);

/// Most site ids the cache holds, positive and negative together.
///
/// Site ids are 16-bit, so the key space is bounded anyway; this keeps the map
/// small on an install with a few sites while a probe walks the rest.
const SITE_CACHE_MAX: usize = 8192;

/// How long an expired entry keeps being served while one request refreshes it.
const REFRESH_GRACE: Duration = Duration::from_secs(5);

/// How long one site lookup may wait on Postgres, pool checkout included.
///
/// A healthy lookup is a millisecond. Waiting the pool's full five-second
/// checkout timeout on every hit during an outage is what turned a database
/// blip into a collection outage.
const SITE_LOOKUP_BUDGET: Duration = Duration::from_secs(1);

/// How long a hit waits for the daily salt before using the in-memory one.
const SALT_BUDGET: Duration = Duration::from_millis(500);

/// Most in-memory fallback salts held at once. One per site per UTC day, kept
/// for two days; past this, a hit gets a salt of its own that is never stored,
/// which costs visitor continuity and never costs privacy.
const MAX_FALLBACK_SALTS: usize = 8192;

/// Most edge signatures remembered for replay refusal at once.
const MAX_SEEN_SIGNATURES: usize = 65_536;

/// How long a site's parsed `robots.txt` and `llms.txt` are held in process.
///
/// Longer than the site cache because the policy files are refetched by a job on
/// the order of hours, so a fresher copy is not available anyway.
const POLICY_CACHE_TTL: Duration = Duration::from_secs(300);

/// Replay window for an edge signature, in seconds. `docs/CONTRACT.md` §3.3.
const SIGNATURE_WINDOW_SECS: i64 = 300;

/// Longest string kept for a free-text column.
const MAX_TEXT: usize = 512;

/// Longest serialised `props` document kept on a row.
const MAX_PROPS: usize = 8 * 1024;

/// A 1×1 transparent GIF, 43 bytes. The no-JS fallback's response body.
const PIXEL_GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every collection route.
///
/// CORS is answered inside these handlers rather than by a layer, because the
/// ingest surface is the only part of the API that is deliberately open to every
/// origin (`docs/CONTRACT.md` §3.3) and the rest of the API must not be. Do not
/// wrap this router in a second CORS layer: two `access-control-allow-origin`
/// headers on one response is a CORS failure, not a permissive one.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/track", post(track).options(preflight))
        .route("/api/track/batch", post(track_batch).options(preflight))
        .route("/api/track/pixel.gif", get(pixel).options(preflight))
        .route("/api/log/edge", post(edge).options(preflight))
}

/// The CORS preflight answer for every ingest route.
///
/// The tracker's normal transport is a `text/plain` beacon, which is a simple
/// request and never preflights. This exists for the paths that do: a
/// `fetch` with `content-type: application/json` from a server SDK running in a
/// browser-like runtime, and any integration that adds a header of its own.
async fn preflight(headers: HeaderMap) -> Response {
    let requested = header(&headers, "access-control-request-headers")
        .map(|v| clamp(v, 512))
        .unwrap_or_else(|| "content-type,x-micaforge-signature".to_string());

    let mut res = StatusCode::NO_CONTENT.into_response();
    let h = res.headers_mut();
    put(h, "access-control-allow-origin", "*");
    put(h, "access-control-allow-methods", "POST, GET, OPTIONS");
    put(h, "access-control-allow-headers", &requested);
    put(h, "access-control-max-age", "86400");
    put(h, "vary", "access-control-request-headers");
    res
}

// ---------------------------------------------------------------------------
// The browser path
// ---------------------------------------------------------------------------

/// `POST /api/track` — one hit from the browser tracker.
///
/// Answers `204` for everything except a hostname that is not one of the site's
/// own, which is `403`. Nothing else about the request can produce a visible
/// failure: `docs/CONTRACT.md` §4 makes the tracker responsible for never
/// throwing into the host page, and a collector that returns `500` on a
/// malformed beacon would make that promise impossible to keep.
async fn track(State(state): State<AppState>, req: Request) -> Response {
    let (ctx, body) = match split_request(req, MAX_TRACK_BODY).await {
        Some(pair) => pair,
        None => return no_content(),
    };

    let payload: Payload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(_) => return no_content(),
    };

    match ingest_one(&state, payload, &ctx).await {
        Verdict::Denied => forbidden(),
        _ => no_content(),
    }
}

/// `POST /api/track/batch` — an array of the same payloads.
///
/// The shape the server SDKs and the offline importers send. Both a bare array
/// and `{ "events": [...] }` are accepted, because two reasonable clients
/// produced both and rejecting one of them would only cost a customer their
/// data.
async fn track_batch(State(state): State<AppState>, req: Request) -> Response {
    let (ctx, body) = match split_request(req, MAX_EDGE_BODY).await {
        Some(pair) => pair,
        None => return no_content(),
    };

    let payloads: Vec<Payload> = match serde_json::from_slice::<Json>(&body) {
        Ok(Json::Array(items)) => decode_batch(items),
        Ok(Json::Object(mut map)) => match map.remove("events") {
            Some(Json::Array(items)) => decode_batch(items),
            _ => return no_content(),
        },
        _ => return no_content(),
    };

    let mut accepted = 0usize;
    let mut denied = 0usize;
    for payload in payloads.into_iter().take(MAX_BATCH_ROWS) {
        match ingest_one(&state, payload, &ctx).await {
            Verdict::Denied => denied += 1,
            _ => accepted += 1,
        }
    }

    // A batch that was rejected in full is a misconfigured integration and is
    // worth saying so; a mixed batch is a page with a stray hostname in it and
    // is not worth failing the whole call over.
    if accepted == 0 && denied > 0 {
        forbidden()
    } else {
        no_content()
    }
}

/// `GET /api/track/pixel.gif` — the no-JavaScript fallback.
///
/// The tracker falls back to this when `sendBeacon` and `fetch` both fail, and
/// a site with no JavaScript at all can embed it directly. The payload is the
/// same one from `docs/CONTRACT.md` §4, query-string encoded, with `p` and `e`
/// carrying a JSON object as a string.
///
/// It always answers with the image, even for a hit that was dropped. An
/// `<img>` that 404s is a broken-image icon on someone's page.
async fn pixel(State(state): State<AppState>, req: Request) -> Response {
    let query = req.uri().query().unwrap_or_default().to_string();
    let (ctx, _) = match split_request(req, 0).await {
        Some(pair) => pair,
        None => return gif(),
    };

    if let Some(payload) = payload_from_query(&query) {
        let _ = ingest_one(&state, payload, &ctx).await;
    }
    gif()
}

// ---------------------------------------------------------------------------
// One hit
// ---------------------------------------------------------------------------

/// What happened to one payload.
enum Verdict {
    /// Written, or deliberately dropped (excluded path, rate limit, opt-out).
    /// The caller cannot tell these apart and must not be able to.
    Accepted,
    /// The hostname is not one of this site's, or the site does not exist. One
    /// verdict for both, so the answer says nothing about which.
    Denied,
}

/// Resolve, authorise, classify, enrich and queue one browser payload.
async fn ingest_one(state: &AppState, p: Payload, ctx: &RequestCtx) -> Verdict {
    let site_id = match u16::try_from(p.s) {
        Ok(id) if id > 0 => id,
        _ => return Verdict::Denied,
    };

    let site = match resolve_site(state, site_id).await {
        SiteLookup::Found(site) => site,
        SiteLookup::Unknown => return Verdict::Denied,
        // Postgres cannot be asked and this process has never seen the site.
        // The hit is lost either way; a `403` would tell a page owner their
        // install is misconfigured when it is not.
        SiteLookup::Unavailable => return Verdict::Accepted,
    };

    // The page URL is the authority on where the hit happened; the `Origin`
    // header is the fallback for a payload that omitted it.
    let url = p.u.as_deref().and_then(parse_page_url);
    let hostname = url
        .as_ref()
        .and_then(|u| u.host_str().map(normalise_host))
        .or_else(|| ctx.origin_host.clone());

    let Some(hostname) = hostname else {
        return Verdict::Denied;
    };
    if !check_origin(&site, &hostname) {
        ingest_guard::count_blocked(state, site_id, Blocked::Hostname);
        return Verdict::Denied;
    }

    // The site's address lists. A blocked hit is answered as accepted: the
    // page is never broken and the sender learns nothing from the answer.
    if !ingest_guard::admit(state, site_id, &site.guard, Some(ctx.ip)) {
        return Verdict::Accepted;
    }

    // One Redis read for the salt, reused for the rate-limit key, the address
    // digest and the visitor id. Fetching it three times would triple the only
    // round trip this path cannot avoid.
    let salt = daily_salt(state, &site).await;
    let ip_hash = ip_digest(&salt, site_id, &ctx.ip_text);

    // Rate limiting runs after the site is known so the key is per site, and it
    // is keyed on the salted digest rather than the address so that the limiter
    // holds no address either.
    if !allow(state, &format!("ingest:{site_id}:{ip_hash}")).await {
        return Verdict::Accepted;
    }

    if site.respect_dnt && ctx.do_not_track {
        return Verdict::Accepted;
    }

    let Some(url) = url else {
        return Verdict::Accepted;
    };
    let pathname = url_norm::normalise_path(url.path(), site.trailing_slash);
    if site.excluded.iter().any(|g| glob_match(g, &pathname)) {
        return Verdict::Accepted;
    }

    let classification =
        micaforge_agents::classify(&ctx.user_agent, &ctx.header_view(), Some(&ctx.ip_text));

    // A beacon is JavaScript, and no crawler runs JavaScript. Named or not, a
    // non-human hit on this route is a spoof or a headless run, so it is
    // recorded as suspected rather than as a genuine agent fetch.
    if !classification.is_human() {
        let location = locate(state, ctx);
        let realtime_member = ip_hash.clone();
        let mut row = BotEventRow {
            site_id,
            timestamp: Utc::now(),
            user_agent: clamp(&ctx.user_agent, MAX_TEXT),
            method: ctx.method.clone(),
            hostname: clamp(&hostname, MAX_TEXT),
            pathname,
            querystring: clamp(url.query().unwrap_or_default(), MAX_TEXT),
            status: 204,
            ip_hash,
            ..BotEventRow::default()
        };
        apply_location(&location, &mut row.asn, &mut row.asn_org, &mut row.country);
        match &classification {
            Classification::Agent(def) => {
                row.agent_id = def.slug.to_string();
                row.reasons = format!("js_beacon_agent_ua:{}", def.slug);
                row.score = 100;
            }
            Classification::SuspectedBot(signals) => {
                row.reasons = clamp(&signals.reasons_joined(), MAX_TEXT);
                row.score = signals.score;
            }
            Classification::Human => {}
        }
        state.writer.push_bot(row);
        let _ = state
            .kv
            .bump_realtime(site_id, &realtime_member, true)
            .await;
        return Verdict::Accepted;
    }

    let Some(kind) = p.kind() else {
        // An unknown `t` is dropped rather than counted as a pageview. A typo in
        // an integration must not silently inflate the headline number.
        return Verdict::Accepted;
    };

    let visitor = identity::visitor_id(&salt, site_id, &ctx.ip_text, &ctx.user_agent);
    let (session, entry_path) = session_for(state, site_id, &visitor, &pathname).await;

    let mut row = build_event_row(
        &site,
        &p,
        kind,
        &url,
        &hostname,
        &pathname,
        &ctx.user_agent,
        ctx.sec_ch_ua.as_deref(),
        ctx.sec_ch_ua_platform.as_deref(),
        ctx.sec_ch_ua_mobile.as_deref(),
        &locate(state, ctx),
    );
    row.visitor_id = visitor.clone();
    row.session_id = session;
    row.entry_path = entry_path;

    if kind == EventKind::Error {
        mirror_error(state, &row);
    }

    state.writer.push_event(row);
    let _ = state.kv.bump_realtime(site_id, &visitor, false).await;
    Verdict::Accepted
}

/// Build the `events` row from a payload and its enrichment.
///
/// Split out because the browser path and the edge path fill exactly the same
/// columns from different sources, and two copies of a sixty-field constructor
/// is two places for a column to be forgotten.
#[allow(clippy::too_many_arguments)]
fn build_event_row(
    site: &SiteCtx,
    p: &Payload,
    kind: EventKind,
    url: &Url,
    hostname: &str,
    pathname: &str,
    user_agent: &str,
    sec_ch_ua: Option<&str>,
    sec_ch_ua_platform: Option<&str>,
    sec_ch_ua_mobile: Option<&str>,
    location: &Location,
) -> EventRow {
    let client = ua::parse(user_agent, sec_ch_ua, sec_ch_ua_platform, sec_ch_ua_mobile);
    let (querystring, url_params) = referrer::strip_and_collect_params(url);
    let attribution = referrer::attribute(p.r.as_deref(), url, &site.domain, &answer_engine_lookup);
    let (props, mut revenue, mut currency) = props_of(p);
    // A trait is a fact about a person, not a sale: a `revenue` trait on an
    // identify call (a customer's company size, say) must never reach the
    // revenue column every report sums.
    if kind == EventKind::Identify {
        revenue = None;
        currency.clear();
    }
    let vitals = p.e.clone().unwrap_or_default();

    let mut row = EventRow {
        site_id: site.site_id,
        timestamp: Utc::now(),
        event_id: micaforge_core::new_id(),
        identified_id: clamp_opt(&p.id, 128),
        kind: kind.as_str().to_string(),
        event_name: clamp_opt(&p.n, 200),
        hostname: clamp(hostname, MAX_TEXT),
        pathname: pathname.to_string(),
        page_title: clamp_opt(&p.title, MAX_TEXT),
        querystring,
        url_params,
        hash: clamp(url.fragment().unwrap_or_default(), MAX_TEXT),
        referrer: clamp_opt(&p.r, MAX_TEXT),
        referrer_host: attribution.referrer_host,
        channel: attribution.channel.as_str().to_string(),
        source: attribution.source,
        browser: client.browser,
        browser_version: client.browser_version,
        os: client.os,
        os_version: client.os_version,
        device_type: client.device_type,
        screen_w: small(p.w),
        screen_h: small(p.h),
        viewport_w: small(p.vw),
        viewport_h: small(p.vh),
        language: clamp_opt(&p.l, 32),
        timezone: clamp_opt(&p.tz, 64),
        exit_path: pathname.to_string(),
        duration_ms: p.d.unwrap_or(0).min(u64::from(u32::MAX)) as u32,
        scroll_pct: p.sc.unwrap_or(0).min(100) as u8,
        props,
        revenue,
        currency,
        experiment: clamp_opt(&p.experiment, 64),
        variant: clamp_opt(&p.variant, 64),
        flags: flags_of(p.flags.as_ref()),
        lcp: vitals.lcp,
        cls: vitals.cls,
        inp: vitals.inp,
        fcp: vitals.fcp,
        ttfb: vitals.ttfb,
        sdk_version: clamp_opt(&p.v, 32),
        sample_rate: sample_rate_of(p.sr),
        ..EventRow::default()
    };

    let utm = referrer::parse_utm(url);
    row.utm_source = utm.source;
    row.utm_medium = utm.medium;
    row.utm_campaign = utm.campaign;
    row.utm_term = utm.term;
    row.utm_content = utm.content;
    row.click_id = utm.click_id;

    row.country = country_code(&location.country);
    row.region = clamp(&location.region, MAX_TEXT);
    row.city = clamp(&location.city, MAX_TEXT);
    row.lat = location.lat;
    row.lon = location.lon;
    row.asn = location.asn;
    row.asn_org = clamp(&location.asn_org, MAX_TEXT);
    row.is_datacenter = u8::from(location.is_datacenter);

    // `timestamp` stays the receive time set above. A browser payload's own
    // clock is never read: the browser routes are unauthenticated, and honouring
    // it would let anyone who can read a site id backdate rows into that site's
    // history. Only the signed edge path sets an event time of its own.
    row
}

/// Mirror a client-side error into the `errors` table.
///
/// The `events` row still exists — an error is a hit and belongs in the funnel
/// that led to it — but the error report reads its own table, because grouping
/// by a fingerprint computed at ingest is what keeps a group stable when the
/// stack text changes between releases.
fn mirror_error(state: &AppState, row: &EventRow) {
    let props: Json = serde_json::from_str(&row.props).unwrap_or(Json::Null);
    let get = |key: &str| -> String {
        props
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| clamp(s, MAX_TEXT))
            .unwrap_or_default()
    };
    let num = |key: &str| -> u32 {
        props
            .get(key)
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .min(u64::from(u32::MAX)) as u32
    };

    let name = if row.event_name.is_empty() {
        get("name")
    } else {
        row.event_name.clone()
    };
    let message = get("message");
    let stack = props
        .get("stack")
        .and_then(|v| v.as_str())
        .map(|s| clamp(s, 4096))
        .unwrap_or_default();
    let filename = get("filename");

    state.writer.push_error(ErrorRow {
        site_id: row.site_id,
        timestamp: row.timestamp,
        session_id: row.session_id.clone(),
        visitor_id: row.visitor_id.clone(),
        fingerprint: error_fingerprint(&name, &message, &filename),
        name,
        message,
        stack,
        filename,
        lineno: num("lineno"),
        colno: num("colno"),
        pathname: row.pathname.clone(),
        browser: row.browser.clone(),
        os: row.os.clone(),
        // The site's own release (`data-release` on the script tag, or a
        // `release` property from a server SDK), and nothing when it gave none.
        // The tracker's version is not a release of the site: grouping a site's
        // errors under `0.1.0` would pin every regression on the wrong deploy.
        release: clamp(&get("release"), 64),
        // The error plugin marks what reached `window.onerror` or an unhandled
        // rejection as `handled: false`; anything a site reports itself through
        // the same route is handled unless it says otherwise.
        handled: u8::from(
            props
                .get("handled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        ),
    });
}

/// A grouping key that survives a redeploy.
///
/// Built from the error's identity (constructor, message, source file) and never
/// from line numbers or the stack body, both of which change on every bundle
/// hash without the bug changing at all.
fn error_fingerprint(name: &str, message: &str, filename: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [name, message, filename] {
        hasher.update(part.as_bytes());
        hasher.update(&[0x1f]);
    }
    hex::encode(&hasher.finalize().as_bytes()[..8])
}

// ---------------------------------------------------------------------------
// The server-side path
// ---------------------------------------------------------------------------

/// `POST /api/log/edge` — hits a server observed, signed with the site's key.
///
/// This is where genuine `agent_events` rows come from. An AI crawler never runs
/// the tracker, so the only witness to its fetch is the server that answered it,
/// and the only way to trust what that witness says is to make it prove it holds
/// the site's ingest key.
///
/// The signature is `x-micaforge-signature: t=<unix>,v1=<hex>`, where `v1` is
/// HMAC-SHA256, keyed with the site's ingest key, over the decimal `t`, one
/// `.`, and then the **raw bytes of the body**: `"{t}.{body}"`. The timestamp
/// is inside the signed material, so a captured request cannot be replayed with
/// a fresh `t`; the accepted window is five minutes either side of now; and an
/// exact repeat of a signature inside that window is refused with `409`
/// ([`first_use`]). `docs/CONTRACT.md` §3.3.
async fn edge(State(state): State<AppState>, req: Request) -> Response {
    let signature = header(req.headers(), "x-micaforge-signature").map(|s| s.to_string());
    let (ctx, body) = match split_request(req, MAX_EDGE_BODY).await {
        Some(pair) => pair,
        None => {
            return api_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "bad_request",
                "body too large",
            )
        }
    };

    // The cheapest rejection comes first. An unsigned request is refused before
    // anything touches Postgres, so a flood on this open endpoint cannot be
    // turned into database load.
    let Some(signature) = signature else {
        return unauthorized();
    };

    // Only failed signatures are charged to the caller's address, so honest
    // shippers that share egress addresses (Cloudflare Workers) never throttle
    // each other here. An address over its failure budget is refused before
    // anything touches Postgres. The key holds the keyed address tag, which
    // nothing can turn back into the address.
    let failures = format!("edge-fail:{}", address_tag(ctx.ip));
    if exhausted(&state, &failures).await {
        return too_many("too many unsigned or unrecognised edge batches");
    }

    let Ok(payload) = serde_json::from_slice::<EdgePayload>(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "body is not a valid edge payload",
        );
    };

    let Ok(site_id) = u16::try_from(payload.site_id) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "site_id is out of range",
        );
    };

    // Read the row fresh rather than from the sixty-second site cache. A key the
    // operator has just rotated must stop verifying immediately — that is what a
    // rotation button means — and the shipper that just picked up the new one
    // has to start working in the same moment.
    let row = sites_model::get_by_site_id(&state.pg, site_id).await.ok();
    let key = match row.as_ref() {
        Some(row) => stored_ingest_key(&state, row).await,
        None => None,
    };

    // The HMAC runs either way: an unknown site verifies against an empty key,
    // which cannot match but costs the same work, so an absent site and a wrong
    // key give the same answer after the same time. Same reason `/api/track`
    // gives one verdict for both.
    let now = Utc::now().timestamp();
    let signed = verify_signature(key.as_deref().unwrap_or(""), &body, &signature, now)
        .filter(|_| key.is_some());
    let (Some(row), Some(signed)) = (row, signed) else {
        let _ = allow(&state, &failures).await;
        return unauthorized();
    };
    let site = Arc::new(SiteCtx::from_row(&row));

    // Keyed on the site alone: the signature proved who is sending, and a
    // shipper's address says nothing useful once it has.
    if !allow_n(
        &state,
        &format!("edge-site:{site_id}"),
        EDGE_SITE_LIMIT_PER_MIN,
    )
    .await
    {
        return too_many("too many edge batches for this site");
    }
    let salt = daily_salt(&state, &site).await;

    // Claimed only now, after the limiter, so a batch refused with `429` can be
    // retried under the same signature.
    if !first_use(seen_signatures(), &signed, now) {
        return api_error(
            StatusCode::CONFLICT,
            "conflict",
            "this signed batch was already received; sign each send afresh",
        );
    }

    let policy = resolve_policy(&state, &site).await;
    let mut accepted = 0usize;
    let mut rejected = 0usize;

    for hit in payload.agent_events.into_iter().take(MAX_EDGE_ROWS) {
        match ingest_agent_hit(&state, &site, hit, policy.as_deref(), &salt).await {
            true => accepted += 1,
            false => rejected += 1,
        }
    }
    for event in payload.events.into_iter().take(MAX_EDGE_ROWS) {
        match ingest_edge_event(&state, &site, event, policy.as_deref(), &salt).await {
            true => accepted += 1,
            false => rejected += 1,
        }
    }

    // `202`, not `403`, even when nothing was taken: the signature already
    // proved the caller owns this site, so a rejected row is a misconfigured
    // `hostname` or an excluded path rather than an intrusion. The counts are in
    // the body so an operator wiring up a shipper can see which.
    let mut res =
        crate::response::ok(json!({ "accepted": accepted, "rejected": rejected })).into_response();
    *res.status_mut() = StatusCode::ACCEPTED;
    res
}

/// One row from the `agent_events` array of an edge payload.
///
/// Returns `false` for a hit that could not be used at all, which is only ever a
/// missing hostname or path: everything else about a fetch is optional.
async fn ingest_agent_hit(
    state: &AppState,
    site: &SiteCtx,
    hit: WireAgentEvent,
    policy: Option<&PolicyCtx>,
    salt: &[u8],
) -> bool {
    let classification = micaforge_agents::classify(&hit.user_agent, &HeaderView::empty(), None);
    record_agent_hit(state, site, hit, &classification, policy, salt).await
}

/// [`ingest_agent_hit`] with the classification already made.
///
/// Split out so a caller that has to classify first to decide whether a hit
/// belongs here at all ([`ingest_server_hits`]) does not pay for the catalog
/// match twice on every line of a log batch.
async fn record_agent_hit(
    state: &AppState,
    site: &SiteCtx,
    hit: WireAgentEvent,
    classification: &Classification,
    policy: Option<&PolicyCtx>,
    salt: &[u8],
) -> bool {
    let hostname = normalise_host(&hit.hostname);
    if hostname.is_empty() || !check_origin(site, &hostname) {
        ingest_guard::count_blocked(state, site.site_id, Blocked::Hostname);
        return false;
    }
    let pathname = url_norm::normalise_path(&hit.pathname, site.trailing_slash);
    if site.excluded.iter().any(|g| glob_match(g, &pathname)) {
        return false;
    }

    let ip = hit.ip.as_deref().and_then(parse_ip);
    if !ingest_guard::admit(state, site.site_id, &site.guard, ip) {
        return false;
    }
    let ip_text = ip.map(|a| a.to_string()).unwrap_or_default();
    let location = ip.map(|a| state.geo.lookup(a)).unwrap_or_default();
    let ip_hash = ip_digest(salt, site.site_id, &ip_text);

    let timestamp = hit.timestamp_utc();
    let (robots_allowed, llms_listed) = policy_flags(policy, classification.agent(), &pathname);

    match classification.agent() {
        Some(def) => {
            let verified = verified_flag(state, def, ip).await;
            let mut row = AgentEventRow {
                site_id: site.site_id,
                timestamp,
                agent_id: def.slug.to_string(),
                operator: def.operator.to_string(),
                purpose: def.purpose.as_str().to_string(),
                verified,
                user_agent: clamp(&hit.user_agent, MAX_TEXT),
                method: method_of(&hit.method),
                hostname: clamp(&hostname, MAX_TEXT),
                pathname,
                querystring: clamp(&hit.querystring, MAX_TEXT),
                status: hit.status.unwrap_or(200),
                bytes: hit.bytes.unwrap_or(0),
                duration_ms: hit.duration_ms.unwrap_or(0),
                content_type: clamp(&hit.content_type, 96),
                ip_hash: ip_hash.clone(),
                robots_allowed,
                llms_listed,
                conditional: u8::from(hit.conditional.unwrap_or(0) != 0),
                ..AgentEventRow::default()
            };
            apply_location(&location, &mut row.asn, &mut row.asn_org, &mut row.country);
            state.writer.push_agent(row);
            let key = if ip_hash.is_empty() {
                def.slug.to_string()
            } else {
                ip_hash
            };
            let _ = state.kv.bump_realtime(site.site_id, &key, true).await;
        }
        None => {
            // Not nameable. `bot_events` keeps the evidence for three months so
            // a detection rule can be argued with, and nothing else.
            let signals = classification.signals();
            let mut row = BotEventRow {
                site_id: site.site_id,
                timestamp,
                user_agent: clamp(&hit.user_agent, MAX_TEXT),
                method: method_of(&hit.method),
                hostname: clamp(&hostname, MAX_TEXT),
                pathname,
                querystring: clamp(&hit.querystring, MAX_TEXT),
                status: hit.status.unwrap_or(200),
                bytes: hit.bytes.unwrap_or(0),
                duration_ms: hit.duration_ms.unwrap_or(0),
                content_type: clamp(&hit.content_type, 96),
                ip_hash,
                robots_allowed,
                llms_listed,
                conditional: u8::from(hit.conditional.unwrap_or(0) != 0),
                reasons: signals
                    .map(|s| clamp(&s.reasons_joined(), MAX_TEXT))
                    .unwrap_or_else(|| "reported_as_agent_hit".to_string()),
                score: signals.map(|s| s.score).unwrap_or(0),
                ..BotEventRow::default()
            };
            apply_location(&location, &mut row.asn, &mut row.asn_org, &mut row.country);
            state.writer.push_bot(row);
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Request logs from a hosting platform
// ---------------------------------------------------------------------------

/// One request a hosting platform's log stream reported.
///
/// The column set of a `WireAgentEvent`, because it describes the same fact:
/// a server answered a fetch. It is a separate type so the log-drain routes
/// never see the edge path's wire format, and so the edge format can change
/// without a drain parser noticing.
#[derive(Debug, Default, Clone)]
pub(crate) struct ServerHit {
    /// When the platform answered the request. `None` means "now".
    pub timestamp: Option<DateTime<Utc>>,
    pub user_agent: String,
    pub method: String,
    pub hostname: String,
    pub pathname: String,
    pub querystring: String,
    pub status: Option<u16>,
    pub bytes: Option<u32>,
    pub duration_ms: Option<u32>,
    pub content_type: String,
    /// Transport only: verified against published ranges, hashed, dropped.
    pub ip: Option<String>,
}

/// Where the hits of one log batch went.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HitTally {
    /// Written to `agent_events`: a named agent.
    pub agents: usize,
    /// Written to `bot_events`: not human, not nameable.
    pub bots: usize,
    /// Classified as a person and skipped.
    pub humans: usize,
    /// Unusable: no hostname, a hostname that is not the site's, or an
    /// excluded path.
    pub rejected: usize,
}

/// Run a batch of platform request logs through the edge path's agent rules.
///
/// This is `POST /api/log/edge` without the envelope: the caller has already
/// proved, its own way, that the batch speaks for `site`. Every hit is
/// classified against the same catalog; a named agent lands in `agent_events`
/// with its verification and policy columns, an unnamed non-human client in
/// `bot_events`, exactly as an edge batch would file them.
///
/// **A person is skipped, not counted.** A platform log carries every request,
/// and the human ones are already counted by the tracker in the page, with a
/// session, a referrer and a visitor digest that a log line cannot give. Filing
/// them here as well would count every reader twice.
pub(crate) async fn ingest_server_hits(
    state: &AppState,
    site: &sites_model::Site,
    hits: Vec<ServerHit>,
) -> HitTally {
    let site = SiteCtx::from_row(site);
    let salt = daily_salt(state, &site).await;
    let policy = resolve_policy(state, &site).await;
    let mut tally = HitTally::default();

    for hit in hits.into_iter().take(MAX_EDGE_ROWS) {
        let classification =
            micaforge_agents::classify(&hit.user_agent, &HeaderView::empty(), None);
        if classification.is_human() {
            tally.humans += 1;
            continue;
        }
        let named = classification.agent().is_some();
        let wire = WireAgentEvent {
            timestamp: hit.timestamp.map(|t| t.to_rfc3339()).unwrap_or_default(),
            user_agent: hit.user_agent,
            method: hit.method,
            hostname: hit.hostname,
            pathname: hit.pathname,
            querystring: hit.querystring,
            status: hit.status,
            bytes: hit.bytes,
            duration_ms: hit.duration_ms,
            content_type: hit.content_type,
            conditional: None,
            ip: hit.ip,
        };
        match record_agent_hit(
            state,
            &site,
            wire,
            &classification,
            policy.as_deref(),
            &salt,
        )
        .await
        {
            true if named => tally.agents += 1,
            true => tally.bots += 1,
            false => tally.rejected += 1,
        }
    }
    tally
}

/// One row from the `events` array of an edge payload.
///
/// A server SDK reports human page views for sites that cannot run the tracker.
/// The same classifier runs, so a hit the shipper thought was a person but whose
/// user agent names a crawler still lands in `agent_events` where it belongs.
async fn ingest_edge_event(
    state: &AppState,
    site: &SiteCtx,
    event: WireEvent,
    policy: Option<&PolicyCtx>,
    salt: &[u8],
) -> bool {
    let hostname = normalise_host(&event.hostname);
    if hostname.is_empty() || !check_origin(site, &hostname) {
        ingest_guard::count_blocked(state, site.site_id, Blocked::Hostname);
        return false;
    }

    let user_agent = event.user_agent.clone().unwrap_or_default();
    let classification = micaforge_agents::classify(&user_agent, &event.header_view(), None);
    if classification.agent().is_some() {
        // Reported as a human, recognised as a crawler. The catalog wins: a
        // shipper that cannot tell the difference is exactly why this server
        // classifies for itself rather than trusting what it was told.
        return ingest_agent_hit(
            state,
            site,
            WireAgentEvent {
                timestamp: event.timestamp.clone(),
                user_agent,
                method: "GET".to_string(),
                hostname: event.hostname.clone(),
                pathname: event.pathname.clone(),
                querystring: event.querystring.clone(),
                ip: event.ip.clone(),
                status: Some(200),
                ..WireAgentEvent::default()
            },
            policy,
            salt,
        )
        .await;
    }

    let Some(url) = event.page_url(&hostname) else {
        return false;
    };
    let pathname = url_norm::normalise_path(url.path(), site.trailing_slash);
    if site.excluded.iter().any(|g| glob_match(g, &pathname)) {
        return false;
    }

    let Some(kind) = EventKind::parse(&event.kind).ok() else {
        return false;
    };

    let ip = event.ip.as_deref().and_then(parse_ip);
    if !ingest_guard::admit(state, site.site_id, &site.guard, ip) {
        return false;
    }
    let ip_text = ip.map(|a| a.to_string()).unwrap_or_default();
    let location = ip.map(|a| state.geo.lookup(a)).unwrap_or_default();

    if !classification.is_human() {
        let signals = classification.signals();
        let mut row = BotEventRow {
            site_id: site.site_id,
            timestamp: event.timestamp_utc(),
            user_agent: clamp(&user_agent, MAX_TEXT),
            hostname: clamp(&hostname, MAX_TEXT),
            pathname,
            status: 200,
            ip_hash: ip_digest(salt, site.site_id, &ip_text),
            reasons: signals
                .map(|s| clamp(&s.reasons_joined(), MAX_TEXT))
                .unwrap_or_default(),
            score: signals.map(|s| s.score).unwrap_or(0),
            ..BotEventRow::default()
        };
        apply_location(&location, &mut row.asn, &mut row.asn_org, &mut row.country);
        state.writer.push_bot(row);
        return true;
    }

    let payload = event.as_payload(site.site_id);
    let visitor = identity::visitor_id(salt, site.site_id, &ip_text, &user_agent);
    let (session, entry_path) = session_for(state, site.site_id, &visitor, &pathname).await;

    let mut row = build_event_row(
        site,
        &payload,
        kind,
        &url,
        &hostname,
        &pathname,
        &user_agent,
        None,
        None,
        None,
        &location,
    );
    row.visitor_id = visitor.clone();
    row.session_id = session;
    row.entry_path = entry_path;
    // The one intake whose clock is believed: the batch is signed with the
    // site's key, so the time it reports is the site's own claim.
    row.timestamp = event.timestamp_utc();

    if kind == EventKind::Error {
        mirror_error(state, &row);
    }
    state.writer.push_event(row);
    let _ = state.kv.bump_realtime(site.site_id, &visitor, false).await;
    true
}

/// Did the fetch come from an address the operator themselves publishes?
///
/// This is what makes `agent_events.verified` a fact rather than a repetition of
/// the user agent's claim. Anything can send `GPTBot`; only OpenAI can send it
/// from OpenAI's published prefixes. An address that is absent, or an operator
/// who publishes nothing, is reported as unverified — which is `0`, "not
/// proven", and never a guess in the other direction.
///
/// The `RangeStore` is shared with the twelve-hourly refresh job, so this is the
/// one place in the module that takes a lock. It is written for
/// `tokio::sync::RwLock`; if `AppState` holds a `std::sync::RwLock` instead,
/// this call becomes `state.ranges.read().ok()` and the guard must be dropped
/// before the next `await`, because a `std` guard is not `Send`.
async fn verified_flag(state: &AppState, def: &'static AgentDef, ip: Option<IpAddr>) -> u8 {
    let Some(addr) = ip else {
        return 0;
    };
    state
        .ranges
        .read()
        .await
        .verify(def.slug, normalize_ip(addr))
        .as_u8()
}

/// Resolve the two policy columns for one fetch.
///
/// Both are recorded at fetch time rather than read at query time, because a
/// site owner who tightens `robots.txt` today has not retroactively made
/// yesterday's crawl a violation, and a report that says otherwise is wrong.
fn policy_flags(
    stated: Option<&PolicyCtx>,
    agent: Option<&'static AgentDef>,
    pathname: &str,
) -> (i8, i8) {
    let Some(stated) = stated else {
        return (-1, -1);
    };
    let llms = stated.llms.as_ref();
    let allowed = match agent {
        Some(def) => policy::evaluate(&stated.robots, llms, def, pathname).0,
        // No named agent: the only rule that can apply is the wildcard group.
        None if stated.robots.group_for("*").is_some() => Some(stated.robots.allows("*", pathname)),
        None => None,
    };
    (
        policy::robots_allowed_flag(allowed),
        policy::llms_listed_flag(llms, pathname),
    )
}

// ---------------------------------------------------------------------------
// The ingest key
// ---------------------------------------------------------------------------

/// Where `routes::sites` seals the ingest key inside `sites.settings`.
///
/// Named here only so this module can tell "the site has a key" from "the site
/// has never needed one" without minting anything. The format, the sealing and
/// the rotation all belong to [`crate::routes::sites::ingest_key`], which stays
/// the single implementation.
const INGEST_KEY_FIELD: &str = "ingest_key_sealed";

/// The site's ingest key, if it already has one.
///
/// [`crate::routes::sites::ingest_key`] mints a key when the site has none,
/// which is right for the settings screen — an operator asking to see the key
/// wants one to exist — and wrong here. This route is unauthenticated, so a
/// stranger posting a nonsense body must not be able to make the server write to
/// Postgres, and a site that has never used the server-side path should not grow
/// a secret because somebody probed it. The presence check happens here and the
/// unsealing is delegated, so only one place knows the stored form.
///
/// `None` therefore means no signature can verify, which is exactly right: a
/// shipper can only hold a key that was minted, and minting stores it.
async fn stored_ingest_key(state: &AppState, site: &sites_model::Site) -> Option<String> {
    site.settings
        .get(INGEST_KEY_FIELD)
        .and_then(|v| v.as_str())
        .filter(|encoded| !encoded.is_empty())?;

    match crate::routes::sites::ingest_key(&state.pg, &state.cfg, site).await {
        Ok(key) => Some(key),
        Err(e) => {
            tracing::warn!(site = %site.id, error = %e, "the stored ingest key could not be opened");
            None
        }
    }
}

/// A signature that verified: its timestamp and its MAC.
struct Signed {
    t: i64,
    mac: [u8; 32],
}

/// Constant-time check of `t=<unix>,v1=<hex>` over `"{t}.{body}"`.
///
/// Three things have to hold, and all three are load-bearing. The timestamp is
/// inside the signed material, so a captured body cannot be replayed under a
/// fresh `t`. The window is five minutes either side of `now`, so a shipper
/// whose clock drifts a little still works and a recording from an hour ago
/// does not. And the comparison is constant time, because a byte-at-a-time
/// compare on an HMAC is a forgery oracle.
fn verify_signature(key: &str, body: &[u8], header_value: &str, now: i64) -> Option<Signed> {
    let mut timestamp: Option<(i64, &str)> = None;
    let mut provided: Option<Vec<u8>> = None;

    for part in header_value.split(',') {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        match name.trim() {
            "t" => {
                let raw = value.trim();
                timestamp = raw.parse::<i64>().ok().map(|t| (t, raw));
            }
            "v1" => provided = hex::decode(value.trim()).ok(),
            _ => {}
        }
    }

    let (Some((t, t_raw)), Some(provided)) = (timestamp, provided) else {
        return None;
    };
    if (now - t).abs() > SIGNATURE_WINDOW_SECS || provided.len() != 32 {
        return None;
    }

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes()).ok()?;
    // The digits exactly as sent, so `t=0170...` signs what the shipper signed.
    mac.update(t_raw.as_bytes());
    mac.update(b".");
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    if !bool::from(expected.as_slice().ct_eq(&provided)) {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&expected);
    Some(Signed { t, mac: out })
}

/// The signatures this process has accepted and that are still inside their
/// window.
fn seen_signatures() -> &'static Mutex<HashMap<[u8; 32], i64>> {
    static SEEN: OnceLock<Mutex<HashMap<[u8; 32], i64>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `true` the first time a verified signature is presented, `false` for an
/// exact repeat while it could still verify.
///
/// The window alone bounds a replay to five minutes; this closes those five
/// minutes for a single process, which is the shape `PRODUCT.md` ships. Each
/// entry lives until its own `t` leaves the window. The map is bounded: when it
/// is full of live entries the eighth closest to expiry is dropped, which can
/// only reopen a replay that was about to expire anyway, and only a holder of
/// the key can fill it.
fn first_use(seen: &Mutex<HashMap<[u8; 32], i64>>, signed: &Signed, now: i64) -> bool {
    let mut map = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if map.get(&signed.mac).is_some_and(|until| *until >= now) {
        return false;
    }
    if map.len() >= MAX_SEEN_SIGNATURES {
        map.retain(|_, until| *until >= now);
    }
    if map.len() >= MAX_SEEN_SIGNATURES {
        let mut by_expiry: Vec<(i64, [u8; 32])> = map.iter().map(|(k, v)| (*v, *k)).collect();
        by_expiry.sort_unstable();
        for (_, mac) in by_expiry.into_iter().take(MAX_SEEN_SIGNATURES / 8) {
            map.remove(&mac);
        }
    }
    map.insert(signed.mac, signed.t + SIGNATURE_WINDOW_SECS);
    true
}

/// A server batch whose signature verified for a site, not yet claimed.
///
/// For the signed collection routes other than [`edge`] itself (the MCP
/// intake in `routes::model_context`). They prove the same thing the same
/// way: the same key, the same window and the same replay memory, so a batch
/// signed for one route cannot be replayed onto another.
pub(crate) struct SignedBatch {
    /// The site the batch speaks for, read fresh like [`edge`] reads it.
    pub site: sites_model::Site,
    signed: Signed,
    now: i64,
}

impl SignedBatch {
    /// Claim the signature: `true` the first time, `false` for an exact repeat
    /// inside its window. Call it after the route's own limiter, as [`edge`]
    /// does, so a batch refused with `429` can be sent again as it was.
    pub(crate) fn claim(&self) -> bool {
        first_use(seen_signatures(), &self.signed, self.now)
    }
}

/// Verify a signed server batch for `site_id` exactly as [`edge`] does.
///
/// `None` for a site that does not exist, a site with no ingest key, a key
/// that will not open, and a signature that is wrong or outside its window,
/// after the same work in every case, so the caller answers all of them with
/// one `401` that says nothing about which.
pub(crate) async fn verify_server_batch(
    state: &AppState,
    site_id: u16,
    body: &[u8],
    signature: &str,
) -> Option<SignedBatch> {
    let row = sites_model::get_by_site_id(&state.pg, site_id).await.ok();
    let key = match row.as_ref() {
        Some(row) => stored_ingest_key(state, row).await,
        None => None,
    };
    let now = Utc::now().timestamp();
    let signed = verify_signature(key.as_deref().unwrap_or(""), body, signature, now)
        .filter(|_| key.is_some())?;
    Some(SignedBatch {
        site: row?,
        signed,
        now,
    })
}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// Everything the handlers read off the transport.
struct RequestCtx {
    /// The client address, after the trusted-proxy chain is walked.
    ip: IpAddr,
    /// The same address rendered once, because it is hashed twice per hit.
    ip_text: String,
    user_agent: String,
    accept: Option<String>,
    accept_language: Option<String>,
    sec_fetch_mode: Option<String>,
    sec_ch_ua: Option<String>,
    sec_ch_ua_platform: Option<String>,
    sec_ch_ua_mobile: Option<String>,
    from: Option<String>,
    signature_agent: Option<String>,
    /// Host of the `Origin` header, used when the payload carries no URL.
    origin_host: Option<String>,
    /// The request method, written onto a `bot_events` row so a pixel hit and a
    /// beacon can be told apart after the fact.
    method: String,
    do_not_track: bool,
    /// The visitor's location as the edge in front of this server resolved it,
    /// when that edge is Cloudflare and is trusted for the address too.
    edge_location: Option<Location>,
}

impl RequestCtx {
    /// The borrowed view the classifier reads.
    fn header_view(&self) -> HeaderView<'_> {
        HeaderView {
            accept: self.accept.as_deref(),
            accept_language: self.accept_language.as_deref(),
            sec_fetch_mode: self.sec_fetch_mode.as_deref(),
            sec_ch_ua: self.sec_ch_ua.as_deref(),
            from: self.from.as_deref(),
            signature_agent: self.signature_agent.as_deref(),
        }
    }
}

/// Take the request apart into the facts the handlers need, plus its body.
///
/// Reading the whole request rather than using typed extractors is deliberate:
/// an extractor that rejects produces a `4xx` with a body, and this surface has
/// to be able to answer `204` to absolutely anything.
async fn split_request(req: Request, limit: usize) -> Option<(RequestCtx, Vec<u8>)> {
    let (parts, body) = req.into_parts();

    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let headers = &parts.headers;

    let origin_host = header(headers, "origin")
        .or_else(|| header(headers, "referer"))
        .and_then(|v| Url::parse(v).ok())
        .and_then(|u| u.host_str().map(normalise_host))
        .filter(|h| !h.is_empty());

    let ip = client_ip(headers, peer);
    let method = parts.method.as_str().to_string();
    let ctx = RequestCtx {
        method,
        ip_text: ip.to_string(),
        ip,
        user_agent: header(headers, "user-agent")
            .map(|v| clamp(v, MAX_TEXT))
            .unwrap_or_default(),
        accept: header(headers, "accept").map(|v| clamp(v, 256)),
        accept_language: header(headers, "accept-language").map(|v| clamp(v, 128)),
        sec_fetch_mode: header(headers, "sec-fetch-mode").map(|v| clamp(v, 32)),
        sec_ch_ua: header(headers, "sec-ch-ua").map(|v| clamp(v, 256)),
        sec_ch_ua_platform: header(headers, "sec-ch-ua-platform").map(|v| clamp(v, 64)),
        sec_ch_ua_mobile: header(headers, "sec-ch-ua-mobile").map(|v| clamp(v, 16)),
        from: header(headers, "from").map(|v| clamp(v, 128)),
        signature_agent: header(headers, "signature-agent").map(|v| clamp(v, 256)),
        do_not_track: matches!(header(headers, "dnt"), Some("1"))
            || matches!(header(headers, "sec-gpc"), Some("1")),
        origin_host,
        edge_location: edge_location(headers),
    };

    if limit == 0 {
        return Some((ctx, Vec::new()));
    }
    match to_bytes(body, limit).await {
        Ok(bytes) => Some((ctx, bytes.to_vec())),
        Err(_) => None,
    }
}

/// The trusted-proxy address rule, shared with the rate limiter and the access
/// log.
///
/// It lives in [`crate::client_ip`] rather than here because three different
/// layers have to agree about which address a request came from: this module
/// folds it into the visitor digest, `middleware::rate_limit` counts against it,
/// and `middleware::trace` logs it. Two implementations of that rule would drift,
/// and the drift would look like a visitor count that disagrees with the log.
use crate::client_ip::client_ip;

// ---------------------------------------------------------------------------
// Site resolution
// ---------------------------------------------------------------------------

/// A site, in the form the ingest path needs it.
///
/// Flattened out of `sites` and its `settings` document once, at cache fill
/// time, so a hit costs a hash lookup rather than a JSON walk.
struct SiteCtx {
    site_id: u16,
    uuid: Uuid,
    /// The primary domain, which is also the self-referral check's site host.
    domain: String,
    /// Every hostname allowed to write into this site.
    hosts: Vec<String>,
    tz: chrono_tz::Tz,
    trailing_slash: bool,
    respect_dnt: bool,
    excluded: Vec<String>,
    /// The site's address allow and deny lists (`crate::ingest_guard`).
    guard: Guard,
}

/// One cache slot. `None` records a site that is known *not* to exist.
struct CachedSite {
    until: Instant,
    site: Option<Arc<SiteCtx>>,
}

/// The answer to "which site is this id?".
enum SiteLookup {
    Found(Arc<SiteCtx>),
    /// Postgres says no site has this id.
    Unknown,
    /// Postgres could not be asked, and this process has no answer cached.
    Unavailable,
}

impl SiteLookup {
    fn of(site: Option<Arc<SiteCtx>>) -> SiteLookup {
        match site {
            Some(site) => SiteLookup::Found(site),
            None => SiteLookup::Unknown,
        }
    }
}

/// What one Postgres lookup produced.
enum Fetched {
    Row(Arc<SiteCtx>),
    Missing,
    Failed,
}

/// Resolve a numeric site id, with a short process-local cache.
///
/// The cache holds negative answers too, and that is a security property rather
/// than an optimisation: a stranger probing site ids must not be able to tell a
/// real site from an absent one by how long the answer took, and a probe that is
/// served from the same cache as a hit takes the same time.
///
/// **Stale while error.** An entry that has expired is refreshed by one request
/// while the others keep being served the old answer for [`REFRESH_GRACE`], and
/// when the refresh fails the old answer is kept for another
/// [`SITE_CACHE_TTL`]. A Postgres outage therefore costs nothing for a site this
/// process has seen; only a site it has never seen is [`SiteLookup::Unavailable`].
async fn resolve_site(state: &AppState, site_id: u16) -> SiteLookup {
    static CACHE: OnceLock<RwLock<HashMap<u16, CachedSite>>> = OnceLock::new();
    static UNAVAILABLE: LogGate = LogGate::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    if let Ok(map) = cache.read() {
        if let Some(entry) = map.get(&site_id) {
            if entry.until > Instant::now() {
                return SiteLookup::of(entry.site.clone());
            }
        }
    }

    // Claim the refresh. Pushing the expired entry a few seconds forward is what
    // stops every concurrent hit for a busy site from queueing on the pool.
    let stale = match cache.write() {
        Ok(mut map) => match map.get_mut(&site_id) {
            Some(entry) if entry.until > Instant::now() => {
                return SiteLookup::of(entry.site.clone());
            }
            Some(entry) => {
                entry.until = Instant::now() + REFRESH_GRACE;
                Some(entry.site.clone())
            }
            None => None,
        },
        Err(_) => None,
    };

    let fetched = match tokio::time::timeout(
        SITE_LOOKUP_BUDGET,
        sites_model::get_by_site_id(&state.pg, site_id),
    )
    .await
    {
        Ok(Ok(row)) => Fetched::Row(Arc::new(SiteCtx::from_row(&row))),
        Ok(Err(micaforge_core::Error::NotFound(_))) => Fetched::Missing,
        Ok(Err(e)) => {
            if UNAVAILABLE.open(Duration::from_secs(60)) {
                tracing::warn!(site_id, error = %e, "site lookup failed; serving cached answers");
            }
            Fetched::Failed
        }
        Err(_) => {
            if UNAVAILABLE.open(Duration::from_secs(60)) {
                tracing::warn!(site_id, "site lookup timed out; serving cached answers");
            }
            Fetched::Failed
        }
    };

    let Some((site, ttl)) = settle(fetched, stale) else {
        return SiteLookup::Unavailable;
    };
    if let Ok(mut map) = cache.write() {
        let now = Instant::now();
        if !map.contains_key(&site_id) {
            make_room(&mut map, now);
        }
        map.insert(
            site_id,
            CachedSite {
                until: now + ttl,
                site: site.clone(),
            },
        );
    }
    SiteLookup::of(site)
}

/// What to cache, and for how long, given what Postgres said and what was
/// cached before. `None` means there is nothing to say and nothing to cache.
///
/// A failed lookup is never cached as "no such site": that would turn a
/// thirty-second outage into a minute of every customer's traffic discarded.
fn settle(
    fetched: Fetched,
    stale: Option<Option<Arc<SiteCtx>>>,
) -> Option<(Option<Arc<SiteCtx>>, Duration)> {
    match fetched {
        Fetched::Row(site) => Some((Some(site), SITE_CACHE_TTL)),
        Fetched::Missing => Some((None, NEGATIVE_TTL)),
        Fetched::Failed => stale.map(|site| (site, SITE_CACHE_TTL)),
    }
}

/// Make room for one more entry without emptying the cache.
///
/// Clearing the whole map when it filled up meant a client probing random ids
/// could keep it permanently empty, and every real hit would go to Postgres.
/// Expired absences go first, because they are worth nothing; then, if the map
/// is still full, an eighth of it is dropped, absences before sites and the
/// soonest to expire first. A probe can only ever evict other probes' answers
/// while fewer than seven eighths of the map are real sites.
fn make_room(map: &mut HashMap<u16, CachedSite>, now: Instant) {
    if map.len() < SITE_CACHE_MAX {
        return;
    }
    map.retain(|_, entry| entry.site.is_some() || entry.until > now);
    if map.len() < SITE_CACHE_MAX {
        return;
    }
    let mut order: Vec<(bool, Instant, u16)> = map
        .iter()
        .map(|(id, entry)| (entry.site.is_some(), entry.until, *id))
        .collect();
    order.sort_unstable();
    for (_, _, id) in order.into_iter().take(SITE_CACHE_MAX / 8) {
        map.remove(&id);
    }
}

impl SiteCtx {
    fn from_row(site: &sites_model::Site) -> SiteCtx {
        let tracking = site.settings.get("tracking");
        let flag = |key: &str, default: bool| -> bool {
            tracking
                .and_then(|t| t.get(key))
                .and_then(|v| v.as_bool())
                .unwrap_or(default)
        };

        let domain = normalise_host(&site.domain);
        let mut hosts = vec![domain.clone()];
        // `settings.domains` is the escape hatch for a site served from several
        // hostnames — a docs subdomain, a country domain, a staging host. It is
        // additive: the primary domain is always allowed.
        if let Some(extra) = site.settings.get("domains").and_then(|v| v.as_array()) {
            for value in extra.iter().take(32) {
                if let Some(host) = value.as_str() {
                    let host = normalise_host(host);
                    if !host.is_empty() && !hosts.contains(&host) {
                        hosts.push(host);
                    }
                }
            }
        }

        let excluded = tracking
            .and_then(|t| t.get("excluded_paths"))
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .take(32)
                    .collect()
            })
            .unwrap_or_default();

        SiteCtx {
            site_id: site.site_id_u16(),
            uuid: site.id,
            domain,
            hosts,
            tz: site.timezone.parse().unwrap_or(chrono_tz::UTC),
            // A site that has not said otherwise keeps `/docs` and `/docs/`
            // apart, because merging two pages that really are two pages cannot
            // be undone after the fact.
            trailing_slash: flag("trailing_slash", true),
            respect_dnt: flag("respect_dnt", false),
            excluded,
            guard: Guard::from_settings(&site.settings),
        }
    }
}

/// Is this hostname one of the site's own?
///
/// This is the check that stops a stranger spraying data into someone else's
/// dashboard. Ingest is unauthenticated by design — a site id is public, it is
/// in the script tag on every page — so the hostname the hit claims to have
/// happened on is the only thing standing between a site's numbers and anyone
/// who can read its HTML.
///
/// A subdomain of an allowed domain is allowed: `docs.example.com` writing into
/// `example.com` is the normal shape of a documentation site. `notexample.com`
/// is not, and the label-boundary test is what tells the two apart.
fn check_origin(site: &SiteCtx, hostname: &str) -> bool {
    let host = normalise_host(hostname);
    if host.is_empty() {
        return false;
    }
    site.hosts.iter().any(|allowed| {
        host == *allowed
            || host
                .len()
                .checked_sub(allowed.len() + 1)
                .is_some_and(|boundary| {
                    host.ends_with(allowed.as_str()) && host.as_bytes()[boundary] == b'.'
                })
    })
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// A site's stated policy, parsed once and reused.
struct PolicyCtx {
    robots: robots::Robots,
    llms: Option<LlmsTxt>,
}

/// One cache slot for a parsed policy.
struct CachedPolicy {
    until: Instant,
    policy: Option<Arc<PolicyCtx>>,
}

/// The site's parsed `robots.txt` and `llms.txt`, from the crawler's stored copy.
///
/// `None` means the crawler has never fetched this site's files, which is a real
/// state and not an error: both policy columns are then written as `-1`,
/// "unknown", rather than as the permissive answer.
async fn resolve_policy(state: &AppState, site: &SiteCtx) -> Option<Arc<PolicyCtx>> {
    static CACHE: OnceLock<RwLock<HashMap<Uuid, CachedPolicy>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    if let Ok(map) = cache.read() {
        if let Some(entry) = map.get(&site.uuid) {
            if entry.until > Instant::now() {
                return entry.policy.clone();
            }
        }
    }

    let parsed = match catalog_model::get_policy(&state.pg, site.uuid).await {
        Ok(Some(row)) => Some(Arc::new(PolicyCtx {
            robots: robots::parse(row.robots_txt.as_deref().unwrap_or_default()),
            llms: row
                .llms_txt
                .as_deref()
                .map(micaforge_agents::llms_txt::parse),
        })),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(site = %site.uuid, error = %e, "policy lookup failed");
            return None;
        }
    };

    if let Ok(mut map) = cache.write() {
        if map.len() > 4096 {
            map.clear();
        }
        map.insert(
            site.uuid,
            CachedPolicy {
                until: Instant::now() + POLICY_CACHE_TTL,
                policy: parsed.clone(),
            },
        );
    }
    parsed
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// The site's salt for the current day in *its own* timezone.
///
/// Rotating at site-local midnight rather than UTC is what makes "unique
/// visitors today" mean the same thing on the dashboard as it does in the
/// operator's head.
///
/// When Redis fails or does not answer within [`SALT_BUDGET`], the hit is hashed
/// with [`fallback_salt`]: random, per site, per UTC day, held only in this
/// process's memory and gone after 48 hours. Visitors seen during the outage
/// count again once Redis is back, which is a slightly high number for a short
/// window. The alternatives are worse: an empty salt makes the digest a
/// permanent, brute-forceable identifier, and dropping the hit leaves a hole.
async fn daily_salt(state: &AppState, site: &SiteCtx) -> Vec<u8> {
    static UNAVAILABLE: LogGate = LogGate::new();
    let today = Utc::now().with_timezone(&site.tz).date_naive();

    let failure =
        match tokio::time::timeout(SALT_BUDGET, state.kv.daily_salt_for(site.site_id, today)).await
        {
            Ok(Ok(salt)) if !salt.is_empty() => return salt,
            Ok(Ok(_)) => "redis returned an empty salt".to_string(),
            Ok(Err(e)) => e.to_string(),
            Err(_) => "redis did not answer in time".to_string(),
        };

    if UNAVAILABLE.open(Duration::from_secs(60)) {
        tracing::warn!(
            site_id = site.site_id,
            reason = %failure,
            "the visitor salt could not be read; hashing with an in-memory salt for this UTC day"
        );
    }
    fallback_salt(site.site_id, Utc::now().date_naive())
}

/// In-memory fallback salts, by site and UTC day.
type FallbackSalts = HashMap<(u16, NaiveDate), Vec<u8>>;

/// The in-memory salt for one site on one UTC day. See [`daily_salt`].
fn fallback_salt(site_id: u16, today: NaiveDate) -> Vec<u8> {
    static SALTS: OnceLock<Mutex<FallbackSalts>> = OnceLock::new();
    let cell = SALTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    salt_from(&mut map, site_id, today)
}

/// [`fallback_salt`] over a map the caller owns, so it can be tested.
///
/// An entry is kept for its own day and the next, which is 48 hours from the
/// first moment it could have been made, and dropped after that: the same
/// lifetime the Redis copy has, for the same reason.
fn salt_from(map: &mut FallbackSalts, site_id: u16, today: NaiveDate) -> Vec<u8> {
    if let Some(salt) = map.get(&(site_id, today)) {
        return salt.clone();
    }
    let oldest = today.pred_opt().unwrap_or(today);
    map.retain(|(_, day), _| *day >= oldest);

    let salt = crypto::random_token(32).into_bytes();
    if map.len() < MAX_FALLBACK_SALTS {
        map.insert((site_id, today), salt.clone());
    }
    salt
}

/// The salted, rotating digest of an address.
///
/// The same construction as the visitor id with an empty user agent, so an agent
/// row groups by address without ever holding one. It shares the visitor salt,
/// which means it rotates and expires on the same 48-hour schedule, and it is
/// synchronous so that a batch of five hundred rows costs one salt read rather
/// than five hundred.
/// Where the visitor is, from the GeoIP database when there is one, and from
/// the trusted edge's own lookup when the database has no answer.
///
/// An install with no `.mmdb` (the default) would otherwise record no
/// geography at all, while Cloudflare has already resolved it on every request.
fn locate(state: &AppState, ctx: &RequestCtx) -> Location {
    let mut location = state.geo.lookup(ctx.ip);
    if location.country.is_empty() {
        if let Some(edge) = &ctx.edge_location {
            location.country = edge.country.clone();
            location.region = edge.region.clone();
            location.city = edge.city.clone();
            location.lat = edge.lat;
            location.lon = edge.lon;
        }
    }
    location
}

/// Cloudflare's own lookup of the visitor, believed only when Cloudflare is the
/// proxy this server already trusts for the client address
/// (`MICAFORGE_TRUSTED_PROXY_HEADER` is `cf-connecting-ip`): the same trust,
/// extended to the same hop's other answers. `CF-IPCountry` is always sent; the
/// city, region and coordinates arrive when the zone has Cloudflare's "Add
/// visitor location headers" transform turned on.
///
/// Also read by the short-link redirect, which locates a click the way ingest
/// locates a pageview.
pub(crate) fn edge_location(headers: &axum::http::HeaderMap) -> Option<Location> {
    if crate::client_ip::proxy_header() != Some("cf-connecting-ip") {
        return None;
    }
    // City names are sent as raw UTF-8, which `header` (visible ASCII only)
    // would refuse, so they are read off the bytes.
    let utf8 = |name: &str| {
        headers
            .get(name)
            .and_then(|v| std::str::from_utf8(v.as_bytes()).ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    parse_edge_location(
        header(headers, "cf-ipcountry"),
        header(headers, "cf-region-code"),
        utf8("cf-region"),
        utf8("cf-ipcity"),
        header(headers, "cf-iplatitude"),
        header(headers, "cf-iplongitude"),
    )
}

/// The location itself, or `None` without a real country code.
fn parse_edge_location(
    country: Option<&str>,
    region_code: Option<&str>,
    region_name: Option<&str>,
    city: Option<&str>,
    lat: Option<&str>,
    lon: Option<&str>,
) -> Option<Location> {
    let country = parse_edge_country(country)?;
    let code = region_code
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| (1..=3).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_alphanumeric()));
    let region = match code {
        Some(code) => format!("{country}-{code}"),
        None => region_name.map(|r| clamp(r, 64)).unwrap_or_default(),
    };
    let coord = |raw: Option<&str>, limit: f32| {
        raw.and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite() && v.abs() <= limit)
    };
    let (lat, lon) = match (coord(lat, 90.0), coord(lon, 180.0)) {
        (Some(lat), Some(lon)) => (lat, lon),
        _ => (0.0, 0.0),
    };
    Some(Location {
        country,
        region,
        city: city.map(|c| clamp(c, 64)).unwrap_or_default(),
        lat,
        lon,
        ..Location::default()
    })
}

/// The country code itself, or `None` for anything that is not one. `XX`
/// (unknown) and `T1` (Tor) are Cloudflare's markers, not countries.
fn parse_edge_country(raw: Option<&str>) -> Option<String> {
    let code = raw?.trim().to_ascii_uppercase();
    let is_code = code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase());
    (is_code && code != "XX" && code != "T1").then_some(code)
}

fn ip_digest(salt: &[u8], site_id: u16, ip_text: &str) -> String {
    if ip_text.is_empty() {
        return String::new();
    }
    crypto::visitor_hash(salt, site_id, ip_text, "")
}

/// Continue this visitor's session, or open a new one.
///
/// Returns the session id and the path the session started on. The entry path is
/// carried on every row (`docs/CONTRACT.md` §1.1) so the session rollup never
/// has to join a table to itself, and it is remembered in Redis beside the
/// session because that is the only place the first hit of a session still
/// exists by the time the second one arrives.
async fn session_for(
    state: &AppState,
    site_id: u16,
    visitor: &str,
    pathname: &str,
) -> (String, String) {
    let entry_key = format!("mf:entry:{site_id}:{visitor}");

    match state.kv.session_for(site_id, visitor).await {
        Ok(Some(session)) => {
            let entry = state
                .kv
                .get_json::<String>(&entry_key)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| pathname.to_string());
            // Push the inactivity window forward; a session ends when the
            // visitor stops, not after a fixed length.
            let _ = state
                .kv
                .set_session(site_id, visitor, &session, identity::SESSION_WINDOW_SECS)
                .await;
            (session, entry)
        }
        _ => {
            let session = identity::new_session_id();
            let _ = state
                .kv
                .set_session(site_id, visitor, &session, identity::SESSION_WINDOW_SECS)
                .await;
            let _ = state
                .kv
                .set_json(
                    &entry_key,
                    &pathname.to_string(),
                    Some(identity::SESSION_WINDOW_SECS),
                )
                .await;
            (session, pathname.to_string())
        }
    }
}

/// The answer-engine lookup `micaforge-enrich` takes as a callback.
///
/// The split exists so the enrichment crate never depends on the agent catalog.
/// This is the one line that joins them, and it is the line that makes
/// `answer_engine` a channel rather than a kind of referral.
fn answer_engine_lookup(host: &str) -> Option<&'static str> {
    micaforge_agents::classify_referrer_host(host).map(|engine| engine.slug)
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// The browser payload from `docs/CONTRACT.md` §4.
///
/// One- and two-character keys, because the whole thing rides in a `sendBeacon`
/// and the tracker's job is to be small. Every field is optional in Rust even
/// where the contract calls it required: a malformed payload has to parse far
/// enough to be *rejected deliberately*, not fail at the serde boundary and
/// become an unexplained `204`.
#[derive(Debug, Default, Clone, Deserialize)]
struct Payload {
    /// Site id.
    #[serde(default, deserialize_with = "flexible_u32")]
    s: u32,
    /// Event kind.
    #[serde(default)]
    t: Option<String>,
    /// Event name, custom events only.
    #[serde(default)]
    n: Option<String>,
    /// Full page URL.
    #[serde(default)]
    u: Option<String>,
    /// `document.referrer`.
    #[serde(default)]
    r: Option<String>,
    /// Screen width and height.
    #[serde(default)]
    w: Option<u32>,
    #[serde(default)]
    h: Option<u32>,
    /// Viewport width and height.
    #[serde(default)]
    vw: Option<u32>,
    #[serde(default)]
    vh: Option<u32>,
    /// `navigator.language`.
    #[serde(default)]
    l: Option<String>,
    /// IANA timezone.
    #[serde(default)]
    tz: Option<String>,
    /// Engagement milliseconds on the previous page.
    #[serde(default)]
    d: Option<u64>,
    /// Deepest scroll on the previous page, as a percentage.
    #[serde(default)]
    sc: Option<u32>,
    /// Custom properties.
    #[serde(default)]
    p: Option<Json>,
    /// The id from `identify()`.
    #[serde(default)]
    id: Option<String>,
    /// SDK version.
    #[serde(default)]
    v: Option<String>,
    /// Web vitals.
    #[serde(default)]
    e: Option<Vitals>,
    /// The tracker's `data-sample` rate, sent only by a visitor inside a
    /// sample. A number rather than an integer on the wire, because the
    /// attribute is whatever the site typed.
    #[serde(default)]
    sr: Option<f64>,

    /// Page title. Not sent by the browser tracker, which has no room for it;
    /// filled in by the server SDKs, which do.
    #[serde(default)]
    title: Option<String>,
    /// Experiment key, from a server SDK.
    #[serde(default)]
    experiment: Option<String>,
    /// Assigned variant, from a server SDK.
    #[serde(default)]
    variant: Option<String>,
    /// Feature-flag exposures, from a server SDK. Held as raw JSON because two
    /// shapes turn up in the wild — an object and an array of pairs — and a
    /// strict type here would drop the whole hit over one of them.
    #[serde(default)]
    flags: Option<Json>,
    // No `timestamp`. A key of that name is accepted and ignored: every row from
    // these unauthenticated routes is stamped with the time it was received.
}

impl Payload {
    /// The event kind, or `None` when `t` names one that does not exist.
    fn kind(&self) -> Option<EventKind> {
        match self.t.as_deref() {
            None | Some("") => Some(EventKind::Pageview),
            Some(raw) => EventKind::parse(raw).ok(),
        }
    }
}

/// Core Web Vitals, sent as `e`.
#[derive(Debug, Default, Clone, Deserialize)]
struct Vitals {
    #[serde(default)]
    lcp: Option<f32>,
    #[serde(default)]
    cls: Option<f32>,
    #[serde(default)]
    inp: Option<f32>,
    #[serde(default)]
    fcp: Option<f32>,
    #[serde(default)]
    ttfb: Option<f32>,
}

/// The `POST /api/log/edge` body.
#[derive(Debug, Default, Deserialize)]
struct EdgePayload {
    #[serde(default, deserialize_with = "flexible_u32")]
    site_id: u32,
    #[serde(default)]
    events: Vec<WireEvent>,
    #[serde(default)]
    agent_events: Vec<WireAgentEvent>,
}

/// One human row in an edge payload. Column names, not the tracker's short keys.
#[derive(Debug, Default, Clone, Deserialize)]
struct WireEvent {
    #[serde(default)]
    timestamp: String,
    #[serde(default = "pageview")]
    kind: String,
    #[serde(default)]
    event_name: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    pathname: String,
    #[serde(default)]
    page_title: String,
    #[serde(default)]
    querystring: String,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    referrer: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    timezone: String,
    #[serde(default)]
    identified_id: String,
    #[serde(default)]
    props: Option<Json>,
    #[serde(default)]
    revenue: Option<f64>,
    #[serde(default)]
    currency: String,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    scroll_pct: Option<u32>,
    #[serde(default)]
    experiment: String,
    #[serde(default)]
    variant: String,
    #[serde(default)]
    flags: Option<HashMap<String, String>>,
    #[serde(default)]
    sdk_version: String,
    /// Transport only: read, used, and never written down.
    #[serde(default)]
    user_agent: Option<String>,
    /// Transport only. `docs/CONTRACT.md` §7.1.
    #[serde(default)]
    ip: Option<String>,
    /// Transport only: the visitor's request headers, read by the bot check and
    /// never written down. Absent means "not known", as before.
    #[serde(default)]
    accept: Option<String>,
    #[serde(default)]
    accept_language: Option<String>,
    #[serde(default)]
    sec_fetch_mode: Option<String>,
    #[serde(default)]
    sec_ch_ua: Option<String>,
}

fn pageview() -> String {
    "pageview".to_string()
}

impl WireEvent {
    /// The visitor's headers the shipper forwarded, for the classifier. An empty
    /// string counts as absent.
    fn header_view(&self) -> HeaderView<'_> {
        fn some(v: &Option<String>) -> Option<&str> {
            v.as_deref().filter(|s| !s.is_empty())
        }
        HeaderView {
            accept: some(&self.accept),
            accept_language: some(&self.accept_language),
            sec_fetch_mode: some(&self.sec_fetch_mode),
            sec_ch_ua: some(&self.sec_ch_ua),
            ..HeaderView::empty()
        }
    }

    /// Rebuild a page URL from the split columns, so one enrichment path serves
    /// both intakes.
    ///
    /// `None` for a hostname that cannot form a URL at all, which the caller
    /// treats as a row it cannot place rather than as a reason to panic.
    fn page_url(&self, hostname: &str) -> Option<Url> {
        let path = if self.pathname.starts_with('/') {
            self.pathname.clone()
        } else {
            format!("/{}", self.pathname)
        };
        let query = if self.querystring.is_empty() {
            String::new()
        } else {
            format!("?{}", self.querystring.trim_start_matches('?'))
        };
        let fragment = if self.hash.is_empty() {
            String::new()
        } else {
            format!("#{}", self.hash.trim_start_matches('#'))
        };
        Url::parse(&format!("https://{hostname}{path}{query}{fragment}"))
            .or_else(|_| Url::parse(&format!("https://{hostname}/")))
            .ok()
    }

    /// The same row in the shape [`build_event_row`] reads.
    ///
    /// Revenue and currency are folded into `props` rather than passed beside
    /// it, because `props_of` is the one place money becomes a column and two
    /// routes into that column is two chances for them to round differently.
    fn as_payload(&self, site_id: u16) -> Payload {
        Payload {
            s: u32::from(site_id),
            t: Some(self.kind.clone()),
            n: non_empty(&self.event_name),
            u: None,
            r: non_empty(&self.referrer),
            l: non_empty(&self.language),
            tz: non_empty(&self.timezone),
            d: self.duration_ms,
            sc: self.scroll_pct,
            p: self.props_with_money(),
            id: non_empty(&self.identified_id),
            v: non_empty(&self.sdk_version),
            title: non_empty(&self.page_title),
            experiment: non_empty(&self.experiment),
            variant: non_empty(&self.variant),
            flags: self.flags.clone().map(|m| json!(m)),
            ..Payload::default()
        }
    }

    /// `props`, with the two money fields written into it when they were sent
    /// as their own columns.
    fn props_with_money(&self) -> Option<Json> {
        if self.revenue.is_none() && self.currency.trim().is_empty() {
            return self.props.clone();
        }
        let mut map = match self.props.clone() {
            Some(Json::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        if let Some(revenue) = self.revenue.filter(|v| v.is_finite()) {
            if let Some(number) = serde_json::Number::from_f64(revenue) {
                map.insert("revenue".to_string(), Json::Number(number));
            }
        }
        if !self.currency.trim().is_empty() {
            map.insert(
                "currency".to_string(),
                Json::String(self.currency.trim().to_string()),
            );
        }
        Some(Json::Object(map))
    }

    /// The event time, defaulting to now.
    fn timestamp_utc(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&self.timestamp)
            .ok()
            .map(|t| t.with_timezone(&Utc))
            .and_then(sane_time)
            .unwrap_or_else(Utc::now)
    }
}

/// One agent row in an edge payload.
///
/// Ten columns that could have been on this struct are not, and their absence is
/// the design: `agent_id`, `operator`, `purpose`, `verified`, `asn`, `asn_org`,
/// `country`, `ip_hash`, `robots_allowed` and `llms_listed` are all facts this
/// server establishes for itself, against the catalog, the operator's published
/// ranges and the site's own policy files. A shipper cannot do that work
/// honestly, so it is never offered the chance to guess.
#[derive(Debug, Default, Clone, Deserialize)]
struct WireAgentEvent {
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    user_agent: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    pathname: String,
    #[serde(default)]
    querystring: String,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    bytes: Option<u32>,
    #[serde(default)]
    duration_ms: Option<u32>,
    #[serde(default)]
    content_type: String,
    #[serde(default)]
    conditional: Option<u8>,
    /// Transport only: verified, resolved, hashed, dropped.
    #[serde(default)]
    ip: Option<String>,
}

impl WireAgentEvent {
    fn timestamp_utc(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&self.timestamp)
            .ok()
            .map(|t| t.with_timezone(&Utc))
            .and_then(sane_time)
            .unwrap_or_else(Utc::now)
    }
}

/// Reject a timestamp that cannot be real.
///
/// Two years ahead or before 2020: either is a broken clock, and honouring one
/// writes a row into a partition no report will ever look at.
fn sane_time(at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    let floor = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).single()?;
    if at > now + chrono::Duration::days(2) || at < floor {
        None
    } else {
        Some(at)
    }
}

/// Decode a batch, discarding entries that are not payload-shaped.
///
/// One bad row in a batch of fifty must not cost the other forty-nine.
fn decode_batch(items: Vec<Json>) -> Vec<Payload> {
    items
        .into_iter()
        .take(MAX_BATCH_ROWS)
        .filter_map(|item| serde_json::from_value::<Payload>(item).ok())
        .collect()
}

/// Rebuild a payload from the pixel's query string.
///
/// The tracker's image fallback encodes the same object with
/// `JSON.stringify` for anything that is not a scalar, so `p` and `e` arrive as
/// strings holding JSON and the numeric keys arrive as decimal text. Both are
/// coerced back here rather than by a lenient deserializer, because the coercion
/// is specific to this one transport.
fn payload_from_query(query: &str) -> Option<Payload> {
    const NUMERIC: &[&str] = &["s", "w", "h", "vw", "vh", "d", "sc"];
    const NESTED: &[&str] = &["p", "e", "flags"];

    let mut object = serde_json::Map::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()).take(64) {
        let key = key.to_string();
        let value = value.to_string();
        if NUMERIC.contains(&key.as_str()) {
            if let Ok(n) = value.parse::<u64>() {
                object.insert(key, Json::from(n));
            }
        } else if key == "sr" {
            // A sampling rate may be fractional (`12.5`), so it is not one of
            // the integer keys above.
            if let Some(n) = value
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
            {
                object.insert(key, Json::Number(n));
            }
        } else if NESTED.contains(&key.as_str()) {
            if let Ok(parsed) = serde_json::from_str::<Json>(&value) {
                object.insert(key, parsed);
            }
        } else {
            object.insert(key, Json::String(value));
        }
    }

    serde_json::from_value(Json::Object(object)).ok()
}

/// Accept a number that arrived as a JSON number *or* as a decimal string.
///
/// Hand-written integrations send `"s": "1"` often enough that refusing it would
/// only ever cost a customer their data, and a site id is unambiguous either way.
fn flexible_u32<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Json::deserialize(d)?;
    Ok(match value {
        Json::Number(n) => n.as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
        Json::String(s) => s.trim().parse::<u32>().unwrap_or(0),
        _ => 0,
    })
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A header value as `&str`, or `None` when absent or not UTF-8.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Set a response header, ignoring a value that cannot be one.
fn put(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(value) = value.parse() {
        headers.insert(name, value);
    }
}

/// Consume one unit against a rate-limit key.
///
/// A Redis that is down, or slower than [`LIMITER_BUDGET`], answers "allowed".
/// Turning a cache outage into a collection outage would be the worse failure,
/// and the limiter is here to bound a broken loop rather than to keep anyone out.
async fn allow(state: &AppState, key: &str) -> bool {
    allow_n(state, key, RATE_LIMIT_PER_MIN).await
}

/// [`allow`] with its own per-minute limit.
async fn allow_n(state: &AppState, key: &str, limit: u32) -> bool {
    match tokio::time::timeout(
        LIMITER_BUDGET,
        state.kv.rate_limit(key, limit, RATE_WINDOW_SECS),
    )
    .await
    {
        Ok(Ok(allowed)) => allowed,
        Ok(Err(_)) | Err(_) => true,
    }
}

/// Whether `key` has used up [`RATE_LIMIT_PER_MIN`] this window, without
/// consuming a unit. Fails open like [`allow`].
async fn exhausted(state: &AppState, key: &str) -> bool {
    matches!(
        tokio::time::timeout(
            LIMITER_BUDGET,
            state
                .kv
                .rate_exhausted(key, RATE_LIMIT_PER_MIN, RATE_WINDOW_SECS),
        )
        .await,
        Ok(Ok(true))
    )
}

/// The answer for a collection request that ran past its time budget, or
/// `None` for a path this module does not serve.
///
/// `middleware::timeout` asks here because the rule that a page never sees our
/// failure belongs to this module. The browser routes answer exactly as if the
/// hit had been taken. The server routes, the edge intake and the batch form
/// the server SDKs use, answer `503` with `Retry-After`: their callers retry,
/// and a `204` would tell them a batch arrived that did not.
pub(crate) fn timed_out(path: &str) -> Option<Response> {
    match path {
        "/api/track" => Some(no_content()),
        "/api/track/pixel.gif" => Some(gif()),
        "/api/log/edge" | "/api/track/batch" => {
            let mut res = api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "timeout",
                "the batch took too long to process; send it again shortly",
            );
            put(res.headers_mut(), "retry-after", "5");
            Some(res)
        }
        _ => None,
    }
}

/// The answer to an edge request that did not prove it holds the site's key.
///
/// One sentence for a missing signature, a wrong one, one outside its window and
/// a site that does not exist, so the response says nothing about which. An
/// exact repeat of a signature that did verify gets `409` instead: whoever sent
/// it already holds a valid request, so the distinction tells them nothing.
fn unauthorized() -> Response {
    api_error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "the signature did not verify for this site",
    )
}

/// `429`, with the `Retry-After` the server SDK backs off on.
fn too_many(message: &str) -> Response {
    let mut res = api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited", message);
    put(res.headers_mut(), "retry-after", "60");
    res
}

/// The empty answer every browser-facing handler gives.
fn no_content() -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    put(res.headers_mut(), "access-control-allow-origin", "*");
    put(res.headers_mut(), "cache-control", "no-store");
    res
}

/// The one visible failure: a hostname that is not this site's.
///
/// Identical for a site that does not exist, so the response cannot be used to
/// enumerate site ids.
fn forbidden() -> Response {
    let mut res = api_error(
        StatusCode::FORBIDDEN,
        "forbidden",
        "this site id does not accept hits from this hostname",
    );
    put(res.headers_mut(), "access-control-allow-origin", "*");
    res
}

/// The error envelope from `docs/CONTRACT.md` §3.3.
fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = json!({ "error": { "code": code, "message": message } });
    let mut res = (status, axum::Json(body)).into_response();
    put(res.headers_mut(), "cache-control", "no-store");
    res
}

/// The transparent GIF, never cached.
fn gif() -> Response {
    let mut res = PIXEL_GIF.into_response();
    let h = res.headers_mut();
    put(h, "content-type", "image/gif");
    put(
        h,
        "cache-control",
        "no-store, no-cache, must-revalidate, private",
    );
    put(h, "pragma", "no-cache");
    put(h, "access-control-allow-origin", "*");
    res
}

/// Copy the geographic columns onto a row.
fn apply_location(loc: &Location, asn: &mut u32, asn_org: &mut String, country: &mut [u8; 2]) {
    *asn = loc.asn;
    *asn_org = clamp(&loc.asn_org, MAX_TEXT);
    *country = country_code(&loc.country);
}

/// The serialised `props` document, plus the two money columns read out of it.
///
/// The browser payload has no revenue field — `docs/CONTRACT.md` §4 keeps the
/// beacon small — so a site reporting a purchase puts `revenue` and `currency`
/// in `props`, and this is where they become columns. They stay in `props` as
/// well, because a property that vanished when it happened to be named
/// `revenue` would be a surprise nobody asked for.
fn props_of(p: &Payload) -> (String, Option<i64>, String) {
    let Some(Json::Object(map)) = p.p.clone() else {
        return ("{}".to_string(), None, String::new());
    };

    let revenue = map
        .get("revenue")
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite())
        .map(decimal64_4);
    let currency = map
        .get("currency")
        .and_then(|v| v.as_str())
        .map(|s| clamp(s, 8).to_ascii_uppercase())
        .unwrap_or_default();

    let mut serialised = serde_json::to_string(&Json::Object(map)).unwrap_or_else(|_| "{}".into());
    if serialised.len() > MAX_PROPS {
        // Truncating JSON would produce a document ClickHouse cannot parse, so
        // an oversized bag is dropped whole and the fact is recorded instead.
        serialised = json!({ "_dropped": "props exceeded the size limit" }).to_string();
    }
    (serialised, revenue, currency)
}

/// The `sample_rate` column from the payload's `sr`.
///
/// Absent, zero, negative or not a number means the hit is not from a sample,
/// which is `100`. A rate is stored as a whole percentage, at least 1: a
/// tracker sampling at half a percent still sends hits, and a row that said
/// `0` would claim a sample that kept nobody.
fn sample_rate_of(sr: Option<f64>) -> u8 {
    match sr {
        Some(v) if v.is_finite() && v > 0.0 && v < 100.0 => v.round().clamp(1.0, 99.0) as u8,
        _ => 100,
    }
}

/// Feature-flag exposures as the `Map(String, String)` column wants them.
///
/// Accepts either `{"beta":"on"}` or `[["beta","on"]]`, because both shapes are
/// sent by real integrations and neither is worth losing a hit over.
fn flags_of(raw: Option<&Json>) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    match raw {
        Some(Json::Object(map)) => {
            for (key, value) in map.iter().take(32) {
                pairs.push((clamp(key, 64), clamp(&json_text(value), 64)));
            }
        }
        Some(Json::Array(items)) => {
            for item in items.iter().take(32) {
                if let Json::Array(pair) = item {
                    if let (Some(key), Some(value)) = (pair.first(), pair.get(1)) {
                        pairs.push((clamp(&json_text(key), 64), clamp(&json_text(value), 64)));
                    }
                }
            }
        }
        _ => {}
    }
    pairs
}

/// A JSON scalar as the string a `LowCardinality(String)` column stores.
///
/// A string keeps its own text rather than gaining a pair of quotes, which is
/// the difference between a flag variant reading `on` and reading `"on"` in
/// every breakdown that groups by it.
fn json_text(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// An HTTP method, upper-cased and bounded.
fn method_of(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "GET".to_string()
    } else {
        clamp(trimmed, 16).to_ascii_uppercase()
    }
}

/// A page URL, or `None` for anything that is not one.
///
/// Only `http` and `https` are accepted. A `file:` or `chrome-extension:` URL is
/// a developer's own machine, not a page anyone visited.
fn parse_page_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// Parse an address, tolerating a port and IPv6 brackets.
fn parse_ip(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(addr) = raw.parse::<IpAddr>() {
        return Some(addr);
    }
    if let Ok(socket) = raw.parse::<SocketAddr>() {
        return Some(socket.ip());
    }
    // `[2001:db8::1]` with no port.
    raw.strip_prefix('[')
        .and_then(|rest| rest.split(']').next())
        .and_then(|inner| inner.parse::<IpAddr>().ok())
}

/// Lowercase, drop a trailing dot, drop a leading `www.`.
///
/// The same normalisation `micaforge-enrich` applies to a referrer host, so a
/// site configured as `www.example.com` and a hit from `example.com` are one
/// site rather than two.
fn normalise_host(host: &str) -> String {
    referrer::normalise_host(host.trim())
}

/// `Some(value)` when the string has anything in it.
fn non_empty(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.trim().to_string())
}

/// Cut to `max` bytes without splitting a character.
fn clamp(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Clamp an optional string, defaulting to empty.
fn clamp_opt(s: &Option<String>, max: usize) -> String {
    s.as_deref().map(|v| clamp(v, max)).unwrap_or_default()
}

/// Narrow a reported pixel dimension into the column's width.
fn small(value: Option<u32>) -> u16 {
    value.unwrap_or(0).min(u32::from(u16::MAX)) as u16
}

/// Match a path against one exclusion glob.
///
/// `?` is one character, `*` is any run inside a single segment, and `**` is any
/// run at all including separators. The same three rules the tracker's own
/// `data-exclude` uses, so a pattern behaves identically whether it is enforced
/// in the page or here.
fn glob_match(pattern: &str, path: &str) -> bool {
    let p = pattern.as_bytes();
    let t = path.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    // Where to resume from if the current star turns out to be too short.
    let mut star: Option<(usize, usize, bool)> = None;

    while ti < t.len() {
        let matched = pi < p.len()
            && match p[pi] {
                b'*' => false,
                b'?' => t[ti] != b'/',
                c => c == t[ti],
            };

        if pi < p.len() && p[pi] == b'*' {
            let deep = pi + 1 < p.len() && p[pi + 1] == b'*';
            let after = pi + if deep { 2 } else { 1 };
            star = Some((after, ti, deep));
            pi = after;
            continue;
        }
        if matched {
            pi += 1;
            ti += 1;
            continue;
        }

        match star {
            // Extend the last star by one character and try again.
            Some((after, from, deep)) if from < t.len() && (deep || t[from] != b'/') => {
                star = Some((after, from + 1, deep));
                pi = after;
                ti = from + 1;
            }
            _ => return false,
        }
    }

    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(domain: &str, extra: &[&str]) -> SiteCtx {
        let mut hosts = vec![normalise_host(domain)];
        hosts.extend(extra.iter().map(|h| normalise_host(h)));
        SiteCtx {
            site_id: 1,
            uuid: Uuid::nil(),
            domain: normalise_host(domain),
            hosts,
            tz: chrono_tz::UTC,
            trailing_slash: true,
            respect_dnt: false,
            excluded: Vec::new(),
            guard: Guard::default(),
        }
    }

    #[test]
    fn only_the_sites_own_hostnames_may_write_to_it() {
        let s = site("example.com", &["docs.example.org"]);
        assert!(check_origin(&s, "example.com"));
        assert!(check_origin(&s, "www.example.com"), "www is the same site");
        assert!(
            check_origin(&s, "blog.example.com"),
            "a subdomain is the same site"
        );
        assert!(
            check_origin(&s, "docs.example.org"),
            "an extra domain is allowed"
        );
        // The whole point of the check.
        assert!(!check_origin(&s, "notexample.com"));
        assert!(!check_origin(&s, "example.com.evil.net"));
        assert!(!check_origin(&s, ""));
    }

    fn sign(key: &str, t: i64, body: &[u8]) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(format!("{t}.").as_bytes());
        mac.update(body);
        format!("t={t},v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn a_signature_covers_the_timestamp_and_the_exact_bytes() {
        let key = "mfi_secret";
        let body = br#"{"site_id":1}"#;
        let now = 1_787_306_400;
        let header = sign(key, now, body);

        let signed = verify_signature(key, body, &header, now).expect("a fresh signature verifies");
        assert_eq!(signed.t, now);
        // Still inside the window a few minutes later.
        assert!(verify_signature(key, body, &header, now + 299).is_some());

        // One byte different in the body.
        assert!(verify_signature(key, br#"{"site_id":2}"#, &header, now).is_none());
        // The right MAC under a fresh `t`: the replay this signature exists to stop.
        let v1 = header.split_once(",v1=").unwrap().1;
        assert!(
            verify_signature(key, body, &format!("t={},v1={v1}", now + 60), now + 60).is_none()
        );
        // The right signature, presented an hour later.
        assert!(verify_signature(key, body, &header, now + 3600).is_none());
        // A body-only MAC, the old scheme, no longer verifies.
        let mut old = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes()).unwrap();
        old.update(body);
        let old = format!("t={now},v1={}", hex::encode(old.finalize().into_bytes()));
        assert!(verify_signature(key, body, &old, now).is_none());
        // The wrong key.
        assert!(verify_signature("other", body, &header, now).is_none());
        // Nonsense.
        assert!(verify_signature(key, body, "garbage", now).is_none());
        assert!(verify_signature(key, body, &format!("t={now},v1=00"), now).is_none());
    }

    #[test]
    fn an_exact_repeat_of_a_signature_is_refused_inside_its_window() {
        let seen = Mutex::new(HashMap::new());
        let key = "mfi_secret";
        let now = 1_787_306_400;
        let first = verify_signature(key, b"{}", &sign(key, now, b"{}"), now).unwrap();
        assert!(first_use(&seen, &first, now));
        assert!(!first_use(&seen, &first, now + 1), "the same request again");
        assert!(!first_use(&seen, &first, now + SIGNATURE_WINDOW_SECS));

        // A fresh signature over the same body is a new send, not a replay.
        let again = verify_signature(key, b"{}", &sign(key, now + 1, b"{}"), now + 1).unwrap();
        assert!(first_use(&seen, &again, now + 1));

        // Once the window has passed, the entry can go: the signature can no
        // longer verify, so remembering it would only cost memory.
        assert!(first_use(&seen, &first, now + SIGNATURE_WINDOW_SECS + 1));
    }

    #[test]
    fn a_browser_payload_cannot_choose_its_own_time() {
        // A `timestamp` key on the public routes is accepted and ignored.
        let p: Payload = serde_json::from_value(json!({
            "s": 1,
            "u": "https://example.com/",
            "timestamp": "2021-01-01T00:00:00Z",
        }))
        .expect("an extra key does not fail the payload");
        let url = Url::parse("https://example.com/").unwrap();
        let before = Utc::now();
        let row = build_event_row(
            &site("example.com", &[]),
            &p,
            EventKind::Pageview,
            &url,
            "example.com",
            "/",
            "",
            None,
            None,
            None,
            &Location::default(),
        );
        assert!(row.timestamp >= before, "stamped with the receive time");

        // The signed edge path keeps the time the site reported.
        let event = WireEvent {
            timestamp: "2026-08-21T09:00:00Z".into(),
            ..WireEvent::default()
        };
        assert_eq!(
            event.timestamp_utc().to_rfc3339(),
            "2026-08-21T09:00:00+00:00"
        );
    }

    #[test]
    fn an_edge_event_with_browser_headers_is_a_human() {
        const CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                              AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";
        let bare: WireEvent = serde_json::from_value(json!({
            "hostname": "example.com",
            "user_agent": CHROME,
        }))
        .unwrap();
        let bare_class = micaforge_agents::classify(CHROME, &bare.header_view(), None);
        assert!(!bare_class.is_human(), "no headers: unchanged, still a bot");

        let full: WireEvent = serde_json::from_value(json!({
            "hostname": "example.com",
            "user_agent": CHROME,
            "accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            "accept_language": "en-GB,en;q=0.9",
            "sec_fetch_mode": "navigate",
            "sec_ch_ua": "\"Chromium\";v=\"128\", \"Google Chrome\";v=\"128\"",
        }))
        .unwrap();
        let full_class = micaforge_agents::classify(CHROME, &full.header_view(), None);
        assert!(full_class.is_human(), "Chrome with its headers is a person");

        let blank: WireEvent = serde_json::from_value(json!({
            "user_agent": CHROME,
            "accept": "",
            "accept_language": "",
            "sec_fetch_mode": "",
            "sec_ch_ua": "",
        }))
        .unwrap();
        assert!(
            !micaforge_agents::classify(CHROME, &blank.header_view(), None).is_human(),
            "empty strings count as absent"
        );
    }

    #[test]
    fn the_fallback_salt_is_random_per_site_and_day_and_expires() {
        let mut map = HashMap::new();
        let day = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let next = day.succ_opt().unwrap();

        let a = salt_from(&mut map, 1, day);
        assert_eq!(a.len(), 43, "32 random bytes, never empty");
        assert_eq!(a, salt_from(&mut map, 1, day), "stable within the day");
        assert_ne!(a, salt_from(&mut map, 2, day), "never shared between sites");
        let tomorrow = salt_from(&mut map, 1, next);
        assert_ne!(a, tomorrow, "rotates with the day");

        // Kept while yesterday is still being written, gone the day after.
        assert!(map.contains_key(&(1, day)));
        let _ = salt_from(&mut map, 1, next.succ_opt().unwrap());
        assert!(!map.contains_key(&(1, day)), "discarded after 48 hours");
        assert!(map.contains_key(&(1, next)));

        // The digest is never the unsalted one.
        let unsalted = ip_digest(b"", 1, "203.0.113.7");
        assert_ne!(ip_digest(&a, 1, "203.0.113.7"), unsalted);
    }

    #[test]
    fn the_fallback_salt_map_is_bounded() {
        let mut map = HashMap::new();
        let day = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        for (site, _) in (0..=u16::MAX).zip(0..MAX_FALLBACK_SALTS) {
            let _ = salt_from(&mut map, site, day);
        }
        assert_eq!(map.len(), MAX_FALLBACK_SALTS);
        // Past the bound a hit still gets a real salt; it is just not kept.
        let extra = salt_from(&mut map, u16::MAX, day);
        assert!(!extra.is_empty());
        assert_eq!(map.len(), MAX_FALLBACK_SALTS);
    }

    #[test]
    fn a_failed_lookup_serves_the_stale_answer_and_is_never_cached_as_absent() {
        let known = Arc::new(site("example.com", &[]));

        // Postgres down, site seen before: keep serving it.
        let (served, ttl) = settle(Fetched::Failed, Some(Some(Arc::clone(&known)))).unwrap();
        assert!(served.is_some());
        assert_eq!(ttl, SITE_CACHE_TTL);
        // Postgres down, site never seen: nothing to say, nothing cached.
        assert!(settle(Fetched::Failed, None).is_none());
        // Postgres down, id known to be absent: still absent.
        assert!(settle(Fetched::Failed, Some(None)).unwrap().0.is_none());
        // Postgres up and sure there is no such site: a short negative entry.
        let (absent, ttl) = settle(Fetched::Missing, Some(Some(known))).unwrap();
        assert!(absent.is_none());
        assert_eq!(ttl, NEGATIVE_TTL);
    }

    #[test]
    fn a_full_site_cache_sheds_probes_before_sites() {
        let now = Instant::now();
        let mut map = HashMap::new();
        let real = Arc::new(site("example.com", &[]));
        // A handful of real sites, some of them expired but still worth keeping
        // for stale-while-error.
        for id in 1..=10u16 {
            let until = if id % 2 == 0 {
                now
            } else {
                now + SITE_CACHE_TTL
            };
            map.insert(
                id,
                CachedSite {
                    until,
                    site: Some(Arc::clone(&real)),
                },
            );
        }
        // A probe fills the rest with live absences.
        let mut id = 11u16;
        while map.len() < SITE_CACHE_MAX {
            map.insert(
                id,
                CachedSite {
                    until: now + NEGATIVE_TTL,
                    site: None,
                },
            );
            id += 1;
        }

        make_room(&mut map, now);
        assert!(map.len() < SITE_CACHE_MAX, "room was made");
        assert!(
            map.len() >= SITE_CACHE_MAX - SITE_CACHE_MAX / 8,
            "not emptied"
        );
        for id in 1..=10u16 {
            assert!(map.contains_key(&id), "real site {id} survived the probe");
        }

        // Expired absences are the first to go, and are enough on their own.
        let mut map: HashMap<u16, CachedSite> = (0..SITE_CACHE_MAX as u16)
            .map(|id| {
                (
                    id,
                    CachedSite {
                        until: now,
                        site: None,
                    },
                )
            })
            .collect();
        map.insert(
            u16::MAX,
            CachedSite {
                until: now,
                site: Some(real),
            },
        );
        make_room(&mut map, now + Duration::from_secs(1));
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&u16::MAX));
    }

    #[test]
    fn the_edge_location_is_read_whole_and_validated() {
        let loc = parse_edge_location(
            Some("US"),
            Some("ca"),
            Some("California"),
            Some("San José"),
            Some("37.3394"),
            Some("-121.8950"),
        )
        .expect("a full set of headers is a location");
        assert_eq!(loc.country, "US");
        assert_eq!(loc.region, "US-CA");
        assert_eq!(loc.city, "San José");
        assert!((loc.lat - 37.3394).abs() < 1e-3 && (loc.lon + 121.895).abs() < 1e-3);

        // No region code: the name stands in, as the GeoIP reader does it.
        let loc = parse_edge_location(Some("GB"), None, Some("England"), None, None, None).unwrap();
        assert_eq!(loc.region, "England");
        assert_eq!((loc.lat, loc.lon), (0.0, 0.0));

        // Nonsense coordinates are dropped rather than plotted.
        let loc =
            parse_edge_location(Some("DE"), None, None, None, Some("91"), Some("10")).unwrap();
        assert_eq!((loc.lat, loc.lon), (0.0, 0.0));

        // Without a country there is no location at all.
        assert!(parse_edge_location(Some("XX"), Some("CA"), None, Some("x"), None, None).is_none());
    }

    #[test]
    fn only_a_real_country_code_from_the_edge_is_kept() {
        assert_eq!(parse_edge_country(Some("de")), Some("DE".to_string()));
        assert_eq!(parse_edge_country(Some(" US ")), Some("US".to_string()));
        // Cloudflare's markers for "unknown" and "Tor exit" are not countries.
        assert_eq!(parse_edge_country(Some("XX")), None);
        assert_eq!(parse_edge_country(Some("T1")), None);
        assert_eq!(parse_edge_country(Some("USA")), None);
        assert_eq!(parse_edge_country(Some("1A")), None);
        assert_eq!(parse_edge_country(None), None);
    }

    #[test]
    fn a_timed_out_beacon_still_gets_the_answer_a_page_expects() {
        assert_eq!(
            timed_out("/api/track").unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            timed_out("/api/track/pixel.gif").unwrap().status(),
            StatusCode::OK
        );
        for server_route in ["/api/log/edge", "/api/track/batch"] {
            let res = timed_out(server_route).unwrap();
            assert_eq!(
                res.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{server_route}"
            );
            assert!(res.headers().contains_key("retry-after"), "{server_route}");
        }
        assert!(timed_out("/api/stats/overview").is_none());
    }

    #[test]
    fn the_pixel_query_rebuilds_the_beacon_payload() {
        let query = "s=1&t=pageview&u=https%3A%2F%2Fexample.com%2Fdocs&p=%7B%22plan%22%3A%22pro%22%7D&sc=68";
        let p = payload_from_query(query).expect("a well-formed pixel query must parse");
        assert_eq!(p.s, 1);
        assert_eq!(p.kind(), Some(EventKind::Pageview));
        assert_eq!(p.u.as_deref(), Some("https://example.com/docs"));
        assert_eq!(p.sc, Some(68));
        assert_eq!(p.p.unwrap()["plan"], "pro");
    }

    #[test]
    fn an_unknown_event_kind_is_dropped_rather_than_counted() {
        let mut p = Payload {
            t: Some("pagevew".into()),
            ..Payload::default()
        };
        assert_eq!(p.kind(), None, "a typo must not inflate the pageview count");
        p.t = None;
        assert_eq!(p.kind(), Some(EventKind::Pageview));
    }

    #[test]
    fn revenue_is_lifted_out_of_props_without_leaving_it() {
        let p = Payload {
            p: Some(json!({ "revenue": 19.99, "currency": "eur", "plan": "pro" })),
            ..Payload::default()
        };
        let (props, revenue, currency) = props_of(&p);
        assert_eq!(revenue, Some(199_900));
        assert_eq!(currency, "EUR");
        assert!(props.contains("\"plan\""));
        assert!(
            props.contains("\"revenue\""),
            "the property is still a property"
        );
    }

    #[test]
    fn exclusion_globs_follow_the_trackers_rules() {
        assert!(glob_match("/admin/*", "/admin/users"));
        assert!(
            !glob_match("/admin/*", "/admin/users/7"),
            "one star is one segment"
        );
        assert!(glob_match("/admin/**", "/admin/users/7"));
        assert!(glob_match("/p?ge", "/page"));
        assert!(
            !glob_match("/p?ge", "/pa/ge"),
            "? never crosses a separator"
        );
        assert!(glob_match("**", "/anything/at/all"));
        assert!(!glob_match("/admin", "/administrator"));
        assert!(glob_match("/exact", "/exact"));
    }

    #[test]
    fn a_wild_timestamp_is_refused() {
        assert!(sane_time(Utc::now()).is_some());
        assert!(sane_time(Utc::now() + chrono::Duration::days(400)).is_none());
        assert!(sane_time(Utc.with_ymd_and_hms(1999, 1, 1, 0, 0, 0).unwrap()).is_none());
    }

    #[test]
    fn a_sampled_hit_carries_its_rate_and_everything_else_is_everyone() {
        assert_eq!(sample_rate_of(None), 100);
        assert_eq!(sample_rate_of(Some(25.0)), 25);
        assert_eq!(sample_rate_of(Some(12.5)), 13);
        assert_eq!(
            sample_rate_of(Some(0.4)),
            1,
            "a tiny sample is still a sample"
        );
        assert_eq!(sample_rate_of(Some(0.0)), 100);
        assert_eq!(sample_rate_of(Some(-5.0)), 100);
        assert_eq!(sample_rate_of(Some(100.0)), 100);
        assert_eq!(sample_rate_of(Some(f64::NAN)), 100);

        let p: Payload = serde_json::from_value(json!({ "s": 1, "sr": 25 })).unwrap();
        let url = Url::parse("https://example.com/").unwrap();
        let row = build_event_row(
            &site("example.com", &[]),
            &p,
            EventKind::Pageview,
            &url,
            "example.com",
            "/",
            "",
            None,
            None,
            None,
            &Location::default(),
        );
        assert_eq!(row.sample_rate, 25);
        assert_eq!(payload_from_query("s=1&sr=12.5").unwrap().sr, Some(12.5));
    }

    #[test]
    fn a_trait_named_revenue_is_never_revenue() {
        let p: Payload = serde_json::from_value(json!({
            "s": 1,
            "t": "identify",
            "id": "u_1",
            "p": { "revenue": 5000000, "currency": "usd", "email": "a@example.com" }
        }))
        .unwrap();
        let url = Url::parse("https://example.com/").unwrap();
        let row = build_event_row(
            &site("example.com", &[]),
            &p,
            EventKind::Identify,
            &url,
            "example.com",
            "/",
            "",
            None,
            None,
            None,
            &Location::default(),
        );
        assert_eq!(row.kind, "identify");
        assert_eq!(row.identified_id, "u_1");
        assert_eq!(row.revenue, None);
        assert!(row.currency.is_empty());
        assert!(
            row.props.contains("a@example.com"),
            "the traits are kept as props"
        );
    }

    #[test]
    fn an_error_fingerprint_ignores_the_line_number() {
        let a = error_fingerprint("TypeError", "x is not a function", "/app.js");
        let b = error_fingerprint("TypeError", "x is not a function", "/app.js");
        let c = error_fingerprint("TypeError", "y is not a function", "/app.js");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
