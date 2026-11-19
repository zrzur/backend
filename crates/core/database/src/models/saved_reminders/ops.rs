use revolt_result::Result;

use crate::{MessageReminder, SavedMessage};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractSavedReminders: Sync + Send {
    /// Save a message for a person. Saving it again keeps the first entry.
    /// Returns whether a new entry was written.
    async fn save_message(&self, entry: &SavedMessage) -> Result<bool>;

    /// Returns whether there was an entry to remove
    async fn unsave_message(&self, user_id: &str, message_id: &str) -> Result<bool>;

    /// Everything a person saved, newest first
    async fn fetch_saved_messages(&self, user_id: &str) -> Result<Vec<SavedMessage>>;

    async fn count_saved_messages(&self, user_id: &str) -> Result<usize>;

    async fn delete_saved_messages_of(&self, user_id: &str) -> Result<()>;

    async fn insert_reminder(&self, reminder: &MessageReminder) -> Result<()>;

    /// A person's own reminder; `NotFound` for anyone else's
    async fn fetch_reminder(&self, user_id: &str, id: &str) -> Result<MessageReminder>;

    /// Every reminder of a person, waiting and delivered
    async fn fetch_reminders(&self, user_id: &str) -> Result<Vec<MessageReminder>>;

    /// Reminders of a person that still wait
    async fn count_waiting_reminders(&self, user_id: &str) -> Result<usize>;

    /// Move a reminder that still waits. Returns whether one was moved.
    async fn move_reminder(&self, user_id: &str, id: &str, due_at: i64) -> Result<bool>;

    /// Returns whether there was a reminder to remove
    async fn delete_reminder(&self, user_id: &str, id: &str) -> Result<bool>;

    async fn delete_reminders_of(&self, user_id: &str) -> Result<()>;

    /// Waiting reminders due at `now`, the longest overdue first
    async fn fetch_due_reminders(&self, now: i64, limit: i64) -> Result<Vec<MessageReminder>>;

    /// Mark a waiting reminder delivered. Returns whether this call did it:
    /// of several callers at once, exactly one gets `true`.
    async fn claim_reminder(&self, id: &str, now: i64) -> Result<bool>;

    /// Remove reminders delivered before `cutoff`. Returns how many.
    async fn delete_reminders_delivered_before(&self, cutoff: i64) -> Result<u64>;
}
