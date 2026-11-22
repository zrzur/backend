use revolt_models::v0::ActivityBlock;
use revolt_result::Result;

use crate::ReferenceDb;

use super::AbstractActivityBlocks;

#[async_trait]
impl AbstractActivityBlocks for ReferenceDb {
    async fn insert_activity_block(&self, block: &ActivityBlock) -> Result<()> {
        self.activity_blocks
            .lock()
            .await
            .insert(block.id.clone(), block.clone());
        Ok(())
    }

    async fn fetch_activity_blocks(&self) -> Result<Vec<ActivityBlock>> {
        Ok(self.activity_blocks.lock().await.values().cloned().collect())
    }

    async fn delete_activity_block(&self, id: &str) -> Result<()> {
        self.activity_blocks.lock().await.remove(id);
        Ok(())
    }
}
