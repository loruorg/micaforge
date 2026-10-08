//! Hashing, sealing and token generation.
//!
//! Five separate jobs live here, and they are kept in one module so that no
//! other crate is ever tempted to grow a sixth implementation:
//!
//! 1. **Share-link passwords**: Argon2id, the only thing in the product that is
//!    meant to be slow. Accounts have no password at all; a person signs in
//!    with a code sent to their address. The one password left in the product
//!    is the optional one an operator puts on a shared dashboard link.
//! 2. **Secrets at rest**: `totp_secret_sealed`, integration credentials and
//!    the site ingest key are sealed with XChaCha20-Poly1305 before they reach
//!    Postgres, keyed off `MICAFORGE_SECRET_KEY_BASE`.
//! 3. **Tokens**: API keys, session tokens and share-link slugs, plus the
//!    digest that is stored in their place.
//! 4. **One-time codes**: the six digits a sign-in email carries, drawn without
//!    bias, and the keyed digest that is stored in their place.
//! 5. **The visitor hash**: the cookieless identity from `PRODUCT.md`, and the
//!    reason no raw IP is ever written to an event row.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{Error, Result};

/// XChaCha20-Poly1305 nonce width. The extended nonce is the whole reason this
/// cipher is used here rather than the IETF variant: 192 random bits can be
/// drawn per message with no counter and no birthday-bound bookkeeping.
const NONCE_LEN: usize = 24;

/// Poly1305 authentication tag width, used only to sanity-check ciphertext length.
const TAG_LEN: usize = 16;

/// Domain separator for [`derive_key`]. Changing this string rotates every
/// derived key in the install, which would make every sealed secret unreadable.
const HKDF_SALT: &[u8] = b"micaforge/hkdf/v1";

/// Field separator for [`visitor_hash`] and [`keyed_digest`]. Without it,
/// `("1.2.3", "4")` and `("1.2", "3.4")` would hash to the same value.
const FIELD_SEP: &[u8] = &[0x1f];

/// The most digits [`numeric_code`] draws. `10^9` is the largest power of ten
/// below `2^32`, which is the width of one draw.
const MAX_CODE_DIGITS: u32 = 9;

/// Seal `plaintext` with XChaCha20-Poly1305, binding it to `aad`.
///
/// The 24-byte random nonce is prefixed to the returned ciphertext, so the
/// output is a single opaque blob a `BYTEA` column can hold with no companion
/// nonce column.
///
/// `aad` is authenticated but not encrypted, and it is what stops a sealed value
/// being moved between rows: seal a user's TOTP secret with that user's id as
/// `aad` and the blob is worthless in anybody else's row.
pub fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));

    let mut nonce = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce);

    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        // The AEAD error is deliberately opaque; it carries nothing worth logging.
        .map_err(|_| Error::internal("seal failed"))?;

    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Open a blob produced by [`seal`].
///
/// Fails if the key is wrong, the `aad` does not match the one used to seal, or
/// a single bit of the blob changed. All three are the same failure to the
/// caller on purpose: distinguishing them tells an attacker which half to work on.
pub fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(Error::internal("sealed value is truncated"));
    }
    let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);

    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| Error::internal("sealed value failed to open"))
}

/// Hash a share-link password with Argon2id and return the PHC string.
///
/// The returned string carries the algorithm, version, parameters and salt, so a
/// later parameter change does not invalidate existing hashes: old hashes still
/// verify against their own recorded cost, and can be re-hashed on next login.
pub fn hash_password(pw: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| Error::internal(format!("password hash failed: {e}")))
}

