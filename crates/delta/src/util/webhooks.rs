//! What every call with a webhook token goes through: a throttle on wrong
//! tokens per address, and the limits per webhook and per channel.
//!
//! Limits live in the route, not in the rate limit fairing: the fairing runs
//! before the webhook is known, and a limit per webhook must hold from any
//! address.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::Mutex;
use std::time::Duration;

use redis_kiss::{get_connection, redis};
use revolt_database::util::{ip::rocket::to_real_ip, reference::Reference};
use revolt_database::{Database, Webhook};
use revolt_ratelimits::ratelimiter::{address_group, retry_after_seconds, Ratelimiter};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::{
    r#gen::OpenApiGenerator,
    request::{OpenApiFromRequest, RequestHeaderInput},
};
use rocket::{
    fairing::{Fairing, Info, Kind},
    request::{FromRequest, Outcome},
    Request, Response,
};

/// Messages one webhook may send in [`WEBHOOK_WINDOW`]
pub const WEBHOOK_LIMIT: u32 = 5;
pub const WEBHOOK_WINDOW: Duration = Duration::from_secs(2);

/// Messages all webhooks of a channel may send together in [`CHANNEL_WINDOW`]
pub const CHANNEL_LIMIT: u32 = 30;
pub const CHANNEL_WINDOW: Duration = Duration::from_secs(60);

/// Wrong tokens or webhook ids one address may try in a minute
pub const BAD_TOKEN_LIMIT: u32 = 20;
const BAD_TOKEN_WINDOW_SECONDS: usize = 60;

/// The limit a request was counted against, for its response headers
#[derive(Clone, Copy)]
struct Counted {
    limit: u32,
    remaining: u32,
    reset: u128,
    bucket: u64,
    refused: bool,
}

#[derive(Default)]
struct CountedSlot(Mutex<Option<Counted>>);

/// Adds the `X-RateLimit-*` headers of the webhook limits. Attached after the
/// general rate limit fairing, so these win over its.
pub struct WebhookLimitHeaders;

#[rocket::async_trait]
impl Fairing for WebhookLimitHeaders {
    fn info(&self) -> Info {
        Info {
            name: "Webhook limit headers",
            kind: Kind::Response,
        }
    }

    async fn on_response<'r>(&self, request: &'r Request<'_>, response: &mut Response<'r>) {
        let slot = request.local_cache(CountedSlot::default);
        let Some(counted) = slot.0.lock().ok().and_then(|counted| *counted) else {
            return;
        };
        response.set_raw_header("X-RateLimit-Limit", counted.limit.to_string());
        response.set_raw_header("X-RateLimit-Bucket", counted.bucket.to_string());
        response.set_raw_header("X-RateLimit-Remaining", counted.remaining.to_string());
        response.set_raw_header("X-RateLimit-Reset-After", counted.reset.to_string());
        if counted.refused {
            response.set_raw_header("Retry-After", retry_after_seconds(counted.reset).to_string());
        }
    }
}

fn key(kind: &str, id: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(kind.as_bytes());
    hash.write(id.as_bytes());
    hash.finish()
}

fn bad_token_key(ip: &str) -> String {
    format!("rl:webhook-token:{}", key("address", &address_group(ip)))
}

/// Milliseconds an address must wait, when it guessed too often
async fn bad_token_wait(ip: &str) -> Option<u128> {
    if ip.is_empty() {
        return None;
    }
    let mut conn = get_connection().await.ok()?.into_inner();
    let key = bad_token_key(ip);
    let count: Option<u32> = redis::cmd("GET").arg(&key).query_async(&mut conn).await.ok()?;
    if count? < BAD_TOKEN_LIMIT {
        return None;
    }
    let left: i64 = redis::cmd("PTTL").arg(&key).query_async(&mut conn).await.ok()?;
    Some(left.max(1) as u128)
}

async fn count_bad_token(ip: &str) {
    if ip.is_empty() {
        return;
    }
    let Ok(conn) = get_connection().await else {
        return;
    };
    let mut conn = conn.into_inner();
    let key = bad_token_key(ip);
    let Ok(count) = redis::cmd("INCR").arg(&key).query_async::<_, u32>(&mut conn).await else {
        return;
    };
    // the minute starts with the first wrong token
    if count == 1 {
        let _: std::result::Result<(), _> = redis::cmd("EXPIRE")
            .arg(&key)
            .arg(BAD_TOKEN_WINDOW_SECONDS)
            .query_async(&mut conn)
            .await;
    }
}

fn rate_limited(retry_after: u128) -> revolt_result::Error {
    create_error!(RateLimited {
        retry_after: retry_after.try_into().unwrap_or(u64::MAX),
        global: false,
    })
}

/// A call with a webhook token
pub struct WebhookCaller<'r> {
    ip: String,
    counted: &'r CountedSlot,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for WebhookCaller<'r> {
    type Error = std::convert::Infallible;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(WebhookCaller {
            ip: to_real_ip(request).await,
            counted: request.local_cache(CountedSlot::default),
        })
    }
}

