use std::collections::BTreeMap;

use iso8601_timestamp::Timestamp;
use once_cell::sync::Lazy;
use regex::Regex;

use super::{File, ProfileConnection, ProfileGames, UserActivity};

#[cfg(feature = "validator")]
use validator::Validate;

/// Regex for valid usernames
///
/// Block zero width space
/// Block lookalike characters
pub static RE_USERNAME: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\p{L}|[\d_.-])+$").unwrap());

/// Regex for valid display names
///
/// Block zero width space
/// Block newline and carriage return
pub static RE_DISPLAY_NAME: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[^\u200B\n\r]+$").unwrap());

auto_derived_partial!(
    /// User
    pub struct User {
        /// Unique Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Username
        pub username: String,
        /// Discriminator
        pub discriminator: String,
        /// Display name
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub display_name: Option<String>,
         /// User's pronouns
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub pronouns: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        /// Avatar attachment
        pub avatar: Option<File>,
        /// Relationships with other users
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Vec::is_empty", default)
        )]
        pub relations: Vec<Relationship>,

        /// Bitfield of user badges
        ///
        /// https://docs.rs/revolt-models/latest/revolt_models/v0/enum.UserBadges.html
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_zero_u32", default)
        )]
        pub badges: u32,
        /// User's current status
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub status: Option<UserStatus>,

        /// Enum of user flags
        ///
        /// https://docs.rs/revolt-models/latest/revolt_models/v0/enum.UserFlags.html
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_zero_u32", default)
        )]
        pub flags: u32,
        /// Whether this user is privileged
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub privileged: bool,
        /// Whether this is an official Zrzur account. Set by Zrzur staff only.
        /// Its messages can't be answered, reacted to, pinned or reported
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub official: bool,
        /// Whether this is a verified official bot: a bot Zrzur runs itself.
        /// Set on the server only. Not the same mark as `official`
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub official_bot: bool,
        /// Name of an official account per language (`ar`, `en`, `de`);
        /// `display_name` is the fallback
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub official_names: Option<BTreeMap<String, String>>,
        /// Bot information
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub bot: Option<BotInformation>,
        /// Profile look chosen by the user (theme, name style, decoration)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub style: Option<UserStyle>,

        /// Current session user's relationship with this user
        pub relationship: RelationshipStatus,
        /// Whether this user is currently online
        pub online: bool,
        /// What they are doing right now, the main activity first. Only
        /// while they are online and visible, share their activity and the
        /// server has the feature on; never stored.
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Vec::is_empty", default)
        )]
        pub activities: Vec<UserActivity>,
    },
    "PartialUser"
);

