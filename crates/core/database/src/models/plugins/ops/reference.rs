use std::collections::HashMap;

use revolt_result::Result;

use crate::{
    Plugin, PluginGrant, PluginInstall, PluginPublisher, PluginRelease, PluginReleaseStatus,
    PluginReviewLog, PluginSettings, ReferenceDb,
};

use super::{AbstractPlugins, PluginQuery};

/// In-memory plugin data of the reference database
#[derive(Default, Debug)]
pub struct PluginStore {
    pub plugins: HashMap<String, Plugin>,
    pub releases: HashMap<String, PluginRelease>,
    pub installs: HashMap<String, PluginInstall>,
    pub grants: HashMap<String, PluginGrant>,
    pub settings: HashMap<String, PluginSettings>,
    pub publishers: HashMap<String, PluginPublisher>,
    pub logs: Vec<PluginReviewLog>,
}

#[async_trait]
impl AbstractPlugins for ReferenceDb {
    async fn insert_plugin(&self, plugin: &Plugin) -> Result<()> {
        let mut store = self.plugin_store.lock().await;
        if store.plugins.contains_key(&plugin.id) {
            return Err(create_database_error!("insert", "plugins"));
        }
        store.plugins.insert(plugin.id.clone(), plugin.clone());
        Ok(())
    }

