use mongodb::options::FindOptions;
use revolt_result::Result;

use crate::{BotVerification, BotVerificationStatus, MongoDb};

use super::AbstractBotVerification;

static COL: &str = "bot_verifications";

fn status_name(status: BotVerificationStatus) -> &'static str {
    match status {
        BotVerificationStatus::Pending => "Pending",
        BotVerificationStatus::Approved => "Approved",
        BotVerificationStatus::Denied => "Denied",
        BotVerificationStatus::Revoked => "Revoked",
    }
}

#[async_trait]
impl AbstractBotVerification for MongoDb {
    async fn insert_bot_verification(&self, request: &BotVerification) -> Result<()> {
        query!(self, insert_one, COL, &request).map(|_| ())
    }

    async fn decide_bot_verification(
        &self,
        request: &BotVerification,
        expected: BotVerificationStatus,
    ) -> Result<bool> {
        // two reviewers deciding at once: only the first counts
        self.col::<BotVerification>(COL)
            .replace_one(doc! { "_id": &request.id, "status": status_name(expected) }, request)
            .await
            .map(|r| r.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", COL))
    }

    async fn fetch_bot_verification(&self, id: &str) -> Result<BotVerification> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_bot_verifications_for_bot(&self, bot_id: &str) -> Result<Vec<BotVerification>> {
        self.find_with_options(
            COL,
            doc! { "bot_id": bot_id },
            FindOptions::builder().sort(doc! { "_id": -1 }).limit(50).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn fetch_bot_verifications(
        &self,
        status: Option<BotVerificationStatus>,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<BotVerification>> {
        let mut filter = doc! {};
        if let Some(status) = status {
            filter.insert("status", status_name(status));
        }
        if let Some(after) = after {
            filter.insert("_id", doc! { "$gt": after });
        }
        self.find_with_options(
            COL,
            filter,
            FindOptions::builder()
                .sort(doc! { "_id": 1 })
                .limit(limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }
}
