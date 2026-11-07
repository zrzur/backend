use revolt_result::Result;

use crate::ReferenceDb;
use crate::{DiscoverRequest, DiscoverRequestStatus, DiscoverRequestType};

use super::AbstractDiscoverRequest;

#[async_trait]
impl AbstractDiscoverRequest for ReferenceDb {
    /// Insert request into database.
    async fn insert_discover_request(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<DiscoverRequest> {
        let ret = DiscoverRequest {
            request_type: request_type.clone(),
            request_id: item.to_string(),
            status: DiscoverRequestStatus::Pending,
            tags: vec![],
            category: None,
            language: None,
            featured: false,
        };

        let mut discover = self.discover_requests.lock().await;
        match discover.entry((request_type, item.to_string())) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(ret.clone());
                Ok(ret)
            }
            // Mirror MongoDB: only a denied request may be resubmitted
            std::collections::hash_map::Entry::Occupied(mut e) => match e.get().status {
                DiscoverRequestStatus::Denied(_) => {
                    e.get_mut().status = DiscoverRequestStatus::Pending;
                    Ok(e.get().clone())
                }
                DiscoverRequestStatus::Removed(_) => Err(create_error!(ContactSupport {
                    locale: "discover.removed_cannot_apply".to_string(),
                    msg: "Your server/bot was removed from Discover. Please contact support for more information.".to_string()
                })),
                _ => Err(create_error!(NoEffect)),
            },
        }
    }

    /// Fetch discover by item type/id combo
    async fn fetch_discover_request_by_item_id(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<DiscoverRequest> {
        let discover = self.discover_requests.lock().await;
        discover
            .get(&(request_type, item.to_string()))
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch the requests of several items of one type
    async fn fetch_discover_requests_by_item_ids(
        &self,
        request_type: DiscoverRequestType,
        items: &[String],
    ) -> Result<Vec<DiscoverRequest>> {
        let discover = self.discover_requests.lock().await;
        Ok(items
            .iter()
            .filter_map(|item| discover.get(&(request_type.clone(), item.clone())).cloned())
            .collect())
    }

    /// Fetch requests still awaiting a decision
    async fn fetch_open_discover_requests(&self) -> Result<Vec<DiscoverRequest>> {
        let discover = self.discover_requests.lock().await;
        Ok(discover
            .values()
            .filter(|d| {
                matches!(
                    d.status,
                    DiscoverRequestStatus::Pending | DiscoverRequestStatus::UnderReview
                )
            })
            .cloned()
            .collect())
    }

    /// Set the status of an existing discover request
    async fn update_discover_request_status(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
        status: DiscoverRequestStatus,
    ) -> Result<()> {
        let mut discover = self.discover_requests.lock().await;
        let request = discover
            .get_mut(&(request_type, item.to_string()))
            .ok_or_else(|| create_error!(NotFound))?;
        request.status = status;
        Ok(())
    }

    /// Replace an existing discover request
    async fn save_discover_request(&self, request: &DiscoverRequest) -> Result<()> {
        let mut discover = self.discover_requests.lock().await;
        let existing = discover
            .get_mut(&(request.request_type.clone(), request.request_id.clone()))
            .ok_or_else(|| create_error!(NotFound))?;
        *existing = request.clone();
        Ok(())
    }

    /// Remove discover request
    async fn delete_discover_request(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<()> {
        let mut discover = self.discover_requests.lock().await;
        discover
            .remove(&(request_type, item.to_string()))
            .map(|_| ())
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch if the item is banned from being requested
    async fn get_discover_ban(&self, item_type: DiscoverRequestType, item: &str) -> Result<bool> {
        let discover = self.discover_bans.lock().await;
        discover
            .iter()
            .find(|(_, d)| d.item_id == item && d.item_type == item_type)
            .map(|(_, _)| true)
            .ok_or_else(|| create_error!(NotFound))
    }
}
