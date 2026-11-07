use revolt_result::Result;

use crate::{DiscoverRequest, DiscoverRequestStatus, DiscoverRequestType};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractDiscoverRequest: Sync + Send {
    /// Insert discover request into database.
    /// Update an existing one if it was previously denied
    async fn insert_discover_request(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<DiscoverRequest>;

    /// Fetch Discover request by their parent id
    async fn fetch_discover_request_by_item_id(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<DiscoverRequest>;

    /// Fetch the Discover requests of several items of one type
    async fn fetch_discover_requests_by_item_ids(
        &self,
        request_type: DiscoverRequestType,
        items: &[String],
    ) -> Result<Vec<DiscoverRequest>>;

    /// Fetch requests still awaiting a decision (pending or under review)
    async fn fetch_open_discover_requests(&self) -> Result<Vec<DiscoverRequest>>;

    /// Set the status of an existing Discover request
    async fn update_discover_request_status(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
        status: DiscoverRequestStatus,
    ) -> Result<()>;

    /// Replace an existing Discover request (listing details, featured flag)
    async fn save_discover_request(&self, request: &DiscoverRequest) -> Result<()>;

    /// Remove Discover request
    async fn delete_discover_request(
        &self,
        request_type: DiscoverRequestType,
        item: &str,
    ) -> Result<()>;

    /// Fetch if the item is banned from being requested. If the item is Some, then the item is banned.
    async fn get_discover_ban(&self, item_type: DiscoverRequestType, item: &str) -> Result<bool>;
}
