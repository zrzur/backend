use revolt_result::Result;

use crate::{
    Plugin, PluginGrant, PluginInstall, PluginPublisher, PluginRelease, PluginReleaseStatus,
    PluginReviewLog, PluginSettings, PluginStatus,
};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

pub use reference::PluginStore;

/// Filters for listing plugins
#[derive(Default, Clone)]
pub struct PluginQuery<'a> {
    /// Case-insensitive match on the name or summary
    pub search: Option<&'a str>,
    pub category: Option<&'a str>,
    pub publisher_id: Option<&'a str>,
    pub statuses: Vec<PluginStatus>,
    /// Only plugins with at least one report
    pub reported: bool,
    pub limit: usize,
    pub offset: usize,
}

#[async_trait]
pub trait AbstractPlugins: Sync + Send {
    async fn insert_plugin(&self, plugin: &Plugin) -> Result<()>;

    /// Fetch a plugin by its manifest id
    async fn fetch_plugin(&self, id: &str) -> Result<Plugin>;

    /// List plugins, most installed first, then newest
    async fn fetch_plugins(&self, query: PluginQuery<'_>) -> Result<Vec<Plugin>>;

    /// Save a plugin only if the stored version is still `expected_version`;
    /// false if someone else changed it first
    async fn update_plugin(&self, plugin: &Plugin, expected_version: i64) -> Result<bool>;

    /// Record a report once per user; returns the number of reports
    async fn report_plugin(&self, id: &str, user_id: &str) -> Result<usize>;

    /// Add `delta` to the install counter
    async fn bump_plugin_installs(&self, id: &str, delta: i64) -> Result<()>;

    async fn insert_plugin_release(&self, release: &PluginRelease) -> Result<()>;

    async fn fetch_plugin_release(&self, id: &str) -> Result<PluginRelease>;

    /// Releases of a plugin, newest first
    async fn fetch_plugin_releases(&self, plugin_id: &str) -> Result<Vec<PluginRelease>>;

    /// Releases in a review state across all plugins, oldest first
    async fn fetch_plugin_releases_by_status(
        &self,
        status: PluginReleaseStatus,
        limit: usize,
    ) -> Result<Vec<PluginRelease>>;

    /// Save the review fields (status, reviewed_by, reviewed_at, review_note)
    async fn update_plugin_release(&self, release: &PluginRelease) -> Result<()>;

    async fn fetch_plugin_install(&self, user_id: &str, plugin_id: &str)
        -> Result<Option<PluginInstall>>;

    async fn fetch_plugin_installs(&self, user_id: &str) -> Result<Vec<PluginInstall>>;

    /// Create or replace
    async fn save_plugin_install(&self, install: &PluginInstall) -> Result<()>;

    /// Remove an install together with its grant and settings
    async fn delete_plugin_install(&self, user_id: &str, plugin_id: &str) -> Result<()>;

    async fn fetch_plugin_grant(&self, user_id: &str, plugin_id: &str)
        -> Result<Option<PluginGrant>>;

    async fn fetch_plugin_grants(&self, user_id: &str) -> Result<Vec<PluginGrant>>;

    async fn save_plugin_grant(&self, grant: &PluginGrant) -> Result<()>;

    async fn fetch_plugin_settings(
        &self,
        user_id: &str,
        plugin_id: &str,
    ) -> Result<Option<PluginSettings>>;

    async fn save_plugin_settings(&self, settings: &PluginSettings) -> Result<()>;

    async fn fetch_plugin_publisher(&self, user_id: &str) -> Result<Option<PluginPublisher>>;

    async fn save_plugin_publisher(&self, publisher: &PluginPublisher) -> Result<()>;

    async fn insert_plugin_log(&self, log: &PluginReviewLog) -> Result<()>;

    /// Review log of one plugin, newest first
    async fn fetch_plugin_logs(&self, plugin_id: &str) -> Result<Vec<PluginReviewLog>>;
}
