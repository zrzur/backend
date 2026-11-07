//! Purge file links at Cloudflare.
//!
//! Files that leave the public (deleted, a suspended design, removed
//! content) must leave the CDN too, or it keeps serving them for up to a
//! month. Purging never gets in the way of what the user did: nothing here
//! returns an error, a failure is retried and then logged, and without a
//! token nothing is sent at all.

use std::time::Duration;

use revolt_config::{config, FilesPurge};

use crate::File;

/// URLs Cloudflare takes in one purge request
const URLS_PER_REQUEST: usize = 30;

/// Tries for one request
const ATTEMPTS: u32 = 4;

/// Wait before the second try; it doubles after that
const FIRST_RETRY: Duration = Duration::from_secs(2);

/// How a purge went
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeOutcome {
    /// Every URL was purged, or there was nothing to purge
    Done,
    /// No token: nothing was sent
    NotConfigured,
    /// At least one request failed after its retries
    Failed,
}

/// A file name as a browser requests it (`encodeURIComponent`)
fn encode_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'!' | b'\''
            | b'(' | b')' | b'*' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Every link a file can be cached under: its preview, `/original` and its
/// named file
pub fn file_urls(base: &str, tag: &str, id: &str, filename: &str) -> Vec<String> {
    let root = format!("{}/{tag}/{id}", base.trim_end_matches('/'));
    vec![
        root.clone(),
        format!("{root}/original"),
        format!("{root}/{}", encode_name(filename)),
    ]
}

/// Links of several files, one list
pub fn urls_of(base: &str, files: &[File]) -> Vec<String> {
    files
        .iter()
        .flat_map(|file| file_urls(base, &file.tag, &file.id, &file.filename))
        .collect()
}

enum Attempt {
    Done,
    /// Try again later
    Retry(String),
    /// Trying again can't help (a bad token, for example)
    Stop(String),
}

async fn send(client: &reqwest::Client, settings: &FilesPurge, urls: &[String]) -> Attempt {
    let endpoint = format!(
        "{}/zones/{}/purge_cache",
        settings.api_url.trim_end_matches('/'),
        settings.zone_id.trim()
    );
    let response = client
        .post(endpoint)
        .bearer_auth(settings.api_token.trim())
        .timeout(Duration::from_secs(10))
        .json(&serde_json::json!({ "files": urls }))
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => Attempt::Done,
        Ok(response) => {
            let status = response.status();
            if status.as_u16() == 429 || status.is_server_error() {
                Attempt::Retry(status.to_string())
            } else {
                Attempt::Stop(status.to_string())
            }
        }
        // the error names the endpoint, never the token
        Err(error) => Attempt::Retry(error.without_url().to_string()),
    }
}

/// Purge `urls` with these settings, waiting `first_retry` (then twice that
/// each time) between tries
pub async fn purge_with(
    settings: &FilesPurge,
    urls: &[String],
    first_retry: Duration,
) -> PurgeOutcome {
    if urls.is_empty() {
        return PurgeOutcome::Done;
    }
    if !settings.configured() {
        log::info!("CDN purge is not configured: {} links not purged", urls.len());
        return PurgeOutcome::NotConfigured;
    }

    let client = reqwest::Client::new();
    for (index, batch) in urls.chunks(URLS_PER_REQUEST).enumerate() {
        let mut wait = first_retry;
        for attempt in 1..=ATTEMPTS {
            match send(&client, settings, batch).await {
                Attempt::Done => break,
                Attempt::Retry(reason) if attempt < ATTEMPTS => {
                    log::warn!("CDN purge try {attempt} failed: {reason}");
                    tokio::time::sleep(wait).await;
                    wait *= 2;
                }
                // the rest would fail the same way, each with its own waits
                Attempt::Retry(reason) | Attempt::Stop(reason) => {
                    let left = urls.len() - index * URLS_PER_REQUEST;
                    log::error!("CDN purge stopped, {left} links not purged: {reason}");
                    return PurgeOutcome::Failed;
                }
            }
        }
    }
    PurgeOutcome::Done
}

/// Purge files and wait for the answer. For background tasks only: a user's
/// request uses [`purge_files_later`].
pub async fn purge_files(files: &[File]) -> PurgeOutcome {
    let config = config().await;
    purge_with(
        &config.files.purge,
        &urls_of(&config.hosts.autumn, files),
        FIRST_RETRY,
    )
    .await
}