/// Verify a password against a PHC string from [`hash_password`].
///
/// Returns `false` for a wrong password *and* for a malformed stored hash. There
/// is no third answer to give a login form, and the comparison inside Argon2 is
/// constant time, so a caller cannot learn anything from how long this took.
pub fn verify_password(pw: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(pw.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A decimal code of `digits` digits, drawn uniformly from the CSPRNG.
///
/// Leading zeros are kept, so a six-digit code is one of exactly a million
/// values, `000000` to `999999`, each as likely as any other. `digits` is
/// clamped to `1..=9`.
///
/// Uniform is the property that matters, and the obvious `draw % 1_000_000`
/// does not have it: `2^32` is not a multiple of a million, so the lowest
/// 967,296 values would each come up once more often than the rest. A guesser
/// who knew that would start with them. [`uniform_below`] rejects the draws past
/// the last whole multiple instead, which costs a second draw about once in
/// four thousand codes.
pub fn numeric_code(digits: u32) -> String {
    let digits = digits.clamp(1, MAX_CODE_DIGITS);
    let space = 10u32.pow(digits);
    let mut rng = rand::rng();
    loop {
        if let Some(n) = uniform_below(rng.next_u32(), space) {
            return format!("{n:0width$}", width = digits as usize);
        }
    }
}

/// Map one 32-bit draw onto `0..space` without bias, or reject it.
///
/// Accepts only draws below the largest multiple of `space` that fits in the
/// `2^32` possible draws. Every value in `0..space` then has exactly the same
/// number of draws mapping onto it, which is what makes the result uniform.
fn uniform_below(draw: u32, space: u32) -> Option<u32> {
    let space = space.max(1);
    let draws = 1u64 << 32;
    let accepted = draws - draws % u64::from(space);
    (u64::from(draw) < accepted).then(|| draw % space)
}

/// HMAC-SHA256 over `parts`, lower-case hex.
///
/// For a value that must be stored and compared later but must not be
/// recoverable from a dump, when the input is too small for a plain digest to
/// protect: a six-digit code has a million values, and `sha256(code)` is
/// reversed by trying all of them. Keyed with a [`derive_key`] output, the
/// digest is worthless without `MICAFORGE_SECRET_KEY_BASE`.
///
/// The parts are separated, so `("ab", "c")` and `("a", "bc")` never collide.
pub fn keyed_digest(key: &[u8; 32], parts: &[&str]) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(key).expect("hmac-sha256 accepts a key of any length");
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            mac.update(FIELD_SEP);
        }
        mac.update(part.as_bytes());
    }
    hex::encode(mac.finalize().into_bytes())
}

/// Compare two digests in constant time.
///
/// A `==` on strings returns at the first differing byte, so how long it took
/// says how many leading characters matched, and for a stored digest that is a
/// way to learn it one character at a time. The length is not secret (every
/// digest here is 64 hex characters), so only the contents are compared in
/// constant time.
pub fn digests_match(stored: &str, supplied: &str) -> bool {
    stored.as_bytes().ct_eq(supplied.as_bytes()).into()
}

/// A url-safe, unpadded base64 token drawn from the OS CSPRNG.
///
/// `bytes` is the entropy, not the string length: the returned string is about
/// `bytes * 4 / 3` characters. Use 32 for anything that authenticates (API keys,
/// session tokens) and 16 for a public identifier such as a share slug.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// SHA-256 of a token, lower-case hex.
///
/// Tokens are stored as this digest and never in the clear, so a database dump
/// does not hand over live credentials. SHA-256 rather than Argon2 is correct
/// here: the input already has full random entropy, so there is nothing for a
/// brute-force to shortcut, and the lookup happens on every authenticated
/// request.
pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
}

/// The cookieless visitor identity: `blake3(salt || site_id || ip || ua)`,
/// truncated to 16 hex characters.
///
/// This is the mechanism behind non-negotiable 1 and 2 in `docs/CONTRACT.md` §7.
/// The raw IP is an input and never a stored value; the salt rotates at
/// 00:00 site-local and is discarded within 48 hours, so yesterday's hashes
/// cannot be recomputed even with the same IP and user agent in hand. That is
/// what makes the identifier a measurement and not a person.
///
/// 64 bits is enough: a site would need on the order of a billion distinct
/// visitors *in one day* before a collision became likely, and the salt makes
/// the space unguessable rather than merely large. The short form keeps the
/// ClickHouse `visitor_id` column small, which matters at the row counts this
/// table reaches.
///
/// Fields are separated by a unit-separator byte so that two different splits of
/// the same characters cannot produce the same visitor.
pub fn visitor_hash(salt: &[u8], site_id: u16, ip: &str, ua: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(salt);
    hasher.update(FIELD_SEP);
    hasher.update(&site_id.to_be_bytes());
    hasher.update(FIELD_SEP);
    hasher.update(ip.as_bytes());
    hasher.update(FIELD_SEP);
    hasher.update(ua.as_bytes());

    let digest = hasher.finalize();
    hex::encode(&digest.as_bytes()[..8])
}

