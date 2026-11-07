use revolt_result::Result;

use crate::{NameRecord, NameScan, OfficialMarkRequest, OfficialMarkStatus, User};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractOfficialMarkRequest: Sync + Send {
    async fn insert_official_mark_request(&self, request: &OfficialMarkRequest) -> Result<()>;

    async fn fetch_official_mark_request(&self, id: &str) -> Result<OfficialMarkRequest>;

    /// The latest proposals, newest first
    async fn fetch_official_mark_requests(&self) -> Result<Vec<OfficialMarkRequest>>;

    /// Replace a proposal if it still has status `expected`; false otherwise
    async fn decide_official_mark_request(
        &self,
        request: &OfficialMarkRequest,
        expected: OfficialMarkStatus,
    ) -> Result<bool>;

    /// Every account with the official mark
    async fn fetch_official_users(&self) -> Result<Vec<User>>;

    /// One page of names, for the list of names that break the name rule.
    /// A page comes back empty when the scan is done.
    async fn fetch_name_records(
        &self,
        scan: NameScan,
        offset: u64,
        limit: i64,
    ) -> Result<Vec<NameRecord>>;
}
