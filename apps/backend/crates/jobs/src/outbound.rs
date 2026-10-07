//! Every request this process makes to a server somebody else named.
//!
//! The jobs in this crate fetch URLs that a signed-up user typed: a site's
//! domain, a webhook, an import link. Registration is open, so "somebody else"
//! includes a stranger, and the threat is the obvious one: a URL, or a redirect
//! from one, that points back inside the network the worker runs on. A
//! `robots.txt` that answers `302 http://169.254.169.254/latest/meta-data/`
//! would otherwise have the metadata service's answer stored and shown on the
//! policy screen.
//!
//! Three checks, each covering what the one before it cannot see:
//!
//! - **The URL.** [`guard_url`] refuses anything that is not `http`/`https`,
//!   has no host, or is a literal address in a range [`is_local`] names. It runs
//!   at write time, so a settings screen can say what is wrong, and again just
//!   before every request.
//! - **Every redirect hop.** The client's redirect policy runs the same check
//!   on each `Location` and stops after [`MAX_REDIRECTS`] hops. Without it the
//!   URL check would vouch for the first hop only.
//! - **Every resolved name.** A hostname is not an address, and checking what
//!   it resolves to before the request would be a race against the resolution
//!   that actually connects. So the client resolves names through
//!   [`GuardedResolver`], which drops every refused address from the answer the
//!   connector dials. A name that resolves only to refused addresses fails to
//!   connect. That covers `localhost`, the compose service names and
//!   `169.254.169.254.nip.io` alike, and a name that changes its answer between
//!   two lookups gains nothing, because there is only one lookup.
//!
//! Proxy environment variables are ignored while the filter is on: through a
//! proxy it is the proxy that resolves the name, and the filter would be
//! checking nothing.
//!
//! **The opt-out.** A self-hoster whose webhook receiver lives on the same LAN
//! sets `MICAFORGE_ALLOW_PRIVATE_OUTBOUND=true`, which switches off the range
//! checks in all three places (the scheme and host checks stay) and honours
//! proxy variables again. It is off by default, and [`client`] logs a warning at
//! boot when it is on.
//!
//! **A ceiling on everything.** Every request has a connect timeout and a total
//! timeout, every body read is counted against a [`BodyCap`], and a webhook
//! response is never read at all.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use micaforge_core::{Error, Result};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;
use serde_json::Value as Json;
use url::{Host, Url};

use crate::fetch::USER_AGENT;

/// The variable that lets outbound requests reach private addresses.
pub const ALLOW_PRIVATE_ENV: &str = "MICAFORGE_ALLOW_PRIVATE_OUTBOUND";

/// Longest any single request may take, headers and body together, unless the
/// caller sets its own.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest to wait for the connection itself.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// Redirect hops allowed before a request is treated as a loop.
pub const MAX_REDIRECTS: usize = 5;

/// True when `MICAFORGE_ALLOW_PRIVATE_OUTBOUND` is set to a true value.
///
/// Read once and remembered, because [`guard_url`] runs on request paths that
/// have no configuration in hand. A value that does not parse counts as false
/// here; [`client`] refuses it at boot, so the process never runs with one.
pub fn allow_private() -> bool {
    static ALLOW: OnceLock<bool> = OnceLock::new();
    *ALLOW.get_or_init(|| {
        parse_flag(std::env::var(ALLOW_PRIVATE_ENV).ok().as_deref()).unwrap_or(false)
    })
}

/// Read a boolean variable the way `micaforge_core::Config` reads its flags.
fn parse_flag(raw: Option<&str>) -> Result<bool> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(false);
    };
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(Error::validation(
            ALLOW_PRIVATE_ENV,
            "must be one of: true, false, 1, 0, yes, no, on, off",
        )),
    }
}

