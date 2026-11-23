//! Bots in direct messages (`features.bot_dms_enabled`).
//!
//! A bot may write to a person in their conversation only once the person
//! started it: pressed Start, or wrote to it. That consent is the person's
//! entry in the channel's `started_by`, and blocking the bot takes it away, so
//! after an unblock only a new Start or a new message counts (messages from
//! before the stop do not). Until then the bot does not even learn about the
//! conversation: it gets `ChannelCreate` when the person starts it.

use revolt_result::Result;

use crate::{events::client::EventV1, Channel, Database, User};

/// Whether bots in direct messages are switched on
pub async fn enabled() -> bool {
    revolt_config::config().await.features.bot_dms_enabled
}

/// The bot of a DM between a person and a bot, seen from `person`
pub async fn bot_of(db: &Database, channel: &Channel, person: &User) -> Option<User> {
    if person.bot.is_some() {
        return None;
    }
    let Channel::DirectMessage { recipients, .. } = channel else {
        return None;
    };
    let other = recipients.iter().find(|id| **id != person.id)?;
    db.fetch_user(other).await.ok().filter(|user| user.bot.is_some())
}

/// The person started the bot (Start, or a message to it): note the consent
/// once and tell the bot's sessions about the conversation
pub async fn note_started(db: &Database, channel: &Channel, person: &User) -> Result<()> {
    if !enabled().await {
        return Ok(());
    }
    let Some(bot) = bot_of(db, channel, person).await else {
        return Ok(());
    };
    if let Channel::DirectMessage { started_by, .. } = channel {
        if started_by.contains(&person.id) {
            return Ok(());
        }
    }
    db.set_bot_started(channel.id(), &person.id, true).await?;
    let channel = db.fetch_channel(channel.id()).await?;
    EventV1::ChannelCreate(channel.into())
        .p_broadcast(vec![bot.id])
        .await;
    Ok(())
}
