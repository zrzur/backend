use revolt_result::Result;

use crate::{AccountChangeRequest, AccountChangeStatus, ReferenceDb};

use super::AbstractAccountChangeRequest;

#[async_trait]
impl AbstractAccountChangeRequest for ReferenceDb {
    async fn insert_account_change_request(&self, request: &AccountChangeRequest) -> Result<()> {
        self.account_change_requests
            .lock()
            .await
            .insert(request.id.clone(), request.clone());
        Ok(())
    }

    async fn fetch_account_change_request(&self, id: &str) -> Result<AccountChangeRequest> {
        self.account_change_requests
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_account_change_requests_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<AccountChangeRequest>> {
        let mut list: Vec<_> = self
            .account_change_requests
            .lock()
            .await
            .values()
            .filter(|r| r.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by(|a, b| b.id.cmp(&a.id));
        list.truncate(50);
        Ok(list)
    }

    async fn fetch_account_change_request_by_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<AccountChangeRequest>> {
        Ok(self
            .account_change_requests
            .lock()
            .await
            .values()
            .find(|r| r.token_hash.as_deref() == Some(token_hash))
            .cloned())
    }

    async fn decide_account_change_request(
        &self,
        request: &AccountChangeRequest,
        expected: AccountChangeStatus,
    ) -> Result<bool> {
        let mut store = self.account_change_requests.lock().await;
        match store.get(&request.id) {
            Some(current) if current.status == expected => {
                store.insert(request.id.clone(), request.clone());
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