/// Build the outbound client from the environment.
///
/// Fails when `MICAFORGE_ALLOW_PRIVATE_OUTBOUND` holds something that is not a
/// boolean, so a typo stops the boot instead of silently meaning "off".
pub fn client() -> Result<reqwest::Client> {
    let allow = parse_flag(std::env::var(ALLOW_PRIVATE_ENV).ok().as_deref())?;
    if allow {
        tracing::warn!(
            "{ALLOW_PRIVATE_ENV} is on: crawls, imports and webhooks may reach loopback, \
             private and link-local addresses, including this host's own services"
        );
    }
    build_client(allow)
}

/// Build an outbound client with the private-range filter on or off.
///
/// One client for the whole process, because `reqwest::Client` holds the
/// connection pool: building one per request is the classic way to turn a
/// crawl into a TLS handshake storm.
pub fn build_client(allow_private: bool) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(redirect_policy(allow_private))
        // A crawler holding sockets open to a hundred customer domains buys
        // nothing: the next fetch of that host is a day away.
        .pool_max_idle_per_host(2);

    if !allow_private {
        builder = builder.dns_resolver(Arc::new(GuardedResolver)).no_proxy();
    }

    builder
        .build()
        .map_err(|e| Error::Internal(format!("could not build the outbound client: {e}")))
}

// ---------------------------------------------------------------------------
// The URL and redirect checks
// ---------------------------------------------------------------------------

/// Why a URL was refused before any socket was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not `http` or `https`.
    Scheme(String),
    /// No host at all.
    NoHost,
    /// A literal address in a range [`is_local`] names.
    LocalAddress(String),
    /// More redirect hops than [`MAX_REDIRECTS`].
    TooManyRedirects,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Scheme(scheme) => write!(f, "`{scheme}` is not a scheme this crawler fetches"),
            Refusal::NoHost => f.write_str("has no host"),
            Refusal::LocalAddress(host) => {
                write!(f, "`{host}` is an address on the local network")
            }
            Refusal::TooManyRedirects => {
                write!(f, "more than {MAX_REDIRECTS} redirects")
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// Check a parsed URL: scheme, host, and a literal address against the ranges.
///
/// A hostname passes here and is judged by [`GuardedResolver`] when the
/// connector resolves it.
pub fn check_target(url: &Url, allow_private: bool) -> std::result::Result<(), Refusal> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Refusal::Scheme(url.scheme().to_string()));
    }

    let ip = match url.host() {
        None => return Err(Refusal::NoHost),
        Some(Host::Domain("")) => return Err(Refusal::NoHost),
        Some(Host::Domain(_)) => return Ok(()),
        Some(Host::Ipv4(v4)) => IpAddr::V4(v4),
        Some(Host::Ipv6(v6)) => IpAddr::V6(v6),
    };

    if !allow_private && is_local(ip) {
        return Err(Refusal::LocalAddress(ip.to_string()));
    }
    Ok(())
}

/// Judge one redirect hop. `previous` is how many URLs the chain has already
/// requested, the original one included, which is what reqwest hands a policy.
pub fn check_redirect(
    next: &Url,
    previous: usize,
    allow_private: bool,
) -> std::result::Result<(), Refusal> {
    if previous > MAX_REDIRECTS {
        return Err(Refusal::TooManyRedirects);
    }
    check_target(next, allow_private)
}

/// The redirect policy every outbound client uses.
fn redirect_policy(allow_private: bool) -> redirect::Policy {
    redirect::Policy::custom(move |attempt| {
        match check_redirect(attempt.url(), attempt.previous().len(), allow_private) {
            Ok(()) => attempt.follow(),
            Err(refusal) => attempt.error(refusal),
        }
    })
}

/// Check a URL before anything opens a socket to it.
///
/// Refuses anything that is not `http` or `https`, anything with no host, and
/// any literal address [`is_local`] names, unless
/// `MICAFORGE_ALLOW_PRIVATE_OUTBOUND` is on. Numeric spellings such as
/// `http://2130706433/` and `http://0x7f.1/` are normalised to dotted form by
/// the URL parser first, so they are judged as the address they are.
///
/// This is the first of three checks, not the only one: a hostname passes here
/// and is judged at resolution time, and every redirect hop is judged again.
/// See the module documentation.
pub fn guard_url(raw: &str) -> Result<Url> {
    guard_url_with(raw, allow_private())
}

