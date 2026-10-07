//! Deciding which address a request actually came from.
//!
//! This is a security decision, not a convenience. The address is folded into
//! the visitor digest (`docs/CONTRACT.md` §7.2), it keys the rate limiters, and
//! it is what an agent fetch is verified against. Believing the wrong one means
//! every visitor behind a proxy collapses into a single hash, or one client can
//! forge somebody else's identity by writing a header.
//!
//! ## No header is trusted unless one is named
//!
//! `MICAFORGE_TRUSTED_PROXY_HEADER` names the one header the reverse proxy in
//! front of the binary writes the client address into. **Unset, empty or
//! `none` means no header is believed at all**, and the socket address is the
//! only source. That is the safe default: on an install that exposes the port
//! directly, any client could otherwise write `X-Forwarded-For: <anyone>` and
//! become somebody else.
//!
//! The shipped compose stack sets it to `x-real-ip`, which Caddy fills from its
//! own `{client_ip}` (the address after Caddy's `trusted_proxies` walk), so the
//! value arrives as one address. A CDN that writes its own single-value header
//! works the same way (`cf-connecting-ip`, `true-client-ip`), and so does
//! `x-forwarded-for` when exactly one proxy appends to it. The header is read
//! once per process: changing whose claim about an address the server believes
//! should require a restart, not a request.
//!
//! ## The rule, once a header is named
//!
//! `X-Forwarded-For` is written left to right: the leftmost entry is whatever
//! the *first hop claimed*, which any client can put there, and each proxy
//! appends the address it actually observed. So the value is read from the
//! **right**, skipping entries that are infrastructure rather than clients
//! (loopback, private ranges, link-local, carrier-grade NAT), and the first
//! genuinely public address wins. A single-value header is the degenerate case
//! of the same walk.
//!
//! Two fallbacks, in order. If every entry is private, which is the ordinary
//! case for a request from a LAN, the rightmost parseable entry is used,
//! because on such an install a private address really is the client. With no
//! usable header at all, the socket address is the truth and nothing can be
//! forged.
//!
//! Note what this module does *not* do: it has no allow-list of trusted proxy
//! addresses. The deployment shape `PRODUCT.md` describes puts one reverse proxy
//! directly in front of the binary and nothing else can reach it, so the header
//! either arrives from that proxy or does not arrive at all.
//!
//! ## Never the address itself in a log or a key
//!
//! [`address_tag`] is what the access log, the rate limiters and every other
//! place that needs to tell two callers apart write instead of the address: a
//! short keyed digest whose key is random, lives only in this process's memory
//! and is replaced every UTC day.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Mutex, OnceLock};

use axum::http::HeaderMap;
use chrono::{NaiveDate, Utc};

/// The header the trusted proxy writes the client address into, if any.
///
/// `None` when `MICAFORGE_TRUSTED_PROXY_HEADER` is unset, empty or `none`, which
/// means only the socket address is used. Lowercased at read time because
/// `HeaderMap` lookups are case-insensitive but the comparison inside
/// [`header`] is not.
pub fn proxy_header() -> Option<&'static str> {
    static HEADER: OnceLock<Option<String>> = OnceLock::new();
    HEADER
        .get_or_init(|| parse_header_setting(std::env::var("MICAFORGE_TRUSTED_PROXY_HEADER").ok()))
        .as_deref()
}

/// Read the setting. Split out so the rule can be tested without touching the
/// process environment, which every other test in the crate shares.
fn parse_header_setting(raw: Option<String>) -> Option<String> {
    raw.map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v != "none")
}

/// The client address, by the rightmost-untrusted rule described above.
///
/// `peer` is the socket address, when one is known. It is `None` on a request
/// that reached the router without `ConnectInfo`: a test, or a transport that
/// has no peer. In that case a request with no usable header resolves to
/// `0.0.0.0`, which hashes to a stable "unknown" visitor rather than failing.
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>) -> IpAddr {
    client_ip_from(headers, peer, proxy_header())
}

/// [`client_ip`] with the trusted header passed in rather than read from the
/// environment.
fn client_ip_from(headers: &HeaderMap, peer: Option<IpAddr>, trusted: Option<&str>) -> IpAddr {
    let fallback = peer.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    let Some(raw) = trusted.and_then(|name| header(headers, name)) else {
        return fallback;
    };

    let mut rightmost_private = None;
    for candidate in raw.rsplit(',') {
        let Some(addr) = parse_ip(candidate) else {
            continue;
        };
        if is_client_address(addr) {
            return addr;
        }
        if rightmost_private.is_none() {
            rightmost_private = Some(addr);
        }
    }
    rightmost_private.unwrap_or(fallback)
}

