//! Zrzur Trust & Safety: reports, review cases, moderation actions,
//! account standing and appeals.
//!
//! A report is filed by a user against one target. Every target kind has
//! its own visibility check on the server, so a report can never be used
//! to read content the reporter could not already see. Reviewers work on
//! the resulting case; reporters only ever see a receipt.

use std::collections::BTreeMap;

#[cfg(feature = "validator")]
use validator::Validate;

/// Version of the reporting policy new reports are filed under
pub const SAFETY_POLICY_VERSION: &str = "2026-11-12";

/// `reporter_id` of reports filed without an account
pub const PUBLIC_REPORTER_ID: &str = "public";

/// Territorial scope of an action unless the reviewer narrows it
pub fn default_territorial_scope() -> String {
    "worldwide".to_string()
}

/// Longest report description
pub const SAFETY_DESCRIPTION_MAX: usize = 1000;

auto_derived!(
    /// What is being reported
    #[derive(Copy, Hash, PartialOrd, Ord)]
    pub enum SafetyTargetKind {
        /// A user account or part of its profile
        User,
        /// A message in a server channel, DM or group
        Message,
        /// A server
        Server,
        /// A server channel
        Channel,
        /// A bot account
        Bot,
        /// A developer application (its id is the bot's id)
        Application,
        /// A community design (decoration or nameplate)
        CommunityItem,
        /// A file attached to a message
        Attachment,
        /// An invite code
        Invite,
        /// A webhook that posted a message
        Webhook,
        /// A server's Discover listing
        DiscoverServer,
        /// A bot's Discover listing
        DiscoverBot,
    }

    /// Which part of a user's profile a report is about
    #[derive(Copy)]
    pub enum SafetyProfileFacet {
        Account,
        Avatar,
        Banner,
        DisplayName,
        Bio,
        Status,
        Decoration,
    }

    /// Reason category of a report
    #[derive(Copy, Hash, PartialOrd, Ord)]
    pub enum SafetyCategory {
        Harassment,
        HateOrThreats,
        Spam,
        ScamFraud,
        Impersonation,
        MaliciousLinks,
        Malware,
        PrivacyViolation,
        PersonalInformation,
        SexualContent,
        SelfHarm,
        IllegalContent,
        BotAbuse,
        MisleadingApplication,
        UnauthorisedContent,
        IntellectualProperty,
        Underage,
        Other,
    }

    /// Lifecycle state of a case
    #[derive(Copy, Hash, PartialOrd, Ord)]
    pub enum SafetyStatus {
        Submitted,
        Queued,
        UnderReview,
        MoreInfoRequired,
        Escalated,
        ActionTaken,
        NoViolation,
        Duplicate,
        Closed,
        Appealed,
        AppealResolved,
    }

    /// How urgent a case is
    #[derive(Copy, Hash, PartialOrd, Ord)]
    pub enum SafetyPriority {
        Low,
        Normal,
        High,
        Urgent,
    }

    /// Who handles a report
    #[derive(Copy)]
    pub enum SafetyRoute {
        /// Zrzur's Trust & Safety team. Security vulnerabilities go
        /// through the confidential bug-report flow (`bugs.security`), and
        /// server-rule reports to a server's moderators are not offered yet.
        Platform,
    }

    /// Reported object
    pub struct SafetyTarget {
        pub kind: SafetyTargetKind,
        pub id: String,
        /// Part of the profile (user reports)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub facet: Option<SafetyProfileFacet>,
        /// Message the target was seen in (attachments, webhooks, user context)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub message_id: Option<String>,
    }

    /// Final decision on a case
    pub struct SafetyResolution {
        pub decided_by: String,
        pub decided_at: i64,
        /// Shown to the reporter (no internal details)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub public_note: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub action_ids: Vec<String>,
    }

    /// Who filed a report without an account and what they said about it
    pub struct SafetyPublicReporter {
        /// Not asked for child sexual abuse material
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub name: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub email: Option<String>,
        /// The link or id the reporter gave for the content
        pub location: String,
        /// Law the reporter says is broken, if they know
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub law: Option<String>,
        /// Language of the page they used (receipt language)
        pub language: String,
        pub good_faith: bool,
    }

    /// A report and the review case built around it
    pub struct SafetyCase {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Short reference shown to the reporter (`ZR-XXXXXXXX`)
        pub reference: String,
        /// Reporter's user id, or `PUBLIC_REPORTER_ID` for a report filed
        /// without an account (see `public_reporter`)
        pub reporter_id: String,
        /// Who filed a report without an account (DSA Art. 16(2))
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub public_reporter: Option<SafetyPublicReporter>,
        pub target: SafetyTarget,
        /// Account responsible for the target (author, owner, creator)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub target_owner_id: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub server_id: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub channel_id: Option<String>,
        /// Server channel, DirectMessage, Group or SavedMessages
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub channel_type: Option<String>,
        pub route: SafetyRoute,
        pub category: SafetyCategory,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub subcategory: Option<String>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub description: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub evidence_ids: Vec<String>,
        pub status: SafetyStatus,
        pub priority: SafetyPriority,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub assignee_id: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub resolution: Option<SafetyResolution>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub duplicate_of: Option<String>,
        /// Request to the reporter while the case needs more information
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub info_request: Option<String>,
        pub policy_version: String,
        pub created_at: i64,
        pub updated_at: i64,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub closed_at: Option<i64>,
        /// Bumped on every change; updates with a stale version fail
        pub version: i64,
        /// A senior reviewer placed a legal hold: evidence of this case is
        /// never purged, and every later copy of the content is held too
        #[cfg_attr(feature = "serde", serde(default))]
        pub legal_hold: bool,
    }

    /// File referenced by evidence (metadata only, never the bytes)
    pub struct SafetyEvidenceFile {
        pub id: String,
        pub tag: String,
        pub filename: String,
        pub content_type: String,
        pub size: isize,
    }

    /// A neighbouring message kept for context
    pub struct SafetyContextMessage {
        pub id: String,
        pub author_id: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub content: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub attachments: usize,
    }

    /// Snapshot of reported content, taken when the report was filed
    pub struct SafetyEvidence {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub case_id: String,
        pub kind: SafetyTargetKind,
        pub object_id: String,
        pub captured_at: i64,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub author_id: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub title: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub content: Option<String>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub files: Vec<SafetyEvidenceFile>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub context: Vec<SafetyContextMessage>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub metadata: BTreeMap<String, String>,
        /// Deleted by the retention job after this time (unless held)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub expires_at: Option<i64>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub legal_hold: bool,
    }

    /// One entry of a case's audit trail. Entries are never edited.
    pub struct SafetyLogEntry {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub case_id: String,
        /// User id, or "system"
        pub actor_id: String,
        /// Created, Status, Assign, Priority, Note, EvidenceViewed, Action,
        /// ActionRevoked, Appeal, AppealDecision, ReporterInfo, Duplicate,
        /// LegalHold
        pub event: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub from: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub to: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub text: Option<String>,
        pub created_at: i64,
    }

    /// Kind of moderation action
    #[derive(Copy, Hash, PartialOrd, Ord)]
    pub enum SafetyActionKind {
        /// Recorded on the account's standing, nothing else changes
        Warning,
        /// The reported message is removed from its channel
        RemoveMessage,
        /// Account suspended for a number of days
        SuspendAccount,
        /// Account banned from the platform
        BanAccount,
        /// Bot / application can no longer sign in and is unlisted
        DisableBot,
        /// Server or bot removed from Discover
        RemoveDiscoverListing,
        /// Community design taken out of the gallery
        SuspendCommunityItem,
        /// Creator paused: their designs are hidden until they are reinstated
        SuspendCreator,
    }

    /// A reversal of an action
    pub struct SafetyRevocation {
        pub actor_id: String,
        pub reason: String,
        pub at: i64,
    }

    /// Why an action was taken (DSA Art. 17(3)(d) and (e)): either a law
    /// the content breaks, or one of Zrzur's own rules
    #[serde(tag = "type")]
    pub enum SafetyGround {
        /// The content is illegal under this law
        Law {
            /// Name of the law, e.g. "StGB (Germany)"
            law: String,
            /// The provision, e.g. "§ 130"
            provision: String,
        },
        /// The content is against Zrzur's terms, not (necessarily) illegal
        Terms {
            /// The rule, e.g. "Harassment"
            rule: String,
        },
    }

    /// What an action followed (DSA Art. 17(3)(b))
    #[derive(Copy, Default)]
    pub enum SafetyActionBasis {
        /// Not recorded (actions older than statements of reasons): not shown
        #[default]
        Unknown,
        /// A report from another person
        Report,
        /// A notice of illegal content, or a legal order
        Notice,
        /// Zrzur's own review
        OwnInitiative,
    }

    /// The statement of reasons that goes with an action (DSA Art. 17)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyStatement {
        /// Law or terms; required
        pub ground: SafetyGround,
        /// The facts and circumstances, shown to the person
        #[cfg_attr(feature = "validator", validate(length(min = 3, max = 1000)))]
        pub facts: String,
        /// "worldwide" or a list of countries
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 200)))]
        #[cfg_attr(feature = "serde", serde(default = "default_territorial_scope"))]
        pub territorial_scope: String,
        /// Automated tools found the content
        #[cfg_attr(feature = "serde", serde(default))]
        pub automated_detection: bool,
        /// The decision itself was automated
        #[cfg_attr(feature = "serde", serde(default))]
        pub automated_decision: bool,
    }

    /// A moderation action taken by a reviewer
    pub struct SafetyAction {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub case_id: Option<String>,
        pub actor_id: String,
        pub kind: SafetyActionKind,
        pub target: SafetyTarget,
        /// Account whose standing this action counts against
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub subject_user_id: Option<String>,
        pub category: SafetyCategory,
        /// Shown to the affected user
        pub public_reason: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub internal_note: Option<String>,
        pub created_at: i64,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub expires_at: Option<i64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub revoked: Option<SafetyRevocation>,
        pub appealable: bool,
        /// Automated tools found the content (DSA Art. 17(3)(c))
        #[cfg_attr(feature = "serde", serde(default))]
        pub automated_detection: bool,
        /// The decision was taken by automated means (Art. 17(3)(c))
        #[cfg_attr(feature = "serde", serde(default))]
        pub automated_decision: bool,
        /// Law or terms the action rests on (Art. 17(3)(d), (e))
        #[cfg_attr(feature = "serde", serde(default))]
        pub ground: SafetyGround,
        /// The facts and circumstances shown to the person (Art. 17(3)(b))
        #[cfg_attr(feature = "serde", serde(default))]
        pub facts: String,
        /// "worldwide" or a list of countries (Art. 17(3)(a))
        #[cfg_attr(feature = "serde", serde(default = "default_territorial_scope"))]
        pub territorial_scope: String,
        /// What the action followed (Art. 17(3)(b))
        #[cfg_attr(feature = "serde", serde(default))]
        pub basis: SafetyActionBasis,
    }

    /// State of an appeal
    #[derive(Copy, Hash, PartialOrd, Ord)]
    pub enum SafetyAppealStatus {
        Pending,
        Upheld,
        Overturned,
    }

    /// A user's appeal against an action on their account or content
    pub struct SafetyAppeal {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub action_id: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub case_id: Option<String>,
        pub user_id: String,
        pub statement: String,
        pub status: SafetyAppealStatus,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub reviewer_id: Option<String>,
        /// Shown to the user
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub decision_note: Option<String>,
        pub created_at: i64,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub decided_at: Option<i64>,
    }

    // ---- requests ----

    /// What to report
    pub struct DataSafetyTarget {
        pub kind: SafetyTargetKind,
        pub id: String,
        pub facet: Option<SafetyProfileFacet>,
        /// Message the target was seen in
        pub message_id: Option<String>,
        /// Server both users share (bots that are not public)
        pub server_id: Option<String>,
    }

    /// Report illegal content without an account (DSA Art. 16(2))
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreatePublicReport {
        /// What kind of thing the link or id points to
        pub target_kind: SafetyTargetKind,
        /// Where it is: a link to the message, server or profile, or its id
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 500)))]
        pub location: String,
        /// Why it is illegal
        #[cfg_attr(feature = "validator", validate(length(min = 10, max = 2000)))]
        pub description: String,
        pub category: SafetyCategory,
        #[cfg_attr(feature = "validator", validate(length(max = 48)))]
        pub subcategory: Option<String>,
        /// Which law is broken, if the reporter knows
        #[cfg_attr(feature = "validator", validate(length(max = 200)))]
        pub law: Option<String>,
        /// Reporter's name; not needed for child sexual abuse material
        #[cfg_attr(feature = "validator", validate(length(max = 100)))]
        pub name: Option<String>,
        /// Reporter's e-mail; not needed for child sexual abuse material
        #[cfg_attr(feature = "validator", validate(length(max = 254)))]
        pub email: Option<String>,
        /// The reporter states in good faith that the report is accurate
        pub good_faith: bool,
        /// Page language (en, ar, de), used for the receipt e-mail
        #[cfg_attr(feature = "validator", validate(length(max = 8)))]
        pub language: Option<String>,
        /// Cloudflare Turnstile token, when the check is switched on
        #[cfg_attr(feature = "validator", validate(length(max = 4000)))]
        pub captcha: Option<String>,
    }

    /// File a report
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateSafetyReport {
        pub target: DataSafetyTarget,
        pub category: SafetyCategory,
        #[cfg_attr(feature = "validator", validate(length(max = 48)))]
        pub subcategory: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 1000)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub description: Option<String>,
        /// Up to 5 more messages from the same channel as evidence
        #[cfg_attr(feature = "validator", validate(length(max = 5)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub evidence_message_ids: Vec<String>,
    }

    /// Answer a reviewer's request for more information
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyReporterInfo {
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 1000)))]
        pub text: String,
    }

    /// Change a case's state
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyCaseStatus {
        pub status: SafetyStatus,
        /// Shown to the reporter (more information requests, decisions)
        #[cfg_attr(feature = "validator", validate(length(max = 500)))]
        pub public_note: Option<String>,
        /// Internal note added to the audit trail
        #[cfg_attr(feature = "validator", validate(length(max = 2000)))]
        pub note: Option<String>,
        /// Case version the reviewer looked at
        pub version: i64,
    }

    /// Assign, prioritise or mark a duplicate
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyCaseEdit {
        /// User id; empty string unassigns
        pub assignee_id: Option<String>,
        pub priority: Option<SafetyPriority>,
        /// Case this one duplicates
        pub duplicate_of: Option<String>,
        pub legal_hold: Option<bool>,
        pub version: i64,
    }

    /// Internal note on a case
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyNote {
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 2000)))]
        pub text: String,
    }

    /// Take a moderation action on a case
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyAction {
        pub kind: SafetyActionKind,
        /// Category the action is for (defaults to the case's)
        pub category: Option<SafetyCategory>,
        /// Shown to the affected user
        #[cfg_attr(feature = "validator", validate(length(min = 3, max = 500)))]
        pub public_reason: String,
        #[cfg_attr(feature = "validator", validate(length(max = 2000)))]
        pub internal_note: Option<String>,
        /// Suspension length in days (SuspendAccount)
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 365)))]
        pub duration_days: Option<u32>,
        /// Answer to the reporter (shown on their receipt; no internal details)
        #[cfg_attr(feature = "validator", validate(length(max = 500)))]
        pub reporter_note: Option<String>,
        /// Case version the reviewer looked at
        pub version: i64,
    }

    /// Reverse an action
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyRevoke {
        #[cfg_attr(feature = "validator", validate(length(min = 3, max = 500)))]
        pub reason: String,
    }

    /// Triage several cases at once (never account actions)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyBulk {
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 50)))]
        pub ids: Vec<String>,
        /// Queued, UnderReview, NoViolation, Duplicate or Closed
        pub status: Option<SafetyStatus>,
        pub priority: Option<SafetyPriority>,
        /// Assign every case to yourself
        #[cfg_attr(feature = "serde", serde(default))]
        pub assign_self: bool,
        #[cfg_attr(feature = "validator", validate(length(max = 2000)))]
        pub note: Option<String>,
    }

    /// Appeal an action
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyAppeal {
        #[cfg_attr(feature = "validator", validate(length(min = 10, max = 2000)))]
        pub statement: String,
    }

    /// Decide an appeal
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSafetyAppealDecision {
        pub overturn: bool,
        /// Shown to the user
        #[cfg_attr(feature = "validator", validate(length(min = 3, max = 1000)))]
        pub note: String,
    }

    // ---- responses ----

    /// Status a reporter sees
    #[derive(Copy)]
    pub enum SafetyReceiptStatus {
        Submitted,
        InReview,
        NeedsInfo,
        ActionTaken,
        NoViolation,
        Closed,
    }

    /// What a reporter sees about their own report. Never contains
    /// reviewer identities, internal notes or other reports.
    pub struct SafetyReceipt {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Short reference to quote to support
        pub reference: String,
        pub target_kind: SafetyTargetKind,
        pub category: SafetyCategory,
        pub status: SafetyReceiptStatus,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub info_request: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub public_note: Option<String>,
        pub created_at: i64,
        pub updated_at: i64,
        /// The reporter already had an open report about this target
        #[cfg_attr(feature = "serde", serde(default))]
        pub existing: bool,
    }

    /// Reasons offered for one target kind
    pub struct SafetyReasonOption {
        pub category: SafetyCategory,
        #[cfg_attr(feature = "serde", serde(default))]
        pub subcategories: Vec<String>,
        /// A description is required
        pub needs_description: bool,
    }

    /// Account standing level, modelled on the five steps users know
    #[derive(Copy, PartialOrd, Ord)]
    pub enum SafetyStandingLevel {
        Good,
        Limited,
        VeryLimited,
        AtRisk,
        Suspended,
    }

    /// One action as the affected user sees it
    pub struct SafetyStandingEntry {
        pub action_id: String,
        pub kind: SafetyActionKind,
        pub category: SafetyCategory,
        pub public_reason: String,
        pub target_kind: SafetyTargetKind,
        pub created_at: i64,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub expires_at: Option<i64>,
        pub active: bool,
        pub revoked: bool,
        pub appealable: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub appeal_status: Option<SafetyAppealStatus>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub appeal_note: Option<String>,
        /// The statement of reasons (DSA Art. 17)
        #[cfg_attr(feature = "serde", serde(default))]
        pub facts: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub ground: SafetyGround,
        #[cfg_attr(feature = "serde", serde(default))]
        pub automated_detection: bool,
        #[cfg_attr(feature = "serde", serde(default))]
        pub automated_decision: bool,
        #[cfg_attr(feature = "serde", serde(default = "default_territorial_scope"))]
        pub territorial_scope: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub basis: SafetyActionBasis,
    }

    /// The signed-in user's account standing
    pub struct SafetyStanding {
        pub level: SafetyStandingLevel,
        pub entries: Vec<SafetyStandingEntry>,
        /// Certified out-of-court dispute settlement body (DSA Art. 21);
        /// none while no body has been chosen
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub out_of_court_body: Option<String>,
    }

    /// Everything a reviewer sees about one case
    pub struct SafetyCaseDetails {
        pub case: SafetyCase,
        pub evidence: Vec<SafetyEvidence>,
        pub log: Vec<SafetyLogEntry>,
        pub actions: Vec<SafetyAction>,
        /// Other reports about the same target (newest first, max 25)
        pub related: Vec<SafetyCase>,
        /// Open reports about the same target from distinct reporters
        pub distinct_reporters: usize,
        /// Appeals against this case's actions
        pub appeals: Vec<SafetyAppeal>,
    }

    /// Page of cases
    pub struct SafetyCasePage {
        pub cases: Vec<SafetyCase>,
        /// Pass as `before` for the next page
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub next: Option<String>,
    }

    /// Result of a bulk triage
    pub struct SafetyBulkResult {
        pub updated: Vec<String>,
        /// Case id → error type
        pub failed: BTreeMap<String, String>,
    }

    /// What the signed-in user may do in Trust & Safety
    pub struct SafetyAccess {
        pub review: bool,
        pub action: bool,
        pub manage: bool,
    }
);

