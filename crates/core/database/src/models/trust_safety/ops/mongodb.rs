use futures::TryStreamExt;
use mongodb::options::FindOptions;
use revolt_models::v0::{
    SafetyAction, SafetyActionKind, SafetyAppeal, SafetyAppealStatus, SafetyCase, SafetyEvidence,
    SafetyLogEntry, SafetyStatus, SafetyTargetKind,
};
use revolt_result::Result;

use crate::MongoDb;

use super::{AbstractTrustSafety, SafetyCaseQuery};

static CASES: &str = "safety_cases";
static EVIDENCE: &str = "safety_evidence";
static LOG: &str = "safety_case_log";
static ACTIONS: &str = "safety_actions";
static APPEALS: &str = "safety_appeals";
static FILES: &str = "attachments";

fn to_bson<T: serde::Serialize>(value: &T) -> bson::Bson {
    bson::to_bson(value).expect("failed to serialize")
}

fn open_statuses() -> Vec<bson::Bson> {
    [
        SafetyStatus::Submitted,
        SafetyStatus::Queued,
        SafetyStatus::UnderReview,
        SafetyStatus::MoreInfoRequired,
        SafetyStatus::Escalated,
    ]
    .iter()
    .map(to_bson)
    .collect()
}

impl MongoDb {
    async fn ts_find<T>(
        &self,
        collection: &'static str,
        filter: bson::Document,
        sort: bson::Document,
        limit: Option<i64>,
    ) -> Result<Vec<T>>
    where
        T: serde::de::DeserializeOwned + Unpin + Send + Sync,
    {
        self.col::<T>(collection)
            .find(filter)
            .with_options(FindOptions::builder().sort(sort).limit(limit).build())
            .await
            .map_err(|_| create_database_error!("find", collection))?
            .try_collect()
            .await
            .map_err(|_| create_database_error!("find", collection))
    }

    async fn ts_insert<T: serde::Serialize + Send + Sync>(
        &self,
        collection: &'static str,
        value: &T,
    ) -> Result<()> {
        self.col::<T>(collection)
            .insert_one(value)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("insert_one", collection))
    }

    async fn ts_one<T>(&self, collection: &'static str, filter: bson::Document) -> Result<Option<T>>
    where
        T: serde::de::DeserializeOwned + Unpin + Send + Sync,
    {
        self.col::<T>(collection)
            .find_one(filter)
            .await
            .map_err(|_| create_database_error!("find_one", collection))
    }
}

#[async_trait]
impl AbstractTrustSafety for MongoDb {
    async fn insert_safety_case(&self, case: &SafetyCase) -> Result<()> {
        self.ts_insert(CASES, case).await
    }

