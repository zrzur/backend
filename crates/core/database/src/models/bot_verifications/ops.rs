use revolt_result::Result;

use crate::{BotVerification, BotVerificationStatus};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractBotVerification: Sync + Send {
    /// Store a new request
    async fn insert_bot_verification(&self, request: &BotVerification) -> Result<()>;

    /// Replace a request if its status is still `expected`; false otherwise
    async fn decide_bot_verification(
        &self,
        request: &BotVerification,
        expected: BotVerificationStatus,
    ) -> Result<bool>;

    async fn fetch_bot_verification(&self, id: &str) -> Result<BotVerification>;

    /// Requests of one bot, newest first
    async fn fetch_bot_verifications_for_bot(&self, bot_id: &str) -> Result<Vec<BotVerification>>;

    /// Requests with a status (all when `None`), oldest first, after `after`
    async fn fetch_bot_verifications(
        &self,
        status: Option<BotVerificationStatus>,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<BotVerification>>;
}
