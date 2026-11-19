//! A person's private saved list (`/users/@me/saved`).
//!
//! Only the person reads their list. An entry holds the ids of a message and
//! its channel; the message itself is read on every request under the
//! permissions the person has at that moment. A message that is gone, or
//! that they may no longer read, is listed as unavailable with nothing of
//! its content, and they can still remove the entry.
//!
//! Behind `features.saved_reminders.enabled`. Not for bots: the answers
//! carry whole messages and their authors, which would pass a bot's intents.

use std::collections::HashSet;

use revolt_config::config;
use revolt_database::{
    now_ms, readable_message, readable_messages, Database, SavedMessage, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

/// Longest search text
const QUERY_MAX: usize = 100;

/// The feature is on and the caller is a person
pub(super) async fn require_open(user: &User) -> Result<()> {
    if !config().await.features.saved_reminders.enabled {
        return Err(create_error!(FeatureDisabled {
            feature: "saved_reminders".to_string()
        }));
    }
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    Ok(())
}

/// The authors of these messages, as the person may see them
pub(super) async fn authors_of(
    db: &Database,
    user: &User,
    messages: &[v0::Message],
) -> Result<Vec<v0::User>> {
    let ids: Vec<String> = messages
        .iter()
        .map(|message| message.author.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(vec![]);
    }
    User::fetch_many_ids_as_mutuals(db, user, &ids).await
}

/// Whether a message or its author matches the search text (lower case)
fn matches(message: &v0::Message, author: Option<&v0::User>, needle: &str) -> bool {
    let has = |text: &str| text.to_lowercase().contains(needle);
    message.content.as_deref().is_some_and(has)
        || message
            .attachments
            .as_ref()
            .is_some_and(|files| files.iter().any(|file| has(&file.filename)))
        || author.is_some_and(|author| {
            has(&author.username) || author.display_name.as_deref().is_some_and(has)
        })
}

/// # Fetch Saved Messages
///
/// Your private saved list, newest first. `before` continues after the
/// `saved_at` of the last entry of the page before. With `query`, only
/// entries whose text, file names or author match are returned.
#[openapi(tag = "Saved Messages")]
#[get("/@me/saved?<before>&<limit>&<query>")]
pub async fn list(
    db: &State<Database>,
    user: User,
    before: Option<i64>,
    limit: Option<usize>,
    query: Option<String>,
) -> Result<Json<v0::SavedMessagesResponse>> {
    require_open(&user).await?;

    let limit = limit.unwrap_or(50).clamp(1, 100);
    let needle = query
        .map(|text| text.trim().to_lowercase())
        .filter(|text| !text.is_empty());
    if needle.as_ref().is_some_and(|text| text.chars().count() > QUERY_MAX) {
        return Err(create_error!(FailedValidation {
            error: "the search text is too long".to_string()
        }));
    }

    let all = db.fetch_saved_messages(&user.id).await?;
    let total = all.len();
    let max = config().await.features.saved_reminders.max_saved;
    let older = |entry: &SavedMessage| before.is_none_or(|before| entry.saved_at < before);

    let (page, mut found) = match &needle {
        // without a search only the page's messages are read
        None => {
            let page: Vec<SavedMessage> =
                all.into_iter().filter(older).take(limit).collect();
            let pairs: Vec<(String, String)> = page
                .iter()
                .map(|entry| (entry.message_id.clone(), entry.channel_id.clone()))
                .collect();
            let found = readable_messages(db, &user, &pairs).await;
            (page, found)
        }
        // a search reads every message the person may still read
        Some(needle) => {
            let pairs: Vec<(String, String)> = all
                .iter()
                .map(|entry| (entry.message_id.clone(), entry.channel_id.clone()))
                .collect();
            let mut found = readable_messages(db, &user, &pairs).await;
            let models: Vec<v0::Message> = found
                .values()
                .cloned()
                .map(|message| message.into_model(None, None))
                .collect();
            let authors = authors_of(db, &user, &models).await?;
            found.retain(|_, message| {
                let model = models.iter().find(|model| model.id == message.id);
                model.is_some_and(|model| {
                    matches(
                        model,
                        authors.iter().find(|author| author.id == model.author),
                        needle,
                    )
                })
            });
            let page: Vec<SavedMessage> = all
                .into_iter()
                .filter(|entry| found.contains_key(&entry.message_id))
                .filter(older)
                .take(limit)
                .collect();
            let on_page: HashSet<&str> =
                page.iter().map(|entry| entry.message_id.as_str()).collect();
            found.retain(|id, _| on_page.contains(id.as_str()));
            (page, found)
        }
    };

    let mut messages = Vec::with_capacity(found.len());
    let entries: Vec<v0::SavedMessage> = page
        .into_iter()
        .map(|entry| match found.remove(&entry.message_id) {
            Some(message) => {
                messages.push(message.into_model(None, None));
                entry.into_model(true)
            }
            None => entry.into_model(false),
        })
        .collect();
    let users = authors_of(db, &user, &messages).await?;

    Ok(Json(v0::SavedMessagesResponse {
        entries,
        messages,
        users,
        total,
        max,
    }))
}

/// # Save Message
///
/// Put a message you can read into your private saved list. Saving it again
/// changes nothing.
#[openapi(tag = "Saved Messages")]
#[put("/@me/saved/<message_id>")]
pub async fn save(
    db: &State<Database>,
    user: User,
    message_id: String,
) -> Result<Json<v0::SavedMessage>> {
    require_open(&user).await?;

    // a message that is not there and one the person may not read give the
    // same answer
    let message = readable_message(db, &user, &message_id)
        .await
        .ok_or_else(|| create_error!(NotFound))?;

    let max = config().await.features.saved_reminders.max_saved;
    let entry = SavedMessage::new(&user.id, &message, now_ms());
    let existing = db
        .fetch_saved_messages(&user.id)
        .await?
        .into_iter()
        .fold((0, None), |(count, hit), saved| {
            let hit = if saved.id == entry.id { Some(saved) } else { hit };
            (count + 1, hit)
        });
    if let (_, Some(saved)) = existing {
        return Ok(Json(saved.into_model(true)));
    }
    if existing.0 >= max {
        return Err(create_error!(TooManySavedMessages { max }));
    }

    db.save_message(&entry).await?;
    Ok(Json(entry.into_model(true)))
}

/// # Remove Saved Message
///
/// Take a message out of your saved list. Works for entries whose message
/// is no longer available too.
#[openapi(tag = "Saved Messages")]
#[delete("/@me/saved/<message_id>")]
pub async fn unsave(
    db: &State<Database>,
    user: User,
    message_id: String,
) -> Result<EmptyResponse> {
    require_open(&user).await?;
    db.unsave_message(&user.id, &message_id).await?;
    Ok(EmptyResponse)
}
