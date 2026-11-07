use mongodb::options::{FindOptions, UpdateOptions};
use revolt_result::Result;

use crate::{MongoDb, PlatformNotice};

use super::AbstractPlatformNotice;

static COL: &str = "platform_notices";
static DISMISSALS: &str = "notice_dismissals";

#[async_trait]
impl AbstractPlatformNotice for MongoDb {
    async fn insert_platform_notice(&self, notice: &PlatformNotice) -> Result<()> {
        query!(self, insert_one, COL, &notice).map(|_| ())
    }

    async fn fetch_platform_notice(&self, id: &str) -> Result<PlatformNotice> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_platform_notices(&self, user_id: Option<&str>, limit: usize) -> Result<Vec<PlatformNotice>> {
        let filter = match user_id {
            Some(user) => doc! { "user_id": user },
            None => doc! {},
        };
        self.find_with_options(
            COL,
            filter,
            FindOptions::builder().sort(doc! { "_id": -1 }).limit(limit as i64).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn fetch_platform_notices_for(&self, user_id: &str, now: i64) -> Result<Vec<PlatformNotice>> {
        let live: Vec<PlatformNotice> = self
            .find_with_options(
                COL,
                doc! {
                    "$and": [
                        { "$or": [ { "user_id": user_id }, { "user_id": { "$exists": false } } ] },
                        { "withdrawn_at": { "$exists": false } },
                        { "$or": [ { "expires_at": { "$exists": false } }, { "expires_at": { "$gt": now } } ] },
                    ]
                },
                FindOptions::builder().sort(doc! { "_id": -1 }).limit(20).build(),
            )
            .await
            .map_err(|_| create_database_error!("find", COL))?;
        if live.is_empty() {
            return Ok(live);
        }
        let ids: Vec<String> = live.iter().map(|n| format!("{}:{}", n.id, user_id)).collect();
        let dismissed: Vec<String> = self
            .col::<bson::Document>(DISMISSALS)
            .distinct("_id", doc! { "_id": { "$in": &ids } })
            .await
            .map_err(|_| create_database_error!("distinct", DISMISSALS))?
            .into_iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        Ok(live
            .into_iter()
            .filter(|n| !dismissed.contains(&format!("{}:{}", n.id, user_id)))
            .collect())
    }

    async fn withdraw_platform_notice(&self, id: &str, at: i64) -> Result<()> {
        let result = self
            .col::<PlatformNotice>(COL)
            .update_one(doc! { "_id": id }, doc! { "$set": { "withdrawn_at": at } })
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        if result.matched_count == 0 {
            return Err(create_error!(NotFound));
        }
        Ok(())
    }

    async fn set_platform_notice_delivery(&self, id: &str, emailed: bool, pushed: bool) -> Result<()> {
        self.col::<PlatformNotice>(COL)
            .update_one(doc! { "_id": id }, doc! { "$set": { "emailed": emailed, "pushed": pushed } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn dismiss_platform_notice(&self, id: &str, user_id: &str) -> Result<()> {
        self.col::<bson::Document>(DISMISSALS)
            .update_one(
                doc! { "_id": format!("{id}:{user_id}") },
                doc! { "$setOnInsert": { "notice_id": id, "user_id": user_id } },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", DISMISSALS))
    }
}
