use crate::{
    AppendMessage, FieldsMessage, Message, MessageFilter, MessageQuery, MessageTimePeriod,
    Metadata, PartialMessage, ReferenceDb,
};
use revolt_models::v0::{MessageHas, MessageSort};
use futures::future::try_join_all;
use indexmap::IndexSet;
use revolt_result::Result;
use std::collections::HashMap;
use std::time::SystemTime;
use ulid::Ulid;

use super::AbstractMessages;

/// Whether a message passes every filter given, as the MongoDB query does
fn matches_filter(filter: &MessageFilter, message: &Message) -> bool {
    if let Some(channel) = &filter.channel {
        if &message.channel != channel {
            return false;
        }
    } else if let Some(channels) = &filter.channels {
        if !channels.contains(&message.channel) {
            return false;
        }
    }

    let authors: Vec<&String> = filter
        .author
        .iter()
        .chain(filter.authors.iter().flatten())
        .collect();
    if !authors.is_empty() && !authors.contains(&&message.author) {
        return false;
    }

    if let Some(query) = &filter.query {
        let Some(content) = &message.content else {
            return false;
        };
        if !content.to_lowercase().contains(&query.to_lowercase()) {
            return false;
        }
    }

    if let Some(pinned) = filter.pinned {
        if message.pinned.unwrap_or_default() != pinned {
            return false;
        }
    }

    if let Some(user) = &filter.mentions {
        if !message.mentions.iter().flatten().any(|id| id == user) {
            return false;
        }
    }

    let attachment = |wanted: fn(&Metadata) -> bool| {
        message
            .attachments
            .iter()
            .flatten()
            .any(|file| wanted(&file.metadata))
    };
    filter.has.iter().flatten().all(|kind| match kind {
        MessageHas::Link => message.content.as_ref().is_some_and(|content| {
            let content = content.to_lowercase();
            content.contains("http://") || content.contains("https://")
        }),
        MessageHas::File => message.attachments.as_ref().is_some_and(|a| !a.is_empty()),
        MessageHas::Image => attachment(|m| matches!(m, Metadata::Image { .. })),
        MessageHas::Video => attachment(|m| matches!(m, Metadata::Video { .. })),
        MessageHas::Sound => attachment(|m| matches!(m, Metadata::Audio)),
        MessageHas::Embed => message.embeds.as_ref().is_some_and(|e| !e.is_empty()),
    })
}

#[async_trait]
impl AbstractMessages for ReferenceDb {
    /// Insert a new message into the database
    async fn insert_message(&self, message: &Message) -> Result<()> {
        let mut messages = self.messages.lock().await;
        if messages.contains_key(&message.id) {
            Err(create_database_error!("insert", "message"))
        } else {
            messages.insert(message.id.to_string(), message.clone());
            Ok(())
        }
    }

