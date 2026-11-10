//! Wrong passwords, counted per e-mail address (normalised) whether or not
//! an account has that address, so the answers never tell which addresses
//! are registered.
//!
//! - Fewer than [`LOGIN_STEP_UP_AFTER`] wrong passwords: nothing changes.
//! - From then on, with a captcha configured, every attempt for the address
//!   needs a solved captcha, from anywhere. The owner can always sign in;
//!   a script has to solve a captcha per guess.
//! - Without a captcha, the address waits after each further wrong password
//!   from the same network (IPv4 /24, IPv6 /64): one minute, then two,
//!   four, ... up to an hour. Someone guessing from elsewhere does not make
//!   the owner wait at home.
//! - The counts are forgotten [`LOGIN_FAILURE_WINDOW`] seconds after the
//!   last wrong password, after a right one, and after a password reset.
//!
//! Keys hold an HMAC-SHA256 of the address (and network) under
//! `files.encryption_key`, never the address. Counting is one atomic step
//! (MULTI: increment and expiry together), and decisions use the count the
//! increment returned.
//!
//! Needs Redis, like the two-factor limit: when it is unreachable the check
//! is skipped, as a broken cache must not lock people out of their accounts
//! (the per-address rate limit still applies).

use hmac::{Hmac, Mac};
use redis_kiss::{get_connection, redis};
use revolt_config::config;
use sha2::Sha256;

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
    /// Nothing gets through from this network for this many more seconds
    Paused(u64),
}

/// The network a client address counts as: IPv4 (also IPv4 written as
/// IPv6) by its /24, IPv6 by its /64. Anything else stays as it is.
pub fn network_of(ip: &str) -> String {
    match ip.trim().parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            let o = v4.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        Ok(std::net::IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => network_of(&v4.to_string()),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        Err(_) => ip.trim().to_string(),
    }
}

/// HMAC-SHA256 under the server's `files.encryption_key`, hex, 32 characters
async fn digest(value: &str) -> String {
    let key = config().await.files.encryption_key;
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("any key length");
    mac.update(b"zrzur-login-limit\0");
    mac.update(value.as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..16])
}

/// Wrong passwords for the address, from anywhere (the captcha step-up)
async fn failures_key(email: &str) -> String {
    format!("login:fail:{}", digest(email).await)
}

/// Per-network counts and waits of the address: a hash with one
/// `<network>:n` (count) and `<network>:until` (unix seconds) per network
async fn networks_key(email: &str) -> String {
    format!("login:net:{}", digest(email).await)
}

async fn network_field(network: &str) -> String {
    digest(&format!("net\0{network}")).await
}