    async fn fetch_plugin(&self, id: &str) -> Result<Plugin> {
        self.plugin_store
            .lock()
            .await
            .plugins
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_plugins(&self, query: PluginQuery<'_>) -> Result<Vec<Plugin>> {
        let search = query
            .search
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let mut plugins: Vec<Plugin> = self
            .plugin_store
            .lock()
            .await
            .plugins
            .values()
            .filter(|p| {
                search.as_ref().is_none_or(|s| {
                    p.name.to_lowercase().contains(s) || p.summary.to_lowercase().contains(s)
                })
            })
            .filter(|p| query.category.is_none_or(|c| p.category == c))
            .filter(|p| query.publisher_id.is_none_or(|id| p.publisher_id == id))
            .filter(|p| query.statuses.is_empty() || query.statuses.contains(&p.status))
            .filter(|p| !query.reported || !p.reports.is_empty())
            .cloned()
            .collect();
        plugins.sort_by(|a, b| {
            b.install_count
                .cmp(&a.install_count)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        Ok(plugins
            .into_iter()
            .skip(query.offset)
            .take(query.limit.clamp(1, 100))
            .collect())
    }

    async fn update_plugin(&self, plugin: &Plugin, expected_version: i64) -> Result<bool> {
        let mut store = self.plugin_store.lock().await;
        match store.plugins.get_mut(&plugin.id) {
            Some(existing) if existing.version == expected_version => {
                // the install counter is owned by `bump_plugin_installs`
                let installs = existing.install_count;
                *existing = plugin.clone();
                existing.install_count = installs;
                Ok(true)
            }
            Some(_) => Ok(false),
            None => Err(create_error!(NotFound)),
        }
    }

    async fn report_plugin(&self, id: &str, user_id: &str) -> Result<usize> {
        let mut store = self.plugin_store.lock().await;
        let plugin = store
            .plugins
            .get_mut(id)
            .ok_or_else(|| create_error!(NotFound))?;
        if !plugin.reports.iter().any(|u| u == user_id) {
            plugin.reports.push(user_id.to_string());
        }
        Ok(plugin.reports.len())
    }

    async fn bump_plugin_installs(&self, id: &str, delta: i64) -> Result<()> {
        if let Some(plugin) = self.plugin_store.lock().await.plugins.get_mut(id) {
            plugin.install_count = (plugin.install_count + delta).max(0);
        }
        Ok(())
    }

    async fn insert_plugin_release(&self, release: &PluginRelease) -> Result<()> {
        let mut store = self.plugin_store.lock().await;
        if store.releases.contains_key(&release.id) {
            return Err(create_database_error!("insert", "plugin_releases"));
        }
        store.releases.insert(release.id.clone(), release.clone());
        Ok(())
    }

    async fn fetch_plugin_release(&self, id: &str) -> Result<PluginRelease> {
        self.plugin_store
            .lock()
            .await
            .releases
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_plugin_releases(&self, plugin_id: &str) -> Result<Vec<PluginRelease>> {
        let mut releases: Vec<PluginRelease> = self
            .plugin_store
            .lock()
            .await
            .releases
            .values()
            .filter(|r| r.plugin_id == plugin_id)
            .cloned()
            .collect();
        releases.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(releases)
    }

    async fn fetch_plugin_releases_by_status(
        &self,
        status: PluginReleaseStatus,
        limit: usize,
    ) -> Result<Vec<PluginRelease>> {
        let mut releases: Vec<PluginRelease> = self
            .plugin_store
            .lock()
            .await
            .releases
            .values()
            .filter(|r| r.status == status)
            .cloned()
            .collect();
        releases.sort_by(|a, b| a.id.cmp(&b.id));
        releases.truncate(limit.clamp(1, 200));
        Ok(releases)
    }

    async fn update_plugin_release(&self, release: &PluginRelease) -> Result<()> {
        let mut store = self.plugin_store.lock().await;
        let existing = store
            .releases
            .get_mut(&release.id)
            .ok_or_else(|| create_error!(NotFound))?;
        existing.status = release.status.clone();
        existing.reviewed_by = release.reviewed_by.clone();
        existing.reviewed_at = release.reviewed_at;
        existing.review_note = release.review_note.clone();
        Ok(())
    }

    async fn fetch_plugin_install(
        &self,
        user_id: &str,
        plugin_id: &str,
    ) -> Result<Option<PluginInstall>> {
        Ok(self
            .plugin_store
            .lock()
            .await
            .installs
            .get(&PluginInstall::key(user_id, plugin_id))
            .cloned())
    }

    async fn fetch_plugin_installs(&self, user_id: &str) -> Result<Vec<PluginInstall>> {
        let mut installs: Vec<PluginInstall> = self
            .plugin_store
            .lock()
            .await
            .installs
            .values()
            .filter(|i| i.user_id == user_id)
            .cloned()
            .collect();
        installs.sort_by(|a, b| a.installed_at.cmp(&b.installed_at));
        Ok(installs)
    }

    async fn save_plugin_install(&self, install: &PluginInstall) -> Result<()> {
        self.plugin_store
            .lock()
            .await
            .installs
            .insert(install.id.clone(), install.clone());
        Ok(())
    }

    async fn delete_plugin_install(&self, user_id: &str, plugin_id: &str) -> Result<()> {
        let key = PluginInstall::key(user_id, plugin_id);
        let mut store = self.plugin_store.lock().await;
        store.installs.remove(&key);
        store.grants.remove(&key);
        store.settings.remove(&key);
        Ok(())
    }

    async fn fetch_plugin_grant(
        &self,
        user_id: &str,
        plugin_id: &str,
    ) -> Result<Option<PluginGrant>> {
        Ok(self
            .plugin_store
            .lock()
            .await
            .grants
            .get(&PluginInstall::key(user_id, plugin_id))
            .cloned())
    }

    async fn fetch_plugin_grants(&self, user_id: &str) -> Result<Vec<PluginGrant>> {
        Ok(self
            .plugin_store
            .lock()
            .await
            .grants
            .values()
            .filter(|g| g.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn save_plugin_grant(&self, grant: &PluginGrant) -> Result<()> {
        self.plugin_store
            .lock()
            .await
            .grants
            .insert(grant.id.clone(), grant.clone());
        Ok(())
    }

    async fn fetch_plugin_settings(
        &self,
        user_id: &str,
        plugin_id: &str,
    ) -> Result<Option<PluginSettings>> {
        Ok(self
            .plugin_store
            .lock()
            .await
            .settings
            .get(&PluginInstall::key(user_id, plugin_id))
            .cloned())
    }

    async fn save_plugin_settings(&self, settings: &PluginSettings) -> Result<()> {
        self.plugin_store
            .lock()
            .await
            .settings
            .insert(settings.id.clone(), settings.clone());
        Ok(())
    }

    async fn fetch_plugin_publisher(&self, user_id: &str) -> Result<Option<PluginPublisher>> {
        Ok(self
            .plugin_store
            .lock()
            .await
            .publishers
            .get(user_id)
            .cloned())
    }

    async fn save_plugin_publisher(&self, publisher: &PluginPublisher) -> Result<()> {
        self.plugin_store
            .lock()
            .await
            .publishers
            .insert(publisher.id.clone(), publisher.clone());
        Ok(())
    }

    async fn insert_plugin_log(&self, log: &PluginReviewLog) -> Result<()> {
        self.plugin_store.lock().await.logs.push(log.clone());
        Ok(())
    }

    async fn fetch_plugin_logs(&self, plugin_id: &str) -> Result<Vec<PluginReviewLog>> {
        let mut logs: Vec<PluginReviewLog> = self
            .plugin_store
            .lock()
            .await
            .logs
            .iter()
            .filter(|l| l.plugin_id == plugin_id)
            .cloned()
            .collect();
        logs.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(logs)
    }
}
