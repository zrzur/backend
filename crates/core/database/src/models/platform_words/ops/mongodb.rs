use futures::StreamExt;
use revolt_models::v0::PlatformWord;
use revolt_result::Result;

use crate::MongoDb;

use super::AbstractPlatformWords;

static COL: &str = "platform_words";

#[async_trait]
impl AbstractPlatformWords for MongoDb {
    async fn insert_platform_word(&self, word: &PlatformWord) -> Result<()> {
        query!(self, insert_one, COL, &word).map(|_| ())
    }

    async fn fetch_platform_words(&self) -> Result<Vec<PlatformWord>> {
        let mut cursor = self
            .col::<PlatformWord>(COL)
            .find(doc! {})
            .sort(doc! { "pattern": 1 })
            .await
            .map_err(|_| create_database_error!("find", COL))?;
        let mut words = vec![];
        while let Some(word) = cursor.next().await {
            words.push(word.map_err(|_| create_database_error!("find", COL))?);
        }
        Ok(words)
    }

    async fn delete_platform_word(&self, id: &str) -> Result<()> {
        self.col::<PlatformWord>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", COL))
    }
}
