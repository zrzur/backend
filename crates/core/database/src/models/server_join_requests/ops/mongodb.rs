use bson::to_bson;
use iso8601_timestamp::Timestamp;
use mongodb::options::ReplaceOptions;
use revolt_result::Result;

use crate::MongoDb;
use crate::{JoinRequestStatus, MemberCompositeKey, ServerJoinRequest};

use super::AbstractServerJoinRequests;

static COL: &str = "server_join_requests";

fn status_name(status: &JoinRequestStatus) -> &'static str {
    match status {
        JoinRequestStatus::Pending => "Pending",
        JoinRequestStatus::Approved => "Approved",
        JoinRequestStatus::Rejected => "Rejected",
    }
}

#[async_trait]
impl AbstractServerJoinRequests for MongoDb {
    async fn save_join_request(&self, request: &ServerJoinRequest) -> Result<()> {
        // an upsert needs the whole _id in the filter (MongoDB refuses
        // "_id.server" / "_id.user" here: "field at '_id' must be exactly
        // specified"), in the order it is stored: server, then user
        let id = to_bson(&request.id).map_err(|_| create_database_error!("to_bson", COL))?;
        self.col::<ServerJoinRequest>(COL)
            .replace_one(doc! { "_id": id }, request)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", COL))
    }

    async fn fetch_join_request(
        &self,
        server_id: &str,
        user_id: &str,
    ) -> Result<ServerJoinRequest> {
        query!(
            self,
            find_one,
            COL,
            doc! {
                "_id.server": server_id,
                "_id.user": user_id
            }
        )?
        .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_join_requests(
        &self,
        server_id: &str,
        status: JoinRequestStatus,
    ) -> Result<Vec<ServerJoinRequest>> {
        let mut requests: Vec<ServerJoinRequest> = query!(
            self,
            find,
            COL,
            doc! {
                "_id.server": server_id,
                "status": status_name(&status)
            }
        )?;
        requests.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(requests)
    }

    async fn decide_join_request(
        &self,
        id: &MemberCompositeKey,
        to: JoinRequestStatus,
        reviewer: &str,
    ) -> Result<bool> {
        let now = to_bson(&Timestamp::now_utc())
            .map_err(|_| create_database_error!("to_bson", COL))?;
        self.col::<ServerJoinRequest>(COL)
            .update_one(
                // only a pending request can be decided, so two reviewers
                // acting at once cannot both win
                doc! {
                    "_id.server": &id.server,
                    "_id.user": &id.user,
                    "status": "Pending"
                },
                doc! {
                    "$set": {
                        "status": status_name(&to),
                        "reviewed_by": reviewer,
                        "reviewed_at": now
                    }
                },
            )
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn delete_join_request(&self, id: &MemberCompositeKey) -> Result<()> {
        query!(
            self,
            delete_one,
            COL,
            doc! {
                "_id.server": &id.server,
                "_id.user": &id.user
            }
        )
        .map(|_| ())
    }
}