/// [`guard_url`] with the private-range filter given explicitly.
pub fn guard_url_with(raw: &str, allow_private: bool) -> Result<Url> {
    let url = Url::parse(raw.trim())
        .map_err(|e| Error::validation("url", format!("`{raw}` is not a URL: {e}")))?;
    check_target(&url, allow_private).map_err(|r| Error::validation("url", r.to_string()))?;
    Ok(url)
}

// ---------------------------------------------------------------------------
// Address classification
// ---------------------------------------------------------------------------

/// Is this an address outbound requests must not reach?
///
/// True for the machine itself, its networks, and every range that is not a
/// public unicast destination:
///
/// - IPv4: `0.0.0.0/8`, loopback `127/8`, RFC 1918 (`10/8`, `172.16/12`,
///   `192.168/16`), link-local `169.254/16` (where every cloud metadata service
///   lives), carrier-grade NAT `100.64/10`, IETF protocol assignments
///   `192.0.0/24`, the three documentation nets, benchmarking `198.18/15`,
///   multicast `224/4`, and reserved `240/4` with the broadcast address.
/// - IPv6: unspecified, loopback, link-local `fe80::/10`, the old site-local
///   `fec0::/10`, unique-local `fc00::/7`, multicast `ff00::/8`, NAT64
///   `64:ff9b::/96` and `64:ff9b:1::/48`, discard-only `100::/64`,
///   documentation `2001:db8::/32` and `3fff::/20`.
/// - An IPv4 address carried inside IPv6 (mapped `::ffff:a.b.c.d`, compatible
///   `::a.b.c.d`, or 6to4 `2002:AABB:CCDD::`) is judged as the IPv4 address it
///   carries, because that is the address that ends up dialled.
///
/// All of NAT64 is refused rather than judged by its embedded address, so an
/// IPv6-only install behind DNS64 needs `MICAFORGE_ALLOW_PRIVATE_OUTBOUND`.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_local_v4(v4),
        IpAddr::V6(v6) => is_local_v6(v6),
    }
}

fn is_local_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    a == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_documentation()
        || ip.is_multicast()
        || ip.is_broadcast()
        // 100.64.0.0/10, carrier-grade NAT: a private network in every way
        // that matters here.
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24, IETF protocol assignments (DS-Lite, NAT64 discovery).
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15, benchmarking.
        || (a == 198 && (b & 0xfe) == 18)
        // 240.0.0.0/4, reserved.
        || a >= 240
}

fn is_local_v6(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return true;
    }
    // `::ffff:a.b.c.d` and `::a.b.c.d`. Checked after loopback, because the
    // standard library reads `::1` as the compatible form of `0.0.0.1`.
    if let Some(v4) = ip.to_ipv4() {
        return is_local_v4(v4);
    }

    let s = ip.segments();
    // fe80::/10 link-local and fec0::/10 site-local, one mask apart.
    (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        // fc00::/7, unique local. The standard-library predicate for this is
        // still unstable, so the prefix is tested directly.
        || (s[0] & 0xfe00) == 0xfc00
        // 64:ff9b::/96, the well-known NAT64 prefix.
        || (s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0])
        // 64:ff9b:1::/48, local-use NAT64.
        || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1)
        // 100::/64, discard-only.
        || (s[0] == 0x100 && s[1..4] == [0, 0, 0])
        // 2001:db8::/32 and 3fff::/20, documentation.
        || (s[0] == 0x2001 && s[1] == 0xdb8)
        || (s[0] == 0x3fff && s[1] < 0x1000)
        // 2002::/16, 6to4, carrying an IPv4 address in the next 32 bits.
        || (s[0] == 0x2002
            && is_local_v4(Ipv4Addr::new(
                (s[1] >> 8) as u8,
                s[1] as u8,
                (s[2] >> 8) as u8,
                s[2] as u8,
            )))
}

