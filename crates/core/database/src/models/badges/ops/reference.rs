use std::collections::HashMap;

use revolt_result::Result;

use crate::{
    BadgeGrant, BadgeLogEntry, BetaFeature, BetaMember, BetaStatus, BugKind, BugReport,
    ReferenceDb,
};

use super::{AbstractBadges, BugReportQuery};

/// In-memory badge, beta and bug report data of the reference database
#[derive(Default, Debug)]
pub struct BadgeStore {
    pub grants: HashMap<String, BadgeGrant>,
    pub log: Vec<BadgeLogEntry>,
    pub features: HashMap<String, BetaFeature>,
    pub members: HashMap<String, BetaMember>,
    pub bugs: HashMap<String, BugReport>,
}

#[async_trait]
impl AbstractBadges for ReferenceDb {
    async fn fetch_badge_grants(&self, user_id: &str) -> Result<Vec<BadgeGrant>> {
        Ok(self
            .badge_store
            .lock()
            .await
            .grants
            .values()
            .filter(|g| g.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn fetch_badge_grants_for(&self, user_ids: &[String]) -> Result<Vec<BadgeGrant>> {
        Ok(self
            .badge_store
            .lock()
            .await
            .grants
            .values()
            .filter(|g| user_ids.contains(&g.user_id))
            .cloned()
            .collect())
    }

    async fn insert_badge_grant(&self, grant: &BadgeGrant) -> Result<bool> {
        let mut store = self.badge_store.lock().await;
        if store.grants.contains_key(&grant.id) {
            return Ok(false);
        }
        store.grants.insert(grant.id.clone(), grant.clone());
        Ok(true)
    }

    async fn save_badge_grant(&self, grant: &BadgeGrant) -> Result<()> {
        self.badge_store
            .lock()
            .await
            .grants
            .insert(grant.id.clone(), grant.clone());
        Ok(())
    }

    async fn fetch_badge_holders(&self, badge: &str, limit: usize) -> Result<Vec<BadgeGrant>> {
        let mut holders: Vec<BadgeGrant> = self
            .badge_store
            .lock()
            .await
            .grants
            .values()
            .filter(|g| g.badge == badge && g.revoked_at.is_none())
            .cloned()
            .collect();
        holders.sort_by(|a, b| b.granted_at.cmp(&a.granted_at));
        holders.truncate(limit.clamp(1, 200));
        Ok(holders)
    }

    async fn fetch_expired_badge_grants(&self, now: i64) -> Result<Vec<BadgeGrant>> {
        Ok(self
            .badge_store
            .lock()
            .await
            .grants
            .values()
            .filter(|g| g.revoked_at.is_none() && g.expires_at.is_some_and(|at| at <= now))
            .cloned()
            .collect())
    }

    async fn insert_badge_log(&self, entry: &BadgeLogEntry) -> Result<()> {
        self.badge_store.lock().await.log.push(entry.clone());
        Ok(())
    }

    async fn fetch_badge_logs(&self, user_id: &str) -> Result<Vec<BadgeLogEntry>> {
        let mut log: Vec<BadgeLogEntry> = self
            .badge_store
            .lock()
            .await
            .log
            .iter()
            .filter(|e| e.user_id == user_id)
            .cloned()
            .collect();
        log.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(log)
    }

    async fn fetch_beta_features(&self) -> Result<Vec<BetaFeature>> {
        Ok(self
            .badge_store
            .lock()
            .await
            .features
            .values()
            .cloned()
            .collect())
    }

    async fn save_beta_feature(&self, feature: &BetaFeature) -> Result<()> {
        self.badge_store
            .lock()
            .await
            .features
            .insert(feature.id.clone(), feature.clone());
        Ok(())
    }

    async fn fetch_beta_member(&self, user_id: &str) -> Result<Option<BetaMember>> {
        Ok(self.badge_store.lock().await.members.get(user_id).cloned())
    }

    async fn fetch_beta_members(&self, status: Option<BetaStatus>) -> Result<Vec<BetaMember>> {
        let mut members: Vec<BetaMember> = self
            .badge_store
            .lock()
            .await
            .members
            .values()
            .filter(|m| status.as_ref().is_none_or(|s| &m.status == s))
            .cloned()
            .collect();
        members.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(members)
    }

    async fn save_beta_member(&self, member: &BetaMember) -> Result<()> {
        self.badge_store
            .lock()
            .await
            .members
            .insert(member.id.clone(), member.clone());
        Ok(())
    }

    async fn add_beta_contribution(&self, user_id: &str) -> Result<()> {
        if let Some(member) = self.badge_store.lock().await.members.get_mut(user_id) {
            member.contributions += 1;
        }
        Ok(())
    }

    async fn insert_bug_report(&self, report: &BugReport) -> Result<()> {
        self.badge_store
            .lock()
            .await
            .bugs
            .insert(report.id.clone(), report.clone());
        Ok(())
    }

    async fn fetch_bug_report(&self, id: &str) -> Result<BugReport> {
        self.badge_store
            .lock()
            .await
            .bugs
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn update_bug_report(&self, report: &BugReport, expected_updated_at: i64) -> Result<bool> {
        let mut store = self.badge_store.lock().await;
        match store.bugs.get(&report.id) {
            Some(current) if current.updated_at == expected_updated_at => {
                store.bugs.insert(report.id.clone(), report.clone());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn fetch_bug_reports(&self, query: BugReportQuery<'_>) -> Result<Vec<BugReport>> {
        let mut reports: Vec<BugReport> = self
            .badge_store
            .lock()
            .await
            .bugs
            .values()
            .filter(|r| query.author_id.is_none_or(|a| r.author_id == a))
            .filter(|r| query.statuses.is_empty() || query.statuses.contains(&r.status))
            .filter(|r| {
                let security = r.kind == BugKind::Security;
                if query.only_security {
                    security
                } else {
                    query.include_security || !security
                }
            })
            .cloned()
            .collect();
        reports.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        reports.truncate(query.limit.clamp(1, 200));
        Ok(reports)
    }

    async fn count_bug_reports_since(&self, author_id: &str, since: i64) -> Result<usize> {
        Ok(self
            .badge_store
            .lock()
            .await
            .bugs
            .values()
            .filter(|r| r.author_id == author_id && r.created_at >= since)
            .count())
    }
}
