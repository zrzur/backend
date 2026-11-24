//! Applications for the badge programs: Zrzur Partner, Verified Server (and
//! Verified Server Owner), Verified Bot Developer and Content Creator.
//!
//! The numbers of each program only let someone apply; a person on the team
//! reviews every application and decides with a statement of reasons. Data
//! kept, and for how long (owner-approved audit, items 31 and 32; legal
//! BV-03):
//!
//! - `badge_applications`: the application, the eligibility figures seen at
//!   submission (counts only), the decision and its reason codes. Kept while
//!   the badge stands and `record_days` after a refusal, a withdrawal or the
//!   end of the badge; untouched drafts go after `draft_days`.
//! - `badge_evidence`: links and short notes the applicant adds. Private:
//!   the applicant and reviewers with `platform.badges.manage` only, every
//!   staff opening written to the audit log. Deleted `evidence_days` after
//!   the final decision (or the end of the appeal window). Never identity
//!   documents.
//! - `badge_reviews`: who did what on an application, with reason codes.
//!   Kept with the application.
//! - `server_activity_weeks`: per server and week, counts only (active
//!   members, contributors, joins, joins still there a week later). Never a
//!   member id, never message content. Kept 26 weeks.
//!
//! The ownership code of a DNS, website or profile challenge is stored only
//! as a SHA-256 hash, shown once to the applicant, and dropped with the
//! evidence.
//!
//! Content Creator (Phase 5): the applicant names one channel or profile on
//! another platform and places a code in its description or a pinned post.
//! Zrzur never calls the platform: a reviewer opens the public page, types
//! the code they saw (it is compared with the hash), and records the
//! follower figure they saw with its date, the activity and works they
//! counted, and the seven quality criteria. Nothing else about the channel
//! is stored (legal BV-02, BV-05).

