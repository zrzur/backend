//! Limit on new accounts per source address, on top of the captcha: a bot
//! that solves one captcha still gets only a few accounts a day from one
//! address. The address is kept only as a hash in Redis, for a day.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use redis_kiss::{get_connection, AsyncCommands};

const DAY_SECONDS: usize = 24 * 60 * 60;

fn digest(value: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(value.as_bytes());
    hash.finish()
}

/// Count one sign-up from `source`; true while it is within `limit` a day
/// (`api.security.captcha.signups_per_source_per_day`). Fails closed: when
/// Redis cannot answer, the sign-up is refused, so the limit cannot be
/// bypassed by making the cache fail.
pub async fn allow_signup(source: &str, limit: i64) -> bool {
    let Ok(mut redis) = get_connection().await else {
        return false;
    };
    let key = format!("signup:{}", digest(source));
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
        assert_eq!(digest("203.0.113.7"), digest("203.0.113.7"));
        assert_ne!(digest("203.0.113.7"), digest("203.0.113.8"));
    }
}
