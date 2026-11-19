use bson::{to_document, Document};
use futures::StreamExt;
use mongodb::options::{FindOptions, UpdateOptions};
use revolt_result::Result;

use crate::MongoDb;
use crate::{MessageReminder, SavedMessage};

use super::AbstractSavedReminders;

static SAVED: &str = "saved_messages";
static REMINDERS: &str = "message_reminders";

impl MongoDb {
    async fn find_reminders(
        &self,
        filter: Document,
        sort: Document,
        limit: Option<i64>,
    ) -> Result<Vec<MessageReminder>> {
        let options = FindOptions::builder().sort(sort).limit(limit).build();
        let mut cursor = self
            .col::<MessageReminder>(REMINDERS)
            .find(filter)
            .with_options(options)
            .await
            .map_err(|_| create_database_error!("find", REMINDERS))?;
        let mut found = vec![];
        while let Some(reminder) = cursor.next().await {
            found.push(reminder.map_err(|_| create_database_error!("find", REMINDERS))?);
        }
        Ok(found)
    }
}

#[async_trait]
impl AbstractSavedReminders for MongoDb {
    async fn save_message(&self, entry: &SavedMessage) -> Result<bool> {
        let document =
            to_document(entry).map_err(|_| create_database_error!("to_document", SAVED))?;
        let result = self
            .col::<SavedMessage>(SAVED)
            .update_one(doc! { "_id": &entry.id }, doc! { "$setOnInsert": document })
            .with_options(UpdateOptions::builder().upsert(true).build())
            .await
            .map_err(|_| create_database_error!("update_one", SAVED))?;
        Ok(result.upserted_id.is_some())
    }

    async fn unsave_message(&self, user_id: &str, message_id: &str) -> Result<bool> {
        self.col::<SavedMessage>(SAVED)
            .delete_one(doc! {
                "_id": SavedMessage::key(user_id, message_id),
                "user_id": user_id,
            })
            .await
            .map(|result| result.deleted_count > 0)
            .map_err(|_| create_database_error!("delete_one", SAVED))
    }

    async fn fetch_saved_messages(&self, user_id: &str) -> Result<Vec<SavedMessage>> {
        let options = FindOptions::builder()
            .sort(doc! { "saved_at": -1_i32, "_id": -1_i32 })
            .build();
        let mut cursor = self
            .col::<SavedMessage>(SAVED)
            .find(doc! { "user_id": user_id })
            .with_options(options)
            .await
            .map_err(|_| create_database_error!("find", SAVED))?;
        let mut found = vec![];
        while let Some(entry) = cursor.next().await {
            found.push(entry.map_err(|_| create_database_error!("find", SAVED))?);
        }
        Ok(found)
    }

    async fn count_saved_messages(&self, user_id: &str) -> Result<usize> {
        self.col::<SavedMessage>(SAVED)
            .count_documents(doc! { "user_id": user_id })
            .await
            .map(|count| count as usize)
            .map_err(|_| create_database_error!("count_documents", SAVED))
    }

    async fn delete_saved_messages_of(&self, user_id: &str) -> Result<()> {
        self.col::<SavedMessage>(SAVED)
            .delete_many(doc! { "user_id": user_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", SAVED))
    }

    async fn insert_reminder(&self, reminder: &MessageReminder) -> Result<()> {
        self.col::<MessageReminder>(REMINDERS)
            .insert_one(reminder)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("insert_one", REMINDERS))
    }

    async fn fetch_reminder(&self, user_id: &str, id: &str) -> Result<MessageReminder> {
        self.col::<MessageReminder>(REMINDERS)
            .find_one(doc! { "_id": id, "user_id": user_id })
            .await
            .map_err(|_| create_database_error!("find_one", REMINDERS))?
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_reminders(&self, user_id: &str) -> Result<Vec<MessageReminder>> {
        self.find_reminders(doc! { "user_id": user_id }, doc! { "due_at": 1_i32 }, None)
            .await
    }

    async fn count_waiting_reminders(&self, user_id: &str) -> Result<usize> {
        self.col::<MessageReminder>(REMINDERS)
            .count_documents(doc! { "user_id": user_id, "delivered_at": null })
            .await
            .map(|count| count as usize)
            .map_err(|_| create_database_error!("count_documents", REMINDERS))
    }

    async fn move_reminder(&self, user_id: &str, id: &str, due_at: i64) -> Result<bool> {
        self.col::<MessageReminder>(REMINDERS)
            .update_one(
                doc! { "_id": id, "user_id": user_id, "delivered_at": null },
                doc! { "$set": { "due_at": due_at } },
            )
            .await
            .map(|result| result.matched_count > 0)
            .map_err(|_| create_database_error!("update_one", REMINDERS))
    }

    async fn delete_reminder(&self, user_id: &str, id: &str) -> Result<bool> {
        self.col::<MessageReminder>(REMINDERS)
            .delete_one(doc! { "_id": id, "user_id": user_id })
            .await
            .map(|result| result.deleted_count > 0)
            .map_err(|_| create_database_error!("delete_one", REMINDERS))
    }

    async fn delete_reminders_of(&self, user_id: &str) -> Result<()> {
        self.col::<MessageReminder>(REMINDERS)
            .delete_many(doc! { "user_id": user_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", REMINDERS))
    }

    async fn fetch_due_reminders(&self, now: i64, limit: i64) -> Result<Vec<MessageReminder>> {
        self.find_reminders(
            doc! { "delivered_at": null, "due_at": { "$lte": now } },
            doc! { "due_at": 1_i32 },
            Some(limit),
        )
        .await
    }

    async fn claim_reminder(&self, id: &str, now: i64) -> Result<bool> {
        self.col::<MessageReminder>(REMINDERS)
            .update_one(
                doc! { "_id": id, "delivered_at": null },
                doc! { "$set": { "delivered_at": now } },
            )
            .await
            .map(|result| result.modified_count > 0)
            .map_err(|_| create_database_error!("update_one", REMINDERS))
    }

    async fn delete_reminders_delivered_before(&self, cutoff: i64) -> Result<u64> {
        self.col::<MessageReminder>(REMINDERS)
            .delete_many(doc! { "delivered_at": { "$lt": cutoff } })
            .await
            .map(|result| result.deleted_count)
            .map_err(|_| create_database_error!("delete_many", REMINDERS))
    }
}
