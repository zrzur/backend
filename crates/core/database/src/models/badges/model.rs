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

    /// An official appointment to the Zrzur team (`staff_appointments`, id
    /// `{user_id}:{role}`). The only source of the Staff and Platform
    /// Moderator badges: no badge permission gives them. Written only by
    /// `zrzur-staff appoint` / `end-appointment`, with two people.
    pub struct StaffAppointment {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// Badge id the appointment gives: `staff` or `platform_moderator`
        pub role: String,
        /// The two people who made the appointment (user ids)
        pub appointed_by: Vec<String>,
        /// Milliseconds since the epoch
        pub appointed_at: i64,
        pub reason: String,
        /// Platform Moderator: moderation rules, conduct and training
        /// acknowledged (milliseconds since the epoch)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub training_acknowledged_at: Option<i64>,
        /// Ended appointments are kept for the record
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ended_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub ended_by: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub end_reason: Option<String>,
    }

    /// State of a badge change that waits for a second person
    #[derive(Copy)]
    pub enum BadgeRequestStatus {
        /// Waiting for a second staff member
        Pending,
        /// A second staff member approved it; the change was made
        Approved,
        /// Refused (or withdrawn by the requester); nothing changed
        Refused,
    }

    /// A sensitive badge change one person asked for
    /// (`badge_change_requests`). Nothing changes until a different staff
    /// member with the right to make it approves.
    pub struct BadgeChangeRequest {
        #[serde(rename = "_id")]
        pub id: String,
        /// The person whose badge changes
        pub user_id: String,
        pub badge: String,
        /// `true` gives the badge, `false` takes it away
        pub grant: bool,
        pub reason: String,
        /// Expiry of the grant, if any (milliseconds since the epoch)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub grant_expires_at: Option<i64>,
        pub requested_by: String,
        /// Where it was asked: `staff_console`, `badge_command`, `cli`
        pub requested_via: String,
        pub status: BadgeRequestStatus,
        pub created_at: i64,
        /// Not approved by then: void
        pub expires_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_at: Option<i64>,
    }

    /// Where a server's community badges stand
    #[derive(Copy)]
    pub enum CommunityCheckStatus {
        /// Counted; nothing unusual
        Clear,
        /// The counted members joined, or their accounts were made, in a
        /// burst: nothing is given until staff look at it
        Held,
        /// Staff looked at a held server and let the badges through
        Released,
        /// Staff found the members manipulated: this server gives no
        /// community badge until they release it
        Refused,
    }

    /// The crond count behind Community Builder and Big Community for one
    /// server (`community_badge_checks`, id = server id). Counts and times
    /// only: never member ids, messages or addresses.
    pub struct CommunityBadgeCheck {
        #[serde(rename = "_id")]
        pub id: String,
        /// Owner at the last count
        pub owner_id: String,
        /// Members of any kind at the last count
        pub members: u32,
        /// Genuine human members at the last count
        pub counted: u32,
        /// Most counted members who joined within one hour
        pub join_burst: u32,
        /// Most counted accounts made within one hour
        pub signup_burst: u32,
        pub status: CommunityCheckStatus,
        /// Who this server gave Community Builder to (each server gives it
        /// once, to the owner when it first reached the count)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub builder_to: Option<String>,
        /// Who this server gave Big Community to
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub big_to: Option<String>,
        /// Milliseconds since the epoch
        pub checked_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_at: Option<i64>,
    }
);

/// A badge change request not approved within 7 days is void
pub const BADGE_REQUEST_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
