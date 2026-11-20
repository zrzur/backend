use mongodb::options::FindOptions;
use revolt_result::Result;

use crate::{MongoDb, TeamApplication, TeamApplicationStatus};

use super::AbstractTeamApplications;

static COL: &str = "team_applications";

fn status_name(status: TeamApplicationStatus) -> &'static str {
    match status {
        TeamApplicationStatus::Open => "Open",
        TeamApplicationStatus::Interview => "Interview",
        TeamApplicationStatus::Accepted => "Accepted",
        TeamApplicationStatus::Rejected => "Rejected",
        TeamApplicationStatus::Withdrawn => "Withdrawn",
    }
}

#[async_trait]
impl AbstractTeamApplications for MongoDb {
    async fn insert_team_application(&self, application: &TeamApplication) -> Result<()> {
        query!(self, insert_one, COL, &application).map(|_| ())
    }

    async fn fetch_team_application(&self, id: &str) -> Result<TeamApplication> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_team_applications_of(&self, user_id: &str) -> Result<Vec<TeamApplication>> {
        self.find_with_options(
            COL,
            doc! { "user_id": user_id },
            FindOptions::builder().sort(doc! { "_id": -1 }).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn fetch_open_team_applications(&self) -> Result<Vec<TeamApplication>> {
        self.find_with_options(
            COL,
            doc! { "status": { "$in": ["Open", "Interview"] } },
            FindOptions::builder().sort(doc! { "_id": 1 }).limit(500).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn fetch_team_applications_pending_grant(&self) -> Result<Vec<TeamApplication>> {
        self.find_with_options(
            COL,
            doc! { "status": "Accepted", "grant": "PendingJoin" },
            FindOptions::builder().sort(doc! { "_id": 1 }).limit(500).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn fetch_team_applications_accepted(&self) -> Result<Vec<TeamApplication>> {
        self.find_with_options(
            COL,
            doc! { "status": "Accepted" },
            FindOptions::builder().sort(doc! { "_id": -1 }).limit(2000).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn replace_team_application_if(
        &self,
        application: &TeamApplication,
        expected: TeamApplicationStatus,
    ) -> Result<bool> {
        self.col::<TeamApplication>(COL)
            .replace_one(
                doc! { "_id": &application.id, "status": status_name(expected) },
                application,
            )
            .await
            .map(|r| r.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", COL))
    }

    async fn delete_team_application(&self, id: &str) -> Result<()> {
        self.col::<TeamApplication>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", COL))
    }

    async fn delete_team_applications_due(&self, now: i64) -> Result<u64> {
        self.col::<TeamApplication>(COL)
            .delete_many(doc! { "purge_at": { "$lt": now } })
            .await
            .map(|r| r.deleted_count)
            .map_err(|_| create_database_error!("delete_many", COL))
    }

    async fn delete_team_applications_of(&self, user_id: &str) -> Result<u64> {
        self.col::<TeamApplication>(COL)
            .delete_many(doc! { "user_id": user_id })
            .await
            .map(|r| r.deleted_count)
            .map_err(|_| create_database_error!("delete_many", COL))
    }
}