// ---------------------------------------------------------------------------
// The resolver
// ---------------------------------------------------------------------------

/// A DNS resolver that never hands the connector a refused address.
///
/// Resolves with the system resolver, as reqwest would by default, then drops
/// every address [`is_local`] names. A name with at least one public address
/// connects to those; a name with none fails with [`Unreachable`].
#[derive(Debug, Default, Clone, Copy)]
pub struct GuardedResolver;

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let found = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let kept = public_only(&host, found)?;
            let addrs: Addrs = Box::new(kept.into_iter());
            Ok(addrs)
        })
    }
}

/// Keep the addresses outbound requests may reach.
fn public_only(
    host: &str,
    found: impl IntoIterator<Item = SocketAddr>,
) -> std::result::Result<Vec<SocketAddr>, Unreachable> {
    let (kept, dropped): (Vec<SocketAddr>, Vec<SocketAddr>) =
        found.into_iter().partition(|addr| !is_local(addr.ip()));

    if kept.is_empty() {
        return Err(Unreachable {
            host: host.to_string(),
            refused: dropped.len(),
        });
    }
    if !dropped.is_empty() {
        tracing::debug!(
            host,
            refused = dropped.len(),
            kept = kept.len(),
            "dropped local addresses from a DNS answer"
        );
    }
    Ok(kept)
}

/// A name that resolved to nothing outbound requests may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreachable {
    host: String,
    refused: usize,
}

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.refused == 0 {
            write!(f, "`{}` resolved to no address", self.host)
        } else {
            write!(
                f,
                "`{}` resolves only to local or private addresses, which outbound requests \
                 may not reach unless {ALLOW_PRIVATE_ENV} is on",
                self.host
            )
        }
    }
}

impl std::error::Error for Unreachable {}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

/// A running byte budget for a body that arrives in chunks.
///
/// The count is kept as the body streams, so a server that advertises nothing
/// and sends forever costs one budget's worth and then stops. Reading first and
/// measuring afterwards would not.
#[derive(Debug, Clone, Copy)]
pub struct BodyCap {
    limit: u64,
    seen: u64,
}

impl BodyCap {
    /// A budget of `limit` bytes.
    pub fn new(limit: u64) -> BodyCap {
        BodyCap { limit, seen: 0 }
    }

    /// Count a chunk of `len` bytes and return how many of them fit: all of
    /// them, or the part still under the cap when this chunk crosses it.
    pub fn admit(&mut self, len: usize) -> usize {
        let room = self.limit.saturating_sub(self.seen);
        self.seen = self.seen.saturating_add(len as u64);
        room.min(len as u64) as usize
    }

    /// True once more than `limit` bytes have been offered.
    pub fn exceeded(&self) -> bool {
        self.seen > self.limit
    }

    /// Bytes offered so far, including any past the cap.
    pub fn seen(&self) -> u64 {
        self.seen
    }
}

/// Read a response body, stopping at `max_bytes`.
///
/// Returns the body and whether the tail was dropped. The connection is let go
/// as soon as the cap is crossed, so the rest is never downloaded.
pub async fn read_capped(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool)> {
    let url = redacted(response.url());
    let mut cap = BodyCap::new(max_bytes as u64);
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::Upstream(format!("reading {url}: {}", describe(&e))))?
    {
        let keep = cap.admit(chunk.len());
        buf.extend_from_slice(&chunk[..keep]);
        if cap.exceeded() {
            tracing::warn!(url, max_bytes, "response truncated at the size cap");
            return Ok((buf, true));
        }
    }
    Ok((buf, false))
}

// ---------------------------------------------------------------------------
// Posting, and saying what went wrong
// ---------------------------------------------------------------------------

