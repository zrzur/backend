//! Limit on new accounts per source address, on top of the captcha: a bot
//! that solves one captcha still gets only a few accounts a day from one
//! address. The address is kept only as a hash in Redis, for a day.

use hmac::{Hmac, Mac};
use revolt_config::config;
use sha2::Sha256;

use redis_kiss::{get_connection, AsyncCommands};

const DAY_SECONDS: usize = 24 * 60 * 60;

const TAG: &[u8] = b"zrzur-signup-limit\0";

/// HMAC-SHA256 under the server's `files.encryption_key`, hex, 32 characters:
/// without the key a stored value cannot be turned back into an address
/// by trying every IPv4 address
async fn digest(value: &str) -> String {
    keyed_digest(&config().await.files.encryption_key, value)
}

fn keyed_digest(key: &str, value: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("any key length");
    mac.update(TAG);
    mac.update(value.as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..16])
}

/// Count one sign-up from `source`; true while it is within `limit` a day
/// (`api.security.captcha.signups_per_source_per_day`). Fails closed: when
/// Redis cannot answer, the sign-up is refused, so the limit cannot be
/// bypassed by making the cache fail.
pub async fn allow_signup(source: &str, limit: i64) -> bool {
    let Ok(mut redis) = get_connection().await else {
        return false;
    };
    let key = format!("signup:{}", digest(source).await);
    let Ok(count) = redis.incr::<_, _, i64>(&key, 1).await else {
        return false;
    };
    if count == 1 {
        let _: Result<(), _> = redis.expire(&key, DAY_SECONDS).await;
    }
    count <= limit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_source_hashes_the_same_and_others_differ() {
        assert_eq!(keyed_digest("k", "203.0.113.7"), keyed_digest("k", "203.0.113.7"));
        assert_ne!(keyed_digest("k", "203.0.113.7"), keyed_digest("k", "203.0.113.8"));
    }

    #[test]
    fn the_stored_value_depends_on_the_server_key() {
        // without the key nobody can rebuild the value from an address
        assert_ne!(keyed_digest("k1", "203.0.113.7"), keyed_digest("k2", "203.0.113.7"));
        assert_eq!(keyed_digest("k", "203.0.113.7").len(), 32);
    }
}