impl<'r> OpenApiFromRequest<'r> for WebhookCaller<'r> {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<RequestHeaderInput> {
        Ok(RequestHeaderInput::None)
    }
}

impl WebhookCaller<'_> {
    fn remember(&self, counted: Counted) {
        if let Ok(mut slot) = self.counted.0.lock() {
            *slot = Some(counted);
        }
    }

    /// The webhook this id and token belong to. After too many wrong guesses
    /// from one address, even a right token waits.
    pub async fn authorise(
        &self,
        db: &Database,
        webhook_id: Reference<'_>,
        token: &str,
    ) -> Result<Webhook> {
        if let Some(wait) = bad_token_wait(&self.ip).await {
            self.remember(Counted {
                limit: BAD_TOKEN_LIMIT,
                remaining: 0,
                reset: wait,
                bucket: key("address", &address_group(&self.ip)),
                refused: true,
            });
            return Err(rate_limited(wait));
        }

        let webhook = match webhook_id.as_webhook(db).await {
            Ok(webhook) => webhook,
            Err(error) => {
                count_bad_token(&self.ip).await;
                return Err(error);
            }
        };
        if let Err(error) = webhook.assert_token(token) {
            count_bad_token(&self.ip).await;
            return Err(error);
        }
        Ok(webhook)
    }

    /// Count one message against the webhook and its channel
    pub async fn count_message(&self, webhook: &Webhook) -> Result<()> {
        let own = Ratelimiter::count(key("webhook", &webhook.id), WEBHOOK_LIMIT, WEBHOOK_WINDOW).await;
        let own = own.map_err(|refused| self.refuse(refused))?;
        let shared =
            Ratelimiter::count(key("channel", &webhook.channel_id), CHANNEL_LIMIT, CHANNEL_WINDOW).await;
        shared.map_err(|refused| self.refuse(refused))?;
        self.remember(Counted {
            limit: own.limit,
            remaining: own.remaining,
            reset: own.reset,
            bucket: own.key,
            refused: false,
        });
        Ok(())
    }

    fn refuse(&self, refused: Ratelimiter) -> revolt_result::Error {
        self.remember(Counted {
            limit: refused.limit,
            remaining: 0,
            reset: refused.reset,
            bucket: refused.key,
            refused: true,
        });
        rate_limited(refused.reset)
    }
}

/// Whether an override avatar is a web address we are willing to hand to
/// clients. Nothing here fetches it: clients load it through the media
/// proxy, which resolves the name once and checks the address and the file.
/// This refuses at the door what the proxy would refuse later.
pub fn avatar_url_problem(address: &str) -> Option<&'static str> {
    const NOT_A_WEB_ADDRESS: &str = "avatar_url must be a public http or https address";
    if address.is_empty() || address.len() > 2048 {
        return Some("avatar_url must be 1 to 2048 characters");
    }
    let Ok(url) = url::Url::parse(address) else {
        return Some(NOT_A_WEB_ADDRESS);
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Some(NOT_A_WEB_ADDRESS);
    }
    let private = match url.host() {
        None => true,
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == "localhost"
                || host.ends_with(".localhost")
                || host.ends_with(".local")
                || host.ends_with(".internal")
                || !host.contains('.')
        }
        Some(url::Host::Ipv4(ip)) => {
            let [a, b, ..] = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
        }
        Some(url::Host::Ipv6(ip)) => {
            let first = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.to_ipv4_mapped().is_some()
                || first & 0xFE00 == 0xFC00
                || first & 0xFFC0 == 0xFE80
        }
    };
    private.then_some(NOT_A_WEB_ADDRESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_addresses() {
        for bad in [
            "",
            "http://127.0.0.1/a.png",
            "http://127.1/a.png",
            "http://2130706433/a.png",
            "http://0x7f.0.0.1/a.png",
            "https://169.254.169.254/latest/meta-data/",
            "https://10.0.0.1/a",
            "https://172.16.0.1/a",
            "https://192.168.1.1/a",
            "https://100.64.0.1/a",
            "https://0.0.0.0/a",
            "https://[::1]/a",
            "https://[::ffff:127.0.0.1]/a",
            "https://[fe80::1]/a",
            "https://[fc00::1]/a",
            "https://localhost/a",
            "https://LOCALHOST./a",
            "https://metadata.google.internal/a",
            "https://printer.local/a",
            "https://intranet/a",
            "file:///etc/passwd",
            "data:image/png;base64,AAAA",
            "javascript:alert(1)",
            "ftp://example.com/a.png",
            "//example.com/a.png",
            "https://user:pass@example.com/a.png",
        ] {
            assert!(avatar_url_problem(bad).is_some(), "{bad}");
        }
        assert!(avatar_url_problem(&format!("https://e.com/{}", "a".repeat(2049))).is_some());
        for good in [
            "https://example.com/a.png",
            "http://example.com/a.png",
            "https://avatars.githubusercontent.com/u/1?v=4",
            "https://8.8.8.8/a.png",
            "https://[2606:4700::1111]/a.png",
        ] {
            assert_eq!(avatar_url_problem(good), None, "{good}");
        }
    }
}