impl Default for SafetyGround {
    /// What an action recorded before statements of reasons existed rests on
    fn default() -> Self {
        SafetyGround::Terms {
            rule: "Terms of Service".to_string(),
        }
    }
}

impl SafetyStatus {
    /// Case is still waiting for a decision
    pub fn is_open(&self) -> bool {
        matches!(
            self,
            SafetyStatus::Submitted
                | SafetyStatus::Queued
                | SafetyStatus::UnderReview
                | SafetyStatus::MoreInfoRequired
                | SafetyStatus::Escalated
        )
    }

    /// What the reporter is shown
    pub fn receipt(&self) -> SafetyReceiptStatus {
        match self {
            SafetyStatus::Submitted | SafetyStatus::Queued => SafetyReceiptStatus::Submitted,
            SafetyStatus::UnderReview | SafetyStatus::Escalated => SafetyReceiptStatus::InReview,
            SafetyStatus::MoreInfoRequired => SafetyReceiptStatus::NeedsInfo,
            SafetyStatus::ActionTaken
            | SafetyStatus::Appealed
            | SafetyStatus::AppealResolved => SafetyReceiptStatus::ActionTaken,
            SafetyStatus::NoViolation => SafetyReceiptStatus::NoViolation,
            SafetyStatus::Duplicate | SafetyStatus::Closed => SafetyReceiptStatus::Closed,
        }
    }
}

