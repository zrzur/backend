use bson::{Bson, Document};
use futures::StreamExt;
use mongodb::options::FindOptions;
use revolt_models::v0::SafetyStatus;
use revolt_result::Result;

use crate::{
    Bot, MongoDb, PlatformAuditEntry, Server, StaffCollectionStat, StaffCounted, StaffDbStats,
    StaffGrant, StaffSession, User, STAFF_DB_COLLECTIONS,
};

/// Statuses of a case someone still has to work on
const OPEN_SAFETY: [SafetyStatus; 6] = [
    SafetyStatus::Submitted,
    SafetyStatus::Queued,
    SafetyStatus::UnderReview,
    SafetyStatus::MoreInfoRequired,
    SafetyStatus::Escalated,
    SafetyStatus::Appealed,
];

fn as_u64(value: Option<&Bson>) -> Option<u64> {
    match value? {
        Bson::Int32(n) => Some(*n as u64),
        Bson::Int64(n) => Some(*n as u64),
        Bson::Double(n) => Some(*n as u64),
        _ => None,
    }
}

use super::{AbstractPlatformStaff, PlatformAuditQuery};

static GRANTS: &str = "platform_staff";
static SESSIONS: &str = "staff_sessions";
static AUDIT: &str = "platform_audit";

fn is_duplicate(error: &mongodb::error::Error) -> bool {
    error.to_string().contains("E11000")
}

#[async_trait]
impl AbstractPlatformStaff for MongoDb {
    async fn fetch_staff_grant(&self, user_id: &str) -> Result<Option<StaffGrant>> {
        query!(self, find_one_by_id, GRANTS, user_id)
    }

