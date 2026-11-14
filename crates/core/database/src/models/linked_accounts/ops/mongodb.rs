use mongodb::options::ReplaceOptions;
use revolt_result::Result;

use crate::{LinkedAccount, MongoDb};

use super::AbstractLinkedAccounts;

static COL: &str = "linked_accounts";

#[async_trait]
impl AbstractLinkedAccounts for MongoDb {
    async fn fetch_linked_accounts(&self, user_id: &str) -> Result<Vec<LinkedAccount>> {
        query!(self, find, COL, doc! { "user_id": user_id })
    }

    async fn fetch_linked_account(
        &self,
        user_id: &str,
        service: &str,
    ) -> Result<Option<LinkedAccount>> {
        query!(self, find_one_by_id, COL, &LinkedAccount::key(user_id, service))
    }

    async fn fetch_linked_account_owner(
        &self,
        service: &str,
        external_id: &str,
    ) -> Result<Option<String>> {
        let found: Option<LinkedAccount> = query!(
            self,
            find_one,
            COL,
            doc! { "service": service, "external_id": external_id }
        )?;
        Ok(found.map(|account| account.user_id))
    }

    async fn save_linked_account(&self, account: &LinkedAccount) -> Result<()> {
        // the unique index on (service, external_id) is the real guard; this
        // check only gives the clear error in the common case
        if let Some(owner) = self
            .fetch_linked_account_owner(&account.service, &account.external_id)
            .await?
        {
            if owner != account.user_id {
                return Err(create_error!(LinkedAccountTaken));
            }
        }
        self.col::<LinkedAccount>(COL)
            .replace_one(doc! { "_id": &account.id }, account)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.to_string().contains("E11000") {
                    create_error!(LinkedAccountTaken)
                } else {
                    create_database_error!("replace_one", COL)
                }
            })
    }

    async fn delete_linked_account(&self, user_id: &str, service: &str) -> Result<()> {
        self.col::<LinkedAccount>(COL)
            .delete_one(doc! { "_id": LinkedAccount::key(user_id, service) })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", COL))
    }

    async fn delete_linked_accounts(&self, user_id: &str) -> Result<()> {
        self.col::<LinkedAccount>(COL)
            .delete_many(doc! { "user_id": user_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", COL))
    }
}
