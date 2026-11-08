use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::Hasher;
use std::ops::Add;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use redis_kiss::redis::ExistenceCheck;
use redis_kiss::{
    get_connection,
    redis::{Pipeline, SetExpiry, SetOptions, aio::Connection},
};
use revolt_result::ToRevoltError;
use serde::Serialize;

static IS_TEST_ENV: LazyLock<bool> = LazyLock::new(|| std::env::var("TEST_DB").is_ok());

pub trait RequestKind {
    type R<'a>;
}

pub trait RatelimitResolver<R>: Send + Sync {
    fn resolve_bucket<'a>(&self, request: &'a R) -> (&'a str, Option<&'a str>);
    fn resolve_bucket_limit(&self, bucket: &str) -> u32;
}

#[derive(Clone)]
pub struct RatelimitStorage<K: RequestKind> {
    pub resolver: Arc<dyn for<'a> RatelimitResolver<K::R<'a>>>,
}

impl<K: RequestKind> RatelimitStorage<K> {
    pub fn new<R: for<'a> RatelimitResolver<K::R<'a>> + 'static>(resolver: R) -> Self {
        Self {
            resolver: Arc::new(resolver),
        }
    }
}

/// Ratelimit Bucket
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    used: u32,
    reset: u128,
}

/// Get the current time from Unix Epoch as a Duration
fn now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards...")
}

impl Entry {
    /// Find bucket by its key, and incremement its usage count
    pub async fn from(conn: &mut Connection, now: Duration, key: u64, window: Duration) -> Entry {
        let expire = now.add(window).as_millis();

        let (used, _, reset) = Pipeline::new()
            .incr(format!("rl:{key}:used"), 1)
            .set_options(
                format!("rl:{key}:reset"),
                expire as usize,
                SetOptions::default()
                    .conditional_set(ExistenceCheck::NX)
                    .with_expiration(SetExpiry::EX(window.as_secs().max(1) as usize)),
            )
            .get(format!("rl:{key}:reset"))
            .query_async::<_, (u32, (), u128)>(conn)
            .await
            .ok()
            .unwrap_or((1, (), expire));

        Entry { used, reset }
    }

    /// Check if the entry is expired, and reset it if so
    pub fn is_expired(&mut self, now: Duration, window: Duration) {
        let current_time = now.as_millis();

        if current_time > self.reset {
            self.used = 1;
            self.reset = now.add(window).as_millis();
        };
    }

    /// Save information
    pub async fn save(self, conn: &mut Connection, key: u64) {
        let _ = Pipeline::new()
            .pexpire_at(format!("rl:{key}:used"), self.reset as usize)
            .pexpire_at(format!("rl:{key}:reset"), self.reset as usize)
            .query_async::<_, ()>(conn)
            .await
            .to_internal_error();
    }

    /// Get remaining units in the bucket
    pub fn get_remaining(&self, now: Duration, limit: u32) -> u32 {
        if now.as_millis() > self.reset {
            limit
        } else {
            (limit.saturating_add(1)).saturating_sub(self.used)
        }
    }

    /// Get how long bucket has until reset
    pub fn left_until_reset(&self, now: Duration) -> u128 {
        let current_time = now.as_millis();
        self.reset.saturating_sub(current_time)
    }
}

/// The storage key of one identifier in one bucket
pub fn bucket_key(identifier: &str, (bucket, resource): (&str, Option<&str>)) -> u64 {
    let mut key = DefaultHasher::new();
    key.write(identifier.as_bytes());
    key.write(bucket.as_bytes());
    if let Some(id) = resource {
        key.write(id.as_bytes());
    }
    key.finish()
}

/// Ratelimit Guard
#[derive(Serialize, Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct Ratelimiter {
    pub key: u64,
    pub limit: u32,
    pub remaining: u32,
    pub reset: u128,
    /// The global per-bot limit is the one that was hit
    #[serde(skip)]
    pub global: bool,
}

