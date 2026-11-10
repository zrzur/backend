use std::collections::BTreeMap;

use iso8601_timestamp::Timestamp;
use once_cell::sync::Lazy;
use regex::Regex;

use super::File;

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
        /// 🦊🦝
        Paw = 128,
        /// Joined as one of the first 1000 users in 2021
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
        /// One year on Zrzur
        OneYear = 1048576,
        /// Three years on Zrzur
        ThreeYears = 2097152,
        /// Owns a server that reached 100 members
        CommunityBuilder = 4194304,
        /// Owns a server that reached 1000 members
        BigCommunity = 8388608,
        /// Owns a verified server
        VerifiedServerOwner = 16777216,
        /// Owns a verified bot
        VerifiedBotDeveloper = 33554432,
        /// Won a tournament, or first place in the games bot
        SahaChampion = 67108864,
        /// Helped people with support, chosen by staff
        CommunityHelper = 134217728,
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
        /// Name colour: one colour (solid) or two (gradient), #rrggbb
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Vec::is_empty", default)
        )]
        pub name_colour: Vec<String>,
        /// Avatar decoration: one of [USER_STYLE_DECORATIONS], or c:<id>
        /// for a community decoration
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub decoration: Option<String>,
        /// Image of a community decoration (set by the server, read-only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub decoration_file: Option<String>,
        /// Nameplate behind the name in member lists: one of
        /// [USER_STYLE_NAMEPLATES], or c:<id> for a community nameplate
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub nameplate: Option<String>,
        /// Image of a community nameplate (set by the server, read-only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub nameplate_file: Option<String>,
        /// Profile effect: one of [USER_STYLE_EFFECTS], or c:<id> for a
        /// community effect. It plays once when a profile opens; nobody
        /// records who saw it.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub effect: Option<String>,
        /// File of a community effect (set by the server, read-only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub effect_file: Option<String>,
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
        /// Id of the owner of this bot
        #[cfg_attr(feature = "serde", serde(rename = "owner"))]
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

/// Zrzur's own profile effects. Each is drawn by the client, plays once for
/// at most 4 seconds, and is flash-tested (see the web `effects` folder).
pub const USER_STYLE_EFFECTS: &[&str] = &[
    "snow", "rain", "confetti", "sparks", "petals", "bubbles", "leaves", "fireflies", "ripples",
    "kites", "aurora", "starfall", "lanterns", "sakura", "embers", "planes", "butterflies",
    "ink", "shards", "comet", "glass-rain", "dust", "sunrays", "moons", "bloom", "waves",
    "burst", "feathers", "dandelions", "hearts",
];

/// Built-in avatar decorations (drawn by the client)
pub const USER_STYLE_DECORATIONS: &[&str] = &[
    "laurel", "light-crown", "rings", "ivy", "thorn-roses", "ice-crystal", "flame-ring",
    "lightning", "moon-halo", "sun-rays", "star-orbit", "sakura-branch", "geometric-hex",
    "neon", "wings", "feathers", "gear", "circuit", "waves", "leaves", "snow", "hearts",
    "music-notes", "shield-frame",
];

