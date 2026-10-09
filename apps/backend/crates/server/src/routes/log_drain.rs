//! Collection: `POST /api/log/vercel/{drain}`, a Vercel log drain.
//!
//! A site hosted on Vercel has no access log to follow and often no server of
//! its own to put a middleware in, so neither the log shipper nor the
//! framework middlewares reach it. Vercel itself answered every crawler fetch,
//! though, and its log drains will post the request log of a project to any
//! HTTPS address. This route is that address. The customer creates a drain on
//! the site's settings screen ([`crate::routes::log_drains`]), pastes the URL
//! and the secret into Vercel, and from then on AI crawler fetches are recorded
//! with no change to the site's code.
//!
//! ## Trust
//!
//! The URL is not the credential; anybody who has seen it can post to it. The
//! secret is. Vercel signs every delivery with `x-vercel-signature`, the
//! lower-case hex HMAC-SHA1 of the raw body keyed with the drain's secret, and
//! the secret is one Micaforge minted and only the operator and Vercel hold.
//! The signature is checked in constant time against the bytes exactly as they
//! arrived, before a single byte is parsed. An unknown drain is checked against
//! an empty key, which cannot pass but costs the same, so a drain that does not
//! exist and a signature that is wrong get one `401` and one sentence after the
//! same work, the rule `POST /api/log/edge` follows for a site.
//!
//! A request with no signature at all is Vercel checking the endpoint: when a
//! drain is created or tested, and in the flows that verify URL ownership by
//! reading an `x-vercel-verify` header back. For a drain that exists it is
//! answered `200`, with that header when the operator saved a token, and
//! nothing in it is read or recorded ([`endpoint_check`]). It is not counted
//! as a delivery either, so a drain whose secret was removed in Vercel keeps
//! showing "no delivery received" on the settings screen instead of looking
//! healthy.
//!
//! Unlike the edge signature, Vercel's covers no timestamp, so a delivery
//! verifies for as long as the secret does. Deliveries travel over HTTPS from
//! Vercel to this server, so capturing one takes more than having seen the
//! URL; what is left is a delivery arriving twice, and [`deliveries`] stops
//! that: the digest of every verified delivery is remembered for ten minutes,
//! and an exact repeat is answered `200` without being recorded again.
//! [`requests`] does the same per request, by Vercel's `requestId`, which also
//! stops one request that logged several lines from being counted once per
//! line.
//!
//! ## What is recorded
//!
//! Only entries that describe a request, which are the ones with a `proxy`
//! section. Build output and the lines a function printed carry none and are
//! skipped. Of the requests, only `GET` and `HEAD` are kept, the log shipper's
//! default: a crawl is a read, and a form post in the crawler tables would make
//! every one of them wrong. Each request then goes through
//! [`ingest_server_hits`], the same classifier and the same tables as an edge
//! batch: a named agent lands in `agent_events` with its verification and
//! policy columns, an unnamed non-human client in `bot_events`, and a person is
//! skipped, because the tracker in the page already counted them with a session
//! a log line cannot give.
//!
//! ## Answers
//!
//! Every verified delivery is answered `200` with the counts, even when nothing
//! in it was usable, because Vercel marks a drain as failing on any other
//! status, and "a delivery of build logs" is not a failure. The rest: `401` as
//! above, `400` for a signed gzip body that will not decompress, `413` for a
//! body larger than any Vercel batch, `429` from the limiter, and `503` with
//! `Retry-After` when the database cannot be asked or the delivery ran past the
//! collection time budget ([`timed_out`]).
//!
//! No CORS header is written. The caller is Vercel's delivery service, never a
//! browser, and the route has nothing a page should be able to read.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::body::to_bytes;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, TimeZone, Utc};
use flate2::read::GzDecoder;
use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value as Json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;
use uuid::Uuid;

use micaforge_store::models::log_drains::LogDrain;
use micaforge_store::models::{log_drains as drains_model, sites as sites_model};

use crate::client_ip::{address_tag, client_ip};
use crate::middleware::{LogGate, LIMITER_BUDGET};
use crate::routes::ingest::{ingest_server_hits, HitTally, ServerHit};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Every drain URL starts with this.
const PATH_PREFIX: &str = "/api/log/vercel/";

/// The header Vercel signs a delivery with.
const SIGNATURE_HEADER: &str = "x-vercel-signature";

/// The header Vercel's URL ownership check reads the token back from.
const VERIFY_HEADER: &str = "x-vercel-verify";

/// Largest delivery accepted as sent.
///
/// Vercel documents batches of up to 1 MB. The cap is twice that, the same as
/// the edge route's, because Vercel documents no retry: a batch refused for a
/// few bytes over a rounded figure is lost, and the margin costs memory only
/// while one request is in flight.
const MAX_BODY: usize = 2 * 1024 * 1024;

/// Largest delivery once gzip is undone. Log lines compress tenfold or more,
/// so this is a bound against a decompression bomb rather than a batch size.
const MAX_INFLATED: usize = 8 * 1024 * 1024;

/// Most log entries read from one delivery. A megabyte of Vercel logs is a
/// few thousand entries; this only bounds a pathological body.
const MAX_ENTRIES: usize = 20_000;

/// Hits handed to [`ingest_server_hits`] per call.
///
/// That function caps the batch it is given, as the edge route caps its own,
/// and anything past the cap would be dropped without a count. A slice well
/// under the cap keeps every request of a large delivery; the per-call cost is
/// one cached salt and policy lookup.
const HITS_PER_CALL: usize = 100;

/// Deliveries per minute per drain, and per drain per sending address before
/// the signature is checked. Generous, like the edge limiter: this bounds a
/// loop, it does not police a busy site.
const RATE_LIMIT_PER_MIN: u32 = 600;

