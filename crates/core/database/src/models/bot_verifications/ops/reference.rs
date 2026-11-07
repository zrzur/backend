use revolt_result::Result;

use crate::{BotVerification, BotVerificationStatus, ReferenceDb};

use super::AbstractBotVerification;

#[async_trait]
impl AbstractBotVerification for ReferenceDb {
    async fn insert_bot_verification(&self, request: &BotVerification) -> Result<()> {
        self.bot_verifications
            .lock()
            .await
            .insert(request.id.clone(), request.clone());
        Ok(())
    }

    async fn decide_bot_verification(
        &self,
        request: &BotVerification,
        expected: BotVerificationStatus,
    ) -> Result<bool> {
        let mut store = self.bot_verifications.lock().await;
        match store.get(&request.id) {
            Some(current) if current.status == expected => {
                store.insert(request.id.clone(), request.clone());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn fetch_bot_verification(&self, id: &str) -> Result<BotVerification> {
        self.bot_verifications
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_bot_verifications_for_bot(&self, bot_id: &str) -> Result<Vec<BotVerification>> {
        let mut list: Vec<_> = self
            .bot_verifications
            .lock()
            .await
            .values()
            .filter(|r| r.bot_id == bot_id)
            .cloned()
            .collect();
        list.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(list)
    }

    async fn fetch_bot_verifications(
        &self,
        status: Option<BotVerificationStatus>,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<BotVerification>> {
        let mut list: Vec<_> = self
            .bot_verifications
            .lock()
            .await
            .values()
            .filter(|r| status.is_none_or(|s| r.status == s))
            .filter(|r| after.as_ref().is_none_or(|a| &r.id > a))
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list.truncate(limit);
        Ok(list)
    }
}
