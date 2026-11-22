use futures::StreamExt;
use revolt_models::v0::ActivityBlock;
use revolt_result::Result;

use crate::MongoDb;

use super::AbstractActivityBlocks;

static COL: &str = "activity_blocks";

#[async_trait]
impl AbstractActivityBlocks for MongoDb {
    async fn insert_activity_block(&self, block: &ActivityBlock) -> Result<()> {
        query!(self, insert_one, COL, &block).map(|_| ())
    }

    async fn fetch_activity_blocks(&self) -> Result<Vec<ActivityBlock>> {
        let mut cursor = self
            .col::<ActivityBlock>(COL)
            .find(doc! {})
            .await
            .map_err(|_| create_database_error!("find", COL))?;
        let mut blocks = vec![];
        while let Some(block) = cursor.next().await {
            blocks.push(block.map_err(|_| create_database_error!("find", COL))?);
        }
        Ok(blocks)
    }

    async fn delete_activity_block(&self, id: &str) -> Result<()> {
        self.col::<ActivityBlock>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", COL))
    }
}
