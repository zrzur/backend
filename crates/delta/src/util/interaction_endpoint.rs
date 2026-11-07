//! Interactions over HTTP: a bot may receive them at its own HTTPS address
//! instead of the gateway. Signing and the guarded request itself are shared
//! (`signed_request`, `signed_delivery`).

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

use revolt_config::config;
use revolt_database::{
    enabled_intents, util::signed_request::derive_secret, Bot, Database,
    Interaction, User, AMQP,
};
use revolt_models::v0::{self, DataInteractionCallback, GatewayIntents, InteractionState};
use revolt_result::{create_error, Result};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

use super::signed_delivery::{parse_target, post, Problem};

/// Names interaction secrets among everything signed with the instance key
pub const PURPOSE: &str = "interactions";

/// Requests on their way out at once; beyond this bots look offline for a moment
const MAX_IN_FLIGHT: usize = 256;
/// One slow bot may not use up the requests of all the others
const MAX_IN_FLIGHT_PER_BOT: usize = 32;
/// Every bot's secret comes from this key: a short one can be guessed
const MIN_SIGNING_KEY_BYTES: usize = 32;

/// The configured key, if it may be used
fn usable_key(key: &str) -> Option<&str> {
    (key.len() >= MIN_SIGNING_KEY_BYTES).then_some(key)
}

/// The instance's signing key, or the feature is off
pub async fn signing_key() -> Result<String> {
    let key = config().await.api.interactions.signing_key;
    match usable_key(&key) {
        Some(key) => Ok(key.to_string()),
        None => {
            if !key.is_empty() {
                log::error!("api.interactions.signing_key is under {MIN_SIGNING_KEY_BYTES} bytes: HTTP interactions stay off");
            }
            Err(create_error!(FeatureDisabled {
                feature: "interactions_endpoint".to_string()
            }))
        }
    }
}

/// Prove an address belongs to the bot before it gets any real interaction:
/// a request signed with the wrong secret must be refused, and a correctly
/// signed `Ping` must get `{"type":"Pong"}`.
pub async fn verify_endpoint(raw: &str, bot: &Bot) -> Result<Url> {
    let config = config().await;
    let allow_private = config.api.interactions.allow_private_urls;
    let url = parse_target(raw, allow_private, &config.january.blocked_domains)?;
    let key = signing_key().await?;
    let secret = derive_secret(&key, PURPOSE, &bot.id, bot.interactions_secret_version);

    let ping = |delivery: &str| {
        serde_json::json!({
            "type": "Ping",
            "_id": delivery,
            "application_id": bot.id,
            "version": v0::INTERACTION_VERSION,
        })
        .to_string()
        .into_bytes()
    };
    let wrong = format!("zrzur_sign_{}", nanoid::nanoid!(64));

    // Either order, each with its own delivery id, so an endpoint can't pass
    // by refusing the first request and accepting the second.
    let forged_first: bool = rand::random();
    for forged in [forged_first, !forged_first] {
        let delivery = ulid::Ulid::new().to_string();
        let body = ping(&delivery);
        if forged {
            let refused = post(&url, &wrong, &delivery, &body, allow_private).await?;
            if (200..300).contains(&refused.status) {
                return Err(Problem::AcceptsBadSignatures.into());
            }
        } else {
            let answer = post(&url, &secret, &delivery, &body, allow_private).await?;
            if answer.status != 200 {
                return Err(Problem::BadStatus.into());
            }
            let pong: serde_json::Value =
                serde_json::from_slice(&answer.body).map_err(|_| Problem::BadAnswer)?;
            if pong["type"] != "Pong" {
                return Err(Problem::BadAnswer.into());
            }
        }
    }
    Ok(url)
}

static IN_FLIGHT: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_IN_FLIGHT)));
static IN_FLIGHT_BY_BOT: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

/// One bot's place among its requests on their way out
struct BotSlot(String);

impl BotSlot {
    fn take(bot: &str) -> Option<BotSlot> {
        let mut counts = IN_FLIGHT_BY_BOT.lock().ok()?;
        let count = counts.entry(bot.to_string()).or_insert(0);
        if *count >= MAX_IN_FLIGHT_PER_BOT {
            return None;
        }
        *count += 1;
        Some(BotSlot(bot.to_string()))
    }
}

impl Drop for BotSlot {
    fn drop(&mut self) {
        let Ok(mut counts) = IN_FLIGHT_BY_BOT.lock() else {
            return;
        };
        if let Some(count) = counts.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.0);
            }
        }
    }
}