    async fn fetch_safety_case(&self, id: &str) -> Result<SafetyCase> {
        self.ts_one(CASES, doc! { "_id": id })
            .await?
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn update_safety_case(&self, case: &SafetyCase, expected_version: i64) -> Result<bool> {
        self.col::<SafetyCase>(CASES)
            .replace_one(doc! { "_id": &case.id, "version": expected_version }, case)
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", CASES))
    }

    async fn find_open_safety_case(
        &self,
        reporter_id: &str,
        kind: SafetyTargetKind,
        target_id: &str,
    ) -> Result<Option<SafetyCase>> {
        self.ts_one(
            CASES,
            doc! {
                "reporter_id": reporter_id,
                "target.kind": to_bson(&kind),
                "target.id": target_id,
                "status": { "$in": open_statuses() },
            },
        )
        .await
    }

    async fn count_safety_cases_since(&self, reporter_id: &str, since: i64) -> Result<usize> {
        self.col::<SafetyCase>(CASES)
            .count_documents(doc! { "reporter_id": reporter_id, "created_at": { "$gte": since } })
            .await
            .map(|n| n as usize)
            .map_err(|_| create_database_error!("count_documents", CASES))
    }

    async fn fetch_safety_cases(&self, query: SafetyCaseQuery) -> Result<Vec<SafetyCase>> {
        let mut filter = doc! {};
        if !query.routes.is_empty() {
            filter.insert("route", doc! { "$in": query.routes.iter().map(to_bson).collect::<Vec<_>>() });
        }
        if !query.statuses.is_empty() {
            filter.insert(
                "status",
                doc! { "$in": query.statuses.iter().map(to_bson).collect::<Vec<_>>() },
            );
        }
        if !query.kinds.is_empty() {
            filter.insert(
                "target.kind",
                doc! { "$in": query.kinds.iter().map(to_bson).collect::<Vec<_>>() },
            );
        }
        if !query.categories.is_empty() {
            filter.insert(
                "category",
                doc! { "$in": query.categories.iter().map(to_bson).collect::<Vec<_>>() },
            );
        }
        if let Some(priority) = &query.priority {
            filter.insert("priority", to_bson(priority));
        }
        if let Some(assignee) = &query.assignee_id {
            filter.insert("assignee_id", assignee);
        } else if query.unassigned {
            filter.insert("assignee_id", doc! { "$exists": false });
        }
        if let Some(reporter) = &query.reporter_id {
            filter.insert("reporter_id", reporter);
        }
        if let Some(search) = query.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            // exact matches only: every one of these fields is indexed
            let reference = search.to_uppercase();
            filter.insert(
                "$or",
                vec![
                    doc! { "_id": search },
                    doc! { "reference": reference },
                    doc! { "target.id": search },
                    doc! { "target_owner_id": search },
                    doc! { "reporter_id": search },
                ],
            );
        }
        if let Some(before) = &query.before {
            let op = if query.oldest_first { "$gt" } else { "$lt" };
            filter.insert("_id", doc! { op: before });
        }

        let sort = if query.oldest_first {
            doc! { "_id": 1 }
        } else {
            doc! { "_id": -1 }
        };
        self.ts_find(CASES, filter, sort, Some(query.limit.clamp(1, 100) as i64))
            .await
    }

    async fn fetch_safety_cases_for_target(
        &self,
        kind: SafetyTargetKind,
        target_id: &str,
        limit: usize,
    ) -> Result<Vec<SafetyCase>> {
        self.ts_find(
            CASES,
            doc! { "target.kind": to_bson(&kind), "target.id": target_id },
            doc! { "_id": -1 },
            Some(limit.clamp(1, 100) as i64),
        )
        .await
    }

    async fn count_safety_reporters(
        &self,
        kind: SafetyTargetKind,
        target_id: &str,
    ) -> Result<usize> {
        let filter = doc! {
            "target.kind": to_bson(&kind),
            "target.id": target_id,
            "status": { "$in": open_statuses() },
        };
        let people = self
            .col::<SafetyCase>(CASES)
            .distinct("reporter_id", filter.clone())
            .await
            .map_err(|_| create_database_error!("distinct", CASES))?
            .into_iter()
            .filter(|id| id.as_str() != Some("public"))
            .count();
        // every report without an account is a different person
        let mut public_filter = filter;
        public_filter.insert("reporter_id", "public");
        let public = self
            .col::<SafetyCase>(CASES)
            .count_documents(public_filter)
            .await
            .map_err(|_| create_database_error!("count_documents", CASES))?;
        Ok(people + public as usize)
    }

    async fn fetch_safety_cases_by_reporter(
        &self,
        reporter_id: &str,
        limit: usize,
    ) -> Result<Vec<SafetyCase>> {
        self.ts_find(
            CASES,
            doc! { "reporter_id": reporter_id },
            doc! { "_id": -1 },
            Some(limit.clamp(1, 100) as i64),
        )
        .await
    }

    async fn insert_safety_evidence(&self, evidence: &SafetyEvidence) -> Result<()> {
        self.ts_insert(EVIDENCE, evidence).await
    }

