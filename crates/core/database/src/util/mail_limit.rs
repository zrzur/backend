//! Limits for mail that anyone can ask for without being signed in
//! (password reset, verification): one mail per address every five minutes,
//! five a day, and a cap per source address. A refused request is skipped
//! silently by the caller, so the answer never reveals whether an account
//! exists.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use redis_kiss::{
    get_connection,
    redis::{ExistenceCheck, SetExpiry, SetOptions},
    AsyncCommands,
};

/// Seconds between two mails to the same address
pub const MAIL_COOLDOWN_SECONDS: usize = 300;
/// Mails a day to the same address
pub const MAIL_PER_ADDRESS_PER_DAY: i64 = 5;
/// Mails an hour that one source address may ask for
pub const MAIL_PER_SOURCE_PER_HOUR: i64 = 20;

fn digest(value: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(value.as_bytes());
    hash.finish()
}

/// Count one request against `counter`; true while it is within `limit`.
/// `None` when Redis cannot answer.
async fn within(counter: &str, limit: i64, window: usize) -> Option<bool> {
    let mut redis = get_connection().await.ok()?;
    let count: i64 = redis.incr(counter, 1).await.ok()?;
    if count == 1 {
        let _: Result<(), _> = redis.expire(counter, window).await;
    }
    Some(count <= limit)
}

/// Start the cooldown; false if one is already running
async fn start_cooldown(key: &str) -> Option<bool> {
    let mut redis = get_connection().await.ok()?;
    let first: Option<String> = redis
        .set_options(
            key,
            1,
            SetOptions::default()
                .conditional_set(ExistenceCheck::NX)
                .with_expiration(SetExpiry::EX(MAIL_COOLDOWN_SECONDS)),
        )
        .await
        .ok()?;
    Some(first.is_some())
}

/// Whether a mail of `kind` may be sent now to `email` (normalised) for a
/// request from `source`. Counts the request. When Redis is unreachable the
/// mail is allowed: a broken cache must not lock people out of recovery.
pub async fn allow_mail(kind: &str, email: &str, source: Option<&str>) -> bool {
    if let Some(source) = source.filter(|source| !source.is_empty()) {
        let key = format!("mail:src:{}", digest(source));
        if within(&key, MAIL_PER_SOURCE_PER_HOUR, 3600).await == Some(false) {
            return false;
        }
    }

    let email = digest(email);
    if start_cooldown(&format!("mail:cd:{kind}:{email}")).await == Some(false) {
        return false;
    }

    within(&format!("mail:day:{email}"), MAIL_PER_ADDRESS_PER_DAY, 86_400)
        .await
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(name: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{name}-{nanos}")
    }

    #[tokio::test]
    async fn a_second_mail_within_five_minutes_is_refused() {
        let email = fresh("cooldown@example.test");
        let source = fresh("198.51.100.1");
        assert!(allow_mail("reset", &email, Some(&source)).await);
        assert!(!allow_mail("reset", &email, Some(&source)).await);
        // another kind of mail has its own cooldown
        assert!(allow_mail("verify", &email, Some(&source)).await);
    }

    #[tokio::test]
    async fn one_source_cannot_mail_many_addresses() {
        let source = fresh("198.51.100.2");
        for n in 0..MAIL_PER_SOURCE_PER_HOUR {
            assert!(allow_mail("reset", &fresh(&format!("a{n}@example.test")), Some(&source)).await);
        }
        assert!(!allow_mail("reset", &fresh("late@example.test"), Some(&source)).await);
    }
}
