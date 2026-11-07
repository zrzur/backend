use revolt_models::v0::{
    SafetyAction, SafetyActionKind, SafetyAppeal, SafetyAppealStatus, SafetyCase, SafetyEvidence,
    SafetyLogEntry, SafetyStatus, SafetyTargetKind,
};
use revolt_result::Result;

use crate::ReferenceDb;

use super::{AbstractTrustSafety, SafetyCaseQuery};

#[async_trait]
impl AbstractTrustSafety for ReferenceDb {
    async fn insert_safety_case(&self, case: &SafetyCase) -> Result<()> {
        self.trust_safety.lock().await.cases.push(case.clone());
        Ok(())
    }

    async fn fetch_safety_case(&self, id: &str) -> Result<SafetyCase> {
        self.trust_safety
            .lock()
            .await
            .cases
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn update_safety_case(&self, case: &SafetyCase, expected_version: i64) -> Result<bool> {
        let mut store = self.trust_safety.lock().await;
        match store
            .cases
            .iter_mut()
            .find(|c| c.id == case.id && c.version == expected_version)
        {
            Some(existing) => {
                *existing = case.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn find_open_safety_case(
        &self,
        reporter_id: &str,
        kind: SafetyTargetKind,
        target_id: &str,
    ) -> Result<Option<SafetyCase>> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .cases
            .iter()
            .find(|c| {
                c.reporter_id == reporter_id
                    && c.target.kind == kind
                    && c.target.id == target_id
                    && c.status.is_open()
            })
            .cloned())
    }

    async fn count_safety_cases_since(&self, reporter_id: &str, since: i64) -> Result<usize> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .cases
            .iter()
            .filter(|c| c.reporter_id == reporter_id && c.created_at >= since)
            .count())
    }

    async fn fetch_safety_cases(&self, query: SafetyCaseQuery) -> Result<Vec<SafetyCase>> {
        let store = self.trust_safety.lock().await;
        let search = query.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let mut cases: Vec<SafetyCase> = store
            .cases
            .iter()
            .filter(|c| query.routes.is_empty() || query.routes.contains(&c.route))
            .filter(|c| query.statuses.is_empty() || query.statuses.contains(&c.status))
            .filter(|c| query.kinds.is_empty() || query.kinds.contains(&c.target.kind))
            .filter(|c| query.categories.is_empty() || query.categories.contains(&c.category))
            .filter(|c| query.priority.is_none_or(|p| p == c.priority))
            .filter(|c| match &query.assignee_id {
                Some(id) => c.assignee_id.as_ref() == Some(id),
                None => !query.unassigned || c.assignee_id.is_none(),
            })
            .filter(|c| query.reporter_id.as_ref().is_none_or(|r| &c.reporter_id == r))
            .filter(|c| {
                search.is_none_or(|s| {
                    c.id == s
                        || c.reference == s.to_uppercase()
                        || c.target.id == s
                        || c.target_owner_id.as_deref() == Some(s)
                        || c.reporter_id == s
                })
            })
            .filter(|c| match &query.before {
                Some(before) if query.oldest_first => &c.id > before,
                Some(before) => &c.id < before,
                None => true,
            })
            .cloned()
            .collect();
        cases.sort_by(|a, b| a.id.cmp(&b.id));
        if !query.oldest_first {
            cases.reverse();
        }
        cases.truncate(query.limit.clamp(1, 100));
        Ok(cases)
    }

    async fn fetch_safety_cases_for_target(
        &self,
        kind: SafetyTargetKind,
        target_id: &str,
        limit: usize,
    ) -> Result<Vec<SafetyCase>> {
        let mut cases: Vec<SafetyCase> = self
            .trust_safety
            .lock()
            .await
            .cases
            .iter()
            .filter(|c| c.target.kind == kind && c.target.id == target_id)
            .cloned()
            .collect();
        cases.sort_by(|a, b| b.id.cmp(&a.id));
        cases.truncate(limit);
        Ok(cases)
    }

