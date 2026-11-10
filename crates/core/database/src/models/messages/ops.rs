use std::collections::HashMap;
use std::time::SystemTime;
use iso8601_timestamp::Timestamp;
use revolt_result::Result;

use crate::{AppendMessage, FieldsMessage, Message, MessageQuery, PartialMessage};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractMessages: Sync + Send {
    /// Insert a new message into the database
    async fn insert_message(&self, message: &Message) -> Result<()>;

    /// Fetch a message by its id
    async fn fetch_message(&self, id: &str) -> Result<Message>;

    /// Fetch multiple messages by given query
    async fn fetch_messages(&self, query: MessageQuery) -> Result<Vec<Message>>;

    /// Fetch multiple messages by given IDs
    async fn fetch_messages_by_id(&self, ids: &[String]) -> Result<Vec<Message>>;

    /// Fetch messages that mention a user (by name, by one of their roles, or
    /// @everyone / @here) in the given channels, from `since` (a message id)
    /// onwards, newest first, leaving out the user's own messages
    async fn fetch_mentions(
        &self,
        user_id: &str,
        role_ids: &[String],
        channel_ids: &[String],
        since: &str,
        limit: i64,
    ) -> Result<Vec<Message>>;

    /// Update a given message with new information
    async fn update_message(&self, id: &str, message: &PartialMessage, remove: Vec<FieldsMessage>) -> Result<()>;

    /// Append information to a given message
    async fn append_message(&self, id: &str, append: &AppendMessage) -> Result<()>;

    /// Add a new reaction to a message
    async fn add_reaction(&self, id: &str, emoji: &str, user: &str) -> Result<()>;

    /// Remove a reaction from a message
    async fn remove_reaction(&self, id: &str, emoji: &str, user: &str) -> Result<()>;

    /// Remove reaction from a message
    async fn clear_reaction(&self, id: &str, emoji: &str) -> Result<()>;

    /// Make `answers` the user's whole choice on the poll of a message: they
    /// are added to these answers and taken off every other one of the
    /// `answer_count`. Does nothing to a poll that has ended.
    async fn set_poll_choice(
        &self,
        id: &str,
        user: &str,
        answers: &[u32],
        answer_count: usize,
    ) -> Result<()>;

    /// Close the poll of a message; true only for the call that closed it
    async fn end_poll(&self, id: &str) -> Result<bool>;

    /// Messages whose poll is still open although `expires_at` is at or
    /// before `before`, oldest end first
    async fn fetch_expired_open_polls(&self, before: Timestamp, limit: i64) -> Result<Vec<Message>>;

    /// Delete a message from the database by its id
    async fn delete_message(&self, id: &str) -> Result<()>;

    /// Delete messages from a channel by their ids and corresponding channel id
    async fn delete_messages(&self, channel: &str, ids: &[String]) -> Result<()>;

    /// Delete all messages from a specific author in a server from a certain ULID onwards
    async fn delete_messages_by_author_since(
        &self,
        channels: &[String],
        author: &str,
        since: SystemTime
    ) -> Result<HashMap<String, Vec<String>>>;

    async fn delete_messages_by_user(&self, user_id: &str) -> Result<()>;

    /// One page of an author's messages that carry files (attachments or
    /// embeds), oldest first, so their files can be removed before the
    /// messages: optionally only in `channels`, only from the id `since`
    /// onwards, and only after the id `after` (the last one of the page
    /// before)
    async fn fetch_messages_with_files(
        &self,
        author: &str,
        channels: Option<&[String]>,
        since: Option<&str>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Message>>;
}
