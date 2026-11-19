//! Reminders a person sets on messages (`/users/@me/reminders`).
//!
//! A reminder holds the ids of a message and its channel and the time it is
//! due. crond delivers it: an event to the person's sessions and a push
//! notification, both read from the message at that moment under the
//! permissions the person has then. The time is an instant; the app works
//! out "tonight" and "tomorrow morning" in the person's own time zone.
//!
//! Waiting reminders are limited (`features.saved_reminders.max_active`,
//! more with Feather: `max_active_plus`). Delivered ones stay listed until
//! the person removes them or crond does after `keep_delivered_days`.

use revolt_config::config;
use revolt_database::{
    due_time_allowed, now_ms, readable_message, readable_messages, reminder_limit, Database,
    MessageReminder, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

use super::saved::{authors_of, require_open};

/// Delivered reminders one person keeps listed; older ones make room
const DELIVERED_KEPT: usize = 100;

async fn require_due_time(due_at: i64, now: i64) -> Result<()> {
    if due_time_allowed(due_at, now).await {
        Ok(())
    } else {
        Err(create_error!(FailedValidation {
            error: "the time must be in the future and within the allowed range".to_string()
        }))
    }
}

/// # Fetch Reminders
///
/// Your reminders: waiting ones soonest first, then delivered ones newest
/// first.
#[openapi(tag = "Saved Messages")]
#[get("/@me/reminders")]
pub async fn list(
    db: &State<Database>,
    user: User,
) -> Result<Json<v0::MessageRemindersResponse>> {
    require_open(&user).await?;

    let mut all = db.fetch_reminders(&user.id).await?;
    all.sort_by_key(|reminder| match reminder.delivered_at {
        None => (0, reminder.due_at),
        Some(at) => (1, -at),
    });

    let pairs: Vec<(String, String)> = all
        .iter()
        .map(|reminder| (reminder.message_id.clone(), reminder.channel_id.clone()))
        .collect();
    let found = readable_messages(db, &user, &pairs).await;

    let reminders: Vec<v0::MessageReminder> = all
        .into_iter()
        .map(|reminder| {
            let available = found.contains_key(&reminder.message_id);
            reminder.into_model(available)
        })
        .collect();
    let messages: Vec<v0::Message> = found
        .into_values()
        .map(|message| message.into_model(None, None))
        .collect();
    let users = authors_of(db, &user, &messages).await?;

    let settings = &config().await.features.saved_reminders;
    Ok(Json(v0::MessageRemindersResponse {
        reminders,
        messages,
        users,
        max_active: reminder_limit(&user).await,
        max_active_plus: settings.max_active_plus.max(settings.max_active),
    }))
}

/// # Set Reminder
///
/// Be reminded of a message you can read at a time you choose.
#[openapi(tag = "Saved Messages")]
#[post("/@me/reminders", data = "<data>")]
pub async fn create(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataCreateReminder>,
) -> Result<Json<v0::MessageReminder>> {
    require_open(&user).await?;
    let data = data.into_inner();
    let now = now_ms();
    require_due_time(data.due_at, now).await?;

    // a message that is not there and one the person may not read give the
    // same answer
    let message = readable_message(db, &user, &data.message)
        .await
        .ok_or_else(|| create_error!(NotFound))?;

    let existing = db.fetch_reminders(&user.id).await?;
    let max = reminder_limit(&user).await;
    let waiting = existing
        .iter()
        .filter(|reminder| reminder.delivered_at.is_none())
        .count();
    if waiting >= max {
        return Err(create_error!(TooManyReminders { max }));
    }

    // delivered reminders make room, oldest first
    let mut delivered: Vec<&MessageReminder> = existing
        .iter()
        .filter(|reminder| reminder.delivered_at.is_some())
        .collect();
    if delivered.len() >= DELIVERED_KEPT {
        delivered.sort_by_key(|reminder| reminder.delivered_at);
        for old in &delivered[..=delivered.len() - DELIVERED_KEPT] {
            db.delete_reminder(&user.id, &old.id).await?;
        }
    }

    let reminder = MessageReminder::new(&user.id, &message, data.due_at, now);
    db.insert_reminder(&reminder).await?;
    Ok(Json(reminder.into_model(true)))
}

/// # Move Reminder
///
/// Change the time of a reminder that still waits.
#[openapi(tag = "Saved Messages")]
#[patch("/@me/reminders/<reminder_id>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    user: User,
    reminder_id: String,
    data: Json<v0::DataEditReminder>,
) -> Result<Json<v0::MessageReminder>> {
    require_open(&user).await?;
    let data = data.into_inner();
    require_due_time(data.due_at, now_ms()).await?;

    if !db.move_reminder(&user.id, &reminder_id, data.due_at).await? {
        return Err(create_error!(NotFound));
    }
    let reminder = db.fetch_reminder(&user.id, &reminder_id).await?;
    let available = readable_message(db, &user, &reminder.message_id)
        .await
        .is_some_and(|message| message.channel == reminder.channel_id);
    Ok(Json(reminder.into_model(available)))
}

/// # Remove Reminder
///
/// Cancel a waiting reminder, or clear a delivered one from your list.
#[openapi(tag = "Saved Messages")]
#[delete("/@me/reminders/<reminder_id>")]
pub async fn delete(
    db: &State<Database>,
    user: User,
    reminder_id: String,
) -> Result<EmptyResponse> {
    require_open(&user).await?;
    if !db.delete_reminder(&user.id, &reminder_id).await? {
        return Err(create_error!(NotFound));
    }
    Ok(EmptyResponse)
}
