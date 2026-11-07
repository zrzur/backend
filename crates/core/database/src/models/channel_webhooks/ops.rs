use revolt_result::Result;

use crate::{FieldsWebhook, PartialWebhook, Webhook};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractWebhooks: Sync + Send {
    /// Insert new webhook into the database
    async fn insert_webhook(&self, webhook: &Webhook) -> Result<()>;

    /// Fetch webhook by id
    async fn fetch_webhook(&self, webhook_id: &str) -> Result<Webhook>;

    /// Fetch webhooks for channel
    async fn fetch_webhooks_for_channel(&self, channel_id: &str) -> Result<Vec<Webhook>>;

    /// Fetch the webhooks of several channels
    async fn fetch_webhooks_for_channels(&self, channel_ids: &[String]) -> Result<Vec<Webhook>>;

    /// Update webhook with new information
    async fn update_webhook(
        &self,
        webhook_id: &str,
        partial: &PartialWebhook,
        remove: &[FieldsWebhook],
    ) -> Result<()>;

    /// Delete webhook by id
    async fn delete_webhook(&self, webhook_id: &str) -> Result<()>;

    /// Delete the webhooks of a channel
    async fn delete_webhooks_for_channel(&self, channel_id: &str) -> Result<()>;
}
