use revolt_models::v0::ActivityBlock;
use revolt_result::Result;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractActivityBlocks: Sync + Send {
    /// Add a block
    async fn insert_activity_block(&self, block: &ActivityBlock) -> Result<()>;

    /// Every block
    async fn fetch_activity_blocks(&self) -> Result<Vec<ActivityBlock>>;

    /// Remove a block (an action reversed)
    async fn delete_activity_block(&self, id: &str) -> Result<()>;
}
