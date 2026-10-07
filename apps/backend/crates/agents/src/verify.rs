//! Proving a fetch came from who it says it did.
//!
//! A user agent is a claim anyone can make. `Mozilla/5.0 (compatible;
//! GPTBot/1.2)` costs nothing to type, and a scraper that wants to be left alone
//! by a rate limiter has every reason to type it. So the agent report separates
//! *named* from *verified*, and `agent_events.verified` is a column of its own.
//!
//! Two proofs, in order of strength:
//!
//! 1. **Published ranges.** Several operators serve a machine-readable document
//!    of the CIDRs their crawlers fetch from. A source address inside one is
//!    proof. [`RangeStore`] holds those sets; [`fetch_published_ranges`] fills
//!    them.
//! 2. **Reverse DNS.** Where an operator documents a hostname suffix instead,
//!    [`verify_rdns`] checks it. The caller must do the forward-confirmation
//!    step itself (resolve the hostname back and check the address is in the
//!    answer); a suffix match on an unconfirmed PTR record proves nothing on its
//!    own, and the doc comment on that function says so again.
//!
//! Anything else is [`Verification::Unknown`], which is a third answer and not a
//! synonym for failure. Most operators publish nothing, and reporting their
//! traffic as "failed verification" would be an accusation the data does not
//! support.
//!
//! **On the no-outbound-call rule.** `PRODUCT.md` requires that a self-hosted
//! install run without any outbound network call. [`fetch_published_ranges`] is
//! the one function here that touches the network, it is never called from the
//! ingest path, and it only runs from a job an operator turns on. Verification
//! is off by default and degrades to `Unknown`, never to a broken install.

use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use micaforge_core::{Error, Result};
use serde::Serialize;
use serde_json::Value;

use crate::catalog::AgentDef;

/// The result of checking a source address against what an operator publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// The address is inside a range the operator publishes, or the hostname
    /// matches a suffix they document.
    Verified,
    /// The operator publishes ranges, and this address is not in them. Someone
    /// is using the name without the addresses.
    Failed,
    /// Nothing to check against: the operator publishes nothing, or the range
    /// set has not been fetched yet.
    #[default]
    Unknown,
}

impl Verification {
    /// The value as written to JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Verification::Verified => "verified",
            Verification::Failed => "failed",
            Verification::Unknown => "unknown",
        }
    }

    /// The `agent_events.verified` column value.
    ///
    /// `UInt8` in the schema, so the column can only say verified or not.
    /// `Failed` and `Unknown` both write `0`, and the difference between them
    /// lives in the report rather than the row — which is why an unverified
    /// fetch is never *labelled* a forgery in the UI.
    pub fn as_u8(&self) -> u8 {
        match self {
            Verification::Verified => 1,
            _ => 0,
        }
    }

    /// True only for a proved fetch.
    pub fn is_verified(&self) -> bool {
        matches!(self, Verification::Verified)
    }
}

impl std::fmt::Display for Verification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The fetched CIDR sets, one per agent slug.
///
/// Held in memory and shared read-only by the ingest path. Ranges are checked on
/// every agent hit, so the check is a masked comparison over a `Vec` rather than
/// anything cleverer: the published sets run to a few hundred prefixes at most,
/// and a flat scan over contiguous memory beats a tree at that size.
#[derive(Debug, Clone, Default)]
pub struct RangeStore {
    by_slug: HashMap<String, Vec<IpNet>>,
}

impl RangeStore {
    /// An empty store. Every lookup returns [`Verification::Unknown`] until
    /// something is inserted, which is the correct state for an install that has
    /// never refreshed its ranges.
    pub fn new() -> Self {
        RangeStore {
            by_slug: HashMap::new(),
        }
    }