auto_derived!(
    /// Optional fields on user object
    pub enum FieldsUser {
        Avatar,
        StatusText,
        StatusPresence,
        ProfileContent,
        ProfileBackground,
        DisplayName,
        Pronouns,
        Style,

        /// Internal field, ignore this.
        Internal,
    }

    /// User's relationship with another user (or themselves)
    #[derive(Default)]
    pub enum RelationshipStatus {
        /// No relationship with other user
        #[default]
        None,
        /// Other user is us
        User,
        /// Friends with the other user
        Friend,
        /// Pending friend request to user
        Outgoing,
        /// Incoming friend request from user
        Incoming,
        /// Blocked this user
        Blocked,
        /// Blocked by this user
        BlockedOther,
    }

    /// Relationship entry indicating current status with other user
    pub struct Relationship {
        /// Other user's Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub user_id: String,
        /// Relationship status with them
        pub status: RelationshipStatus,
    }

    /// Presence status
    pub enum Presence {
        /// User is online
        Online,
        /// User is not currently available
        Idle,
        /// User is focusing / will only receive mentions
        Focus,
        /// User is busy / will not receive any notifications
        Busy,
        /// User appears to be offline
        Invisible,
    }

    /// User's active status
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct UserStatus {
        /// Custom status text
        #[validate(length(min = 0, max = 128))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub text: Option<String>,
        /// Emoji shown before the status text: one unicode emoji, or the id
        /// of a custom emoji from a server the user is in
        #[validate(length(min = 1, max = 32))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub emoji: Option<String>,
        /// When the status text (and its emoji) is cleared by the server
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub expires_at: Option<Timestamp>,
        /// Current presence option
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub presence: Option<Presence>,
    }

    /// User's profile
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct UserProfile {
        /// Text content on user's profile
        #[validate(length(min = 0, max = 2000))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub content: Option<String>,
        /// Background visible on user's profile
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub background: Option<File>,
        /// Links to other sites, real addresses only (https, at most
        /// [PROFILE_MAX_LINKS]). Not sent when the section is hidden.
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Vec::is_empty", default)
        )]
        pub links: Vec<String>,
        /// Interests, ids from [PROFILE_INTERESTS]. For showing only.
        /// Not sent when the section is hidden.
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Vec::is_empty", default)
        )]
        pub interests: Vec<String>,
        /// This person is an active creator and this viewer may know: never
        /// for a suspended or revoked creator, and on a minor's profile only
        /// for friends. Says nothing about their public creator page.
        #[cfg_attr(feature = "serde", serde(default))]
        pub creator: bool,
        /// Sections that show, in order. Hidden ones are never listed for
        /// other people; absent on a profile that kept the default.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub sections: Option<Vec<String>>,
        /// Sections the person turned off. Only sent to the person.
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Vec::is_empty", default)
        )]
        pub hidden_sections: Vec<String>,
        /// Games from a linked Steam account. Only when the person shows it,
        /// the `games` section is not hidden and this viewer may see
        /// personal sections. Filled by the profile route, not stored here.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub games: Option<ProfileGames>,
        /// Linked Twitch, GitHub and Battle.net accounts the person shows.
        /// Same rule as `games`, under the `connections` section. Filled by
        /// the profile route, not stored here.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub connections: Vec<ProfileConnection>,
    }

    /// User badge bitfield
    #[repr(u32)]
    pub enum UserBadges {
        /// Revolt Developer
        Developer = 1,
        /// Helped translate Revolt
        Translator = 2,
        /// Monetarily supported Revolt
        Supporter = 4,
        /// Responsibly disclosed a security issue
        ResponsibleDisclosure = 8,
        /// Revolt Founder
        Founder = 16,
        /// Platform moderator
        PlatformModeration = 32,
        /// Active monetary supporter
        ActiveSupporter = 64,
        /// Retired for good: never given, never shown; the bit stays reserved
        Paw = 128,
        /// Account made before `[api.users] early_adopter_cutoff` (computed;
        /// follows the "Member since" profile section)
        EarlyAdopter = 256,
        /// Amogus
        ReservedRelevantJokeBadge1 = 512,
        /// Low resolution troll face
        ReservedRelevantJokeBadge2 = 1024,
        /// Has Zrzur Feather (shown unless they hide it)
        Plus = 2048,
        /// Publishes profile designs and decorations in Zrzur
        Creator = 4096,
        /// Outstanding designer, chosen by staff
        SuperCreator = 8192,
        /// Reported a bug that staff verified
        BugHunter = 16384,
        /// Exceptional bug and security contributions, chosen by staff
        EliteBugHunter = 32768,
        /// In the beta program right now
        BetaTester = 65536,
        /// Completed the beta program
        BetaVeteran = 131072,
        /// Member of the Zrzur team
        Staff = 262144,
        /// Approved Zrzur partner
        Partner = 524288,
        /// At least one full calendar year on Zrzur (computed; follows the
        /// "Member since" profile section). Apps draw the highest year reached,
        /// up to ten, from this bit and the account id
        OneYear = 1048576,
        /// At least three full calendar years on Zrzur (computed, like `OneYear`)
        ThreeYears = 2097152,
        /// Owns a server that reached 100 members
        CommunityBuilder = 4194304,
        /// Owns a server that reached 1000 members
        BigCommunity = 8388608,
        /// Verified Server Owner: cancelled for good (2026-11-24), never
        /// given again; holders keep it, the bit stays reserved and is never
        /// reused. Verified servers carry their own mark instead.
        VerifiedServerOwner = 16777216,
        /// Owns a verified bot
        VerifiedBotDeveloper = 33554432,
        /// Saha Champion: retired for good (2026-11-24), never given again;
        /// the bit stays reserved and is never reused
        SahaChampion = 67108864,
        /// Helped people with support, chosen by staff
        CommunityHelper = 134217728,
        /// Established creator of original content on another platform:
        /// an application, a staff quality review and two people
        ContentCreator = 268435456,
    }

    /// User flag enum
    #[repr(u32)]
    pub enum UserFlags {
        /// User has been suspended from the platform
        SuspendedUntil = 1,
        /// User has deleted their account
        Deleted = 2,
        /// User was banned off the platform
        Banned = 4,
        /// User was marked as spam and removed from platform
        Spam = 8,
    }

    /// New user profile data
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataUserProfile {
        /// Text to set as user profile description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 2000)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub content: Option<String>,
        /// Attachment Id for background
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub background: Option<String>,
        /// Links to other sites; replaces the whole list
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub links: Option<Vec<String>>,
        /// Interest ids; replaces the whole list
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub interests: Option<Vec<String>>,
        /// Sections that show, in order
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub sections: Option<Vec<String>>,
        /// Sections that do not show
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub hidden_sections: Option<Vec<String>>,
    }

    /// New user information
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataEditUser {
        /// New display name
        #[cfg_attr(
            feature = "validator",
            validate(length(min = 2, max = 32), regex = "RE_DISPLAY_NAME")
        )]
        pub display_name: Option<String>,
        /// New pronouns
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 24)))]
        pub pronouns: Option<String>,
        /// Attachment Id for avatar
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub avatar: Option<String>,

        /// New user status
        #[cfg_attr(feature = "validator", validate)]
        pub status: Option<UserStatus>,
        /// New user profile data
        ///
        /// This is applied as a partial.
        #[cfg_attr(feature = "validator", validate)]
        pub profile: Option<DataUserProfile>,

        /// Bitfield of user badges
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub badges: Option<i32>,
        /// Enum of user flags
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub flags: Option<i32>,

        /// New profile look; replaces the whole style (validated against
        /// the known fonts and decorations)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub style: Option<UserStyle>,

        /// Fields to remove from user object
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsUser>,
    }

    /// How a user's name and profile look
    #[derive(Default)]
    pub struct UserStyle {
        /// One or two colours (#rrggbb) for the profile card and banner
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Vec::is_empty", default)
        )]
        pub theme: Vec<String>,
        /// Font for the display name, one of [USER_STYLE_FONTS]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub name_font: Option<String>,
        /// Name colour, #rrggbb: one (solid), two (gradient) or three to
        /// five (spectrum)
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Vec::is_empty", default)
        )]
        pub name_colour: Vec<String>,
        /// Effect drawn on the display name, one of [USER_STYLE_NAME_EFFECTS]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub name_effect: Option<String>,
        /// Avatar decoration: c:<id> of a community decoration
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub decoration: Option<String>,
        /// Image of a community decoration (set by the server, read-only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub decoration_file: Option<String>,
        /// Nameplate behind the name in member lists: c:<id> of a community
        /// nameplate
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub nameplate: Option<String>,
        /// Image of a community nameplate (set by the server, read-only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub nameplate_file: Option<String>,
        /// Profile effect: c:<id> of a community effect. It plays once when a
        /// profile opens; nobody records who saw it.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub effect: Option<String>,
        /// File of a community effect (set by the server, read-only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub effect_file: Option<String>,
        /// Frame drawn around the whole profile card: c:<id> of a community
        /// frame
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub profile_frame: Option<String>,
        /// Image of a community frame (set by the server, read-only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub profile_frame_file: Option<String>,
    }

    /// Who can start a direct message with a user
    ///
    /// Friends always can, blocked users never can, and anyone the user
    /// wrote to first can reply.
    #[derive(Default)]
    pub enum DirectMessagePrivacy {
        /// Anyone
        Everyone,
        /// Friends and people sharing a server or group with the user
        #[default]
        Mutual,
        /// Friends only
        Friends,
    }

    /// Who can add a user to group chats
    #[derive(Default)]
    pub enum GroupInvitePrivacy {
        /// Anyone who can invite people to the group
        Everyone,
        /// Friends only
        #[default]
        Friends,
        /// Nobody, not even friends
        Nobody,
    }

    /// A user's privacy settings (only ever sent to the user themselves)
    #[derive(Default)]
    pub struct UserPrivacy {
        /// Who can start a direct message
        #[cfg_attr(feature = "serde", serde(default))]
        pub direct_messages: DirectMessagePrivacy,
        /// Who can add this user to group chats
        #[cfg_attr(feature = "serde", serde(default))]
        pub group_invites: GroupInvitePrivacy,
        /// Others may see what this user is doing right now. Unset means
        /// off (for everyone, until the person turns it on). Answers always
        /// carry the value in force.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub share_activity: Option<bool>,
        /// Only friends see this user's activity: they are under 18, have
        /// not given their age, or their age could not be read. Set by the
        /// server in answers; ignored when sent.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub activity_friends_only: bool,
        /// The one-time activity intro card was answered (either button)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub activity_intro_seen: bool,
        /// The linked Steam account's "now playing" counts as an activity
        /// (only while `share_activity` is on). Off unless chosen.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub share_steam_activity: bool,
    }

    /// Change privacy settings; fields left out stay the same
    #[derive(Default)]
    pub struct DataEditPrivacy {
        /// Who can start a direct message
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub direct_messages: Option<DirectMessagePrivacy>,
        /// Who can add this user to group chats
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub group_invites: Option<GroupInvitePrivacy>,
        /// Share what you are doing right now
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub share_activity: Option<bool>,
        /// Share your Steam "now playing"
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub share_steam_activity: Option<bool>,
        /// The activity intro card was answered
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub activity_intro_seen: Option<bool>,
    }

    /// User flag reponse
    pub struct FlagResponse {
        /// Flags
        pub flags: i32,
    }

    /// Mutual friends, servers, groups and DMs response
    pub struct MutualResponse {
        /// Array of mutual user IDs that both users are friends with
        pub users: Vec<String>,
        /// Array of mutual server IDs that both users are in
        pub servers: Vec<String>,
        /// Array of mutual group and dm IDs that both users are in
        pub channels: Vec<String>,
    }

    /// Bot information for if the user is a bot
    pub struct BotInformation {
        /// Id of the owner of this bot. Sent only to the owner, the bot
        /// itself and platform staff; missing for everyone else (who made a
        /// bot is the owner's choice: `PublicBot.creator`)
        #[cfg_attr(
            feature = "serde",
            serde(rename = "owner", skip_serializing_if = "String::is_empty", default)
        )]
        pub owner_id: String,
        /// Verified by Trust & Safety
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub verified: bool,
    }

    /// User lookup information
    pub struct DataSendFriendRequest {
        /// Username and discriminator combo separated by #
        pub username: String,
    }

    /// Profile changes Zrzur asks you to agree to
    #[derive(Default)]
    pub struct AccountChangeSummary {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub display_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub pronouns: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub status_text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub bio: Option<String>,
        /// Fields that would be cleared
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub remove: Vec<String>,
    }

    /// A change to your account that waits for your answer
    pub struct AccountChangeRequestView {
        #[serde(rename = "_id")]
        pub id: String,
        /// Why Zrzur asks
        pub reason: String,
        pub changes: AccountChangeSummary,
        /// Pending, Accepted, Declined, Cancelled or Expired
        pub status: String,
        pub created_at: i64,
        pub expires_at: i64,
    }

    /// Answer a change request
    pub struct DataAnswerChangeRequest {
        pub accept: bool,
    }

    /// An email or two-factor change, as the emailed link shows it
    pub struct AccountSecurityChangeView {
        /// "email" or "two_factor"
        pub kind: String,
        /// The new email (email changes)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub new_email: Option<String>,
        /// Why Zrzur asks
        pub reason: String,
        /// Pending, Accepted, Declined, Cancelled or Expired
        pub status: String,
        pub expires_at: i64,
    }
);