/// The rate-limit window.
const RATE_WINDOW_SECS: u32 = 60;

/// How long a verified delivery and a request id are remembered.
const REMEMBER_SECS: i64 = 600;

/// Most delivery digests remembered at once.
const MAX_DELIVERIES: usize = 16_384;

/// Most request ids remembered at once, across every drain in the process.
const MAX_REQUESTS: usize = 131_072;

/// `last_received_at` is written at most this often per drain.
const RECEIVED_EVERY_SECS: i64 = 60;

/// The methods kept. The log shipper's default, for the same reason.
const READ_METHODS: &[&str] = &["GET", "HEAD"];

// ---------------------------------------------------------------------------
// Router and the middleware's questions
// ---------------------------------------------------------------------------

/// The drain intake.
///
/// `GET` exists only for Vercel's endpoint check, in case a flow sends it as a
/// `GET`; see [`endpoint_check`].
pub fn router() -> Router<AppState> {
    Router::new().route("/api/log/vercel/{drain}", get(check).post(collect))
}

/// Is `path` a drain URL?
///
/// `middleware::is_collection_path` asks, so a delivery gets the collection
/// time budget and the quiet access-log level rather than the API's.
pub(crate) fn is_drain_path(path: &str) -> bool {
    path.strip_prefix(PATH_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
}

/// The answer for a delivery that ran past its time budget, or `None` for a
/// path this module does not serve.
///
/// `503` with `Retry-After`, never the `200` a page would be given: a delivery
/// that was cut short was not recorded, and saying otherwise would hide the
/// loss from the drain's status in Vercel.
pub(crate) fn timed_out(path: &str) -> Option<Response> {
    is_drain_path(path)
        .then(|| unavailable("the delivery took too long to process; send it again shortly"))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/log/vercel/{drain}` — Vercel's endpoint check.
async fn check(State(state): State<AppState>, Path(drain): Path<String>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let id = parse_drain_id(&drain);
    if !allow(&state, &probe_key(id, &parts)).await {
        return too_many();
    }
    match lookup(&state, id).await {
        Ok(row) => endpoint_check(row.as_ref()),
        Err(StoreDown) => store_down(),
    }
}

/// `POST /api/log/vercel/{drain}` — one delivery of request logs.
async fn collect(
    State(state): State<AppState>,
    Path(drain): Path<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let signature = header(&parts.headers, SIGNATURE_HEADER).map(str::to_string);
    let Ok(body) = to_bytes(body, MAX_BODY).await else {
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "bad_request",
            "the delivery is larger than a Vercel batch can be",
        );
    };

    // Keyed on the drain and the sending address together. Vercel delivers for
    // every customer from the same addresses, so a key on the address alone
    // would let one busy drain throttle every other; a key on the drain alone
    // would let anybody who has seen its URL throttle it.
    let id = parse_drain_id(&drain);
    if !allow(&state, &probe_key(id, &parts)).await {
        return too_many();
    }

    let Ok(row) = lookup(&state, id).await else {
        return store_down();
    };

    // Unsigned: Vercel checking the endpoint. Nothing in it is read or recorded.
    let Some(signature) = signature else {
        return endpoint_check(row.as_ref());
    };

    // The HMAC runs whether or not the drain exists, against an empty key when
    // it does not; the empty key's result is then thrown away, because a
    // stranger can compute it too.
    let secret = row.as_ref().and_then(|drain| open_secret(&state, drain));
    let key = secret.as_deref().unwrap_or("");
    let opened = open_delivery(key.as_bytes(), &body, &signature);
    let (drain, delivery) = match (row, secret.is_some(), opened) {
        (Some(drain), true, Opened::Verified(bytes)) => (drain, bytes),
        (Some(_), true, Opened::Unreadable) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "the delivery is gzip that does not decompress within the size limit",
            )
        }
        _ => return unauthorized(),
    };

    // Read fresh for the same reason the drain is: a domain added in settings
    // a moment ago should be accepted by the next delivery.
    let site = match sites_model::get(&state.pg, drain.site_id).await {
        Ok(site) => site,
        Err(e) => {
            static SITE: LogGate = LogGate::new();
            if SITE.open(Duration::from_secs(60)) {
                tracing::warn!(drain = %drain.id, error = %e, "a drain's site could not be read");
            }
            return store_down();
        }
    };

    if !allow(&state, &format!("drain:{}", drain.id)).await {
        return too_many();
    }

    let now = Utc::now();
    note_received(&state, &drain, now).await;

    // Claimed only now, after the limiter, so a delivery refused with `429`
    // is still recorded when it comes again.
    let digest = keyed_digest(b"delivery", drain.id, &delivery);
    if !deliveries().first_sight(digest, now.timestamp()) {
        return delivered(
            &drain,
            json!({ "duplicate": true, "entries": 0, "agents": 0, "bots": 0, "people": 0,
                    "rejected": 0, "skipped": 0, "repeats": 0 }),
        );
    }

    let batch = read_delivery(&delivery);
    let mut repeats = 0usize;
    let mut hits = Vec::with_capacity(batch.requests.len());
    for seen in batch.requests {
        let fresh = match &seen.id {
            Some(request_id) => requests().first_sight(
                keyed_digest(b"request", drain.id, request_id.as_bytes()),
                now.timestamp(),
            ),
            None => true,
        };
        if fresh {
            hits.push(seen.hit);
        } else {
            repeats += 1;
        }
    }

    let mut tally = HitTally::default();
    while !hits.is_empty() {
        let rest = hits.split_off(hits.len().min(HITS_PER_CALL));
        let part = std::mem::replace(&mut hits, rest);
        let counted = ingest_server_hits(&state, &site, part).await;
        tally.agents += counted.agents;
        tally.bots += counted.bots;
        tally.humans += counted.humans;
        tally.rejected += counted.rejected;
    }

    delivered(
        &drain,
        json!({
            "duplicate": false,
            "entries": batch.entries,
            "agents": tally.agents,
            "bots": tally.bots,
            "people": tally.humans,
            "rejected": tally.rejected,
            "skipped": batch.skipped,
            "repeats": repeats,
        }),
    )
}

// ---------------------------------------------------------------------------
// The drain and its secret
// ---------------------------------------------------------------------------

/// The path segment as a drain id, or `None` for one that cannot be.
fn parse_drain_id(raw: &str) -> Option<Uuid> {
    Uuid::parse_str(raw.trim()).ok()
}

/// The limiter key for a request whose signature has not been checked yet.
///
/// The drain part is the parsed id or a fixed word, never the raw segment, so
/// a caller cannot write arbitrary text into a Valkey key. The address part is
/// the keyed tag, never the address.
fn probe_key(id: Option<Uuid>, parts: &Parts) -> String {
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let caller = address_tag(client_ip(&parts.headers, peer));
    match id {
        Some(id) => format!("drain-ip:{id}:{caller}"),
        None => format!("drain-ip:none:{caller}"),
    }
}

/// Postgres could not be asked. Answered with [`store_down`], which says
/// nothing about whether the drain exists.
struct StoreDown;

/// Read the drain fresh from Postgres, never from a cache.
///
/// A rotated secret has to stop verifying on the next delivery, which is what
/// the rotate button promises.
async fn lookup(state: &AppState, id: Option<Uuid>) -> Result<Option<LogDrain>, StoreDown> {
    let Some(id) = id else {
        return Ok(None);
    };
    drains_model::find(&state.pg, id).await.map_err(|e| {
        static UNAVAILABLE: LogGate = LogGate::new();
        if UNAVAILABLE.open(Duration::from_secs(60)) {
            tracing::warn!(error = %e, "a log drain could not be read");
        }
        StoreDown
    })
}

/// The drain's plaintext secret, or `None` when the stored form will not
/// open. Only the drain id is logged, never anything derived from the secret.
fn open_secret(state: &AppState, drain: &LogDrain) -> Option<String> {
    match crate::routes::log_drains::drain_secret(&state.cfg, drain) {
        Ok(secret) => Some(secret),
        Err(e) => {
            tracing::warn!(drain = %drain.id, error = %e, "a log drain's secret could not be opened");
            None
        }
    }
}

/// The answer to an unsigned request: Vercel checking the endpoint.
///
/// `200` for a drain that exists, with the ownership token in
/// `x-vercel-verify` when the operator saved one, because Vercel will not
/// create a drain whose test it cannot pass, and some of its flows send that
/// test unsigned. An id that names no drain gets the same `401` as a bad
/// signature. The answer says how much was recorded, which is nothing, so an
/// operator reading it in Vercel is not misled.
fn endpoint_check(drain: Option<&LogDrain>) -> Response {
    let Some(drain) = drain else {
        return unauthorized();
    };
    delivered(
        drain,
        json!({
            "recorded": 0,
            "note": "unsigned requests are answered and never recorded; set the drain's signature secret in Vercel",
        }),
    )
}

/// Record that a verified delivery arrived, at most once a minute per drain.
///
/// The row was just read, so the minute is checked here first and most
/// deliveries cost no write at all. Best effort: a failure is a stale line on
/// the settings screen, never a refused delivery.
async fn note_received(state: &AppState, drain: &LogDrain, now: DateTime<Utc>) {
    let due = drain
        .last_received_at
        .is_none_or(|at| (now - at).num_seconds() >= RECEIVED_EVERY_SECS);
    if !due {
        return;
    }
    if let Err(e) = drains_model::mark_received(&state.pg, drain.id).await {
        static FAILED: LogGate = LogGate::new();
        if FAILED.open(Duration::from_secs(60)) {
            tracing::warn!(drain = %drain.id, error = %e, "could not record a drain delivery");
        }
    }
}

// ---------------------------------------------------------------------------
// The signature
// ---------------------------------------------------------------------------

/// What checking a delivery's signature produced.
enum Opened<'a> {
    /// The signature verified. The bytes are the delivery, decompressed when
    /// it arrived as gzip.
    Verified(Cow<'a, [u8]>),
    /// The signature verified over gzip that will not decompress within
    /// [`MAX_INFLATED`].
    Unreadable,
    /// The signature did not verify.
    Refused,
}

/// Check the signature, and decompress the body if Vercel compressed it.
///
/// Vercel can deliver gzip, and documents the signature only as "a hash of the
/// payload body". The bytes as sent are tried first, which is what an HTTP
/// signature normally covers; then, for a gzip body only, the decompressed
/// bytes. Both attempts run for an unknown drain as well, so the extra work
/// says nothing about which kind of failure a `401` was. Decompression of an
/// unverified body is bounded by [`MAX_INFLATED`] and by the limiter in front.
fn open_delivery<'a>(key: &[u8], body: &'a [u8], signature: &str) -> Opened<'a> {
    let gzip = is_gzip(body);
    if signature_matches(key, body, signature) {
        if !gzip {
            return Opened::Verified(Cow::Borrowed(body));
        }
        return match inflate(body) {
            Some(plain) => Opened::Verified(Cow::Owned(plain)),
            None => Opened::Unreadable,
        };
    }
    if gzip {
        if let Some(plain) = inflate(body) {
            if signature_matches(key, &plain, signature) {
                return Opened::Verified(Cow::Owned(plain));
            }
        }
    }
    Opened::Refused
}

/// Constant-time check of `x-vercel-signature` over `body`.
///
/// The expected MAC is always computed, whatever the header holds, and the
/// comparison is `subtle`'s: a byte-at-a-time compare on an HMAC is a forgery
/// oracle. A header that is not hex, or not twenty bytes, compares as a
/// mismatch of the same cost.
fn signature_matches(key: &[u8], body: &[u8], header_value: &str) -> bool {
    let provided = hex::decode(header_value.trim()).unwrap_or_default();
    let Ok(mut mac) = <Hmac<Sha1> as Mac>::new_from_slice(key) else {
        return false;
    };
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    bool::from(expected.as_slice().ct_eq(provided.as_slice()))
}

/// Does the body start with the gzip magic number? JSON never can.
fn is_gzip(body: &[u8]) -> bool {
    body.starts_with(&[0x1f, 0x8b])
}

/// Decompress gzip, or `None` past [`MAX_INFLATED`] or on a corrupt stream.
fn inflate(body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let limit = u64::try_from(MAX_INFLATED).unwrap_or(u64::MAX) + 1;
    GzDecoder::new(body)
        .take(limit)
        .read_to_end(&mut out)
        .ok()?;
    (out.len() <= MAX_INFLATED).then_some(out)
}

// ---------------------------------------------------------------------------
// Replay and repeat guards
// ---------------------------------------------------------------------------

/// Digests seen recently, each remembered until its own expiry.
///
/// Process-local, like the edge path's signature guard, which is the shape
/// `PRODUCT.md` ships. Bounded: when it is full of live entries the eighth
/// closest to expiry is forgotten, which can only let through a repeat that
/// was about to be let through anyway.
struct Recent {
    cap: usize,
    seen: Mutex<HashMap<[u8; 32], i64>>,
}

impl Recent {
    fn new(cap: usize) -> Recent {
        Recent {
            cap,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// `true` the first time `key` is offered inside [`REMEMBER_SECS`].
    fn first_sight(&self, key: [u8; 32], now: i64) -> bool {
        let mut map = self
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if map.get(&key).is_some_and(|until| *until > now) {
            return false;
        }
        if map.len() >= self.cap {
            map.retain(|_, until| *until > now);
        }
        if map.len() >= self.cap {
            let mut by_expiry: Vec<(i64, [u8; 32])> = map.iter().map(|(k, v)| (*v, *k)).collect();
            by_expiry.sort_unstable();
            for (_, stale) in by_expiry.into_iter().take((self.cap / 8).max(1)) {
                map.remove(&stale);
            }
        }
        map.insert(key, now + REMEMBER_SECS);
        true
    }
}

/// Verified deliveries, by digest.
fn deliveries() -> &'static Recent {
    static SEEN: OnceLock<Recent> = OnceLock::new();
    SEEN.get_or_init(|| Recent::new(MAX_DELIVERIES))
}

/// Recorded requests, by Vercel request id.
fn requests() -> &'static Recent {
    static SEEN: OnceLock<Recent> = OnceLock::new();
    SEEN.get_or_init(|| Recent::new(MAX_REQUESTS))
}

/// SHA-256 over a kind label, the drain id and the material. The label keeps a
/// request id from ever colliding with a delivery body, and the drain id keeps
/// two drains from sharing an entry.
fn keyed_digest(kind: &[u8], drain: Uuid, material: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(kind)
        .chain_update(b":")
        .chain_update(drain.as_bytes())
        .chain_update(material)
        .finalize()
        .into()
}

// ---------------------------------------------------------------------------
// Reading a delivery
// ---------------------------------------------------------------------------

/// One request a delivery reported, with Vercel's id for it when present.
#[derive(Debug)]
struct Seen {
    id: Option<String>,
    hit: ServerHit,
}

/// What one delivery held.
#[derive(Debug, Default)]
struct Batch {
    /// Log entries read, up to [`MAX_ENTRIES`].
    entries: usize,
    /// Entries that were not a usable request: no `proxy` section, a method
    /// other than `GET` and `HEAD`, a background revalidation, no host.
    skipped: usize,
    requests: Vec<Seen>,
}

impl Batch {
    /// Take one top-level JSON value: an array of entries or one entry.
    fn take(&mut self, value: Json) {
        match value {
            Json::Array(items) => items.into_iter().for_each(|item| self.take_entry(item)),
            entry @ Json::Object(_) => self.take_entry(entry),
            _ => {}
        }
    }

    fn take_entry(&mut self, entry: Json) {
        // Every Vercel log entry is an object; anything else in an array is not
        // an entry at all, so it is neither counted nor skipped.
        if self.entries >= MAX_ENTRIES || !entry.is_object() {
            return;
        }
        self.entries += 1;
        match request_of(&entry) {
            Some(seen) => self.requests.push(seen),
            None => self.skipped += 1,
        }
    }
}

/// Read a delivery in any of the shapes Vercel sends.
///
/// The format is told by the content, not the `content-type`: Vercel's JSON
/// format is one array, its NDJSON format is one object per line, and a test
/// delivery can be a single object. One streaming pass reads all three, since
/// each is a sequence of JSON values separated by whitespace. Only when that
/// pass meets a malformed value does it start again line by line, so one bad
/// line in an NDJSON delivery costs that line and not the rest.
fn read_delivery(body: &[u8]) -> Batch {
    let mut batch = Batch::default();
    let clean = serde_json::Deserializer::from_slice(body)
        .into_iter::<Json>()
        .all(|value| match value {
            Ok(value) => {
                batch.take(value);
                true
            }
            Err(_) => false,
        });
    if clean {
        return batch;
    }

    let mut batch = Batch::default();
    for line in body.split(|byte| *byte == b'\n') {
        if let Ok(value) = serde_json::from_slice::<Json>(line.trim_ascii()) {
            batch.take(value);
        }
    }
    batch
}

/// The request one log entry describes, or `None` when it is not one to keep.
///
/// Every field is read leniently, because one entry of an unexpected shape
/// must not cost the rest of the delivery. Only the host is required: without
/// it there is no site to check the request against.
fn request_of(entry: &Json) -> Option<Seen> {
    let proxy = entry.get("proxy")?.as_object()?;

    let method = text(proxy, "method")?.to_ascii_uppercase();
    if !READ_METHODS.contains(&method.as_str()) {
        return None;
    }

    // Vercel documents `-1` as a revalidation in the background: Vercel
    // refreshing its own cache, with no status any client was answered with.
    // Counting it could count one fetch twice, and an inflated crawl count is
    // the one error this product cannot make.
    let status = proxy.get("statusCode").and_then(Json::as_i64);
    if status == Some(-1) {
        return None;
    }

    let hostname = text(proxy, "host")?.to_string();
    let (pathname, querystring) = split_path(text(proxy, "path").unwrap_or("/"));

    Some(Seen {
        id: entry
            .get("requestId")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        hit: ServerHit {
            timestamp: millis(proxy.get("timestamp")).or_else(|| millis(entry.get("timestamp"))),
            user_agent: user_agent_of(proxy.get("userAgent")),
            method,
            hostname,
            pathname,
            querystring,
            status: status
                .and_then(|s| u16::try_from(s).ok())
                .filter(|s| (100..=599).contains(s)),
            bytes: proxy
                .get("responseByteSize")
                .and_then(Json::as_u64)
                .map(|b| u32::try_from(b).unwrap_or(u32::MAX)),
            duration_ms: None,
            content_type: String::new(),
            ip: text(proxy, "clientIp").map(str::to_string),
        },
    })
}

/// A non-empty string field, trimmed.
fn text<'a>(object: &'a Map<String, Json>, key: &str) -> Option<&'a str> {
    object
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// `proxy.userAgent` is documented as an array of strings; the first non-empty
/// one is the request's. A plain string is accepted too.
fn user_agent_of(value: Option<&Json>) -> String {
    let first = match value {
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(Json::as_str)
            .map(str::trim)
            .find(|ua| !ua.is_empty()),
        Some(Json::String(ua)) => Some(ua.trim()),
        _ => None,
    };
    first.unwrap_or_default().to_string()
}

/// Split `proxy.path`, "request path with query parameters", into the path and
/// the query without its `?`. A full URL is reduced to the same two parts.
fn split_path(raw: &str) -> (String, String) {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        if let Ok(url) = Url::parse(raw) {
            return (
                url.path().to_string(),
                url.query().unwrap_or_default().to_string(),
            );
        }
    }
    let without_fragment = raw.split('#').next().unwrap_or_default();
    match without_fragment.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (without_fragment.to_string(), String::new()),
    }
}

/// A Unix time in milliseconds, as Vercel writes every timestamp.
fn millis(value: Option<&Json>) -> Option<DateTime<Utc>> {
    let ms = match value? {
        Json::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?,
        _ => return None,
    };
    Utc.timestamp_millis_opt(ms).single()
}

// ---------------------------------------------------------------------------
// Small helpers and answers
// ---------------------------------------------------------------------------

/// A header value as `&str`, or `None` when absent, empty or not UTF-8.
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
/// A Valkey that is down, or slower than [`LIMITER_BUDGET`], answers
/// "allowed": a cache outage must not become a collection outage, and the
/// limiter bounds a loop rather than keeping anyone out.
async fn allow(state: &AppState, key: &str) -> bool {
    match tokio::time::timeout(
        LIMITER_BUDGET,
        state
            .kv
            .rate_limit(key, RATE_LIMIT_PER_MIN, RATE_WINDOW_SECS),
    )
    .await
    {
        Ok(Ok(allowed)) => allowed,
        Ok(Err(_)) | Err(_) => true,
    }
}

/// The `200` for a verified delivery, with the ownership token when the drain
/// has one, so the check also passes against an ordinary delivery.
fn delivered(drain: &LogDrain, counts: Json) -> Response {
    let mut res = crate::response::ok(counts).into_response();
    put(res.headers_mut(), "cache-control", "no-store");
    if let Some(token) = drain.verify_token.as_deref() {
        put(res.headers_mut(), VERIFY_HEADER, token);
    }
    res
}

/// One answer for an unknown drain, a missing or wrong signature, and an
/// unsigned request to a drain with no ownership token.
fn unauthorized() -> Response {
    api_error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "the signature did not verify for this drain",
    )
}

/// `429`, with the `Retry-After` a sender backs off on.
fn too_many() -> Response {
    let mut res = api_error(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "too many deliveries for this drain",
    );
    put(res.headers_mut(), "retry-after", "60");
    res
}

/// `503`, with `Retry-After`.
fn unavailable(message: &str) -> Response {
    let mut res = api_error(StatusCode::SERVICE_UNAVAILABLE, "timeout", message);
    put(res.headers_mut(), "retry-after", "5");
    res
}

/// The `503` for a delivery Postgres could not be asked about.
fn store_down() -> Response {
    unavailable("the delivery could not be processed now; send it again shortly")
}

/// The error envelope from `docs/CONTRACT.md` §3.3.
fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = json!({ "error": { "code": code, "message": message } });
    let mut res = (status, axum::Json(body)).into_response();
    put(res.headers_mut(), "cache-control", "no-store");
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use axum::http::Method;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    /// A body shaped like Vercel's documented JSON format, built by hand.
    const VECTOR_BODY: &str = r#"[{"id":"1","source":"static","proxy":{"method":"GET","host":"docs.example.com","path":"/guide","userAgent":["GPTBot/1.2"]}}]"#;

    /// The secret the vector was signed with.
    const VECTOR_SECRET: &str = "mfd_test-secret";

    /// `printf %s "$VECTOR_BODY" | openssl dgst -sha1 -hmac "$VECTOR_SECRET"`,
    /// computed outside this crate and cross-checked with Python's `hmac`.
    const VECTOR_SIGNATURE: &str = "be0c172f4d4f65c3d49c9cb5a92c5b0fdde2f23b";

    /// The documented one: RFC 2104 HMAC-SHA1 with key `key` over "The quick
    /// brown fox jumps over the lazy dog".
    const FOX_SIGNATURE: &str = "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9";

    /// Vercel's documented JSON example, abridged only in the user agent.
    const DOC_JSON: &str = r#"[
  { "id": "1573817187330377061717300000", "deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD", "source": "build", "host": "my-app-abc123.vercel.app", "timestamp": 1573817187330, "projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU", "level": "info", "message": "Build completed successfully", "buildId": "bld_cotnkcr76", "type": "stdout", "projectName": "my-app" },
  { "id": "1573817250283254651097202070", "deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD", "source": "lambda", "host": "my-app-abc123.vercel.app", "timestamp": 1573817250283, "projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU", "level": "info", "message": "API request processed", "entrypoint": "api/index.js", "requestId": "643af4e3-975a-4cc7-9e7a-1eda11539d90", "statusCode": 200, "path": "/api/users", "executionRegion": "sfo1", "environment": "production", "traceId": "1b02cd14bb8642fd092bc23f54c7ffcd", "spanId": "f24e8631bd11faa7", "trace.id": "1b02cd14bb8642fd092bc23f54c7ffcd", "span.id": "f24e8631bd11faa7", "proxy": { "timestamp": 1573817250172, "method": "GET", "host": "my-app.vercel.app", "path": "/api/users?page=1", "userAgent": ["Mozilla/5.0 (compatible; GPTBot/1.2; +https://openai.com/gptbot)"], "referer": "https://my-app.vercel.app", "region": "sfo1", "statusCode": 200, "clientIp": "120.75.16.101", "scheme": "https", "vercelCache": "MISS", "responseByteSize": 1024 } }
]"#;

    /// The same two entries in Vercel's documented NDJSON format.
    const DOC_NDJSON: &str = concat!(
        r#"{"id": "1573817187330377061717300000","deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD","source": "build","host": "my-app-abc123.vercel.app","timestamp": 1573817187330,"projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU","level": "info","message": "Build completed successfully","buildId": "bld_cotnkcr76","type": "stdout","projectName": "my-app"}"#,
        "\n",
        r#"{"id": "1573817250283254651097202070","deploymentId": "dpl_233NRGRjVZX1caZrXWtz5g1TAksD","source": "lambda","host": "my-app-abc123.vercel.app","timestamp": 1573817250283,"projectId": "gdufoJxB6b9b1fEqr1jUtFkyavUU","level": "info","message": "API request processed","entrypoint": "api/index.js","requestId": "643af4e3-975a-4cc7-9e7a-1eda11539d90","statusCode": 200,"path": "/api/users","executionRegion": "sfo1","environment": "production","proxy": {"timestamp": 1573817250172,"method": "GET","host": "my-app.vercel.app","path": "/api/users?page=1","userAgent": ["Mozilla/5.0 (compatible; GPTBot/1.2; +https://openai.com/gptbot)"],"referer": "https://my-app.vercel.app","region": "sfo1","statusCode": 200,"clientIp": "120.75.16.101","scheme": "https","vercelCache": "MISS","responseByteSize": 1024}}"#,
        "\n"
    );

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn sign(key: &str, body: &[u8]) -> String {
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn the_signature_matches_vectors_computed_elsewhere() {
        assert!(signature_matches(
            VECTOR_SECRET.as_bytes(),
            VECTOR_BODY.as_bytes(),
            VECTOR_SIGNATURE
        ));
        assert!(signature_matches(
            b"key",
            b"The quick brown fox jumps over the lazy dog",
            FOX_SIGNATURE
        ));
        // Hex case and surrounding space are the header's, not the MAC's.
        assert!(signature_matches(
            VECTOR_SECRET.as_bytes(),
            VECTOR_BODY.as_bytes(),
            &format!(" {} ", VECTOR_SIGNATURE.to_uppercase())
        ));
    }

    #[test]
    fn a_signature_covers_the_exact_bytes_and_the_exact_key() {
        let body = VECTOR_BODY.as_bytes();
        // One byte of the body changed.
        let mut altered = body.to_vec();
        altered[10] ^= 1;
        assert!(!signature_matches(
            VECTOR_SECRET.as_bytes(),
            &altered,
            VECTOR_SIGNATURE
        ));
        // A different secret.
        assert!(!signature_matches(b"mfd_other", body, VECTOR_SIGNATURE));
        // An unknown drain verifies against the empty key, and fails.
        assert!(!signature_matches(b"", body, VECTOR_SIGNATURE));
        // Not hex, too short, too long, empty: all a plain mismatch.
        let too_long = format!("{VECTOR_SIGNATURE}00");
        for bad in ["zz", &VECTOR_SIGNATURE[..38], too_long.as_str(), ""] {
            assert!(
                !signature_matches(VECTOR_SECRET.as_bytes(), body, bad),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_gzip_delivery_verifies_over_either_form_and_is_decompressed() {
        let plain = DOC_NDJSON.as_bytes();
        let packed = gzip(plain);

        // Signed over the bytes as sent.
        let signature = sign(VECTOR_SECRET, &packed);
        match open_delivery(VECTOR_SECRET.as_bytes(), &packed, &signature) {
            Opened::Verified(bytes) => assert_eq!(bytes.as_ref(), plain),
            _ => panic!("a signature over the gzip bytes must verify"),
        }

        // Signed over the decompressed bytes.
        let signature = sign(VECTOR_SECRET, plain);
        match open_delivery(VECTOR_SECRET.as_bytes(), &packed, &signature) {
            Opened::Verified(bytes) => assert_eq!(bytes.as_ref(), plain),
            _ => panic!("a signature over the decompressed bytes must verify"),
        }

        // A plain body is borrowed, not copied.
        let signature = sign(VECTOR_SECRET, plain);
        assert!(matches!(
            open_delivery(VECTOR_SECRET.as_bytes(), plain, &signature),
            Opened::Verified(Cow::Borrowed(_))
        ));

        // The wrong key is refused in both forms.
        assert!(matches!(
            open_delivery(b"mfd_other", &packed, &signature),
            Opened::Refused
        ));
    }

    #[test]
    fn a_decompression_bomb_is_unreadable_not_read() {
        let bomb = gzip(&vec![b' '; MAX_INFLATED + 1]);
        assert!(
            bomb.len() < MAX_BODY,
            "the test needs a body the limit admits"
        );
        assert!(inflate(&bomb).is_none());
        let signature = sign(VECTOR_SECRET, &bomb);
        assert!(matches!(
            open_delivery(VECTOR_SECRET.as_bytes(), &bomb, &signature),
            Opened::Unreadable
        ));
        assert_eq!(inflate(&gzip(b"[]")).as_deref(), Some(b"[]".as_slice()));
    }

    #[test]
    fn the_documented_json_and_ndjson_examples_read_the_same() {
        for body in [DOC_JSON, DOC_NDJSON] {
            let batch = read_delivery(body.as_bytes());
            assert_eq!(batch.entries, 2, "{body}");
            assert_eq!(batch.skipped, 1, "the build line has no request");
            assert_eq!(batch.requests.len(), 1);
        }
    }

    #[test]
    fn a_single_object_and_a_bad_ndjson_line_are_both_read() {
        let entry = r#"{"requestId":"r1","proxy":{"method":"GET","host":"a.example","path":"/x","userAgent":["ClaudeBot/1.0"]}}"#;

        let batch = read_delivery(entry.as_bytes());
        assert_eq!((batch.entries, batch.requests.len()), (1, 1));

        // A pretty-printed single object spans lines and still reads whole.
        let pretty =
            serde_json::to_string_pretty(&serde_json::from_str::<Json>(entry).unwrap()).unwrap();
        assert_eq!(read_delivery(pretty.as_bytes()).requests.len(), 1);

        // One broken line costs that line only.
        let body = format!("{entry}\n{{\"proxy\": {{ broken\n{entry}\n");
        let batch = read_delivery(body.as_bytes());
        assert_eq!(batch.requests.len(), 2);

        // Nothing usable at all is an empty batch, not an error.
        assert_eq!(read_delivery(b"").entries, 0);
        assert_eq!(read_delivery(b"not json").entries, 0);
        assert_eq!(read_delivery(b"[1, \"two\", null]").entries, 0);
    }

    #[test]
    fn every_field_of_a_request_entry_is_mapped() {
        let batch = read_delivery(DOC_JSON.as_bytes());
        let seen = &batch.requests[0];
        assert_eq!(
            seen.id.as_deref(),
            Some("643af4e3-975a-4cc7-9e7a-1eda11539d90")
        );

        let hit = &seen.hit;
        assert_eq!(
            hit.timestamp,
            Utc.timestamp_millis_opt(1_573_817_250_172).single(),
            "the proxy's time, not the log line's"
        );
        assert_eq!(
            hit.user_agent,
            "Mozilla/5.0 (compatible; GPTBot/1.2; +https://openai.com/gptbot)"
        );
        assert_eq!(hit.method, "GET");
        assert_eq!(
            hit.hostname, "my-app.vercel.app",
            "the request's host, not the deployment's"
        );
        assert_eq!(hit.pathname, "/api/users");
        assert_eq!(hit.querystring, "page=1");
        assert_eq!(hit.status, Some(200));
        assert_eq!(hit.bytes, Some(1024));
        assert_eq!(hit.duration_ms, None);
        assert_eq!(hit.content_type, "");
        assert_eq!(hit.ip.as_deref(), Some("120.75.16.101"));
    }

    #[test]
    fn only_reads_are_kept_and_revalidations_are_not_requests() {
        let entry = |method: &str, status: i64| {
            json!({ "proxy": { "method": method, "host": "a.example", "path": "/",
                               "userAgent": ["GPTBot/1.2"], "statusCode": status } })
        };
        assert!(request_of(&entry("GET", 200)).is_some());
        assert!(
            request_of(&entry("head", 304)).is_some(),
            "method case is not meaning"
        );
        for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            assert!(request_of(&entry(method, 200)).is_none(), "{method}");
        }
        assert!(request_of(&entry("GET", -1)).is_none());

        // A status that is not an HTTP status is unknown, not a reason to drop.
        let odd = request_of(&entry("GET", 0)).unwrap();
        assert_eq!(odd.hit.status, None);
    }

    #[test]
    fn entries_without_a_usable_request_are_skipped() {
        for entry in [
            json!({ "source": "build", "type": "stdout", "message": "compiled" }),
            json!({ "source": "lambda", "type": "stderr", "message": "boom", "proxy": null }),
            json!({ "proxy": "GET /" }),
            json!({ "proxy": { "host": "a.example", "path": "/" } }),
            json!({ "proxy": { "method": "GET", "path": "/" } }),
            json!({ "proxy": { "method": "GET", "host": "  ", "path": "/" } }),
        ] {
            assert!(request_of(&entry).is_none(), "{entry}");
        }
    }

    #[test]
    fn the_lenient_fields_read_what_they_can() {
        let seen = request_of(&json!({
            "timestamp": 1_573_817_187_330_i64,
            "proxy": { "method": "GET", "host": "a.example",
                       "path": "https://a.example/p/q?x=1#frag",
                       "userAgent": "PerplexityBot/1.0",
                       "responseByteSize": 99_999_999_999_i64,
                       "timestamp": "not a number" }
        }))
        .unwrap();
        assert_eq!(seen.id, None);
        assert_eq!(seen.hit.user_agent, "PerplexityBot/1.0");
        assert_eq!(
            (seen.hit.pathname.as_str(), seen.hit.querystring.as_str()),
            ("/p/q", "x=1")
        );
        assert_eq!(seen.hit.bytes, Some(u32::MAX));
        assert_eq!(
            seen.hit.timestamp,
            Utc.timestamp_millis_opt(1_573_817_187_330).single(),
            "the log line's time when the proxy's is unreadable"
        );

        assert_eq!(user_agent_of(Some(&json!(["", "  ", "Bot/1"]))), "Bot/1");
        assert_eq!(user_agent_of(Some(&json!([]))), "");
        assert_eq!(user_agent_of(None), "");
        assert_eq!(split_path("/a?b=1&c=2"), ("/a".into(), "b=1&c=2".into()));
        assert_eq!(split_path("/a#top"), ("/a".into(), String::new()));
        assert_eq!(split_path(""), (String::new(), String::new()));
    }

    #[test]
    fn a_repeat_is_refused_inside_its_window_and_only_there() {
        let recent = Recent::new(64);
        let drain = Uuid::now_v7();
        let key = keyed_digest(b"delivery", drain, VECTOR_BODY.as_bytes());
        assert!(recent.first_sight(key, 1_000));
        assert!(!recent.first_sight(key, 1_000));
        assert!(!recent.first_sight(key, 1_000 + REMEMBER_SECS - 1));
        assert!(
            recent.first_sight(key, 1_000 + REMEMBER_SECS),
            "remembered ten minutes, not for ever"
        );

        // The same body on another drain, and a request id equal to the body,
        // are different entries.
        assert!(recent.first_sight(
            keyed_digest(b"delivery", Uuid::now_v7(), VECTOR_BODY.as_bytes()),
            1_000
        ));
        assert!(recent.first_sight(
            keyed_digest(b"request", drain, VECTOR_BODY.as_bytes()),
            1_000
        ));
    }

    #[test]
    fn the_repeat_memory_is_bounded() {
        let recent = Recent::new(64);
        let drain = Uuid::now_v7();
        for i in 0..1_000u32 {
            assert!(recent.first_sight(keyed_digest(b"request", drain, &i.to_be_bytes()), 5_000));
        }
        let held = recent.seen.lock().unwrap().len();
        assert!(held <= 64, "held {held}");
        // The newest entry survives the eviction it caused.
        assert!(!recent.first_sight(
            keyed_digest(b"request", drain, &999u32.to_be_bytes()),
            5_000
        ));
    }

    #[test]
    fn drain_paths_get_the_collection_budget_and_a_retry_answer() {
        let path = format!("{PATH_PREFIX}{}", Uuid::now_v7());
        assert!(is_drain_path(&path));
        assert!(crate::middleware::is_collection_path(&path));
        assert_eq!(
            crate::middleware::budget_for(&Method::POST, &path),
            Some(crate::middleware::INGEST_BUDGET)
        );
        let res = timed_out(&path).unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(res.headers()["retry-after"], "5");

        for other in [
            "/api/log/vercel/",
            "/api/log/vercel",
            "/api/log/vercel/a/b",
            "/api/log/edge",
        ] {
            assert!(!is_drain_path(other), "{other}");
            assert!(timed_out(other).is_none(), "{other}");
        }
    }

    #[test]
    fn the_endpoint_check_answers_a_drain_and_echoes_its_token() {
        let mut drain = LogDrain {
            id: Uuid::now_v7(),
            site_id: Uuid::now_v7(),
            provider: "vercel".into(),
            secret_sealed: String::new(),
            verify_token: None,
            created_by: None,
            created_at: Utc::now(),
            last_received_at: None,
        };
        // No drain: the same answer as a bad signature.
        assert_eq!(endpoint_check(None).status(), StatusCode::UNAUTHORIZED);
        // A drain with no token: answered, with nothing to echo.
        let res = endpoint_check(Some(&drain));
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get(VERIFY_HEADER).is_none());

        drain.verify_token = Some("abc123".into());
        let res = endpoint_check(Some(&drain));
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[VERIFY_HEADER], "abc123");

        // A verified delivery carries it too.
        let res = delivered(&drain, json!({}));
        assert_eq!(res.headers()[VERIFY_HEADER], "abc123");
    }

    #[test]
    fn an_unknown_drain_and_a_bad_signature_read_the_same() {
        let a = unauthorized();
        let b = unauthorized();
        assert_eq!(a.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(a.status(), b.status());
        assert!(a.headers().get(VERIFY_HEADER).is_none());
        assert_eq!(parse_drain_id("not-a-uuid"), None);
        assert!(parse_drain_id(&Uuid::now_v7().to_string()).is_some());
    }
}
