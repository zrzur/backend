//! Saved messages and message reminders.
//!
//! Both are private to one person. A saved entry and a reminder hold ids
//! only (message, channel), never a copy of the message: what the person
//! sees is read from the message at that moment, under the permissions they
//! have at that moment. A message that is gone, or that they may no longer
//! read, comes back as "unavailable" with nothing of its content.
//!
//! crond delivers reminders (`deliver_due_reminders`): it claims each due
//! reminder with one conditional write, so two crond processes never deliver
//! the same reminder twice, then tells the person's sessions and queues a
//! push notification.

use std::collections::{HashMap, HashSet};

use revolt_config::config;
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::Result;
use ulid::Ulid;

use crate::events::client::EventV1;
use crate::util::permissions::DatabasePermissionQuery;
use crate::{Channel, Database, Message, User};

auto_derived!(
    /// A message a person saved (`saved_messages`)
    pub struct SavedMessage {
        /// `{user_id}:{message_id}`: one entry per person and message
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub message_id: String,
        pub channel_id: String,
        /// Milliseconds since the epoch
        pub saved_at: i64,
    }

    /// A reminder a person set on a message (`message_reminders`)
    pub struct MessageReminder {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub message_id: String,
        pub channel_id: String,
        /// Milliseconds since the epoch
        pub due_at: i64,
        pub created_at: i64,
        /// Set by crond when it delivers; waiting while `None`
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub delivered_at: Option<i64>,
    }
);

/// Milliseconds since the epoch
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|now| now.as_millis() as i64)
        .unwrap_or_default()
}

impl SavedMessage {
    pub fn key(user_id: &str, message_id: &str) -> String {
        format!("{user_id}:{message_id}")
    }

    pub fn new(user_id: &str, message: &Message, now: i64) -> Self {
        Self {
            id: Self::key(user_id, &message.id),
            user_id: user_id.to_string(),
            message_id: message.id.clone(),
            channel_id: message.channel.clone(),
            saved_at: now,
        }
    }

    pub fn into_model(self, available: bool) -> v0::SavedMessage {
        v0::SavedMessage {
            message: self.message_id,
            channel: self.channel_id,
            saved_at: self.saved_at,
            available,
        }
    }
}

impl MessageReminder {
    pub fn new(user_id: &str, message: &Message, due_at: i64, now: i64) -> Self {
        Self {
            id: Ulid::new().to_string(),
            user_id: user_id.to_string(),
            message_id: message.id.clone(),
            channel_id: message.channel.clone(),
            due_at,
            created_at: now,
            delivered_at: None,
        }
    }

    pub fn into_model(self, available: bool) -> v0::MessageReminder {
        v0::MessageReminder {
            id: self.id,
            message: self.message_id,
            channel: self.channel_id,
            due_at: self.due_at,
            created_at: self.created_at,
            delivered_at: self.delivered_at,
            available,
        }
    }
}

/// How many reminders this person may have waiting at once
pub async fn reminder_limit(user: &User) -> usize {
    let settings = &config().await.features.saved_reminders;
    if user.has_plus().await {
        settings.max_active_plus.max(settings.max_active)
    } else {
        settings.max_active
    }
}

/// Whether `due_at` is a time a reminder may be set for: later than now
/// (with a few seconds of slack for the trip here) and not further ahead
/// than the setting allows
pub async fn due_time_allowed(due_at: i64, now: i64) -> bool {
    let days = config().await.features.saved_reminders.max_days_ahead as i64;
    due_at > now - 5_000 && due_at <= now + days * 24 * 60 * 60 * 1000
}

