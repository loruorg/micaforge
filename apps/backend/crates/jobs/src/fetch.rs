//! The polite HTTP client every crawling job goes out through.
//!
//! Four jobs in this crate fetch somebody else's server: the two site crawlers,
//! the sitemap walker and the agent-range refresher. They share this module so
//! that the manners are written once.
//!
//! **One user agent, and it is honest.** [`USER_AGENT`] names the product and
//! links to a page describing what it does, so a site owner reading their own
//! logs can find out who we are without asking. A crawler that hides is a
//! crawler that deserves to be blocked.
//!
//! **A ceiling on everything.** Every fetch has a timeout, a redirect limit and
//! a byte cap, and the cap is enforced while the body streams rather than after
//! it has arrived. A sitemap that turns out to be a two-gigabyte file must cost
//! a truncation, never the process.
//!
//! **Gzip twice over.** `reqwest` transparently decodes a gzip *transfer*
//! encoding. It does not, and should not, decode a `.xml.gz` file that arrives
//! as `application/gzip`, because there the compression is the file rather than
//! the transport. Sitemaps are routinely published that way, so [`get`] sniffs
//! the magic bytes and decompresses when it finds them.
//!
//! **A refusal to be pointed anywhere interesting.** The domain these jobs fetch
//! is typed into a settings screen by whoever owns the site, and registration is
//! open. [`client`] and [`guard_url`] come from [`crate::outbound`], which
//! refuses a local address in the URL, in every redirect hop and in every DNS
//! answer, so neither a site created as `http://169.254.169.254/` nor a
//! `robots.txt` that redirects there can turn the worker into a cloud-metadata
//! reader.

use std::io::Read;
use std::time::Duration;

use chrono::{DateTime, Utc};
use micaforge_core::{Error, Result};
use url::Url;

/// What Micaforge calls itself when it fetches a page.
///
/// The `+url` form is the convention every crawler documentation page uses, and
/// the token before the slash is what a site owner writes in `robots.txt` to
/// talk to us. Both halves are part of the contract with the sites we read.
pub const USER_AGENT: &str =
    "Micaforge/1.0 (+https://micaforge.com/docs/agents/robots-and-llms-txt)";

/// The `robots.txt` product token these crawlers obey.
pub const ROBOTS_TOKEN: &str = "Micaforge";

/// Cap for a `robots.txt`. RFC 9309 tells crawlers to parse at least the first
/// 500 KiB and permits ignoring the rest.
pub const ROBOTS_MAX_BYTES: usize = 512 * 1024;

/// Cap for an `llms.txt`. It is a curated index by design; anything larger is
/// not the file the specification describes.
pub const LLMS_MAX_BYTES: usize = 1024 * 1024;

/// Cap for one sitemap document, decompressed. The sitemaps protocol caps an
/// uncompressed document at 50 MB, and this sits under that on purpose: a
/// document larger than this is being used as a bulk dump rather than as an
/// index, and truncating it costs the tail of a list we already cap at 50 000
/// URLs.
pub const SITEMAP_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Cap for one of the site's own pages. We read it for a title and a word
/// count, and both live in the first few megabytes of any real document.
pub const PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Cap for an operator's published IP-range document. The real ones are tens
/// of kilobytes; this leaves room for an operator that grows and none for an
/// endpoint that streams.
pub const RANGES_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Build the HTTP client every job shares.
///
/// One client for the whole process, because `reqwest::Client` holds the
/// connection pool. The timeouts are short, redirects are bounded and checked
/// hop by hop, and names resolve through the guarded resolver: see
/// [`crate::outbound`]. A crawl that hangs is worse than a crawl that fails,
/// because the failure retries and the hang holds a worker lane until the job
/// timeout fires.
///
/// Fails when `MICAFORGE_ALLOW_PRIVATE_OUTBOUND` is set to something that is
/// not a boolean.
pub fn client() -> Result<reqwest::Client> {
    crate::outbound::client()
}

/// One fetched document.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The URL that actually answered, after redirects.
    pub url: String,
    /// The status it answered with. Always a success code: a non-success is an
    /// error or an absence, never a `Fetched`.
    pub status: u16,
    /// The body, decompressed if it was a gzip *file*, and lossily decoded as
    /// UTF-8 because a site's own encoding declaration is not something a
    /// sitemap walker gets to argue with.
    pub body: String,
    /// `Last-Modified`, when the server sent a parseable one.
    pub last_modified: Option<DateTime<Utc>>,
    /// `Content-Type`, lower-cased, without parameters.
    pub content_type: String,
    /// True when the body hit `max_bytes` and the tail was dropped.
    pub truncated: bool,
}

