//! Signed requests from Zrzur to someone else's server.
//!
//! Used by bot interaction endpoints; written for any feature that calls out
//! to a third party, which names itself with a `purpose`. Sending lives in
//! delta (`util::signed_delivery`), this is the part with no I/O.
//!
//! Every request carries `X-Zrzur-Timestamp`, `X-Zrzur-Signature` and
//! `X-Zrzur-Delivery`. The signature is `v1=` and the hex HMAC-SHA256 of
//! `v1:<timestamp>:<raw body>`, so the time can't be changed on its own.
//!
//! Secrets are derived from the instance's key, one per owner, purpose and
//! generation: a copy of the database holds none, and a new generation
//! replaces the old one. Keep the generation number with the owner.

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const HEADER_TIMESTAMP: &str = "X-Zrzur-Timestamp";
pub const HEADER_SIGNATURE: &str = "X-Zrzur-Signature";
pub const HEADER_DELIVERY: &str = "X-Zrzur-Delivery";

/// How far a timestamp may be from the receiver's clock, in seconds. An old
/// captured request is refused for this reason alone; inside the window
/// receivers drop a `X-Zrzur-Delivery` they have already seen.
pub const TIMESTAMP_TOLERANCE_SECS: i64 = 300;

fn mac(key: &str) -> Hmac<Sha256> {
    Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC takes a key of any length")
}

/// The secret of one owner for one purpose and generation. Starts with
/// `zrzur_sign_` so a leaked one is easy to spot.
pub fn derive_secret(signing_key: &str, purpose: &str, owner_id: &str, version: i32) -> String {
    let mut mac = mac(signing_key);
    mac.update(format!("zrzur {purpose} secret\0{owner_id}\0{version}").as_bytes());
    format!("zrzur_sign_{}", hex::encode(mac.finalize().into_bytes()))
}

/// The value of `X-Zrzur-Signature`
pub fn sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = mac(secret);
    mac.update(format!("v1:{timestamp}:").as_bytes());
    mac.update(body);
    format!("v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// Check a signature in constant time, and that its time is close to `now`.
/// What a receiver must do; here for Rust receivers and as the reference.
pub fn verify(secret: &str, timestamp: i64, body: &[u8], signature: &str, now: i64) -> bool {
    if timestamp.abs_diff(now) > TIMESTAMP_TOLERANCE_SECS as u64 {
        return false;
    }
    let Some(given) = signature
        .strip_prefix("v1=")
        .and_then(|hex_part| hex::decode(hex_part).ok())
    else {
        return false;
    };
    let mut mac = mac(secret);
    mac.update(format!("v1:{timestamp}:").as_bytes());
    mac.update(body);
    // `verify_slice` compares in constant time
    mac.verify_slice(&given).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{derive_secret, sign, verify, TIMESTAMP_TOLERANCE_SECS};

    const BODY: &[u8] = br#"{"type":"Ping"}"#;
    const NOW: i64 = 1700000000;

    #[test]
    fn signing_matches_the_published_example() {
        // the same values are in the SDK's tests and the developer docs
        let secret = derive_secret("instance-key", "interactions", "01BOT", 1);
        assert_eq!(
            secret,
            "zrzur_sign_f27c5773e21ddeceaeb5e956dc45c16451f737addd5c0e4472f61060bca9b030"
        );
        assert_eq!(
            sign(&secret, NOW, BODY),
            "v1=928e5ffdc88564103b59f5485a6bfa7e1a633dca64f397396606a314f572c4d3"
        );
    }

    #[test]
    fn a_secret_belongs_to_one_owner_purpose_and_generation() {
        let secret = derive_secret("key", "interactions", "a", 1);
        assert_ne!(secret, derive_secret("key", "interactions", "a", 2));
        assert_ne!(secret, derive_secret("key", "interactions", "b", 1));
        assert_ne!(secret, derive_secret("key", "other", "a", 1));
        assert_ne!(secret, derive_secret("other", "interactions", "a", 1));
    }

    #[test]
    fn only_the_exact_request_verifies() {
        let good = sign("s", NOW, BODY);
        assert!(verify("s", NOW, BODY, &good, NOW));
        assert!(!verify("other", NOW, BODY, &good, NOW));
        assert!(!verify("s", NOW, b"{}", &good, NOW));
        // the time is signed: moving it breaks the signature
        assert!(!verify("s", NOW + 1, BODY, &good, NOW));
        assert!(!verify("s", NOW, BODY, "v1=zz", NOW));
        assert!(!verify("s", NOW, BODY, "v2=00", NOW));
        assert!(!verify("s", NOW, BODY, "", NOW));
    }

    #[test]
    fn an_old_or_future_request_is_a_replay() {
        let good = sign("s", NOW, BODY);
        assert!(verify("s", NOW, BODY, &good, NOW + TIMESTAMP_TOLERANCE_SECS));
        assert!(!verify("s", NOW, BODY, &good, NOW + TIMESTAMP_TOLERANCE_SECS + 1));
        assert!(!verify("s", NOW, BODY, &good, NOW - TIMESTAMP_TOLERANCE_SECS - 1));
    }
}