    async fn fetch_staff_grants(&self) -> Result<Vec<StaffGrant>> {
        self.find_with_options(
            GRANTS,
            doc! {},
            FindOptions::builder()
                .sort(doc! { "updated_at": -1 })
                .limit(500)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", GRANTS))
    }

    async fn save_staff_grant(
        &self,
        grant: &StaffGrant,
        expected_version: Option<i64>,
    ) -> Result<bool> {
        let col = self.col::<StaffGrant>(GRANTS);
        match expected_version {
            None => match col.insert_one(grant).await {
                Ok(_) => Ok(true),
                Err(error) if is_duplicate(&error) => Ok(false),
                Err(_) => Err(create_database_error!("insert_one", GRANTS)),
            },
            Some(version) => col
                .replace_one(doc! { "_id": &grant.id, "version": version }, grant)
                .await
                .map(|result| result.matched_count == 1)
                .map_err(|_| create_database_error!("replace_one", GRANTS)),
        }
    }

    async fn claim_staff_totp_step(&self, user_id: &str, step: u64) -> Result<bool> {
        self.col::<StaffGrant>(GRANTS)
            .update_one(
                doc! { "_id": user_id, "last_totp_step": { "$lt": step as i64 } },
                doc! { "$set": { "last_totp_step": step as i64 } },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", GRANTS))
    }

    async fn insert_staff_session(&self, session: &StaffSession) -> Result<()> {
        query!(self, insert_one, SESSIONS, &session).map(|_| ())
    }

    async fn fetch_staff_session_by_hash(&self, token_hash: &str) -> Result<Option<StaffSession>> {
        query!(self, find_one, SESSIONS, doc! { "token_hash": token_hash })
    }

    async fn touch_staff_session(&self, id: &str, at: i64) -> Result<()> {
        self.col::<StaffSession>(SESSIONS)
            .update_one(doc! { "_id": id }, doc! { "$set": { "last_seen_at": at } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", SESSIONS))
    }

    async fn set_staff_step_up(&self, id: &str, until: i64) -> Result<()> {
        self.col::<StaffSession>(SESSIONS)
            .update_one(doc! { "_id": id }, doc! { "$set": { "step_up_until": until } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", SESSIONS))
    }

    async fn revoke_staff_session(&self, id: &str, at: i64) -> Result<()> {
        self.col::<StaffSession>(SESSIONS)
            .update_one(
                doc! { "_id": id, "revoked_at": { "$exists": false } },
                doc! { "$set": { "revoked_at": at } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", SESSIONS))
    }

    async fn revoke_staff_sessions_for_user(&self, user_id: &str, at: i64) -> Result<usize> {
        self.col::<StaffSession>(SESSIONS)
            .update_many(
                doc! { "user_id": user_id, "revoked_at": { "$exists": false } },
                doc! { "$set": { "revoked_at": at } },
            )
            .await
            .map(|result| result.modified_count as usize)
            .map_err(|_| create_database_error!("update_many", SESSIONS))
    }

    async fn fetch_staff_sessions_for_user(
        &self,
        user_id: &str,
        now: i64,
    ) -> Result<Vec<StaffSession>> {
        self.find_with_options(
            SESSIONS,
            doc! {
                "user_id": user_id,
                "revoked_at": { "$exists": false },
                "expires_at": { "$gt": now },
            },
            FindOptions::builder()
                .sort(doc! { "created_at": -1 })
                .limit(50)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", SESSIONS))
    }

    async fn fetch_last_platform_audit(&self) -> Result<Option<PlatformAuditEntry>> {
        let mut last: Vec<PlatformAuditEntry> = self
            .find_with_options(
                AUDIT,
                doc! {},
                FindOptions::builder().sort(doc! { "seq": -1 }).limit(1).build(),
            )
            .await
            .map_err(|_| create_database_error!("find", AUDIT))?;
        Ok(last.pop())
    }

    async fn insert_platform_audit(&self, entry: &PlatformAuditEntry) -> Result<bool> {
        match self.col::<PlatformAuditEntry>(AUDIT).insert_one(entry).await {
            Ok(_) => Ok(true),
            Err(error) if is_duplicate(&error) => Ok(false),
            Err(_) => Err(create_database_error!("insert_one", AUDIT)),
        }
    }

    async fn fetch_platform_audit(
        &self,
        query: PlatformAuditQuery,
    ) -> Result<Vec<PlatformAuditEntry>> {
        let mut filter = doc! {};
        if let Some(before) = query.before_seq {
            filter.insert("seq", doc! { "$lt": before });
        }
        if let Some(actor) = query.actor_id {
            filter.insert("actor_id", actor);
        }
        if let Some(target) = query.target_id {
            filter.insert("target_id", target);
        }
        self.find_with_options(
            AUDIT,
            filter,
            FindOptions::builder()
                .sort(doc! { "seq": -1 })
                .limit(query.limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", AUDIT))
    }

    async fn fetch_platform_audit_range(
        &self,
        from_seq: i64,
        limit: usize,
    ) -> Result<Vec<PlatformAuditEntry>> {
        self.find_with_options(
            AUDIT,
            doc! { "seq": { "$gte": from_seq } },
            FindOptions::builder()
                .sort(doc! { "seq": 1 })
                .limit(limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", AUDIT))
    }

    async fn staff_count(
        &self,
        what: StaffCounted,
        from_id: Option<&str>,
        to_id: Option<&str>,
    ) -> Result<u64> {
        let col = self.col::<Document>(what.collection());
        if from_id.is_none() && to_id.is_none() {
            return col
                .estimated_document_count()
                .await
                .map_err(|_| create_database_error!("estimated_document_count", what.collection()));
        }
        let mut range = doc! {};
        if let Some(from) = from_id {
            range.insert("$gte", from);
        }
        if let Some(to) = to_id {
            range.insert("$lt", to);
        }
        col.count_documents(doc! { "_id": range })
            .await
            .map_err(|_| create_database_error!("count_documents", what.collection()))
    }

    async fn staff_open_safety_cases(&self) -> Result<u64> {
        let statuses: Vec<Bson> = OPEN_SAFETY
            .iter()
            .filter_map(|s| bson::to_bson(s).ok())
            .collect();
        self.col::<Document>("safety_cases")
            .count_documents(doc! { "status": { "$in": statuses } })
            .await
            .map_err(|_| create_database_error!("count_documents", "safety_cases"))
    }

    async fn staff_channel_types(&self) -> Result<Vec<(String, u64)>> {
        let mut cursor = self
            .col::<Document>("channels")
            .aggregate(vec![
                doc! { "$group": { "_id": "$channel_type", "n": { "$sum": 1 } } },
                doc! { "$sort": { "n": -1 } },
            ])
            .await
            .map_err(|_| create_database_error!("aggregate", "channels"))?;
        let mut out = vec![];
        while let Some(Ok(doc)) = cursor.next().await {
            let kind = doc.get_str("_id").unwrap_or("Unknown").to_string();
            out.push((kind, as_u64(doc.get("n")).unwrap_or(0)));
        }
        Ok(out)
    }

    async fn staff_list_users(&self, before: Option<&str>, limit: usize) -> Result<Vec<User>> {
        let filter = match before {
            Some(id) => doc! { "_id": { "$lt": id } },
            None => doc! {},
        };
        self.find_with_options(
            "users",
            filter,
            FindOptions::builder()
                .sort(doc! { "_id": -1 })
                .limit(limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", "users"))
    }

    async fn staff_list_servers(
        &self,
        query: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Server>> {
        let mut filter = doc! {};
        if let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) {
            let name = doc! { "name": { "$regex": regex::escape(query), "$options": "i" } };
            filter = if query.len() == 26 {
                doc! { "$or": [ { "_id": query }, name ] }
            } else {
                name
            };
        }
        if let Some(id) = before {
            filter.insert("_id", doc! { "$lt": id });
        }
        self.find_with_options(
            "servers",
            filter,
            FindOptions::builder()
                .sort(doc! { "_id": -1 })
                .limit(limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", "servers"))
    }

    async fn staff_list_bots(
        &self,
        query: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Bot>> {
        let mut filter = doc! {};
        if let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) {
            filter = doc! { "$or": [ { "_id": query }, { "owner": query } ] };
        }
        if let Some(id) = before {
            filter.insert("_id", doc! { "$lt": id });
        }
        self.find_with_options(
            "bots",
            filter,
            FindOptions::builder()
                .sort(doc! { "_id": -1 })
                .limit(limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", "bots"))
    }

    async fn staff_db_stats(&self) -> Result<StaffDbStats> {
        let stats = self
            .db()
            .run_command(doc! { "dbStats": 1 })
            .await
            .map_err(|_| create_database_error!("dbStats", "database"))?;
        let mut collections = Vec::with_capacity(STAFF_DB_COLLECTIONS.len());
        for name in STAFF_DB_COLLECTIONS {
            let documents = self
                .col::<Document>(name)
                .estimated_document_count()
                .await
                .unwrap_or(0);
            collections.push(StaffCollectionStat {
                name: name.to_string(),
                documents,
                size_bytes: None,
            });
        }
        Ok(StaffDbStats {
            engine: "mongodb".to_string(),
            collections,
            data_bytes: as_u64(stats.get("dataSize")),
            storage_bytes: as_u64(stats.get("storageSize")),
            index_bytes: as_u64(stats.get("indexSize")),
        })
    }
}