/// Whether this person may read the messages of this channel right now:
/// they can view it and its history, and it is not a mature place closed to
/// them. Any failure on the way counts as "no".
pub async fn can_read_channel(db: &Database, user: &User, channel: &Channel) -> bool {
    if crate::util::age::assert_can_view_channel(db, user, channel)
        .await
        .is_err()
    {
        return false;
    }
    let mut query = DatabasePermissionQuery::new(db, user).channel(channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.has_channel_permission(ChannelPermission::ViewChannel)
        && permissions.has_channel_permission(ChannelPermission::ReadMessageHistory)
}

/// The message, if it exists and this person may read it right now
pub async fn readable_message(db: &Database, user: &User, message_id: &str) -> Option<Message> {
    let message = db.fetch_message(message_id).await.ok()?;
    let channel = db.fetch_channel(&message.channel).await.ok()?;
    can_read_channel(db, user, &channel)
        .await
        .then_some(message)
}

/// Of the given `(message id, channel id)` pairs, the messages that exist
/// and that this person may read right now, by id.
pub async fn readable_messages(
    db: &Database,
    user: &User,
    pairs: &[(String, String)],
) -> HashMap<String, Message> {
    // channels first: a channel that is closed costs no message read
    let mut open: HashMap<String, bool> = HashMap::new();
    for (_, channel_id) in pairs {
        if open.contains_key(channel_id) {
            continue;
        }
        let readable = match db.fetch_channel(channel_id).await {
            Ok(channel) => can_read_channel(db, user, &channel).await,
            Err(_) => false,
        };
        open.insert(channel_id.clone(), readable);
    }

    let wanted: Vec<String> = pairs
        .iter()
        .filter(|(_, channel)| open.get(channel).copied().unwrap_or(false))
        .map(|(message, _)| message.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if wanted.is_empty() {
        return HashMap::new();
    }

    // one query when every message is still there; one by one when the
    // bulk read stops at a missing message
    let found = match db.fetch_messages_by_id(&wanted).await {
        Ok(found) => found,
        Err(_) => {
            let mut found = Vec::with_capacity(wanted.len());
            for id in &wanted {
                if let Ok(message) = db.fetch_message(id).await {
                    found.push(message);
                }
            }
            found
        }
    };

    let expected: HashMap<&str, &str> = pairs
        .iter()
        .map(|(message, channel)| (message.as_str(), channel.as_str()))
        .collect();
    found
        .into_iter()
        // the check above ran on the stored channel, so the message must
        // really be in that channel
        .filter(|message| expected.get(message.id.as_str()) == Some(&message.channel.as_str()))
        .map(|message| (message.id.clone(), message))
        .collect()
}

/// What one crond pass did
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReminderPassReport {
    pub delivered: usize,
    /// Of those, delivered without the message: gone, or closed to the person
    pub unavailable: usize,
    /// Old delivered reminders removed
    pub removed: u64,
    pub failed: usize,
}

/// Deliver every reminder that is due at `now` and remove delivered ones
/// older than the setting keeps them. One reminder failing is logged and the
/// others go on.
pub async fn deliver_due_reminders(db: &Database, now: i64) -> Result<ReminderPassReport> {
    let mut report = ReminderPassReport::default();

    loop {
        let due = db.fetch_due_reminders(now, 100).await?;
        let count = due.len();
        let mut claimed = 0;
        for reminder in due {
            // only the process whose write lands delivers it
            match db.claim_reminder(&reminder.id, now).await {
                Ok(true) => claimed += 1,
                Ok(false) => continue,
                Err(error) => {
                    log::error!("Reminder {} not claimed: {error:?}", reminder.id);
                    report.failed += 1;
                    continue;
                }
            }
            report.delivered += 1;
            if !deliver(db, reminder, now).await {
                report.unavailable += 1;
            }
        }
        // a full page that gave nothing to claim would come back the same
        if count < 100 || claimed == 0 {
            break;
        }
    }

    let keep_days = config().await.features.saved_reminders.keep_delivered_days as i64;
    report.removed = db
        .delete_reminders_delivered_before(now - keep_days * 24 * 60 * 60 * 1000)
        .await?;

    Ok(report)
}

/// Tell the person. Returns whether the message could be shown to them.
async fn deliver(db: &Database, mut reminder: MessageReminder, now: i64) -> bool {
    reminder.delivered_at = Some(now);

    let Ok(user) = db.fetch_user(&reminder.user_id).await else {
        return false;
    };
    let message = match db.fetch_message(&reminder.message_id).await {
        Ok(message) if message.channel == reminder.channel_id => Some(message),
        _ => None,
    };
    let channel = match &message {
        Some(_) => db.fetch_channel(&reminder.channel_id).await.ok(),
        None => None,
    };
    let readable = match (&message, &channel) {
        (Some(_), Some(channel)) => can_read_channel(db, &user, channel).await,
        _ => false,
    };

    let (message, channel) = match (readable, message, channel) {
        (true, Some(message), Some(channel)) => (message, channel),
        _ => {
            // nothing of the message leaves: the app shows "unavailable"
            EventV1::MessageReminderDue(v0::MessageReminderDue {
                reminder: reminder.into_model(false),
                message: None,
                user: None,
            })
            .p(user.id.clone())
            .await;
            return false;
        }
    };

    let author = match db.fetch_user(&message.author).await {
        Ok(author) => Some(author.into(db, Some(&user)).await),
        Err(_) => None,
    };
    let model = message.into_model(None, None);

    EventV1::MessageReminderDue(v0::MessageReminderDue {
        reminder: reminder.clone().into_model(true),
        message: Some(model.clone()),
        user: author.clone(),
    })
    .p(user.id.clone())
    .await;

    // the push is the message's own notification under a tag of its own, so
    // it opens the message and does not replace the channel's notification
    let mut notification = v0::PushNotification::from(
        model,
        author.as_ref().map(v0::MessageAuthor::User),
        channel.into(),
    )
    .await;
    notification.tag = format!("reminder-{}", reminder.id);
    if let Err(error) = crate::amqp::get_amqp()
        .message_sent(vec![user.id.clone()], notification)
        .await
    {
        log::warn!(
            "Could not queue the push of reminder {}: {error:?}",
            reminder.id
        );
    }
    true
}
