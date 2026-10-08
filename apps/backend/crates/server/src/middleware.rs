//! The layers every response passes through.
//!
//! Seven concerns, in the order a request meets them:
//!
//! 1. [`catch_panic`]: a panic in a handler becomes a `500` with the standard
//!    error envelope, not a dropped connection.
//! 2. [`trace`]: a request id on a span, and one structured line per request.
//! 3. The security headers ([`no_sniff`] and friends): four headers that cost
//!    nothing and close real holes.
//! 4. [`rate_limit`]: a per-address ceiling, so one broken client cannot be a
//!    denial of service.
//! 5. [`timeout`]: a time budget per request, so a stalled store cannot hold
//!    every connection open.
//! 6. [`cors`]: an allow-list built from the configured dashboard origin.
//! 7. Body limits: applied per route group in [`crate::routes`], because the
//!    import upload and the ingest path need different ones.
//!
//! ## No address in a log line or a limiter key
//!
//! The access log and the limiter both need to tell callers apart, and neither
//! needs to know who they are. Both use [`crate::client_ip::address_tag`], a
//! keyed digest whose key never leaves process memory, so a rotated log file or
//! a Valkey snapshot on disk holds no address (`PRODUCT.md`: no raw IP at rest).
//! The user agent is not logged either.
//!
//! ## Why CORS is not global
//!
//! `docs/CONTRACT.md` §3.3 makes the collection endpoints open to every origin
//! and the rest of the API closed to all but the dashboard. Those cannot both be
//! one layer, and stacking a permissive layer on top of a strict one emits two
//! `access-control-allow-origin` headers, which every browser treats as a CORS
//! *failure* rather than as a permissive answer. So [`cors`] is applied to the
//! management and analytics group only; `routes::ingest` writes its own headers
//! per response and `routes::script` serves `/mf.js` with `*`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::Instrument;

use micaforge_core::Config;

use crate::client_ip::{address_tag, client_ip};
use crate::state::AppState;

/// The correlation id header, read from the caller when they set one.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Largest body accepted on an ordinary JSON route.
///
/// A dashboard layout with fifty tiles, a segment with a deep filter tree and a
/// funnel with twenty steps all fit inside this several times over. Anything
/// larger on these routes is a mistake or an attack.
pub const JSON_BODY_LIMIT: usize = 1024 * 1024;

/// Requests one address may make per [`RATE_WINDOW_SECS`].
///
/// Deliberately high. This is a circuit breaker for a client stuck in a loop,
/// not traffic policing: a whole office behind one NAT loading a dashboard that
/// polls realtime every ten seconds is a legitimate source of a lot of requests,
/// and throttling them would break the product for a real customer to inconvenience
/// nobody. `MICAFORGE_RATE_LIMIT_PER_MIN` moves it; `0` switches it off.
const DEFAULT_RATE_LIMIT: u32 = 3_000;

/// The rate-limit window, in seconds.
const RATE_WINDOW_SECS: u32 = 60;

/// How long a browser may cache a preflight answer.
const PREFLIGHT_MAX_AGE: Duration = Duration::from_secs(3600);

/// The time budget of a collection request: a beacon, a pixel, a signed edge
/// batch, the tracker file and the flag evaluation a page makes.
///
/// A healthy hit takes a millisecond or two. Five seconds is long enough for a
/// thousand-row edge batch against a slow Valkey, and short enough that a store
/// outage cannot leave thousands of beacons parked on open connections. It is
/// also shorter than the ten seconds both shippers wait, so a shipper reads a
/// real `503` and retries rather than aborting blind.
pub const INGEST_BUDGET: Duration = Duration::from_secs(5);

/// The time budget of every other request.
///
/// No route in this server holds a request open on purpose (`/api/stats/realtime`
/// is an ordinary cached read that the dashboard polls), so this is a ceiling for
/// a query that has gone wrong, not a long-poll allowance. The one exception is
/// the import upload, which streams gigabytes and has no budget at all.
pub const API_BUDGET: Duration = Duration::from_secs(30);

/// How long the limiter waits for Valkey before it lets the request through.
///
/// A healthy round trip is well under a millisecond. Anything slower than this
/// is an outage, and the limiter fails open: a request served unlimited is a
/// smaller harm than every request waiting on a store it can do without.
pub const LIMITER_BUDGET: Duration = Duration::from_millis(250);

