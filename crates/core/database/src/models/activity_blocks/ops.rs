use revolt_models::v0::ActivityBlock;
use revolt_result::Result;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractActivityBlocks: Sync + Send {
    /// Add a block
    async fn insert_activity_block(&self, block: &ActivityBlock) -> Result<()>;

    /// The blocks whose value is one of `values` (index `{value, kind}`)
    async fn fetch_activity_blocks_with_values(&self, values: &[String]) -> Result<Vec<ActivityBlock>>;

    /// The blocks taken on one case (index `{case_id}`)
    async fn fetch_activity_blocks_of_case(&self, case_id: &str) -> Result<Vec<ActivityBlock>>;

    /// Remove a block (an action reversed)
    async fn delete_activity_block(&self, id: &str) -> Result<()>;
}
