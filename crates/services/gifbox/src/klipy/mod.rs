//! KLIPY GIF client (server side only).
//!
//! The browser never talks to KLIPY: it asks gifbox, gifbox asks KLIPY with
//! the API key kept here, and the media bytes come back through gifbox too.
//!
//! Privacy rules, enforced by tests in this module:
//! - no per-user identifier is ever sent to KLIPY. KLIPY wants a
//!   `customer_id`, so one random id is made per server process and shared by
//!   every request. It is not derived from a user, session or IP address.
//! - no share/registration call is made.
//! - the API key never appears in a response or an error message.
//! - media is served only through signed, expiring links that gifbox itself
//!   put into a logged-in user's search results (not an open proxy).

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::body::Bytes;
use rand::RngCore;
use reqwest::{redirect::Policy, Client, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

const DEFAULT_BASE: &str = "https://api.klipy.com/api/v1";

/// The only host media is fetched from.
pub const MEDIA_HOST: &str = "static.klipy.com";

/// Largest media file passed on (bytes).
pub const MAX_MEDIA_BYTES: usize = 16 * 1024 * 1024;

/// How long a signed media link works (seconds).
pub const LINK_LIFETIME: u64 = 60 * 60;

/// Shortest accepted `GIFBOX_MEDIA_SECRET`.
pub const MIN_SECRET_LEN: usize = 16;

/// Media files fetched from KLIPY at the same time.
pub const MAX_CONCURRENT_MEDIA: usize = 16;

/// Content types passed on from the media host.
pub const ALLOWED_MEDIA_TYPES: [&str; 5] = [
    "image/gif",
    "image/webp",
    "image/jpeg",
    "image/png",
    "video/mp4",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KlipyError {
    /// No API key configured
    Unavailable,
    /// Bad input from the caller
    Invalid,
    /// KLIPY did not answer properly
    Upstream,
    /// All media slots are taken
    Busy,
}

fn random_u64() -> u64 {
    // RandomState keys are seeded randomly by the operating system
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u8(0);
    hasher.finish()
}

/// One random id per server process (not per user).
fn process_customer_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let mut id = String::with_capacity(32);
        for _ in 0..2 {
            id.push_str(&format!("{:016x}", random_u64()));
        }
        id
    })
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Where media may come from. Always https://static.klipy.com, except in tests.
#[derive(Clone, Debug)]
pub struct MediaRule {
    scheme: &'static str,
    host: String,
    port: Option<u16>,
}

impl Default for MediaRule {
    fn default() -> Self {
        Self {
            scheme: "https",
            host: MEDIA_HOST.to_string(),
            port: None,
        }
    }
}

#[derive(Clone)]
pub struct Klipy {
    media_rule: MediaRule,
    key: Option<String>,
    base: String,
    client: Client,
    /// Secret for signed media links (`GIFBOX_MEDIA_SECRET`; random per process if unset)
    secret: Arc<Vec<u8>>,
    media_slots: Arc<Semaphore>,
}

impl Klipy {
    /// Key from `GIFBOX_KLIPY_KEY`, optional base URL from `GIFBOX_KLIPY_BASE_URL`,
    /// link-signing secret from `GIFBOX_MEDIA_SECRET`. In production the KLIPY
    /// routes stay off unless a valid secret is configured.
    pub fn from_env(production: bool) -> Self {
        let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self::configure(
            get("GIFBOX_KLIPY_KEY"),
            get("GIFBOX_KLIPY_BASE_URL"),
            get("GIFBOX_MEDIA_SECRET"),
            production,
        )
    }

