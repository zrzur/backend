use revolt_result::Result;

use crate::ReferenceDb;
use crate::{MessageReminder, SavedMessage};

use super::AbstractSavedReminders;

#[async_trait]
impl AbstractSavedReminders for ReferenceDb {
    async fn save_message(&self, entry: &SavedMessage) -> Result<bool> {
        let mut saved = self.saved_messages.lock().await;
        if saved.contains_key(&entry.id) {
            return Ok(false);
        }
        saved.insert(entry.id.clone(), entry.clone());
        Ok(true)
    }

    async fn unsave_message(&self, user_id: &str, message_id: &str) -> Result<bool> {
        Ok(self
            .saved_messages
            .lock()
            .await
            .remove(&SavedMessage::key(user_id, message_id))
            .is_some())
    }

    async fn fetch_saved_messages(&self, user_id: &str) -> Result<Vec<SavedMessage>> {
        let mut list: Vec<_> = self
            .saved_messages
            .lock()
            .await
            .values()
            .filter(|entry| entry.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by(|a, b| (b.saved_at, &b.id).cmp(&(a.saved_at, &a.id)));
        Ok(list)
    }

    async fn count_saved_messages(&self, user_id: &str) -> Result<usize> {
        Ok(self
            .saved_messages
            .lock()
            .await
            .values()
            .filter(|entry| entry.user_id == user_id)
            .count())
    }

    async fn delete_saved_messages_of(&self, user_id: &str) -> Result<()> {
        self.saved_messages
            .lock()
            .await
            .retain(|_, entry| entry.user_id != user_id);
        Ok(())
    }

    async fn insert_reminder(&self, reminder: &MessageReminder) -> Result<()> {
        let mut reminders = self.message_reminders.lock().await;
        if reminders.contains_key(&reminder.id) {
            return Err(create_database_error!("insert", "message_reminders"));
        }
        reminders.insert(reminder.id.clone(), reminder.clone());
        Ok(())
    }

    async fn fetch_reminder(&self, user_id: &str, id: &str) -> Result<MessageReminder> {
        self.message_reminders
            .lock()
            .await
            .get(id)
            .filter(|reminder| reminder.user_id == user_id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_reminders(&self, user_id: &str) -> Result<Vec<MessageReminder>> {
        let mut list: Vec<_> = self
            .message_reminders
            .lock()
            .await
            .values()
            .filter(|reminder| reminder.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by_key(|reminder| reminder.due_at);
        Ok(list)
    }

    async fn count_waiting_reminders(&self, user_id: &str) -> Result<usize> {
        Ok(self
            .message_reminders
            .lock()
            .await
            .values()
            .filter(|reminder| reminder.user_id == user_id && reminder.delivered_at.is_none())
            .count())
    }

    async fn move_reminder(&self, user_id: &str, id: &str, due_at: i64) -> Result<bool> {
        match self.message_reminders.lock().await.get_mut(id) {
            Some(reminder) if reminder.user_id == user_id && reminder.delivered_at.is_none() => {
                reminder.due_at = due_at;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete_reminder(&self, user_id: &str, id: &str) -> Result<bool> {
        let mut reminders = self.message_reminders.lock().await;
        if reminders.get(id).is_some_and(|r| r.user_id == user_id) {
            reminders.remove(id);
            return Ok(true);
        }
        Ok(false)
    }

    async fn delete_reminders_of(&self, user_id: &str) -> Result<()> {
        self.message_reminders
            .lock()
            .await
            .retain(|_, reminder| reminder.user_id != user_id);
        Ok(())
    }

    async fn fetch_due_reminders(&self, now: i64, limit: i64) -> Result<Vec<MessageReminder>> {
        let mut list: Vec<_> = self
            .message_reminders
            .lock()
            .await
            .values()
            .filter(|reminder| reminder.delivered_at.is_none() && reminder.due_at <= now)
            .cloned()
            .collect();
        list.sort_by_key(|reminder| reminder.due_at);
        list.truncate(limit.max(0) as usize);
        Ok(list)
    }

    async fn claim_reminder(&self, id: &str, now: i64) -> Result<bool> {
        match self.message_reminders.lock().await.get_mut(id) {
            Some(reminder) if reminder.delivered_at.is_none() => {
                reminder.delivered_at = Some(now);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete_reminders_delivered_before(&self, cutoff: i64) -> Result<u64> {
        let mut reminders = self.message_reminders.lock().await;
        let before = reminders.len();
        reminders.retain(|_, reminder| reminder.delivered_at.is_none_or(|at| at >= cutoff));
        Ok((before - reminders.len()) as u64)
    }
}
