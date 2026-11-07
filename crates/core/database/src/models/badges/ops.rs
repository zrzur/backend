use revolt_result::Result;

use crate::{BadgeGrant, BadgeLogEntry, BetaFeature, BetaMember, BetaStatus, BugReport, BugStatus};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

pub use reference::BadgeStore;

/// Filters for listing bug reports
#[derive(Default, Clone)]
pub struct BugReportQuery<'a> {
    pub author_id: Option<&'a str>,
    pub statuses: Vec<BugStatus>,
    /// Include security reports (security reviewers only)
    pub include_security: bool,
    /// Only security reports
    pub only_security: bool,
    pub limit: usize,
}

#[async_trait]
pub trait AbstractBadges: Sync + Send {
    /// Every grant of one user, held or not
    async fn fetch_badge_grants(&self, user_id: &str) -> Result<Vec<BadgeGrant>>;

    /// Grants of several users (one query for a page of profiles)
    async fn fetch_badge_grants_for(&self, user_ids: &[String]) -> Result<Vec<BadgeGrant>>;

    /// Save a new grant; false if the user already has a record of it
    async fn insert_badge_grant(&self, grant: &BadgeGrant) -> Result<bool>;

    /// Replace a grant record
    async fn save_badge_grant(&self, grant: &BadgeGrant) -> Result<()>;

    /// Held grants of one badge, newest first
    async fn fetch_badge_holders(&self, badge: &str, limit: usize) -> Result<Vec<BadgeGrant>>;

    /// Held grants whose expiry has passed
    async fn fetch_expired_badge_grants(&self, now: i64) -> Result<Vec<BadgeGrant>>;

    /// Append to the badge audit log
    async fn insert_badge_log(&self, entry: &BadgeLogEntry) -> Result<()>;

    /// Badge audit log of one user, newest first
    async fn fetch_badge_logs(&self, user_id: &str) -> Result<Vec<BadgeLogEntry>>;

    /// All beta features
    async fn fetch_beta_features(&self) -> Result<Vec<BetaFeature>>;

    /// Create or replace a beta feature
    async fn save_beta_feature(&self, feature: &BetaFeature) -> Result<()>;

    /// One user's beta membership
    async fn fetch_beta_member(&self, user_id: &str) -> Result<Option<BetaMember>>;

    /// Beta members, optionally in one status
    async fn fetch_beta_members(&self, status: Option<BetaStatus>) -> Result<Vec<BetaMember>>;

    /// Create or replace a beta membership
    async fn save_beta_member(&self, member: &BetaMember) -> Result<()>;

    /// Add one to a tester's contributions
    async fn add_beta_contribution(&self, user_id: &str) -> Result<()>;

    /// Save a new bug report
    async fn insert_bug_report(&self, report: &BugReport) -> Result<()>;

    /// One bug report
    async fn fetch_bug_report(&self, id: &str) -> Result<BugReport>;

    /// Replace a bug report if nobody changed it since `expected_updated_at`
    async fn update_bug_report(&self, report: &BugReport, expected_updated_at: i64) -> Result<bool>;

    /// List bug reports, newest first
    async fn fetch_bug_reports(&self, query: BugReportQuery<'_>) -> Result<Vec<BugReport>>;

    /// Reports by one author since a time (rate limit)
    async fn count_bug_reports_since(&self, author_id: &str, since: i64) -> Result<usize>;
}
