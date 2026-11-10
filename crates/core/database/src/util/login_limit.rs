//! Wrong passwords counted per e-mail address (normalised), whether or not
//! an account has that address, so the answers never tell which addresses
//! are registered.
//!
//! - Fewer than [`LOGIN_STEP_UP_AFTER`] wrong passwords: nothing changes.
//! - From then on, with a captcha configured, every attempt for the address
//!   needs a solved captcha. The owner can always sign in (no lock anyone
//!   else can hold); a script has to solve a captcha per guess.
//! - Without a captcha the address waits after each further wrong password:
//!   one minute, then two, four, ... up to an hour.
//! - The count is forgotten [`LOGIN_FAILURE_WINDOW`] seconds after the last
//!   wrong password, and at once after a right one.
//!
//! Needs Redis, like the two-factor limit: when it is unreachable the check
//! is skipped, as a broken cache must not lock people out of their accounts
//! (the per-address rate limit still applies).

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use redis_kiss::{get_connection, AsyncCommands};

/// Wrong passwords for one address before the step-up starts
pub const LOGIN_STEP_UP_AFTER: i64 = 10;
/// Seconds a wrong password is remembered after the last one
pub const LOGIN_FAILURE_WINDOW: u64 = 15 * 60;
/// First wait without a captcha, in seconds; it doubles each time
pub const LOGIN_PAUSE_FIRST: u64 = 60;
/// Longest wait without a captcha, in seconds
pub const LOGIN_PAUSE_MAX: u64 = 60 * 60;

/// What a sign-in attempt for an address must get past first
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginGate {
    /// Nothing extra
    Open,
    /// A solved captcha
    Captcha,
    /// Nothing gets through for this many more seconds
    Paused(u64),
}

fn digest(email: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(email.as_bytes());
    hash.finish()
}

fn failures_key(email: &str) -> String {
    format!("login:fail:{}", digest(email))
}

fn pause_key(email: &str) -> String {
    format!("login:pause:{}", digest(email))
}

/// The wait after the `count`-th wrong password, without a captcha
pub fn pause_after(count: i64) -> Option<u64> {
    if count < LOGIN_STEP_UP_AFTER {
        return None;
    }
    let doublings = (count - LOGIN_STEP_UP_AFTER).min(16) as u32;
    Some((LOGIN_PAUSE_FIRST << doublings).min(LOGIN_PAUSE_MAX))
}

/// The gate for `email` (normalised). `captcha` is whether the server has a
/// captcha configured.
pub async fn login_gate(email: &str, captcha: bool) -> LoginGate {
    let Ok(mut redis) = get_connection().await else {
        return LoginGate::Open;
    };
    let paused: i64 = redis.ttl(pause_key(email)).await.unwrap_or(-2);
    if paused > 0 {
        return LoginGate::Paused(paused as u64);
    }
    let count: i64 = redis
        .get::<_, Option<i64>>(failures_key(email))
        .await
        .ok()
        .flatten()
        .unwrap_or(0);
    if captcha && count >= LOGIN_STEP_UP_AFTER {
        LoginGate::Captcha
    } else {
        LoginGate::Open
    }
}

/// Count one wrong password (or unknown address) for `email`
pub async fn count_login_failure(email: &str, captcha: bool) {
    let Ok(mut redis) = get_connection().await else {
        return;
    };
    let key = failures_key(email);
    let Ok(count) = redis.incr::<_, _, i64>(&key, 1).await else {
        return;
    };
    let pause = if captcha { None } else { pause_after(count) };
    // the count outlives the wait, so the next wrong password waits longer
    let keep = LOGIN_FAILURE_WINDOW + pause.unwrap_or(0);
    let _: Result<(), _> = redis.expire(&key, keep as usize).await;
    if let Some(pause) = pause {
        let _: Result<(), _> = redis.set_ex(pause_key(email), 1, pause as usize).await;
    }
}

/// Forget the wrong passwords after a right one
pub async fn clear_login_failures(email: &str) {
    if let Ok(mut redis) = get_connection().await {
        let _: Result<(), _> = redis.del(&[failures_key(email), pause_key(email)]).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(name: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{name}-{nanos}@example.test")
    }

    #[test]
    fn the_wait_doubles_from_a_minute_up_to_an_hour() {
        assert_eq!(pause_after(LOGIN_STEP_UP_AFTER - 1), None);
        assert_eq!(pause_after(LOGIN_STEP_UP_AFTER), Some(60));
        assert_eq!(pause_after(LOGIN_STEP_UP_AFTER + 1), Some(120));
        assert_eq!(pause_after(LOGIN_STEP_UP_AFTER + 5), Some(1920));
        assert_eq!(pause_after(LOGIN_STEP_UP_AFTER + 6), Some(3600));
        assert_eq!(pause_after(i64::MAX), Some(3600));
    }

    #[tokio::test]
    async fn without_a_captcha_the_tenth_wrong_password_pauses_the_address() {
        let email = fresh("pause");
        for _ in 0..LOGIN_STEP_UP_AFTER - 1 {
            count_login_failure(&email, false).await;
        }
        assert_eq!(login_gate(&email, false).await, LoginGate::Open);
        count_login_failure(&email, false).await;
        assert!(matches!(login_gate(&email, false).await, LoginGate::Paused(s) if s > 0 && s <= 60));
        clear_login_failures(&email).await;
        assert_eq!(login_gate(&email, false).await, LoginGate::Open);
    }

    #[tokio::test]
    async fn with_a_captcha_the_address_asks_for_one_and_never_pauses() {
        let email = fresh("captcha");
        for _ in 0..LOGIN_STEP_UP_AFTER + 3 {
            count_login_failure(&email, true).await;
        }
        assert_eq!(login_gate(&email, true).await, LoginGate::Captcha);
        // another address is not affected
        assert_eq!(login_gate(&fresh("other"), true).await, LoginGate::Open);
        clear_login_failures(&email).await;
        assert_eq!(login_gate(&email, true).await, LoginGate::Open);
    }
}
