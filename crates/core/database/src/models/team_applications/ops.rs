use revolt_result::Result;

use crate::{TeamApplication, TeamApplicationStatus};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractTeamApplications: Sync + Send {
    async fn insert_team_application(&self, application: &TeamApplication) -> Result<()>;

    async fn fetch_team_application(&self, id: &str) -> Result<TeamApplication>;

    /// Every application of one person that still exists, newest first
    async fn fetch_team_applications_of(&self, user_id: &str) -> Result<Vec<TeamApplication>>;

    /// Applications waiting for a decision (open or in interview), oldest first
    async fn fetch_open_team_applications(&self) -> Result<Vec<TeamApplication>>;

    /// Accepted applications whose role waits for the person to join the server
    async fn fetch_team_applications_pending_grant(&self) -> Result<Vec<TeamApplication>>;

    /// Replace an application only while its status is still `expected`;
    /// `false` when someone changed it in between
    async fn replace_team_application_if(
        &self,
        application: &TeamApplication,
        expected: TeamApplicationStatus,
    ) -> Result<bool>;

    async fn delete_team_application(&self, id: &str) -> Result<()>;

    /// Delete applications whose `purge_at` lies before `now` (milliseconds
    /// since the epoch); returns how many went
    async fn delete_team_applications_due(&self, now: i64) -> Result<u64>;

    /// Delete everything a person ever sent (account deletion)
    async fn delete_team_applications_of(&self, user_id: &str) -> Result<u64>;
}
