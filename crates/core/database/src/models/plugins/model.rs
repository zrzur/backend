//! Zrzur plugins. Six separate records, so one can change without
//! touching the others:
//!
//! - `plugins`: the definition (who publishes it, store listing state)
//! - `plugin_releases`: one approved-or-not package per version
//! - `plugin_installs`: a user's installation and on/off state
//! - `plugin_grants`: the permissions that user consented to
//! - `plugin_settings`: that user's settings for the plugin
//! - `plugin_reviews`: append-only review and security log
//!
//! plus `plugin_publishers` (verification and suspension of publishers).

auto_derived!(
    /// Store state of a plugin
    pub enum PluginStatus {
        /// Created by its publisher; nothing approved yet
        Draft,
        /// Has an approved release and shows in the store
        Listed,
        /// Taken out of the store by a reviewer; installs stop running
        Suspended,
        /// Withdrawn for good by its publisher or a reviewer
        Removed,
    }

    /// Review state of one release
    pub enum PluginReleaseStatus {
        /// Waiting for a reviewer
        PendingReview,
        /// Can be installed
        Approved,
        /// Not accepted
        Rejected,
        /// Was approved, then found unsafe; never runs again
        Revoked,
        /// Withdrawn by its publisher before review
        Withdrawn,
    }

    /// Highest risk of what a release asks for
    pub enum PluginRisk {
        Low,
        Medium,
        High,
    }

    /// One scanner finding
    pub struct PluginScanFinding {
        pub rule: String,
        /// Blocking findings reject the package outright
        pub block: bool,
        pub message: String,
        pub line: u32,
    }

    /// Plugin definition
    pub struct Plugin {
        /// Manifest id, e.g. `dev.example.focus`
        #[serde(rename = "_id")]
        pub id: String,
        /// Account that owns it (always the creating session, never input)
        pub publisher_id: String,
        pub name: String,
        pub summary: String,
        pub category: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub icon: Option<String>,
        pub status: PluginStatus,
        /// Made by Zrzur (only privileged accounts can create these)
        #[serde(default)]
        pub official: bool,
        /// Newest approved release
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub latest_release: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub latest_version: Option<String>,
        #[serde(default)]
        pub install_count: i64,
        /// Users who reported it since its last review
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub reports: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_reason: Option<String>,
        /// Milliseconds since the epoch
        pub created_at: i64,
        pub updated_at: i64,
        /// Bumped on every change, so two reviewers can't both act
        #[serde(default)]
        pub version: i64,
    }

    /// A versioned plugin package
    pub struct PluginRelease {
        #[serde(rename = "_id")]
        pub id: String,
        pub plugin_id: String,
        pub version: String,
        /// Manifest exactly as validated (JSON text; keys like `$schema`
        /// are not valid Mongo field names)
        pub manifest: String,
        /// Worker code, if any
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub code_sha256: Option<String>,
        /// SHA-256 of manifest text + code, for the audit log
        pub package_sha256: String,
        /// Bytes of manifest + code
        pub size: i64,
        pub permissions: Vec<String>,
        pub risk: PluginRisk,
        /// `declarative` or `worker`
        pub entrypoint: String,
        pub platforms: Vec<String>,
        pub min_app_version: String,
        pub status: PluginReleaseStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub changelog: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub scan: Vec<PluginScanFinding>,
        pub submitted_by: String,
        pub submitted_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reviewed_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reviewed_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub review_note: Option<String>,
    }

    /// A user's installation
    pub struct PluginInstall {
        /// `<user_id>:<plugin_id>`
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub plugin_id: String,
        /// Release the user consented to
        pub release_id: String,
        pub version: String,
        pub enabled: bool,
        pub installed_at: i64,
        pub updated_at: i64,
    }

    /// Permissions a user granted to one release of a plugin
    pub struct PluginGrant {
        /// `<user_id>:<plugin_id>`
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub plugin_id: String,
        pub release_id: String,
        pub permissions: Vec<String>,
        pub granted_at: i64,
    }

    /// A user's settings for one plugin
    pub struct PluginSettings {
        /// `<user_id>:<plugin_id>`
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub plugin_id: String,
        /// JSON object text, checked against the manifest's settings
        pub values: String,
        pub updated_at: i64,
    }

    /// Whether a publisher may submit
    pub enum PluginPublisherStatus {
        Active,
        Suspended,
    }

    /// Publisher standing (`plugin_publishers`); missing = active, unverified
    pub struct PluginPublisher {
        /// User id
        #[serde(rename = "_id")]
        pub id: String,
        pub status: PluginPublisherStatus,
        /// A reviewer checked who this is
        #[serde(default)]
        pub verified: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        pub updated_at: i64,
        pub updated_by: String,
    }

    /// What happened, for the review log
    pub enum PluginReviewAction {
        Create,
        Submit,
        /// Rejected by validation or the scanner before a human saw it
        AutoReject,
        Approve,
        Reject,
        Revoke,
        Withdraw,
        Suspend,
        Reinstate,
        Remove,
        Report,
        VerifyPublisher,
        SuspendPublisher,
        ReinstatePublisher,
    }

    /// One entry in the review log (`plugin_reviews`)
    pub struct PluginReviewLog {
        #[serde(rename = "_id")]
        pub id: String,
        pub plugin_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub release_id: Option<String>,
        pub actor_id: String,
        pub action: PluginReviewAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        pub created_at: i64,
    }
);

/// Reports from different users that take a plugin out of the store until
/// a reviewer looks at it
pub const PLUGIN_REPORTS_TO_SUSPEND: usize = 5;

/// Plugins one publisher may own (not counting removed ones)
pub const PLUGINS_PER_PUBLISHER: usize = 20;

/// Releases waiting for review per plugin
pub const PENDING_RELEASES_PER_PLUGIN: usize = 3;

/// Installs per user
pub const PLUGIN_INSTALLS_PER_USER: usize = 50;

impl PluginInstall {
    pub fn key(user_id: &str, plugin_id: &str) -> String {
        format!("{user_id}:{plugin_id}")
    }
}