auto_derived!(
    /// A program someone applies for
    #[derive(Copy, Hash)]
    pub enum BadgeProgram {
        /// Zrzur Partner (badge `partner`, on the server's owner)
        Partner,
        /// Verified Server mark and the Verified Server Owner badge
        VerifiedServer,
        /// Verified mark on a bot and the Verified Bot Developer badge
        VerifiedBot,
        /// Content Creator (badge `content_creator`, on the applicant): an
        /// established creator of original content on another platform
        ContentCreator,
    }

    /// Where an application stands
    #[derive(Copy)]
    pub enum ApplicationStatus {
        /// Being written; only the applicant sees it
        Draft,
        /// Sent; waits for a reviewer (no promised date: volunteers)
        Submitted,
        /// A reviewer took it
        InReview,
        /// The reviewer asked for more information; the applicant edits and
        /// sends it again
        MoreInfo,
        /// Approved: the mark or badge was given
        Approved,
        /// Not approved; appeal within the window, apply again after the wait
        Refused,
        /// Taken back by the applicant
        Withdrawn,
        /// Was approved; ended (ownership moved, server or bot deleted or
        /// suspended, a recheck ended it)
        Ended,
    }

    /// How control of a domain is proven
    #[derive(Copy)]
    pub enum OwnershipMethod {
        /// A TXT record at `_zrzur-verification.<domain>`
        Dns,
        /// A file at `https://<domain>/.well-known/zrzur-verification.txt`
        Website,
        /// Content Creator: the code in the description or a pinned post of
        /// the named channel or profile, seen by a reviewer (never fetched)
        Profile,
    }

    /// An ownership challenge: only the hash of the code is kept
    pub struct OwnershipChallenge {
        pub method: OwnershipMethod,
        /// Lower-case host name, no scheme or path
        pub domain: String,
        /// SHA-256 of the code, hex; emptied when the evidence is deleted
        pub code_hash: String,
        pub issued_at: i64,
        /// The code is refused after this
        pub expires_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub verified_at: Option<i64>,
    }

    /// One line of the eligibility checklist. `met` is `None` when the
    /// server has no data to decide it (the reviewer checks it by hand).
    pub struct EligibilityItem {
        /// Stable code; the apps show it in the reader's language
        pub code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub met: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub value: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub needed: Option<i64>,
    }

    /// Content Creator: the one channel or profile the application is about,
    /// as the applicant names it
    pub struct CreatorChannel {
        /// `youtube`, `tiktok`, `twitch`, `instagram`, `x`, `kick`,
        /// `facebook` or `other`; shown as plain text, never a logo
        pub platform: String,
        /// The platform's name when `platform` is `other`
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub platform_name: Option<String>,
        /// Public https address of the channel or profile
        pub url: String,
        /// `education`, `technology`, `gaming`, `livestream`, `tutorials`,
        /// `creative`, `comedy`, `art`, `film`, `commentary` or `other`
        pub category: String,
        /// Public https links to recent original works (at most 10)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub works: Vec<String>,
        /// A regular livestream schedule, in the applicant's words
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub schedule: Option<String>,
    }

    /// Content Creator: what a reviewer saw on the public channel page and
    /// recorded by hand. Only the figure, its date and the counts; never a
    /// follower list or a copy of the page.
    pub struct CreatorFigures {
        /// Followers or subscribers on that one platform
        pub followers: i64,
        /// The day the reviewer saw the figure
        pub seen_at: i64,
        /// Days of active publishing the reviewer counted
        pub active_days: i64,
        /// Substantial original works in the last 90 days
        pub works: i64,
        /// A regular livestream schedule counts in place of the works
        #[serde(default)]
        pub schedule: bool,
        /// Quality criteria the reviewer found met: `originality`, `effort`,
        /// `consistency`, `authenticity` (no signs of bought followers or
        /// fake engagement in the review), `safety`, `honesty`, `rights`
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub quality: Vec<String>,
        pub recorded_by: String,
        pub recorded_at: i64,
    }

    /// Where an appeal stands
    #[derive(Copy)]
    pub enum AppealStatus {
        Open,
        /// The refusal was set aside: the application is reviewed again
        Upheld,
        /// The refusal stands
        Rejected,
    }

    /// One appeal against a refusal (one per decision)
    pub struct ApplicationAppeal {
        pub text: String,
        pub created_at: i64,
        pub status: AppealStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub reasons: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub note: Option<String>,
    }

    /// An application for a badge program (`badge_applications`)
    pub struct BadgeApplication {
        #[serde(rename = "_id")]
        pub id: String,
        pub program: BadgeProgram,
        pub applicant_id: String,
        /// Server id (Partner, Verified Server) or bot id (Verified Bot)
        pub target_id: String,
        pub status: ApplicationStatus,
        /// Verified Server below the numbers, as an official entity: needs
        /// a staff Owner with a step-up and a second person
        #[serde(default)]
        pub exception: bool,
        /// What the server or bot is and why it fits the program
        pub statement: String,
        /// Public https links the applicant names (website, rules page...)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub links: Vec<String>,
        /// The applicant confirmed they may represent the server, entity or
        /// bot
        #[serde(default)]
        pub authorized: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ownership: Option<OwnershipChallenge>,
        /// Content Creator: the channel or profile it is about
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub creator: Option<CreatorChannel>,
        /// Content Creator: what the reviewer saw and recorded
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub creator_figures: Option<CreatorFigures>,
        /// The checklist as it stood at submission (counts only)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub eligibility: Vec<EligibilityItem>,
        pub created_at: i64,
        /// Every change sets it; a save only goes through when it still
        /// matches (two reviewers at once)
        pub updated_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub submitted_at: Option<i64>,
        /// The reviewer who took it
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reviewer_id: Option<String>,
        /// Two-person approvals: who approved first
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub first_approval_by: Option<String>,
        /// Partner: the badge change request the first approval opened
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub badge_request_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_by: Option<String>,
        /// Reason codes of the last decision (shown in the reader's language)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub reasons: Vec<String>,
        /// The reviewer's words to the applicant
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub note: Option<String>,
        /// After a refusal: no new application for this target before then
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reapply_after: Option<i64>,
        /// After a refusal: the appeal window ends then
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub appeal_until: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub appeal: Option<ApplicationAppeal>,
        /// Partner: checked again then
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub next_review_at: Option<i64>,
        /// The recheck is due: waits for a reviewer (crond only flags it)
        #[serde(default)]
        pub recheck_due: bool,
        /// After a recheck warning: 30 days to improve
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub improve_until: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ended_at: Option<i64>,
        /// Code: `ownership_transferred`, `server_deleted`, `bot_deleted`,
        /// `bot_suspended`, `mark_removed`, `recheck`
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub end_reason: Option<String>,
        /// Evidence and the ownership code go then
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evidence_purge_at: Option<i64>,
        #[serde(default)]
        pub evidence_purged: bool,
        /// The whole record goes then
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub purge_at: Option<i64>,
    }

    /// A link or a short note the applicant adds (`badge_evidence`).
    /// Private, never published.
    pub struct BadgeEvidence {
        #[serde(rename = "_id")]
        pub id: String,
        pub application_id: String,
        pub applicant_id: String,
        /// `link` or `note`
        pub kind: String,
        pub text: String,
        pub created_at: i64,
    }

    /// One step a reviewer took (`badge_reviews`)
    pub struct BadgeReview {
        #[serde(rename = "_id")]
        pub id: String,
        pub application_id: String,
        pub reviewer_id: String,
        /// `take`, `approve_first`, `approve`, `refuse`, `more_info`,
        /// `appeal_upheld`, `appeal_rejected`, `recheck_keep`,
        /// `recheck_warning`, `recheck_end`, `evidence_viewed`
        pub action: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub reasons: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub note: Option<String>,
        pub created_at: i64,
    }

    /// A server's activity in one week, counts only
    /// (`server_activity_weeks`, id `<server>:<week>`)
    pub struct ServerActivityWeek {
        #[serde(rename = "_id")]
        pub id: String,
        pub server_id: String,
        /// Weeks since Monday 1970-01-05 (UTC)
        pub week: i64,
        /// People who wrote or read in the server's channels that week
        pub active: u32,
        /// People who wrote in the server's channels that week
        pub contributors: u32,
        /// Joins that week (insights counter; at least those still there)
        pub joins: u32,
        /// Of the people who joined that week, how many were still members
        /// a week after the week ended (filled in then)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub retained: Option<u32>,
        pub computed_at: i64,
    }
);