    async fn count_safety_reporters(
        &self,
        kind: SafetyTargetKind,
        target_id: &str,
    ) -> Result<usize> {
        let store = self.trust_safety.lock().await;
        let open = || {
            store
                .cases
                .iter()
                .filter(|c| c.target.kind == kind && c.target.id == target_id && c.status.is_open())
        };
        // every report without an account is a different person
        let public = open().filter(|c| c.public_reporter.is_some()).count();
        let reporters: std::collections::HashSet<&str> = open()
            .filter(|c| c.public_reporter.is_none())
            .map(|c| c.reporter_id.as_str())
            .collect();
        Ok(reporters.len() + public)
    }

    async fn fetch_safety_cases_by_reporter(
        &self,
        reporter_id: &str,
        limit: usize,
    ) -> Result<Vec<SafetyCase>> {
        let mut cases: Vec<SafetyCase> = self
            .trust_safety
            .lock()
            .await
            .cases
            .iter()
            .filter(|c| c.reporter_id == reporter_id)
            .cloned()
            .collect();
        cases.sort_by(|a, b| b.id.cmp(&a.id));
        cases.truncate(limit);
        Ok(cases)
    }

    async fn insert_safety_evidence(&self, evidence: &SafetyEvidence) -> Result<()> {
        self.trust_safety.lock().await.evidence.push(evidence.clone());
        Ok(())
    }