/// Built-in nameplates (files in the client): countries, themes, zodiac signs
pub const USER_STYLE_NAMEPLATES: &[&str] = &[
    "flag-af", "flag-al", "flag-dz", "flag-ad", "flag-ao", "flag-ag", "flag-ar", "flag-am",
    "flag-au", "flag-at", "flag-az", "flag-bs", "flag-bh", "flag-bb", "flag-by",
    "flag-be", "flag-bz", "flag-bj", "flag-bt", "flag-bo", "flag-ba", "flag-bw", "flag-br",
    "flag-bn", "flag-bg", "flag-bf", "flag-bi", "flag-cv", "flag-kh", "flag-cm", "flag-ca",
    "flag-cf", "flag-td", "flag-cl", "flag-cn", "flag-co", "flag-km", "flag-cg", "flag-cd",
    "flag-cr", "flag-ci", "flag-hr", "flag-cu", "flag-cy", "flag-cz", "flag-dk", "flag-dj",
    "flag-dm", "flag-do", "flag-ec", "flag-eg", "flag-sv", "flag-gq", "flag-er", "flag-ee",
    "flag-sz", "flag-et", "flag-fj", "flag-fi", "flag-fr", "flag-ga", "flag-gm", "flag-ge",
    "flag-de", "flag-gh", "flag-gr", "flag-gd", "flag-gt", "flag-gn", "flag-gw", "flag-gy",
    "flag-ht", "flag-hn", "flag-hu", "flag-is", "flag-in", "flag-id", "flag-ir", "flag-iq",
    "flag-ie", "flag-it", "flag-jm", "flag-jo", "flag-kz", "flag-ke",
    "flag-ki", "flag-kw", "flag-kg", "flag-lv", "flag-lb",
    "flag-ls", "flag-lr", "flag-ly", "flag-li", "flag-lt", "flag-lu", "flag-mg", "flag-mw",
    "flag-my", "flag-mv", "flag-ml", "flag-mt", "flag-mh", "flag-mr", "flag-mu", "flag-mx",
    "flag-md", "flag-mc", "flag-mn", "flag-me", "flag-ma", "flag-mz", "flag-mm",
    "flag-na", "flag-nr", "flag-nl", "flag-nz", "flag-ni", "flag-ng",
    "flag-mk", "flag-no", "flag-om", "flag-pk", "flag-ps", "flag-pg",
    "flag-py", "flag-pe", "flag-ph", "flag-pl", "flag-pt", "flag-qa", "flag-ro", "flag-ru",
    "flag-rw", "flag-kn", "flag-lc", "flag-vc", "flag-ws", "flag-sm", "flag-st", "flag-sa",
    "flag-sn", "flag-rs", "flag-sc", "flag-sl", "flag-sg", "flag-sk", "flag-si", "flag-sb",
    "flag-so", "flag-za", "flag-ss", "flag-es", "flag-lk", "flag-sd", "flag-sr", "flag-se",
    "flag-sy", "flag-tj", "flag-tz", "flag-th", "flag-tl", "flag-tg", "flag-to",
    "flag-tt", "flag-tn", "flag-tr", "flag-tm", "flag-tv", "flag-ug", "flag-ua", "flag-ae",
    "flag-gb", "flag-us", "flag-uy", "flag-uz", "flag-vu", "flag-va", "flag-ve", "flag-vn",
    "flag-ye", "flag-zm", "flag-zw", "flag-gb-eng", "flag-gb-sct", "flag-kurdistan", "flag-bd",
    "flag-jp", "flag-la", "flag-ne", "flag-pw", "flag-kp", "flag-kr", "flag-ch", "flag-fm",
    "flag-np", "flag-pa", "aurora",
    "autumn", "sakura", "ocean", "clouds", "sunset", "midnight", "mint", "zodiac-aries",
    "zodiac-taurus", "zodiac-gemini", "zodiac-cancer", "zodiac-leo", "zodiac-virgo",
    "zodiac-libra", "zodiac-scorpio", "zodiac-sagittarius", "zodiac-capricorn",
    "zodiac-aquarius", "zodiac-pisces",
];

/// Decorations that are no longer offered. Saved profiles may still hold one:
/// clients show nothing for it, and a save that sends it back must not fail.
const USER_STYLE_RETIRED_DECORATIONS: &[&str] = &[
    "flames", "headphones", "stars", "glow", "fire", "ice", "gold", "sparkle", "galaxy",
    "heartbeat", "toxic",
];

