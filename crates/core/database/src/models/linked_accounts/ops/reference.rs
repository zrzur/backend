use revolt_result::Result;

use crate::{LinkedAccount, ReferenceDb};

use super::AbstractLinkedAccounts;

#[async_trait]
impl AbstractLinkedAccounts for ReferenceDb {
    async fn fetch_linked_accounts(&self, user_id: &str) -> Result<Vec<LinkedAccount>> {
        let mut list: Vec<_> = self
            .linked_accounts
            .lock()
            .await
            .values()
            .filter(|account| account.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.service.cmp(&b.service));
        Ok(list)
    }

    async fn fetch_linked_account(
        &self,
        user_id: &str,
        service: &str,
    ) -> Result<Option<LinkedAccount>> {
        Ok(self
            .linked_accounts
            .lock()
            .await
            .get(&LinkedAccount::key(user_id, service))
            .cloned())
    }

    async fn fetch_linked_account_owner(
        &self,
        service: &str,
        external_id: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .linked_accounts
            .lock()
            .await
            .values()
            .find(|account| account.service == service && account.external_id == external_id)
            .map(|account| account.user_id.clone()))
    }

    async fn save_linked_account(&self, account: &LinkedAccount) -> Result<()> {
        let mut accounts = self.linked_accounts.lock().await;
        if accounts.values().any(|other| {
            other.service == account.service
                && other.external_id == account.external_id
                && other.user_id != account.user_id
        }) {
            return Err(create_error!(LinkedAccountTaken));
        }
        accounts.insert(account.id.clone(), account.clone());
        Ok(())
    }

    async fn delete_linked_account(&self, user_id: &str, service: &str) -> Result<()> {
        self.linked_accounts
            .lock()
            .await
            .remove(&LinkedAccount::key(user_id, service));
        Ok(())
    }

    async fn delete_linked_accounts(&self, user_id: &str) -> Result<()> {
        self.linked_accounts
            .lock()
            .await
            .retain(|_, account| account.user_id != user_id);
        Ok(())
    }
}