    /// Fetch a message by its id
    async fn fetch_message(&self, id: &str) -> Result<Message> {
        let messages = self.messages.lock().await;
        messages
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch multiple messages by given query
    async fn fetch_messages(&self, query: MessageQuery) -> Result<Vec<Message>> {
        let messages = self.messages.lock().await;
        let filter = &query.filter;
        let matched: Vec<&Message> = messages
            .values()
            .filter(|message| matches_filter(filter, message))
            .collect();

        let limit = query.limit.unwrap_or(50).max(0) as usize;
        let newest_first = |list: &mut Vec<&Message>| list.sort_by(|a, b| b.id.cmp(&a.id));
        let oldest_first = |list: &mut Vec<&Message>| list.sort_by(|a, b| a.id.cmp(&b.id));

        // the same windows and order as MongoDB
        let found: Vec<&Message> = match query.time_period {
            MessageTimePeriod::Relative { nearby } => {
                let mut newer: Vec<&Message> =
                    matched.iter().copied().filter(|m| m.id >= nearby).collect();
                let mut older: Vec<&Message> =
                    matched.iter().copied().filter(|m| m.id < nearby).collect();
                oldest_first(&mut newer);
                newest_first(&mut older);
                newer.truncate(limit / 2 + 1);
                older.truncate(limit / 2 + 1);
                newer.into_iter().chain(older).collect()
            }
            MessageTimePeriod::Absolute {
                before,
                after,
                sort,
            } => {
                let mut list: Vec<&Message> = matched
                    .into_iter()
                    .filter(|m| before.as_ref().is_none_or(|before| &m.id < before))
                    .filter(|m| after.as_ref().is_none_or(|after| &m.id > after))
                    .collect();
                match sort.unwrap_or(MessageSort::Latest) {
                    // no text score here: relevance is newest first
                    MessageSort::Relevance | MessageSort::Latest => newest_first(&mut list),
                    MessageSort::Oldest => oldest_first(&mut list),
                }
                list.truncate(limit);
                list
            }
        };

        Ok(found.into_iter().cloned().collect())
    }

    /// Fetch multiple messages by given IDs
    async fn fetch_messages_by_id(&self, ids: &[String]) -> Result<Vec<Message>> {
        try_join_all(ids.iter().map(|id| self.fetch_message(id))).await
    }

    /// Fetch messages that mention a user
    async fn fetch_mentions(
        &self,
        user_id: &str,
        role_ids: &[String],
        channel_ids: &[String],
        since: &str,
        limit: i64,
    ) -> Result<Vec<Message>> {
        let messages = self.messages.lock().await;
        let mut found: Vec<Message> = messages
            .values()
            .filter(|message| {
                channel_ids.contains(&message.channel)
                    && message.id.as_str() >= since
                    && message.author != user_id
                    && (message
                        .mentions
                        .as_ref()
                        .is_some_and(|mentions| mentions.iter().any(|id| id == user_id))
                        || message.role_mentions.as_ref().is_some_and(|roles| {
                            roles.iter().any(|role| role_ids.contains(role))
                        })
                        || message
                            .flags
                            .is_some_and(|flags| flags & ((1 << 2) | (1 << 3)) != 0))
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| b.id.cmp(&a.id));
        found.truncate(limit.max(0) as usize);
        Ok(found)
    }

    /// Update a given message with new information
    async fn update_message(
        &self,
        id: &str,
        message: &PartialMessage,
        remove: Vec<FieldsMessage>,
    ) -> Result<()> {
        let mut messages = self.messages.lock().await;
        if let Some(message_data) = messages.get_mut(id) {
            message_data.apply_options(message.to_owned());

            for field in remove {
                #[allow(clippy::disallowed_methods)]
                message_data.remove_field(&field);
            }
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Append information to a given message
    async fn append_message(&self, id: &str, append: &AppendMessage) -> Result<()> {
        let mut messages = self.messages.lock().await;
        if let Some(message_data) = messages.get_mut(id) {
            if let Some(embeds) = &append.embeds {
                if !embeds.is_empty() {
                    if let Some(embeds_data) = &mut message_data.embeds {
                        embeds_data.extend(embeds.clone());
                    } else {
                        message_data.embeds = Some(embeds.clone());
                    }
                }
            }

            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Add a new reaction to a message
    async fn add_reaction(&self, id: &str, emoji: &str, user: &str) -> Result<()> {
        let mut messages = self.messages.lock().await;
        if let Some(message) = messages.get_mut(id) {
            if let Some(users) = message.reactions.get_mut(emoji) {
                users.insert(user.to_string());
            } else {
                message
                    .reactions
                    .insert(emoji.to_string(), IndexSet::from([user.to_string()]));
            }

            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Remove a reaction from a message
    async fn remove_reaction(&self, id: &str, emoji: &str, user: &str) -> Result<()> {
        let mut messages = self.messages.lock().await;
        if let Some(message) = messages.get_mut(id) {
            if let Some(users) = message.reactions.get_mut(emoji) {
                users.swap_remove(&user.to_string());
            }

            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Remove reaction from a message
    async fn clear_reaction(&self, id: &str, emoji: &str) -> Result<()> {
        let mut messages = self.messages.lock().await;
        if let Some(message) = messages.get_mut(id) {
            message.reactions.swap_remove(emoji);
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Delete a message from the database by its id
    async fn delete_message(&self, id: &str) -> Result<()> {
        let mut messages = self.messages.lock().await;
        if messages.remove(id).is_some() {
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Delete messages from a channel by their ids and corresponding channel id
    async fn delete_messages(&self, channel: &str, ids: &[String]) -> Result<()> {
        // keep everything but the named messages of this channel (this kept
        // only other channels before, deleting all of this one)
        self.messages
            .lock()
            .await
            .retain(|id, message| message.channel != channel || !ids.contains(id));

        Ok(())
    }

    /// Delete all messages from a specific author in a list of channels from a certain ULID onwards
    async fn delete_messages_by_author_since(
        &self,
        channels: &[String],
        author: &str,
        since: SystemTime,
    ) -> Result<HashMap<String, Vec<String>>> {
        let threshold_ulid = Ulid::from_datetime(since).to_string();
        let mut deleted_messages: HashMap<String, Vec<String>> = HashMap::new();
        let mut attachment_ids: Vec<String> = Vec::new();

        let messages = self.messages.lock().await;

        // First pass: collect attachment IDs and message IDs to delete
        for (id, message) in messages.iter() {
            let should_delete = message.author == author
                && channels.contains(&message.channel)
                && id.as_str() >= threshold_ulid.as_str();

            if should_delete {
                // Collect attachment IDs
                if let Some(attachments) = &message.attachments {
                    for attachment in attachments {
                        attachment_ids.push(attachment.id.clone());
                    }
                }

                deleted_messages
                    .entry(message.channel.clone())
                    .or_default()
                    .push(id.clone());
            }
        }
        drop(messages);

        // Mark attachments as deleted
        if !attachment_ids.is_empty() {
            let mut files = self.files.lock().await;
            for attachment_id in attachment_ids {
                if let Some(file) = files.get_mut(&attachment_id) {
                    file.deleted = Some(true);
                }
            }
        }

        // Delete the messages
        self.messages.lock().await.retain(|id, message| {
            let should_keep = !(message.author == author
                && channels.contains(&message.channel)
                && id.as_str() >= threshold_ulid.as_str());
            should_keep
        });

        Ok(deleted_messages)
    }

    async fn delete_messages_by_user(&self, user_id: &str) -> Result<()> {
        let mut messages = self.messages.lock().await;

        // files are removed before this through their messages
        // (Message::remove_files)
        messages.retain(|_, message| message.author != user_id);

        Ok(())
    }

    async fn fetch_messages_with_files(
        &self,
        author: &str,
        channels: Option<&[String]>,
        since: Option<&str>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let mut page: Vec<Message> = self
            .messages
            .lock()
            .await
            .values()
            .filter(|message| {
                message.author == author
                    && (message.attachments.as_ref().is_some_and(|files| !files.is_empty())
                        || message.embeds.as_ref().is_some_and(|embeds| !embeds.is_empty()))
                    && channels.is_none_or(|channels| channels.contains(&message.channel))
                    && since.is_none_or(|since| message.id.as_str() >= since)
                    && after.is_none_or(|after| message.id.as_str() > after)
            })
            .cloned()
            .collect();
        page.sort_by(|a, b| a.id.cmp(&b.id));
        page.truncate(limit);
        Ok(page)
    }
}
