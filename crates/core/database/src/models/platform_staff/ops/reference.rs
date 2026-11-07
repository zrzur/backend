use revolt_models::v0::SafetyStatus;
use revolt_result::Result;

use crate::{
    Bot, PlatformAuditEntry, ReferenceDb, Server, StaffCollectionStat, StaffCounted, StaffDbStats,
    StaffGrant, StaffSession, User,
};

fn in_range(id: &str, from: Option<&str>, to: Option<&str>) -> bool {
    from.is_none_or(|f| id >= f) && to.is_none_or(|t| id < t)
}

use super::{AbstractPlatformStaff, PlatformAuditQuery};

#[async_trait]
impl AbstractPlatformStaff for ReferenceDb {
    async fn fetch_staff_grant(&self, user_id: &str) -> Result<Option<StaffGrant>> {
        Ok(self.platform_staff.lock().await.grants.get(user_id).cloned())
    }

    async fn fetch_staff_grants(&self) -> Result<Vec<StaffGrant>> {
        let mut grants: Vec<_> = self.platform_staff.lock().await.grants.values().cloned().collect();
        grants.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        grants.truncate(500);
        Ok(grants)
    }

    async fn save_staff_grant(
        &self,
        grant: &StaffGrant,
        expected_version: Option<i64>,
    ) -> Result<bool> {
        let mut store = self.platform_staff.lock().await;
        let current = store.grants.get(&grant.id).map(|g| g.version);
        if current != expected_version {
            return Ok(false);
        }
        store.grants.insert(grant.id.clone(), grant.clone());
        Ok(true)
    }

