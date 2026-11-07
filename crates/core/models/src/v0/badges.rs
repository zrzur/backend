use super::{CreatorAccount, File};

#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// A badge in the catalogue. Names and descriptions are translated by
    /// clients from the id.
    pub struct BadgeInfo {
        pub id: String,
        /// Bit in `User.badges`
        pub bit: u32,
        /// official, community, contribution, program, subscription, history
        pub category: String,
        /// 0 regular, 1 rare, 2 top tier
        pub tier: u8,
        /// Lower is shown first
        pub priority: u16,
        /// dynamic, automatic, staff or privileged
        pub rule: String,
        /// No longer given out
        pub retired: bool,
    }

    /// One of your badges, held or not any more
    pub struct OwnBadge {
        pub badge: String,
        /// admin, system or legacy
        pub source: String,
        pub granted_at: i64,
        pub expires_at: Option<i64>,
        pub revoked_at: Option<i64>,
        /// expired, no longer eligible, a staff reason...
        pub revoke_reason: Option<String>,
        pub hidden: bool,
    }

    /// Progress towards the design badges
    pub struct CreatorProgress {
        /// Active, Suspended, Revoked or none
        pub standing: Option<String>,
        /// May upload designs (`cosmetics.submit`); separate from the badge
        pub can_submit: bool,
        pub published: usize,
        pub eligible: bool,
        pub super_creator_designs: usize,
    }

    /// Progress towards the bug badges
    pub struct BugProgress {
        /// Your confirmed or fixed reports
        pub verified: usize,
        /// Of those, security reports rated high or critical
        pub security_verified: usize,
        /// Verified reports that make you a candidate for Elite Bug Hunter
        pub elite_verified: usize,
    }

    /// Your beta program state
    pub struct BetaMembership {
        /// Requested, Invited, Active, Removed or Completed
        pub status: String,
        /// Beta features you can use right now
        pub features: Vec<String>,
        pub joined_at: Option<i64>,
        pub ended_at: Option<i64>,
        pub contributions: i64,
    }

    /// Everything the My Badges page shows
    pub struct MyBadges {
        pub badges: Vec<OwnBadge>,
        pub creator: CreatorProgress,
        pub bugs: BugProgress,
        pub beta: Option<BetaMembership>,
        /// Plus is active (from the subscription, not from a grant)
        pub plus: bool,
        /// What you may do in the staff tools
        pub permissions: Vec<String>,
        pub privileged: bool,
    }

    /// Public badge dates of a user
    pub struct PublicBadge {
        pub badge: String,
        pub granted_at: i64,
    }

    /// Show or hide one of your badges
    pub struct DataBadgeVisibility {
        pub hidden: bool,
    }

    /// Grant a badge (staff)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataGrantBadge {
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        pub user: String,
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        pub badge: String,
        /// Why; kept in the audit log
        #[cfg_attr(feature = "validator", validate(length(min = 3, max = 500)))]
        pub reason: String,
        /// Milliseconds since the epoch
        pub expires_at: Option<i64>,
    }

    /// Revoke a badge (staff)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataRevokeBadge {
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        pub user: String,
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        pub badge: String,
        #[cfg_attr(feature = "validator", validate(length(min = 3, max = 500)))]
        pub reason: String,
    }

    /// Staff view of one grant
    pub struct AdminBadgeGrant {
        pub badge: String,
        pub source: String,
        pub source_ref: Option<String>,
        pub granted_by: String,
        pub granted_at: i64,
        pub reason: Option<String>,
        pub expires_at: Option<i64>,
        pub revoked_at: Option<i64>,
        pub revoked_by: Option<String>,
        pub revoke_reason: Option<String>,
        pub hidden: bool,
        pub held: bool,
    }

    /// One entry of the badge audit log
    pub struct BadgeLogEntry {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub badge: String,
        pub actor_id: String,
        /// Grant, Revoke, Expire, Hide or Show
        pub action: String,
        pub reason: Option<String>,
        pub created_at: i64,
    }

    /// Whether staff can give one badge to this user now
    pub struct BadgeEligibility {
        pub badge: String,
        pub allowed: bool,
        pub reason: Option<String>,
    }

    /// Staff view of a user's badges
    pub struct AdminUserBadges {
        pub user: CreatorAccount,
        pub grants: Vec<AdminBadgeGrant>,
        pub log: Vec<BadgeLogEntry>,
        pub eligibility: Vec<BadgeEligibility>,
    }

    /// Search users or list holders of a badge (staff)
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsBadgeSearch {
        #[cfg_attr(feature = "validator", validate(length(max = 64)))]
        pub query: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 64)))]
        pub badge: Option<String>,
    }

    /// A user and their held badges (staff search)
    pub struct BadgeHolder {
        pub user: CreatorAccount,
        pub badges: Vec<String>,
    }

    /// A beta feature flag
    pub struct BetaFeature {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub name: String,
        pub description: Option<String>,
        pub enabled: bool,
        pub all_testers: bool,
        pub updated_at: i64,
    }

    /// Create or change a beta feature flag
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataBetaFeature {
        /// Lowercase letters, digits, `-` and `_`
        #[cfg_attr(feature = "validator", validate(length(min = 2, max = 48)))]
        pub id: String,
        #[cfg_attr(feature = "validator", validate(length(min = 2, max = 64)))]
        pub name: String,
        #[cfg_attr(feature = "validator", validate(length(max = 280)))]
        pub description: Option<String>,
        pub enabled: bool,
        pub all_testers: bool,
    }

    /// Change someone's beta membership (staff)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataBetaMember {
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        pub user: String,
        /// invite, approve, remove, complete or features
        pub action: String,
        /// Features this tester gets (for invite, approve and features)
        pub features: Option<Vec<String>>,
        #[cfg_attr(feature = "validator", validate(length(max = 500)))]
        pub note: Option<String>,
    }

    /// Staff view of one beta member
    pub struct BetaMemberRecord {
        pub user: CreatorAccount,
        pub status: String,
        pub features: Vec<String>,
        pub created_at: i64,
        pub joined_at: Option<i64>,
        pub ended_at: Option<i64>,
        pub contributions: i64,
        pub note: Option<String>,
    }

    /// Whether you can use one beta feature
    pub struct BetaAccess {
        pub feature: String,
        pub allowed: bool,
    }

    /// Report a bug, a security issue or beta feedback
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataBugReport {
        /// bug, security or feedback
        pub kind: String,
        #[cfg_attr(feature = "validator", validate(length(min = 5, max = 120)))]
        pub title: String,
        #[cfg_attr(feature = "validator", validate(length(min = 10, max = 4000)))]
        pub description: String,
        #[cfg_attr(feature = "validator", validate(length(max = 4000)))]
        pub steps: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 2000)))]
        pub expected: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 2000)))]
        pub actual: Option<String>,
        /// web, desktop-windows, desktop-macos, desktop-linux or other
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub platform: String,
        #[cfg_attr(feature = "validator", validate(length(max = 120)))]
        pub device: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 32)))]
        pub app_version: Option<String>,
        /// Beta feature id (feedback)
        #[cfg_attr(feature = "validator", validate(length(max = 48)))]
        pub feature: Option<String>,
        /// Ids of screenshots uploaded to `attachments`
        #[cfg_attr(feature = "validator", validate(length(max = 4)))]
        pub attachments: Option<Vec<String>>,
    }

    /// One step in a report's history
    pub struct BugHistoryEntry {
        pub actor_id: String,
        pub status: String,
        pub note: Option<String>,
        pub created_at: i64,
    }

    /// A bug report
    pub struct BugReport {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub author_id: String,
        /// Bug, Security or Feedback
        pub kind: String,
        pub title: String,
        pub description: String,
        pub steps: Option<String>,
        pub expected: Option<String>,
        pub actual: Option<String>,
        pub platform: String,
        pub device: Option<String>,
        pub app_version: Option<String>,
        pub feature: Option<String>,
        pub attachments: Vec<File>,
        /// Open, Triaged, NeedsInfo, Confirmed, Fixed, Duplicate, NotABug
        pub status: String,
        pub severity: Option<String>,
        pub duplicate_of: Option<String>,
        pub reviewer_note: Option<String>,
        /// Counts towards Bug Hunter
        pub verified: bool,
        pub history: Vec<BugHistoryEntry>,
        pub created_at: i64,
        pub updated_at: i64,
    }

    /// A report as reviewers see it
    pub struct BugReportReview {
        pub report: BugReport,
        pub author: Option<CreatorAccount>,
        /// Open reports with a similar title
        pub possible_duplicates: Vec<String>,
    }

    /// Reviewer decision on a report
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataBugReview {
        /// triage, need_info, confirm, fix, duplicate, not_a_bug or reopen
        pub action: String,
        #[cfg_attr(feature = "validator", validate(length(max = 1000)))]
        pub note: Option<String>,
        /// low, medium, high or critical
        pub severity: Option<String>,
        /// For `duplicate`
        pub duplicate_of: Option<String>,
        /// `updated_at` the reviewer looked at
        pub updated_at: i64,
    }

    /// Reviewer list filters
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsBugQueue {
        /// Comma separated statuses; empty = open ones
        pub status: Option<String>,
        /// Only security reports
        pub security: Option<bool>,
    }
);