impl BadgeProgram {
    /// Stable name in the API and the audit log
    pub fn name(&self) -> &'static str {
        match self {
            BadgeProgram::Partner => "partner",
            BadgeProgram::VerifiedServer => "verified_server",
            BadgeProgram::VerifiedBot => "verified_bot",
            BadgeProgram::ContentCreator => "content_creator",
        }
    }

    pub fn from_name(name: &str) -> Option<BadgeProgram> {
        match name {
            "partner" => Some(BadgeProgram::Partner),
            "verified_server" => Some(BadgeProgram::VerifiedServer),
            "verified_bot" => Some(BadgeProgram::VerifiedBot),
            "content_creator" => Some(BadgeProgram::ContentCreator),
            _ => None,
        }
    }

    /// Whether the target is a bot
    pub fn targets_bot(&self) -> bool {
        matches!(self, BadgeProgram::VerifiedBot)
    }

    /// Whether the target is the applicant themself (else a server or bot)
    pub fn targets_person(&self) -> bool {
        matches!(self, BadgeProgram::ContentCreator)
    }

    /// The profile badge an approval gives through the badge gate (the
    /// marks of Verified Server and Verified Bot follow their own records)
    pub fn gate_badge(&self) -> Option<&'static str> {
        match self {
            BadgeProgram::Partner => Some("partner"),
            BadgeProgram::ContentCreator => Some("content_creator"),
            BadgeProgram::VerifiedServer | BadgeProgram::VerifiedBot => None,
        }
    }

    /// Every program, in the order the apps list them
    pub const ALL: [BadgeProgram; 4] = [
        BadgeProgram::Partner,
        BadgeProgram::VerifiedServer,
        BadgeProgram::VerifiedBot,
        BadgeProgram::ContentCreator,
    ];
}

impl ApplicationStatus {
    pub fn name(&self) -> &'static str {
        match self {
            ApplicationStatus::Draft => "Draft",
            ApplicationStatus::Submitted => "Submitted",
            ApplicationStatus::InReview => "InReview",
            ApplicationStatus::MoreInfo => "MoreInfo",
            ApplicationStatus::Approved => "Approved",
            ApplicationStatus::Refused => "Refused",
            ApplicationStatus::Withdrawn => "Withdrawn",
            ApplicationStatus::Ended => "Ended",
        }
    }

    pub fn from_name(name: &str) -> Option<ApplicationStatus> {
        [
            ApplicationStatus::Draft,
            ApplicationStatus::Submitted,
            ApplicationStatus::InReview,
            ApplicationStatus::MoreInfo,
            ApplicationStatus::Approved,
            ApplicationStatus::Refused,
            ApplicationStatus::Withdrawn,
            ApplicationStatus::Ended,
        ]
        .into_iter()
        .find(|status| status.name() == name)
    }

    /// Still being worked on: only one per program and target at a time
    pub fn is_open(&self) -> bool {
        matches!(
            self,
            ApplicationStatus::Draft
                | ApplicationStatus::Submitted
                | ApplicationStatus::InReview
                | ApplicationStatus::MoreInfo
        )
    }
}

impl AppealStatus {
    pub fn name(&self) -> &'static str {
        match self {
            AppealStatus::Open => "Open",
            AppealStatus::Upheld => "Upheld",
            AppealStatus::Rejected => "Rejected",
        }
    }
}

impl OwnershipMethod {
    pub fn name(&self) -> &'static str {
        match self {
            OwnershipMethod::Dns => "dns",
            OwnershipMethod::Website => "website",
            OwnershipMethod::Profile => "profile",
        }
    }

    pub fn from_name(name: &str) -> Option<OwnershipMethod> {
        match name {
            "dns" => Some(OwnershipMethod::Dns),
            "website" => Some(OwnershipMethod::Website),
            "profile" => Some(OwnershipMethod::Profile),
            _ => None,
        }
    }
}
