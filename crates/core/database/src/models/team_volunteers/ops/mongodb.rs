use mongodb::options::{FindOptions, ReplaceOptions};
use revolt_result::Result;

use crate::{
    MongoDb, TeamActivityDay, TeamIdentity, TeamMember, TeamMemberFilter, TeamMemberStatus,
};

use super::AbstractTeamVolunteers;

static IDENTITIES: &str = "team_identities";
static MEMBERS: &str = "team_members";
static DAYS: &str = "team_activity_days";

fn status_name(status: TeamMemberStatus) -> &'static str {
    match status {
        TeamMemberStatus::Trial => "Trial",
        TeamMemberStatus::Active => "Active",
        TeamMemberStatus::Away => "Away",
        TeamMemberStatus::Inactive => "Inactive",
        TeamMemberStatus::Left => "Left",
    }
}

#[async_trait]
impl AbstractTeamVolunteers for MongoDb {
    async fn upsert_team_identity(&self, identity: &TeamIdentity) -> Result<()> {
        self.col::<TeamIdentity>(IDENTITIES)
            .replace_one(doc! { "_id": &identity.id }, identity)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", IDENTITIES))
    }

    async fn fetch_team_identity(&self, user_id: &str) -> Result<Option<TeamIdentity>> {
        query!(self, find_one_by_id, IDENTITIES, user_id)
    }

    async fn delete_team_identity(&self, user_id: &str) -> Result<()> {
        self.col::<TeamIdentity>(IDENTITIES)
            .delete_one(doc! { "_id": user_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", IDENTITIES))
    }

    async fn delete_team_identities_due(&self, now: i64) -> Result<u64> {
        self.col::<TeamIdentity>(IDENTITIES)
            .delete_many(doc! { "purge_at": { "$lt": now } })
            .await
            .map(|r| r.deleted_count)
            .map_err(|_| create_database_error!("delete_many", IDENTITIES))
    }

    async fn insert_team_member(&self, member: &TeamMember) -> Result<()> {
        self.col::<TeamMember>(MEMBERS)
            .insert_one(member)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.to_string().contains("E11000") {
                    create_error!(InvalidOperation)
                } else {
                    create_database_error!("insert_one", MEMBERS)
                }
            })
    }

    async fn fetch_team_member(&self, id: &str) -> Result<TeamMember> {
        query!(self, find_one_by_id, MEMBERS, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_team_members_of(&self, user_id: &str) -> Result<Vec<TeamMember>> {
        self.find_with_options(
            MEMBERS,
            doc! { "user_id": user_id },
            FindOptions::builder().sort(doc! { "_id": 1 }).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", MEMBERS))
    }

    async fn fetch_team_members(&self, filter: &TeamMemberFilter) -> Result<Vec<TeamMember>> {
        let mut query = doc! {};
        if let Some(role) = &filter.role {
            query.insert("role", role);
        }
        if let Some(status) = filter.status {
            query.insert("status", status_name(status));
        }
        if let Some(after) = &filter.after {
            query.insert("_id", doc! { "$gt": after });
        }
        // no limit means everything, as in the reference driver
        let options = FindOptions::builder()
            .sort(doc! { "_id": 1 })
            .limit(filter.limit.map(i64::from))
            .build();
        self.find_with_options(MEMBERS, query, options)
            .await
            .map_err(|_| create_database_error!("find", MEMBERS))
    }

    async fn update_team_member(&self, member: &TeamMember) -> Result<()> {
        let matched = self
            .col::<TeamMember>(MEMBERS)
            .replace_one(doc! { "_id": &member.id }, member)
            .await
            .map(|r| r.matched_count)
            .map_err(|_| create_database_error!("replace_one", MEMBERS))?;
        if matched == 1 {
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    async fn delete_team_members_of(&self, user_id: &str) -> Result<u64> {
        self.col::<TeamMember>(MEMBERS)
            .delete_many(doc! { "user_id": user_id })
            .await
            .map(|r| r.deleted_count)
            .map_err(|_| create_database_error!("delete_many", MEMBERS))
    }

    async fn upsert_team_activity_day(&self, day: &TeamActivityDay) -> Result<()> {
        self.col::<TeamActivityDay>(DAYS)
            .replace_one(doc! { "_id": &day.id }, day)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", DAYS))
    }

    async fn fetch_team_activity(
        &self,
        user_id: &str,
        from_day: i32,
        to_day: i32,
    ) -> Result<Vec<TeamActivityDay>> {
        self.find_with_options(
            DAYS,
            doc! { "user_id": user_id, "day": { "$gte": from_day, "$lte": to_day } },
            FindOptions::builder().sort(doc! { "day": 1 }).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", DAYS))
    }

    async fn delete_team_activity_before(&self, day: i32) -> Result<u64> {
        self.col::<TeamActivityDay>(DAYS)
            .delete_many(doc! { "day": { "$lt": day } })
            .await
            .map(|r| r.deleted_count)
            .map_err(|_| create_database_error!("delete_many", DAYS))
    }

    async fn delete_team_activity_of(&self, user_id: &str) -> Result<u64> {
        self.col::<TeamActivityDay>(DAYS)
            .delete_many(doc! { "user_id": user_id })
            .await
            .map(|r| r.deleted_count)
            .map_err(|_| create_database_error!("delete_many", DAYS))
    }
}
