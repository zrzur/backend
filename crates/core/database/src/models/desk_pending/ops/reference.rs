use iso8601_timestamp::Timestamp;
use revolt_result::Result;

use crate::{DeskPending, ReferenceDb};

use super::AbstractDeskPending;

#[async_trait]
impl AbstractDeskPending for ReferenceDb {
    async fn insert_desk_pending(&self, pending: &DeskPending) -> Result<()> {
        let mut all = self.desk_pending.lock().await;
        if all.contains_key(&pending.id) {
            return Err(create_database_error!("insert_one", "desk_pending"));
        }
        all.insert(pending.id.clone(), pending.clone());
        Ok(())
    }

    async fn fetch_desk_pending(&self, id: &str) -> Result<Option<DeskPending>> {
        Ok(self.desk_pending.lock().await.get(id).cloned())
    }

    async fn bind_desk_pending_card(&self, id: &str, message_id: &str) -> Result<bool> {
        let mut all = self.desk_pending.lock().await;
        let Some(pending) = all.get_mut(id) else {
            return Ok(false);
        };
        match pending.card_message.as_deref() {
            None => {
                pending.card_message = Some(message_id.to_string());
                Ok(true)
            }
            Some(bound) => Ok(bound == message_id),
        }
    }

    async fn claim_desk_pending(&self, id: &str, now: Timestamp) -> Result<bool> {
        let mut all = self.desk_pending.lock().await;
        match all.get_mut(id) {
            Some(pending) if !pending.used && pending.expires_at > now => {
                pending.used = true;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn release_desk_pending(&self, id: &str) -> Result<()> {
        if let Some(pending) = self.desk_pending.lock().await.get_mut(id) {
            pending.used = false;
        }
        Ok(())
    }
}