/// Could this address belong to a client rather than to a proxy?
///
/// Every address a proxy writes about *itself* comes from one of the ranges
/// below: a container network, a VPC, a load balancer's own interface. Skipping
/// them is what makes the walk in [`client_ip`] land on the reader rather than
/// on the last hop that handled their request.
///
/// Deliberately narrower than "globally routable". The reserved documentation
/// and benchmarking ranges are *not* excluded, because no proxy inserts one and
/// treating them as infrastructure would only ever mean discarding a genuine
/// entry, including in this project's own tests, which use `203.0.113.0/24`.
pub fn is_client_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                // Carrier-grade NAT, 100.64.0.0/10. A real subscriber behind
                // CGNAT reaches us as their carrier's public address; a 100.64
                // entry in a forwarded chain is a load balancer, not a person.
                || (a == 100 && (64..=127).contains(&b)))
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                // Unique local, fc00::/7, and link-local, fe80::/10.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// Parse one entry of a forwarded chain.
///
/// Handles the three spellings that arrive in the wild: a bare address, an
/// address in square brackets (the IPv6 form some proxies write), and an
/// address with a port appended. A bracketed or ported entry that still does not
/// parse is skipped rather than guessed at.
pub fn parse_ip(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim().trim_matches('"');
    if raw.is_empty() {
        return None;
    }
    if let Ok(addr) = raw.parse::<IpAddr>() {
        return Some(addr);
    }
    // `[2001:db8::1]:443` and `[2001:db8::1]`.
    if let Some(rest) = raw.strip_prefix('[') {
        if let Some((inside, _)) = rest.split_once(']') {
            return inside.parse().ok();
        }
    }
    // `203.0.113.7:9000`. Only ever split on the last colon, and only when there
    // is exactly one, so an unbracketed IPv6 address is not mangled.
    if raw.matches(':').count() == 1 {
        if let Some((host, _)) = raw.rsplit_once(':') {
            return host.parse().ok();
        }
    }
    None
}

/// A short keyed digest of an address, for a log line or a limiter key.
///
/// Sixteen hex characters of `blake3` keyed with a 32-byte key that is drawn at
/// random, held only in this process's memory and replaced at every UTC
/// midnight. Two requests from one address on one day carry the same tag, which
/// is all a log reader or a rate limiter needs. Nothing can turn the tag back
/// into the address: the key never reaches a disk, a log or Valkey, so a log
/// file or a Valkey snapshot holds no address, and the key that made yesterday's
/// tags no longer exists anywhere. An IPv4 address space is small enough to
/// brute-force through any digest whose key can be recovered, which is why this
/// one is not derived from `MICAFORGE_SECRET_KEY_BASE`.
///
/// The cost is that two replicas tag one address differently, so a per-address
/// limit counts per replica. The limits that use this are circuit breakers
/// rather than quotas, and loosening one by the replica count is harmless.
pub fn address_tag(ip: IpAddr) -> String {
    tag_with(&tag_key(Utc::now().date_naive()), ip)
}

/// Today's tag key for this process.
fn tag_key(today: NaiveDate) -> [u8; 32] {
    static KEY: OnceLock<Mutex<(NaiveDate, [u8; 32])>> = OnceLock::new();
    rotate(KEY.get_or_init(|| Mutex::new((today, fresh_key()))), today)
}

/// The key in `slot`, replaced first when it belongs to another day.
fn rotate(slot: &Mutex<(NaiveDate, [u8; 32])>, today: NaiveDate) -> [u8; 32] {
    // A poisoned lock still holds a valid key; a panic elsewhere must not stop
    // the access log or the limiter.
    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.0 != today {
        *slot = (today, fresh_key());
    }
    slot.1
}

/// Thirty-two bytes from the OS generator, by way of the token helper.
fn fresh_key() -> [u8; 32] {
    *blake3::hash(micaforge_core::crypto::random_token(32).as_bytes()).as_bytes()
}

/// The digest itself, with the key passed in so it can be tested.
fn tag_with(key: &[u8; 32], ip: IpAddr) -> String {
    let digest = blake3::keyed_hash(key, ip.to_string().as_bytes());
    hex::encode(&digest.as_bytes()[..8])
}

