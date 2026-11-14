//! Small pieces of cryptography for linked accounts, with no I/O: checking
//! Twitch's signed live/offline messages, and the PKCE pair for services
//! that support it (GitHub).

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// Twitch EventSub: `sha256=` and the hex HMAC-SHA256 of message id,
/// timestamp and raw body, with the secret we gave Twitch
pub fn twitch_signature(secret: &str, message_id: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(message_id.as_bytes());
    mac.update(timestamp.as_bytes());
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Whether Twitch's signature header matches, compared in constant time
pub fn twitch_signature_valid(
    secret: &str,
    message_id: &str,
    timestamp: &str,
    body: &[u8],
    header: &str,
) -> bool {
    let expected = twitch_signature(secret, message_id, timestamp, body);
    let (a, b) = (expected.as_bytes(), header.trim().as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// PKCE S256: the challenge for this verifier (base64url of its SHA-256,
/// no padding)
pub fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_example() {
        // RFC 7636, appendix B
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn twitch_signature_checks_every_part() {
        let good = twitch_signature("secret-0123456789", "id-1", "2026-11-14T10:00:00Z", b"{}");
        assert!(good.starts_with("sha256=") && good.len() == 7 + 64);
        assert!(twitch_signature_valid("secret-0123456789", "id-1", "2026-11-14T10:00:00Z", b"{}", &good));
        assert!(!twitch_signature_valid("secret-0123456789", "id-2", "2026-11-14T10:00:00Z", b"{}", &good));
        assert!(!twitch_signature_valid("secret-0123456789", "id-1", "2026-11-14T10:00:01Z", b"{}", &good));
        assert!(!twitch_signature_valid("secret-0123456789", "id-1", "2026-11-14T10:00:00Z", b"{ }", &good));
        assert!(!twitch_signature_valid("other-secret-000", "id-1", "2026-11-14T10:00:00Z", b"{}", &good));
        assert!(!twitch_signature_valid("secret-0123456789", "id-1", "2026-11-14T10:00:00Z", b"{}", "sha256="));
    }
}
