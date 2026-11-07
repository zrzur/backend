use mongodb::options::FindOptions;
use revolt_result::Result;

use crate::{AccountChangeRequest, AccountChangeStatus, MongoDb};

use super::AbstractAccountChangeRequest;

static COL: &str = "account_change_requests";

fn status_name(status: AccountChangeStatus) -> &'static str {
    match status {
        AccountChangeStatus::Pending => "Pending",
        AccountChangeStatus::Accepted => "Accepted",
        AccountChangeStatus::Declined => "Declined",
        AccountChangeStatus::Cancelled => "Cancelled",
    }
}

#[async_trait]
impl AbstractAccountChangeRequest for MongoDb {
    async fn insert_account_change_request(&self, request: &AccountChangeRequest) -> Result<()> {
        query!(self, insert_one, COL, &request).map(|_| ())
    }

    async fn fetch_account_change_request(&self, id: &str) -> Result<AccountChangeRequest> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_account_change_requests_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<AccountChangeRequest>> {
        self.find_with_options(
            COL,
            doc! { "user_id": user_id },
            FindOptions::builder().sort(doc! { "_id": -1 }).limit(50).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn fetch_account_change_request_by_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<AccountChangeRequest>> {
        query!(self, find_one, COL, doc! { "token_hash": token_hash })
    }

    async fn decide_account_change_request(
        &self,
        request: &AccountChangeRequest,
        expected: AccountChangeStatus,
    ) -> Result<bool> {
        // accepted twice from two devices: only the first counts
        self.col::<AccountChangeRequest>(COL)
            .replace_one(doc! { "_id": &request.id, "status": status_name(expected) }, request)
            .await
            .map(|r| r.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", COL))
    }
}