auto_derived_partial!(
    /// Voice State information for a user
    pub struct UserVoiceState {
        pub id: String,
        pub joined_at: Timestamp,
        pub is_receiving: bool,
        pub is_publishing: bool,
        pub screensharing: bool,
        pub camera: bool,
    },
    "PartialUserVoiceState"
);

pub trait CheckRelationship {
    fn with(&self, user: &str) -> RelationshipStatus;
}

impl CheckRelationship for Vec<Relationship> {
    fn with(&self, user: &str) -> RelationshipStatus {
        for entry in self {
            if entry.user_id == user {
                return entry.status.clone();
            }
        }

        RelationshipStatus::None
    }
}

/// Fonts a display name can use (the client maps each id to a typeface)
pub const USER_STYLE_FONTS: &[&str] = &[
    "default", "bold", "serif", "mono", "script", "pixel", "wide", "rounded", "geo", "modern",
    "elegant", "strong", "clear", "soft", "slab", "classic", "comic", "typewriter", "code", "kufi",
];

/// Effects a display name can wear (the client draws each one)
pub const USER_STYLE_NAME_EFFECTS: &[&str] = &["halo", "edge", "lifted", "sheen"];

/// Most colours in a display name (a spectrum)
pub const USER_STYLE_NAME_MAX_COLOURS: usize = 5;