/// Categories offered on the public report form (Art. 16)
pub fn safety_public_categories() -> &'static [SafetyCategory] {
    use SafetyCategory::*;
    &[
        IllegalContent,
        SexualContent,
        HateOrThreats,
        ScamFraud,
        Malware,
        Harassment,
        PrivacyViolation,
        PersonalInformation,
        IntellectualProperty,
        Other,
    ]
}

/// Reports whose evidence the retention purge never deletes: child sexual abuse material,
/// anything about under-age users, and any case a reviewer put on hold
pub fn safety_keeps_evidence(category: SafetyCategory, subcategory: Option<&str>) -> bool {
    safety_is_csam(category, subcategory) || category == SafetyCategory::Underage
}

/// Child sexual abuse material: the one report where name and e-mail are optional
pub fn safety_is_csam(category: SafetyCategory, subcategory: Option<&str>) -> bool {
    category == SafetyCategory::SexualContent && subcategory == Some("minor_safety")
}

/// Short reference shown to the reporter: the random tail of the id
pub fn safety_reference(id: &str) -> String {
    let tail = if id.len() > 8 { &id[id.len() - 8..] } else { id };
    format!("ZR-{}", tail.to_uppercase())
}

/// Reason categories offered for a target kind. The server rejects any
/// other category, so the list in the client is only a convenience.
pub fn safety_categories(kind: SafetyTargetKind) -> &'static [SafetyCategory] {
    use SafetyCategory::*;
    use SafetyTargetKind as K;
    match kind {
        K::User => &[
            Harassment,
            HateOrThreats,
            Spam,
            ScamFraud,
            Impersonation,
            PrivacyViolation,
            PersonalInformation,
            SexualContent,
            SelfHarm,
            IllegalContent,
            Underage,
            Other,
        ],
        K::Message | K::Attachment => &[
            Harassment,
            HateOrThreats,
            Spam,
            ScamFraud,
            MaliciousLinks,
            Malware,
            PrivacyViolation,
            PersonalInformation,
            SexualContent,
            SelfHarm,
            IllegalContent,
            UnauthorisedContent,
            IntellectualProperty,
            Other,
        ],
        K::Server | K::Channel | K::DiscoverServer => &[
            Harassment,
            HateOrThreats,
            Spam,
            ScamFraud,
            Impersonation,
            MaliciousLinks,
            Malware,
            SexualContent,
            SelfHarm,
            IllegalContent,
            UnauthorisedContent,
            Other,
        ],
        K::Bot | K::Application | K::DiscoverBot => &[
            BotAbuse,
            MisleadingApplication,
            Spam,
            ScamFraud,
            MaliciousLinks,
            Malware,
            PrivacyViolation,
            Impersonation,
            HateOrThreats,
            SexualContent,
            IllegalContent,
            Other,
        ],
        K::Webhook => &[Spam, ScamFraud, MaliciousLinks, Malware, Harassment, Other],
        K::CommunityItem => &[
            HateOrThreats,
            SexualContent,
            IntellectualProperty,
            Impersonation,
            UnauthorisedContent,
            IllegalContent,
            Other,
        ],
        K::Invite => &[Spam, ScamFraud, MaliciousLinks, Malware, IllegalContent, Other],
    }
}

