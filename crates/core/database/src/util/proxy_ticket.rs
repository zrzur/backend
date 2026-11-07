//! Tickets for the media proxy. Images load from `<img src>`, so they cannot
//! send a session, and a session must never go in a URL. A signed ticket with
//! a short life can: January checks it with no database call.
//!
//! Format: `<expires>.<user id>.<hex HMAC-SHA256 of "<expires>.<user id>">`,
//! key `january.proxy_key`. January has its own check of the same format.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Seconds a ticket works
pub const PROXY_TICKET_SECONDS: i64 = 60 * 60;

/// Sign a ticket for `user_id` that works until `expires` (Unix seconds)
pub fn sign_proxy_ticket(key: &str, user_id: &str, expires: i64) -> String {
    let payload = format!("{expires}.{user_id}");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(payload.as_bytes());
    format!("{payload}.{}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same value is in January's tests: both sides must agree
    #[test]
    fn a_known_ticket_is_signed_the_same() {
        assert_eq!(
            sign_proxy_ticket("test-proxy-key", "01ABCDEFGHJKMNPQRSTVWXYZ01", 1893456000),
            "1893456000.01ABCDEFGHJKMNPQRSTVWXYZ01.2907d453dff601dc82f3401977444db4a2570d5c0d2a4215f4cab2ad17ad8dc5"
        );
    }
}