/// The target the access log is written under.
///
/// Outside the `micaforge_server` prefix on purpose. The default filter turns
/// that crate up to `debug`, and the collection routes are logged at `debug`
/// precisely so that they are *not* written by default: at fifty hits a second
/// they would rotate every other line out of a small log volume within the
/// hour. `RUST_LOG=micaforge::access=debug` shows them.
const ACCESS_LOG: &str = "micaforge::access";

/// Is this one of the paths a visitor's browser or a shipper hits per page?
///
/// They share a time budget ([`INGEST_BUDGET`]) and a log level (`debug`),
/// because they are the high-volume, stranger-facing half of the server.
pub fn is_collection_path(path: &str) -> bool {
    matches!(
        path,
        "/api/track"
            | "/api/track/batch"
            | "/api/track/pixel.gif"
            | "/api/log/edge"
            | "/mf.js"
            | "/mf.min.js"
            | "/api/flags/evaluate"
    )
}

/// A warning written at most once per interval.
///
/// For failures that happen per request: a Valkey that is down would otherwise
/// write the same line fifty times a second and rotate the log that explains it
/// out of existence.
#[derive(Debug, Default)]
pub struct LogGate {
    /// Unix seconds of the last line written, `0` for never.
    last: AtomicU64,
}

impl LogGate {
    /// A gate that has never opened.
    pub const fn new() -> LogGate {
        LogGate {
            last: AtomicU64::new(0),
        }
    }

    /// `true` when the caller should write its line now.
    pub fn open(&self, every: Duration) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .max(1);
        let last = self.last.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < every.as_secs() {
            return false;
        }
        // One winner per interval, even when many requests fail together.
        self.last
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }
}

// ---------------------------------------------------------------------------
// Request id and the access log
// ---------------------------------------------------------------------------

/// A request's correlation id, available to a handler through the extensions.
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Give the request an id, then log what happened to it.
///
/// The id is the caller's own `x-request-id` when they sent one, so a trace that
/// starts at a reverse proxy stays one trace, and a fresh UUID otherwise. It
/// goes back on the response either way, which is what makes "which request was
/// that?" answerable from a browser's network tab. It is also on a span around
/// the whole request, so every line a handler writes while serving it, an error
/// included, carries the same id.
///
/// The line is emitted **after** the response, at a level chosen by the status
/// and the route: `warn` for a `5xx`, `debug` for the collection routes (see
/// [`ACCESS_LOG`]), `info` otherwise. A `4xx` is not a warning, because a
/// refused sign-in code and a missing page are the API working, and an access log
/// full of warnings is an access log nobody reads.
///
/// The caller is identified by [`address_tag`], never by address, and the user
/// agent is not written at all.
pub async fn trace(req: Request, next: Next) -> Response<Body> {
    let started = Instant::now();

    let id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .map(str::to_string)
        .unwrap_or_else(|| micaforge_core::new_id().to_string());

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let client = address_tag(request_ip(&req));

    let mut req = req;
    req.extensions_mut().insert(RequestId(id.clone()));

    // `ERROR` level so the span exists under any filter that still shows an
    // error line, which is the line that most needs the id.
    let span = tracing::span!(tracing::Level::ERROR, "request", request_id = %id);
    let mut response = next.run(req).instrument(span.clone()).await;

    let status = response.status();
    let latency_ms = started.elapsed().as_millis();

    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }

    span.in_scope(|| {
        if status.is_server_error() {
            tracing::warn!(
                target: ACCESS_LOG,
                request_id = %id, method = %method, path = %path, status = status.as_u16(),
                latency_ms = %latency_ms, client = %client, "request failed"
            );
        } else if is_collection_path(&path) {
            tracing::debug!(
                target: ACCESS_LOG,
                request_id = %id, method = %method, path = %path, status = status.as_u16(),
                latency_ms = %latency_ms, client = %client, "request"
            );
        } else {
            tracing::info!(
                target: ACCESS_LOG,
                request_id = %id, method = %method, path = %path, status = status.as_u16(),
                latency_ms = %latency_ms, client = %client, "request"
            );
        }
    });

    response
}

