use mongodb::options::{FindOptions, ReplaceOptions};
use revolt_result::Result;

use crate::{
    BadgeGrant, BadgeLogEntry, BetaFeature, BetaMember, BetaStatus, BugKind, BugReport, MongoDb,
};

use super::{AbstractBadges, BugReportQuery};

static GRANTS: &str = "badge_grants";
static LOG: &str = "badge_log";
static FEATURES: &str = "beta_features";
static MEMBERS: &str = "beta_members";
static BUGS: &str = "bug_reports";

fn to_bson<T: serde::Serialize>(value: &T) -> bson::Bson {
    bson::to_bson(value).expect("failed to serialize")
}

fn is_duplicate(error: &mongodb::error::Error) -> bool {
    error.to_string().contains("E11000")
}

/// Ids are `{user_id}:...`, so a user's documents are found with the `_id`
/// index alone
fn user_prefix(user_id: &str) -> bson::Document {
    doc! { "$regex": format!("^{}:", regex::escape(user_id)) }
}

#[async_trait]
impl AbstractBadges for MongoDb {
    async fn fetch_badge_grants(&self, user_id: &str) -> Result<Vec<BadgeGrant>> {
        self.find_with_options(GRANTS, doc! { "_id": user_prefix(user_id) }, None)
            .await
            .map_err(|_| create_database_error!("find", GRANTS))
    }

    async fn fetch_badge_grants_for(&self, user_ids: &[String]) -> Result<Vec<BadgeGrant>> {
        self.find_with_options(GRANTS, doc! { "user_id": { "$in": user_ids } }, None)
            .await
            .map_err(|_| create_database_error!("find", GRANTS))
    }

    async fn insert_badge_grant(&self, grant: &BadgeGrant) -> Result<bool> {
        match self.col::<BadgeGrant>(GRANTS).insert_one(grant).await {
            Ok(_) => Ok(true),
            Err(error) if is_duplicate(&error) => Ok(false),
            Err(_) => Err(create_database_error!("insert_one", GRANTS)),
        }
    }

    async fn save_badge_grant(&self, grant: &BadgeGrant) -> Result<()> {
        self.col::<BadgeGrant>(GRANTS)
            .replace_one(doc! { "_id": &grant.id }, grant)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", GRANTS))
    }

    async fn fetch_badge_holders(&self, badge: &str, limit: usize) -> Result<Vec<BadgeGrant>> {
        self.find_with_options(
            GRANTS,
            doc! { "badge": badge, "revoked_at": { "$exists": false } },
            FindOptions::builder()
                .sort(doc! { "granted_at": -1 })
                .limit(limit.clamp(1, 200) as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", GRANTS))
    }

    async fn fetch_expired_badge_grants(&self, now: i64) -> Result<Vec<BadgeGrant>> {
        self.find_with_options(
            GRANTS,
            doc! { "expires_at": { "$lte": now }, "revoked_at": { "$exists": false } },
            FindOptions::builder().limit(500).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", GRANTS))
    }

    async fn insert_badge_log(&self, entry: &BadgeLogEntry) -> Result<()> {
        query!(self, insert_one, LOG, &entry).map(|_| ())
    }

    async fn fetch_badge_logs(&self, user_id: &str) -> Result<Vec<BadgeLogEntry>> {
        self.find_with_options(
            LOG,
            doc! { "_id": user_prefix(user_id) },
            FindOptions::builder()
                .sort(doc! { "_id": -1 })
                .limit(200)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", LOG))
    }

    async fn fetch_beta_features(&self) -> Result<Vec<BetaFeature>> {
        self.find_with_options(FEATURES, doc! {}, None)
            .await
            .map_err(|_| create_database_error!("find", FEATURES))
    }

    async fn save_beta_feature(&self, feature: &BetaFeature) -> Result<()> {
        self.col::<BetaFeature>(FEATURES)
            .replace_one(doc! { "_id": &feature.id }, feature)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", FEATURES))
    }

    async fn fetch_beta_member(&self, user_id: &str) -> Result<Option<BetaMember>> {
        query!(self, find_one_by_id, MEMBERS, user_id)
    }

    async fn fetch_beta_members(&self, status: Option<BetaStatus>) -> Result<Vec<BetaMember>> {
        let filter = match status {
            Some(status) => doc! { "status": to_bson(&status) },
            None => doc! {},
        };
        self.find_with_options(
            MEMBERS,
            filter,
            FindOptions::builder()
                .sort(doc! { "created_at": -1 })
                .limit(500)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", MEMBERS))
    }

    async fn save_beta_member(&self, member: &BetaMember) -> Result<()> {
        self.col::<BetaMember>(MEMBERS)
            .replace_one(doc! { "_id": &member.id }, member)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", MEMBERS))
    }

    async fn add_beta_contribution(&self, user_id: &str) -> Result<()> {
        self.col::<BetaMember>(MEMBERS)
            .update_one(doc! { "_id": user_id }, doc! { "$inc": { "contributions": 1_i64 } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", MEMBERS))
    }

    async fn insert_bug_report(&self, report: &BugReport) -> Result<()> {
        query!(self, insert_one, BUGS, &report).map(|_| ())
    }

    async fn fetch_bug_report(&self, id: &str) -> Result<BugReport> {
        query!(self, find_one_by_id, BUGS, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn update_bug_report(&self, report: &BugReport, expected_updated_at: i64) -> Result<bool> {
        self.col::<BugReport>(BUGS)
            .replace_one(
                doc! { "_id": &report.id, "updated_at": expected_updated_at },
                report,
            )
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", BUGS))
    }

    async fn fetch_bug_reports(&self, query: BugReportQuery<'_>) -> Result<Vec<BugReport>> {
        let mut filter = doc! {};
        if let Some(author) = query.author_id {
            filter.insert("author_id", author);
        }
        if !query.statuses.is_empty() {
            filter.insert(
                "status",
                doc! { "$in": query.statuses.iter().map(to_bson).collect::<Vec<_>>() },
            );
        }
        if query.only_security {
            filter.insert("kind", to_bson(&BugKind::Security));
        } else if !query.include_security {
            filter.insert("kind", doc! { "$ne": to_bson(&BugKind::Security) });
        }
        self.find_with_options(
            BUGS,
            filter,
            FindOptions::builder()
                .sort(doc! { "created_at": -1 })
                .limit(query.limit.clamp(1, 200) as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", BUGS))
    }

    async fn count_bug_reports_since(&self, author_id: &str, since: i64) -> Result<usize> {
        self.col::<BugReport>(BUGS)
            .count_documents(doc! { "author_id": author_id, "created_at": { "$gte": since } })
            .await
            .map(|count| count as usize)
            .map_err(|_| create_database_error!("count_documents", BUGS))
    }
}
