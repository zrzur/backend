use revolt_result::Result;

use crate::LinkedAccount;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractLinkedAccounts: Sync + Send {
    /// Every account this person linked
    async fn fetch_linked_accounts(&self, user_id: &str) -> Result<Vec<LinkedAccount>>;

    /// This person's account on one service
    async fn fetch_linked_account(
        &self,
        user_id: &str,
        service: &str,
    ) -> Result<Option<LinkedAccount>>;

    /// Who linked this account on that service, if anyone
    async fn fetch_linked_account_owner(
        &self,
        service: &str,
        external_id: &str,
    ) -> Result<Option<String>>;

    /// Insert or replace. Fails with `LinkedAccountTaken` when the service
    /// account is already linked to someone else.
    async fn save_linked_account(&self, account: &LinkedAccount) -> Result<()>;

    /// Remove one link and its copy
    async fn delete_linked_account(&self, user_id: &str, service: &str) -> Result<()>;

    /// Remove every link of this person (account deletion)
    async fn delete_linked_accounts(&self, user_id: &str) -> Result<()>;
}
