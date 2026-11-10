use iso8601_timestamp::Timestamp;
use revolt_models::v0::ServerEventStatus;
use revolt_result::Result;

use crate::ServerEvent;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractServerEvents: Sync + Send {
    /// Insert a new event
    async fn insert_server_event(&self, event: &ServerEvent) -> Result<()>;

    /// Fetch an event by its id
    async fn fetch_server_event(&self, id: &str) -> Result<ServerEvent>;

    /// Scheduled and active events of a server, soonest first
    async fn fetch_upcoming_server_events(&self, server: &str) -> Result<Vec<ServerEvent>>;

    /// Write the fields a manager edits (never the status or the interested
    /// count, which change on their own)
    async fn update_server_event(&self, event: &ServerEvent) -> Result<()>;

    /// Move an event from one status to another in one step, noting when it
    /// started or ended. Returns false when it was not in `from` any more.
    async fn set_server_event_status(
        &self,
        id: &str,
        from: ServerEventStatus,
        to: ServerEventStatus,
        at: Timestamp,
    ) -> Result<bool>;

    /// Delete an event and its interested marks
    async fn delete_server_event(&self, id: &str) -> Result<()>;

    /// Scheduled events whose start time has come
    async fn fetch_server_events_to_start(&self, now: Timestamp) -> Result<Vec<ServerEvent>>;

    /// Active events (crond checks which of them have ended)
    async fn fetch_active_server_events(&self) -> Result<Vec<ServerEvent>>;

    /// Mark an event "Interested". Returns the new count, or None when the
    /// member had marked it already.
    async fn add_server_event_interest(
        &self,
        event: &str,
        server: &str,
        user: &str,
    ) -> Result<Option<u32>>;

    /// Unmark an event. Returns the new count, or None when it was not marked.
    async fn remove_server_event_interest(&self, event: &str, user: &str) -> Result<Option<u32>>;

    /// User ids that marked an event, by id, after `after`
    async fn fetch_server_event_interested(
        &self,
        event: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>>;

    /// Note the occurrence that follows an event
    async fn set_server_event_next(&self, id: &str, next: &str) -> Result<()>;

    /// Repeating events that are over (completed or cancelled on their own)
    /// and have no next occurrence yet
    async fn fetch_server_events_missing_next(&self) -> Result<Vec<ServerEvent>>;

    /// A member left a server: remove their marks there, lower the counts,
    /// return the events that changed
    async fn remove_server_event_interests_of(&self, server: &str, user: &str)
        -> Result<Vec<String>>;

    /// Which of these events the user marked
    async fn fetch_server_event_interests_of(
        &self,
        user: &str,
        events: &[String],
    ) -> Result<Vec<String>>;
}
