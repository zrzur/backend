use revolt_result::Result;

use crate::{AccountChangeRequest, AccountChangeStatus};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractAccountChangeRequest: Sync + Send {
    async fn insert_account_change_request(&self, request: &AccountChangeRequest) -> Result<()>;

    async fn fetch_account_change_request(&self, id: &str) -> Result<AccountChangeRequest>;

    /// Requests about one user, newest first
    async fn fetch_account_change_requests_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<AccountChangeRequest>>;

    /// The request an emailed confirmation token belongs to
    async fn fetch_account_change_request_by_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<AccountChangeRequest>>;

    /// Replace a request if it still has status `expected`; false otherwise
    async fn decide_account_change_request(
        &self,
        request: &AccountChangeRequest,
        expected: AccountChangeStatus,
    ) -> Result<bool>;
}
