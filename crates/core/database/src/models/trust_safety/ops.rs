use revolt_models::v0::{
    SafetyAction, SafetyActionKind, SafetyAppeal, SafetyAppealStatus, SafetyCase, SafetyCategory,
    SafetyEvidence, SafetyLogEntry, SafetyPriority, SafetyRoute, SafetyStatus, SafetyTargetKind,
};
use revolt_result::Result;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

/// Filters for the reviewer queue. Every filter maps to an index.
#[derive(Default, Clone, Debug)]
pub struct SafetyCaseQuery {
    pub routes: Vec<SafetyRoute>,
    pub statuses: Vec<SafetyStatus>,
    pub kinds: Vec<SafetyTargetKind>,
    pub categories: Vec<SafetyCategory>,
    pub priority: Option<SafetyPriority>,
    pub assignee_id: Option<String>,
    pub unassigned: bool,
    pub reporter_id: Option<String>,
    /// Matches the target id, the target's owner, the reporter or the reference
    pub search: Option<String>,
    /// Cursor: case id to continue after
    pub before: Option<String>,
    pub oldest_first: bool,
    pub limit: usize,
}

#[async_trait]
pub trait AbstractTrustSafety: Sync + Send {
    async fn insert_safety_case(&self, case: &SafetyCase) -> Result<()>;
    async fn fetch_safety_case(&self, id: &str) -> Result<SafetyCase>;
    /// Replace a case if nobody changed it since `expected_version`
    async fn update_safety_case(&self, case: &SafetyCase, expected_version: i64) -> Result<bool>;
    /// The reporter's open case about this target, if any
    async fn find_open_safety_case(
        &self,
        reporter_id: &str,
        kind: SafetyTargetKind,
        target_id: &str,
    ) -> Result<Option<SafetyCase>>;
    async fn count_safety_cases_since(&self, reporter_id: &str, since: i64) -> Result<usize>;
    async fn fetch_safety_cases(&self, query: SafetyCaseQuery) -> Result<Vec<SafetyCase>>;
    async fn fetch_safety_cases_for_target(
        &self,
        kind: SafetyTargetKind,
        target_id: &str,
        limit: usize,
    ) -> Result<Vec<SafetyCase>>;
    /// Distinct reporters with an open case about this target
    async fn count_safety_reporters(&self, kind: SafetyTargetKind, target_id: &str)
        -> Result<usize>;
    /// The reporter's own cases, newest first
    async fn fetch_safety_cases_by_reporter(
        &self,
        reporter_id: &str,
        limit: usize,
    ) -> Result<Vec<SafetyCase>>;

    async fn insert_safety_evidence(&self, evidence: &SafetyEvidence) -> Result<()>;
    async fn fetch_safety_evidence(&self, case_id: &str) -> Result<Vec<SafetyEvidence>>;
    /// Place or lift a legal hold on a case's evidence
    async fn set_safety_evidence_hold(
        &self,
        case_id: &str,
        hold: bool,
        expires_at: Option<i64>,
    ) -> Result<()>;
    /// Delete evidence past its retention time (never held evidence).
    /// Returns the ids of files no remaining evidence refers to.
    async fn purge_expired_safety_evidence(&self, now: i64) -> Result<Vec<String>>;
    /// Remove the name and e-mail of people who reported without an account
    /// from cases closed before `closed_before`. Returns how many changed.
    async fn clear_public_reporters(&self, closed_before: i64) -> Result<usize>;

    async fn insert_safety_log(&self, entry: &SafetyLogEntry) -> Result<()>;
    async fn fetch_safety_log(&self, case_id: &str) -> Result<Vec<SafetyLogEntry>>;

    async fn insert_safety_action(&self, action: &SafetyAction) -> Result<()>;
    async fn fetch_safety_action(&self, id: &str) -> Result<SafetyAction>;
    async fn save_safety_action(&self, action: &SafetyAction) -> Result<()>;
    async fn fetch_safety_actions_for_user(&self, user_id: &str) -> Result<Vec<SafetyAction>>;
    async fn fetch_safety_actions_for_case(&self, case_id: &str) -> Result<Vec<SafetyAction>>;
    /// Active, not revoked actions of a kind whose time ran out
    async fn fetch_expired_safety_actions(
        &self,
        kind: SafetyActionKind,
        now: i64,
    ) -> Result<Vec<SafetyAction>>;

    /// Fails with `AlreadyExists`-style error if the action already has an appeal
    async fn insert_safety_appeal(&self, appeal: &SafetyAppeal) -> Result<()>;
    async fn fetch_safety_appeal(&self, id: &str) -> Result<SafetyAppeal>;
    async fn fetch_safety_appeal_for_action(&self, action_id: &str)
        -> Result<Option<SafetyAppeal>>;
    /// Save a decision if the appeal is still pending
    async fn decide_safety_appeal(&self, appeal: &SafetyAppeal) -> Result<bool>;
    async fn fetch_safety_appeals(
        &self,
        status: Option<SafetyAppealStatus>,
        before: Option<String>,
        limit: usize,
    ) -> Result<Vec<SafetyAppeal>>;
    async fn fetch_safety_appeals_for_user(&self, user_id: &str) -> Result<Vec<SafetyAppeal>>;
    async fn fetch_safety_appeals_for_case(&self, case_id: &str) -> Result<Vec<SafetyAppeal>>;
}
