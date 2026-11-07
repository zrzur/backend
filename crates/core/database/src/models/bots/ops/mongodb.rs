use mongodb::options::FindOptions;
use revolt_result::Result;

use crate::{Bot, FieldsBot, PartialBot};
use crate::{IntoDocumentPath, MongoDb};

use super::AbstractBots;

static COL: &str = "bots";

#[async_trait]
impl AbstractBots for MongoDb {
    /// Insert new bot into the database
    async fn insert_bot(&self, bot: &Bot) -> Result<()> {
        query!(self, insert_one, COL, &bot).map(|_| ())
    }

    /// Fetch a bot by its id
    async fn fetch_bot(&self, id: &str) -> Result<Bot> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch a bot by its token
    async fn fetch_bot_by_token(&self, token: &str) -> Result<Bot> {
        // only the hash is stored
        let hash = crate::hash_token(token);
        let bot: Bot = query!(
            self,
            find_one,
            COL,
            doc! {
                "token_hash": &hash
            }
        )?
        .ok_or_else(|| create_error!(NotFound))?;
        if !crate::same_token_hash(&bot.token_hash, &hash) {
            return Err(create_error!(NotFound));
        }
        Ok(bot)
    }

    /// Fetch bots owned by a user
    async fn fetch_bots_by_user(&self, user_id: &str) -> Result<Vec<Bot>> {
        query!(
            self,
            find,
            COL,
            doc! {
                "owner": user_id
            }
        )
    }

    /// Fetch public bots listed on Discover, newest first
    async fn fetch_discoverable_bots(&self, limit: i64) -> Result<Vec<Bot>> {
        self.find_with_options(
            COL,
            doc! { "discoverable": true, "public": true },
            FindOptions::builder()
                .sort(doc! { "_id": -1 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    /// Ids of the bots Trust & Safety verified
    async fn fetch_verified_bot_ids(&self) -> Result<Vec<String>> {
        let verified = revolt_models::v0::BotFlags::Verified as i32;
        Ok(self
            .find::<Bot>(COL, doc! { "flags": { "$bitsAllSet": verified } })
            .await
            .map_err(|_| create_database_error!("find", COL))?
            .into_iter()
            .map(|bot| bot.id)
            .collect())
    }

    /// Get the number of bots owned by a user
    async fn get_number_of_bots_by_user(&self, user_id: &str) -> Result<usize> {
        query!(
            self,
            count_documents,
            COL,
            doc! {
                "owner": user_id
            }
        )
        .map(|v| v as usize)
    }

    /// Update bot with new information
    async fn update_bot(
        &self,
        id: &str,
        partial: &PartialBot,
        remove: Vec<FieldsBot>,
    ) -> Result<()> {
        query!(
            self,
            update_one_by_id,
            COL,
            id,
            partial,
            remove.iter().map(|x| x as &dyn IntoDocumentPath).collect(),
            None
        )
        .map(|_| ())
    }

    /// Delete a bot from the database
    async fn delete_bot(&self, id: &str) -> Result<()> {
        query!(self, delete_one_by_id, COL, id).map(|_| ())
    }
}

impl IntoDocumentPath for FieldsBot {
    fn as_path(&self) -> Option<&'static str> {
        match self {
            FieldsBot::InteractionsURL => Some("interactions_url"),
            FieldsBot::InteractionsEndpoint => Some("interactions_endpoint"),
            FieldsBot::Token => None,
        }
    }
}
