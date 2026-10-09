//! One uptime check: request the URL, time it, and judge the answer.
//!
//! The request goes out through the same three guards as every other request
//! this crate makes ([`crate::outbound`]): the URL is checked, every redirect
//! hop is checked, and every resolved address is checked. A monitor is a URL a
//! signed-up user typed, and "check this every minute" must not become a way
//! to poll the metadata service of the machine the worker runs on.
//!
//! The client differs from the shared one in two ways, both on purpose:
//!
//! - **No connection reuse.** A pooled connection would answer the next check
//!   without a DNS lookup, a TCP handshake or a TLS handshake, which are three
//!   of the things that fail when a site is down. Every check opens its own.
//! - **TLS details are kept**, so the certificate's expiry can be read from
//!   the handshake that just happened rather than from a second connection.
//!
//! [`judge`] is the pure part: given what came back, did the check pass? It
//! is what the tests drive.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use micaforge_core::{Error, Result};
use reqwest::redirect;
use serde::Serialize;
use serde_json::Value as Json;

use super::cert;
use crate::outbound::{self, BodyCap, GuardedResolver};

/// What a monitor check sends as its user agent. Distinct from the crawler's,
/// so a site owner reading their access log can tell a monitor from a crawl.
pub const USER_AGENT: &str =
    "Micaforge-Uptime/1.0 (+https://micaforge.com/docs/dashboard/uptime-monitoring)";

/// The most body read for a keyword or JSON assertion. A health endpoint is a
/// few hundred bytes; a home page is rarely past a megabyte, and a keyword
/// past that point is not a check anyone meant.
pub const BODY_LIMIT: usize = 1024 * 1024;

/// How long the connection itself may take, at most. The monitor's own
/// timeout still bounds the whole check.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// What a monitor asks of a URL.
#[derive(Debug, Clone)]
pub struct Spec {
    /// The URL.
    pub url: String,
    /// `GET` | `HEAD` | `POST`.
    pub method: String,
    /// The whole check's time budget.
    pub timeout: Duration,
    /// Lowest accepted status.
    pub status_min: u16,
    /// Highest accepted status.
    pub status_max: u16,
    /// Text the body must contain.
    pub keyword: Option<String>,
    /// A path into a JSON body.
    pub json_path: Option<String>,
    /// The value expected there.
    pub json_expected: Option<String>,
    /// Whether to judge the certificate's expiry.
    pub ssl_check: bool,
    /// The threshold in days.
    pub ssl_expiry_days: i32,
}

impl Spec {
    /// True when an assertion needs the body.
    fn needs_body(&self) -> bool {
        self.keyword.is_some() || self.json_path.is_some()
    }
}

/// The outcome of one check.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Outcome {
    /// Every assertion held.
    pub ok: bool,
    /// The final status, `None` when no response arrived.
    pub status_code: Option<u16>,
    /// Milliseconds to the response headers, or to the failure.
    pub latency_ms: u32,
    /// Why it failed, as a sentence a person can act on.
    pub error: Option<String>,
    /// Whole days left on the certificate, when it was read.
    pub ssl_days_left: Option<i32>,
    /// The probe region label.
    pub region: String,
    /// When the check started.
    pub checked_at: DateTime<Utc>,
}

/// What came back, before it is judged.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    /// The final status.
    pub status: u16,
    /// The body, up to [`BODY_LIMIT`], when an assertion needed it.
    pub body: Option<Vec<u8>>,
    /// Whether the body went past the limit.
    pub truncated: bool,
    /// Days left on the certificate, when known.
    pub ssl_days_left: Option<i32>,
}