    /// Replace the ranges for one agent.
    ///
    /// Replace rather than merge: an operator's document is the whole truth
    /// about their address space at that moment, and a range they removed must
    /// stop verifying. Inserting an empty vector removes the entry entirely, so
    /// a fetch that returned nothing cannot turn every hit into `Failed`.
    pub fn insert(&mut self, slug: &str, cidrs: Vec<IpNet>) {
        if cidrs.is_empty() {
            self.by_slug.remove(slug);
            return;
        }
        let mut cidrs = cidrs;
        cidrs.sort();
        cidrs.dedup();
        self.by_slug.insert(slug.to_string(), cidrs);
    }

    /// Check a source address against one agent's published ranges.
    pub fn verify(&self, slug: &str, ip: IpAddr) -> Verification {
        match self.by_slug.get(slug) {
            None => Verification::Unknown,
            Some(nets) => {
                if nets.iter().any(|net| net_contains(net, ip)) {
                    Verification::Verified
                } else {
                    Verification::Failed
                }
            }
        }
    }

    /// True when ranges have been loaded for this agent.
    pub fn has(&self, slug: &str) -> bool {
        self.by_slug.contains_key(slug)
    }

    /// The number of prefixes held for one agent.
    pub fn len_for(&self, slug: &str) -> usize {
        self.by_slug.get(slug).map_or(0, Vec::len)
    }

    /// Every agent slug with loaded ranges.
    pub fn slugs(&self) -> Vec<&str> {
        self.by_slug.keys().map(String::as_str).collect()
    }

    /// True when nothing has been loaded at all.
    pub fn is_empty(&self) -> bool {
        self.by_slug.is_empty()
    }
}

/// Whether a CIDR contains an address.
///
/// Written out rather than taken from `ipnet`'s trait so that the comparison is
/// visible: mask both sides to the prefix length and compare. A `/0` is handled
/// separately because shifting an integer by its own width is undefined.
fn net_contains(net: &IpNet, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpNet::V4(net), IpAddr::V4(addr)) => {
            let len = net.prefix_len() as u32;
            if len == 0 {
                return true;
            }
            let mask = u32::MAX << (32 - len);
            (u32::from(net.addr()) & mask) == (u32::from(addr) & mask)
        }
        (IpNet::V6(net), IpAddr::V6(addr)) => {
            let len = net.prefix_len() as u32;
            if len == 0 {
                return true;
            }
            let mask = u128::MAX << (128 - len);
            (u128::from(net.addr()) & mask) == (u128::from(addr) & mask)
        }
        // A v4 address is never inside a v6 prefix, and the reverse. Mapped
        // addresses are normalised by the caller, not silently here.
        _ => false,
    }
}