    async fn fetch_safety_evidence(&self, case_id: &str) -> Result<Vec<SafetyEvidence>> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .evidence
            .iter()
            .filter(|e| e.case_id == case_id)
            .cloned()
            .collect())
    }

    async fn set_safety_evidence_hold(
        &self,
        case_id: &str,
        hold: bool,
        expires_at: Option<i64>,
    ) -> Result<()> {
        for evidence in self
            .trust_safety
            .lock()
            .await
            .evidence
            .iter_mut()
            .filter(|e| e.case_id == case_id)
        {
            evidence.legal_hold = hold;
            if expires_at.is_some() {
                evidence.expires_at = expires_at;
            }
        }
        Ok(())
    }

    async fn purge_expired_safety_evidence(&self, now: i64) -> Result<Vec<String>> {
        let mut store = self.trust_safety.lock().await;
        // reports of child sexual abuse material are never purged here
        let csam_cases: std::collections::HashSet<String> = store
            .cases
            .iter()
            .filter(|c| c.legal_hold || revolt_models::v0::safety_keeps_evidence(c.category, c.subcategory.as_deref()))
            .map(|c| c.id.clone())
            .collect();
        let (expired, kept): (Vec<_>, Vec<_>) = store.evidence.drain(..).partition(|e| {
            !e.legal_hold
                && !csam_cases.contains(&e.case_id)
                && e.expires_at.is_some_and(|at| at <= now)
        });
        store.evidence = kept;
        let still_used: std::collections::HashSet<String> = store
            .evidence
            .iter()
            .flat_map(|e| e.files.iter().map(|f| f.id.clone()))
            .collect();
        let mut released: Vec<String> = expired
            .iter()
            .flat_map(|e| e.files.iter().map(|f| f.id.clone()))
            .filter(|id| !still_used.contains(id))
            .collect();
        released.dedup();
        Ok(released)
    }

    async fn clear_public_reporters(&self, closed_before: i64) -> Result<usize> {
        let mut store = self.trust_safety.lock().await;
        let mut changed = 0;
        for case in store.cases.iter_mut() {
            let decided = matches!(
                case.status,
                SafetyStatus::ActionTaken
                    | SafetyStatus::NoViolation
                    | SafetyStatus::Duplicate
                    | SafetyStatus::Closed
                    | SafetyStatus::AppealResolved
            );
            let ended = case.closed_at.or(case.resolution.as_ref().map(|r| r.decided_at));
            if decided
                && ended.is_some_and(|at| at <= closed_before)
                && let Some(reporter) = case.public_reporter.as_mut()
                && (reporter.name.is_some() || reporter.email.is_some())
            {
                reporter.name = None;
                reporter.email = None;
                changed += 1;
            }
        }
        Ok(changed)
    }

    async fn insert_safety_log(&self, entry: &SafetyLogEntry) -> Result<()> {
        self.trust_safety.lock().await.log.push(entry.clone());
        Ok(())
    }

    async fn fetch_safety_log(&self, case_id: &str) -> Result<Vec<SafetyLogEntry>> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .log
            .iter()
            .filter(|e| e.case_id == case_id)
            .cloned()
            .collect())
    }

    async fn insert_safety_action(&self, action: &SafetyAction) -> Result<()> {
        self.trust_safety.lock().await.actions.push(action.clone());
        Ok(())
    }

    async fn fetch_safety_action(&self, id: &str) -> Result<SafetyAction> {
        self.trust_safety
            .lock()
            .await
            .actions
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn save_safety_action(&self, action: &SafetyAction) -> Result<()> {
        let mut store = self.trust_safety.lock().await;
        if let Some(existing) = store.actions.iter_mut().find(|a| a.id == action.id) {
            *existing = action.clone();
        }
        Ok(())
    }

    async fn fetch_safety_actions_for_user(&self, user_id: &str) -> Result<Vec<SafetyAction>> {
        let mut actions: Vec<SafetyAction> = self
            .trust_safety
            .lock()
            .await
            .actions
            .iter()
            .filter(|a| a.subject_user_id.as_deref() == Some(user_id))
            .cloned()
            .collect();
        actions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(actions)
    }

    async fn fetch_safety_actions_for_case(&self, case_id: &str) -> Result<Vec<SafetyAction>> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .actions
            .iter()
            .filter(|a| a.case_id.as_deref() == Some(case_id))
            .cloned()
            .collect())
    }

    async fn fetch_expired_safety_actions(
        &self,
        kind: SafetyActionKind,
        now: i64,
    ) -> Result<Vec<SafetyAction>> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .actions
            .iter()
            .filter(|a| {
                a.kind == kind && a.revoked.is_none() && a.expires_at.is_some_and(|at| at <= now)
            })
            .cloned()
            .collect())
    }

    async fn insert_safety_appeal(&self, appeal: &SafetyAppeal) -> Result<()> {
        let mut store = self.trust_safety.lock().await;
        if store.appeals.iter().any(|a| a.action_id == appeal.action_id) {
            return Err(create_error!(InvalidOperation));
        }
        store.appeals.push(appeal.clone());
        Ok(())
    }

    async fn fetch_safety_appeal(&self, id: &str) -> Result<SafetyAppeal> {
        self.trust_safety
            .lock()
            .await
            .appeals
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_safety_appeal_for_action(
        &self,
        action_id: &str,
    ) -> Result<Option<SafetyAppeal>> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .appeals
            .iter()
            .find(|a| a.action_id == action_id)
            .cloned())
    }

    async fn decide_safety_appeal(&self, appeal: &SafetyAppeal) -> Result<bool> {
        let mut store = self.trust_safety.lock().await;
        match store
            .appeals
            .iter_mut()
            .find(|a| a.id == appeal.id && a.status == SafetyAppealStatus::Pending)
        {
            Some(existing) => {
                *existing = appeal.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn fetch_safety_appeals(
        &self,
        status: Option<SafetyAppealStatus>,
        before: Option<String>,
        limit: usize,
    ) -> Result<Vec<SafetyAppeal>> {
        let mut appeals: Vec<SafetyAppeal> = self
            .trust_safety
            .lock()
            .await
            .appeals
            .iter()
            .filter(|a| status.is_none_or(|s| a.status == s))
            .filter(|a| before.as_ref().is_none_or(|b| &a.id < b))
            .cloned()
            .collect();
        appeals.sort_by(|a, b| b.id.cmp(&a.id));
        appeals.truncate(limit.clamp(1, 100));
        Ok(appeals)
    }

    async fn fetch_safety_appeals_for_user(&self, user_id: &str) -> Result<Vec<SafetyAppeal>> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .appeals
            .iter()
            .filter(|a| a.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn fetch_safety_appeals_for_case(&self, case_id: &str) -> Result<Vec<SafetyAppeal>> {
        Ok(self
            .trust_safety
            .lock()
            .await
            .appeals
            .iter()
            .filter(|a| a.case_id.as_deref() == Some(case_id))
            .cloned()
            .collect())
    }
}