/// The client every check goes out through. Built once.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let allow = outbound::allow_private();
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .tls_info(true)
            .pool_max_idle_per_host(0)
            .redirect(redirect::Policy::custom(
                move |attempt| match outbound::check_redirect(
                    attempt.url(),
                    attempt.previous().len(),
                    allow,
                ) {
                    Ok(()) => attempt.follow(),
                    Err(refusal) => attempt.error(refusal),
                },
            ));
        if !allow {
            builder = builder.dns_resolver(Arc::new(GuardedResolver)).no_proxy();
        }
        // The builder only fails on a TLS backend that cannot initialise, which
        // the shared client would already have reported at boot. The default
        // client still checks; it only loses the certificate expiry.
        builder.build().unwrap_or_else(|e| {
            tracing::error!(error = %e, "could not build the uptime client; using defaults");
            reqwest::Client::new()
        })
    })
}

/// Run one check.
pub async fn check(spec: &Spec, region: &str) -> Outcome {
    let checked_at = Utc::now();
    let started = Instant::now();
    let elapsed =
        |started: Instant| u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let failed = |message: String, status: Option<u16>, latency_ms: u32| Outcome {
        ok: false,
        status_code: status,
        latency_ms,
        error: Some(message),
        ssl_days_left: None,
        region: region.to_string(),
        checked_at,
    };

    let target = match outbound::guard_url(&spec.url) {
        Ok(url) => url,
        Err(e) => return failed(format!("the URL is not allowed: {e}"), None, 0),
    };
    let method = match spec.method.as_str() {
        "HEAD" => reqwest::Method::HEAD,
        "POST" => reqwest::Method::POST,
        _ => reqwest::Method::GET,
    };

    let request = client()
        .request(method, target.clone())
        .timeout(spec.timeout)
        .header(reqwest::header::CACHE_CONTROL, "no-cache");
    let response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            let latency = elapsed(started);
            tracing::debug!(url = %outbound::redacted(&target), error = %outbound::Failure::from_reqwest(&target, &e), "uptime check failed");
            return failed(transport_error(&e, spec.timeout), None, latency);
        }
    };
    let latency_ms = elapsed(started);

    let status = response.status().as_u16();
    let ssl_days_left = if spec.ssl_check && response.url().scheme() == "https" {
        response
            .extensions()
            .get::<reqwest::tls::TlsInfo>()
            .and_then(|info| info.peer_certificate())
            .and_then(cert::not_after)
            .map(|expires| cert::days_left(expires, Utc::now()))
    } else {
        None
    };

    let mut observed = Observed {
        status,
        body: None,
        truncated: false,
        ssl_days_left,
    };
    if spec.needs_body() {
        let remaining = spec.timeout.saturating_sub(started.elapsed());
        match tokio::time::timeout(remaining, read_body(response)).await {
            Ok(Ok((body, truncated))) => {
                observed.body = Some(body);
                observed.truncated = truncated;
            }
            Ok(Err(_)) => {
                return Outcome {
                    ssl_days_left,
                    ..failed(
                        "the response body could not be read".to_string(),
                        Some(status),
                        latency_ms,
                    )
                }
            }
            Err(_) => {
                return Outcome {
                    ssl_days_left,
                    ..failed(
                        format!("timed out after {} reading the body", seconds(spec.timeout)),
                        Some(status),
                        latency_ms,
                    )
                }
            }
        }
    }

    let verdict = judge(spec, &observed);
    Outcome {
        ok: verdict.is_ok(),
        status_code: Some(status),
        latency_ms,
        error: verdict.err(),
        ssl_days_left,
        region: region.to_string(),
        checked_at,
    }
}

/// Read up to [`BODY_LIMIT`] of the body, stopping as soon as the cap is
/// crossed.
async fn read_body(mut response: reqwest::Response) -> Result<(Vec<u8>, bool)> {
    let mut cap = BodyCap::new(BODY_LIMIT as u64);
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::Upstream(format!("reading the body: {e}")))?
    {
        let keep = cap.admit(chunk.len());
        body.extend_from_slice(&chunk[..keep]);
        if cap.exceeded() {
            return Ok((body, true));
        }
    }
    Ok((body, false))
}

