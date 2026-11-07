auto_derived!(
    /// State of a change request
    #[derive(Copy)]
    pub enum AccountChangeStatus {
        /// Waiting for the user
        Pending,
        /// The user agreed; the change was applied
        Accepted,
        /// The user said no; nothing changed
        Declined,
        /// Withdrawn by staff before the user answered
        Cancelled,
    }

    /// Profile changes staff propose (only applied if the user accepts)
    #[derive(Default)]
    pub struct AccountChanges {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub display_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pronouns: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_text: Option<String>,
        /// Profile text
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub bio: Option<String>,
        /// Fields to clear: Avatar, ProfileBackground, DisplayName, Pronouns,
        /// StatusText, ProfileContent
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub remove: Vec<String>,
        /// New sign-in email (confirmed by a link to the current email)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub email: Option<String>,
        /// Turn off two-factor authentication (confirmed by a link to the
        /// current email)
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pub reset_totp: bool,
    }

    /// A change to someone's account that waits for their consent
    /// (`account_change_requests`)
    pub struct AccountChangeRequest {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// Staff member who asked
        pub requested_by: String,
        /// Shown to the user
        pub reason: String,
        pub changes: AccountChanges,
        pub status: AccountChangeStatus,
        /// Milliseconds since the epoch
        pub created_at: i64,
        /// Not answered by then: treated as declined
        pub expires_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_at: Option<i64>,
        /// SHA-256 of the emailed confirmation token (email and two-factor
        /// changes only; those are answered by the link, not in the app)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub token_hash: Option<String>,
    }
);

/// How long a user has to answer
pub const ACCOUNT_CHANGE_TTL_MS: i64 = 14 * 24 * 60 * 60 * 1000;
/// Fields staff may propose to clear
pub const ACCOUNT_CHANGE_REMOVABLE: [&str; 6] = [
    "Avatar",
    "ProfileBackground",
    "DisplayName",
    "Pronouns",
    "StatusText",
    "ProfileContent",
];
/// Open requests one user can have at once
pub const ACCOUNT_CHANGE_MAX_OPEN: usize = 3;
/// How long an emailed confirmation link works
pub const ACCOUNT_SECURITY_CHANGE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