    async fn fetch_safety_evidence(&self, case_id: &str) -> Result<Vec<SafetyEvidence>> {
        self.ts_find(EVIDENCE, doc! { "case_id": case_id }, doc! { "_id": 1 }, None)
            .await
    }

    async fn set_safety_evidence_hold(
        &self,
        case_id: &str,
        hold: bool,
        expires_at: Option<i64>,
    ) -> Result<()> {
        let update = match expires_at {
            Some(at) => doc! { "$set": { "legal_hold": hold, "expires_at": at } },
            None => doc! { "$set": { "legal_hold": hold } },
        };
        self.col::<SafetyEvidence>(EVIDENCE)
            .update_many(doc! { "case_id": case_id }, update)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_many", EVIDENCE))
    }

    async fn purge_expired_safety_evidence(&self, now: i64) -> Result<Vec<String>> {
        // reports of child sexual abuse material are never purged here
        let csam_cases: Vec<String> = self
            .col::<bson::Document>(CASES)
            .find(doc! { "$or": [
                { "category": "SexualContent", "subcategory": "minor_safety" },
                { "category": "Underage" },
                { "legal_hold": true },
            ] })
            .await
            .map_err(|_| create_database_error!("find", CASES))?
            .try_collect::<Vec<_>>()
            .await
            .map_err(|_| create_database_error!("find", CASES))?
            .into_iter()
            .filter_map(|case| case.get_str("_id").ok().map(str::to_string))
            .collect();
        let expired: Vec<SafetyEvidence> = self
            .ts_find(
                EVIDENCE,
                doc! {
                    "legal_hold": { "$ne": true },
                    "case_id": { "$nin": &csam_cases },
                    "expires_at": { "$lte": now }
                },
                doc! { "expires_at": 1 },
                Some(500),
            )
            .await?;
        if expired.is_empty() {
            return Ok(vec![]);
        }

        let ids: Vec<&str> = expired.iter().map(|e| e.id.as_str()).collect();
        self.col::<SafetyEvidence>(EVIDENCE)
            .delete_many(doc! { "_id": { "$in": &ids } })
            .await
            .map_err(|_| create_database_error!("delete_many", EVIDENCE))?;

        // release files no other evidence keeps
        let mut released = vec![];
        for file in expired.iter().flat_map(|e| e.files.iter()) {
            let still_used = self
                .col::<SafetyEvidence>(EVIDENCE)
                .count_documents(doc! { "files.id": &file.id })
                .await
                .map_err(|_| create_database_error!("count_documents", EVIDENCE))?;
            if still_used == 0 && !released.contains(&file.id) {
                self.col::<bson::Document>(FILES)
                    .update_one(doc! { "_id": &file.id }, doc! { "$unset": { "reported": 1 } })
                    .await
                    .map_err(|_| create_database_error!("update_one", FILES))?;
                released.push(file.id.clone());
            }
        }
        Ok(released)
    }

    async fn clear_public_reporters(&self, closed_before: i64) -> Result<usize> {
        self.col::<bson::Document>(CASES)
            .update_many(
                doc! {
                    // the date a case closed, or for a case an action ended
                    // the decision (a case waiting on an appeal is not decided)
                    "status": { "$in": ["ActionTaken", "NoViolation", "Duplicate", "Closed", "AppealResolved"] },
                    "$and": [
                        { "$or": [
                            { "closed_at": { "$lte": closed_before } },
                            { "closed_at": null, "resolution.decided_at": { "$lte": closed_before } },
                        ] },
                        { "$or": [
                            { "public_reporter.name": { "$exists": true } },
                            { "public_reporter.email": { "$exists": true } },
                        ] },
                    ],
                },
                doc! { "$unset": { "public_reporter.name": 1, "public_reporter.email": 1 } },
            )
            .await
            .map(|result| result.modified_count as usize)
            .map_err(|_| create_database_error!("update_many", CASES))
    }

    async fn insert_safety_log(&self, entry: &SafetyLogEntry) -> Result<()> {
        self.ts_insert(LOG, entry).await
    }