/// A transport failure, in words. Coarse on purpose, as in
/// [`outbound::FailureKind`]: the monitor's owner learns that the server could
/// not be reached, not which internal address refused.
fn transport_error(e: &reqwest::Error, timeout: Duration) -> String {
    if e.is_timeout() {
        format!("timed out after {}", seconds(timeout))
    } else if e.is_redirect() {
        "the redirect chain was refused: too long, or it points at a local address".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else {
        "the request failed before a response arrived".to_string()
    }
}

/// `10 s`, `1.5 s`.
fn seconds(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

/// Did the check pass? `Err` carries the first failure, as a sentence.
///
/// The order is the order a person would want to hear about it: a wrong
/// status first (nothing after it means much), then the body assertions,
/// then the certificate, which is a warning about the future rather than a
/// failure of the present and so is only reported when everything else held.
pub fn judge(spec: &Spec, observed: &Observed) -> std::result::Result<(), String> {
    if observed.status < spec.status_min || observed.status > spec.status_max {
        let expected = if spec.status_min == spec.status_max {
            format!("{}", spec.status_min)
        } else {
            format!("{}-{}", spec.status_min, spec.status_max)
        };
        return Err(format!(
            "expected status {expected}, got {}",
            observed.status
        ));
    }

    let body = observed.body.as_deref().unwrap_or_default();

    if let Some(keyword) = spec.keyword.as_deref().filter(|k| !k.is_empty()) {
        let text = String::from_utf8_lossy(body);
        if !text.contains(keyword) {
            let scope = if observed.truncated {
                format!(" in the first {} KiB", BODY_LIMIT / 1024)
            } else {
                String::new()
            };
            return Err(format!(
                "the response does not contain \"{keyword}\"{scope}"
            ));
        }
    }

    if let Some(path) = spec.json_path.as_deref().filter(|p| !p.trim().is_empty()) {
        let document: Json = serde_json::from_slice(body)
            .map_err(|_| "the response is not valid JSON".to_string())?;
        let segments = parse_json_path(path).map_err(|e| format!("the JSON path {e}"))?;
        let found =
            lookup(&document, &segments).ok_or_else(|| format!("nothing at JSON path {path}"))?;
        if let Some(expected) = spec.json_expected.as_deref() {
            let actual = json_text(found);
            if actual != expected {
                return Err(format!("{path} is \"{actual}\", expected \"{expected}\""));
            }
        }
    }

    if spec.ssl_check {
        if let Some(days) = observed.ssl_days_left {
            if days < 0 {
                let ago = -i64::from(days);
                return Err(format!(
                    "the certificate expired {ago} {} ago",
                    plural(ago, "day", "days")
                ));
            }
            if days < spec.ssl_expiry_days {
                return Err(format!(
                    "the certificate expires in {days} {} (threshold {})",
                    plural(i64::from(days), "day", "days"),
                    spec.ssl_expiry_days
                ));
            }
        }
    }

    Ok(())
}

fn plural<'a>(n: i64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// One step of a JSON path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// An object key.
    Key(String),
    /// An array index.
    Index(usize),
}

/// Parse `data.items[0].ok`, with an optional leading `$` or `$.`.
///
/// The small, unambiguous subset a health check needs: keys separated by
/// dots, and zero-based indexes in brackets. A key with a dot or a bracket in
/// it can be written in brackets and quotes, `["a.b"]`. Anything else is
/// refused at write time, so a monitor never fails because its path was
/// misread.
pub fn parse_json_path(raw: &str) -> std::result::Result<Vec<Segment>, String> {
    let mut path = raw.trim();
    path = path.strip_prefix('$').unwrap_or(path);
    path = path.strip_prefix('.').unwrap_or(path);
    if path.is_empty() {
        return Err("is empty".to_string());
    }

    let mut segments = Vec::new();
    let mut key = String::new();
    // A dot straight after `]` separates; a dot after another dot, or at the
    // end, is an empty step.
    let mut after_bracket = false;
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if key.is_empty() && !after_bracket {
                    return Err(format!("`{raw}` has an empty step"));
                }
                if !key.is_empty() {
                    segments.push(Segment::Key(std::mem::take(&mut key)));
                }
                after_bracket = false;
                if chars.peek().is_none() {
                    return Err(format!("`{raw}` has an empty step"));
                }
            }
            '[' => {
                if !key.is_empty() {
                    segments.push(Segment::Key(std::mem::take(&mut key)));
                }
                let mut inner = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == ']' {
                        closed = true;
                        break;
                    }
                    inner.push(c);
                }
                if !closed {
                    return Err(format!("`{raw}` has an unclosed `[`"));
                }
                let inner = inner.trim();
                if let Some(quoted) = inner
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .or_else(|| inner.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                {
                    segments.push(Segment::Key(quoted.to_string()));
                } else {
                    let index = inner
                        .parse::<usize>()
                        .map_err(|_| format!("`{raw}` has `[{inner}]`, which is not an index"))?;
                    segments.push(Segment::Index(index));
                }
                if let Some(&next) = chars.peek() {
                    if next != '.' && next != '[' {
                        return Err(format!("`{raw}` needs a `.` after `]`"));
                    }
                }
                after_bracket = true;
            }
            ']' => return Err(format!("`{raw}` has a `]` with no `[`")),
            other => key.push(other),
        }
    }
    if !key.is_empty() {
        segments.push(Segment::Key(key));
    }
    if segments.is_empty() {
        return Err(format!("`{raw}` names nothing"));
    }
    Ok(segments)
}