    /// The same from plain values (so it can be tested).
    pub fn configure(
        key: Option<String>,
        base: Option<String>,
        secret: Option<String>,
        production: bool,
    ) -> Self {
        let valid = match &secret {
            Some(value) if value.len() < MIN_SECRET_LEN => {
                tracing::error!(
                    "GIFBOX_MEDIA_SECRET is too short: it needs at least {MIN_SECRET_LEN} characters"
                );
                None
            }
            Some(value) => Some(value.clone().into_bytes()),
            None => None,
        };
        let mut key = key;
        if key.is_some() && valid.is_none() {
            if production {
                tracing::error!(
                    "GIFBOX_KLIPY_KEY is set but GIFBOX_MEDIA_SECRET is missing or too short: the KLIPY routes are switched off"
                );
                key = None;
            } else {
                tracing::warn!(
                    "GIFBOX_MEDIA_SECRET is not valid: media links use a random secret that changes on restart and differs between instances (development only)"
                );
            }
        }
        Self::new_with_secret(key, base, valid)
    }

    pub fn new(key: Option<String>, base: Option<String>) -> Self {
        Self::new_with_secret(key, base, None)
    }

    pub fn new_with_secret(
        key: Option<String>,
        base: Option<String>,
        secret: Option<Vec<u8>>,
    ) -> Self {
        Self {
            key,
            base: base
                .unwrap_or_else(|| DEFAULT_BASE.to_string())
                .trim_end_matches('/')
                .to_string(),
            client: Client::builder()
                .redirect(Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
            media_rule: MediaRule::default(),
            secret: Arc::new(secret.unwrap_or_else(|| {
                // operating system randomness, 32 bytes
                let mut bytes = vec![0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                bytes
            })),
            media_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_MEDIA)),
        }
    }

    pub fn enabled(&self) -> bool {
        self.key.is_some()
    }

    /// Request URL, including the key. Never log or return this.
    pub(crate) fn url(&self, path: &str, params: &[(&str, String)]) -> Result<Url, KlipyError> {
        let key = self.key.as_ref().ok_or(KlipyError::Unavailable)?;
        let mut url = Url::parse(&format!("{}/{}/{}", self.base, key, path))
            .map_err(|_| KlipyError::Upstream)?;
        {
            let mut query = url.query_pairs_mut();
            for (name, value) in params {
                query.append_pair(name, value);
            }
        }
        Ok(url)
    }

    /// Search, or trending when the query is empty.
    pub fn gifs_url(
        &self,
        query: &str,
        page: u32,
        per_page: u32,
        locale: &str,
    ) -> Result<Url, KlipyError> {
        let query = query.trim();
        if query.chars().count() > 200 {
            return Err(KlipyError::Invalid);
        }
        let mut params = vec![
            ("page", page.clamp(1, 1000).to_string()),
            ("per_page", per_page.clamp(1, 50).to_string()),
            ("customer_id", process_customer_id().to_string()),
            ("locale", clean_locale(locale)),
            ("content_filter", "medium".to_string()),
        ];
        if query.is_empty() {
            self.url("gifs/trending", &params)
        } else {
            params.push(("q", query.to_string()));
            self.url("gifs/search", &params)
        }
    }

    pub fn categories_url(&self, locale: &str) -> Result<Url, KlipyError> {
        self.url("gifs/categories", &[("locale", clean_locale(locale))])
    }

