//! Hashing, sealing and token generation.
//!
//! Four separate jobs live here, and they are kept in one module so that no
//! other crate is ever tempted to grow a fifth implementation:
//!
//! 1. **Passwords**: Argon2id, the only thing in the product that is meant to
//!    be slow.
//! 2. **Secrets at rest**: `totp_secret_sealed`, integration credentials and
//!    the site ingest key are sealed with XChaCha20-Poly1305 before they reach
//!    Postgres, keyed off `MICAFORGE_SECRET_KEY_BASE`.
//! 3. **Tokens**: API keys, session tokens and share-link slugs, plus the
//!    digest that is stored in their place.
//! 4. **The visitor hash**: the cookieless identity from `PRODUCT.md`, and the
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

/// Field separator for [`visitor_hash`]. Without it, `("1.2.3", "4")` and
/// `("1.2", "3.4")` would hash to the same visitor.
const FIELD_SEP: &[u8] = &[0x1f];

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

/// Hash a password with Argon2id and return the PHC string.
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
