//! The staff inbox: one short line in a staff channel when something new
//! waits for the team (a report, a support ticket, a bug or feedback), posted
//! as the Zrzur Desk bot. Only ids and kinds, never what was written: staff
//! read the rest in the desk, with their own grant and code.

use revolt_config::config;
use ulid::Ulid;

use crate::{Database, Message};

/// The channel a kind of news goes to (its own, or the inbox) and the bot
/// that posts it; `None` when the desk or the inbox is off
pub fn inbox_of(desk: &revolt_config::FeaturesStaffDesk, kind: &str) -> Option<(String, String)> {
    let own = desk.inbox_channels.get(kind).map(|c| c.trim()).filter(|c| !c.is_empty());
    let channel = own.unwrap_or(desk.inbox.trim());
    let bot = desk.bot.trim();
    (desk.enabled && !channel.is_empty() && !bot.is_empty()).then(|| (channel.to_string(), bot.to_string()))
}

/// Post one line to the staff inbox. Never fails the request that called it.
pub async fn announce(db: &Database, kind: &str, text: String) {
    let desk = config().await.features.staff_desk;
    let Some((channel, bot)) = inbox_of(&desk, kind) else {
        return;
    };
    let mut message = Message {
        id: Ulid::new().to_string(),
        channel,
        author: bot,
        content: Some(text.chars().take(400).collect()),
        ..Default::default()
    };
    if let Err(error) = message
        .send_without_notifications(db, None, None, false, false, false)
        .await
    {
        log::warn!("staff inbox: {error:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::inbox_of;

    #[test]
    fn the_inbox_needs_the_desk_a_bot_and_a_channel() {
        let mut desk = revolt_config::FeaturesStaffDesk::default();
        assert_eq!(inbox_of(&desk, "reports"), None);
        desk.enabled = true;
        desk.bot = "BOT".into();
        assert_eq!(inbox_of(&desk, "reports"), None);
        desk.inbox = " CHANNEL ".into();
        assert_eq!(inbox_of(&desk, "reports"), Some(("CHANNEL".into(), "BOT".into())));
        desk.enabled = false;
        assert_eq!(inbox_of(&desk, "reports"), None);
    }

    #[test]
    fn a_kind_can_have_its_own_channel() {
        let mut desk = revolt_config::FeaturesStaffDesk {
            enabled: true,
            bot: "BOT".into(),
            inbox: "INBOX".into(),
            ..Default::default()
        };
        desk.inbox_channels.insert("verification".into(), "REVIEW".into());
        desk.inbox_channels.insert("discover".into(), " ".into());
        assert_eq!(inbox_of(&desk, "verification").unwrap().0, "REVIEW");
        assert_eq!(inbox_of(&desk, "discover").unwrap().0, "INBOX");
        assert_eq!(inbox_of(&desk, "support").unwrap().0, "INBOX");
        desk.inbox = String::new();
        assert_eq!(inbox_of(&desk, "support"), None);
        assert_eq!(inbox_of(&desk, "verification").unwrap().0, "REVIEW");
    }
}