    /// Fetch a KLIPY JSON endpoint and return `{ "result": true, "data": ... }`
    /// with a signed `/klipy/media` link next to every media URL.
    pub async fn get_json(&self, url: Url) -> Result<Value, KlipyError> {
        // the error value is dropped on purpose: its text would hold the key
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| KlipyError::Upstream)?;
        if !response.status().is_success() {
            return Err(KlipyError::Upstream);
        }
        let body: Value = response.json().await.map_err(|_| KlipyError::Upstream)?;
        if body.get("result") == Some(&Value::Bool(false)) {
            return Err(KlipyError::Upstream);
        }
        let data = body.get("data").cloned().ok_or(KlipyError::Upstream)?;
        let mut out = json!({ "result": true, "data": data });
        self.sign_media(&mut out, now());
        Ok(self.scrub(out))
    }

    /// HMAC-SHA256 over the media URL and the expiry time.
    fn signature(&self, url: &str, expires: u64) -> String {
        hmac_sha256(&self.secret, format!("{url}\n{expires}").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Relative link to `/klipy/media` for one KLIPY media URL, valid for an hour.
    pub fn signed_link(&self, url: &str, at: u64) -> String {
        let expires = at + LINK_LIFETIME;
        let mut link = Url::parse("https://x.invalid/klipy/media").expect("static url");
        link.query_pairs_mut()
            .append_pair("u", url)
            .append_pair("e", &expires.to_string())
            .append_pair("s", &self.signature(url, expires));
        format!("{}?{}", link.path(), link.query().unwrap_or(""))
    }

    /// Next to every KLIPY media URL (`url`, `preview_url`, ...) add a signed
    /// link (`src`, `preview_src`, ...). The original stays for sending.
    pub fn sign_media(&self, value: &mut Value, at: u64) {
        match value {
            Value::Object(map) => {
                let mut added = Vec::new();
                for (name, inner) in map.iter_mut() {
                    match inner {
                        Value::String(text) if self.media_rule.check(text).is_ok() => {
                            let key = match name.as_str() {
                                "url" => "src".to_string(),
                                other => format!("{}_src", other.strip_suffix("_url").unwrap_or(other)),
                            };
                            added.push((key, Value::String(self.signed_link(text, at))));
                        }
                        other => self.sign_media(other, at),
                    }
                }
                map.extend(added);
            }
            Value::Array(items) => items.iter_mut().for_each(|i| self.sign_media(i, at)),
            _ => {}
        }
    }

    /// A media request is allowed only with a valid, unexpired signature.
    pub fn verify_link(&self, url: &str, expires: u64, signature: &str, at: u64) -> bool {
        if expires < at || expires > at + LINK_LIFETIME + 60 {
            return false;
        }
        let expected = self.signature(url, expires);
        // constant-time comparison
        expected.len() == signature.len()
            && expected
                .bytes()
                .zip(signature.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }

    /// Make sure the key is not echoed back to the browser.
    fn scrub(&self, value: Value) -> Value {
        let Some(key) = &self.key else { return value };
        let text = value.to_string();
        if text.contains(key.as_str()) {
            serde_json::from_str(&text.replace(key.as_str(), "")).unwrap_or(Value::Null)
        } else {
            value
        }
    }

    /// Wait for one of the media slots; `Busy` after 5 seconds.
    async fn slot(&self) -> Result<tokio::sync::SemaphorePermit<'_>, KlipyError> {
        tokio::time::timeout(Duration::from_secs(5), self.media_slots.acquire())
            .await
            .map_err(|_| KlipyError::Busy)?
            .map_err(|_| KlipyError::Upstream)
    }

    fn content_type_of(response: &reqwest::Response) -> Result<String, KlipyError> {
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        if !ALLOWED_MEDIA_TYPES.contains(&content_type.as_str()) {
            return Err(KlipyError::Upstream);
        }
        if response
            .content_length()
            .is_some_and(|len| len as usize > MAX_MEDIA_BYTES)
        {
            return Err(KlipyError::Upstream);
        }
        Ok(content_type)
    }

    /// Content type and size of one media file, without loading it (for HEAD).
    pub async fn head_media(&self, raw: &str) -> Result<(String, Option<u64>), KlipyError> {
        let url = self.media_rule.check(raw)?;
        let _slot = self.slot().await?;
        let response = self
            .client
            .head(url)
            .send()
            .await
            .map_err(|_| KlipyError::Upstream)?;
        if !response.status().is_success() {
            return Err(KlipyError::Upstream);
        }
        Ok((Self::content_type_of(&response)?, response.content_length()))
    }

    /// Fetch one media file from the KLIPY media host.
    pub async fn get_media(&self, raw: &str) -> Result<(String, Bytes), KlipyError> {
        let url = self.media_rule.check(raw)?;
        // at most MAX_CONCURRENT_MEDIA files in flight; others wait briefly, then are told to retry
        let _slot = self.slot().await?;
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| KlipyError::Upstream)?;
        if !response.status().is_success() {
            return Err(KlipyError::Upstream);
        }
        let content_type = Self::content_type_of(&response)?;
        // read piece by piece and stop at the cap (a missing or false length header is no risk)
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| KlipyError::Upstream)? {
            if bytes.len() + chunk.len() > MAX_MEDIA_BYTES {
                return Err(KlipyError::Upstream);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((content_type, Bytes::from(bytes)))
    }

    /// Tests only: let media come from a local plain-http server.
    #[cfg(test)]
    pub fn with_local_media(mut self, host: &str, port: u16) -> Self {
        self.media_rule = MediaRule {
            scheme: "http",
            host: host.to_string(),
            port: Some(port),
        };
        self
    }
}

/// What a `Range` header asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum RangeSpec {
    /// No usable range: send the whole file (also for several ranges)
    Full,
    /// One range, first and last byte
    Partial(usize, usize),
    /// Starts after the end of the file: 416
    Unsatisfiable,
}