/// Allowed sub-reasons of a category
pub fn safety_subcategories(category: SafetyCategory) -> &'static [&'static str] {
    use SafetyCategory::*;
    match category {
        Harassment => &["targeted", "bullying", "unwanted_contact", "raid"],
        HateOrThreats => &["hate_speech", "violent_threat", "violent_extremism"],
        Spam => &["unsolicited_ads", "mass_mentions", "fake_engagement"],
        ScamFraud => &["phishing", "fake_giveaway", "financial_scam", "account_trading"],
        Impersonation => &["impersonates_me", "impersonates_someone", "impersonates_zrzur"],
        PersonalInformation => &["doxxing", "address_or_phone", "private_images"],
        SexualContent => &["unsolicited", "non_consensual", "minor_safety"],
        SelfHarm => &["self_harm", "suicide"],
        IllegalContent => &["drugs_goods", "extortion", "hacking", "violent_gore"],
        BotAbuse => &["spam_dms", "command_abuse", "excessive_permissions"],
        MisleadingApplication => &["fake_features", "deceptive_oauth", "fraudulent_links"],
        IntellectualProperty => &["copyright", "trademark"],
        Other => &["flashing"],
        _ => &[],
    }
}

/// Starting priority for a new case
pub fn safety_priority(category: SafetyCategory, subcategory: Option<&str>) -> SafetyPriority {
    use SafetyCategory::*;
    match (category, subcategory) {
        // an effect that flashes can harm the next person who opens a profile
        (_, Some("flashing")) => SafetyPriority::Urgent,
        (SexualContent, Some("minor_safety" | "non_consensual")) => SafetyPriority::Urgent,
        (SelfHarm, _) | (HateOrThreats, Some("violent_threat")) => SafetyPriority::Urgent,
        (PersonalInformation, Some("address_or_phone")) => SafetyPriority::Urgent,
        (IllegalContent | PersonalInformation | Underage | Malware, _) => SafetyPriority::High,
        (SexualContent | HateOrThreats | ScamFraud, _) => SafetyPriority::High,
        (Other, _) => SafetyPriority::Low,
        _ => SafetyPriority::Normal,
    }
}

