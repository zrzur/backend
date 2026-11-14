//! `POST /connections/twitch/events`: Twitch tells us a linked channel went
//! live or offline, or that someone removed Zrzur in their Twitch settings.
//!
//! Nobody signs in here: a message counts only with Twitch's HMAC signature
//! made with the secret we gave Twitch, a time under 10 minutes old and a
//! message id not seen before. Anything else gets 403 and changes nothing.

use iso8601_timestamp::Timestamp;
use redis_kiss::{get_connection, AsyncCommands};
use revolt_database::{util::linked_crypto::twitch_signature_valid, Database};
use rocket::{
    data::{Data, ToByteUnit},
    http::Status,
    request::{FromRequest, Outcome, Request},
    response::content::RawText,
    State,
};
use serde::Deserialize;

use crate::{
    routes::users::connections_oauth::now_ms,
    util::{oauth_link::Service, twitch},
};

/// Oldest message accepted
const MAX_AGE_MS: i64 = 10 * 60 * 1000;
/// Biggest message read
const MAX_BODY_KB: u64 = 64;

/// Twitch's own headers on every message
pub struct TwitchHeaders {
    id: String,
    timestamp: String,
    signature: String,
    kind: String,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for TwitchHeaders {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let header = |name: &str| request.headers().get_one(name).map(str::to_string);
        match (
            header("Twitch-Eventsub-Message-Id"),
            header("Twitch-Eventsub-Message-Timestamp"),
            header("Twitch-Eventsub-Message-Signature"),
            header("Twitch-Eventsub-Message-Type"),
        ) {
            (Some(id), Some(timestamp), Some(signature), Some(kind))
                if !id.is_empty() && id.len() <= 128 =>
            {
                Outcome::Success(TwitchHeaders { id, timestamp, signature, kind })
            }
            _ => Outcome::Error((Status::Forbidden, ())),
        }
    }
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    challenge: Option<String>,
    subscription: Subscription,
    #[serde(default)]
    event: Option<Event>,
}

#[derive(Deserialize)]
struct Subscription {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    condition: Condition,
}

#[derive(Deserialize, Default)]
struct Condition {
    #[serde(default)]
    broadcaster_user_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct Event {
    #[serde(default)]
    broadcaster_user_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
}

/// Milliseconds since the epoch of Twitch's RFC 3339 time (any fraction)
pub fn twitch_time_ms(text: &str) -> Option<i64> {
    let seconds = text.get(..19)?;
    let parsed = Timestamp::parse(&format!("{seconds}Z"))?;
    Some(parsed.duration_since(Timestamp::UNIX_EPOCH).whole_milliseconds() as i64)
}

/// # Twitch Live Status
///
/// Called by Twitch only, signed with the secret this server gave it.
#[openapi(skip)]
#[post("/twitch/events", data = "<body>")]
pub async fn twitch_events(
    db: &State<Database>,
    headers: TwitchHeaders,
    body: Data<'_>,
) -> Result<RawText<String>, Status> {
    let config = Service::Twitch.config().await;
    if !config.live_enabled() {
        return Err(Status::NotFound);
    }
    let body = body
        .open(MAX_BODY_KB.kibibytes())
        .into_bytes()
        .await
        .map_err(|_| Status::BadRequest)?;
    if !body.is_complete() {
        return Err(Status::PayloadTooLarge);
    }
    let body = body.into_inner();

    if !twitch_signature_valid(
        &config.eventsub_secret,
        &headers.id,
        &headers.timestamp,
        &body,
        &headers.signature,
    ) {
        return Err(Status::Forbidden);
    }
    let sent = twitch_time_ms(&headers.timestamp).ok_or(Status::Forbidden)?;
    if (now_ms() - sent).abs() > MAX_AGE_MS {
        return Err(Status::Forbidden);
    }

    let message: Message = serde_json::from_slice(&body).map_err(|_| Status::BadRequest)?;

    // the same message twice (Twitch retries) does its work once
    let mut redis = get_connection()
        .await
        .map_err(|_| Status::ServiceUnavailable)?
        .into_inner();
    let first: bool = redis
        .set_nx(format!("twitch-event:{}", headers.id), 1)
        .await
        .map_err(|_| Status::ServiceUnavailable)?;
    if first {
        let _: () = redis
            .expire(format!("twitch-event:{}", headers.id), (MAX_AGE_MS / 1000) as usize)
            .await
            .map_err(|_| Status::ServiceUnavailable)?;
    }

    match headers.kind.as_str() {
        "webhook_callback_verification" => {
            // Twitch checks the address is ours before it sends anything
            let challenge = message.challenge.ok_or(Status::BadRequest)?;
            if challenge.len() > 1024 {
                return Err(Status::BadRequest);
            }
            Ok(RawText(challenge))
        }
        _ if !first => Ok(RawText(String::new())),
        "notification" => {
            let event = message.event.unwrap_or_default();
            if handle(db, &config, &message.subscription, event).await.is_err() {
                // let Twitch's retry do the work
                let _: Result<(), _> = redis.del(format!("twitch-event:{}", headers.id)).await;
                return Err(Status::ServiceUnavailable);
            }
            Ok(RawText(String::new()))
        }
        "revocation" => {
            // the app-wide "removed at Twitch" subscription must never lapse
            if message.subscription.kind == "user.authorization.revoke" {
                if twitch::subscribe_revocations(&config).await.is_err() {
                    log::warn!("Twitch revocation messages could not be set up again; the hourly upkeep tries again");
                }
                return Ok(RawText(String::new()));
            }
            // Twitch ended a subscription (account gone, too many failures):
            // forget it, so Disconnect does not ask for it again
            if let Some(broadcaster) = message.subscription.condition.broadcaster_user_id {
                if let Ok(Some(mut account)) =
                    db.fetch_linked_account_by_external("twitch", &broadcaster).await
                {
                    account.subscriptions.retain(|id| *id != message.subscription.id);
                    let _ = db.save_linked_account(&account).await;
                }
            }
            Ok(RawText(String::new()))
        }
        _ => Ok(RawText(String::new())),
    }
}

async fn handle(
    db: &Database,
    config: &revolt_config::ApiConnectionOAuth,
    subscription: &Subscription,
    event: Event,
) -> revolt_result::Result<()> {
    match subscription.kind.as_str() {
        "stream.online" | "stream.offline" => {
            let Some(broadcaster) = event.broadcaster_user_id else {
                return Ok(());
            };
            let Some(mut account) = db.fetch_linked_account_by_external("twitch", &broadcaster).await?
            else {
                // nobody links this channel any more (account deleted)
                twitch::unsubscribe(config, &[subscription.id.clone()]).await;
                return Ok(());
            };
            let online = subscription.kind == "stream.online";
            account.live = Some(online);
            account.live_at = Some(now_ms());
            account.live_title = if online {
                twitch::live_streams(config, &[broadcaster.clone()])
                    .await
                    .ok()
                    .and_then(|mut live| live.remove(&broadcaster))
                    .filter(|title| !title.is_empty())
            } else {
                None
            };
            db.save_linked_account(&account).await
        }
        // someone removed Zrzur in their Twitch settings: the link goes
        "user.authorization.revoke" => {
            let Some(user_id) = event.user_id else {
                return Ok(());
            };
            if let Some(account) = db.fetch_linked_account_by_external("twitch", &user_id).await? {
                twitch::unsubscribe(config, &account.subscriptions).await;
                db.delete_linked_account(&account.user_id, "twitch").await?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
