use revolt_result::Result;

use crate::DiscoverBan;
use crate::DiscoverRequest;
use crate::DiscoverRequestStatus;
use crate::DiscoverRequestType;
use crate::MongoDb;

use super::AbstractDiscoverRequest;

static DISCOVER_COL: &str = "discover_requests";
static DISCOVER_BANS_COL: &str = "discover_bans";

fn key(request_type: &DiscoverRequestType, item: &str) -> bson::Document {
    doc! {"request_type": bson::to_bson(request_type).expect("failed to serialize"), "request_id": item}
}

#[async_trait]
impl AbstractDiscoverRequest for MongoDb {
    /// Insert request into database.
    async fn insert_discover_request(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<DiscoverRequest> {
        if let Ok(mut prev) = self
            .fetch_discover_request_by_item_id(request_type.clone(), item)
            .await
        {
            match prev.status {
                DiscoverRequestStatus::Approved(_)
                | DiscoverRequestStatus::Pending
                | DiscoverRequestStatus::UnderReview => return Err(create_error!(NoEffect)),
                DiscoverRequestStatus::Removed(_) => {
                    return Err(create_error!(ContactSupport {
                        locale: "discover.removed_cannot_apply".to_string(),
                        msg: "Your server/bot was removed from Discover. Please contact support for more information.".to_string()
                    }))
                }
                _ => Ok(()),
            }?;
            self.col::<DiscoverRequest>(DISCOVER_COL).update_one(
                key(&request_type, item),
                doc! {"$set": {"status": bson::to_bson(&DiscoverRequestStatus::Pending).expect("failed to serialize")}},
            ).await.map_err(|_| create_database_error!("update_one", DISCOVER_COL))?;

            prev.status = DiscoverRequestStatus::Pending;
            Ok(prev)
        } else {
            let ret = DiscoverRequest {
                request_type,
                request_id: item.to_string(),
                status: DiscoverRequestStatus::Pending,
                tags: vec![],
                category: None,
                language: None,
                featured: false,
            };
            self.col::<DiscoverRequest>(DISCOVER_COL)
                .insert_one(ret.clone())
                .await
                .map_err(|_| create_database_error!("insert_one", DISCOVER_COL))?;
            Ok(ret)
        }
    }

    /// Fetch discover by item type/id combo
    async fn fetch_discover_request_by_item_id(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<DiscoverRequest> {
        query!(self, find_one, DISCOVER_COL, key(&request_type, item))?
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch the requests of several items of one type
    async fn fetch_discover_requests_by_item_ids(
        &self,
        request_type: DiscoverRequestType,
        items: &[String],
    ) -> Result<Vec<DiscoverRequest>> {
        query!(
            self,
            find,
            DISCOVER_COL,
            doc! {
                "request_type": bson::to_bson(&request_type).expect("failed to serialize"),
                "request_id": { "$in": items }
            }
        )
    }

    /// Fetch requests still awaiting a decision
    async fn fetch_open_discover_requests(&self) -> Result<Vec<DiscoverRequest>> {
        query!(
            self,
            find,
            DISCOVER_COL,
            doc! {"status": {"$in": [
                bson::to_bson(&DiscoverRequestStatus::Pending).expect("failed to serialize"),
                bson::to_bson(&DiscoverRequestStatus::UnderReview).expect("failed to serialize"),
            ]}}
        )
    }

    /// Set the status of an existing discover request
    async fn update_discover_request_status(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
        status: DiscoverRequestStatus,
    ) -> Result<()> {
        let result = self
            .col::<DiscoverRequest>(DISCOVER_COL)
            .update_one(
                key(&request_type, item),
                doc! {"$set": {"status": bson::to_bson(&status).expect("failed to serialize")}},
            )
            .await
            .map_err(|_| create_database_error!("update_one", DISCOVER_COL))?;

        if result.matched_count == 0 {
            Err(create_error!(NotFound))
        } else {
            Ok(())
        }
    }

    /// Replace an existing discover request
    async fn save_discover_request(&self, request: &DiscoverRequest) -> Result<()> {
        let result = self
            .col::<DiscoverRequest>(DISCOVER_COL)
            .replace_one(key(&request.request_type, &request.request_id), request)
            .await
            .map_err(|_| create_database_error!("replace_one", DISCOVER_COL))?;

        if result.matched_count == 0 {
            Err(create_error!(NotFound))
        } else {
            Ok(())
        }
    }

    /// Remove discover request
    async fn delete_discover_request(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<()> {
        query!(self, delete_one, DISCOVER_COL, key(&request_type, item)).map(|_| ())
    }

    /// Fetch if the item is banned from being requested
    async fn get_discover_ban(&self, item_type: DiscoverRequestType, item: &str) -> Result<bool> {
        query!(
            self,
            find_one,
            DISCOVER_BANS_COL,
            // DiscoverBan serialises its fields as `type` / `item_id`
            doc! {"type": bson::to_bson(&item_type).expect("failed to serialize"), "item_id": item}
        )?.ok_or_else(|| create_error!(NotFound)).map(|_: DiscoverBan| true)
    }
}