/// Whether the reporter must describe the problem
pub fn safety_needs_description(category: SafetyCategory) -> bool {
    matches!(
        category,
        SafetyCategory::Other
            | SafetyCategory::Impersonation
            | SafetyCategory::IntellectualProperty
    )
}

/// Reviewer state changes. `ActionTaken`, `Appealed` and `AppealResolved`
/// are only reached through actions and appeals, never set directly.
pub fn safety_transition_allowed(from: SafetyStatus, to: SafetyStatus) -> bool {
    use SafetyStatus::*;
    matches!(
        (from, to),
        (Submitted, Queued | UnderReview | Escalated | NoViolation | Duplicate)
            | (Queued, UnderReview | Escalated | NoViolation | Duplicate)
            | (UnderReview, Queued | MoreInfoRequired | Escalated | NoViolation | Duplicate)
            | (MoreInfoRequired, UnderReview | Escalated | NoViolation | Closed)
            | (Escalated, UnderReview | NoViolation | Duplicate)
            | (ActionTaken | NoViolation | Duplicate | AppealResolved, Closed)
            // reopening a decided case is a senior decision (checked by the route)
            | (NoViolation | Duplicate | Closed, UnderReview)
    )
}

/// Reasons offered for a target kind, for the report dialog
pub fn safety_reason_options(kind: SafetyTargetKind) -> Vec<SafetyReasonOption> {
    safety_categories(kind)
        .iter()
        .map(|category| SafetyReasonOption {
            category: *category,
            subcategories: safety_subcategories(*category)
                .iter()
                .map(|s| s.to_string())
                .collect(),
            needs_description: safety_needs_description(*category),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users_cannot_reach_decisions_through_transitions() {
        use SafetyStatus::*;
        for from in [
            Submitted,
            Queued,
            UnderReview,
            MoreInfoRequired,
            Escalated,
            ActionTaken,
            NoViolation,
            Duplicate,
            Closed,
            Appealed,
            AppealResolved,
        ] {
            assert!(!safety_transition_allowed(from, ActionTaken));
            assert!(!safety_transition_allowed(from, Appealed));
            assert!(!safety_transition_allowed(from, AppealResolved));
            assert!(!safety_transition_allowed(from, Submitted));
        }
        assert!(safety_transition_allowed(Submitted, UnderReview));
        assert!(!safety_transition_allowed(Appealed, Closed));
    }

    #[test]
    fn categories_are_contextual() {
        assert!(!safety_categories(SafetyTargetKind::Message).contains(&SafetyCategory::Underage));
        assert!(!safety_categories(SafetyTargetKind::User).contains(&SafetyCategory::BotAbuse));
        assert!(safety_categories(SafetyTargetKind::Bot).contains(&SafetyCategory::BotAbuse));
    }

    #[test]
    fn reference_uses_random_tail() {
        assert_eq!(safety_reference("01HZZZZZZZZZZZZZZZABCDEFGH"), "ZR-ABCDEFGH");
    }

    #[test]
    fn urgent_categories() {
        assert_eq!(
            safety_priority(SafetyCategory::SexualContent, Some("minor_safety")),
            SafetyPriority::Urgent
        );
        assert_eq!(safety_priority(SafetyCategory::Other, None), SafetyPriority::Low);
    }
}