/// A borrowed header value, or `None` when it is absent or not UTF-8.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    const XFF: Option<&str> = Some("x-forwarded-for");

    #[test]
    fn the_chain_is_read_from_the_right() {
        let h = headers(&[(
            "x-forwarded-for",
            "9.9.9.9, 203.0.113.7, 10.0.0.3, 10.0.0.4",
        )]);
        assert_eq!(
            client_ip_from(&h, None, XFF).to_string(),
            "203.0.113.7",
            "the private hops the proxies added must be skipped"
        );
    }

    #[test]
    fn a_forged_leftmost_entry_cannot_win() {
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 198.51.100.9")]);
        assert_eq!(client_ip_from(&h, None, XFF).to_string(), "198.51.100.9");
    }

    #[test]
    fn a_private_only_chain_falls_back_to_the_rightmost_hop() {
        let h = headers(&[("x-forwarded-for", "10.1.1.1, 192.168.0.9")]);
        assert_eq!(client_ip_from(&h, None, XFF).to_string(), "192.168.0.9");
    }

    #[test]
    fn no_header_means_the_socket_address() {
        let peer: IpAddr = "203.0.113.200".parse().unwrap();
        assert_eq!(client_ip_from(&HeaderMap::new(), Some(peer), XFF), peer);
    }

    #[test]
    fn an_unset_setting_trusts_no_header_at_all() {
        for raw in [None, Some(""), Some("  "), Some("none"), Some("NONE")] {
            assert_eq!(
                parse_header_setting(raw.map(str::to_string)),
                None,
                "{raw:?}"
            );
        }
        assert_eq!(
            parse_header_setting(Some(" X-Real-IP ".into())).as_deref(),
            Some("x-real-ip")
        );

        // With nothing trusted, a forged header is ignored and the socket wins.
        let peer: IpAddr = "198.51.100.20".parse().unwrap();
        let h = headers(&[
            ("x-forwarded-for", "203.0.113.7"),
            ("x-real-ip", "203.0.113.8"),
        ]);
        assert_eq!(client_ip_from(&h, Some(peer), None), peer);
    }

    #[test]
    fn a_single_value_header_from_the_proxy_is_used_as_is() {
        let peer: IpAddr = "172.18.0.5".parse().unwrap();
        let h = headers(&[
            ("x-real-ip", "203.0.113.7"),
            // Another header the client wrote must not matter.
            ("x-forwarded-for", "9.9.9.9"),
        ]);
        assert_eq!(
            client_ip_from(&h, Some(peer), Some("x-real-ip")).to_string(),
            "203.0.113.7"
        );
        let h = headers(&[("cf-connecting-ip", "2001:db8::7")]);
        assert_eq!(
            client_ip_from(&h, Some(peer), Some("cf-connecting-ip")).to_string(),
            "2001:db8::7"
        );
        // A LAN visitor behind the proxy is still themselves.
        let h = headers(&[("x-real-ip", "192.168.1.40")]);
        assert_eq!(
            client_ip_from(&h, Some(peer), Some("x-real-ip")).to_string(),
            "192.168.1.40"
        );
        // A header that is named but missing falls back to the socket.
        assert_eq!(
            client_ip_from(&HeaderMap::new(), Some(peer), Some("x-real-ip")),
            peer
        );
    }

    #[test]
    fn an_address_tag_is_stable_short_keyed_and_never_the_address() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let key = [7u8; 32];
        let tag = tag_with(&key, ip);
        assert_eq!(tag.len(), 16);
        assert_eq!(tag, tag_with(&key, ip));
        assert!(!tag.contains("203"));
        assert_ne!(tag, tag_with(&[8u8; 32], ip), "another key, another tag");
        assert_ne!(tag, tag_with(&key, "203.0.113.8".parse().unwrap()));

        // One key per day, replaced on the first call of the next.
        let day = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let slot = Mutex::new((day, fresh_key()));
        let first = rotate(&slot, day);
        assert_eq!(first, rotate(&slot, day));
        assert_ne!(first, rotate(&slot, day.succ_opt().unwrap()));
    }

    #[test]
    fn infrastructure_ranges_are_never_mistaken_for_a_client() {
        for infra in [
            "10.0.0.1",
            "172.16.4.4",
            "192.168.1.1",
            "127.0.0.1",
            "100.100.1.1",
        ] {
            assert!(!is_client_address(infra.parse().unwrap()), "{infra}");
        }
        for client in ["203.0.113.7", "8.8.8.8", "2001:db8::1"] {
            assert!(is_client_address(client.parse().unwrap()), "{client}");
        }
        assert!(
            !is_client_address("fd00::1".parse().unwrap()),
            "unique-local is a fabric"
        );
        assert!(
            !is_client_address("fe80::1".parse().unwrap()),
            "link-local is a fabric"
        );
    }

    #[test]
    fn ported_and_bracketed_entries_still_parse() {
        assert_eq!(
            parse_ip("203.0.113.7:9000").unwrap().to_string(),
            "203.0.113.7"
        );
        assert_eq!(
            parse_ip("[2001:db8::1]:443").unwrap().to_string(),
            "2001:db8::1"
        );
        assert_eq!(parse_ip("2001:db8::1").unwrap().to_string(), "2001:db8::1");
        assert!(parse_ip("   ").is_none());
        assert!(parse_ip("not-an-address").is_none());
    }
}