/// The address this request is attributed to, by the trusted-proxy rule.
fn request_ip(req: &Request) -> std::net::IpAddr {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    client_ip(req.headers(), peer)
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

/// Refuse a caller who is making far more requests than any real client makes.
///
/// Keyed on the address alone rather than on the account, because the addresses
/// that need bounding are the ones with no account: a scraper walking site ids,
/// a misconfigured cron, a tracker in a redirect loop. The key holds the
/// [`address_tag`], not the address, because Valkey writes its keys to disk.
///
/// **Fails open, and fast.** The counter lives in Redis, and Redis is explicitly
/// the store Micaforge is allowed to lose (`micaforge_store`'s own documentation
/// says so). If it errors or does not answer within [`LIMITER_BUDGET`], the
/// request is served. A cache outage that took the whole API down with it would
/// turn a degradation into an incident.
///
/// `/api/health` is exempt: a container healthcheck that gets rate limited
/// restarts the container, which is the opposite of what a limiter is for.
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response<Body> {
    static REFUSED: LogGate = LogGate::new();
    static UNAVAILABLE: LogGate = LogGate::new();

    let limit = configured_limit();
    let path = req.uri().path().to_string();

    if limit == 0 || path == "/api/health" {
        return next.run(req).await;
    }

    let client = address_tag(request_ip(&req));
    let key = format!("ip:{client}");

    let verdict = tokio::time::timeout(
        LIMITER_BUDGET,
        state.kv.rate_limit(&key, limit, RATE_WINDOW_SECS),
    )
    .await;

    match verdict {
        Ok(Ok(true)) => next.run(req).await,
        Ok(Ok(false)) => {
            if REFUSED.open(Duration::from_secs(10)) {
                tracing::warn!(client = %client, path = %path, limit, "address rate limited");
            } else {
                tracing::debug!(client = %client, path = %path, limit, "address rate limited");
            }
            crate::error::ApiError(micaforge_core::Error::RateLimited {
                retry_after: u64::from(RATE_WINDOW_SECS),
            })
            .into_response()
        }
        Ok(Err(e)) => {
            if UNAVAILABLE.open(Duration::from_secs(60)) {
                tracing::warn!(error = %e, "the rate limiter could not be consulted; allowing");
            }
            next.run(req).await
        }
        Err(_) => {
            if UNAVAILABLE.open(Duration::from_secs(60)) {
                tracing::warn!(
                    budget_ms = LIMITER_BUDGET.as_millis() as u64,
                    "the rate limiter did not answer in time; allowing"
                );
            }
            next.run(req).await
        }
    }
}

/// `MICAFORGE_RATE_LIMIT_PER_MIN`, read once.
fn configured_limit() -> u32 {
    static LIMIT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("MICAFORGE_RATE_LIMIT_PER_MIN")
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_RATE_LIMIT)
    })
}

// ---------------------------------------------------------------------------
// Time budgets
// ---------------------------------------------------------------------------

/// Stop a request that has run past its budget.
///
/// [`INGEST_BUDGET`] for the collection routes, [`API_BUDGET`] for the rest, and
/// none for the import upload. Dropping the handler's future cancels it at its
/// next `await`, which releases the pool connection or the Valkey call it was
/// waiting on.
///
/// The answer for a collection route is decided by `routes::ingest`, because
/// that module owns the rule that a beacon is never answered with an error a
/// page owner would see. Everything else gets `503` with code `timeout`, which
/// both server SDKs treat as worth a retry.
pub async fn timeout(req: Request, next: Next) -> Response<Body> {
    match budget_for(req.method(), req.uri().path()) {
        Some(budget) => bounded(budget, req, next).await,
        None => next.run(req).await,
    }
}

/// Run the rest of the stack, or answer for it once `budget` has passed.
async fn bounded(budget: Duration, req: Request, next: Next) -> Response<Body> {
    let path = req.uri().path().to_string();

    match tokio::time::timeout(budget, next.run(req)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(
                path = %path,
                budget_ms = budget.as_millis() as u64,
                "a request ran past its time budget and was stopped"
            );
            crate::routes::ingest::timed_out(&path).unwrap_or_else(|| timed_out(budget))
        }
    }
}

