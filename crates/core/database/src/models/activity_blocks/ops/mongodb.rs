use futures::StreamExt;
use revolt_models::v0::ActivityBlock;
use revolt_result::Result;

use crate::MongoDb;

use super::AbstractActivityBlocks;

static COL: &str = "activity_blocks";

impl MongoDb {
    async fn find_blocks(&self, filter: mongodb::bson::Document) -> Result<Vec<ActivityBlock>> {
        let mut cursor = self
            .col::<ActivityBlock>(COL)
            .find(filter)
            .await
            .map_err(|_| create_database_error!("find", COL))?;
        let mut blocks = vec![];
        while let Some(block) = cursor.next().await {
            blocks.push(block.map_err(|_| create_database_error!("find", COL))?);
        }
        Ok(blocks)
    }
}

#[async_trait]
impl AbstractActivityBlocks for MongoDb {
    async fn insert_activity_block(&self, block: &ActivityBlock) -> Result<()> {
        query!(self, insert_one, COL, &block).map(|_| ())
    }

    async fn fetch_activity_blocks_with_values(&self, values: &[String]) -> Result<Vec<ActivityBlock>> {
        if values.is_empty() {
            return Ok(vec![]);
        }
        self.find_blocks(doc! { "value": { "$in": values } }).await
    }

    async fn fetch_activity_blocks_of_case(&self, case_id: &str) -> Result<Vec<ActivityBlock>> {
        self.find_blocks(doc! { "case_id": case_id }).await
    }

    async fn delete_activity_block(&self, id: &str) -> Result<()> {
        self.col::<ActivityBlock>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", COL))
    }
}
