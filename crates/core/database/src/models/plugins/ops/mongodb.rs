use mongodb::options::{FindOptions, ReplaceOptions};
use revolt_result::Result;

use crate::{
    MongoDb, Plugin, PluginGrant, PluginInstall, PluginPublisher, PluginRelease,
    PluginReleaseStatus, PluginReviewLog, PluginSettings,
};

use super::{AbstractPlugins, PluginQuery};

static PLUGINS: &str = "plugins";
static RELEASES: &str = "plugin_releases";
static INSTALLS: &str = "plugin_installs";
static GRANTS: &str = "plugin_grants";
static SETTINGS: &str = "plugin_settings";
static PUBLISHERS: &str = "plugin_publishers";
static LOGS: &str = "plugin_reviews";

fn to_bson<T: serde::Serialize>(value: &T) -> bson::Bson {
    bson::to_bson(value).expect("failed to serialize")
}

#[async_trait]
impl AbstractPlugins for MongoDb {
    async fn insert_plugin(&self, plugin: &Plugin) -> Result<()> {
        query!(self, insert_one, PLUGINS, &plugin).map(|_| ())
    }

    async fn fetch_plugin(&self, id: &str) -> Result<Plugin> {
        query!(self, find_one_by_id, PLUGINS, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_plugins(&self, query: PluginQuery<'_>) -> Result<Vec<Plugin>> {
        let mut filter = doc! {};
        if let Some(search) = query.search.map(str::trim).filter(|s| !s.is_empty()) {
            let pattern = doc! { "$regex": regex::escape(search), "$options": "i" };
            filter.insert("$or", vec![doc! { "name": pattern.clone() }, doc! { "summary": pattern }]);
        }
        if let Some(category) = query.category {
            filter.insert("category", category);
        }
        if let Some(publisher) = query.publisher_id {
            filter.insert("publisher_id", publisher);
        }
        if !query.statuses.is_empty() {
            filter.insert(
                "status",
                doc! { "$in": query.statuses.iter().map(to_bson).collect::<Vec<_>>() },
            );
        }
        if query.reported {
            filter.insert("reports.0", doc! { "$exists": true });
        }
        self.find_with_options(
            PLUGINS,
            filter,
            FindOptions::builder()
                .sort(doc! { "install_count": -1, "created_at": -1 })
                .skip(query.offset as u64)
                .limit(query.limit.clamp(1, 100) as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", PLUGINS))
    }

    async fn update_plugin(&self, plugin: &Plugin, expected_version: i64) -> Result<bool> {
        let mut set = doc! {
            "name": &plugin.name,
            "summary": &plugin.summary,
            "category": &plugin.category,
            "status": to_bson(&plugin.status),
            "official": plugin.official,
            "reports": to_bson(&plugin.reports),
            "updated_at": plugin.updated_at,
            "version": plugin.version,
        };
        let mut unset = doc! {};
        for (key, value) in [
            ("icon", plugin.icon.as_ref().map(to_bson)),
            ("latest_release", plugin.latest_release.as_ref().map(to_bson)),
            ("latest_version", plugin.latest_version.as_ref().map(to_bson)),
            ("status_reason", plugin.status_reason.as_ref().map(to_bson)),
        ] {
            match value {
                Some(value) => {
                    set.insert(key, value);
                }
                None => {
                    unset.insert(key, 1);
                }
            }
        }
        let mut update = doc! { "$set": set };
        if !unset.is_empty() {
            update.insert("$unset", unset);
        }
        self.col::<Plugin>(PLUGINS)
            .update_one(doc! { "_id": &plugin.id, "version": expected_version }, update)
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", PLUGINS))
    }

    async fn report_plugin(&self, id: &str, user_id: &str) -> Result<usize> {
        self.col::<Plugin>(PLUGINS)
            .update_one(doc! { "_id": id }, doc! { "$addToSet": { "reports": user_id } })
            .await
            .map_err(|_| create_database_error!("update_one", PLUGINS))?;
        Ok(self.fetch_plugin(id).await?.reports.len())
    }

    async fn bump_plugin_installs(&self, id: &str, delta: i64) -> Result<()> {
        self.col::<Plugin>(PLUGINS)
            .update_one(doc! { "_id": id }, doc! { "$inc": { "install_count": delta } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", PLUGINS))
    }

    async fn insert_plugin_release(&self, release: &PluginRelease) -> Result<()> {
        query!(self, insert_one, RELEASES, &release).map(|_| ())
    }

    async fn fetch_plugin_release(&self, id: &str) -> Result<PluginRelease> {
        query!(self, find_one_by_id, RELEASES, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_plugin_releases(&self, plugin_id: &str) -> Result<Vec<PluginRelease>> {
        self.find_with_options(
            RELEASES,
            doc! { "plugin_id": plugin_id },
            FindOptions::builder().sort(doc! { "_id": -1 }).limit(200).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", RELEASES))
    }

    async fn fetch_plugin_releases_by_status(
        &self,
        status: PluginReleaseStatus,
        limit: usize,
    ) -> Result<Vec<PluginRelease>> {
        self.find_with_options(
            RELEASES,
            doc! { "status": to_bson(&status) },
            FindOptions::builder()
                .sort(doc! { "_id": 1 })
                .limit(limit.clamp(1, 200) as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", RELEASES))
    }

    async fn update_plugin_release(&self, release: &PluginRelease) -> Result<()> {
        let mut set = doc! { "status": to_bson(&release.status) };
        let mut unset = doc! {};
        for (key, value) in [
            ("reviewed_by", release.reviewed_by.as_ref().map(to_bson)),
            ("reviewed_at", release.reviewed_at.map(bson::Bson::Int64)),
            ("review_note", release.review_note.as_ref().map(to_bson)),
        ] {
            match value {
                Some(value) => {
                    set.insert(key, value);
                }
                None => {
                    unset.insert(key, 1);
                }
            }
        }
        let mut update = doc! { "$set": set };
        if !unset.is_empty() {
            update.insert("$unset", unset);
        }
        self.col::<PluginRelease>(RELEASES)
            .update_one(doc! { "_id": &release.id }, update)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", RELEASES))
    }

    async fn fetch_plugin_install(
        &self,
        user_id: &str,
        plugin_id: &str,
    ) -> Result<Option<PluginInstall>> {
        query!(self, find_one_by_id, INSTALLS, &PluginInstall::key(user_id, plugin_id))
    }

    async fn fetch_plugin_installs(&self, user_id: &str) -> Result<Vec<PluginInstall>> {
        self.find_with_options(
            INSTALLS,
            doc! { "user_id": user_id },
            FindOptions::builder().sort(doc! { "installed_at": 1 }).limit(200).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", INSTALLS))
    }

    async fn save_plugin_install(&self, install: &PluginInstall) -> Result<()> {
        self.col::<PluginInstall>(INSTALLS)
            .replace_one(doc! { "_id": &install.id }, install)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", INSTALLS))
    }

    async fn delete_plugin_install(&self, user_id: &str, plugin_id: &str) -> Result<()> {
        let key = PluginInstall::key(user_id, plugin_id);
        for collection in [INSTALLS, GRANTS, SETTINGS] {
            self.col::<bson::Document>(collection)
                .delete_one(doc! { "_id": &key })
                .await
                .map_err(|_| create_database_error!("delete_one", collection))?;
        }
        Ok(())
    }

    async fn fetch_plugin_grant(
        &self,
        user_id: &str,
        plugin_id: &str,
    ) -> Result<Option<PluginGrant>> {
        query!(self, find_one_by_id, GRANTS, &PluginInstall::key(user_id, plugin_id))
    }

    async fn fetch_plugin_grants(&self, user_id: &str) -> Result<Vec<PluginGrant>> {
        self.find_with_options(GRANTS, doc! { "user_id": user_id }, None)
            .await
            .map_err(|_| create_database_error!("find", GRANTS))
    }

    async fn save_plugin_grant(&self, grant: &PluginGrant) -> Result<()> {
        self.col::<PluginGrant>(GRANTS)
            .replace_one(doc! { "_id": &grant.id }, grant)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", GRANTS))
    }

    async fn fetch_plugin_settings(
        &self,
        user_id: &str,
        plugin_id: &str,
    ) -> Result<Option<PluginSettings>> {
        query!(self, find_one_by_id, SETTINGS, &PluginInstall::key(user_id, plugin_id))
    }

    async fn save_plugin_settings(&self, settings: &PluginSettings) -> Result<()> {
        self.col::<PluginSettings>(SETTINGS)
            .replace_one(doc! { "_id": &settings.id }, settings)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", SETTINGS))
    }

    async fn fetch_plugin_publisher(&self, user_id: &str) -> Result<Option<PluginPublisher>> {
        query!(self, find_one_by_id, PUBLISHERS, user_id)
    }

    async fn save_plugin_publisher(&self, publisher: &PluginPublisher) -> Result<()> {
        self.col::<PluginPublisher>(PUBLISHERS)
            .replace_one(doc! { "_id": &publisher.id }, publisher)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", PUBLISHERS))
    }

    async fn insert_plugin_log(&self, log: &PluginReviewLog) -> Result<()> {
        query!(self, insert_one, LOGS, &log).map(|_| ())
    }

    async fn fetch_plugin_logs(&self, plugin_id: &str) -> Result<Vec<PluginReviewLog>> {
        self.find_with_options(
            LOGS,
            doc! { "plugin_id": plugin_id },
            FindOptions::builder().sort(doc! { "_id": -1 }).limit(200).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", LOGS))
    }
}
