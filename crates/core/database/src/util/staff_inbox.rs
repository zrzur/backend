//! The staff inbox: one short line in a staff channel when something new
//! waits for the team (a report, a support ticket, a bug or feedback), posted
//! as the Zrzur Desk bot. Only ids and kinds, never what was written: staff
//! read the rest in the desk, with their own grant and code.

use revolt_config::config;
use ulid::Ulid;

use crate::{Database, Message};

/// The inbox channel, when the desk has one
pub fn inbox_of(desk: &revolt_config::FeaturesStaffDesk) -> Option<(String, String)> {
    let channel = desk.inbox.trim();
    let bot = desk.bot.trim();
    (desk.enabled && !channel.is_empty() && !bot.is_empty()).then(|| (channel.to_string(), bot.to_string()))
}

/// Post one line to the staff inbox. Never fails the request that called it.
pub async fn announce(db: &Database, text: String) {
    let desk = config().await.features.staff_desk;
    let Some((channel, bot)) = inbox_of(&desk) else {
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
        assert_eq!(inbox_of(&desk), None);
        desk.enabled = true;
        desk.bot = "BOT".into();
        assert_eq!(inbox_of(&desk), None);
        desk.inbox = " CHANNEL ".into();
        assert_eq!(inbox_of(&desk), Some(("CHANNEL".into(), "BOT".into())));
        desk.enabled = false;
        assert_eq!(inbox_of(&desk), None);
    }
}
