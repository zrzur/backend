//! Stripe webhook signatures: `Stripe-Signature: t=<unix>,v1=<hex>`, where
//! v1 is HMAC-SHA256 of `"{t}.{raw body}"` with the endpoint's signing
//! secret. Checked on the exact bytes received, in constant time, with a
//! time window against replays.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Oldest (or furthest in the future) signature accepted, in seconds
pub const SIGNATURE_TOLERANCE_SECONDS: i64 = 300;

/// Largest webhook body accepted
pub const MAX_WEBHOOK_BODY: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    /// Header missing or unreadable
    Malformed,
    /// Signed too long ago or in the future
    OutsideTolerance,
    /// No signature matches any secret
    Mismatch,
    /// No secret configured
    NoSecret,
}

/// Check a signature header against the raw body with any of the secrets
/// (current first, then the one being rotated out)
pub fn verify_stripe_signature(
    header: &str,
    body: &[u8],
    secrets: &[String],
    now: i64,
) -> Result<i64, SignatureError> {
    if secrets.is_empty() {
        return Err(SignatureError::NoSecret);
    }
    if header.len() > 2048 {
        return Err(SignatureError::Malformed);
    }

    let mut timestamp: Option<i64> = None;
    let mut signatures: Vec<Vec<u8>> = vec![];
    for part in header.split(',') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        match key {
            "t" => timestamp = value.parse().ok(),
            "v1" => {
                if let Ok(bytes) = hex::decode(value) {
                    signatures.push(bytes);
                }
            }
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(SignatureError::Malformed)?;
    if signatures.is_empty() || signatures.len() > 8 {
        return Err(SignatureError::Malformed);
    }
    if (now - timestamp).abs() > SIGNATURE_TOLERANCE_SECONDS {
        return Err(SignatureError::OutsideTolerance);
    }

    for secret in secrets {
        for signature in &signatures {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                .map_err(|_| SignatureError::NoSecret)?;
            mac.update(timestamp.to_string().as_bytes());
            mac.update(b".");
            mac.update(body);
            // constant-time comparison
            if mac.verify_slice(signature).is_ok() {
                return Ok(timestamp);
            }
        }
    }
    Err(SignatureError::Mismatch)
}

/// Make a signature header the way Stripe does (for tests and local
/// webhook replays; never used to accept anything)
pub fn sign_stripe_payload(body: &[u8], secret: &str, timestamp: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!(
        "t={timestamp},v1={}",
        hex::encode(mac.finalize().into_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_test_secret";
    const BODY: &[u8] = br#"{"id":"evt_1","type":"checkout.session.completed"}"#;

    fn secrets() -> Vec<String> {
        vec![SECRET.to_string()]
    }

    #[test]
    fn accepts_valid_signature() {
        let header = sign_stripe_payload(BODY, SECRET, 1_000);
        assert_eq!(verify_stripe_signature(&header, BODY, &secrets(), 1_010), Ok(1_000));
    }

    #[test]
    fn accepts_previous_secret_while_rotating() {
        let header = sign_stripe_payload(BODY, "whsec_old", 1_000);
        let both = vec![SECRET.to_string(), "whsec_old".to_string()];
        assert_eq!(verify_stripe_signature(&header, BODY, &both, 1_000), Ok(1_000));
    }

    #[test]
    fn rejects_wrong_secret() {
        let header = sign_stripe_payload(BODY, "whsec_other", 1_000);
        assert_eq!(
            verify_stripe_signature(&header, BODY, &secrets(), 1_000),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn rejects_changed_body() {
        let header = sign_stripe_payload(BODY, SECRET, 1_000);
        let changed = br#"{"id":"evt_1","type":"checkout.session.completed" }"#;
        assert_eq!(
            verify_stripe_signature(&header, changed, &secrets(), 1_000),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn rejects_replay_outside_window() {
        let header = sign_stripe_payload(BODY, SECRET, 1_000);
        assert_eq!(
            verify_stripe_signature(&header, BODY, &secrets(), 1_000 + 301),
            Err(SignatureError::OutsideTolerance)
        );
        assert_eq!(
            verify_stripe_signature(&header, BODY, &secrets(), 1_000 - 301),
            Err(SignatureError::OutsideTolerance)
        );
    }

    #[test]
    fn rejects_changed_timestamp() {
        let header = sign_stripe_payload(BODY, SECRET, 1_000).replace("t=1000", "t=1001");
        assert_eq!(
            verify_stripe_signature(&header, BODY, &secrets(), 1_001),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn rejects_malformed_headers() {
        for header in ["", "t=1000", "v1=abcd", "garbage", "t=x,v1=00"] {
            assert!(verify_stripe_signature(header, BODY, &secrets(), 1_000).is_err());
        }
        assert_eq!(
            verify_stripe_signature("t=1000,v1=00", BODY, &[], 1_000),
            Err(SignatureError::NoSecret)
        );
    }

    #[test]
    fn ignores_v0_signatures() {
        // v0 is Stripe's test-only scheme and must not be accepted
        let good = sign_stripe_payload(BODY, SECRET, 1_000);
        let v0 = good.replace("v1=", "v0=");
        assert_eq!(
            verify_stripe_signature(&v0, BODY, &secrets(), 1_000),
            Err(SignatureError::Malformed)
        );
    }
}
