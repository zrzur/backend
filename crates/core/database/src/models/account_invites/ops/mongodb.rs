use crate::{AbstractAccountInvites, AccountInvite, MongoDb};
use bson::to_document;
use mongodb::options::UpdateOptions;
use revolt_result::Result;

const COL: &str = "account_invites";

#[async_trait]
impl AbstractAccountInvites for MongoDb {
    /// Find invite by id
    async fn fetch_account_invite(&self, id: &str) -> Result<AccountInvite> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(InvalidInvite))
    }

    async fn claim_account_invite(&self, id: &str) -> Result<AccountInvite> {
        // One atomic step: only the request that flips `used` wins
        self.col::<AccountInvite>(COL)
            .find_one_and_update(
                // an unused invite has no `used` field at all (skipped when false)
                doc! { "_id": id, "used": { "$ne": true } },
                doc! { "$set": { "used": true } },
            )
            .await
            .map_err(|_| create_database_error!("find_one_and_update", COL))?
            .map(|mut invite| {
                invite.used = true;
                invite
            })
            .ok_or_else(|| create_error!(InvalidInvite))
    }

    async fn release_account_invite(&self, id: &str) -> Result<()> {
        self.col::<AccountInvite>(COL)
            .update_one(
                doc! { "_id": id },
                doc! { "$set": { "used": false }, "$unset": { "claimed_by": 1 } },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))
            .map(|_| ())
    }

    /// Save invite
    async fn save_account_invite(&self, invite: &AccountInvite) -> Result<()> {
        self.col::<AccountInvite>(COL)
            .update_one(
                doc! {
                    "_id": &invite.id
                },
                doc! {
                    "$set": to_document(invite).map_err(|_| create_database_error!("to_document", COL))?,
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .await
            .map_err(|_| create_database_error!("upsert_one", COL))
            .map(|_| ())
    }
}