    async fn claim_staff_totp_step(&self, user_id: &str, step: u64) -> Result<bool> {
        let mut store = self.platform_staff.lock().await;
        match store.grants.get_mut(user_id) {
            Some(grant) if grant.last_totp_step < step => {
                grant.last_totp_step = step;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn insert_staff_session(&self, session: &StaffSession) -> Result<()> {
        self.platform_staff
            .lock()
            .await
            .sessions
            .insert(session.id.clone(), session.clone());
        Ok(())
    }

    async fn fetch_staff_session_by_hash(&self, token_hash: &str) -> Result<Option<StaffSession>> {
        Ok(self
            .platform_staff
            .lock()
            .await
            .sessions
            .values()
            .find(|s| s.token_hash == token_hash)
            .cloned())
    }

    async fn touch_staff_session(&self, id: &str, at: i64) -> Result<()> {
        if let Some(s) = self.platform_staff.lock().await.sessions.get_mut(id) {
            s.last_seen_at = at;
        }
        Ok(())
    }

    async fn set_staff_step_up(&self, id: &str, until: i64) -> Result<()> {
        if let Some(s) = self.platform_staff.lock().await.sessions.get_mut(id) {
            s.step_up_until = Some(until);
        }
        Ok(())
    }

    async fn revoke_staff_session(&self, id: &str, at: i64) -> Result<()> {
        if let Some(s) = self.platform_staff.lock().await.sessions.get_mut(id) {
            s.revoked_at.get_or_insert(at);
        }
        Ok(())
    }

    async fn revoke_staff_sessions_for_user(&self, user_id: &str, at: i64) -> Result<usize> {
        let mut store = self.platform_staff.lock().await;
        let mut count = 0;
        for s in store.sessions.values_mut() {
            if s.user_id == user_id && s.revoked_at.is_none() {
                s.revoked_at = Some(at);
                count += 1;
            }
        }
        Ok(count)
    }

    async fn fetch_staff_sessions_for_user(
        &self,
        user_id: &str,
        now: i64,
    ) -> Result<Vec<StaffSession>> {
        let mut sessions: Vec<_> = self
            .platform_staff
            .lock()
            .await
            .sessions
            .values()
            .filter(|s| s.user_id == user_id && s.revoked_at.is_none() && s.expires_at > now)
            .cloned()
            .collect();
        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(sessions)
    }

    async fn fetch_last_platform_audit(&self) -> Result<Option<PlatformAuditEntry>> {
        Ok(self.platform_staff.lock().await.audit.last().cloned())
    }

    async fn insert_platform_audit(&self, entry: &PlatformAuditEntry) -> Result<bool> {
        let mut store = self.platform_staff.lock().await;
        if store.audit.iter().any(|e| e.seq == entry.seq) {
            return Ok(false);
        }
        store.audit.push(entry.clone());
        store.audit.sort_by_key(|e| e.seq);
        Ok(true)
    }

    async fn fetch_platform_audit(
        &self,
        query: PlatformAuditQuery,
    ) -> Result<Vec<PlatformAuditEntry>> {
        Ok(self
            .platform_staff
            .lock()
            .await
            .audit
            .iter()
            .rev()
            .filter(|e| query.before_seq.is_none_or(|b| e.seq < b))
            .filter(|e| query.actor_id.as_ref().is_none_or(|a| &e.actor_id == a))
            .filter(|e| {
                query
                    .target_id
                    .as_ref()
                    .is_none_or(|t| e.target_id.as_ref() == Some(t))
            })
            .take(query.limit)
            .cloned()
            .collect())
    }

    async fn fetch_platform_audit_range(
        &self,
        from_seq: i64,
        limit: usize,
    ) -> Result<Vec<PlatformAuditEntry>> {
        Ok(self
            .platform_staff
            .lock()
            .await
            .audit
            .iter()
            .filter(|e| e.seq >= from_seq)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn staff_count(
        &self,
        what: StaffCounted,
        from_id: Option<&str>,
        to_id: Option<&str>,
    ) -> Result<u64> {
        let ids: Vec<String> = match what {
            StaffCounted::Users => self.users.lock().await.keys().cloned().collect(),
            StaffCounted::Servers => self.servers.lock().await.keys().cloned().collect(),
            StaffCounted::Channels => self.channels.lock().await.keys().cloned().collect(),
            StaffCounted::Messages => self.messages.lock().await.keys().cloned().collect(),
        };
        Ok(ids.iter().filter(|id| in_range(id, from_id, to_id)).count() as u64)
    }

    async fn staff_open_safety_cases(&self) -> Result<u64> {
        use SafetyStatus::*;
        Ok(self
            .trust_safety
            .lock()
            .await
            .cases
            .iter()
            .filter(|c| {
                matches!(
                    c.status,
                    Submitted | Queued | UnderReview | MoreInfoRequired | Escalated | Appealed
                )
            })
            .count() as u64)
    }

    async fn staff_channel_types(&self) -> Result<Vec<(String, u64)>> {
        let mut counts = std::collections::BTreeMap::<String, u64>::new();
        for channel in self.channels.lock().await.values() {
            let kind = serde_json::to_value(channel)
                .ok()
                .and_then(|v| v.get("channel_type").and_then(|k| k.as_str()).map(String::from))
                .unwrap_or_else(|| "Unknown".to_string());
            *counts.entry(kind).or_default() += 1;
        }
        let mut out: Vec<_> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(out)
    }

    async fn staff_list_users(&self, before: Option<&str>, limit: usize) -> Result<Vec<User>> {
        let mut users: Vec<User> = self
            .users
            .lock()
            .await
            .values()
            .filter(|u| before.is_none_or(|b| u.id.as_str() < b))
            .cloned()
            .collect();
        users.sort_by(|a, b| b.id.cmp(&a.id));
        users.truncate(limit);
        Ok(users)
    }

    async fn staff_list_servers(
        &self,
        query: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Server>> {
        let query = query.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty());
        let mut servers: Vec<Server> = self
            .servers
            .lock()
            .await
            .values()
            .filter(|s| before.is_none_or(|b| s.id.as_str() < b))
            .filter(|s| {
                query
                    .as_ref()
                    .is_none_or(|q| s.id.to_lowercase() == *q || s.name.to_lowercase().contains(q))
            })
            .cloned()
            .collect();
        servers.sort_by(|a, b| b.id.cmp(&a.id));
        servers.truncate(limit);
        Ok(servers)
    }

    async fn staff_list_bots(
        &self,
        query: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Bot>> {
        let query = query.map(str::trim).filter(|q| !q.is_empty());
        let mut bots: Vec<Bot> = self
            .bots
            .lock()
            .await
            .values()
            .filter(|b| before.is_none_or(|x| b.id.as_str() < x))
            .filter(|b| query.is_none_or(|q| b.id == q || b.owner == q))
            .cloned()
            .collect();
        bots.sort_by(|a, b| b.id.cmp(&a.id));
        bots.truncate(limit);
        Ok(bots)
    }

    async fn staff_db_stats(&self) -> Result<StaffDbStats> {
        let collections = vec![
            ("users", self.users.lock().await.len()),
            ("accounts", self.accounts.lock().await.len()),
            ("sessions", self.sessions.lock().await.len()),
            ("servers", self.servers.lock().await.len()),
            ("server_members", self.server_members.lock().await.len()),
            ("channels", self.channels.lock().await.len()),
            ("messages", self.messages.lock().await.len()),
            ("platform_audit", self.platform_staff.lock().await.audit.len()),
        ];
        Ok(StaffDbStats {
            engine: "reference".to_string(),
            collections: collections
                .into_iter()
                .map(|(name, n)| StaffCollectionStat {
                    name: name.to_string(),
                    documents: n as u64,
                    size_bytes: None,
                })
                .collect(),
            data_bytes: None,
            storage_bytes: None,
            index_bytes: None,
        })
    }
}