impl Ratelimiter {
    /// Generate guard from identifier and target bucket
    pub async fn from(
        identifier: &str,
        limit: u32,
        bucket: (&str, Option<&str>),
    ) -> Result<Ratelimiter, Ratelimiter> {
        let key = bucket_key(identifier, bucket);

        if *IS_TEST_ENV {
            return Ok(Ratelimiter {
                key,
                limit,
                remaining: limit,
                reset: 10000,
                global: false,
            });
        }

        Self::count(key, limit, Duration::from_secs(10)).await
    }

    /// The global limit of a bot: [`BOT_GLOBAL_LIMIT`] requests a second
    /// across every route, on top of the route buckets
    pub async fn global_for_bot(bot_id: &str) -> Result<Ratelimiter, Ratelimiter> {
        if *IS_TEST_ENV {
            return Ok(Ratelimiter {
                key: 0,
                limit: BOT_GLOBAL_LIMIT,
                remaining: BOT_GLOBAL_LIMIT,
                reset: 1000,
                global: true,
            });
        }
        Self::count_global(bot_id).await
    }

    /// Count one request against the global limit (also used by tests)
    pub async fn count_global(bot_id: &str) -> Result<Ratelimiter, Ratelimiter> {
        let mut key = DefaultHasher::new();
        key.write(b"bot-global:");
        key.write(bot_id.as_bytes());
        let mark = |mut r: Ratelimiter| {
            r.global = true;
            r
        };
        Self::count(key.finish(), BOT_GLOBAL_LIMIT, Duration::from_secs(1))
            .await
            .map(mark)
            .map_err(mark)
    }

    /// Count one request in a bucket (`window` long) and say whether it may pass
    pub async fn count(key: u64, limit: u32, window: Duration) -> Result<Ratelimiter, Ratelimiter> {
        let mut conn = get_connection()
            .await
            .expect("Failed to get redis connection")
            .into_inner();

        let now = now();
        let mut entry = Entry::from(&mut conn, now, key, window).await;

        let remaining = entry.get_remaining(now, limit);
        let reset = entry.left_until_reset(now);
        let mut ratelimiter = Ratelimiter {
            key,
            limit,
            remaining,
            reset,
            global: false,
        };
        if remaining == 0 {
            return Err(ratelimiter);
        }

        entry.is_expired(now, window);
        entry.save(&mut conn, key).await;
        ratelimiter.remaining = ratelimiter.remaining.saturating_sub(1);

        Ok(ratelimiter)
    }
}

impl RatelimitInformation {
    /// The body of a 429
    pub fn failure(ratelimiter: &Ratelimiter) -> RatelimitInformation {
        RatelimitInformation::Failure {
            retry_after: ratelimiter.reset,
            global: ratelimiter.global,
        }
    }
}

/// Requests a second one bot may make across every route
pub const BOT_GLOBAL_LIMIT: u32 = 50;

/// Refused requests (401, 403) from one address within ten minutes
/// before it is blocked for ten minutes: token guessing, abusive clients
pub const BAD_REQUESTS_LIMIT: u32 = 10_000;
const BAD_REQUESTS_WINDOW: usize = 600;

/// Who a request is counted against: a bot by its id (the same from any
/// address, and separate from other bots on the same host), else the
/// session, else the address
pub fn identifier(bot: Option<&str>, session: Option<&str>, ip: &str) -> String {
    match (bot, session) {
        (Some(bot), _) => format!("bot:{bot}"),
        (None, Some(session)) => session.to_string(),
        (None, None) => address_group(ip),
    }
}

/// The address a limit counts against. IPv6 hosts usually get a whole /64,
/// so one person could take a fresh address (and a fresh bucket) for every
/// request; all addresses of one /64 count as one. IPv4, and IPv4 written as
/// IPv6 (`::ffff:a.b.c.d`), stay one address. Anything that is not an address
/// is kept as it is.
pub fn address_group(ip: &str) -> String {
    match ip.trim().parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.to_string(),
        Ok(std::net::IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        Err(_) => ip.to_string(),
    }
}

/// Sign-in and account routes are counted against the address only. A
/// session (or many sessions of one person) must never give extra room for
/// guessing passwords, codes or e-mail addresses.
pub fn is_address_only_bucket(bucket: &str) -> bool {
    matches!(bucket, "auth" | "auth_delete" | "staff_auth")
}

