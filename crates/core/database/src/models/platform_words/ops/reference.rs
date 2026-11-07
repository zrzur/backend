use revolt_models::v0::PlatformWord;
use revolt_result::Result;

use crate::ReferenceDb;

use super::AbstractPlatformWords;

#[async_trait]
impl AbstractPlatformWords for ReferenceDb {
    async fn insert_platform_word(&self, word: &PlatformWord) -> Result<()> {
        self.platform_words
            .lock()
            .await
            .insert(word.id.clone(), word.clone());
        Ok(())
    }

    async fn fetch_platform_words(&self) -> Result<Vec<PlatformWord>> {
        let mut words: Vec<PlatformWord> =
            self.platform_words.lock().await.values().cloned().collect();
        words.sort_by(|a, b| a.pattern.cmp(&b.pattern));
        Ok(words)
    }

    async fn delete_platform_word(&self, id: &str) -> Result<()> {
        self.platform_words.lock().await.remove(id);
        Ok(())
    }
}