/// Profile sections a person can order or hide, in their default order.
/// Roles, mutual servers and the message box depend on who is looking, so
/// they keep their place.
pub const PROFILE_SECTIONS: &[&str] = &[
    "about",
    "links",
    "creations",
    "wearing",
    "member_since",
    "interests",
    "connections",
    "games",
];

/// Links on one profile
pub const PROFILE_MAX_LINKS: usize = 5;
/// Longest link, in characters
pub const PROFILE_LINK_MAX_LENGTH: usize = 256;
/// Interests on one profile
pub const PROFILE_MAX_INTERESTS: usize = 10;
/// The fixed list of interests. It leaves out belief, politics, health and
/// sexuality on purpose; a new entry needs a new legal look.
pub const PROFILE_INTERESTS: &[&str] = &[
    "gaming", "board_games", "music", "art", "design", "photography", "film", "anime",
    "reading", "writing", "languages", "travel", "cooking", "sports", "football", "fitness",
    "technology", "programming", "science", "nature", "pets", "fashion", "history", "cars",
];

/// Size of a community frame file, in pixels: twice the template the client
/// lays over the card (see [PROFILE_FRAME_TEMPLATE])
pub const PROFILE_FRAME_FILE_SIZE: (usize, usize) = (800, 1020);

/// Where a community frame may be drawn. It is a 400 x 510 template (CSS px)
/// laid over the card, which is the rectangle `card`: the frame may reach
/// 30 px past its sides, 70 px above it and 40 px below it. Rectangles are
/// left, top, right, bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileFrameTemplate {
    pub width: u32,
    pub height: u32,
    /// The card inside the template
    pub card: (u32, u32, u32, u32),
    /// Must stay transparent: it lies over the card's content
    pub safe: (u32, u32, u32, u32),
    /// May be drawn on although inside `safe`: the crest over the middle of
    /// the banner
    pub crest: (u32, u32, u32, u32),
}