    async fn fetch_safety_log(&self, case_id: &str) -> Result<Vec<SafetyLogEntry>> {
        self.ts_find(LOG, doc! { "case_id": case_id }, doc! { "_id": 1 }, Some(500))
            .await
    }

    async fn insert_safety_action(&self, action: &SafetyAction) -> Result<()> {
        self.ts_insert(ACTIONS, action).await
    }

    async fn fetch_safety_action(&self, id: &str) -> Result<SafetyAction> {
        self.ts_one(ACTIONS, doc! { "_id": id })
            .await?
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn save_safety_action(&self, action: &SafetyAction) -> Result<()> {
        self.col::<SafetyAction>(ACTIONS)
            .replace_one(doc! { "_id": &action.id }, action)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", ACTIONS))
    }

    async fn fetch_safety_actions_for_user(&self, user_id: &str) -> Result<Vec<SafetyAction>> {
        self.ts_find(
            ACTIONS,
            doc! { "subject_user_id": user_id },
            doc! { "created_at": -1 },
            Some(200),
        )
        .await
    }

    async fn fetch_safety_actions_for_case(&self, case_id: &str) -> Result<Vec<SafetyAction>> {
        self.ts_find(ACTIONS, doc! { "case_id": case_id }, doc! { "_id": 1 }, Some(100))
            .await
    }

    async fn fetch_expired_safety_actions(
        &self,
        kind: SafetyActionKind,
        now: i64,
    ) -> Result<Vec<SafetyAction>> {
        self.ts_find(
            ACTIONS,
            doc! {
                "kind": to_bson(&kind),
                "expires_at": { "$lte": now },
                "revoked": { "$exists": false },
            },
            doc! { "expires_at": 1 },
            Some(200),
        )
        .await
    }

    async fn insert_safety_appeal(&self, appeal: &SafetyAppeal) -> Result<()> {
        // unique index on action_id: a second appeal fails here
        self.col::<SafetyAppeal>(APPEALS)
            .insert_one(appeal)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.to_string().contains("E11000") {
                    create_error!(InvalidOperation)
                } else {
                    create_database_error!("insert_one", APPEALS)
                }
            })
    }

    async fn fetch_safety_appeal(&self, id: &str) -> Result<SafetyAppeal> {
        self.ts_one(APPEALS, doc! { "_id": id })
            .await?
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_safety_appeal_for_action(
        &self,
        action_id: &str,
    ) -> Result<Option<SafetyAppeal>> {
        self.ts_one(APPEALS, doc! { "action_id": action_id }).await
    }

    async fn decide_safety_appeal(&self, appeal: &SafetyAppeal) -> Result<bool> {
        self.col::<SafetyAppeal>(APPEALS)
            .replace_one(
                doc! { "_id": &appeal.id, "status": to_bson(&SafetyAppealStatus::Pending) },
                appeal,
            )
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", APPEALS))
    }

    async fn fetch_safety_appeals(
        &self,
        status: Option<SafetyAppealStatus>,
        before: Option<String>,
        limit: usize,
    ) -> Result<Vec<SafetyAppeal>> {
        let mut filter = doc! {};
        if let Some(status) = status {
            filter.insert("status", to_bson(&status));
        }
        if let Some(before) = before {
            filter.insert("_id", doc! { "$lt": before });
        }
        self.ts_find(APPEALS, filter, doc! { "_id": -1 }, Some(limit.clamp(1, 100) as i64))
            .await
    }

    async fn fetch_safety_appeals_for_user(&self, user_id: &str) -> Result<Vec<SafetyAppeal>> {
        self.ts_find(APPEALS, doc! { "user_id": user_id }, doc! { "_id": -1 }, Some(200))
            .await
    }

    async fn fetch_safety_appeals_for_case(&self, case_id: &str) -> Result<Vec<SafetyAppeal>> {
        self.ts_find(APPEALS, doc! { "case_id": case_id }, doc! { "_id": 1 }, Some(100))
            .await
    }
}