/// [`identifier`], but address-only for the buckets that need it
pub fn identifier_for_bucket(
    bucket: &str,
    bot: Option<&str>,
    session: Option<&str>,
    ip: &str,
) -> String {
    if is_address_only_bucket(bucket) {
        format!("ip:{}", address_group(ip))
    } else {
        identifier(bot, session, ip)
    }
}

/// Bot tokens already looked up: hash of the token → (bot id, when). Only
/// a hash is kept, never the token.
static BOT_TOKENS: LazyLock<Mutex<HashMap<u64, (Option<String>, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The bot a token belongs to, cached for a minute (unknown tokens for ten
/// seconds) so the limiter doesn't ask the database on every request
pub async fn bot_for_token(db: &revolt_database::Database, token: &str) -> Option<String> {
    let mut hash = DefaultHasher::new();
    hash.write(token.as_bytes());
    let hash = hash.finish();
    if let Some((bot, at)) = BOT_TOKENS.lock().ok().and_then(|cache| cache.get(&hash).cloned()) {
        let fresh = if bot.is_some() { 60 } else { 10 };
        if at.elapsed() < Duration::from_secs(fresh) {
            return bot;
        }
    }
    let bot = db.fetch_bot_by_token(token).await.ok().map(|bot| bot.id);
    if let Ok(mut cache) = BOT_TOKENS.lock() {
        if cache.len() > 50_000 {
            cache.clear();
        }
        cache.insert(hash, (bot.clone(), Instant::now()));
    }
    bot
}

/// Answers that count towards blocking an address. A 429 does not: it is
/// already refused, and everyone behind one proxy shares an address, so
/// counting it would let one busy client block them all.
pub fn counts_as_refused(status: u16) -> bool {
    matches!(status, 401 | 403)
}

fn address_key(kind: &str, ip: &str) -> String {
    let mut hash = DefaultHasher::new();
    hash.write(address_group(ip).as_bytes());
    format!("rl:{kind}:{}", hash.finish())
}

/// Seconds an address stays blocked, if it is
pub async fn address_blocked(ip: &str) -> Option<u64> {
    if *IS_TEST_ENV {
        return None;
    }
    read_block(ip).await
}

/// The block lookup itself, without the test-environment shortcut (tests use it)
pub async fn read_block(ip: &str) -> Option<u64> {
    let mut conn = get_connection().await.ok()?.into_inner();
    let ttl: i64 = redis_kiss::redis::cmd("TTL")
        .arg(address_key("block", ip))
        .query_async(&mut conn)
        .await
        .ok()?;
    (ttl > 0).then_some(ttl as u64)
}

/// Count a refused request from an address; true when it is now blocked
pub async fn count_refused(ip: &str) -> bool {
    if *IS_TEST_ENV {
        return false;
    }
    count_refused_with_limit(ip, BAD_REQUESTS_LIMIT).await
}

/// [`count_refused`] with a chosen limit (tests use a small one)
pub async fn count_refused_with_limit(ip: &str, limit: u32) -> bool {
    let Ok(conn) = get_connection().await else {
        return false;
    };
    let mut conn = conn.into_inner();
    let counter = address_key("refused", ip);
    let Ok(count) = redis_kiss::redis::cmd("INCR")
        .arg(&counter)
        .query_async::<_, u32>(&mut conn)
        .await
    else {
        return false;
    };
    // the ten minutes start with the first refused request
    if count == 1 {
        let _: Result<(), _> = redis_kiss::redis::cmd("EXPIRE")
            .arg(&counter)
            .arg(BAD_REQUESTS_WINDOW)
            .query_async(&mut conn)
            .await;
    }
    if count >= limit {
        let _: Result<(), _> = redis_kiss::redis::cmd("SET")
            .arg(address_key("block", ip))
            .arg(1)
            .arg("EX")
            .arg(BAD_REQUESTS_WINDOW)
            .query_async(&mut conn)
            .await;
        return true;
    }
    false
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum RatelimitInformation {
    Success(Ratelimiter),
    /// No `type` on purpose: clients tell a plain rate limit, which they wait
    /// out, from a typed 429 such as `InSlowmode`, which they show. Released
    /// SDKs depend on it.
    Failure {
        /// Milliseconds until the bucket refills
        retry_after: u128,
        /// The bot's limit across all routes was hit, not this route's:
        /// pause every request, not only this one
        global: bool,
    },
}

/// A request path fit for a log: webhook and interaction paths carry a
/// secret in their third segment
pub fn path_for_logs(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    if matches!(parts.get(1), Some(&"webhooks" | &"interactions")) {
        if let Some(secret) = parts.get_mut(3) {
            if *secret != "token" {
                *secret = "***";
            }
        }
    }
    parts.join("/")
}

/// Response headers browsers must be allowed to read (CORS `expose_headers`).
/// Every header the middleware sets belongs here, or browser clients cannot back off.
pub const EXPOSED_HEADERS: &[&str] = &[
    "X-RateLimit-Limit",
    "X-RateLimit-Bucket",
    "X-RateLimit-Remaining",
    "X-RateLimit-Reset-After",
    "X-RateLimit-Global",
    "Retry-After",
];

/// Value of the standard `Retry-After` header: whole seconds, rounded up, at least 1.
/// (`X-RateLimit-Reset-After` and the `retry_after` body field stay in milliseconds.)
pub fn retry_after_seconds(reset_ms: u128) -> u64 {
    (reset_ms.div_ceil(1000) as u64).max(1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_ipv6_slash_64_counts_as_one_address() {
        use super::{address_group, identifier, identifier_for_bucket};
        let a = "2001:db8:1:2:aaaa::1";
        let b = "2001:db8:1:2:ffff:ffff:ffff:ffff";
        let other = "2001:db8:1:3::1";
        assert_eq!(address_group(a), address_group(b));
        assert_ne!(address_group(a), address_group(other));
        assert_eq!(address_group(a), "2001:db8:1:2::/64");
        // the sign-in buckets and anonymous requests both use the group
        assert_eq!(
            identifier_for_bucket("auth", None, None, a),
            identifier_for_bucket("auth", None, None, b)
        );
        assert_eq!(identifier(None, None, a), identifier(None, None, b));
        // a session is still counted by itself, not by its address
        assert_eq!(identifier(None, Some("session"), a), "session");
    }

    #[test]
    fn ipv4_addresses_stay_single() {
        use super::address_group;
        assert_eq!(address_group("198.51.100.7"), "198.51.100.7");
        assert_ne!(address_group("198.51.100.7"), address_group("198.51.100.8"));
        assert_eq!(address_group("::ffff:198.51.100.7"), "198.51.100.7");
        assert_eq!(address_group("not an address"), "not an address");
    }

    use super::*;

    #[test]
    fn retry_after_header_is_whole_seconds_rounded_up() {
        assert_eq!(retry_after_seconds(0), 1);
        assert_eq!(retry_after_seconds(1), 1);
        assert_eq!(retry_after_seconds(1000), 1);
        assert_eq!(retry_after_seconds(1001), 2);
        assert_eq!(retry_after_seconds(59_500), 60);
    }

    #[test]
    fn logs_never_get_a_webhook_token() {
        assert_eq!(path_for_logs("/webhooks/ID/zrzur_wh_secret"), "/webhooks/ID/***");
        assert_eq!(
            path_for_logs("/webhooks/ID/zrzur_wh_secret/messages/M"),
            "/webhooks/ID/***/messages/M"
        );
        assert_eq!(path_for_logs("/webhooks/ID"), "/webhooks/ID");
        assert_eq!(path_for_logs("/channels/ID/messages"), "/channels/ID/messages");
    }

    #[test]
    fn cors_exposes_every_header_the_middleware_sets() {
        for header in ["X-RateLimit-Global", "Retry-After", "X-RateLimit-Reset-After"] {
            assert!(EXPOSED_HEADERS.iter().any(|h| h.eq_ignore_ascii_case(header)), "{header}");
        }
    }

    /// A name no earlier run used, so counters start empty
    fn fresh(name: &str) -> String {
        format!("{name}-{}", now().as_nanos())
    }

    #[test]
    fn being_rate_limited_does_not_count_towards_a_block() {
        assert!(!counts_as_refused(429));
        assert!(counts_as_refused(401));
        assert!(counts_as_refused(403));
    }

    #[test]
    fn auth_routes_ignore_the_session() {
        let ip = "203.0.113.7";
        for bucket in ["auth", "auth_delete", "staff_auth"] {
            assert_eq!(
                identifier_for_bucket(bucket, None, Some("session-a"), ip),
                identifier_for_bucket(bucket, None, Some("session-b"), ip),
                "{bucket}"
            );
            assert_eq!(
                identifier_for_bucket(bucket, Some("BOT"), Some("session-a"), ip),
                format!("ip:{ip}")
            );
        }
        assert_eq!(identifier_for_bucket("channels", None, Some("session-a"), ip), "session-a");
    }

    #[test]
    fn bots_are_counted_by_id_not_by_address() {
        let ip = "203.0.113.7";
        assert_ne!(identifier(Some("BOT_A"), None, ip), identifier(Some("BOT_B"), None, ip));
        assert_eq!(
            identifier(Some("BOT_A"), None, ip),
            identifier(Some("BOT_A"), None, "198.51.100.9")
        );
        assert_eq!(identifier(None, Some("session"), ip), "session");
        assert_eq!(identifier(None, None, ip), ip);
    }

    #[tokio::test]
    async fn two_bots_on_one_address_have_separate_limits() {
        // a fresh address per run: refusals of one address stay blocked for ten minutes
        let ip_name = fresh("203.0.113.7");
        let ip = ip_name.as_str();
        let (a, b) = (fresh("bot-a"), fresh("bot-b"));
        let bucket = ("messaging", Some("channel"));
        for _ in 0..10 {
            assert!(count_in_bucket(&identifier(Some(&a), None, ip), 10, bucket).await.is_ok());
        }
        assert!(count_in_bucket(&identifier(Some(&a), None, ip), 10, bucket).await.is_err());
        // the other bot on the same host is not affected
        assert!(count_in_bucket(&identifier(Some(&b), None, ip), 10, bucket).await.is_ok());
    }

    #[tokio::test]
    async fn one_bot_from_two_addresses_shares_one_limit() {
        let bot = fresh("bot");
        let ip_name = fresh("203.0.113.7");
        let other_name = fresh("198.51.100.9");
        let bucket = ("messaging", Some("channel"));
        for _ in 0..10 {
            let id = identifier(Some(&bot), None, &ip_name);
            assert!(count_in_bucket(&id, 10, bucket).await.is_ok());
        }
        let elsewhere = identifier(Some(&bot), None, &other_name);
        assert!(count_in_bucket(&elsewhere, 10, bucket).await.is_err());
    }

    /// Counts against the real store even under TEST_DB, where `from` only passes
    async fn count_in_bucket(
        identifier: &str,
        limit: u32,
        bucket: (&str, Option<&str>),
    ) -> Result<Ratelimiter, Ratelimiter> {
        Ratelimiter::count(bucket_key(identifier, bucket), limit, Duration::from_secs(10)).await
    }

    #[tokio::test]
    async fn the_51st_request_in_a_second_hits_the_global_limit() {
        let bot = fresh("bot");
        for n in 0..BOT_GLOBAL_LIMIT {
            assert!(Ratelimiter::count_global(&bot).await.is_ok(), "request {n}");
        }
        let refused = Ratelimiter::count_global(&bot).await.expect_err("51st request");
        // the response says it was the global limit (X-RateLimit-Global)
        assert!(refused.global);
        assert_eq!(refused.remaining, 0);
        assert!(refused.reset <= 1000);
    }

    #[tokio::test]
    async fn an_address_with_too_many_refused_requests_is_blocked() {
        let ip = fresh("203.0.113.66");
        assert_eq!(read_block(&ip).await, None);
        for _ in 0..4 {
            assert!(!count_refused_with_limit(&ip, 5).await);
        }
        assert!(count_refused_with_limit(&ip, 5).await);
        let left = read_block(&ip).await.expect("blocked");
        assert!(left > 590 && left <= 600, "{left}");
    }
}
