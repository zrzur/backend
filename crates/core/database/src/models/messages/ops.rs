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

    /// Add the user to one answer of a message's poll; with `single`, also
    /// take them off every other of the `answer_count` answers. Only while
    /// the poll is open (not ended, `expires_at` after `now`), in one write.
    /// Returns the message after the change, or None when nothing matched
    /// (the poll closed meanwhile).
    async fn add_poll_vote(
        &self,
        id: &str,
        user: &str,
        answer: u32,
        single: bool,
        answer_count: usize,
        now: Timestamp,
    ) -> Result<Option<Message>>;

    /// Take the user off one answer of an open poll; None when it closed
    async fn remove_poll_vote(
        &self,
        id: &str,
        user: &str,
        answer: u32,
        now: Timestamp,
    ) -> Result<Option<Message>>;

    /// Close the poll of a message at `now`; true only for the call that
    /// closed it
    async fn end_poll(&self, id: &str, now: Timestamp) -> Result<bool>;

    /// Note that the "poll ended" reply of a message's poll is posted
    async fn mark_poll_replied(&self, id: &str) -> Result<()>;

    /// Polls crond still has to finish: no reply posted yet, and either
    /// closed or past `expires_at` at `before`; oldest end first
    async fn fetch_polls_to_close(&self, before: Timestamp, limit: i64) -> Result<Vec<Message>>;

    /// Messages with a poll the user voted on (data export), newest first
    async fn fetch_messages_voted_by(&self, user: &str, limit: i64) -> Result<Vec<Message>>;

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