/// What kind of failure a request met, at the level a user may be told.
///
/// Deliberately coarse. A user who can make the server send a request must not
/// learn from the answer which internal names exist or which ports are open, so
/// a name that resolves to a private address, a name that does not resolve and
/// a refused connection all read as [`FailureKind::CouldNotConnect`]. The
/// detail goes to the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The URL itself was refused by [`guard_url`]: the caller's own input.
    NotAllowed,
    /// No connection: DNS, refusal, TLS, a refused redirect.
    CouldNotConnect,
    /// The connect or the total timeout ran out.
    TimedOut,
    /// The server answered with something other than 2xx.
    Status(u16),
    /// Anything else.
    Failed,
}

impl FailureKind {
    /// Classify a reqwest error.
    pub fn of(e: &reqwest::Error) -> FailureKind {
        if e.is_timeout() {
            FailureKind::TimedOut
        } else if e.is_connect() || e.is_redirect() {
            FailureKind::CouldNotConnect
        } else if let Some(status) = e.status() {
            FailureKind::Status(status.as_u16())
        } else {
            FailureKind::Failed
        }
    }

    /// The sentence a client may be shown. Never carries an address, a port or
    /// an operating-system error.
    pub fn public_message(&self) -> String {
        match self {
            FailureKind::NotAllowed => {
                "the URL is not one Micaforge may send to: it must be http or https and must \
                 not name a local or private address"
                    .to_string()
            }
            FailureKind::CouldNotConnect => "could not connect".to_string(),
            FailureKind::TimedOut => "timed out".to_string(),
            FailureKind::Status(code) => format!("non-2xx status {code}"),
            FailureKind::Failed => "the request failed".to_string(),
        }
    }
}

/// A failed outbound request: a coarse kind for the client, a detail for the log.
#[derive(Debug, Clone)]
pub struct Failure {
    /// What may be said about it.
    pub kind: FailureKind,
    /// What happened, for the log. Names the host but never the path or query,
    /// because a webhook URL routinely carries its secret there.
    detail: String,
}

impl Failure {
    /// The log-only description.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// The sentence a client may be shown.
    pub fn public_message(&self) -> String {
        self.kind.public_message()
    }

    /// Describe a reqwest error against the URL it was for.
    pub fn from_reqwest(target: &Url, e: &reqwest::Error) -> Failure {
        Failure {
            kind: FailureKind::of(e),
            detail: format!("{}: {}", redacted(target), describe(e)),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl From<Failure> for Error {
    fn from(failure: Failure) -> Error {
        match failure.kind {
            FailureKind::NotAllowed => Error::validation("url", failure.detail),
            _ => Error::Upstream(failure.detail),
        }
    }
}

/// POST a JSON document to a user-configured URL and return the status.
///
/// The URL is guarded first, the request runs under `timeout`, and the response
/// body is never read: a receiver has nothing to say beyond its status, and not
/// reading the body is the cheapest cap there is. A non-2xx answer is a
/// [`FailureKind::Status`].
pub async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &Json,
    timeout: Duration,
) -> std::result::Result<u16, Failure> {
    let target = guard_url(url).map_err(|e| Failure {
        kind: FailureKind::NotAllowed,
        detail: e.to_string(),
    })?;

    let response = client
        .post(target.clone())
        .timeout(timeout)
        .json(body)
        .send()
        .await
        .map_err(|e| Failure::from_reqwest(&target, &e))?;

    let status = response.status();
    drop(response);
    if !status.is_success() {
        return Err(Failure {
            kind: FailureKind::Status(status.as_u16()),
            detail: format!("{} returned HTTP {status}", redacted(&target)),
        });
    }
    Ok(status.as_u16())
}

/// `scheme://host[:port]`, for a log line that must not carry a path or query.
pub fn redacted(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{}://{host}:{port}", url.scheme()),
        (Some(host), None) => format!("{}://{host}", url.scheme()),
        _ => url.scheme().to_string(),
    }
}

/// A reqwest error and its causes, without the URL reqwest puts at the top.
fn describe(e: &reqwest::Error) -> String {
    let mut parts = Vec::new();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    if parts.is_empty() {
        // No cause to show, so the error's own text is all there is. Strip
        // the URL reqwest appends in brackets.
        let text = e.to_string();
        return match text.find(" for url (") {
            Some(at) => text[..at].to_string(),
            None => text,
        };
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn ip(raw: &str) -> IpAddr {
        raw.parse()
            .unwrap_or_else(|_| panic!("{raw} is not an address"))
    }

    #[test]
    fn every_local_ipv4_range_is_refused() {
        for raw in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "10.255.255.255",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "127.8.9.10",
            "169.254.169.254",
            "169.254.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(is_local(ip(raw)), "{raw} should be refused");
        }
    }

    #[test]
    fn public_ipv4_neighbours_of_those_ranges_are_allowed() {
        for raw in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "169.253.0.1",
            "192.0.1.1",
            "192.169.0.1",
            "198.17.0.1",
            "198.20.0.1",
            "223.255.255.254",
        ] {
            assert!(!is_local(ip(raw)), "{raw} should be allowed");
        }
    }

