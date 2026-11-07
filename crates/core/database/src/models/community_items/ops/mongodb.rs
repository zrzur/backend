use futures::StreamExt;
use mongodb::options::{FindOptions, ReplaceOptions, UpdateOptions};
use revolt_result::Result;

use crate::{
    CommunityItem, CommunityItemStatus, CommunityReview, CreatorPermissionLog, CreatorStanding,
    MongoDb, PlatformRole, User,
};

use super::{AbstractCommunityItems, CommunityItemQuery, CommunitySort};

static COL: &str = "community_items";
static USERS: &str = "users";
static REVIEWS: &str = "community_reviews";
static FAVOURITES: &str = "community_favourites";
static USES: &str = "community_uses";
static ROLES: &str = "platform_roles";
static CREATOR_LOG: &str = "creator_permissions";

fn to_bson<T: serde::Serialize>(value: &T) -> bson::Bson {
    bson::to_bson(value).expect("failed to serialize")
}

fn is_duplicate(error: &mongodb::error::Error) -> bool {
    error.to_string().contains("E11000")
}

#[async_trait]
impl AbstractCommunityItems for MongoDb {
    async fn insert_community_item(&self, item: &CommunityItem) -> Result<()> {
        query!(self, insert_one, COL, &item).map(|_| ())
    }