pub fn parse_range(header: &str, len: usize) -> RangeSpec {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeSpec::Full;
    };
    if spec.contains(',') {
        return RangeSpec::Full;
    }
    let Some((start, end)) = spec.trim().split_once('-') else {
        return RangeSpec::Full;
    };
    if start.is_empty() {
        // suffix: the last N bytes
        return match end.parse::<usize>() {
            Ok(0) => RangeSpec::Unsatisfiable,
            Ok(n) if len > 0 => RangeSpec::Partial(len - n.min(len), len - 1),
            Ok(_) => RangeSpec::Unsatisfiable,
            Err(_) => RangeSpec::Full,
        };
    }
    let Ok(start) = start.parse::<usize>() else {
        return RangeSpec::Full;
    };
    let end = if end.is_empty() {
        usize::MAX
    } else {
        match end.parse::<usize>() {
            Ok(end) => end,
            Err(_) => return RangeSpec::Full,
        }
    };
    if end < start {
        // not a valid range: ignore it
        return RangeSpec::Full;
    }
    if start >= len {
        return RangeSpec::Unsatisfiable;
    }
    RangeSpec::Partial(start, end.min(len - 1))
}

impl MediaRule {
    /// Only plain URLs on the allowed media host (not an open proxy).
    pub fn check(&self, raw: &str) -> Result<Url, KlipyError> {
        let url = Url::parse(raw).map_err(|_| KlipyError::Invalid)?;
        if url.scheme() != self.scheme
            || url.host_str() != Some(self.host.as_str())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port() != self.port
        {
            return Err(KlipyError::Invalid);
        }
        Ok(url)
    }
}

/// Only plain https URLs on the KLIPY media host (not an open proxy).
pub fn media_url(raw: &str) -> Result<Url, KlipyError> {
    MediaRule::default().check(raw)
}

