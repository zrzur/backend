use revolt_models::v0::PlatformWord;
use revolt_result::Result;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractPlatformWords: Sync + Send {
    /// Add an entry
    async fn insert_platform_word(&self, word: &PlatformWord) -> Result<()>;

    /// The whole list
    async fn fetch_platform_words(&self) -> Result<Vec<PlatformWord>>;

    /// Remove an entry
    async fn delete_platform_word(&self, id: &str) -> Result<()>;
}
