auto_derived!(
    /// A platform staff role. Roles are defined in code (see
    /// `StaffRole::permissions`), never edited in the database, so a
    /// changed document can't widen what a role means.
    #[derive(Hash, Copy, PartialOrd, Ord)]
    pub enum StaffRole {
        /// Everything, including staff management. Granted only from the
        /// command line on the server.
        #[serde(rename = "owner")]
        Owner,
        #[serde(rename = "security")]
        Security,
        #[serde(rename = "operations")]
        Operations,
        #[serde(rename = "trust_safety")]
        TrustSafety,
        #[serde(rename = "senior_support")]
        SeniorSupport,
        #[serde(rename = "support")]
        Support,
        #[serde(rename = "billing")]
        Billing,
        #[serde(rename = "analyst")]
        Analyst,
        #[serde(rename = "auditor")]
        Auditor,
    }

    /// A platform administration permission. Server roles, server
    /// ownership and the `privileged` flag never grant any of these.
    #[derive(Hash, Copy, PartialOrd, Ord)]
    pub enum StaffPermission {
        #[serde(rename = "platform.dashboard.read")]
        DashboardRead,
        #[serde(rename = "platform.analytics.read")]
        AnalyticsRead,
        #[serde(rename = "platform.users.read")]
        UsersRead,
        /// Email, IP and other personal data
        #[serde(rename = "platform.users.pii.read")]
        UsersPiiRead,
        #[serde(rename = "platform.users.suspend")]
        UsersSuspend,
        #[serde(rename = "platform.users.sessions.revoke")]
        UsersSessionsRevoke,
        #[serde(rename = "platform.users.delete.request")]
        UsersDeleteRequest,
        #[serde(rename = "platform.servers.read")]
        ServersRead,
        #[serde(rename = "platform.servers.restrict")]
        ServersRestrict,
        #[serde(rename = "platform.servers.delete.request")]
        ServersDeleteRequest,
        #[serde(rename = "platform.reports.read")]
        ReportsRead,
        #[serde(rename = "platform.reports.resolve")]
        ReportsResolve,
        #[serde(rename = "platform.support.read")]
        SupportRead,
        #[serde(rename = "platform.support.manage")]
        SupportManage,
        #[serde(rename = "platform.security.read")]
        SecurityRead,
        #[serde(rename = "platform.security.respond")]
        SecurityRespond,
        #[serde(rename = "platform.operations.read")]
        OperationsRead,
        #[serde(rename = "platform.operations.manage")]
        OperationsManage,
        #[serde(rename = "platform.billing.read")]
        BillingRead,
        #[serde(rename = "platform.billing.manage")]
        BillingManage,
        #[serde(rename = "platform.audit.read")]
        AuditRead,
        #[serde(rename = "platform.staff.read")]
        StaffRead,
        #[serde(rename = "platform.staff.manage")]
        StaffManage,
        #[serde(rename = "platform.settings.read")]
        SettingsRead,
        #[serde(rename = "platform.settings.manage")]
        SettingsManage,
        #[serde(rename = "platform.badges.manage")]
        BadgesManage,
        #[serde(rename = "platform.creators.manage")]
        CreatorsManage,
        /// Propose and confirm the official mark (two different people)
        #[serde(rename = "platform.official.manage")]
        OfficialManage,
    }

    /// Staff roles of one user (`platform_staff`)
    pub struct StaffGrant {
        /// User id
        #[serde(rename = "_id")]
        pub id: String,
        #[serde(default)]
        pub roles: Vec<StaffRole>,
        /// Access switched off; roles are kept for the record
        #[serde(default)]
        pub disabled: bool,
        /// Break-glass grants end here (milliseconds since the epoch)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expires_at: Option<i64>,
        /// Last TOTP time step used to sign in or step up (replay guard)
        #[serde(default)]
        pub last_totp_step: u64,
        /// Bumped on every change, for optimistic concurrency
        #[serde(default)]
        pub version: i64,
        pub updated_at: i64,
        /// User id, or `system:cli`
        pub updated_by: String,
    }

    /// A staff session (`staff_sessions`). Only the SHA-256 of the token is
    /// stored. It is bound to the ordinary session it was opened from.
    pub struct StaffSession {
        #[serde(rename = "_id")]
        pub id: String,
        pub token_hash: String,
        pub user_id: String,
        /// Id of the ordinary session this staff session belongs to
        pub session_id: String,
        pub created_at: i64,
        pub last_seen_at: i64,
        /// Hard end, whatever the activity
        pub expires_at: i64,
        /// Re-authenticated for high-risk actions until then
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub step_up_until: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub revoked_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub user_agent: Option<String>,
    }

    /// Result of an audited operation
    pub enum AuditOutcome {
        #[serde(rename = "success")]
        Success,
        #[serde(rename = "denied")]
        Denied,
        #[serde(rename = "failed")]
        Failed,
    }

    /// One record of the platform audit log (`platform_audit`). Records
    /// are only ever inserted; each one carries the hash of the one before,
    /// so a changed or removed record breaks the chain.
    pub struct PlatformAuditEntry {
        #[serde(rename = "_id")]
        pub id: String,
        /// Position in the chain, starting at 1 (unique)
        pub seq: i64,
        pub prev_hash: String,
        pub hash: String,
        /// User id, or `system:cli`
        pub actor_id: String,
        /// `staff.session.create`, `staff.grant.update`, ...
        pub action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub permission: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub target_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub target_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        pub outcome: AuditOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub staff_session_id: Option<String>,
        /// Short, sanitised before/after summary; never secrets or content
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub summary: Option<String>,
        pub created_at: i64,
    }
);

/// Collections the admin console counts. Their ids are ULIDs, so a
/// time window is an `_id` range (served by the `_id` index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaffCounted {
    Users,
    Servers,
    Channels,
    Messages,
}

impl StaffCounted {
    pub fn collection(&self) -> &'static str {
        match self {
            StaffCounted::Users => "users",
            StaffCounted::Servers => "servers",
            StaffCounted::Channels => "channels",
            StaffCounted::Messages => "messages",
        }
    }
}

/// Size of one collection (no contents)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaffCollectionStat {
    pub name: String,
    pub documents: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// Database health and size, read-only
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaffDbStats {
    /// `mongodb` or `reference`
    pub engine: String,
    pub collections: Vec<StaffCollectionStat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_bytes: Option<u64>,
}

/// Collections listed on the database health page
pub const STAFF_DB_COLLECTIONS: [&str; 14] = [
    "users",
    "accounts",
    "sessions",
    "servers",
    "server_members",
    "channels",
    "messages",
    "channel_invites",
    "bots",
    "attachments",
    "safety_cases",
    "platform_staff",
    "staff_sessions",
    "platform_audit",
];

/// Hard end of a staff session (8 hours)
pub const STAFF_SESSION_MAX_MS: i64 = 8 * 60 * 60 * 1000;
/// A staff session ends after this much inactivity (30 minutes)
pub const STAFF_SESSION_IDLE_MS: i64 = 30 * 60 * 1000;
/// How long a step-up (fresh TOTP) lasts (5 minutes)
pub const STAFF_STEP_UP_MS: i64 = 5 * 60 * 1000;
/// Break-glass grants from the command line last 24 hours
pub const STAFF_BREAK_GLASS_MS: i64 = 24 * 60 * 60 * 1000;
/// Actor id used for changes made from the command line
pub const STAFF_ACTOR_CLI: &str = "system:cli";
/// Hash before the first audit record
pub const AUDIT_GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";
