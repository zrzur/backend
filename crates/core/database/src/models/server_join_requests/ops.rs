use revolt_result::Result;

use crate::{JoinRequestStatus, MemberCompositeKey, ServerJoinRequest};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractServerJoinRequests: Sync + Send {
    /// Insert or replace the request of this server + user
    async fn save_join_request(&self, request: &ServerJoinRequest) -> Result<()>;

    /// Fetch the request of a user for a server
    async fn fetch_join_request(&self, server_id: &str, user_id: &str)
        -> Result<ServerJoinRequest>;

    /// Fetch a server's requests with a given status, oldest first
    async fn fetch_join_requests(
        &self,
        server_id: &str,
        status: JoinRequestStatus,
    ) -> Result<Vec<ServerJoinRequest>>;

    /// Move a request from `Pending` to `to` in one step. Returns false when
    /// it was no longer pending (someone else decided first).
    async fn decide_join_request(
        &self,
        id: &MemberCompositeKey,
        to: JoinRequestStatus,
        reviewer: &str,
    ) -> Result<bool>;

    /// Delete a request (cancel)
    async fn delete_join_request(&self, id: &MemberCompositeKey) -> Result<()>;
}
