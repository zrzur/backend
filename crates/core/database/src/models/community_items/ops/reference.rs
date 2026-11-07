use revolt_result::Result;

use crate::{
    CommunityItem, CommunityItemStatus, CommunityReview, CreatorPermissionLog, PlatformRole,
    ReferenceDb, User,
};

use super::{AbstractCommunityItems, CommunityItemQuery, CommunitySort};

#[async_trait]
impl AbstractCommunityItems for ReferenceDb {
    async fn insert_community_item(&self, item: &CommunityItem) -> Result<()> {
        let mut items = self.community_items.lock().await;
        if items.contains_key(&item.id) {
            return Err(create_database_error!("insert", "community_items"));
        }
        items.insert(item.id.clone(), item.clone());
        Ok(())
    }

    async fn fetch_community_item(&self, id: &str) -> Result<CommunityItem> {
        self.community_items
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_community_items(
        &self,
        query: CommunityItemQuery<'_>,
    ) -> Result<Vec<CommunityItem>> {
        let search = query
            .search
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let mut items: Vec<CommunityItem> = self
            .community_items
            .lock()
            .await
            .values()
            .filter(|item| query.kind.as_ref().is_none_or(|kind| &item.kind == kind))
            .filter(|item| {
                search.as_ref().is_none_or(|s| {
                    item.name.to_lowercase().contains(s)
                        || item.tags.iter().any(|tag| tag.contains(s))
                        || query.search_creators.contains(&item.creator_id)
                })
            })
            .filter(|item| query.creator_id.is_none_or(|id| item.creator_id == id))
            .filter(|item| !query.hidden_creators.contains(&item.creator_id))
            .filter(|item| query.ids.as_ref().is_none_or(|ids| ids.contains(&item.id)))
            .filter(|item| query.statuses.is_empty() || query.statuses.contains(&item.status))
            .filter(|item| !query.reported || !item.reports.is_empty())
            .filter(|item| {
                !query.pending_revision
                    || item
                        .pending
                        .as_ref()
                        .is_some_and(|p| p.status == CommunityItemStatus::PendingReview)
            })
            .cloned()
            .collect();
        match query.sort {
            CommunitySort::New => items.sort_by(|a, b| b.id.cmp(&a.id)),
            CommunitySort::Popular => items.sort_by(|a, b| {
                b.popularity
                    .cmp(&a.popularity)
                    .then_with(|| b.id.cmp(&a.id))
            }),
        }
        Ok(items
            .into_iter()
            .skip(query.offset)
            .take(query.limit.clamp(1, 100))
            .collect())
    }

    async fn count_community_items_by_creator(
        &self,
        creator_id: &str,
        statuses: &[CommunityItemStatus],
    ) -> Result<usize> {
        Ok(self
            .community_items
            .lock()
            .await
            .values()
            .filter(|item| item.creator_id == creator_id && statuses.contains(&item.status))
            .count())
    }

    async fn report_community_item(&self, id: &str, user_id: &str) -> Result<usize> {
        let mut items = self.community_items.lock().await;
        let item = items.get_mut(id).ok_or_else(|| create_error!(NotFound))?;
        if !item.reports.iter().any(|reporter| reporter == user_id) {
            item.reports.push(user_id.to_string());
        }
        Ok(item.reports.len())
    }

    async fn update_community_item(
        &self,
        item: &CommunityItem,
        expected_version: i64,
        clear_reports: bool,
    ) -> Result<bool> {
        let mut items = self.community_items.lock().await;
        let stored = items.get_mut(&item.id).ok_or_else(|| create_error!(NotFound))?;
        if stored.version != expected_version {
            return Ok(false);
        }
        let reports = if clear_reports {
            vec![]
        } else {
            std::mem::take(&mut stored.reports)
        };
        let popularity = stored.popularity;
        *stored = CommunityItem {
            reports,
            popularity,
            ..item.clone()
        };
        Ok(true)
    }

    async fn delete_community_item(&self, id: &str) -> Result<()> {
        self.community_favourites
            .lock()
            .await
            .retain(|(_, item)| item != id);
        let prefix = format!("{id}:");
        self.community_uses
            .lock()
            .await
            .retain(|entry| !entry.starts_with(&prefix));
        self.community_reviews
            .lock()
            .await
            .retain(|review| review.item_id != id);
        self.community_items.lock().await.remove(id);
        Ok(())
    }

    async fn unset_community_item_from_users(&self, id: &str) -> Result<()> {
        let custom = format!("c:{id}");
        for user in self.users.lock().await.values_mut() {
            if let Some(style) = user.style.as_mut() {
                if style.decoration.as_deref() == Some(&custom) {
                    style.decoration = None;
                    style.decoration_file = None;
                }
                if style.nameplate.as_deref() == Some(&custom) {
                    style.nameplate = None;
                    style.nameplate_file = None;
                }
                if style.effect.as_deref() == Some(&custom) {
                    style.effect = None;
                    style.effect_file = None;
                }
            }
        }
        Ok(())
    }

    async fn set_community_item_file_on_users(&self, id: &str, file_id: &str) -> Result<()> {
        let custom = format!("c:{id}");
        for user in self.users.lock().await.values_mut() {
            if let Some(style) = user.style.as_mut() {
                if style.decoration.as_deref() == Some(&custom) {
                    style.decoration_file = Some(file_id.to_string());
                }
                if style.nameplate.as_deref() == Some(&custom) {
                    style.nameplate_file = Some(file_id.to_string());
                }
                if style.effect.as_deref() == Some(&custom) {
                    style.effect_file = Some(file_id.to_string());
                }
            }
        }
        Ok(())
    }

    async fn community_item_wearers(&self, id: &str, limit: usize) -> Result<Vec<String>> {
        let custom = Some(format!("c:{id}"));
        Ok(self
            .users
            .lock()
            .await
            .values()
            .filter(|user| {
                user.style.as_ref().is_some_and(|style| {
                    style.decoration == custom || style.nameplate == custom || style.effect == custom
                })
            })
            .take(limit)
            .map(|user| user.id.clone())
            .collect())
    }

    async fn record_community_item_use(&self, id: &str, user_id: &str) -> Result<()> {
        if self
            .community_uses
            .lock()
            .await
            .insert(format!("{id}:{user_id}"))
        {
            if let Some(item) = self.community_items.lock().await.get_mut(id) {
                item.popularity += 1;
            }
        }
        Ok(())
    }

    async fn insert_community_review(&self, review: &CommunityReview) -> Result<()> {
        self.community_reviews.lock().await.push(review.clone());
        Ok(())
    }

    async fn fetch_community_reviews(&self, item_id: &str) -> Result<Vec<CommunityReview>> {
        let mut list: Vec<_> = self
            .community_reviews
            .lock()
            .await
            .iter()
            .filter(|review| review.item_id == item_id)
            .cloned()
            .collect();
        list.reverse();
        Ok(list)
    }

    async fn add_community_favourite(&self, user_id: &str, item_id: &str) -> Result<()> {
        let mut favourites = self.community_favourites.lock().await;
        let entry = (user_id.to_string(), item_id.to_string());
        if !favourites.contains(&entry) {
            favourites.push(entry);
        }
        Ok(())
    }

    async fn remove_community_favourite(&self, user_id: &str, item_id: &str) -> Result<()> {
        self.community_favourites
            .lock()
            .await
            .retain(|(user, item)| !(user == user_id && item == item_id));
        Ok(())
    }

    async fn fetch_community_favourites(&self, user_id: &str) -> Result<Vec<String>> {
        Ok(self
            .community_favourites
            .lock()
            .await
            .iter()
            .rev()
            .filter(|(user, _)| user == user_id)
            .map(|(_, item)| item.clone())
            .collect())
    }

    async fn fetch_platform_role(&self, user_id: &str) -> Result<Option<PlatformRole>> {
        Ok(self.platform_roles.lock().await.get(user_id).cloned())
    }

    async fn fetch_platform_roles(&self, user_ids: &[String]) -> Result<Vec<PlatformRole>> {
        let roles = self.platform_roles.lock().await;
        Ok(user_ids
            .iter()
            .filter_map(|id| roles.get(id).cloned())
            .collect())
    }

    async fn fetch_suspended_creators(&self) -> Result<Vec<String>> {
        Ok(self
            .platform_roles
            .lock()
            .await
            .values()
            .filter(|role| role.creator == Some(crate::CreatorStanding::Suspended))
            .map(|role| role.id.clone())
            .collect())
    }

    async fn fetch_all_platform_roles(&self) -> Result<Vec<PlatformRole>> {
        let mut list: Vec<_> = self.platform_roles.lock().await.values().cloned().collect();
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(list)
    }

    async fn save_platform_role(&self, role: &PlatformRole) -> Result<()> {
        self.platform_roles
            .lock()
            .await
            .insert(role.id.clone(), role.clone());
        Ok(())
    }

    async fn insert_creator_log(&self, log: &CreatorPermissionLog) -> Result<()> {
        self.creator_logs.lock().await.push(log.clone());
        Ok(())
    }

    async fn fetch_creator_logs(&self, user_id: &str) -> Result<Vec<CreatorPermissionLog>> {
        Ok(self
            .creator_logs
            .lock()
            .await
            .iter()
            .rev()
            .filter(|log| log.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn search_users_by_name(&self, query: &str, limit: usize) -> Result<Vec<User>> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Ok(vec![]);
        }
        Ok(self
            .users
            .lock()
            .await
            .values()
            .filter(|user| {
                user.id.to_lowercase() == query
                    || user.username.to_lowercase().contains(&query)
                    || user
                        .display_name
                        .as_ref()
                        .is_some_and(|name| name.to_lowercase().contains(&query))
            })
            .take(limit.clamp(1, 50))
            .cloned()
            .collect())
    }
}