    #[test]
    fn every_local_ipv6_range_is_refused() {
        for raw in [
            "::",
            "::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "fc00::1",
            "fd12:3456:789a::1",
            "ff02::1",
            "ff05::2",
            "64:ff9b::808:808",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "100::1",
            "2001:db8::1",
            "3fff::1",
            "3fff:fff::1",
            // 6to4 carrying 192.168.0.1 and 127.0.0.1.
            "2002:c0a8:1::1",
            "2002:7f00:1::1",
        ] {
            assert!(is_local(ip(raw)), "{raw} should be refused");
        }
    }

    #[test]
    fn an_ipv4_address_in_ipv6_clothing_is_judged_as_itself() {
        for raw in [
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:100.64.0.1",
            "::ffff:0.0.0.0",
            "::ffff:255.255.255.255",
            "::ffff:224.0.0.1",
            "::ffff:192.0.2.1",
            // The deprecated compatible form.
            "::127.0.0.1",
            "::10.0.0.1",
            "::169.254.169.254",
        ] {
            assert!(is_local(ip(raw)), "{raw} should be refused");
        }
        assert!(!is_local(ip("::ffff:93.184.216.34")));
        assert!(!is_local(ip("::93.184.216.34")));
    }

    #[test]
    fn public_ipv6_is_allowed() {
        for raw in [
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "2a00:1450:4001::1",
            "2002:5db8:d822::1",
            "3fff:1000::1",
            "64:ff9b:2::1",
        ] {
            assert!(!is_local(ip(raw)), "{raw} should be allowed");
        }
    }