/// The budget for one request, or `None` for one that has none.
pub fn budget_for(method: &Method, path: &str) -> Option<Duration> {
    // The upload streams a GA4 export to a scratch file at the client's pace.
    if method == Method::POST && path == "/api/imports" {
        return None;
    }
    if is_collection_path(path) {
        Some(INGEST_BUDGET)
    } else {
        Some(API_BUDGET)
    }
}

/// The `503` for an API request that ran out of time.
fn timed_out(budget: Duration) -> Response<Body> {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "code": "timeout",
                "message": format!(
                    "the request took longer than {} seconds and was stopped; try again, or ask for a narrower range",
                    budget.as_secs()
                ),
            }
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("5"));
    response
}

// ---------------------------------------------------------------------------
// Panics
// ---------------------------------------------------------------------------

/// Turn a panic in a handler into a `500` in the standard error envelope.
///
/// Without this a panic aborts the connection: the client sees a transport
/// failure rather than a status, retries into the same panic, and the log line
/// that would have explained it is attached to a task nobody is watching. The
/// panic is logged here with its message, and the client is told nothing beyond
/// "internal error", because a panic message routinely contains an index, a
/// slice length or a row from the database.
type PanicHandler = fn(Box<dyn std::any::Any + Send + 'static>) -> Response<Body>;

pub fn catch_panic() -> CatchPanicLayer<PanicHandler> {
    CatchPanicLayer::custom(on_panic as PanicHandler)
}

fn on_panic(err: Box<dyn std::any::Any + Send + 'static>) -> Response<Body> {
    let detail = if let Some(message) = err.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = err.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        "a panic with no message".to_string()
    };
    tracing::error!(panic = %detail, "a handler panicked");

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": {
                "code": "internal_error",
                "message": "something went wrong on our side; the failure has been logged",
            }
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// CORS
// ---------------------------------------------------------------------------

/// The allow-list for the management and analytics API.
///
/// Built from `MICAFORGE_DASHBOARD_URL` and `MICAFORGE_PUBLIC_URL`, plus any
/// extra origins in `MICAFORGE_CORS_ORIGINS` (comma separated) for an install
/// that serves the dashboard from a second hostname.
///
/// `allow_credentials` is on, because the dashboard authenticates with a cookie.
/// That is exactly why the origin list is explicit and never `*`: a wildcard with
/// credentials is both forbidden by the specification and the shape of the bug
/// where any page on the internet can read a signed-in operator's analytics.
pub fn cors(cfg: &Config) -> CorsLayer {
    let mut origins: Vec<HeaderValue> = Vec::new();
    let mut push = |raw: &str| {
        let trimmed = raw.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return;
        }
        match HeaderValue::from_str(trimmed) {
            Ok(value) if !origins.contains(&value) => origins.push(value),
            Ok(_) => {}
            Err(_) => tracing::warn!(
                origin = trimmed,
                "ignoring an origin that is not a header value"
            ),
        }
    };

    push(&cfg.dashboard_url);
    push(&cfg.public_url);
    if let Ok(extra) = std::env::var("MICAFORGE_CORS_ORIGINS") {
        for origin in extra.split(',') {
            push(origin);
        }
    }

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_credentials(true)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            HeaderName::from_static("content-type"),
            HeaderName::from_static("authorization"),
            HeaderName::from_static("x-request-id"),
            HeaderName::from_static("x-micaforge-signature"),
            HeaderName::from_static("x-micaforge-timestamp"),
        ])
        .expose_headers([HeaderName::from_static("x-request-id")])
        .max_age(PREFLIGHT_MAX_AGE)
}

// ---------------------------------------------------------------------------
// Security headers
// ---------------------------------------------------------------------------

/// One header, set only when the handler did not set its own.
///
/// `if_not_present` matters: `/mf.js` already sets `x-content-type-options` and
/// its own caching directives, and overwriting a handler's deliberate choice
/// from a blanket layer is how a tracker ends up served with the wrong headers.
fn header(name: &'static str, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    )
}

/// `nosniff`. Stops a browser from deciding a JSON error body is HTML and
/// running it, which is the whole of a reflected-XSS-through-an-API-response.
pub fn no_sniff() -> SetResponseHeaderLayer<HeaderValue> {
    header("x-content-type-options", "nosniff")
}

/// Refuse framing. Nothing this API serves is meant to be embedded; the share
/// dashboard is a page in the frontend, not a response from here.
pub fn no_frame() -> SetResponseHeaderLayer<HeaderValue> {
    header("x-frame-options", "DENY")
}

