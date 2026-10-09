//! Proving that a chat request came from the platform it claims.
//!
//! The three webhook endpoints are public URLs on purpose: Slack, Discord and
//! Telegram must reach them from the internet. What keeps a stranger from
//! posting "link ABCD2345" or a question that spends an organisation's AI
//! allowance is this module, and each platform's scheme is followed to the
//! letter:
//!
//! - **Slack**: `X-Slack-Signature: v0=<hex>` is HMAC-SHA256, keyed with the
//!   app's signing secret, over `v0:<X-Slack-Request-Timestamp>:<raw body>`.
//!   A timestamp more than five minutes from the server's clock is refused, so
//!   a captured request cannot be replayed later.
//! - **Discord**: `X-Signature-Ed25519` is an Ed25519 signature, by the
//!   application's key, over `<X-Signature-Timestamp>` followed by the raw
//!   body. Discord tests the endpoint with deliberately bad signatures and
//!   expects a `401` for them.
//! - **Telegram**: the `X-Telegram-Bot-Api-Secret-Token` header must equal the
//!   secret the server gave `setWebhook`.
//!
//! Every comparison is constant time, and every check reads the body exactly
//! as received: re-serialising JSON before verifying it is the classic way to
//! make a valid signature fail, or an invalid one pass.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// How far a Slack timestamp may be from the server's clock, in seconds.
pub const SLACK_TOLERANCE_SECS: i64 = 5 * 60;

/// Verify a Slack request.
///
/// `now` is Unix seconds, passed in so the window can be tested.
pub fn slack(
    signing_secret: &str,
    timestamp: &str,
    signature: &str,
    body: &[u8],
    now: i64,
) -> bool {
    let Ok(sent_at) = timestamp.trim().parse::<i64>() else {
        return false;
    };
    if (now - sent_at).abs() > SLACK_TOLERANCE_SECS {
        return false;
    }
    let Some(hex_sig) = signature.trim().strip_prefix("v0=") else {
        return false;
    };
    let Ok(presented) = hex::decode(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(signing_secret.as_bytes()) else {
        return false;
    };
    mac.update(b"v0:");
    mac.update(timestamp.trim().as_bytes());
    mac.update(b":");
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    expected.as_slice().ct_eq(&presented).into()
}

/// Verify a Discord interaction.
///
/// `public_key_hex` is the application's public key as the developer portal
/// shows it; the configuration has already checked it is 64 hex characters.
pub fn discord(public_key_hex: &str, signature_hex: &str, timestamp: &str, body: &[u8]) -> bool {
    let Ok(key_bytes) = hex::decode(public_key_hex.trim()) else {
        return false;
    };
    let Ok(key_array) = <[u8; 32]>::try_from(key_bytes.as_slice()) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&key_array) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(signature_hex.trim()) else {
        return false;
    };
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_array);

    let mut message = Vec::with_capacity(timestamp.len() + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.extend_from_slice(body);
    key.verify(&message, &signature).is_ok()
}

/// Verify a Telegram update by its secret token header.
pub fn telegram(expected_secret: &str, presented: Option<&str>) -> bool {
    match presented {
        Some(value) => {
            value.len() == expected_secret.len()
                && bool::from(value.as_bytes().ct_eq(expected_secret.as_bytes()))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Slack's own worked example, from "Verifying requests from Slack": the
    /// signing secret, the timestamp, the body and the signature they publish.
    #[test]
    fn slacks_published_example_verifies() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let timestamp = "1531420618";
        let body = b"token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&team_domain=testteamnow&channel_id=G8PSS9T3V&channel_name=foobar&user_id=U2CERLKJA&user_name=roadrunner&command=%2Fwebhook-collect&text=&response_url=https%3A%2F%2Fhooks.slack.com%2Fcommands%2FT1DC2JH3J%2F397700885554%2F96rGlfmibIGlgcZRskXaIFfN&trigger_id=398738663015.47445629121.803a0bc887a14d10d2c447fce8b6703c";
        let signature = "v0=a2114d57b48eac39b9ad189dd8316235a7b4a8d21a10bd27519666489c69b503";
        let now = 1_531_420_618;
        assert!(slack(secret, timestamp, signature, body, now));
    }

    /// The same check against a signature this test computes, so the rules
    /// around it can be exercised one at a time.
    #[test]
    fn a_slack_signature_must_be_fresh_exact_and_ours() {
        let secret = "signing-secret";
        let body = b"command=%2Fmicaforge&text=stats";
        let ts = "1700000000";
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("v0:{ts}:").as_bytes());
        mac.update(body);
        let good = format!("v0={}", hex::encode(mac.finalize().into_bytes()));
        let now = 1_700_000_000;

        assert!(slack(secret, ts, &good, body, now));
        assert!(slack(secret, ts, &good, body, now + SLACK_TOLERANCE_SECS));
        // Replayed after the window.
        assert!(!slack(
            secret,
            ts,
            &good,
            body,
            now + SLACK_TOLERANCE_SECS + 1
        ));
        // A body changed by one byte.
        assert!(!slack(
            secret,
            ts,
            &good,
            b"command=%2Fmicaforge&text=statz",
            now
        ));
        // Another app's secret.
        assert!(!slack("other-secret", ts, &good, body, now));
        // A signature without its version, or not hex at all.
        assert!(!slack(
            secret,
            ts,
            good.trim_start_matches("v0="),
            body,
            now
        ));
        assert!(!slack(secret, ts, "v0=zz", body, now));
        assert!(!slack(secret, "yesterday", &good, body, now));
    }

    /// A key pair generated here, signed the way Discord signs.
    #[test]
    fn a_discord_interaction_verifies_with_the_application_key_only() {
        let mut seed = [0u8; 32];
        seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let signing = SigningKey::from_bytes(&seed);
        let public_hex = hex::encode(signing.verifying_key().to_bytes());

        let timestamp = "1700000000";
        let body = br#"{"type":1}"#;
        let mut message = timestamp.as_bytes().to_vec();
        message.extend_from_slice(body);
        let signature = hex::encode(signing.sign(&message).to_bytes());

        assert!(discord(&public_hex, &signature, timestamp, body));
        // Another timestamp, another body, another key: all refused.
        assert!(!discord(&public_hex, &signature, "1700000001", body));
        assert!(!discord(
            &public_hex,
            &signature,
            timestamp,
            br#"{"type":2}"#
        ));
        let other = SigningKey::from_bytes(&[7u8; 32]);
        assert!(!discord(
            &hex::encode(other.verifying_key().to_bytes()),
            &signature,
            timestamp,
            body
        ));
        // Malformed input is a refusal, never a panic.
        assert!(!discord(&public_hex, "abcd", timestamp, body));
        assert!(!discord("not-hex", &signature, timestamp, body));
    }

    #[test]
    fn a_telegram_secret_must_match_exactly() {
        assert!(telegram("s3cret_value-1", Some("s3cret_value-1")));
        assert!(!telegram("s3cret_value-1", Some("s3cret_value-2")));
        assert!(!telegram("s3cret_value-1", Some("s3cret_value-1 ")));
        assert!(!telegram("s3cret_value-1", None));
    }
}
