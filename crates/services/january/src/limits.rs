//! What one caller can cost this service: how much of a remote file is read,
//! how many downloads and image decodes run at once, how many requests one
//! address may make, and who may ask for embeds.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use axum::{
    extract::{ConnectInfo, Request as HttpRequest},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
use moka::future::Cache;
use revolt_config::config;
use revolt_result::{Result, create_error};
use sha2::Sha256;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};

/// Most bytes read from any remote response
pub const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;
/// Remote responses being read at once. With [`MAX_BODY_BYTES`] this bounds
/// the memory held by downloads (400 MiB).
pub const MAX_FETCHES: usize = 16;
/// Images being decoded at once
pub const MAX_DECODES: usize = 4;
/// How long a request waits for its turn before giving up
pub const QUEUE_WAIT: Duration = Duration::from_secs(5);
/// Requests one address may make per [`ADDRESS_WINDOW`]. A chat view with a
/// lot of images is about 50, and people behind one router share an address.
pub const ADDRESS_LIMIT: u32 = 200;
pub const ADDRESS_WINDOW: Duration = Duration::from_secs(10);

/// Header carrying `january.embed_key` on calls from the API workers
pub const EMBED_KEY_HEADER: &str = "x-january-key";

pub static FETCHES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_FETCHES)));
pub static DECODES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_DECODES)));
static ADDRESSES: LazyLock<AddressLimiter> =
    LazyLock::new(|| AddressLimiter::new(ADDRESS_LIMIT, ADDRESS_WINDOW));

/// Wait for a place at `gate`. A full queue answers `ProxyError` instead of
/// piling up more work.
pub async fn wait_for(gate: &Arc<Semaphore>, wait: Duration) -> Result<OwnedSemaphorePermit> {
    timeout(wait, gate.clone().acquire_owned())
        .await
        .map_err(|_| create_error!(ProxyError))?
        .map_err(|_| create_error!(ProxyError))
}

/// Read a remote body, stopping as soon as it passes `cap` bytes. A declared
/// length over the cap is refused before anything is read; the length is only
/// a hint, so the running total is what actually enforces the cap.
pub async fn read_capped(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>> {
    let declared = response.content_length().unwrap_or(0);
    if declared > cap as u64 {
        return Err(create_error!(FileTooLarge { max: cap }));
    }

    let mut body = Vec::with_capacity(declared as usize);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| create_error!(ProxyError))?
    {
        if body.len() + chunk.len() > cap {
            return Err(create_error!(FileTooLarge { max: cap }));
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

/// Fixed-window counter per address, kept in memory (one January process)
pub struct AddressLimiter {
    limit: u32,
    windows: Cache<IpAddr, Arc<AtomicU32>>,
}

impl AddressLimiter {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            windows: Cache::builder()
                .max_capacity(100_000)
                .time_to_live(window)
                .build(),
        }
    }

    /// Count one request; false once the address is over its limit
    pub async fn allow(&self, address: IpAddr) -> bool {
        let count = self
            .windows
            .get_with(limit_key(address), async { Arc::new(AtomicU32::new(0)) })
            .await;
        count.fetch_add(1, Ordering::Relaxed) < self.limit
    }
}

/// What is counted: the address itself, or the /64 for IPv6, since one
/// connection is given a whole /64 to rotate through
fn limit_key(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let [a, b, c, d, ..] = v6.segments();
                IpAddr::V6(std::net::Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
            }
        },
    }
}

/// A connection from a private address is the reverse proxy on our own
/// network, never a person on the internet
fn is_our_proxy(peer: IpAddr) -> bool {
    match peer {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.is_private() || v4.is_loopback(),
            None => {
                v6.is_loopback()
                    || matches!(v6.segments()[0] & 0xfe00, 0xfc00)
                    || matches!(v6.segments()[0] & 0xffc0, 0xfe80)
            }
        },
    }
}

/// The address a request is counted against. Forwarding headers are believed
/// only from our own proxy, otherwise anyone could pick their own address;
/// without them everybody behind the proxy would share one limit. The last
/// `X-Forwarded-For` entry is the one our proxy wrote.
pub fn client_address(peer: IpAddr, headers: &HeaderMap, trust_cloudflare: bool) -> IpAddr {
    let parse = |value: Option<&str>| value.and_then(|v| v.trim().parse::<IpAddr>().ok());

    if trust_cloudflare {
        let cloudflare = headers
            .get("CF-Connecting-IP")
            .and_then(|value| value.to_str().ok());
        if let Some(address) = parse(cloudflare) {
            return address;
        }
    }

    if is_our_proxy(peer) {
        let forwarded = headers
            .get("X-Forwarded-For")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit(',').next());
        if let Some(address) = parse(forwarded) {
            return address;
        }
    }

    peer
}

/// Refuse an address that is over its limit
pub async fn per_address(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: HttpRequest,
    next: Next,
) -> Response {
    let trust_cloudflare = config().await.api.security.trust_cloudflare;
    let address = client_address(peer.ip(), request.headers(), trust_cloudflare);

    if ADDRESSES.allow(address).await {
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, ADDRESS_WINDOW.as_secs().to_string())],
        )
            .into_response()
    }
}