fn clean_locale(locale: &str) -> String {
    let cleaned: String = locale
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(8)
        .collect();
    if cleaned.is_empty() {
        "en".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::{extract::Request, routing::any, Router};
    use tokio::net::TcpListener;

    use super::*;

    const KEY: &str = "SECRET-KEY-123";

    fn klipy() -> Klipy {
        Klipy::new(Some(KEY.into()), Some("http://127.0.0.1:1/api".into()))
    }

    fn signed() -> Klipy {
        Klipy::new_with_secret(
            Some(KEY.into()),
            None,
            Some(b"0123456789abcdef0123456789abcdef".to_vec()),
        )
    }

    #[test]
    fn requests_carry_one_process_wide_id_and_nothing_per_user() {
        let a = klipy().gifs_url("cats", 1, 24, "en").unwrap();
        let b = klipy().gifs_url("dogs", 2, 24, "de").unwrap();
        let id = |u: &Url| {
            u.query_pairs()
                .find(|(k, _)| k == "customer_id")
                .map(|(_, v)| v.to_string())
                .unwrap()
        };
        // same id for different searches: it is not derived from the user or query
        assert_eq!(id(&a), id(&b));
        assert_eq!(id(&a).len(), 32);
        assert_eq!(id(&a), process_customer_id());
        // no other identifying parameter
        for (name, _) in a.query_pairs() {
            assert!(
                [
                    "page",
                    "per_page",
                    "customer_id",
                    "locale",
                    "content_filter",
                    "q"
                ]
                .contains(&name.as_ref()),
                "unexpected parameter {name}"
            );
        }
    }

    #[test]
    fn empty_query_is_trending_and_there_is_no_share_route() {
        let trending = klipy().gifs_url("  ", 1, 24, "en").unwrap();
        assert!(trending.path().ends_with("/gifs/trending"));
        assert!(trending.query_pairs().all(|(k, _)| k != "q"));
        let search = klipy().gifs_url("hi", 1, 24, "en").unwrap();
        assert!(search.path().ends_with("/gifs/search"));
        assert!(!search.path().contains("share"));
    }

    #[test]
    fn disabled_without_key() {
        let k = Klipy::new(None, None);
        assert!(!k.enabled());
        assert_eq!(k.gifs_url("a", 1, 1, "en"), Err(KlipyError::Unavailable));
    }

    #[test]
    fn media_only_from_the_klipy_media_host() {
        assert!(media_url("https://static.klipy.com/ii/a/b.gif").is_ok());
        for bad in [
            "http://static.klipy.com/a.gif",
            "https://static.klipy.com.evil.test/a.gif",
            "https://evil.test/static.klipy.com/a.gif",
            "https://user@static.klipy.com/a.gif",
            "https://static.klipy.com:8443/a.gif",
            "https://127.0.0.1/a.gif",
            "not a url",
        ] {
            assert_eq!(media_url(bad), Err(KlipyError::Invalid), "{bad}");
        }
    }

    fn parts(link: &str) -> (String, u64, String) {
        let url = Url::parse(&format!("https://x.invalid{link}")).unwrap();
        let get = |n: &str| {
            url.query_pairs()
                .find(|(k, _)| k == n)
                .unwrap()
                .1
                .to_string()
        };
        (get("u"), get("e").parse().unwrap(), get("s"))
    }

    #[test]
    fn hmac_matches_the_rfc_4231_vector() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn media_links_are_signed_and_expire() {
        let k = signed();
        let url = "https://static.klipy.com/ii/a/b.gif";
        let link = k.signed_link(url, 1_000);
        assert!(link.starts_with("/klipy/media?"));
        let (u, e, s) = parts(&link);
        assert_eq!(u, url);
        assert!(k.verify_link(&u, e, &s, 1_000));
        assert!(k.verify_link(&u, e, &s, 1_000 + LINK_LIFETIME));
        // expired
        assert!(!k.verify_link(&u, e, &s, e + 1));
        // other file, other expiry, other signature, other secret
        assert!(!k.verify_link("https://static.klipy.com/ii/a/c.gif", e, &s, 1_000));
        assert!(!k.verify_link(&u, e + 1, &s, 1_000));
        assert!(!k.verify_link(&u, e, &"0".repeat(64), 1_000));
        assert!(!Klipy::new(Some(KEY.into()), None).verify_link(&u, e, &s, 1_000));
        // an expiry far in the future is refused too
        let far = k.signature(&u, 1_000 + 10 * LINK_LIFETIME);
        assert!(!k.verify_link(&u, 1_000 + 10 * LINK_LIFETIME, &far, 1_000));
    }

    #[test]
    fn responses_get_signed_links_and_keep_the_original_url() {
        let k = signed();
        let mut body = json!({"result": true, "data": {
            "categories": [{"category": "Hi", "preview_url": "https://static.klipy.com/p.gif"}],
            "data": [{"slug": "a", "file": {"md": {"gif": {"url": "https://static.klipy.com/m.gif", "width": 1}}}}]
        }});
        k.sign_media(&mut body, 5);
        let cat = &body["data"]["categories"][0];
        assert_eq!(cat["preview_url"], "https://static.klipy.com/p.gif");
        assert!(cat["preview_src"]
            .as_str()
            .unwrap()
            .starts_with("/klipy/media?u="));
        let gif = &body["data"]["data"][0]["file"]["md"]["gif"];
        assert_eq!(gif["url"], "https://static.klipy.com/m.gif");
        assert!(gif["src"].as_str().unwrap().contains("&s="));
        // other hosts are not signed
        let mut other = json!({"url": "https://evil.test/a.gif"});
        k.sign_media(&mut other, 5);
        assert!(other.get("src").is_none());
    }

    #[test]
    fn ranges() {
        use RangeSpec::*;
        assert_eq!(parse_range("bytes=0-9", 100), Partial(0, 9));
        assert_eq!(parse_range("bytes=10-", 100), Partial(10, 99));
        assert_eq!(parse_range("bytes=-5", 100), Partial(95, 99));
        assert_eq!(parse_range("bytes=0-999", 100), Partial(0, 99));
        // several ranges, bad syntax, other units: the whole file
        assert_eq!(parse_range("bytes=0-1,5-6", 100), Full);
        assert_eq!(parse_range("bytes=9-1", 100), Full);
        assert_eq!(parse_range("items=0-1", 100), Full);
        assert_eq!(parse_range("bytes=a-b", 100), Full);
        // starts after the end
        assert_eq!(parse_range("bytes=100-", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=500-600", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 100), Unsatisfiable);
    }

    #[test]
    fn secret_rules() {
        let k = |key: Option<&str>, secret: Option<&str>, production| {
            Klipy::configure(
                key.map(String::from),
                None,
                secret.map(String::from),
                production,
            )
        };
        // production: key without a good secret switches the routes off
        assert!(!k(Some("key"), None, true).enabled());
        assert!(!k(Some("key"), Some("short"), true).enabled());
        assert!(k(Some("key"), Some("0123456789abcdef"), true).enabled());
        // development stays usable
        assert!(k(Some("key"), None, false).enabled());
        // no key: off either way
        assert!(!k(None, Some("0123456789abcdef"), true).enabled());
    }

    #[test]
    fn random_secrets_differ_between_instances() {
        let url = "https://static.klipy.com/a.gif";
        let a = Klipy::new(Some("k".into()), None).signed_link(url, 1);
        let b = Klipy::new(Some("k".into()), None).signed_link(url, 1);
        assert_ne!(a, b);
    }

    /// Local stand-in for KLIPY that records what it was asked.
    async fn mock(body: Value, seen: Arc<Mutex<Vec<(String, String)>>>) -> String {
        let app = Router::new().fallback(any(move |req: Request| {
            let seen = seen.clone();
            let body = body.clone();
            async move {
                let mut headers = String::new();
                for (name, value) in req.headers() {
                    headers.push_str(&format!("{name}: {}\n", value.to_str().unwrap_or("")));
                }
                seen.lock().unwrap().push((req.uri().to_string(), headers));
                axum::Json(body)
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/api")
    }

    #[tokio::test]
    async fn upstream_sees_no_user_headers_and_response_hides_the_key() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        // a hostile upstream that echoes the key
        let base = mock(
            json!({"result": true, "data": {"data": [{"echo": KEY}], "has_next": false}}),
            seen.clone(),
        )
        .await;
        let k = Klipy::new(Some(KEY.into()), Some(base));
        let url = k.gifs_url("cats", 1, 24, "en").unwrap();
        let out = k.get_json(url).await.unwrap();
        assert!(!out.to_string().contains(KEY));

        let seen = seen.lock().unwrap();
        let (uri, headers) = &seen[0];
        assert!(uri.contains("/gifs/search"));
        let lower = headers.to_ascii_lowercase();
        for forbidden in [
            "x-session-token",
            "x-bot-token",
            "cookie",
            "authorization",
            "x-forwarded-for",
            "referer",
        ] {
            assert!(!lower.contains(forbidden), "{forbidden} was forwarded");
        }
    }

    #[tokio::test]
    async fn errors_do_not_contain_the_key() {
        // nothing listens here, so the request fails
        let k = klipy();
        let url = k.gifs_url("cats", 1, 24, "en").unwrap();
        let err = k.get_json(url).await.unwrap_err();
        assert_eq!(err, KlipyError::Upstream);
        assert!(!format!("{err:?}").contains(KEY));
    }
}
