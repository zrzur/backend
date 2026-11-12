use crate::File;

auto_derived!(
    /// What a community item is
    pub enum CommunityItemKind {
        /// Frame drawn around an avatar
        Decoration,
        /// Background drawn behind a name in the member list
        Nameplate,
        /// Transparent animation that plays once over a profile
        Effect,
    }

    /// What a creator promised about an item (L-20)
    pub struct CommunityRights {
        /// Version of the Creator Terms they accepted
        pub terms_version: String,
        /// Milliseconds since the epoch
        pub confirmed_at: i64,
        /// Parts made by others, and their licence
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub third_party: Option<String>,
        /// Parts made with AI tools
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ai_parts: Option<String>,
    }

    /// Review state of a community item (or of a pending revision)
    ///
    /// Items made before review existed were stored as `Active`, `Hidden`
    /// or `Removed`; migration 55 renames them and the aliases keep old
    /// documents readable until it has run.
    pub enum CommunityItemStatus {
        /// Saved by its creator, not sent for review yet
        Draft,
        /// Waiting for a reviewer
        PendingReview,
        /// A reviewer asked the creator to change something
        ChangesRequested,
        /// Approved: visible to everyone and usable
        #[serde(alias = "Active")]
        Published,
        /// Not accepted
        Rejected,
        /// Taken out of the gallery by a reviewer (after reports, say)
        #[serde(alias = "Hidden")]
        Suspended,
        /// Withdrawn by its creator or an admin
        #[serde(alias = "Removed")]
        Archived,
    }

    /// A new version of a published item, waiting for review. The public
    /// image only changes when it is approved.
    pub struct CommunityRevision {
        /// Revision number it will get once approved
        pub revision: u32,
        pub name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub tags: Vec<String>,
        pub file: File,
        /// Draft, PendingReview, ChangesRequested or Rejected
        pub status: CommunityItemStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub review_note: Option<String>,
        /// Milliseconds since the epoch
        pub submitted_at: i64,
    }

    /// A decoration or nameplate made by a creator
    pub struct CommunityItem {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,
        /// Decoration, nameplate or effect
        pub kind: CommunityItemKind,
        /// Name shown in the gallery
        pub name: String,
        /// Who made it (always the uploader's session, never client input)
        pub creator_id: String,
        /// The image (PNG / APNG / GIF / WebP)
        pub file: File,
        /// Review state
        pub status: CommunityItemStatus,
        /// Short description shown on the details page
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        /// Search tags (lowercase)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub tags: Vec<String>,
        /// Approved revision number, starting at 1
        #[serde(default = "first_revision")]
        pub revision: u32,
        /// When it was first published (milliseconds since the epoch)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub published_at: Option<i64>,
        /// Last reviewer
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reviewed_by: Option<String>,
        /// Last note from a reviewer, shown to the creator
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub review_note: Option<String>,
        /// New version of a published item, waiting for review
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pending: Option<CommunityRevision>,
        /// Unique users who ever applied it
        #[serde(default)]
        pub popularity: i64,
        /// Bumped on every review change, so two reviewers can't both act
        #[serde(default)]
        pub version: i64,
        /// Users who reported it since its last review (each user counts once)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub reports: Vec<String>,
        /// What the creator promised; needed before an effect goes to review
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub rights: Option<CommunityRights>,
    }

    /// One entry in the review log (`community_reviews`)
    pub struct CommunityReview {
        #[serde(rename = "_id")]
        pub id: String,
        pub item_id: String,
        /// Revision the action was about
        pub revision: u32,
        /// Who acted (a reviewer, the creator or the system)
        pub actor_id: String,
        pub action: CommunityReviewAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        /// Milliseconds since the epoch
        pub created_at: i64,
    }

    /// What happened to an item
    pub enum CommunityReviewAction {
        /// Sent for review (before creators published directly; kept for
        /// older log entries)
        Submit,
        /// A creator published their own design directly, without review
        Publish,
        Approve,
        Reject,
        RequestChanges,
        Suspend,
        Archive,
        Report,
        /// An admin changed a decision someone else made
        Override,
    }

    /// Platform-level permissions of one user (`platform_roles`). Server
    /// roles, Plus, feathers and boosts never grant any of these.
    pub struct PlatformRole {
        /// User id
        #[serde(rename = "_id")]
        pub id: String,
        #[serde(default)]
        pub permissions: Vec<PlatformPermission>,
        /// Creator standing; `None` if never a creator
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub creator: Option<CreatorStanding>,
        /// The creator agreed to be shown by name to visitors without an
        /// account (public Designs site). Off unless they switch it on.
        #[serde(default)]
        pub show_public_name: bool,
        /// Milliseconds since the epoch
        pub updated_at: i64,
        pub updated_by: String,
    }

    /// A platform permission
    #[derive(Hash, Copy, PartialOrd, Ord)]
    pub enum PlatformPermission {
        /// Upload and submit community designs
        #[serde(rename = "cosmetics.submit")]
        CosmeticsSubmit,
        /// See and revise your own designs
        #[serde(rename = "cosmetics.manage_own")]
        CosmeticsManageOwn,
        /// Approve, reject, request changes, suspend
        #[serde(rename = "cosmetics.review")]
        CosmeticsReview,
        /// Grant, revoke and suspend creators
        #[serde(rename = "cosmetics.manage_creators")]
        CosmeticsManageCreators,
        /// Everything, including overriding reviews and granting reviewers
        #[serde(rename = "cosmetics.manage_all")]
        CosmeticsManageAll,
        /// Grant and revoke staff-given badges (not Staff itself)
        #[serde(rename = "badges.grant")]
        BadgesGrant,
        /// Run the beta program: invite, approve, remove, feature flags
        #[serde(rename = "beta.manage")]
        BetaManage,
        /// Review bug reports
        #[serde(rename = "bugs.review")]
        BugsReview,
        /// See and review security reports
        #[serde(rename = "bugs.security")]
        BugsSecurity,
        /// Trust & Safety: read the report queue, triage, notes
        #[serde(rename = "safety.review")]
        SafetyReview,
        /// Trust & Safety: take and revoke moderation actions, decide appeals
        #[serde(rename = "safety.action")]
        SafetyAction,
        /// Trust & Safety senior: escalations, reopening, legal holds
        #[serde(rename = "safety.manage")]
        SafetyManage,
        /// Review, approve, reject and revoke plugin releases; verify and
        /// suspend plugin publishers
        #[serde(rename = "plugins.review")]
        PluginsReview,
    }

    /// Whether a creator may currently submit
    pub enum CreatorStanding {
        Active,
        Suspended,
        Revoked,
    }

    /// One entry in the creator log (`creator_permissions`)
    pub struct CreatorPermissionLog {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub actor_id: String,
        pub action: CreatorAction,
        /// Permissions after the change
        #[serde(default)]
        pub permissions: Vec<PlatformPermission>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        /// Milliseconds since the epoch
        pub created_at: i64,
    }

    /// What an admin did to a creator
    pub enum CreatorAction {
        Grant,
        Revoke,
        Suspend,
        Reinstate,
        SetPermissions,
    }
);

fn first_revision() -> u32 {
    1
}

/// Reports from different users that take an item out of the gallery
/// until a reviewer looks at it
pub const COMMUNITY_REPORTS_TO_HIDE: usize = 5;

/// Version of the Creator Terms a creator accepts with an effect. Changes
/// when `website/src/policies/creator-terms.ts` changes in substance.
pub const CREATOR_TERMS_VERSION: &str = "2026-11-12";
