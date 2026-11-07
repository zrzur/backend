use revolt_result::Result;

use crate::PolicyChange;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractPolicyChange: Sync + Send {
    /// Fetch all policy changes
    async fn fetch_policy_changes(&self) -> Result<Vec<PolicyChange>>;

    /// Acknowledge policy changes
    async fn acknowledge_policy_changes(&self, user_id: &str) -> Result<()>;

    /// Publish a policy change: everyone who hasn't acknowledged it sees it
    async fn insert_policy_change(&self, change: &PolicyChange) -> Result<()>;

    /// Withdraw a policy change
    async fn delete_policy_change(&self, id: &str) -> Result<()>;
}