/// Fetch and parse an operator's published IP range document.
///
/// Tolerant by design. There is no standard for these documents and the shapes
/// in the wild differ on every axis that could differ:
///
/// ```jsonc
/// { "prefixes": [ { "ipv4Prefix": "23.98.142.176/28" } ] }   // OpenAI, Google, Bing
/// { "prefixes": [ { "ip_prefix":  "3.5.140.0/22" } ] }       // AWS style
/// { "result":   { "ipv4_cidrs": [ "173.245.48.0/20" ] } }    // Cloudflare style
/// [ "203.0.113.0/24" ]                                       // a plain list
/// ```
///
/// So the parser walks the whole JSON tree and takes every string that is a
/// valid CIDR, plus every string under a key that names a prefix even when it
/// carries a bare address. Nothing about the document's structure is assumed
/// beyond it being JSON, which means a shape change at the operator's end does
/// not silently empty a customer's verification.
///
/// Returns [`Error::NotFound`] when the agent publishes no document at all, so a
/// caller can tell "this operator has no list" apart from "the fetch failed".
pub async fn fetch_published_ranges(
    client: &reqwest::Client,
    def: &AgentDef,
) -> Result<Vec<IpNet>> {
    let url = def.ip_ranges_url.ok_or_else(|| {
        Error::NotFound(format!(
            "{} publishes no machine-readable IP range document",
            def.slug
        ))
    })?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Upstream(format!("fetching {url}: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        return Err(Error::Upstream(format!("{url} returned HTTP {status}")));
    }

    let body = response
        .text()
        .await
        .map_err(|e| Error::Upstream(format!("reading {url}: {e}")))?;

    let ranges = parse_range_document(&body)?;
    if ranges.is_empty() {
        return Err(Error::Upstream(format!(
            "{url} contained no usable prefixes"
        )));
    }
    tracing::debug!(
        agent = def.slug,
        prefixes = ranges.len(),
        "fetched published ranges"
    );
    Ok(ranges)
}

/// Refresh every agent that publishes ranges into a fresh store.
///
/// Returns the store alongside one line per agent describing what happened, so
/// the job that calls this can report partial success honestly instead of
/// throwing away a good fetch because a different operator's endpoint was down.
pub async fn fetch_all_published(client: &reqwest::Client) -> (RangeStore, Vec<RefreshOutcome>) {
    let mut store = RangeStore::new();
    let mut outcomes = Vec::new();

    for def in crate::catalog::verifiable() {
        match fetch_published_ranges(client, def).await {
            Ok(ranges) => {
                let count = ranges.len();
                store.insert(def.slug, ranges);
                outcomes.push(RefreshOutcome {
                    slug: def.slug,
                    prefixes: count,
                    error: None,
                });
            }
            Err(e) => {
                tracing::warn!(agent = def.slug, error = %e, "range refresh failed");
                outcomes.push(RefreshOutcome {
                    slug: def.slug,
                    prefixes: 0,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    (store, outcomes)
}

/// What one agent's range refresh did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefreshOutcome {
    /// The agent this outcome is about.
    pub slug: &'static str,
    /// How many prefixes were loaded. Zero on failure.
    pub prefixes: usize,
    /// The failure, when there was one.
    pub error: Option<String>,
}

/// Keys whose string values are addresses even without a prefix length.
const PREFIX_KEYS: &[&str] = &[
    "ipv4prefix",
    "ipv6prefix",
    "prefix",
    "cidr",
    "ip_prefix",
    "ipv6_prefix",
    "ipv4_cidrs",
    "ipv6_cidrs",
    "ipv4",
    "ipv6",
    "ip",
    "address",
];

/// Parse a published range document of any shape. Split out from the fetch so it
/// can be tested against a real document body with no network.
pub fn parse_range_document(body: &str) -> Result<Vec<IpNet>> {
    let value: Value = serde_json::from_str(body)?;
    let mut out = Vec::new();
    collect_prefixes(&value, None, &mut out);
    out.sort();
    out.dedup();
    Ok(out)
}

/// Walk the tree, carrying the key a value was found under.
fn collect_prefixes(value: &Value, key: Option<&str>, out: &mut Vec<IpNet>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                collect_prefixes(v, Some(k.as_str()), out);
            }
        }
        // An array inherits its key, so `{"ipv4_cidrs": ["1.2.3.0/24"]}` works
        // exactly like `{"prefixes": [{"cidr": "1.2.3.0/24"}]}`.
        Value::Array(items) => {
            for item in items {
                collect_prefixes(item, key, out);
            }
        }
        Value::String(s) => {
            let named = key.is_some_and(|k| {
                let k = k.to_ascii_lowercase();
                PREFIX_KEYS.contains(&k.as_str())
            });
            // A string with a slash that parses as a network is taken whatever
            // it was called; a bare address is taken only under a key that says
            // it is one, so a version string or a date can never slip in.
            if named || s.contains('/') {
                if let Some(net) = parse_cidr(s) {
                    out.push(net);
                }
            }
        }
        _ => {}
    }
}

/// Parse a CIDR, accepting a bare address as a host route.
fn parse_cidr(s: &str) -> Option<IpNet> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(net) = IpNet::from_str(s) {
        return Some(net);
    }
    match IpAddr::from_str(s) {
        Ok(IpAddr::V4(addr)) => Ipv4Net::new(addr, 32).ok().map(IpNet::V4),
        Ok(IpAddr::V6(addr)) => Ipv6Net::new(addr, 128).ok().map(IpNet::V6),
        Err(_) => None,
    }
}

/// Check a reverse-DNS hostname against the suffixes an operator documents.
///
/// **This is half of the check.** The full procedure, as Google, Bing and Apple
/// all document it, is: resolve the source address to a hostname (PTR), confirm
/// the hostname ends with the operator's domain, then resolve that hostname back
/// to an address and confirm it is the one you started with. A PTR record is
/// controlled by whoever holds the address block, so skipping the forward
/// confirmation lets anyone claim any name. This function performs the suffix
/// step only; the caller owns the two lookups and the comparison.
pub fn verify_rdns(hostname: &str, def: &AgentDef) -> Verification {
    if def.rdns_suffixes.is_empty() {
        return Verification::Unknown;
    }
    let host = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Verification::Failed;
    }
    let matched = def.rdns_suffixes.iter().any(|suffix| {
        let suffix = suffix.trim_start_matches('.').to_ascii_lowercase();
        crate::host_matches(&host, &suffix)
    });
    if matched {
        Verification::Verified
    } else {
        Verification::Failed
    }
}