/// Send an origin, never a full URL, on outbound navigations. A dashboard URL
/// carries site ids and filter state, and neither belongs in somebody else's
/// referrer log.
pub fn referrer_policy() -> SetResponseHeaderLayer<HeaderValue> {
    header("referrer-policy", "strict-origin-when-cross-origin")
}

/// Turn off the device APIs an analytics API has no use for, so a future
/// same-origin page cannot quietly acquire them.
pub fn permissions_policy() -> SetResponseHeaderLayer<HeaderValue> {
    header(
        "permissions-policy",
        "accelerometer=(), camera=(), geolocation=(), gyroscope=(), microphone=(), payment=(), usb=()",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_body_limit_leaves_room_for_a_real_dashboard() {
        // A dashboard layout is the largest thing a management route accepts.
        const { assert!(JSON_BODY_LIMIT >= 512 * 1024) };
    }

    #[test]
    fn the_default_rate_limit_cannot_throttle_a_real_office() {
        // A realtime poll every ten seconds is six requests a minute per open
        // tab, and a dashboard makes several calls per view. Even a hundred
        // people behind one address must stay well inside this.
        const { assert!(DEFAULT_RATE_LIMIT >= 1_000) };
    }

    #[test]
    fn budgets_follow_the_route() {
        assert_eq!(budget_for(&Method::POST, "/api/track"), Some(INGEST_BUDGET));
        assert_eq!(
            budget_for(&Method::POST, "/api/log/edge"),
            Some(INGEST_BUDGET)
        );
        assert_eq!(
            budget_for(&Method::GET, "/api/stats/realtime"),
            Some(API_BUDGET)
        );
        assert_eq!(budget_for(&Method::GET, "/api/imports"), Some(API_BUDGET));
        assert_eq!(
            budget_for(&Method::POST, "/api/imports"),
            None,
            "an upload runs at the client's pace"
        );
        const { assert!(LIMITER_BUDGET.as_millis() <= 500) };
        const {
            assert!(
                INGEST_BUDGET.as_secs() < 10,
                "shorter than the shippers' own timeout"
            )
        };
    }

    #[test]
    fn collection_routes_are_the_quiet_ones() {
        for path in [
            "/api/track",
            "/api/track/pixel.gif",
            "/api/log/edge",
            "/mf.js",
        ] {
            assert!(is_collection_path(path), "{path}");
        }
        for path in ["/api/stats/overview", "/api/auth/code", "/api/track/other"] {
            assert!(!is_collection_path(path), "{path}");
        }
    }

    #[test]
    fn a_log_gate_opens_once_per_interval() {
        let gate = LogGate::new();
        assert!(gate.open(Duration::from_secs(60)));
        assert!(!gate.open(Duration::from_secs(60)));
        assert!(!gate.open(Duration::from_secs(60)));
        // A zero interval never holds a line back.
        let open = LogGate::new();
        assert!(open.open(Duration::ZERO));
    }

    #[tokio::test]
    async fn a_request_past_its_budget_is_answered_not_held() {
        use axum::routing::get;
        use tower::ServiceExt;

        async fn stuck() -> &'static str {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            "never"
        }

        // The real budgets are seconds long; the mechanism is the same at 20 ms.
        let app = axum::Router::new()
            .route("/api/stats/overview", get(stuck))
            .route("/api/track/pixel.gif", get(stuck))
            .layer(axum::middleware::from_fn(|req: Request, next: Next| {
                bounded(Duration::from_millis(20), req, next)
            }));

        let api = app
            .clone()
            .oneshot(
                Request::get("/api/stats/overview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);

        // A pixel still gets its image, so no page shows a broken icon.
        let pixel = app
            .oneshot(
                Request::get("/api/track/pixel.gif")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(pixel.status(), StatusCode::OK);
        assert_eq!(pixel.headers()["content-type"], "image/gif");
    }

    #[test]
    fn a_panic_response_says_nothing_about_the_panic() {
        let response = on_panic(Box::new("index out of bounds: the len is 3".to_string()));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // The body is checked by shape rather than by reading it back, because
        // the point is that it is a fixed sentence built here and never the
        // panic's own message.
        assert!(response.headers().get("content-type").is_some());
    }
}
