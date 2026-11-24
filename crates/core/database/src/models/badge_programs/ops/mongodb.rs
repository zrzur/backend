use bson::Document;
use futures::StreamExt;
use mongodb::options::{FindOptions, ReplaceOptions};
use revolt_result::Result;

use crate::{
    ApplicationStatus, BadgeApplication, BadgeEvidence, BadgeProgram, BadgeReview, MongoDb,
    ServerActivityWeek,
};

use super::AbstractBadgePrograms;

static APPLICATIONS: &str = "badge_applications";
static EVIDENCE: &str = "badge_evidence";
static REVIEWS: &str = "badge_reviews";
static WEEKS: &str = "server_activity_weeks";

fn to_bson<T: serde::Serialize>(value: &T) -> bson::Bson {
    bson::to_bson(value).expect("failed to serialize")
}

fn newest(limit: i64) -> FindOptions {
    FindOptions::builder().sort(doc! { "_id": -1 }).limit(limit).build()
}

#[async_trait]
impl AbstractBadgePrograms for MongoDb {
    async fn insert_badge_application(&self, application: &BadgeApplication) -> Result<()> {
        query!(self, insert_one, APPLICATIONS, &application).map(|_| ())
    }

    async fn fetch_badge_application(&self, id: &str) -> Result<BadgeApplication> {
        query!(self, find_one_by_id, APPLICATIONS, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn save_badge_application(&self, application: &BadgeApplication, expected_updated_at: i64) -> Result<bool> {
        self.col::<BadgeApplication>(APPLICATIONS)
            .replace_one(
                doc! { "_id": &application.id, "updated_at": expected_updated_at },
                application,
            )
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", APPLICATIONS))
    }

    async fn fetch_badge_applications_by_applicant(&self, user_id: &str) -> Result<Vec<BadgeApplication>> {
        self.find_with_options(APPLICATIONS, doc! { "applicant_id": user_id }, newest(200))
            .await
            .map_err(|_| create_database_error!("find", APPLICATIONS))
    }

    async fn fetch_badge_applications_for_target(&self, target_id: &str) -> Result<Vec<BadgeApplication>> {
        self.find_with_options(APPLICATIONS, doc! { "target_id": target_id }, newest(200))
            .await
            .map_err(|_| create_database_error!("find", APPLICATIONS))
    }

    async fn fetch_badge_applications(
        &self,
        status: Option<ApplicationStatus>,
        program: Option<BadgeProgram>,
        limit: usize,
    ) -> Result<Vec<BadgeApplication>> {
        let mut filter = Document::new();
        if let Some(status) = status {
            filter.insert("status", to_bson(&status));
        }
        if let Some(program) = program {
            filter.insert("program", to_bson(&program));
        }
        self.find_with_options(APPLICATIONS, filter, newest(limit as i64))
            .await
            .map_err(|_| create_database_error!("find", APPLICATIONS))
    }

    async fn fetch_badge_applications_due(&self, now: i64) -> Result<Vec<BadgeApplication>> {
        self.find_with_options(
            APPLICATIONS,
            doc! {
                "status": to_bson(&ApplicationStatus::Approved),
                "recheck_due": false,
                "next_review_at": { "$lte": now },
            },
            None,
        )
        .await
        .map_err(|_| create_database_error!("find", APPLICATIONS))
    }

    async fn fetch_badge_applications_expired(&self, now: i64, draft_before: i64) -> Result<Vec<BadgeApplication>> {
        self.find_with_options(
            APPLICATIONS,
            doc! { "$or": [
                { "evidence_purged": false, "evidence_purge_at": { "$lte": now } },
                { "purge_at": { "$lte": now } },
                { "status": to_bson(&ApplicationStatus::Draft), "updated_at": { "$lt": draft_before } },
            ] },
            None,
        )
        .await
        .map_err(|_| create_database_error!("find", APPLICATIONS))
    }

    async fn delete_badge_application(&self, id: &str) -> Result<()> {
        self.col::<Document>(EVIDENCE)
            .delete_many(doc! { "application_id": id })
            .await
            .map_err(|_| create_database_error!("delete_many", EVIDENCE))?;
        self.col::<Document>(REVIEWS)
            .delete_many(doc! { "application_id": id })
            .await
            .map_err(|_| create_database_error!("delete_many", REVIEWS))?;
        self.col::<Document>(APPLICATIONS)
            .delete_one(doc! { "_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", APPLICATIONS))
    }

    async fn insert_badge_evidence(&self, evidence: &BadgeEvidence) -> Result<()> {
        query!(self, insert_one, EVIDENCE, &evidence).map(|_| ())
    }

    async fn fetch_badge_evidence(&self, application_id: &str) -> Result<Vec<BadgeEvidence>> {
        self.find_with_options(
            EVIDENCE,
            doc! { "application_id": application_id },
            FindOptions::builder().sort(doc! { "_id": 1 }).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", EVIDENCE))
    }

    async fn delete_badge_evidence(&self, application_id: &str) -> Result<()> {
        self.col::<Document>(EVIDENCE)
            .delete_many(doc! { "application_id": application_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", EVIDENCE))
    }

    async fn insert_badge_review(&self, review: &BadgeReview) -> Result<()> {
        query!(self, insert_one, REVIEWS, &review).map(|_| ())
    }

    async fn fetch_badge_reviews(&self, application_id: &str) -> Result<Vec<BadgeReview>> {
        self.find_with_options(
            REVIEWS,
            doc! { "application_id": application_id },
            FindOptions::builder().sort(doc! { "_id": 1 }).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", REVIEWS))
    }

    async fn save_server_activity_week(&self, week: &ServerActivityWeek) -> Result<()> {
        self.col::<ServerActivityWeek>(WEEKS)
            .replace_one(doc! { "_id": &week.id }, week)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", WEEKS))
    }

    async fn fetch_server_activity_weeks(&self, server_id: &str, from_week: i64) -> Result<Vec<ServerActivityWeek>> {
        self.find_with_options(
            WEEKS,
            doc! { "server_id": server_id, "week": { "$gte": from_week } },
            FindOptions::builder().sort(doc! { "week": 1 }).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", WEEKS))
    }

    async fn delete_server_activity_weeks_before(&self, week: i64) -> Result<()> {
        self.col::<Document>(WEEKS)
            .delete_many(doc! { "week": { "$lt": week } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", WEEKS))
    }

    async fn fetch_message_author_ids(&self, channels: &[String], from: &str, to: &str) -> Result<Vec<String>> {
        // the (channel, _id) index; ids only, never content
        let mut cursor = self
            .col::<Document>("messages")
            .aggregate(vec![
                doc! { "$match": {
                    "channel": { "$in": channels.to_vec() },
                    "_id": { "$gte": from, "$lt": to },
                    "system": { "$exists": false },
                    "webhook": { "$exists": false },
                } },
                doc! { "$group": { "_id": "$author" } },
            ])
            .await
            .map_err(|_| create_database_error!("aggregate", "messages"))?;
        let mut ids = vec![];
        while let Some(group) = cursor.next().await {
            let group = group.map_err(|_| create_database_error!("aggregate", "messages"))?;
            if let Ok(id) = group.get_str("_id") {
                ids.push(id.to_string());
            }
        }
        Ok(ids)
    }

    async fn fetch_channel_reader_ids(&self, channels: &[String], from: &str, to: &str) -> Result<Vec<String>> {
        let mut cursor = self
            .col::<Document>("channel_unreads")
            .aggregate(vec![
                doc! { "$match": {
                    "_id.channel": { "$in": channels.to_vec() },
                    "last_id": { "$gte": from, "$lt": to },
                } },
                doc! { "$group": { "_id": "$_id.user" } },
            ])
            .await
            .map_err(|_| create_database_error!("aggregate", "channel_unreads"))?;
        let mut ids = vec![];
        while let Some(group) = cursor.next().await {
            let group = group.map_err(|_| create_database_error!("aggregate", "channel_unreads"))?;
            if let Ok(id) = group.get_str("_id") {
                ids.push(id.to_string());
            }
        }
        Ok(ids)
    }
}
