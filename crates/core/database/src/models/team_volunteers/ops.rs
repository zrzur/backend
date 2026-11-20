use revolt_result::Result;

use crate::{TeamActivityDay, TeamIdentity, TeamMember, TeamMemberFilter};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractTeamVolunteers: Sync + Send {
    async fn upsert_team_identity(&self, identity: &TeamIdentity) -> Result<()>;

    async fn fetch_team_identity(&self, user_id: &str) -> Result<Option<TeamIdentity>>;

    async fn delete_team_identity(&self, user_id: &str) -> Result<()>;

    /// Delete identities whose `purge_at` lies before `now` (milliseconds
    /// since the epoch); returns how many went
    async fn delete_team_identities_due(&self, now: i64) -> Result<u64>;

    /// Fails when the person already has a membership in that role
    async fn insert_team_member(&self, member: &TeamMember) -> Result<()>;

    async fn fetch_team_member(&self, id: &str) -> Result<TeamMember>;

    async fn fetch_team_members_of(&self, user_id: &str) -> Result<Vec<TeamMember>>;

    async fn fetch_team_members(&self, filter: &TeamMemberFilter) -> Result<Vec<TeamMember>>;

    async fn update_team_member(&self, member: &TeamMember) -> Result<()>;

    /// Delete every membership of a person (account deletion)
    async fn delete_team_members_of(&self, user_id: &str) -> Result<u64>;

    async fn upsert_team_activity_day(&self, day: &TeamActivityDay) -> Result<()>;

    /// Days from `from_day` to `to_day`, both included (yyyymmdd)
    async fn fetch_team_activity(
        &self,
        user_id: &str,
        from_day: i32,
        to_day: i32,
    ) -> Result<Vec<TeamActivityDay>>;

    /// Delete days earlier than `day`; returns how many went
    async fn delete_team_activity_before(&self, day: i32) -> Result<u64>;

    /// Delete every activity day of a person (account deletion)
    async fn delete_team_activity_of(&self, user_id: &str) -> Result<u64>;
}