/// Normalise an IPv4-mapped IPv6 address (`::ffff:1.2.3.4`) down to its IPv4
/// form, so a dual-stack proxy in front of the app does not defeat every range
/// check. Any other address is returned unchanged.
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(addr) => match addr.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(addr),
        },
        v4 => v4,
    }
}

/// Parse a source address from a header value, normalising the mapped form.
///
/// Returns `None` rather than an error: an unparseable `X-Forwarded-For` is a
/// misconfigured proxy, and dropping the hit over it would lose real data.
pub fn parse_ip(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix('[')
        .and_then(|r| r.split(']').next())
        .unwrap_or(raw);
    IpAddr::from_str(raw).ok().map(normalize_ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::find_by_slug;

    fn net(s: &str) -> IpNet {
        IpNet::from_str(s).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    #[test]
    fn an_empty_store_knows_nothing() {
        let store = RangeStore::new();
        assert_eq!(store.verify("gptbot", ip("1.2.3.4")), Verification::Unknown);
        assert!(store.is_empty());
    }

    #[test]
    fn a_loaded_store_verifies_and_fails() {
        let mut store = RangeStore::new();
        store.insert(
            "gptbot",
            vec![net("23.98.142.176/28"), net("2600:1f00::/40")],
        );

        assert_eq!(
            store.verify("gptbot", ip("23.98.142.180")),
            Verification::Verified
        );
        assert_eq!(
            store.verify("gptbot", ip("23.98.142.200")),
            Verification::Failed
        );
        assert_eq!(
            store.verify("gptbot", ip("2600:1f00:1::9")),
            Verification::Verified
        );
        // A different agent's traffic is not checked against these ranges.
        assert_eq!(
            store.verify("claudebot", ip("23.98.142.180")),
            Verification::Unknown
        );
    }

    #[test]
    fn boundaries_are_inclusive_at_both_ends() {
        let mut store = RangeStore::new();
        store.insert("x", vec![net("10.0.0.0/24")]);
        assert_eq!(store.verify("x", ip("10.0.0.0")), Verification::Verified);
        assert_eq!(store.verify("x", ip("10.0.0.255")), Verification::Verified);
        assert_eq!(store.verify("x", ip("10.0.1.0")), Verification::Failed);
        // /32 and /0 are the two shift edge cases.
        store.insert("host", vec![net("192.0.2.7/32")]);
        assert_eq!(
            store.verify("host", ip("192.0.2.7")),
            Verification::Verified
        );
        assert_eq!(store.verify("host", ip("192.0.2.8")), Verification::Failed);
        store.insert("all", vec![net("0.0.0.0/0")]);
        assert_eq!(
            store.verify("all", ip("198.51.100.1")),
            Verification::Verified
        );
    }

    #[test]
    fn families_do_not_cross() {
        let mut store = RangeStore::new();
        store.insert("x", vec![net("0.0.0.0/0")]);
        assert_eq!(store.verify("x", ip("::1")), Verification::Failed);
    }

    #[test]
    fn an_empty_fetch_never_turns_into_a_failure() {
        let mut store = RangeStore::new();
        store.insert("gptbot", vec![net("10.0.0.0/8")]);
        store.insert("gptbot", vec![]);
        assert_eq!(
            store.verify("gptbot", ip("10.0.0.1")),
            Verification::Unknown
        );
    }

    #[test]
    fn the_openai_and_google_document_shape_parses() {
        let body = r#"{
            "creationTime": "2025-01-01T00:00:00.000000",
            "prefixes": [
                { "ipv4Prefix": "23.98.142.176/28" },
                { "ipv6Prefix": "2a05:d014:d0c:c400::/56" }
            ]
        }"#;
        let ranges = parse_range_document(body).unwrap();
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn other_shapes_parse_too() {
        let aws = r#"{"prefixes":[{"ip_prefix":"3.5.140.0/22","region":"eu-west-1"}]}"#;
        assert_eq!(parse_range_document(aws).unwrap().len(), 1);

        let cloudflare =
            r#"{"result":{"ipv4_cidrs":["173.245.48.0/20"],"ipv6_cidrs":["2400:cb00::/32"]}}"#;
        assert_eq!(parse_range_document(cloudflare).unwrap().len(), 2);

        let plain = r#"["203.0.113.0/24","198.51.100.0/24"]"#;
        assert_eq!(parse_range_document(plain).unwrap().len(), 2);

        // A bare address under a naming key becomes a host route.
        let bare = r#"{"addresses":[{"ip":"203.0.113.9"}]}"#;
        assert_eq!(
            parse_range_document(bare).unwrap(),
            vec![net("203.0.113.9/32")]
        );
    }

    #[test]
    fn noise_in_the_document_is_ignored() {
        let body = r#"{
            "creationTime": "2025/01/01",
            "version": "1.0/beta",
            "docs": "https://example.com/a/b",
            "prefixes": [{ "ipv4Prefix": "192.0.2.0/24" }]
        }"#;
        assert_eq!(
            parse_range_document(body).unwrap(),
            vec![net("192.0.2.0/24")]
        );
    }

    #[test]
    fn a_broken_document_is_an_error_not_an_empty_set() {
        assert!(parse_range_document("{oops").is_err());
    }

    #[test]
    fn rdns_suffixes_match_only_on_a_label_boundary() {
        let googlebot = find_by_slug("googlebot").unwrap();
        assert_eq!(
            verify_rdns("crawl-66-249-66-1.googlebot.com", googlebot),
            Verification::Verified
        );
        assert_eq!(
            verify_rdns("crawl-66-249-66-1.googlebot.com.", googlebot),
            Verification::Verified
        );
        // The classic near-miss: a domain that merely ends in the same letters.
        assert_eq!(
            verify_rdns("notgooglebot.com", googlebot),
            Verification::Failed
        );
        assert_eq!(
            verify_rdns("googlebot.com.example.net", googlebot),
            Verification::Failed
        );

        // An agent with no documented scheme cannot be checked this way.
        let ccbot = find_by_slug("ccbot").unwrap();
        assert_eq!(
            verify_rdns("crawl.example.org", ccbot),
            Verification::Unknown
        );
    }

    #[test]
    fn mapped_addresses_are_normalised() {
        assert_eq!(parse_ip("::ffff:192.0.2.7"), Some(ip("192.0.2.7")));
        assert_eq!(parse_ip("[2001:db8::1]"), Some(ip("2001:db8::1")));
        assert_eq!(parse_ip(" 192.0.2.7 "), Some(ip("192.0.2.7")));
        assert_eq!(parse_ip("not-an-ip"), None);
    }

    #[test]
    fn the_verified_column_only_says_yes_for_proof() {
        assert_eq!(Verification::Verified.as_u8(), 1);
        assert_eq!(Verification::Failed.as_u8(), 0);
        assert_eq!(Verification::Unknown.as_u8(), 0);
    }
}