    async fn fetch_community_item(&self, id: &str) -> Result<CommunityItem> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_community_items(
        &self,
        query: CommunityItemQuery<'_>,
    ) -> Result<Vec<CommunityItem>> {
        let mut filter = doc! {};
        if let Some(kind) = &query.kind {
            filter.insert("kind", to_bson(kind));
        }
        if let Some(search) = query.search.map(str::trim).filter(|s| !s.is_empty()) {
            let pattern = doc! { "$regex": regex::escape(search), "$options": "i" };
            let mut any = vec![doc! { "name": pattern.clone() }, doc! { "tags": pattern }];
            if !query.search_creators.is_empty() {
                any.push(doc! { "creator_id": { "$in": &query.search_creators } });
            }
            filter.insert("$or", any);
        }
        if let Some(creator_id) = query.creator_id {
            filter.insert("creator_id", creator_id);
        }
        if !query.hidden_creators.is_empty() {
            filter.insert(
                "$and",
                vec![doc! { "creator_id": { "$nin": &query.hidden_creators } }],
            );
        }
        if let Some(ids) = &query.ids {
            filter.insert("_id", doc! { "$in": ids });
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
        if query.pending_revision {
            filter.insert(
                "pending.status",
                to_bson(&CommunityItemStatus::PendingReview),
            );
        }

        let sort = match query.sort {
            CommunitySort::New => doc! { "_id": -1 },
            CommunitySort::Popular => doc! { "popularity": -1, "_id": -1 },
        };

        self.find_with_options(
            COL,
            filter,
            FindOptions::builder()
                .sort(sort)
                .skip(query.offset as u64)
                .limit(query.limit.clamp(1, 100) as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn count_community_items_by_creator(
        &self,
        creator_id: &str,
        statuses: &[CommunityItemStatus],
    ) -> Result<usize> {
        self.col::<CommunityItem>(COL)
            .count_documents(doc! {
                "creator_id": creator_id,
                "status": { "$in": statuses.iter().map(to_bson).collect::<Vec<_>>() },
            })
            .await
            .map(|count| count as usize)
            .map_err(|_| create_database_error!("count_documents", COL))
    }

    async fn report_community_item(&self, id: &str, user_id: &str) -> Result<usize> {
        self.col::<CommunityItem>(COL)
            .update_one(doc! { "_id": id }, doc! { "$addToSet": { "reports": user_id } })
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        Ok(self.fetch_community_item(id).await?.reports.len())
    }

    async fn update_community_item(
        &self,
        item: &CommunityItem,
        expected_version: i64,
        clear_reports: bool,
    ) -> Result<bool> {
        let mut set = doc! {
            "status": to_bson(&item.status),
            "file": to_bson(&item.file),
            "name": &item.name,
            "tags": to_bson(&item.tags),
            "revision": item.revision as i64,
            "version": item.version,
        };
        let mut unset = doc! {};
        for (key, value) in [
            ("description", item.description.as_ref().map(to_bson)),
            ("published_at", item.published_at.map(bson::Bson::Int64)),
            ("reviewed_by", item.reviewed_by.as_ref().map(to_bson)),
            ("review_note", item.review_note.as_ref().map(to_bson)),
            ("pending", item.pending.as_ref().map(to_bson)),
            ("rights", item.rights.as_ref().map(to_bson)),
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
        if clear_reports {
            unset.insert("reports", 1);
        }
        let mut update = doc! { "$set": set };
        if !unset.is_empty() {
            update.insert("$unset", unset);
        }

        // Old documents have no version field: treat it as 0
        let version_filter = if expected_version == 0 {
            doc! { "$in": [0_i64, bson::Bson::Null] }
        } else {
            doc! { "$eq": expected_version }
        };

        self.col::<CommunityItem>(COL)
            .update_one(doc! { "_id": &item.id, "version": version_filter }, update)
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn delete_community_item(&self, id: &str) -> Result<()> {
        for (collection, filter) in [
            (FAVOURITES, doc! { "item_id": id }),
            (USES, doc! { "item": id }),
            (REVIEWS, doc! { "item_id": id }),
        ] {
            self.col::<bson::Document>(collection)
                .delete_many(filter)
                .await
                .map_err(|_| create_database_error!("delete_many", collection))?;
        }
        // last: a failure above leaves the item to be found again on the retry
        self.col::<bson::Document>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", COL))
    }

    async fn unset_community_item_from_users(&self, id: &str) -> Result<()> {
        let custom = format!("c:{id}");
        for (field, file) in [
            ("style.decoration", "style.decoration_file"),
            ("style.nameplate", "style.nameplate_file"),
            ("style.effect", "style.effect_file"),
        ] {
            self.col::<bson::Document>(USERS)
                .update_many(
                    doc! { field: &custom },
                    doc! { "$unset": { field: 1, file: 1 } },
                )
                .await
                .map_err(|_| create_database_error!("update_many", USERS))?;
        }
        Ok(())
    }

    async fn set_community_item_file_on_users(&self, id: &str, file_id: &str) -> Result<()> {
        let custom = format!("c:{id}");
        for (field, file) in [
            ("style.decoration", "style.decoration_file"),
            ("style.nameplate", "style.nameplate_file"),
            ("style.effect", "style.effect_file"),
        ] {
            self.col::<bson::Document>(USERS)
                .update_many(doc! { field: &custom }, doc! { "$set": { file: file_id } })
                .await
                .map_err(|_| create_database_error!("update_many", USERS))?;
        }
        Ok(())
    }

    async fn community_item_wearers(&self, id: &str, limit: usize) -> Result<Vec<String>> {
        let custom = format!("c:{id}");
        let mut cursor = self
            .col::<bson::Document>(USERS)
            .find(doc! { "$or": [
                { "style.decoration": &custom },
                { "style.nameplate": &custom },
                { "style.effect": &custom },
            ] })
            .projection(doc! { "_id": 1 })
            .limit(limit as i64)
            .await
            .map_err(|_| create_database_error!("find", USERS))?;
        let mut ids = vec![];
        while cursor
            .advance()
            .await
            .map_err(|_| create_database_error!("find", USERS))?
        {
            if let Ok(document) = cursor.deserialize_current() {
                if let Ok(user) = document.get_str("_id") {
                    ids.push(user.to_string());
                }
            }
        }
        Ok(ids)
    }

    async fn record_community_item_use(&self, id: &str, user_id: &str) -> Result<()> {
        let inserted = self
            .col::<bson::Document>(USES)
            .insert_one(doc! { "_id": format!("{id}:{user_id}"), "item": id, "user": user_id })
            .await;
        match inserted {
            Ok(_) => {
                self.col::<CommunityItem>(COL)
                    .update_one(doc! { "_id": id }, doc! { "$inc": { "popularity": 1_i64 } })
                    .await
                    .map_err(|_| create_database_error!("update_one", COL))?;
                Ok(())
            }
            Err(error) if is_duplicate(&error) => Ok(()),
            Err(_) => Err(create_database_error!("insert_one", USES)),
        }
    }

    async fn insert_community_review(&self, review: &CommunityReview) -> Result<()> {
        query!(self, insert_one, REVIEWS, &review).map(|_| ())
    }

    async fn fetch_community_reviews(&self, item_id: &str) -> Result<Vec<CommunityReview>> {
        self.find_with_options(
            REVIEWS,
            doc! { "item_id": item_id },
            FindOptions::builder()
                .sort(doc! { "_id": -1 })
                .limit(100)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", REVIEWS))
    }

    async fn add_community_favourite(&self, user_id: &str, item_id: &str) -> Result<()> {
        self.col::<bson::Document>(FAVOURITES)
            .update_one(
                doc! { "_id": format!("{user_id}:{item_id}") },
                doc! { "$setOnInsert": {
                    "user_id": user_id,
                    "item_id": item_id,
                    "created_at": crate::epoch_ms(iso8601_timestamp::Timestamp::now_utc()),
                } },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", FAVOURITES))
    }

    async fn remove_community_favourite(&self, user_id: &str, item_id: &str) -> Result<()> {
        self.col::<bson::Document>(FAVOURITES)
            .delete_one(doc! { "_id": format!("{user_id}:{item_id}") })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", FAVOURITES))
    }

    async fn fetch_community_favourites(&self, user_id: &str) -> Result<Vec<String>> {
        let mut cursor = self
            .col::<bson::Document>(FAVOURITES)
            .find(doc! { "user_id": user_id })
            .sort(doc! { "created_at": -1 })
            .limit(500)
            .await
            .map_err(|_| create_database_error!("find", FAVOURITES))?;
        let mut ids = vec![];
        while let Some(Ok(doc)) = cursor.next().await {
            if let Ok(id) = doc.get_str("item_id") {
                ids.push(id.to_string());
            }
        }
        Ok(ids)
    }

    async fn fetch_platform_role(&self, user_id: &str) -> Result<Option<PlatformRole>> {
        query!(self, find_one_by_id, ROLES, user_id)
    }

    async fn fetch_platform_roles(&self, user_ids: &[String]) -> Result<Vec<PlatformRole>> {
        self.find_with_options(ROLES, doc! { "_id": { "$in": user_ids } }, None)
            .await
            .map_err(|_| create_database_error!("find", ROLES))
    }

    async fn fetch_suspended_creators(&self) -> Result<Vec<String>> {
        let roles: Vec<PlatformRole> = self
            .find(ROLES, doc! { "creator": to_bson(&CreatorStanding::Suspended) })
            .await
            .map_err(|_| create_database_error!("find", ROLES))?;
        Ok(roles.into_iter().map(|role| role.id).collect())
    }

    async fn fetch_all_platform_roles(&self) -> Result<Vec<PlatformRole>> {
        self.find_with_options(
            ROLES,
            doc! {},
            FindOptions::builder()
                .sort(doc! { "updated_at": -1 })
                .limit(500)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", ROLES))
    }

    async fn save_platform_role(&self, role: &PlatformRole) -> Result<()> {
        self.col::<PlatformRole>(ROLES)
            .replace_one(doc! { "_id": &role.id }, role)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", ROLES))
    }

    async fn insert_creator_log(&self, log: &CreatorPermissionLog) -> Result<()> {
        query!(self, insert_one, CREATOR_LOG, &log).map(|_| ())
    }

    async fn fetch_creator_logs(&self, user_id: &str) -> Result<Vec<CreatorPermissionLog>> {
        self.find_with_options(
            CREATOR_LOG,
            doc! { "user_id": user_id },
            FindOptions::builder()
                .sort(doc! { "_id": -1 })
                .limit(100)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", CREATOR_LOG))
    }

    async fn search_users_by_name(&self, query: &str, limit: usize) -> Result<Vec<User>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(vec![]);
        }
        let pattern = doc! { "$regex": regex::escape(query), "$options": "i" };
        let mut filter = doc! { "$or": [
            { "username": pattern.clone() },
            { "display_name": pattern },
        ] };
        // exact id lookups help staff
        if query.len() == 26 {
            filter = doc! { "$or": [ { "_id": query }, filter ] };
        }
        self.find_with_options(
            USERS,
            filter,
            FindOptions::builder()
                .limit(limit.clamp(1, 50) as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", USERS))
    }
}
