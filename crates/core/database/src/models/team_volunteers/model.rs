auto_derived!(
    /// Who a volunteer is: name, age and country given in "About you"
    /// (`team_identities`, `_id` = the user id). Seen only through
    /// "Show identity" and by staff with `TeamManage`.
    pub struct TeamIdentity {
        #[serde(rename = "_id")]
        pub id: String,
        pub name: String,
        pub age: u8,
        pub country: String,
        /// Version of the volunteer rules the person agreed to
        pub rules_version: String,
        /// Milliseconds since the epoch
        pub rules_at: i64,
        /// When the record is deleted (set when the person is not a volunteer
        /// any more)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub purge_at: Option<i64>,
        /// Given by a reviewer who holds no membership ("Add my details" to
        /// accept the rules): kept while they hold a reviewer role, and
        /// released `identity_keep_days` after they lose it
        #[serde(default)]
        pub reviewer: bool,
    }

    /// Where a volunteer stands
    #[derive(Copy)]
    pub enum TeamMemberStatus {
        Trial,
        Active,
        Away,
        Inactive,
        Left,
    }

    /// One thing that happened to a membership. `action` is a fixed word,
    /// never free text.
    pub struct TeamHistory {
        pub at: i64,
        /// Who did it (staff id, or the person's own id)
        pub actor: String,
        pub action: String,
    }

    /// A person's place in one team role (`team_members`); one per person
    /// and role
    pub struct TeamMember {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// Key of the team role in `[features.team_applications]`
        pub role: String,
        pub server: String,
        pub status: TeamMemberStatus,
        pub joined_at: i64,
        pub trial_ends_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub confirmed_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub away_until: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub left_at: Option<i64>,
        /// Months in a row below the monthly minimum
        #[serde(default)]
        pub low_months: u8,
        /// Staff notes. Part of the person's own export (GDPR Art. 15).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub notes: Option<String>,
        /// The single-use invite to the team server made for this
        /// membership; deleted when the membership ends or goes
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub invite: Option<String>,
        #[serde(default)]
        pub history: Vec<TeamHistory>,
    }

    /// Counts of one server on one day
    pub struct TeamActivityCount {
        pub server: String,
        pub messages: u32,
        pub replies: u32,
        pub moderation: u32,
        pub reviews: u32,
        pub tickets: u32,
    }

    /// What a volunteer did on one day (`team_activity_days`); counts only
    pub struct TeamActivityDay {
        /// `<user>:<yyyymmdd>`
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// yyyymmdd
        pub day: i32,
        pub counts: Vec<TeamActivityCount>,
    }

    /// Filter for listing members; `None` matches all. Always sorted by
    /// member id; `after` (exclusive) and `limit` page through the list the
    /// same way in every database driver. No `limit` means everything.
    #[derive(Default)]
    pub struct TeamMemberFilter {
        pub role: Option<String>,
        pub status: Option<TeamMemberStatus>,
        /// Only members whose id sorts after this one
        pub after: Option<String>,
        /// At most this many
        pub limit: Option<u32>,
    }
);

impl TeamMember {
    /// Whether the person still belongs to the team in this role
    pub fn is_current(&self) -> bool {
        self.status != TeamMemberStatus::Left
    }
}