/// Nameplates that are no longer offered, as above
const USER_STYLE_RETIRED_NAMEPLATES: &[&str] = &["galaxy", "ember"];

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
        if !colours_ok(&self.name_colour) {
            return Err("name_colour must be one or two #rrggbb colours");
        }
        if let Some(font) = &self.name_font {
            if !USER_STYLE_FONTS.contains(&font.as_str()) {
                return Err("unknown name_font");
            }
        }
        if let Some(decoration) = &self.decoration {
            if !USER_STYLE_DECORATIONS.contains(&decoration.as_str())
                && !USER_STYLE_RETIRED_DECORATIONS.contains(&decoration.as_str())
                && community_item_ref(decoration).is_none()
            {
                return Err("unknown decoration");
            }
        }
        if let Some(effect) = &self.effect {
            if !USER_STYLE_EFFECTS.contains(&effect.as_str())
                && community_item_ref(effect).is_none()
            {
                return Err("unknown effect");
            }
        }
        if let Some(nameplate) = &self.nameplate {
            if !USER_STYLE_NAMEPLATES.contains(&nameplate.as_str())
                && !USER_STYLE_RETIRED_NAMEPLATES.contains(&nameplate.as_str())
                && community_item_ref(nameplate).is_none()
            {
                return Err("unknown nameplate");
            }
        }
        Ok(())
    }

    /// Nothing chosen at all
    pub fn is_empty(&self) -> bool {
        self.theme.is_empty()
            && self.name_font.is_none()
            && self.name_colour.is_empty()
            && self.decoration.is_none()
            && self.nameplate.is_none()
            && self.effect.is_none()
    }
}

#[cfg(test)]
mod style_tests {
    use super::UserStyle;

    fn style() -> UserStyle {
        UserStyle {
            theme: vec!["#ff7a45".into(), "#1d1f2b".into()],
            name_font: Some("serif".into()),
            name_colour: vec!["#ffffff".into()],
            decoration: Some("lightning".into()),
            nameplate: Some("aurora".into()),
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
        s.name_colour = vec!["#000000".into(), "#111111".into(), "#222222".into()];
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
    fn a_saved_retired_design_does_not_block_a_save() {
        let mut s = style();
        s.decoration = Some("headphones".into());
        s.nameplate = Some("galaxy".into());
        assert!(s.validate_style().is_ok());
    }

    #[test]
    fn accepts_the_new_designs() {
        let mut s = style();
        s.decoration = Some("shield-frame".into());
        s.nameplate = Some("flag-sy".into());
        s.name_font = Some("kufi".into());
        s.effect = Some("glass-rain".into());
        assert!(s.validate_style().is_ok());
    }

    #[test]
    fn accepts_community_references() {
        let mut s = style();
        s.decoration = Some("c:01M5TGZFE80SKMC8JVGJKBHS2A".into());
        s.nameplate = Some("c:01M5TGZFE80SKMC8JVGJKBHS2A".into());
        assert!(s.validate_style().is_ok());
        assert_eq!(
            super::community_item_ref("c:01M5TGZFE80SKMC8JVGJKBHS2A"),
            Some("01M5TGZFE80SKMC8JVGJKBHS2A")
        );
    }
}

#[cfg(test)]
mod nameplate_list_tests {
    use super::{USER_STYLE_NAMEPLATES, USER_STYLE_RETIRED_NAMEPLATES};

    #[test]
    fn the_flags_the_app_offers_are_on_the_list() {
        for flag in [
            "flag-bd", "flag-jp", "flag-la", "flag-ne", "flag-pw", "flag-kp", "flag-kr", "flag-ch",
            "flag-fm", "flag-np", "flag-pa",
        ] {
            assert!(USER_STYLE_NAMEPLATES.contains(&flag), "{flag}");
            assert!(!USER_STYLE_RETIRED_NAMEPLATES.contains(&flag), "{flag}");
        }
    }

    #[test]
    fn a_removed_flag_is_on_neither_list() {
        assert!(!USER_STYLE_NAMEPLATES.contains(&"flag-il"));
        assert!(!USER_STYLE_RETIRED_NAMEPLATES.contains(&"flag-il"));
    }
}
