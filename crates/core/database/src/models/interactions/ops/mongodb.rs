use revolt_result::Result;

use revolt_models::v0::EphemeralMessage;

use crate::{Interaction, InteractionStatus, MongoDb, MAX_FOLLOWUPS};

use super::{AbstractInteractions, InteractionTransition};

pub(crate) static COL: &str = "interactions";

fn status_name(status: InteractionStatus) -> &'static str {
    match status {
        InteractionStatus::Pending => "Pending",
        InteractionStatus::Deferred => "Deferred",
        InteractionStatus::Responded => "Responded",
    }
}

fn to_bson<T: serde::Serialize>(value: &T) -> Result<bson::Bson> {
    bson::to_bson(value).map_err(|_| create_database_error!("to_bson", COL))
}

#[async_trait]
impl AbstractInteractions for MongoDb {
    async fn insert_interaction(&self, interaction: &Interaction) -> Result<()> {
        self.col::<Interaction>(COL)
            .insert_one(interaction)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.to_string().contains("E11000") {
                    create_error!(DuplicateNonce)
                } else {
                    create_database_error!("insert_one", COL)
                }
            })
    }

    async fn fetch_interaction(&self, id: &str) -> Result<Interaction> {
        self.col::<Interaction>(COL)
            .find_one(doc! { "_id": id })
            .await
            .map_err(|_| create_database_error!("find_one", COL))?
            .ok_or_else(|| create_error!(UnknownInteraction))
    }

    async fn fetch_interaction_by_nonce(
        &self,
        user_id: &str,
        nonce: &str,
    ) -> Result<Option<Interaction>> {
        self.col::<Interaction>(COL)
            .find_one(doc! { "user_id": user_id, "nonce": nonce })
            .await
            .map_err(|_| create_database_error!("find_one", COL))
    }

    async fn fetch_interaction_by_ephemeral_message(
        &self,
        user_id: &str,
        message_id: &str,
    ) -> Result<Option<Interaction>> {
        let interaction = self
            .col::<Interaction>(COL)
            .find_one(doc! {
                "user_id": user_id,
                "$or": [ { "ephemeral_message._id": message_id }, { "followups._id": message_id } ]
            })
            .await
            .map_err(|_| create_database_error!("find_one", COL))?;
        // The TTL monitor runs about once a minute; do not serve expired documents
        Ok(interaction.filter(|interaction| !interaction.is_expired()))
    }

    async fn transition_interaction(
        &self,
        id: &str,
        from: &[InteractionStatus],
        to: InteractionStatus,
        set: InteractionTransition,
    ) -> Result<bool> {
        let mut update = doc! { "status": status_name(to) };
        if let Some(ephemeral) = set.ephemeral {
            update.insert("ephemeral", ephemeral);
        }
        if let Some(updates_source) = set.updates_source {
            update.insert("updates_source", updates_source);
        }
        if let Some(message_id) = set.original_message_id {
            update.insert("original_message_id", message_id);
        }
        if let Some(message) = set.ephemeral_message {
            update.insert("ephemeral_message", to_bson(&message)?);
        }
        if let Some(modal) = set.modal {
            update.insert("modal", to_bson(&modal)?);
        }

        let from: Vec<&str> = from.iter().map(|status| status_name(*status)).collect();
        let result = self
            .col::<Interaction>(COL)
            .update_one(
                doc! { "_id": id, "status": { "$in": from } },
                doc! { "$set": update },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        Ok(result.matched_count == 1)
    }

    async fn claim_modal_submission(&self, id: &str) -> Result<bool> {
        let result = self
            .col::<Interaction>(COL)
            .update_one(
                doc! { "_id": id, "modal": { "$exists": true }, "modal_submitted": { "$ne": true } },
                doc! { "$set": { "modal_submitted": true } },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        Ok(result.modified_count == 1)
    }

    async fn push_interaction_followup(
        &self,
        id: &str,
        ephemeral: Option<EphemeralMessage>,
        message_id: Option<String>,
    ) -> Result<bool> {
        let push = match (ephemeral, message_id) {
            (Some(message), _) => doc! { "followups": to_bson(&message)? },
            (None, Some(message_id)) => doc! { "followup_message_ids": message_id },
            (None, None) => return Ok(false),
        };
        // both lists together stay below the limit
        let limit = (MAX_FOLLOWUPS - 1) as i64;
        let result = self
            .col::<Interaction>(COL)
            .update_one(
                doc! {
                    "_id": id,
                    "$expr": { "$lte": [
                        { "$add": [
                            { "$size": { "$ifNull": ["$followups", []] } },
                            { "$size": { "$ifNull": ["$followup_message_ids", []] } }
                        ] },
                        limit
                    ] }
                },
                doc! { "$push": push },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        Ok(result.modified_count == 1)
    }

    async fn replace_interaction_ephemeral(
        &self,
        id: &str,
        message: &EphemeralMessage,
    ) -> Result<bool> {
        let value = to_bson(message)?;
        for (filter, update) in [
            (
                doc! { "_id": id, "ephemeral_message._id": &message.id },
                doc! { "$set": { "ephemeral_message": value.clone() } },
            ),
            (
                doc! { "_id": id, "followups._id": &message.id },
                doc! { "$set": { "followups.$": value.clone() } },
            ),
        ] {
            let result = self
                .col::<Interaction>(COL)
                .update_one(filter, update)
                .await
                .map_err(|_| create_database_error!("update_one", COL))?;
            if result.matched_count == 1 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn remove_interaction_ephemeral(&self, id: &str, message_id: &str) -> Result<bool> {
        for (filter, update) in [
            (
                doc! { "_id": id, "ephemeral_message._id": message_id },
                doc! { "$unset": { "ephemeral_message": "" } },
            ),
            (
                doc! { "_id": id, "followups._id": message_id },
                doc! { "$pull": { "followups": { "_id": message_id } } },
            ),
        ] {
            let result = self
                .col::<Interaction>(COL)
                .update_one(filter, update)
                .await
                .map_err(|_| create_database_error!("update_one", COL))?;
            if result.matched_count == 1 {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
