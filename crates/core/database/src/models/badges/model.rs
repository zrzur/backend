use crate::File;

auto_derived!(
    /// Where a badge grant came from
    pub enum BadgeSource {
        /// Staff granted it by hand
        Admin,
        /// Granted from a verified record: published designs, a confirmed
        /// bug report, beta membership
        System,
        /// The bit was set on the user before grants were tracked
        Legacy,
    }

    /// One badge a user holds or once held (`badge_grants`, id
    /// `{user_id}:{badge}`, so a badge can never be granted twice)
    pub struct BadgeGrant {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// Badge id from the catalogue (`creator`, `bug_hunter`, ...)
        pub badge: String,
        pub source: BadgeSource,
        /// What earned it (`bug:<id>`, `beta`, `designs`). Never shown to
        /// anyone but staff.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub source_ref: Option<String>,
        pub granted_by: String,
        /// Milliseconds since the epoch
        pub granted_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        /// The badge goes away at this time (milliseconds since the epoch)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expires_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub revoked_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub revoked_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub revoke_reason: Option<String>,
        /// Hidden from the profile by its owner (still held)
        #[serde(default, skip_serializing_if = "crate::if_false")]
        pub hidden: bool,
    }

    /// What happened to a badge
    pub enum BadgeAction {
        Grant,
        Revoke,
        Expire,
        Hide,
        Show,
    }

    /// One entry in the badge audit log (`badge_log`, id
    /// `{user_id}:{ulid}`)
    pub struct BadgeLogEntry {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub badge: String,
        /// Who acted (staff, the user, or the system)
        pub actor_id: String,
        pub action: BadgeAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        /// Milliseconds since the epoch
        pub created_at: i64,
    }

    /// A feature only beta testers can use (`beta_features`, id is a slug)
    pub struct BetaFeature {
        #[serde(rename = "_id")]
        pub id: String,
        pub name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        /// Switched off, nobody gets it
        pub enabled: bool,
        /// Every active tester gets it, not only the ones it was given to
        #[serde(default)]
        pub all_testers: bool,
        pub updated_at: i64,
        pub updated_by: String,
    }

    /// Where someone is in the beta program
    pub enum BetaStatus {
        /// Asked to join, waiting for staff
        Requested,
        /// Invited by staff, has not accepted yet
        Invited,
        /// In the program
        Active,
        /// Taken out of the program
        Removed,
        /// Finished the program (Beta Veteran)
        Completed,
    }

    /// Beta program membership (`beta_members`, id is the user id)
    pub struct BetaMember {
        #[serde(rename = "_id")]
        pub id: String,
        pub status: BetaStatus,
        /// Features given to this tester (on top of `all_testers` ones)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub features: Vec<String>,
        /// Staff member who invited or approved them
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub approved_by: Option<String>,
        /// Milliseconds since the epoch
        pub created_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub joined_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ended_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub note: Option<String>,
        /// Feedback and bug reports sent while active
        #[serde(default)]
        pub contributions: i64,
    }

    /// Where a bug report is
    pub enum BugStatus {
        Open,
        Triaged,
        /// The reviewer needs more from the reporter
        NeedsInfo,
        /// Reproduced by a reviewer: counts as a verified report
        Confirmed,
        /// Confirmed and fixed: counts as a verified report
        Fixed,
        Duplicate,
        NotABug,
    }

    /// What a report is about
    pub enum BugKind {
        Bug,
        /// Security issue: only the reporter and security reviewers see it
        Security,
        /// Beta feedback
        Feedback,
    }

    /// One step in a report's history
    pub struct BugHistoryEntry {
        pub actor_id: String,
        pub status: BugStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub note: Option<String>,
        pub created_at: i64,
    }

    /// A bug report (`bug_reports`)
    pub struct BugReport {
        #[serde(rename = "_id")]
        pub id: String,
        pub author_id: String,
        pub kind: BugKind,
        pub title: String,
        pub description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub steps: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expected: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub actual: Option<String>,
        /// web, desktop-windows, desktop-macos, desktop-linux, other
        pub platform: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub device: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub app_version: Option<String>,
        /// Beta feature this is about
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub feature: Option<String>,
        /// Screenshots
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub attachments: Vec<File>,
        pub status: BugStatus,
        /// low, medium, high or critical (set by a reviewer)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub severity: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub duplicate_of: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reviewed_by: Option<String>,
        /// Shown to the reporter
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reviewer_note: Option<String>,
        /// Lowercase words of the title, for finding duplicates
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub keywords: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub history: Vec<BugHistoryEntry>,
        pub created_at: i64,
        pub updated_at: i64,
    }
);
