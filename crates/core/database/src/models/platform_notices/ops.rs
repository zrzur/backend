use revolt_result::Result;

use crate::PlatformNotice;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractPlatformNotice: Sync + Send {
    async fn insert_platform_notice(&self, notice: &PlatformNotice) -> Result<()>;

    async fn fetch_platform_notice(&self, id: &str) -> Result<PlatformNotice>;

    /// Newest first; only those for one person when `user_id` is given
    async fn fetch_platform_notices(&self, user_id: Option<&str>, limit: usize) -> Result<Vec<PlatformNotice>>;

    /// Live notices for this person or everyone, newest first, without the
    /// ones they dismissed
    async fn fetch_platform_notices_for(&self, user_id: &str, now: i64) -> Result<Vec<PlatformNotice>>;

    async fn withdraw_platform_notice(&self, id: &str, at: i64) -> Result<()>;

    async fn set_platform_notice_delivery(&self, id: &str, emailed: bool, pushed: bool) -> Result<()>;

    async fn dismiss_platform_notice(&self, id: &str, user_id: &str) -> Result<()>;
}