/// Derive a 32-byte key for one purpose from `MICAFORGE_SECRET_KEY_BASE`.
///
/// A single HKDF-SHA256 extract-and-expand round. Every subsystem that needs a
/// key asks for its own purpose string (`"cookie"`, `"totp"`, `"ingest"`,
/// `"share"`), so the keys are independent: recovering one does not expose the
/// others, and an operator only ever has to hold one secret.
///
/// One block of output is all that is needed, so the expand step is a single
/// HMAC with the counter byte `0x01`.
pub fn derive_key(secret_key_base: &str, purpose: &str) -> [u8; 32] {
    type HmacSha256 = Hmac<Sha256>;

    // Extract: compress whatever entropy the operator's secret has into a PRK.
    let mut extract = <HmacSha256 as Mac>::new_from_slice(HKDF_SALT)
        .expect("hmac-sha256 accepts a key of any length");
    extract.update(secret_key_base.as_bytes());
    let prk = extract.finalize().into_bytes();

    // Expand: bind the PRK to this one purpose.
    let mut expand =
        <HmacSha256 as Mac>::new_from_slice(&prk).expect("hmac-sha256 accepts a key of any length");
    expand.update(purpose.as_bytes());
    expand.update(&[0x01]);
    let okm = expand.finalize().into_bytes();

    let mut key = [0u8; 32];
    key.copy_from_slice(&okm);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_round_trips_and_binds_its_aad() {
        let key = derive_key("a-secret-key-base-of-adequate-length", "totp");
        let sealed = seal(&key, b"user-1", b"JBSWY3DPEHPK3PXP").unwrap();

        assert_eq!(open(&key, b"user-1", &sealed).unwrap(), b"JBSWY3DPEHPK3PXP");
        // Same blob, another user's row: must not open.
        assert!(open(&key, b"user-2", &sealed).is_err());
        // Another key: must not open.
        let other = derive_key("a-secret-key-base-of-adequate-length", "cookie");
        assert!(open(&other, b"user-1", &sealed).is_err());
    }

    #[test]
    fn seal_uses_a_fresh_nonce_every_time() {
        let key = derive_key("a-secret-key-base-of-adequate-length", "ingest");
        let a = seal(&key, b"", b"same").unwrap();
        let b = seal(&key, b"", b"same").unwrap();
        assert_ne!(
            a, b,
            "identical plaintexts must not produce identical blobs"
        );
    }

    #[test]
    fn truncated_and_tampered_blobs_are_rejected() {
        let key = derive_key("a-secret-key-base-of-adequate-length", "totp");
        let mut sealed = seal(&key, b"aad", b"secret").unwrap();
        assert!(open(&key, b"aad", &sealed[..8]).is_err());

        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(open(&key, b"aad", &sealed).is_err());
    }

    #[test]
    fn codes_have_the_requested_width_and_keep_leading_zeros() {
        for _ in 0..200 {
            let code = numeric_code(6);
            assert_eq!(code.len(), 6);
            assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code}");
        }
        assert_eq!(numeric_code(0).len(), 1, "clamped up to one digit");
        assert_eq!(numeric_code(40).len(), 9, "clamped down to nine digits");
        // A leading zero is one code in ten. Two thousand codes without one
        // would mean the padding, or the draw, was wrong.
        assert!((0..2_000).any(|_| numeric_code(6).starts_with('0')));
    }

    #[test]
    fn a_draw_is_rejected_exactly_past_the_last_whole_multiple() {
        let space = 1_000_000;
        // 2^32 is 4,294,967,296, so the last accepted draw is 4,293,999,999.
        let accepted = 4_294_000_000u64;
        assert_eq!(accepted % u64::from(space), 0);
        assert_eq!(uniform_below(0, space), Some(0));
        assert_eq!(uniform_below((accepted - 1) as u32, space), Some(999_999));
        assert_eq!(uniform_below(accepted as u32, space), None);
        assert_eq!(uniform_below(u32::MAX, space), None);

        // The same rule for a space with a small remainder: 2^32 is 4 more
        // than a multiple of 7, so exactly the top 4 draws are rejected.
        assert_eq!(uniform_below(u32::MAX - 4, 7), Some((u32::MAX - 4) % 7));
        assert_eq!(uniform_below(u32::MAX - 3, 7), None);
    }

    #[test]
    fn every_digit_comes_up_about_as_often_as_every_other() {
        // A chi-squared test on the leading digit of 60,000 codes. With nine
        // degrees of freedom a fair draw stays under 40 except about once in a
        // hundred thousand runs; a biased or broken draw does not.
        let draws = 60_000;
        let mut counts = [0u32; 10];
        for _ in 0..draws {
            let lead = numeric_code(6).as_bytes()[0] - b'0';
            counts[usize::from(lead)] += 1;
        }
        let expected = f64::from(draws) / 10.0;
        let chi2: f64 = counts
            .iter()
            .map(|&n| (f64::from(n) - expected).powi(2) / expected)
            .sum();
        assert!(chi2 < 40.0, "chi-squared {chi2} for {counts:?}");
    }

    #[test]
    fn keyed_digests_need_the_key_and_keep_their_parts_apart() {
        let key = derive_key("a-secret-key-base-of-adequate-length", "sign-in-code");
        let other = derive_key("a-secret-key-base-of-adequate-length", "sign-in-address");
        let a = keyed_digest(&key, &["ada@example.com", "123456"]);
        assert_eq!(a.len(), 64);
        assert_eq!(a, keyed_digest(&key, &["ada@example.com", "123456"]));
        assert_ne!(a, keyed_digest(&other, &["ada@example.com", "123456"]));
        assert_ne!(a, keyed_digest(&key, &["ada@example.com", "123457"]));
        assert_ne!(
            keyed_digest(&key, &["ab", "c"]),
            keyed_digest(&key, &["a", "bc"])
        );
        // Not the unkeyed digest, which a million guesses would reverse.
        assert_ne!(a, token_hash("123456"));
    }

    #[test]
    fn digests_match_only_when_every_byte_does() {
        let key = derive_key("a-secret-key-base-of-adequate-length", "sign-in-code");
        let stored = keyed_digest(&key, &["ada@example.com", "123456"]);
        assert!(digests_match(&stored, &stored.clone()));
        assert!(!digests_match(
            &stored,
            &keyed_digest(&key, &["ada@example.com", "654321"])
        ));
        assert!(!digests_match(&stored, &stored[..63]));
        assert!(!digests_match(&stored, ""));
    }

    #[test]
    fn passwords_verify_only_against_themselves() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("correct horse battery stapl", &hash));
        assert!(!verify_password("anything", "not-a-phc-string"));
    }

    #[test]
    fn visitor_hash_is_stable_salted_and_separated() {
        let a = visitor_hash(b"day-salt", 1, "203.0.113.7", "Mozilla/5.0");
        assert_eq!(a.len(), 16);
        assert_eq!(
            a,
            visitor_hash(b"day-salt", 1, "203.0.113.7", "Mozilla/5.0")
        );

        // Tomorrow's salt gives a different visitor for the same person.
        assert_ne!(
            a,
            visitor_hash(b"next-salt", 1, "203.0.113.7", "Mozilla/5.0")
        );
        // Another site never shares a visitor with this one.
        assert_ne!(
            a,
            visitor_hash(b"day-salt", 2, "203.0.113.7", "Mozilla/5.0")
        );
        // The separator stops the fields running together.
        assert_ne!(
            visitor_hash(b"s", 1, "1.2.3", "4"),
            visitor_hash(b"s", 1, "1.2", "3.4")
        );
    }

    #[test]
    fn tokens_are_url_safe_and_unique() {
        let a = random_token(32);
        let b = random_token(32);
        assert_ne!(a, b);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(token_hash("mfg_example").len(), 64);
        assert_eq!(token_hash("mfg_example"), token_hash("mfg_example"));
        assert_ne!(token_hash("mfg_example"), token_hash("mfg_examplf"));
    }

    #[test]
    fn derived_keys_are_purpose_separated_and_deterministic() {
        let base = "a-secret-key-base-of-adequate-length";
        assert_eq!(derive_key(base, "cookie"), derive_key(base, "cookie"));
        assert_ne!(derive_key(base, "cookie"), derive_key(base, "ingest"));
        assert_ne!(
            derive_key(base, "cookie"),
            derive_key("other-base", "cookie")
        );
    }
}