/// An interaction ready to go to a bot's endpoint
pub struct Delivery {
    url: Url,
    secret: String,
    body: Vec<u8>,
    allow_private: bool,
    _permit: OwnedSemaphorePermit,
    _slot: BotSlot,
}

/// Get a delivery ready. A bot that can't be reached this way counts as
/// offline, like a bot without a gateway connection.
pub async fn prepare(
    db: &Database,
    bot: &Bot,
    endpoint: &str,
    payload: &v0::Interaction,
) -> Result<Delivery> {
    let config = config().await;
    let allow_private = config.api.interactions.allow_private_urls;
    let offline = || create_error!(ApplicationOffline);
    let key = signing_key().await.map_err(|_| offline())?;
    let url = parse_target(endpoint, allow_private, &config.january.blocked_domains)
        .map_err(|_| offline())?;
    let slot = BotSlot::take(&bot.id).ok_or_else(offline)?;
    let permit = IN_FLIGHT.clone().try_acquire_owned().map_err(|_| offline())?;

    // the same rule as the gateway: intents decide what a bot sees
    let reader = User {
        id: bot.id.clone(),
        bot: Some(revolt_database::BotInformation {
            owner: bot.owner.clone(),
            verified: false,
        }),
        ..Default::default()
    };
    let mut payload = payload.clone();
    if enabled_intents(db, &reader).await & GatewayIntents::PRESENCE == 0 {
        payload.hide_presence();
    }

    Ok(Delivery {
        url,
        secret: derive_secret(&key, PURPOSE, &bot.id, bot.interactions_secret_version),
        body: serde_json::to_vec(&payload).map_err(|_| create_error!(InternalError))?,
        allow_private,
        _permit: permit,
        _slot: slot,
    })
}

impl Delivery {
    /// Send it. A 200 may carry the answer itself (same body as the
    /// callback route), 202 or 204 means the bot answers through the callback
    /// route; anything else fails the interaction for the person waiting.
    pub async fn send(self, db: Database, amqp: AMQP, interaction: Interaction) {
        if let Err(reason) = self.exchange(&db, &amqp, &interaction).await {
            log::warn!(
                "interaction {} of bot {} not delivered: {reason}",
                interaction.id,
                interaction.application_id
            );
            crate::routes::interactions::state_update(&interaction, InteractionState::Failed, None)
                .p(interaction.user_id.clone())
                .await;
        }
    }

    async fn exchange(
        &self,
        db: &Database,
        amqp: &AMQP,
        interaction: &Interaction,
    ) -> std::result::Result<(), &'static str> {
        let answer = post(
            &self.url,
            &self.secret,
            &interaction.id,
            &self.body,
            self.allow_private,
        )
        .await
        .map_err(Problem::code)?;

        match answer.status {
            202 | 204 => Ok(()),
            200 => {
                let data: DataInteractionCallback = serde_json::from_slice(&answer.body)
                    .map_err(|_| Problem::BadAnswer.code())?;
                crate::routes::interactions::answer(db, amqp, interaction.clone(), data)
                    .await
                    .map_err(|_| "answer refused")
            }
            _ => Err(Problem::BadStatus.code()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{usable_key, BotSlot, MAX_IN_FLIGHT_PER_BOT, MIN_SIGNING_KEY_BYTES};

    #[test]
    fn a_short_signing_key_is_refused() {
        assert_eq!(usable_key(""), None);
        assert_eq!(usable_key(&"k".repeat(MIN_SIGNING_KEY_BYTES - 1)), None);
        let long = "k".repeat(MIN_SIGNING_KEY_BYTES);
        assert_eq!(usable_key(&long), Some(long.as_str()));
    }

    #[test]
    fn one_bot_cannot_use_up_the_queue() {
        let held: Vec<_> = (0..MAX_IN_FLIGHT_PER_BOT)
            .map(|_| BotSlot::take("busy").expect("a place"))
            .collect();
        assert!(BotSlot::take("busy").is_none(), "over its own limit");
        assert!(BotSlot::take("other").is_some(), "others keep theirs");
        drop(held);
        assert!(BotSlot::take("busy").is_some(), "places come back");
    }

    #[test]
    fn the_shipped_configuration_is_closed() {
        let defaults = include_str!("../../../core/config/Revolt.toml");
        assert!(defaults.contains("allow_private_urls = false"));
        assert!(defaults.contains("signing_key = \"\""));
    }
}
