auto_derived!(
    /// Where an application to join the team stands
    #[derive(Copy)]
    pub enum TeamApplicationStatus {
        /// Waiting for the review team
        Open,
        /// A reviewer is talking to the person
        Interview,
        Accepted,
        Rejected,
        /// Taken back by the person; the answers are erased
        Withdrawn,
    }

    /// What became of the server role of an accepted application
    #[derive(Copy)]
    pub enum TeamGrant {
        /// The person holds the role
        Granted,
        /// The person is not in that server yet; given once they join
        PendingJoin,
        /// Never given by the system: the owner gives it by hand
        Manual,
    }

    /// One answer of the form, with the question as the person saw it
    pub struct TeamAnswer {
        pub id: String,
        pub label: String,
        pub value: String,
    }

    /// An application to join the Zrzur team (`team_applications`)
    pub struct TeamApplication {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// Key of the team role in `[features.team_applications]`
        pub role: String,
        pub status: TeamApplicationStatus,
        /// Whether the age stated on the account was 18 or over when the
        /// person applied. The only age fact kept.
        #[serde(default)]
        pub adult: bool,
        /// Erased when the person withdraws
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub answers: Vec<TeamAnswer>,
        /// Milliseconds since the epoch
        pub submitted_at: i64,
        /// Reviewer who last acted. Never shown to the applicant by id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub acted_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub acted_at: Option<i64>,
        /// Reviewer who opened the interview
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub interviewer: Option<String>,
        /// Reason of a rejection, as sent to the person
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub grant: Option<TeamGrant>,
        /// When the record is deleted (set once it is decided or withdrawn)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub purge_at: Option<i64>,
    }
);

impl TeamApplication {
    pub fn is_open(&self) -> bool {
        matches!(
            self.status,
            TeamApplicationStatus::Open | TeamApplicationStatus::Interview
        )
    }
}