/// Purge files without holding up the request that took them down
pub fn purge_files_later(files: Vec<File>) {
    if files.is_empty() {
        return;
    }
    tokio::spawn(async move {
        purge_files(&files).await;
    });
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
    };

    use super::*;

    /// Answers each request with the next status; the bodies it was sent
    fn cloudflare(statuses: Vec<u16>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/client/v4", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let read = stream.read(&mut chunk).unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .to_lowercase()
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: ")?.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                log.lock().unwrap().push(String::from_utf8_lossy(&request).to_string());
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                )
                .unwrap();
            }
        });
        (url, seen)
    }

    fn settings(api_url: String) -> FilesPurge {
        FilesPurge {
            zone_id: "zone1".into(),
            api_token: "purge-only".into(),
            api_url,
        }
    }

    #[test]
    fn a_file_has_three_links() {
        assert_eq!(
            file_urls("https://cdn.zrzur.com/", "avatars", "abc", "my cat (1).png"),
            vec![
                "https://cdn.zrzur.com/avatars/abc",
                "https://cdn.zrzur.com/avatars/abc/original",
                "https://cdn.zrzur.com/avatars/abc/my%20cat%20(1).png",
            ]
        );
        assert_eq!(
            file_urls("https://cdn.zrzur.com", "attachments", "x", "صورة.png")[2],
            "https://cdn.zrzur.com/attachments/x/%D8%B5%D9%88%D8%B1%D8%A9.png"
        );
    }

    #[tokio::test]
    async fn no_token_sends_nothing_and_fails_nothing() {
        let unused = FilesPurge {
            zone_id: "zone1".into(),
            api_token: "  ".into(),
            api_url: "http://127.0.0.1:1".into(),
        };
        let outcome = purge_with(&unused, &["https://cdn/x".to_string()], Duration::ZERO).await;
        assert_eq!(outcome, PurgeOutcome::NotConfigured);
    }

    #[tokio::test]
    async fn sends_the_links_with_the_token() {
        let (url, seen) = cloudflare(vec![200]);
        let urls = vec!["https://cdn.zrzur.com/avatars/a".to_string()];
        let outcome = purge_with(&settings(url), &urls, Duration::ZERO).await;
        assert_eq!(outcome, PurgeOutcome::Done);
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.starts_with("POST /client/v4/zones/zone1/purge_cache "), "{request}");
        assert!(request.to_lowercase().contains("authorization: bearer purge-only"));
        assert!(request.ends_with(r#"{"files":["https://cdn.zrzur.com/avatars/a"]}"#), "{request}");
    }

    #[tokio::test]
    async fn retries_when_cloudflare_is_busy() {
        let (url, seen) = cloudflare(vec![429, 503, 200]);
        let urls = vec!["https://cdn/x".to_string()];
        let outcome = purge_with(&settings(url), &urls, Duration::from_millis(1)).await;
        assert_eq!(outcome, PurgeOutcome::Done);
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn gives_up_after_its_tries_without_an_error() {
        let (url, seen) = cloudflare(vec![500, 500, 500, 500]);
        let urls = vec!["https://cdn/x".to_string()];
        let outcome = purge_with(&settings(url), &urls, Duration::from_millis(1)).await;
        assert_eq!(outcome, PurgeOutcome::Failed);
        assert_eq!(seen.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn a_refused_token_is_not_retried() {
        let (url, seen) = cloudflare(vec![403]);
        let urls = vec!["https://cdn/x".to_string()];
        let outcome = purge_with(&settings(url), &urls, Duration::from_millis(1)).await;
        assert_eq!(outcome, PurgeOutcome::Failed);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_dead_api_stops_the_run_after_one_batch() {
        let (url, seen) = cloudflare(vec![500, 500, 500, 500]);
        let urls: Vec<String> = (0..90).map(|i| format!("https://cdn/{i}")).collect();
        let outcome = purge_with(&settings(url), &urls, Duration::from_millis(1)).await;
        assert_eq!(outcome, PurgeOutcome::Failed);
        assert_eq!(seen.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn large_purges_go_in_batches() {
        let (url, seen) = cloudflare(vec![200, 200]);
        let urls: Vec<String> = (0..31).map(|i| format!("https://cdn/{i}")).collect();
        let outcome = purge_with(&settings(url), &urls, Duration::ZERO).await;
        assert_eq!(outcome, PurgeOutcome::Done);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }
}
