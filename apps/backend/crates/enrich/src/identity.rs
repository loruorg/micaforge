//! Who this is, for exactly as long as the product is allowed to know.
//!
//! Micaforge has no cookie and no persistent identifier. A visitor is a
//! `blake3` digest of a per-site daily salt, the site id, the IP and the user
//! agent (`docs/CONTRACT.md` §7.2). The salt rotates at midnight in the site's
//! own timezone and is never kept beyond 48 hours, which is what makes the
//! digest un-reversible in practice: without the salt there is nothing to
//! attack, and the salt is gone.
//!
//! The consequence is deliberate and worth stating plainly, because it is the
//! difference between this and a cookie-based tool: **a returning visitor
//! tomorrow is a new visitor.** Daily uniques are exact, and any longer-window
//! "unique visitors" number is an estimate. Micaforge would rather report that
//! honestly than keep an identifier it promised not to keep.
//!
//! The hash itself lives in `micaforge_core::crypto`, not here. Ingest, the
//! importer, the log-shipper path and the tests must all produce byte-identical
//! digests for the same inputs, and the surest way to guarantee that is for
//! there to be exactly one implementation. This module re-exports it and adds
//! the session identity around it.

use uuid::Uuid;

/// Inactivity gap that ends a session, in seconds.
///
/// Thirty minutes, matching the definition in `docs/CONTRACT.md` §0 and the
/// convention every other analytics tool uses. It is a constant rather than a
/// setting on purpose: a site that quietly redefined a session would report
/// session counts that could not be compared with anybody else's, including
/// its own from last month.
pub const SESSION_WINDOW_SECS: u32 = 1800;

/// Crockford base32, lowercased.
///
/// Crockford rather than RFC 4648 because it drops `i`, `l`, `o` and `u`: the
/// session id shows up in support conversations and in URLs like
/// `/app/1/sessions/:id`, and those four characters are where a transcription
/// goes wrong. Lowercased because the surrounding URLs are.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The daily-rotating visitor digest.
///
/// `salt` is the site's salt for the current local day. Rotating it is the
/// caller's job (the `jobs` crate runs it); this function only hashes.
///
/// Delegates to `micaforge_core::crypto::visitor_hash` so that every producer
/// of a visitor id in the system shares one implementation.
pub fn visitor_id(salt: &[u8], site_id: u16, ip: &str, ua: &str) -> String {
    micaforge_core::crypto::visitor_hash(salt, site_id, ip, ua)
}

/// A fresh session id: 26 lowercase Crockford base32 characters.
///
/// Rendered from a UUIDv7, so the id sorts by creation time as a string. That
/// matters more than it sounds: the ids end up in a ClickHouse `ORDER BY` key
/// and in cursor pagination over a session list, and a sortable id turns both
/// of those into a range scan instead of a sort.
///
/// ```
/// # use micaforge_enrich::identity::new_session_id;
/// let a = new_session_id();
/// assert_eq!(a.len(), 26);
/// assert!(a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
/// ```
pub fn new_session_id() -> String {
    encode_base32(Uuid::now_v7().as_u128())
}

/// Render a 128-bit value as 26 base32 characters, most significant first.
///
/// 26 characters carry 130 bits, so the top two bits of the first character are
/// always zero. That is the same layout ULID uses, and it is what keeps the
/// lexical order of the string identical to the numeric order of the value.
fn encode_base32(mut value: u128) -> String {
    let mut out = [b'0'; 26];
    for slot in out.iter_mut().rev() {
        *slot = ALPHABET[(value & 0x1f) as usize];
        value >>= 5;
    }
    // Every byte came from `ALPHABET`, which is ASCII.
    String::from_utf8(out.to_vec()).expect("base32 alphabet is ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_twenty_six_lowercase_base32_characters() {
        let id = new_session_id();
        assert_eq!(id.len(), 26);
        assert!(id.bytes().all(|b| ALPHABET.contains(&b)));
        for banned in ['i', 'l', 'o', 'u'] {
            assert!(!id.contains(banned), "{id} contains {banned}");
        }
    }

    #[test]
    fn session_ids_sort_in_creation_order() {
        // UUIDv7 has millisecond resolution, so ids minted in the same
        // millisecond are ordered by their random tail rather than by time.
        // Encoding two known values proves the property the encoding is
        // responsible for: string order follows numeric order.
        assert!(encode_base32(1) < encode_base32(2));
        assert!(encode_base32(u128::from(u64::MAX)) < encode_base32(u128::from(u64::MAX) + 1));
        assert!(encode_base32(0) < encode_base32(u128::MAX));
    }

    #[test]
    fn session_ids_are_distinct() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
    }

    #[test]
    fn the_visitor_digest_is_stable_and_salt_dependent() {
        let a = visitor_id(b"monday", 1, "203.0.113.7", "Mozilla/5.0");
        let same = visitor_id(b"monday", 1, "203.0.113.7", "Mozilla/5.0");
        let tomorrow = visitor_id(b"tuesday", 1, "203.0.113.7", "Mozilla/5.0");
        let other_site = visitor_id(b"monday", 2, "203.0.113.7", "Mozilla/5.0");

        assert_eq!(a, same);
        assert_ne!(a, tomorrow, "the salt rotation must break the link");
        assert_ne!(a, other_site, "one person is two visitors across two sites");
        assert_eq!(a.len(), 16);
    }
}
