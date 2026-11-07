use super::{File, SafetyAppealStatus};

#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// What a community item is
    pub enum CommunityItemKind {
        /// Frame drawn around an avatar
        Decoration,
        /// Background drawn behind a name in member lists
        Nameplate,
        /// Transparent animation that plays once over a profile
        Effect,
    }

    /// Review state of a community item
    pub enum CommunityItemStatus {
        Draft,
        PendingReview,
        ChangesRequested,
        Published,
        Rejected,
        Suspended,
        Archived,
    }

    /// A new version of a published item, waiting for review
    pub struct CommunityRevision {
        pub revision: u32,
        pub name: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub description: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub tags: Vec<String>,
        pub file: File,
        pub status: CommunityItemStatus,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub review_note: Option<String>,
        /// Milliseconds since the epoch
        pub submitted_at: i64,
    }

    /// What a creator promised about an item (creator and reviewers only)
    pub struct CommunityRights {
        /// Version of the Creator Terms they accepted
        pub terms_version: String,
        /// When (milliseconds since the epoch)
        pub confirmed_at: i64,
        /// Parts made by others, and their licence
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub third_party: Option<String>,
        /// Parts made with AI tools
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub ai_parts: Option<String>,
    }

    /// The promise sent with an effect
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCommunityRights {
        /// "I made this or hold the rights to it, and I accept the Creator
        /// Terms": ticked by the creator, never by default
        pub confirmed: bool,
        /// The Creator Terms version they were shown
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub terms_version: String,
        #[cfg_attr(feature = "validator", validate(length(max = 280)))]
        pub third_party: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 280)))]
        pub ai_parts: Option<String>,
    }

    /// A decoration, nameplate or profile effect made by a creator
    pub struct CommunityItem {
        /// Unique Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Decoration, nameplate or effect
        pub kind: CommunityItemKind,
        /// Name shown in the gallery
        pub name: String,
        /// Who made it
        pub creator_id: String,
        /// The image
        pub file: File,
        /// Review state
        pub status: CommunityItemStatus,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub description: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub tags: Vec<String>,
        /// Approved revision number
        pub revision: u32,
        /// When it was first published (milliseconds since the epoch)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub published_at: Option<i64>,
        /// Unique users who applied it
        pub popularity: i64,
        /// In the signed-in user's favourites
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub favourite: bool,
        /// Last note from a reviewer (creator and reviewers only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub review_note: Option<String>,
        /// New version waiting for review (creator and reviewers only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub pending: Option<CommunityRevision>,
        /// Number of reports (creator and reviewers only)
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_zero_u32", default)
        )]
        pub reports: u32,
        /// Version to send with a review decision (creator and reviewers only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub version: Option<i64>,
        /// What the creator promised (creator and reviewers only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub rights: Option<CommunityRights>,
    }

    /// Who made a community item
    pub struct CommunityCreator {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Their display name, or their username without the tag
        pub display_name: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub avatar: Option<File>,
        /// Currently an active creator
        pub verified: bool,
        /// They opened a page of all their work; without it, don't link to one
        #[cfg_attr(feature = "serde", serde(default))]
        pub open_page: bool,
    }

    /// A person as staff see them. Never in a public answer: `username#tag`
    /// is how to reach someone.
    pub struct CreatorAccount {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub username: String,
        pub discriminator: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub display_name: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub avatar: Option<File>,
        /// Currently an active creator
        pub verified: bool,
    }

    /// A page of community items with their creators
    pub struct CommunityPage {
        pub items: Vec<CommunityItem>,
        pub creators: Vec<CommunityCreator>,
    }

    /// Query for the community gallery
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsCommunityQuery {
        /// Search by name, tag or creator
        #[cfg_attr(feature = "validator", validate(length(max = 64)))]
        pub query: Option<String>,
        /// `new` (default) or `popular`
        pub sort: Option<String>,
        /// Only designs by this user
        #[cfg_attr(feature = "validator", validate(length(max = 64)))]
        pub creator: Option<String>,
        /// Only my favourites
        pub favourites: Option<bool>,
        /// Maximum number of items (1-100, default 48)
        pub limit: Option<i64>,
        /// Number of items to skip
        pub offset: Option<i64>,
    }

    /// Submit a new design (creators only)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateCommunityItem {
        /// Name shown in the gallery
        #[cfg_attr(feature = "validator", validate(length(min = 2, max = 32)))]
        pub name: String,
        /// Id of a file uploaded to the `decorations` tag, or to `effects`
        /// for an effect
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub file: String,
        #[cfg_attr(feature = "validator", validate(length(max = 280)))]
        pub description: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 8)))]
        pub tags: Option<Vec<String>>,
        /// Send for review now (otherwise it stays a draft)
        pub submit: Option<bool>,
        /// Needed to send an effect for review
        #[cfg_attr(feature = "validator", validate)]
        pub rights: Option<DataCommunityRights>,
    }

    /// Change a design (creators only). For a published design this makes
    /// a pending revision; the public version stays until it is approved.
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataEditCommunityItem {
        #[cfg_attr(feature = "validator", validate(length(min = 2, max = 32)))]
        pub name: Option<String>,
        /// New image uploaded to the `decorations` tag, or `effects`
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub file: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 280)))]
        pub description: Option<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 8)))]
        pub tags: Option<Vec<String>>,
        /// Send for review now
        pub submit: Option<bool>,
        /// Needed again when an effect gets a new file
        #[cfg_attr(feature = "validator", validate)]
        pub rights: Option<DataCommunityRights>,
    }

    /// A review decision
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCommunityDecision {
        /// Why (required to reject, request changes or suspend)
        #[cfg_attr(feature = "validator", validate(length(max = 500)))]
        pub reason: Option<String>,
        /// The `version` the reviewer looked at; the decision fails if the
        /// design changed since
        pub version: Option<i64>,
    }

    /// One entry of an item's review history
    pub struct CommunityReviewEntry {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub revision: u32,
        pub actor_id: String,
        /// Publish, Submit (older entries), Approve, Reject, RequestChanges,
        /// Suspend, Archive, Report, Override
        pub action: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub reason: Option<String>,
        pub created_at: i64,
    }

    /// One design with everything the signed-in user may see about it
    pub struct CommunityItemDetails {
        pub item: CommunityItem,
        pub creator: Option<CommunityCreator>,
        /// Version to send with a review decision
        pub version: i64,
        /// Review history (creator and reviewers only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub history: Vec<CommunityReviewEntry>,
        /// Users who reported it (reviewers only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub reporters: Vec<String>,
    }

    /// Review queue
    pub struct CommunityReviewQueue {
        /// New designs waiting for review
        pub pending: Vec<CommunityItem>,
        /// Published designs with a new version waiting
        pub revisions: Vec<CommunityItem>,
        /// Published designs with reports
        pub reported: Vec<CommunityItem>,
        pub creators: Vec<CommunityCreator>,
    }

    /// What the signed-in user may do with community designs
    pub struct CommunityPermissions {
        /// `cosmetics.submit`, `cosmetics.manage_own`, `cosmetics.review`,
        /// `cosmetics.manage_creators`, `cosmetics.manage_all`
        pub permissions: Vec<String>,
        /// Active, Suspended or Revoked; absent if never a creator
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub creator: Option<String>,
        /// Named to visitors, with an open page of their work (default off)
        #[cfg_attr(feature = "serde", serde(default))]
        pub show_public_name: bool,
    }

    /// Creator's own choice about the public Designs site
    pub struct DataCreatorPublicName {
        /// Show my name and avatar to visitors, and open a page of all my work
        pub show_public_name: bool,
    }

    /// A creator's own standing: why they are suspended and how to appeal
    pub struct CommunityCreatorStanding {
        /// Active, Suspended or Revoked
        pub standing: String,
        /// Why, in the words shown to the creator
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub reason: Option<String>,
        /// When it started (milliseconds since the epoch)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub since: Option<i64>,
        /// Goes to `POST /safety/standing/{action_id}/appeal`
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub action_id: Option<String>,
        /// There is an action and it has no appeal yet
        #[cfg_attr(feature = "serde", serde(default))]
        pub can_appeal: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub appeal_status: Option<SafetyAppealStatus>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub appeal_note: Option<String>,
    }

    /// Where to fetch your own copy of a design
    pub struct CommunityItemDownload {
        /// Send your session with the request: an unpublished file answers
        /// nobody else
        pub url: String,
        pub filename: String,
    }

    /// A creator profile: who they are and what they published
    pub struct CommunityCreatorProfile {
        pub creator: CommunityCreator,
        pub items: Vec<CommunityItem>,
    }

    /// Staff view of one user's platform permissions
    pub struct CreatorRecord {
        pub user: CreatorAccount,
        pub permissions: Vec<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub creator: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub updated_at: Option<i64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub updated_by: Option<String>,
    }

    /// One entry of a user's creator history
    pub struct CreatorLogEntry {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub actor_id: String,
        /// Grant, Revoke, Suspend, Reinstate, SetPermissions
        pub action: String,
        pub permissions: Vec<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub reason: Option<String>,
        pub created_at: i64,
    }

    /// Search users for creator management
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsCreatorSearch {
        /// Username, display name or user id; empty lists current creators
        #[cfg_attr(feature = "validator", validate(length(max = 64)))]
        pub query: Option<String>,
    }

    /// Change a creator's standing
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreatorAction {
        pub user: String,
        /// `grant`, `revoke`, `suspend` or `reinstate`; `suspend` needs a reason
        pub action: String,
        #[cfg_attr(feature = "validator", validate(length(max = 500)))]
        pub reason: Option<String>,
    }

    /// Set someone's platform permissions (admins with
    /// `cosmetics.manage_all` only)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataPlatformPermissions {
        pub user: String,
        pub permissions: Vec<String>,
        #[cfg_attr(feature = "validator", validate(length(max = 500)))]
        pub reason: Option<String>,
    }
);
