//! Twitch with the server's own app token (client credentials): live/offline
//! subscriptions (EventSub by webhook), stream titles and the daily name
//! check. Nothing here uses a person's token.
//!
//! Twitch's Developer Agreement (last modified 12/04/2024) allows a copy of
//! Twitch data for 24 hours: names are read again once a day, a live flag
//! older than that is checked again or dropped.

use std::collections::HashMap;

use revolt_config::ApiConnectionOAuth;
use revolt_result::{create_error, Result};
use serde::Deserialize;
use serde_json::json;

use super::oauth_link::{clean_text, client, get_json, twitch_identity, Identity, TwitchUser, TWITCH_API, TWITCH_AUTH};

/// Longest stream title kept
pub const MAX_TITLE: usize = 140;

#[derive(Deserialize)]
struct AppToken {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

/// The app token, fetched again shortly before it runs out
async fn app_token(config: &ApiConnectionOAuth) -> Result<String> {
    use std::time::{Duration, Instant};
    use tokio::sync::Mutex;

    static TOKEN: std::sync::OnceLock<Mutex<Option<(String, Instant, String)>>> =
        std::sync::OnceLock::new();
    let mut cached = TOKEN.get_or_init(|| Mutex::new(None)).lock().await;
    if let Some((token, until, client_id)) = cached.as_ref() {
        if *client_id == config.client_id && Instant::now() < *until {
            return Ok(token.clone());
        }
    }
    let answer: AppToken = get_json(
        client()
            .post(format!("{}/oauth2/token", config.auth_base_or(TWITCH_AUTH)))
            .form(&[
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.as_str()),
                ("grant_type", "client_credentials"),
            ]),
    )
    .await?;
    let lifetime = Duration::from_secs(answer.expires_in.max(120) - 60);
    *cached = Some((
        answer.access_token.clone(),
        Instant::now() + lifetime,
        config.client_id.clone(),
    ));
    Ok(answer.access_token)
}

async fn helix(config: &ApiConnectionOAuth, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
    let token = app_token(config).await?;
    Ok(client()
        .request(method, format!("{}/helix/{path}", config.api_base_or(TWITCH_API)))
        .header("client-id", &config.client_id)
        .bearer_auth(token))
}

#[derive(Deserialize)]
struct Created {
    data: Vec<CreatedSub>,
}

#[derive(Deserialize)]
struct CreatedSub {
    id: String,
}

/// Ask Twitch to tell us when this channel goes live or offline. Returns the
/// subscription ids; empty when live status is off on this server.
pub async fn subscribe(config: &ApiConnectionOAuth, broadcaster_id: &str) -> Result<Vec<String>> {
    if !config.live_enabled() {
        return Ok(vec![]);
    }
    #[cfg(test)]
    {
        test_twitch::record(format!("subscribe {broadcaster_id}"));
        return Ok(vec![
            format!("sub-online-{broadcaster_id}"),
            format!("sub-offline-{broadcaster_id}"),
        ]);
    }

    #[allow(unreachable_code)]
    {
        let mut ids = vec![];
        for kind in ["stream.online", "stream.offline"] {
            let body = json!({
                "type": kind,
                "version": "1",
                "condition": { "broadcaster_user_id": broadcaster_id },
                "transport": {
                    "method": "webhook",
                    "callback": config.eventsub_callback,
                    "secret": config.eventsub_secret,
                },
            });
            let answer = helix(config, reqwest::Method::POST, "eventsub/subscriptions")
                .await?
                .json(&body)
                .send()
                .await
                .map_err(|_| create_error!(LinkedServiceFailed))?;
            // 409: it exists already (a link made again)
            if answer.status() == reqwest::StatusCode::CONFLICT {
                continue;
            }
            if !answer.status().is_success() {
                return Err(create_error!(LinkedServiceFailed));
            }
            let created: Created = answer
                .json()
                .await
                .map_err(|_| create_error!(LinkedServiceFailed))?;
            ids.extend(created.data.into_iter().map(|sub| sub.id));
        }
        Ok(ids)
    }
}

/// Ask Twitch to tell us when anyone removes Zrzur in their Twitch settings
/// (one subscription for the whole app; 409 means it is there)
pub async fn subscribe_revocations(config: &ApiConnectionOAuth) -> Result<()> {
    if !config.live_enabled() {
        return Ok(());
    }
    #[cfg(test)]
    {
        test_twitch::record("subscribe revocations".into());
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        let body = json!({
            "type": "user.authorization.revoke",
            "version": "1",
            "condition": { "client_id": config.client_id },
            "transport": {
                "method": "webhook",
                "callback": config.eventsub_callback,
                "secret": config.eventsub_secret,
            },
        });
        let answer = helix(config, reqwest::Method::POST, "eventsub/subscriptions")
            .await?
            .json(&body)
            .send()
            .await
            .map_err(|_| create_error!(LinkedServiceFailed))?;
        if answer.status().is_success() || answer.status() == reqwest::StatusCode::CONFLICT {
            Ok(())
        } else {
            Err(create_error!(LinkedServiceFailed))
        }
    }
}

