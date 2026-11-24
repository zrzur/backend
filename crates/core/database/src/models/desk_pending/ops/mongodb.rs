use revolt_result::Result;

use crate::{DeskPending, MongoDb};

use super::AbstractDeskPending;

pub(crate) static COL: &str = "desk_pending";

#[async_trait]
impl AbstractDeskPending for MongoDb {
    async fn insert_desk_pending(&self, pending: &DeskPending) -> Result<()> {
        // `purge_at`: a BSON date copy of `expires_at` for the TTL index
        // (only a date expires; the model keeps milliseconds)
        let mut document =
            bson::to_document(pending).map_err(|_| create_database_error!("to_document", COL))?;
        document.insert("purge_at", bson::DateTime::from_millis(pending.expires_at));
        self.col::<bson::Document>(COL)
            .insert_one(document)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("insert_one", COL))
    }

    async fn fetch_desk_pending(&self, id: &str) -> Result<Option<DeskPending>> {
        self.col::<DeskPending>(COL)
            .find_one(doc! { "_id": id })
            .await
            .map_err(|_| create_database_error!("find_one", COL))
    }

    async fn bind_desk_pending_card(&self, id: &str, message_id: &str) -> Result<bool> {
        let result = self
            .col::<DeskPending>(COL)
            .update_one(
                doc! {
                    "_id": id,
                    "$or": [
                        { "card_message": { "$exists": false } },
                        { "card_message": message_id },
                    ],
                },
                doc! { "$set": { "card_message": message_id } },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        Ok(result.matched_count == 1)
    }

    async fn claim_desk_pending(&self, id: &str, now: i64) -> Result<bool> {
        let result = self
            .col::<DeskPending>(COL)
            .update_one(
                doc! { "_id": id, "used": { "$ne": true }, "expires_at": { "$gt": now } },
                doc! { "$set": { "used": true } },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        Ok(result.modified_count == 1)
    }

    async fn release_desk_pending(&self, id: &str) -> Result<()> {
        self.col::<DeskPending>(COL)
            .update_one(doc! { "_id": id }, doc! { "$set": { "used": false } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }
}
