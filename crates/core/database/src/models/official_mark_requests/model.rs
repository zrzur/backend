auto_derived!(
    /// State of a proposal to give or remove the official mark
    #[derive(Copy)]
    pub enum OfficialMarkStatus {
        /// Waiting for a second staff member
        Pending,
        /// A second staff member confirmed; the change was applied
        Confirmed,
        /// A second staff member said no; nothing changed
        Refused,
        /// The proposer took it back; nothing changed
        Withdrawn,
    }

    /// One staff member's proposal to give or remove the official mark
    /// (`official_mark_requests`). Nothing changes until a different staff
    /// member confirms it.
    pub struct OfficialMarkRequest {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// `true` gives the mark, `false` removes it
        pub grant: bool,
        /// Staff member who proposed
        pub requested_by: String,
        pub reason: String,
        pub status: OfficialMarkStatus,
        /// Milliseconds since the epoch
        pub created_at: i64,
        /// Not confirmed by then: void
        pub expires_at: i64,
        /// Staff member who confirmed or refused
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_at: Option<i64>,
    }

    /// Where a name lives, for the list of names that break the name rule
    #[derive(Copy)]
    pub enum NameScan {
        /// Usernames and display names, bots included
        Users,
        /// Server nicknames
        Nicknames,
        Webhooks,
    }

    /// One name in the database
    pub struct NameRecord {
        pub kind: NameScan,
        /// User id, webhook id or `server/user`
        pub id: String,
        pub name: String,
        /// Whether this user is a bot
        pub bot: bool,
        /// Official accounts and official bots may use reserved names
        pub official: bool,
    }
);

/// A proposal not confirmed within 7 days is void
pub const OFFICIAL_MARK_REQUEST_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