/// Fetch a document, or report that there is not one.
///
/// `Ok(None)` means the server answered and said there is nothing here: `404`,
/// `410`, or `204`. That is a normal answer for `llms.txt` on most sites and for
/// `sitemap.xml` on many, and treating it as a failure would retry it eight
/// times and then dead-letter a job that worked perfectly.
///
/// Every other non-success status is [`Error::Upstream`], which the worker
/// retries.
pub async fn get(client: &reqwest::Client, url: &str, max_bytes: usize) -> Result<Option<Fetched>> {
    let target = guard_url(url)?;

    let response = client.get(target.clone()).send().await.map_err(|e| {
        Error::Upstream(crate::outbound::Failure::from_reqwest(&target, &e).to_string())
    })?;

    let status = response.status();
    if status.as_u16() == 404 || status.as_u16() == 410 || status.as_u16() == 204 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(Error::Upstream(format!("{target} returned HTTP {status}")));
    }

    let final_url = response.url().to_string();
    let last_modified = response
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_http_date);
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();

    let (bytes, truncated) = crate::outbound::read_capped(response, max_bytes).await?;
    let bytes = maybe_gunzip(bytes, max_bytes);

    Ok(Some(Fetched {
        url: final_url,
        status: status.as_u16(),
        body: String::from_utf8_lossy(&bytes).into_owned(),
        last_modified,
        content_type,
        truncated,
    }))
}

/// Decompress a body that is a gzip *file* rather than a gzip transfer encoding.
///
/// Detected by the two magic bytes rather than by the content type, because a
/// `.xml.gz` sitemap is served as `application/gzip`, `application/x-gzip`,
/// `text/xml` and `application/octet-stream` by different servers, and the bytes
/// are the only thing all of them agree on.
///
/// A body that claims to be gzip and will not decompress is returned untouched:
/// the XML scanner downstream will find nothing in it and say so, which is a
/// better outcome than failing the whole sitemap walk over one bad file.
fn maybe_gunzip(bytes: Vec<u8>, max_bytes: usize) -> Vec<u8> {
    if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
        return bytes;
    }

    // `MultiGzDecoder`, not `GzDecoder`: sitemap generators routinely concatenate
    // gzip members, and the single-member decoder stops silently at the first
    // one, which would quietly index a fraction of a site.
    let mut out = Vec::new();
    let mut decoder = flate2::read::MultiGzDecoder::new(&bytes[..]).take(max_bytes as u64);
    match decoder.read_to_end(&mut out) {
        Ok(n) => {
            tracing::debug!(
                compressed = bytes.len(),
                decompressed = n,
                "gunzipped a document"
            );
            out
        }
        Err(e) => {
            tracing::warn!(error = %e, "a gzip-looking body would not decompress; using it raw");
            bytes
        }
    }
}

/// Parse an HTTP date header.
///
/// RFC 9110 names three formats and only requires servers to send the first, so
/// all three are tried. A header nobody can parse becomes `None` rather than an
/// error: an unparseable `Last-Modified` is a missing fact, not a failed fetch.
pub fn parse_http_date(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();

    if let Ok(dt) = DateTime::parse_from_rfc2822(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    // The obsolete asctime form: `Sun Nov  6 08:49:37 1994`.
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, "%a %b %e %H:%M:%S %Y") {
        return Some(naive.and_utc());
    }
    None
}

/// Parse a `lastmod` value from a sitemap.
///
/// The sitemaps protocol says W3C datetime, which is a date, or a date and time,
/// with or without an offset. All three turn up.
pub fn parse_sitemap_date(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();

    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        return Some(naive.and_utc());
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc());
    }
    None
}

/// Check a URL before anything opens a socket to it.
///
/// The same check as [`crate::outbound::guard_url`], kept under this name
/// because the server validates webhook and import URLs through it. It refuses
/// anything that is not `http`/`https`, anything with no host, and any literal
/// local address. A hostname passes here and is judged by the resolver when the
/// request connects.
pub fn guard_url(raw: &str) -> Result<Url> {
    crate::outbound::guard_url(raw)
}

/// Build an absolute URL for a path on a site's own domain.
///
/// The `domain` stored on a site is a bare host (`example.com`), sometimes with
/// a scheme in front of it because somebody pasted one. Both are accepted, and
/// the result is guarded.
pub fn site_url(domain: &str, path: &str) -> Result<Url> {
    let domain = domain.trim().trim_end_matches('/');
    let base = if domain.contains("://") {
        domain.to_string()
    } else {
        format!("https://{domain}")
    };

    let base = guard_url(&base)?;
    base.join(path)
        .map_err(|e| Error::validation("url", format!("`{path}` is not a path on {base}: {e}")))
}