/// Follow a parsed path into a document.
pub fn lookup<'a>(document: &'a Json, segments: &[Segment]) -> Option<&'a Json> {
    segments
        .iter()
        .try_fold(document, |node, segment| match segment {
            Segment::Key(key) => node.as_object()?.get(key),
            Segment::Index(index) => node.as_array()?.get(*index),
        })
}

/// A JSON value as the text an operator would type to expect it: a string
/// without its quotes, everything else as JSON.
fn json_text(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> Spec {
        Spec {
            url: "https://example.com/health".into(),
            method: "GET".into(),
            timeout: Duration::from_secs(10),
            status_min: 200,
            status_max: 299,
            keyword: None,
            json_path: None,
            json_expected: None,
            ssl_check: true,
            ssl_expiry_days: 14,
        }
    }

    fn seen(status: u16, body: &str) -> Observed {
        Observed {
            status,
            body: Some(body.as_bytes().to_vec()),
            truncated: false,
            ssl_days_left: Some(90),
        }
    }

    #[test]
    fn the_status_range_is_inclusive_at_both_ends() {
        let s = spec();
        assert!(judge(&s, &seen(200, "")).is_ok());
        assert!(judge(&s, &seen(299, "")).is_ok());
        assert_eq!(
            judge(&s, &seen(503, "")).unwrap_err(),
            "expected status 200-299, got 503"
        );
        let exact = Spec {
            status_min: 204,
            status_max: 204,
            ..spec()
        };
        assert_eq!(
            judge(&exact, &seen(200, "")).unwrap_err(),
            "expected status 204, got 200"
        );
    }

    #[test]
    fn a_keyword_must_appear_and_is_case_sensitive() {
        let s = Spec {
            keyword: Some("All good".into()),
            ..spec()
        };
        assert!(judge(&s, &seen(200, "<p>All good</p>")).is_ok());
        assert_eq!(
            judge(&s, &seen(200, "<p>all good</p>")).unwrap_err(),
            "the response does not contain \"All good\""
        );
        let truncated = Observed {
            truncated: true,
            ..seen(200, "nothing")
        };
        assert!(judge(&s, &truncated)
            .unwrap_err()
            .ends_with("in the first 1024 KiB"));
    }

    #[test]
    fn json_assertions_compare_text_or_require_presence() {
        let body = r#"{"status":"ok","checks":[{"db":true},{"cache":false}],"version":3}"#;
        let equals = |path: &str, expected: Option<&str>| Spec {
            json_path: Some(path.into()),
            json_expected: expected.map(str::to_string),
            ..spec()
        };
        assert!(judge(&equals("status", Some("ok")), &seen(200, body)).is_ok());
        assert!(judge(&equals("$.status", Some("ok")), &seen(200, body)).is_ok());
        assert!(judge(&equals("checks[0].db", Some("true")), &seen(200, body)).is_ok());
        assert!(judge(&equals("version", Some("3")), &seen(200, body)).is_ok());
        assert!(judge(&equals("checks[1]", None), &seen(200, body)).is_ok());
        assert_eq!(
            judge(&equals("checks[1].cache", Some("true")), &seen(200, body)).unwrap_err(),
            "checks[1].cache is \"false\", expected \"true\""
        );
        assert_eq!(
            judge(&equals("checks[5]", None), &seen(200, body)).unwrap_err(),
            "nothing at JSON path checks[5]"
        );
        assert_eq!(
            judge(&equals("status", Some("ok")), &seen(200, "<html>")).unwrap_err(),
            "the response is not valid JSON"
        );
    }

    #[test]
    fn a_wrong_status_is_reported_before_the_body() {
        let s = Spec {
            keyword: Some("ok".into()),
            ..spec()
        };
        assert!(judge(&s, &seen(500, "not it"))
            .unwrap_err()
            .starts_with("expected status"));
    }

    #[test]
    fn the_certificate_fails_below_its_threshold_and_after_expiry() {
        let s = spec();
        let days = |d: i32| Observed {
            ssl_days_left: Some(d),
            ..seen(200, "")
        };
        assert!(judge(&s, &days(14)).is_ok());
        assert_eq!(
            judge(&s, &days(13)).unwrap_err(),
            "the certificate expires in 13 days (threshold 14)"
        );
        assert_eq!(
            judge(&s, &days(1)).unwrap_err(),
            "the certificate expires in 1 day (threshold 14)"
        );
        assert_eq!(
            judge(&s, &days(-2)).unwrap_err(),
            "the certificate expired 2 days ago"
        );
        // Off, or unknown, is never a failure.
        let off = Spec {
            ssl_check: false,
            ..spec()
        };
        assert!(judge(&off, &days(-2)).is_ok());
        let unknown = Observed {
            ssl_days_left: None,
            ..seen(200, "")
        };
        assert!(judge(&s, &unknown).is_ok());
    }

    #[test]
    fn json_paths_parse_into_keys_and_indexes() {
        assert_eq!(
            parse_json_path("data.items[0].ok").unwrap(),
            vec![
                Segment::Key("data".into()),
                Segment::Key("items".into()),
                Segment::Index(0),
                Segment::Key("ok".into()),
            ]
        );
        assert_eq!(
            parse_json_path("$[\"a.b\"][2]").unwrap(),
            vec![Segment::Key("a.b".into()), Segment::Index(2)]
        );
        assert_eq!(
            parse_json_path("$.status").unwrap(),
            vec![Segment::Key("status".into())]
        );
        for bad in ["", "$", "a..b", "a.", "a[x]", "a[0", "a]", "a[0]b"] {
            assert!(parse_json_path(bad).is_err(), "{bad:?} should be refused");
        }
        let doc = json!({ "a.b": [1, 2, { "c": null }] });
        let found = lookup(&doc, &parse_json_path("[\"a.b\"][2].c").unwrap());
        assert_eq!(found, Some(&Json::Null));
    }

    #[tokio::test]
    async fn a_local_address_is_refused_before_any_socket_opens() {
        if outbound::allow_private() {
            return;
        }
        let outcome = check(
            &Spec {
                url: "http://127.0.0.1:9/health".into(),
                ..spec()
            },
            "test",
        )
        .await;
        assert!(!outcome.ok);
        assert_eq!(outcome.status_code, None);
        assert!(outcome
            .error
            .as_deref()
            .unwrap_or_default()
            .starts_with("the URL is not allowed"));
        assert_eq!(outcome.region, "test");
    }
}