/// End subscriptions (Disconnect, or a channel nobody links any more)
pub async fn unsubscribe(config: &ApiConnectionOAuth, ids: &[String]) {
    #[cfg(test)]
    {
        let _ = config;
        for id in ids {
            test_twitch::record(format!("unsubscribe {id}"));
        }
        return;
    }

    #[allow(unreachable_code)]
    for id in ids {
        if let Ok(request) = helix(config, reqwest::Method::DELETE, "eventsub/subscriptions").await {
            let _ = request.query(&[("id", id.as_str())]).send().await;
        }
    }
}

#[derive(Deserialize)]
struct Streams {
    data: Vec<Stream>,
}

#[derive(Deserialize)]
struct Stream {
    user_id: String,
    #[serde(default)]
    title: String,
}

/// Which of these channels are live now, with their titles (up to 100 ids)
pub async fn live_streams(config: &ApiConnectionOAuth, ids: &[String]) -> Result<HashMap<String, String>> {
    #[cfg(test)]
    {
        let _ = config;
        return Ok(ids
            .iter()
            .filter(|id| !id.ends_with('0'))
            .map(|id| (id.clone(), format!("Live title {id}")))
            .collect());
    }

    #[allow(unreachable_code)]
    {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let query: Vec<(&str, &str)> = ids.iter().take(100).map(|id| ("user_id", id.as_str())).collect();
        let streams: Streams = get_json(
            helix(config, reqwest::Method::GET, "streams")
                .await?
                .query(&query)
                .query(&[("type", "live"), ("first", "100")]),
        )
        .await?;
        Ok(streams
            .data
            .into_iter()
            .map(|stream| (stream.user_id, clean_text(&stream.title, MAX_TITLE)))
            .collect())
    }
}

#[derive(Deserialize)]
struct Users {
    data: Vec<TwitchUser>,
}

/// Names of these accounts as Twitch shows them now (up to 100 ids).
/// Deleted or suspended accounts are missing from the answer.
pub async fn users(config: &ApiConnectionOAuth, ids: &[String]) -> Result<Vec<Identity>> {
    #[cfg(test)]
    {
        let _ = config;
        // a channel id ending in 503: Twitch is down for this request
        if ids.iter().any(|id| id.ends_with("503")) {
            return Err(create_error!(LinkedServiceFailed));
        }
        return Ok(ids
            .iter()
            .filter(|id| !id.ends_with("404"))
            .filter_map(|id| {
                twitch_identity(TwitchUser {
                    id: id.clone(),
                    login: format!("renamed_{id}"),
                    display_name: format!("Renamed{id}"),
                })
                .ok()
            })
            .collect());
    }

    #[allow(unreachable_code)]
    {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let query: Vec<(&str, &str)> = ids.iter().take(100).map(|id| ("id", id.as_str())).collect();
        let users: Users = get_json(helix(config, reqwest::Method::GET, "users").await?.query(&query)).await?;
        Ok(users
            .data
            .into_iter()
            .filter_map(|user| twitch_identity(user).ok())
            .collect())
    }
}

#[derive(Deserialize)]
struct SubscriptionPage {
    data: Vec<SubscriptionEntry>,
    #[serde(default)]
    pagination: Pagination,
}

#[derive(Deserialize, Default)]
struct Pagination {
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct SubscriptionEntry {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    condition: SubscriptionCondition,
}

#[derive(Deserialize, Default)]
struct SubscriptionCondition {
    #[serde(default)]
    broadcaster_user_id: Option<String>,
}

/// Our live/offline subscriptions at Twitch: (subscription id, channel id)
pub async fn stream_subscriptions(config: &ApiConnectionOAuth) -> Result<Vec<(String, String)>> {
    if !config.live_enabled() {
        return Ok(vec![]);
    }
    #[cfg(test)]
    {
        return Ok(test_twitch::subscriptions());
    }

    #[allow(unreachable_code)]
    {
        let mut found = vec![];
        let mut cursor: Option<String> = None;
        // at most 100 pages of 100: far more than this server links
        for _ in 0..100 {
            let mut request = helix(config, reqwest::Method::GET, "eventsub/subscriptions").await?;
            if let Some(after) = &cursor {
                request = request.query(&[("after", after.as_str())]);
            }
            let page: SubscriptionPage = get_json(request).await?;
            for entry in page.data {
                if entry.kind.starts_with("stream.") {
                    if let Some(channel) = entry.condition.broadcaster_user_id {
                        found.push((entry.id, channel));
                    }
                }
            }
            match page.pagination.cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(found)
    }
}

/// What the tests saw the server ask Twitch to do
#[cfg(test)]
pub mod test_twitch {
    use std::sync::Mutex;

    static CALLS: Mutex<Vec<String>> = Mutex::new(vec![]);

    static SUBSCRIPTIONS: Mutex<Vec<(String, String)>> = Mutex::new(vec![]);

    pub fn record(call: String) {
        CALLS.lock().unwrap().push(call);
    }

    /// Make Twitch report a subscription for this channel
    pub fn add_subscription(id: &str, channel: &str) {
        SUBSCRIPTIONS.lock().unwrap().push((id.into(), channel.into()));
    }

    pub fn subscriptions() -> Vec<(String, String)> {
        SUBSCRIPTIONS.lock().unwrap().clone()
    }

    /// Whether this call was made (tests run in parallel: look for your own ids)
    pub fn called(call: &str) -> bool {
        CALLS.lock().unwrap().iter().any(|c| c == call)
    }
}