    #[test]
    fn the_url_guard_refuses_literal_local_addresses_in_every_spelling() {
        for raw in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:8123/",
            "http://10.0.0.5/robots.txt",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:a9fe:a9fe]/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://100.100.0.1/",
            // Numeric spellings the URL parser normalises to 127.0.0.1.
            "http://2130706433/",
            "http://0x7f.1/",
            "http://0177.0.0.1/",
            "http://0/",
        ] {
            assert!(
                guard_url_with(raw, false).is_err(),
                "{raw} should be refused"
            );
        }
        assert!(guard_url_with("file:///etc/passwd", false).is_err());
        assert!(guard_url_with("gopher://example.com/", false).is_err());
        assert!(guard_url_with("https://example.com/robots.txt", false).is_ok());
        assert!(guard_url_with("http://93.184.216.34/", false).is_ok());
    }

    #[test]
    fn the_opt_out_allows_local_addresses_but_never_other_schemes() {
        assert!(guard_url_with("http://192.168.1.20:9000/hook", true).is_ok());
        assert!(guard_url_with("http://[::1]/", true).is_ok());
        assert!(guard_url_with("file:///etc/passwd", true).is_err());
        assert!(guard_url_with("ftp://192.168.1.20/", true).is_err());
    }

    #[test]
    fn a_redirect_hop_is_judged_like_the_first_url() {
        let url = |raw: &str| Url::parse(raw).unwrap();

        assert_eq!(
            check_redirect(&url("https://example.com/next"), 1, false),
            Ok(())
        );
        // A hostname passes here; the resolver judges what it resolves to.
        assert_eq!(check_redirect(&url("http://localhost/"), 1, false), Ok(()));
        assert_eq!(
            check_redirect(&url("http://169.254.169.254/latest/"), 1, false),
            Err(Refusal::LocalAddress("169.254.169.254".into()))
        );
        assert_eq!(
            check_redirect(&url("http://[::ffff:10.0.0.1]/"), 2, false),
            Err(Refusal::LocalAddress("::ffff:10.0.0.1".into()))
        );
        assert_eq!(
            check_redirect(&url("file:///etc/passwd"), 1, false),
            Err(Refusal::Scheme("file".into()))
        );
        assert_eq!(
            check_redirect(&url("gopher://example.com/"), 1, true),
            Err(Refusal::Scheme("gopher".into()))
        );
        assert_eq!(check_redirect(&url("http://10.0.0.1/"), 1, true), Ok(()));
    }

    #[test]
    fn the_redirect_chain_is_capped() {
        let next = Url::parse("https://example.com/again").unwrap();
        // `previous` counts the original URL, so five hops are five redirects.
        assert_eq!(check_redirect(&next, MAX_REDIRECTS, false), Ok(()));
        assert_eq!(
            check_redirect(&next, MAX_REDIRECTS + 1, false),
            Err(Refusal::TooManyRedirects)
        );
    }

    #[test]
    fn the_resolver_keeps_public_addresses_and_drops_the_rest() {
        let addr = |raw: &str| SocketAddr::new(ip(raw), 0);

        let mixed = public_only(
            "mixed.example",
            [addr("10.0.0.1"), addr("93.184.216.34"), addr("::1")],
        )
        .unwrap();
        assert_eq!(mixed, vec![addr("93.184.216.34")]);

        let refused = public_only(
            "169.254.169.254.nip.io",
            [addr("169.254.169.254"), addr("::ffff:169.254.169.254")],
        )
        .unwrap_err();
        assert_eq!(refused.refused, 2);
        assert!(refused.to_string().contains(ALLOW_PRIVATE_ENV));

        let empty = public_only("nothing.example", []).unwrap_err();
        assert!(empty.to_string().contains("no address"));
    }

    #[test]
    fn the_body_cap_counts_across_chunks() {
        let mut cap = BodyCap::new(10);
        assert_eq!(cap.admit(4), 4);
        assert_eq!(cap.admit(6), 6);
        // Exactly at the cap is not over it.
        assert!(!cap.exceeded());
        assert_eq!(cap.admit(1), 0);
        assert!(cap.exceeded());

        let mut cap = BodyCap::new(10);
        assert_eq!(cap.admit(7), 7);
        // A chunk that crosses the cap keeps the part that fits.
        assert_eq!(cap.admit(7), 3);
        assert!(cap.exceeded());
        assert_eq!(cap.seen(), 14);

        let mut none = BodyCap::new(0);
        assert_eq!(none.admit(0), 0);
        assert!(!none.exceeded());
        assert_eq!(none.admit(1), 0);
        assert!(none.exceeded());
    }

    #[test]
    fn the_flag_reads_like_every_other_flag() {
        assert!(!parse_flag(None).unwrap());
        assert!(!parse_flag(Some("  ")).unwrap());
        assert!(parse_flag(Some("true")).unwrap());
        assert!(parse_flag(Some("ON")).unwrap());
        assert!(!parse_flag(Some("0")).unwrap());
        assert!(parse_flag(Some("maybe")).is_err());
    }

    #[test]
    fn a_failure_never_tells_the_client_an_address() {
        for kind in [
            FailureKind::NotAllowed,
            FailureKind::CouldNotConnect,
            FailureKind::TimedOut,
            FailureKind::Status(503),
            FailureKind::Failed,
        ] {
            let message = kind.public_message();
            assert!(!message.contains("127.0.0.1") && !message.contains("os error"));
        }
        assert_eq!(
            FailureKind::Status(404).public_message(),
            "non-2xx status 404"
        );
    }

    #[test]
    fn a_log_line_never_carries_the_path_or_query() {
        let url = Url::parse("https://hooks.slack.com:8443/services/T0/B0/secret?x=1").unwrap();
        assert_eq!(redacted(&url), "https://hooks.slack.com:8443");
        let url = Url::parse("http://example.com/a/b").unwrap();
        assert_eq!(redacted(&url), "http://example.com");
    }

    /// A one-shot HTTP server on loopback. Answers every connection with
    /// `response`, then a body of `body_len` bytes, and counts connections.
    async fn serve(
        response: &'static str,
        body_len: usize,
    ) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut request = [0u8; 2048];
                    let _ = socket.read(&mut request).await;
                    let _ = socket.write_all(response.as_bytes()).await;
                    let chunk = vec![b'a'; 16 * 1024];
                    let mut sent = 0;
                    while sent < body_len {
                        let n = chunk.len().min(body_len - sent);
                        if socket.write_all(&chunk[..n]).await.is_err() {
                            return;
                        }
                        sent += n;
                    }
                });
            }
        });
        (port, hits)
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_loopback_is_never_dialled() {
        let (port, hits) = serve("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok", 0).await;
        let strict = build_client(false).unwrap();

        let err = strict
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .unwrap_err();
        assert_eq!(FailureKind::of(&err), FailureKind::CouldNotConnect);
        assert!(
            describe(&err).contains("local or private"),
            "{}",
            describe(&err)
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);

        // The same name with the opt-out on is an ordinary request.
        let open = build_client(true).unwrap();
        let ok = open
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn a_redirect_to_a_literal_local_address_is_not_followed() {
        let (port, _) = serve(
            "HTTP/1.1 302 Found\r\nlocation: http://169.254.169.254/latest/meta-data/\r\ncontent-length: 0\r\n\r\n",
            0,
        )
        .await;
        // Only the redirect policy, no guarded resolver, so the loopback test
        // server is reachable and the hop is the one thing being judged.
        let client = reqwest::Client::builder()
            .redirect(redirect_policy(false))
            .build()
            .unwrap();
        let err = client
            .get(format!("http://127.0.0.1:{port}/robots.txt"))
            .send()
            .await
            .unwrap_err();
        assert!(err.is_redirect());
        assert_eq!(FailureKind::of(&err), FailureKind::CouldNotConnect);
    }

    #[tokio::test]
    async fn a_body_past_the_cap_is_cut_off_while_it_streams() {
        let (port, _) = serve(
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\n\r\n",
            4 * 1024 * 1024,
        )
        .await;
        let client = build_client(true).unwrap();
        let response = client
            .get(format!("http://localhost:{port}/big"))
            .send()
            .await
            .unwrap();

        let (body, truncated) = read_capped(response, 100_000).await.unwrap();
        assert!(truncated);
        assert_eq!(body.len(), 100_000);
    }

    #[tokio::test]
    async fn a_body_under_the_cap_is_read_whole() {
        let (port, _) = serve("HTTP/1.1 200 OK\r\ncontent-length: 5000\r\n\r\n", 5000).await;
        let client = build_client(true).unwrap();
        let response = client
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .unwrap();

        let (body, truncated) = read_capped(response, 5000).await.unwrap();
        assert!(!truncated);
        assert_eq!(body.len(), 5000);
    }
}
