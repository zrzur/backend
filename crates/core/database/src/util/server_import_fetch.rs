//! Fetches a Discord server template by its code: the one call this server
//! ever makes to Discord, and only when a person asks for it.
//!
//! One GET, no credential, no redirect followed, five seconds, one megabyte.
//! The address comes from the configuration and the code from
//! `template_code`, so a person never chooses where the call goes. Nothing
//! about the call is logged but its outcome and how long it took.

use std::time::{Duration, Instant};

use redis_kiss::{get_connection, AsyncCommands};
use revolt_result::{create_error, Result};

use super::server_import::MAX_TEMPLATE_BYTES;

/// Says honestly who is calling; the page tells what this client does
pub const USER_AGENT: &str = "Zrzur-ServerImport/1.0 (+https://zrzur.com/bot)";
const TIMEOUT: Duration = Duration::from_secs(5);

/// Count one link read against the account; an error when it is over its
/// hour, or when the count can't be kept (no count, no call out)
pub async fn count_read(user: &str, per_hour: i64) -> Result<()> {
    let unavailable = || create_error!(ImportSourceUnavailable);
    let counter = format!("server_import:reads:{user}");
    let mut redis = get_connection().await.map_err(|_| unavailable())?;
    let count: i64 = redis.incr(&counter, 1).await.map_err(|_| unavailable())?;
    if count == 1 {
        let _: std::result::Result<(), _> = redis.expire(&counter, 3600).await;
    }
    if count > per_hour {
        let wait: i64 = redis.ttl(&counter).await.unwrap_or(3600);
        return Err(create_error!(RateLimited {
            retry_after: (wait.max(1) as u64) * 1000,
            global: false,
        }));
    }
    Ok(())
}

/// The template's text
pub async fn fetch_template(endpoint: &str, code: &str) -> Result<String> {
    let started = Instant::now();
    let outcome = fetch(endpoint, code).await;
    log::info!(
        "server import: template read {} in {} ms",
        match &outcome {
            Ok(_) => "ok",
            Err(error) => match error.error_type {
                revolt_result::ErrorType::ImportTemplateNotFound => "not found",
                revolt_result::ErrorType::ImportInvalid { .. } => "too large",
                _ => "unavailable",
            },
        },
        started.elapsed().as_millis()
    );
    outcome
}

async fn fetch(endpoint: &str, code: &str) -> Result<String> {
    let unavailable = || create_error!(ImportSourceUnavailable);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|_| unavailable())?;
    let mut response = client
        .get(format!("{endpoint}{code}"))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| unavailable())?;
    match response.status().as_u16() {
        200 => {}
        404 => return Err(create_error!(ImportTemplateNotFound)),
        // a redirect, a refusal, a limit, a fault: all the same to a person
        _ => return Err(unavailable()),
    }
    let too_large = || create_error!(ImportInvalid { reason: "too_large".to_string() });
    if response.content_length().is_some_and(|length| length > MAX_TEMPLATE_BYTES as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if body.len() + chunk.len() > MAX_TEMPLATE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body)
        .map_err(|_| create_error!(ImportInvalid { reason: "not_a_template".to_string() }))
}