/// Compare without stopping at the first different byte
fn same_key(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// Embeds are made for the API workers, not for browsers. With no key
/// configured the route stays open, as it was.
pub fn embed_key_is_valid(configured: &str, headers: &HeaderMap) -> bool {
    configured.is_empty()
        || headers
            .get(EMBED_KEY_HEADER)
            .is_some_and(|given| same_key(configured.as_bytes(), given.as_bytes()))
}

/// Refuse an embed request without the key
pub async fn embed_key(request: HttpRequest, next: Next) -> Response {
    if embed_key_is_valid(&config().await.january.embed_key, request.headers()) {
        next.run(request).await
    } else {
        create_error!(NotAuthenticated).into_response()
    }
}

/// Unix time in seconds
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// Check a ticket from `POST /auth/session/proxy_ticket`:
/// `<expires>.<user id>.<hex HMAC-SHA256 of "<expires>.<user id>">`. With no
/// key configured nothing is valid: an empty key would let anyone sign.
pub fn proxy_ticket_is_valid(key: &str, ticket: Option<&str>, now: i64) -> bool {
    let (Some(ticket), false) = (ticket, key.is_empty()) else {
        return false;
    };
    let Some((payload, signature)) = ticket.rsplit_once('.') else {
        return false;
    };
    let Some(expires) = payload
        .split_once('.')
        .and_then(|(expires, _)| expires.parse::<i64>().ok())
    else {
        return false;
    };
    let Ok(signature) = hex::decode(signature) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key.as_bytes()) else {
        return false;
    };
    mac.update(payload.as_bytes());
    // the signature first, in constant time; only then trust what it covers
    mac.verify_slice(&signature).is_ok() && expires > now
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, middleware::from_fn, routing::get};
    use revolt_result::ErrorType;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    /// Answer every connection with `head`, then send zero bytes in steps of
    /// `step` for as long as the client keeps reading; records bytes sent
    async fn endless_server(head: &'static str, step: usize) -> (String, Arc<AtomicU32>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let sent = Arc::new(AtomicU32::new(0));
        let counter = sent.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 1024];
                    let _ = socket.read(&mut request).await;
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let chunk = vec![0u8; step];
                    while socket.write_all(&chunk).await.is_ok() {
                        counter.fetch_add(step as u32, Ordering::Relaxed);
                    }
                });
            }
        });
        (url, sent)
    }

    fn too_large(error: revolt_result::Error) -> bool {
        matches!(error.error_type, ErrorType::FileTooLarge { .. })
    }

    #[tokio::test]
    async fn a_body_without_a_length_stops_at_the_cap() {
        let (url, sent) = endless_server("HTTP/1.1 200 OK\r\n\r\n", 64 * 1024).await;
        let response = reqwest::get(url).await.unwrap();

        let error = read_capped(response, 1024 * 1024)
            .await
            .expect_err("over the cap");
        assert!(too_large(error));
        // the read stopped: the sender is not still being drained
        assert!(sent.load(Ordering::Relaxed) < 64 * 1024 * 1024);
    }

    #[tokio::test]
    async fn a_lying_length_cannot_pass_the_cap() {
        // a declared length under the cap with more bytes behind it is cut by
        // the HTTP client at that length; never more than the cap is returned
        let (url, _) =
            endless_server("HTTP/1.1 200 OK\r\nContent-Length: 2048\r\n\r\n", 4096).await;
        let response = reqwest::get(url).await.unwrap();
        let body = read_capped(response, 4096).await.expect("within the cap");
        assert_eq!(body.len(), 2048);
    }

    #[tokio::test]
    async fn a_declared_length_over_the_cap_reads_nothing() {
        let (url, _) =
            endless_server("HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n", 1024).await;
        let response = reqwest::get(url).await.unwrap();
        assert!(too_large(
            read_capped(response, MAX_BODY_BYTES).await.unwrap_err()
        ));
    }

    #[tokio::test]
    async fn a_full_gate_refuses_instead_of_queueing_forever() {
        let gate = Arc::new(Semaphore::new(1));
        let held = wait_for(&gate, QUEUE_WAIT).await.expect("first place");
        assert!(wait_for(&gate, Duration::from_millis(20)).await.is_err());
        drop(held);
        assert!(wait_for(&gate, Duration::from_millis(20)).await.is_ok());
    }

    #[tokio::test]
    async fn an_address_is_limited_and_another_is_not() {
        let limiter = AddressLimiter::new(3, Duration::from_secs(60));
        let (a, b): (IpAddr, IpAddr) = (
            "203.0.113.1".parse().unwrap(),
            "203.0.113.2".parse().unwrap(),
        );
        for _ in 0..3 {
            assert!(limiter.allow(a).await);
        }
        assert!(!limiter.allow(a).await);
        assert!(limiter.allow(b).await);
    }

    #[tokio::test]
    async fn the_limit_is_given_back_after_the_window() {
        let limiter = AddressLimiter::new(1, Duration::from_millis(50));
        let a: IpAddr = "203.0.113.3".parse().unwrap();
        assert!(limiter.allow(a).await);
        assert!(!limiter.allow(a).await);
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(limiter.allow(a).await);
    }

    #[tokio::test]
    async fn rotating_inside_an_ipv6_block_does_not_help() {
        let limiter = AddressLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.allow("2606:4700:1:2::1".parse().unwrap()).await);
        assert!(
            !limiter
                .allow("2606:4700:1:2:ffff::9".parse().unwrap())
                .await
        );
        assert!(limiter.allow("2606:4700:1:3::1".parse().unwrap()).await);
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn a_stranger_cannot_choose_their_address() {
        let peer: IpAddr = "8.8.8.8".parse().unwrap();
        let spoofed = headers(&[
            ("x-forwarded-for", "1.2.3.4"),
            ("cf-connecting-ip", "1.2.3.4"),
        ]);
        assert_eq!(client_address(peer, &spoofed, false), peer);
    }

    #[test]
    fn behind_our_proxy_each_person_has_their_own_address() {
        let proxy: IpAddr = "172.20.0.5".parse().unwrap();
        let forwarded = headers(&[("x-forwarded-for", "9.9.9.9, 1.1.1.1")]);
        // the last entry is the one our proxy wrote
        assert_eq!(
            client_address(proxy, &forwarded, false),
            "1.1.1.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(client_address(proxy, &HeaderMap::new(), false), proxy);
        assert_eq!(
            client_address(proxy, &headers(&[("x-forwarded-for", "nonsense")]), false),
            proxy
        );
    }

    #[test]
    fn cloudflare_is_believed_only_when_configured() {
        let peer: IpAddr = "8.8.8.8".parse().unwrap();
        let cf = headers(&[("cf-connecting-ip", "1.1.1.1")]);
        assert_eq!(
            client_address(peer, &cf, true),
            "1.1.1.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(client_address(peer, &cf, false), peer);
    }

    #[test]
    fn embeds_need_the_key_once_one_is_set() {
        let right = headers(&[(EMBED_KEY_HEADER, "s3cret")]);
        let wrong = headers(&[(EMBED_KEY_HEADER, "s3cres")]);
        assert!(embed_key_is_valid("", &HeaderMap::new()));
        assert!(embed_key_is_valid("s3cret", &right));
        assert!(!embed_key_is_valid("s3cret", &wrong));
        assert!(!embed_key_is_valid("s3cret", &HeaderMap::new()));
    }

    #[tokio::test]
    async fn the_route_answers_429_over_the_limit() {
        async fn ok() -> &'static str {
            "ok"
        }

        let app = Router::new()
            .route("/proxy", get(ok))
            .layer(from_fn(per_address));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/proxy", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let client = reqwest::Client::new();
        for n in 0..ADDRESS_LIMIT {
            let response = client.get(&url).send().await.unwrap();
            assert_eq!(response.status().as_u16(), 200, "request {n}");
        }
        let refused = client.get(&url).send().await.unwrap();
        assert_eq!(refused.status().as_u16(), 429);
        assert_eq!(refused.headers()["retry-after"], "10");
    }

    /// Signed by the API's `sign_proxy_ticket`: the same value is in its tests
    const KEY: &str = "test-proxy-key";
    const TICKET: &str = "1893456000.01ABCDEFGHJKMNPQRSTVWXYZ01.2907d453dff601dc82f3401977444db4a2570d5c0d2a4215f4cab2ad17ad8dc5";
    const BEFORE: i64 = 1893456000 - 60;

    #[test]
    fn a_ticket_from_the_api_is_accepted() {
        assert!(proxy_ticket_is_valid(KEY, Some(TICKET), BEFORE));
    }

    #[test]
    fn a_ticket_stops_working_when_it_expires() {
        assert!(!proxy_ticket_is_valid(KEY, Some(TICKET), 1893456000));
    }

    #[test]
    fn a_changed_ticket_is_refused() {
        let later = TICKET.replacen("1893456000", "1893456999", 1);
        let other_user = TICKET.replacen("01ABC", "01ABD", 1);
        let cut = &TICKET[..TICKET.len() - 2];
        for ticket in [
            later.as_str(),
            other_user.as_str(),
            cut,
            "",
            ".",
            "1.2.3",
            "a.b.zz",
        ] {
            assert!(
                !proxy_ticket_is_valid(KEY, Some(ticket), BEFORE),
                "{ticket}"
            );
        }
    }

    #[test]
    fn no_ticket_or_no_key_is_refused() {
        assert!(!proxy_ticket_is_valid(KEY, None, BEFORE));
        assert!(!proxy_ticket_is_valid("other-key", Some(TICKET), BEFORE));
        assert!(!proxy_ticket_is_valid("", Some(TICKET), BEFORE));
    }
}