pub const PROFILE_FRAME_TEMPLATE: ProfileFrameTemplate = ProfileFrameTemplate {
    width: 400,
    height: 510,
    card: (30, 70, 370, 470),
    safe: (40, 80, 360, 460),
    crest: (120, 80, 280, 110),
};

static RE_HEX_COLOUR: Lazy<Regex> = Lazy::new(|| Regex::new(r"^#[0-9a-fA-F]{6}$").unwrap());

/// c: followed by a ULID: a community item
static RE_COMMUNITY_REF: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^c:[0-9A-HJKMNP-TV-Z]{26}$").unwrap());

/// Community item id inside a c:<id> style value
pub fn community_item_ref(value: &str) -> Option<&str> {
    RE_COMMUNITY_REF.is_match(value).then(|| &value[2..])
}

impl UserStyle {
    /// Check every field against the allow-lists; colours must be `#rrggbb`
    /// (no CSS strings, so nothing can be injected into the client's styles)
    pub fn validate_style(&self) -> Result<(), &'static str> {
        let colours_ok =
            |list: &Vec<String>| list.len() <= 2 && list.iter().all(|c| RE_HEX_COLOUR.is_match(c));

        if !colours_ok(&self.theme) {
            return Err("theme must be one or two #rrggbb colours");
        }
        if self.name_colour.len() > USER_STYLE_NAME_MAX_COLOURS
            || !self.name_colour.iter().all(|c| RE_HEX_COLOUR.is_match(c))
        {
            return Err("name_colour must be one to five #rrggbb colours");
        }
        if let Some(effect) = &self.name_effect {
            if !USER_STYLE_NAME_EFFECTS.contains(&effect.as_str()) {
                return Err("unknown name_effect");
            }
        }
        if let Some(font) = &self.name_font {
            if !USER_STYLE_FONTS.contains(&font.as_str()) {
                return Err("unknown name_font");
            }
        }
        // Every design is a creator's: c:<id>. The built-in ones became
        // community designs on 2026-11-26, and a stored one was cleared
        // (migration `clear_builtin_style`), so their ids are refused.
        let designs = [
            (&self.decoration, "unknown decoration"),
            (&self.effect, "unknown effect"),
            (&self.nameplate, "unknown nameplate"),
            (&self.profile_frame, "unknown profile_frame"),
        ];
        for (value, error) in designs {
            if value.as_deref().is_some_and(|value| community_item_ref(value).is_none()) {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Nothing chosen at all
    pub fn is_empty(&self) -> bool {
        self.theme.is_empty()
            && self.name_font.is_none()
            && self.name_colour.is_empty()
            && self.name_effect.is_none()
            && self.decoration.is_none()
            && self.nameplate.is_none()
            && self.effect.is_none()
            && self.profile_frame.is_none()
    }
}

#[cfg(test)]
mod style_tests {
    use super::UserStyle;

    const DESIGN: &str = "c:01M5TGZFE80SKMC8JVGJKBHS2A";

    fn style() -> UserStyle {
        UserStyle {
            theme: vec!["#ff7a45".into(), "#1d1f2b".into()],
            name_font: Some("serif".into()),
            name_colour: vec!["#ffffff".into()],
            decoration: Some(DESIGN.into()),
            nameplate: Some(DESIGN.into()),
            ..Default::default()
        }
    }

    #[test]
    fn accepts_known_values() {
        assert!(style().validate_style().is_ok());
        assert!(UserStyle::default().validate_style().is_ok());
        assert!(UserStyle::default().is_empty());
    }

    #[test]
    fn rejects_css_and_unknown_values() {
        let mut s = style();
        s.theme = vec!["red; background: url(x)".into()];
        assert!(s.validate_style().is_err());

        let mut s = style();
        s.name_colour = vec!["#fff".into()];
        assert!(s.validate_style().is_err());

        let mut s = style();
        s.name_colour = vec!["#000000".into(); 6];
        assert!(s.validate_style().is_err());

        let mut s = style();
        s.name_colour = vec!["#000000".into(), "red".into(), "#222222".into()];
        assert!(s.validate_style().is_err());

        let mut s = style();
        s.name_effect = Some("glow; color: red".into());
        assert!(s.validate_style().is_err());

        let mut s = style();
        s.name_effect = Some("snow".into());
        assert!(s.validate_style().is_err());

        let mut s = style();
        s.name_font = Some("comic-sans".into());
        assert!(s.validate_style().is_err());

        let mut s = style();
        s.decoration = Some("../../etc".into());
        assert!(s.validate_style().is_err());

        let mut s = style();
        s.nameplate = Some("c:not-an-id".into());
        assert!(s.validate_style().is_err());
    }

    #[test]
    fn accepts_spectrum_and_name_effects() {
        for count in 1..=5 {
            let mut s = style();
            s.name_colour = vec!["#ff5c8a".into(); count];
            assert!(s.validate_style().is_ok(), "{count} colours");
        }
        for effect in super::USER_STYLE_NAME_EFFECTS {
            let mut s = style();
            s.name_effect = Some((*effect).into());
            assert!(s.validate_style().is_ok(), "{effect}");
        }
        let only_effect = UserStyle {
            name_effect: Some("halo".into()),
            ..Default::default()
        };
        assert!(!only_effect.is_empty());
    }

    /// The built-in designs became community designs on 2026-11-26: none of
    /// their ids is taken any more, in any design field
    #[test]
    fn a_built_in_design_is_refused() {
        let built_in = [
            "lightning", "laurel", "rank-gold", "shield-frame", "headphones", "aurora",
            "flag-sy", "zodiac-leo", "arcade-night", "galaxy", "ember", "snow", "glass-rain",
            "coin-rush", "crystal", "zrzur-neon", "coral-reef",
        ];
        for id in built_in {
            for field in 0..4 {
                let mut s = style();
                let value = Some(id.to_string());
                match field {
                    0 => s.decoration = value,
                    1 => s.nameplate = value,
                    2 => s.effect = value,
                    _ => s.profile_frame = value,
                }
                assert!(s.validate_style().is_err(), "{id} in field {field}");
            }
        }
    }

    #[test]
    fn profile_frames_are_checked() {
        let mut s = style();
        s.profile_frame = Some(DESIGN.into());
        assert!(s.validate_style().is_ok());
        for bad in ["gold", "c:not-an-id", "crystal; x", "khatam", "", "crystal"] {
            let mut s = style();
            s.profile_frame = Some(bad.into());
            assert!(s.validate_style().is_err(), "{bad}");
        }
        let s = super::UserStyle {
            profile_frame: Some(DESIGN.into()),
            ..Default::default()
        };
        assert!(!s.is_empty());
    }

    #[test]
    fn the_frame_template_keeps_the_card_clear() {
        let t = super::PROFILE_FRAME_TEMPLATE;
        let (fw, fh) = super::PROFILE_FRAME_FILE_SIZE;
        assert_eq!((fw as u32, fh as u32), (t.width * 2, t.height * 2));
        // the safe area is the card less 10 px on every side
        assert_eq!(
            t.safe,
            (t.card.0 + 10, t.card.1 + 10, t.card.2 - 10, t.card.3 - 10)
        );
        // the crest starts at the top of the safe area and stays clear of
        // the corner buttons (12 px in, two of 32 px with 8 between)
        assert!(t.crest.0 >= t.safe.0 && t.crest.2 <= t.safe.2 && t.crest.1 == t.safe.1);
        assert!(t.crest.2 <= t.card.2 - 12 - 72);
        assert!(t.crest.0 >= t.card.0 + 12 + 72);
    }

    #[test]
    fn accepts_community_references() {
        let mut s = style();
        s.decoration = Some(DESIGN.into());
        s.nameplate = Some(DESIGN.into());
        s.effect = Some(DESIGN.into());
        s.profile_frame = Some(DESIGN.into());
        assert!(s.validate_style().is_ok());
        assert_eq!(
            super::community_item_ref("c:01M5TGZFE80SKMC8JVGJKBHS2A"),
            Some("01M5TGZFE80SKMC8JVGJKBHS2A")
        );
    }
}

/// The tag of a person's account that owns its username: the name alone
/// finds it and no tag is shown. Bots, and accounts from before unique
/// usernames, carry a four-digit tag instead.
pub const UNIQUE_DISCRIMINATOR: &str = "0000";

/// What people type to find an account: `name`, or `name#1234` for an
/// account that carries a number tag
pub fn user_handle(username: &str, discriminator: &str) -> String {
    if discriminator == UNIQUE_DISCRIMINATOR || discriminator.is_empty() {
        username.to_string()
    } else {
        format!("{username}#{discriminator}")
    }
}

#[cfg(test)]
mod handle_tests {
    use super::{user_handle, UNIQUE_DISCRIMINATOR};

    #[test]
    fn handle_shows_a_tag_only_when_there_is_one() {
        assert_eq!("layla", user_handle("layla", UNIQUE_DISCRIMINATOR));
        assert_eq!("omar#1151", user_handle("omar", "1151"));
        assert_eq!("omar", user_handle("omar", ""));
    }
}