/// The wait after the `count`-th wrong password, without a captcha
pub fn pause_after(count: i64) -> Option<u64> {
    if count < LOGIN_STEP_UP_AFTER {
        return None;
    }
    let doublings = (count - LOGIN_STEP_UP_AFTER).min(16) as u32;
    Some((LOGIN_PAUSE_FIRST << doublings).min(LOGIN_PAUSE_MAX))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The gate for `email` (normalised) asked from `ip`. `captcha` is whether
/// the server has a captcha configured.
pub async fn login_gate(email: &str, ip: &str, captcha: bool) -> LoginGate {
    let Ok(conn) = get_connection().await else {
        return LoginGate::Open;
    };
    let mut conn = conn.into_inner();
    let field = network_field(&network_of(ip)).await;
    let (count, until): (Option<i64>, Option<u64>) = match redis::pipe()
        .get(failures_key(email).await)
        .hget(networks_key(email).await, format!("{field}:until"))
        .query_async(&mut conn)
        .await
    {
        Ok(values) => values,
        Err(_) => return LoginGate::Open,
    };
    if captcha {
        if count.unwrap_or(0) >= LOGIN_STEP_UP_AFTER {
            return LoginGate::Captcha;
        }
        return LoginGate::Open;
    }
    match until {
        Some(until) if until > now() => LoginGate::Paused(until - now()),
        _ => LoginGate::Open,
    }
}

/// Count one wrong password (or unknown address) for `email` from `ip`
pub async fn count_login_failure(email: &str, ip: &str, captcha: bool) {
    let Ok(conn) = get_connection().await else {
        return;
    };
    let mut conn = conn.into_inner();
    let failures = failures_key(email).await;
    let networks = networks_key(email).await;
    let field = network_field(&network_of(ip)).await;
    // the counts outlive the longest wait, so the next wrong password
    // after a wait waits longer
    let keep = (LOGIN_FAILURE_WINDOW + LOGIN_PAUSE_MAX) as usize;
    let counted: Result<(i64, i64, i64, i64), _> = redis::pipe()
        .atomic()
        .incr(&failures, 1i64)
        .expire(&failures, LOGIN_FAILURE_WINDOW as usize)
        .hincr(&networks, format!("{field}:n"), 1i64)
        .expire(&networks, keep)
        .query_async(&mut conn)
        .await;
    let Ok((_, _, from_here, _)) = counted else {
        return;
    };
    if captcha {
        return;
    }
    if let Some(pause) = pause_after(from_here) {
        let _: Result<(), _> = redis::cmd("HSET")
            .arg(&networks)
            .arg(format!("{field}:until"))
            .arg(now() + pause)
            .query_async(&mut conn)
            .await;
    }
}

/// Forget every count and wait of the address: after a right password, and
/// after a password reset (the owner proved they hold the mailbox)
pub async fn clear_login_failures(email: &str) {
    if let Ok(conn) = get_connection().await {
        let mut conn = conn.into_inner();
        let _: Result<(), _> = redis::cmd("DEL")
            .arg(failures_key(email).await)
            .arg(networks_key(email).await)
            .query_async(&mut conn)
            .await;
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

    #[test]
    fn networks_are_a_slash_24_or_a_slash_64() {
        assert_eq!(network_of("203.0.113.7"), "203.0.113.0/24");
        assert_eq!(network_of("::ffff:203.0.113.7"), "203.0.113.0/24");
        assert_eq!(network_of("2001:db8:1:2:3:4:5:6"), "2001:db8:1:2::/64");
        assert_eq!(network_of(""), "");
    }

    #[tokio::test]
    async fn keys_hold_no_address() {
        let email = "someone@example.test";
        for key in [failures_key(email).await, networks_key(email).await] {
            assert!(!key.contains("someone"), "{key}");
            assert_eq!(key.rsplit(':').next().unwrap().len(), 32);
        }
        assert_ne!(digest("a").await, digest("b").await);
    }

    #[tokio::test]
    async fn without_a_captcha_the_tenth_wrong_password_pauses_that_network() {
        let email = fresh("pause");
        for _ in 0..LOGIN_STEP_UP_AFTER - 1 {
            count_login_failure(&email, "203.0.113.7", false).await;
        }
        assert_eq!(login_gate(&email, "203.0.113.7", false).await, LoginGate::Open);
        count_login_failure(&email, "203.0.113.7", false).await;
        // the same /24 waits
        assert!(matches!(
            login_gate(&email, "203.0.113.99", false).await,
            LoginGate::Paused(s) if s > 0 && s <= 60
        ));
        // another network does not
        assert_eq!(login_gate(&email, "198.51.100.7", false).await, LoginGate::Open);
        clear_login_failures(&email).await;
        assert_eq!(login_gate(&email, "203.0.113.7", false).await, LoginGate::Open);
    }

    #[tokio::test]
    async fn with_a_captcha_the_address_asks_for_one_everywhere_and_never_pauses() {
        let email = fresh("captcha");
        for _ in 0..LOGIN_STEP_UP_AFTER + 3 {
            count_login_failure(&email, "203.0.113.7", true).await;
        }
        assert_eq!(login_gate(&email, "203.0.113.7", true).await, LoginGate::Captcha);
        assert_eq!(login_gate(&email, "198.51.100.7", true).await, LoginGate::Captcha);
        // another address is not affected
        assert_eq!(login_gate(&fresh("other"), "203.0.113.7", true).await, LoginGate::Open);
        clear_login_failures(&email).await;
        assert_eq!(login_gate(&email, "203.0.113.7", true).await, LoginGate::Open);
    }

    #[tokio::test]
    async fn counts_from_many_tasks_at_once_are_all_kept() {
        let email = fresh("race");
        let tasks: Vec<_> = (0..LOGIN_STEP_UP_AFTER)
            .map(|_| {
                let email = email.clone();
                tokio::spawn(async move { count_login_failure(&email, "203.0.113.7", false).await })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        assert!(matches!(
            login_gate(&email, "203.0.113.7", false).await,
            LoginGate::Paused(_)
        ));
        clear_login_failures(&email).await;
    }
}