/// One request per second, per site.
///
/// A crawl of twenty-five pages should take twenty-five seconds and be invisible
/// in the site's own dashboard. The limiter is deliberately a plain sleep rather
/// than a token bucket: there is one crawl per site in flight at a time, so
/// there is no burst to smooth and nothing to share.
pub struct Pace {
    interval: Duration,
    last: Option<tokio::time::Instant>,
}

impl Pace {
    /// A pacer that allows one request every `interval`.
    pub fn new(interval: Duration) -> Pace {
        Pace {
            interval,
            last: None,
        }
    }

    /// One request per second, the rate every site crawl in this crate uses.
    pub fn per_second() -> Pace {
        Pace::new(Duration::from_secs(1))
    }

    /// Wait until the next request is due, then mark it as taken.
    ///
    /// Measured from the *start* of the previous request rather than its end, so
    /// a site that answers slowly is not also crawled slowly: the rate promised
    /// is one request per second, and a page that took 900 ms already spent most
    /// of that second.
    pub async fn tick(&mut self) {
        let now = tokio::time::Instant::now();
        if let Some(last) = self.last {
            let elapsed = now.duration_since(last);
            if elapsed < self.interval {
                tokio::time::sleep(self.interval - elapsed).await;
            }
        }
        self.last = Some(tokio::time::Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_metadata_endpoint_is_not_fetchable() {
        assert!(guard_url("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(guard_url("http://127.0.0.1:8123/").is_err());
        assert!(guard_url("http://10.0.0.5/robots.txt").is_err());
        assert!(guard_url("http://192.168.1.1/").is_err());
        assert!(guard_url("http://[::1]/").is_err());
        assert!(guard_url("http://[fd00::1]/").is_err());
        assert!(guard_url("http://[::ffff:127.0.0.1]/").is_err());
        assert!(guard_url("http://100.100.0.1/").is_err());
        // Not HTTP at all.
        assert!(guard_url("file:///etc/passwd").is_err());
        assert!(guard_url("gopher://example.com/").is_err());
        // And the ordinary case still works.
        assert!(guard_url("https://example.com/robots.txt").is_ok());
        assert!(guard_url("http://93.184.216.34/").is_ok());
    }

    #[test]
    fn a_site_domain_becomes_an_https_url_however_it_was_typed() {
        assert_eq!(
            site_url("example.com", "/robots.txt").unwrap().as_str(),
            "https://example.com/robots.txt"
        );
        assert_eq!(
            site_url("https://example.com/", "/llms.txt")
                .unwrap()
                .as_str(),
            "https://example.com/llms.txt"
        );
        assert_eq!(
            site_url("http://example.com", "/sitemap.xml")
                .unwrap()
                .as_str(),
            "http://example.com/sitemap.xml"
        );
        // A name is not an address, so the URL guard passes it; the guarded
        // resolver refuses it when the request connects. See `outbound`.
        assert!(site_url("localhost", "/robots.txt").is_ok());
    }

    #[test]
    fn http_dates_parse_in_every_shape_a_server_sends() {
        let expected = "1994-11-06T08:49:37Z";
        for raw in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "1994-11-06T08:49:37+00:00",
            "Sun Nov  6 08:49:37 1994",
        ] {
            let parsed = parse_http_date(raw).unwrap_or_else(|| panic!("could not parse {raw}"));
            assert_eq!(
                parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                expected
            );
        }
        assert!(parse_http_date("last tuesday").is_none());
    }

    #[test]
    fn sitemap_dates_parse_in_all_three_w3c_shapes() {
        assert!(parse_sitemap_date("2026-08-21").is_some());
        assert!(parse_sitemap_date("2026-08-21T09:30:00").is_some());
        assert!(parse_sitemap_date("2026-08-21T09:30:00+01:00").is_some());
        assert!(parse_sitemap_date("").is_none());
    }

    #[test]
    fn a_body_that_is_not_gzip_is_left_alone() {
        let plain = b"<urlset></urlset>".to_vec();
        assert_eq!(maybe_gunzip(plain.clone(), 1024), plain);
        // A truncated gzip header must not be mistaken for a gzip body.
        assert_eq!(maybe_gunzip(vec![0x1f], 1024), vec![0x1f]);
    }

    #[tokio::test]
    async fn the_pacer_lets_the_first_request_straight_through() {
        let mut pace = Pace::new(Duration::from_millis(50));
        let started = tokio::time::Instant::now();
        pace.tick().await;
        assert!(started.elapsed() < Duration::from_millis(20));
        pace.tick().await;
        assert!(started.elapsed() >= Duration::from_millis(45));
    }
}
