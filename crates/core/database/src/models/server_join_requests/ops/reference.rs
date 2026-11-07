use iso8601_timestamp::Timestamp;
use revolt_result::Result;

use crate::ReferenceDb;
use crate::{JoinRequestStatus, MemberCompositeKey, ServerJoinRequest};

use super::AbstractServerJoinRequests;

#[async_trait]
impl AbstractServerJoinRequests for ReferenceDb {
    async fn save_join_request(&self, request: &ServerJoinRequest) -> Result<()> {
        let mut requests = self.server_join_requests.lock().await;
        requests.insert(request.id.clone(), request.clone());
        Ok(())
    }

    async fn fetch_join_request(
        &self,
        server_id: &str,
        user_id: &str,
    ) -> Result<ServerJoinRequest> {
        let requests = self.server_join_requests.lock().await;
        requests
            .get(&MemberCompositeKey {
                server: server_id.to_string(),
                user: user_id.to_string(),
            })
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_join_requests(
        &self,
        server_id: &str,
        status: JoinRequestStatus,
    ) -> Result<Vec<ServerJoinRequest>> {
        let requests = self.server_join_requests.lock().await;
        let mut list: Vec<ServerJoinRequest> = requests
            .values()
            .filter(|request| request.id.server == server_id && request.status == status)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(list)
    }

    async fn decide_join_request(
        &self,
        id: &MemberCompositeKey,
        to: JoinRequestStatus,
        reviewer: &str,
    ) -> Result<bool> {
        let mut requests = self.server_join_requests.lock().await;
        match requests.get_mut(id) {
            Some(request) if request.status == JoinRequestStatus::Pending => {
                request.status = to;
                request.reviewed_by = Some(reviewer.to_string());
                request.reviewed_at = Some(Timestamp::now_utc());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete_join_request(&self, id: &MemberCompositeKey) -> Result<()> {
        let mut requests = self.server_join_requests.lock().await;
        requests.remove(id);
        Ok(())
    }
}
