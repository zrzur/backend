#![allow(deprecated)]
use super::{File, UserVoiceState};

use revolt_permissions::{Override, OverrideField};
use std::collections::{HashMap, HashSet};

#[cfg(feature = "rocket")]
use rocket::FromForm;
#[cfg(feature = "validator")]
#[allow(unused_imports)]
use validator::Validate;

auto_derived!(
    /// Channel
    #[serde(tag = "channel_type")]
    pub enum Channel {
        /// Personal "Saved Notes" channel which allows users to save messages
        SavedMessages {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,
            /// Id of the user this channel belongs to
            user: String,
        },
        /// Direct message channel between two users
        DirectMessage {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,

            /// Whether this direct message channel is currently open on both sides
            active: bool,
            /// 2-tuple of user ids participating in direct message
            recipients: Vec<String>,
            /// Id of the last message sent in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            last_message_id: Option<String>,
        },
        /// Group channel between 1 or more participants
        Group {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,

            /// Display name of the channel
            name: String,
            /// User id of the owner of the group
            owner: String,
            /// Channel description
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            description: Option<String>,
            /// Array of user ids participating in channel
            recipients: Vec<String>,

            /// Custom icon attachment
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            icon: Option<File>,
            /// Id of the last message sent in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            last_message_id: Option<String>,

            /// Permissions assigned to members of this group
            /// (does not apply to the owner of the group)
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            permissions: Option<i64>,

            /// Whether this group is marked as not safe for work
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            nsfw: bool,
        },
        /// Text channel belonging to a server
        TextChannel {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,
            /// Id of the server this channel belongs to
            server: String,

            /// Display name of the channel
            name: String,
            /// Channel description
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            description: Option<String>,

            /// Custom icon attachment
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            icon: Option<File>,
            /// Id of the last message sent in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            last_message_id: Option<String>,

            /// Default permissions assigned to users in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            default_permissions: Option<OverrideField>,
            /// Permissions assigned based on role to this channel
            #[cfg_attr(
                feature = "serde",
                serde(
                    default = "HashMap::<String, OverrideField>::new",
                    skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
                )
            )]
            role_permissions: HashMap<String, OverrideField>,
            /// Permissions for single members, applied after the roles
            #[cfg_attr(
                feature = "serde",
                serde(
                    default = "HashMap::<String, OverrideField>::new",
                    skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
                )
            )]
            member_permissions: HashMap<String, OverrideField>,

            /// Whether this channel is marked as not safe for work
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            nsfw: bool,

            /// Voice Information for when this channel is also a voice channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            voice: Option<VoiceInformation>,

            /// The channel's slowmode delay in seconds
            #[serde(skip_serializing_if = "Option::is_none")]
            slowmode: Option<u64>,

            /// Announcement channel: only people who can manage messages post
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            announcement: bool,

            /// Uses its category's default and role permissions instead of
            /// its own; per-member overrides still apply after them
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            permissions_synced: bool,

            /// The channel this thread belongs to (a thread uses its permissions)
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            parent: Option<String>,
            /// Thread state, when this channel is a thread
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            thread: Option<ThreadInformation>,
            /// Forum settings, when this channel is a forum (posts are threads)
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            forum: Option<ForumInformation>,
        },
    }

    /// State of a thread
    #[derive(Default)]
    pub struct ThreadInformation {
        /// Who started it
        pub owner: String,
        /// Message it was started from
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub starter_message: Option<String>,
        /// Hidden from the active list; a new message reopens it
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub archived: bool,
        /// Only people who can manage messages write in it
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub locked: bool,
        /// Forum tags of this post
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub tags: Vec<String>,
        /// Messages sent in it
        #[cfg_attr(feature = "serde", serde(default))]
        pub message_count: u32,
    }

    /// Settings of a forum channel
    #[derive(Default)]
    pub struct ForumInformation {
        /// Tags posts can carry
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub tags: Vec<ForumTag>,
        /// Every post needs at least one tag
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub require_tag: bool,
    }

    /// A forum tag
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct ForumTag {
        /// Unique id within the forum (1-32 characters)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub id: String,
        /// Shown name (1-20 characters)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 20)))]
        pub name: String,
        /// Unicode emoji or custom emoji id
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub emoji: Option<String>,
    }

    /// Start a thread
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataCreateThread {
        /// Thread name (1-100 characters)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 100)))]
        pub name: String,
        /// Message to start the thread from
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub message: Option<String>,
        /// First message of the thread (required in forums)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 2000)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub content: Option<String>,
        /// Forum tags
        #[cfg_attr(feature = "serde", serde(default))]
        pub tags: Vec<String>,
        /// Embeds of the first message. The first message is sent as by
        /// `POST /channels/{target}/messages` in the new thread: the same
        /// permissions (Send Embeds), the same mention rules (pinging a role
        /// needs Mention Roles), and if it is refused no thread is made.
        /// A forum post still needs `content`.
        #[cfg_attr(feature = "validator", validate)]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub embeds: Option<Vec<super::SendableEmbed>>,
        /// Buttons and selects of the first message (bots only, as on
        /// message send)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub components: Option<Vec<super::ComponentBlock>>,
    }

    /// Change a thread
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataEditThread {
        /// New name (1-100 characters)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 100)))]
        pub name: Option<String>,
        /// Hide it from the active list; a new message reopens it
        pub archived: Option<bool>,
        /// Only people who can manage messages write in it (moderators only)
        pub locked: Option<bool>,
        /// Forum post: its tags
        pub tags: Option<Vec<String>>,
    }

    /// Voice information for a channel
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct VoiceInformation {
        /// Maximium amount of users allowed in the voice channel at once
        #[cfg_attr(feature = "validator", validate(range(min = 1)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub max_users: Option<usize>,
    }

    /// Partial representation of a channel
    #[derive(Default)]
    pub struct PartialChannel {
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub name: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub owner: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub description: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub icon: Option<File>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub nsfw: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub active: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub permissions: Option<i64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub role_permissions: Option<HashMap<String, OverrideField>>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub member_permissions: Option<HashMap<String, OverrideField>>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_permissions: Option<OverrideField>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub last_message_id: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub voice: Option<VoiceInformation>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub slowmode: Option<u64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub announcement: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub permissions_synced: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub thread: Option<ThreadInformation>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub forum: Option<ForumInformation>,
    }

    /// Optional fields on channel object
    pub enum FieldsChannel {
        Description,
        Icon,
        DefaultPermissions,
        Voice,
        Slowmode,
    }

    /// New webhook information
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataEditChannel {
        /// Channel name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: Option<String>,

        /// Channel description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        pub description: Option<String>,

        /// Group owner
        pub owner: Option<String>,

        /// Icon
        ///
        /// Provide an Autumn attachment Id.
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub icon: Option<String>,

        /// Whether this channel is age-restricted
        pub nsfw: Option<bool>,

        /// Whether this channel is archived
        pub archived: Option<bool>,

        /// Voice Information for voice channels
        pub voice: Option<VoiceInformation>,

        /// The channel's slow mode delay in seconds, up to 6 hours
        #[cfg_attr(feature = "validator", validate(range(min = 0, max = 21600)))]
        pub slowmode: Option<u64>,

        /// Whether this is an announcement channel (only people who can
        /// manage messages post in it)
        pub announcement: Option<bool>,

        /// Forum: the tags posts can carry (at most 20)
        #[cfg_attr(feature = "validator", validate(length(max = 20)))]
        pub forum_tags: Option<Vec<ForumTag>>,

        /// Forum: every post needs at least one tag
        pub require_tag: Option<bool>,

        /// Fields to remove from channel
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsChannel>,
    }

    /// Create new group
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataCreateGroup {
        /// Group name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Group description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        pub description: Option<String>,
        /// Group icon
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub icon: Option<String>,
        /// Array of user IDs to add to the group
        ///
        /// Must be friends with these users.
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 49)))]
        #[serde(default)]
        pub users: HashSet<String>,
        /// Whether this group is age-restricted
        #[serde(skip_serializing_if = "Option::is_none")]
        pub nsfw: Option<bool>,
    }

    /// Server Channel Type
    #[derive(Default)]
    pub enum LegacyServerChannelType {
        /// Text Channel
        #[default]
        Text,
        /// Voice Channel
        Voice,
        /// Forum Channel: every post is a thread
        Forum,
    }

    /// Create new server channel
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataCreateServerChannel {
        /// Channel type
        #[serde(rename = "type", default = "LegacyServerChannelType::default")]
        pub channel_type: LegacyServerChannelType,
        /// Channel name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Channel description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        pub description: Option<String>,
        /// Whether this channel is age restricted
        #[serde(skip_serializing_if = "Option::is_none")]
        pub nsfw: Option<bool>,

        /// Voice Information for when this channel is also a voice channel
        #[serde(skip_serializing_if = "Option::is_none")]
        pub voice: Option<VoiceInformation>,

        /// Make it an announcement channel (text channels only)
        #[cfg_attr(feature = "serde", serde(default))]
        pub announcement: Option<bool>,

        /// Put the channel into this category of the server
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
        pub category: Option<String>,

        /// Private: hidden from @everyone; only the owner and the roles and
        /// members given access later see it
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
        pub private: Option<bool>,
    }

    /// New default permissions
    #[serde(untagged)]
    pub enum DataDefaultChannelPermissions {
        Value {
            /// Permission values to set for members in a `Group`
            permissions: u64,
        },
        Field {
            /// Allow / deny values to set for members in this server channel
            permissions: Override,
        },
    }

    /// New role permissions
    pub struct DataSetRolePermissions {
        /// Allow / deny values to set for this role
        pub permissions: Override,
    }

    /// Options when deleting a channel
    #[cfg_attr(feature = "rocket", derive(FromForm))]
    pub struct OptionsChannelDelete {
        /// Whether to not send a leave message
        pub leave_silently: Option<bool>,
    }

    /// Voice server token response
    pub struct CreateVoiceUserResponse {
        /// Token for authenticating with the voice server
        pub token: String,
        /// Url of the livekit server to connect to
        pub url: String,
    }

    /// Voice state for a channel
    pub struct ChannelVoiceState {
        pub id: String,
        /// The states of the users who are connected to the channel
        pub participants: Vec<UserVoiceState>,
    }

    /// Join a voice channel
    pub struct DataJoinCall {
        /// Name of the node to join
        pub node: Option<String>,
        /// Whether to force disconnect any other existing voice connections
        ///
        /// Useful for disconnecting on another device and joining on a new.
        pub force_disconnect: Option<bool>,
        /// Users which should be notified of the call starting
        ///
        /// Only used when the user is the first one connected.
        pub recipients: Option<Vec<String>>,
        /// LiveKit participant session this device had before its connection
        /// dropped. The join is refused (`AlreadyConnected`) when another
        /// session of the user has taken the seat since, so a device coming
        /// back online does not push out the newer one.
        pub previous_session: Option<String>,
    }

    pub struct ChannelSlowmode {
        pub channel_id: String,
        pub duration: u64,
        pub retry_after: u64,
    }
);

impl Channel {
    /// Get a reference to this channel's id
    pub fn id(&self) -> &str {
        match self {
            Channel::DirectMessage { id, .. }
            | Channel::Group { id, .. }
            | Channel::SavedMessages { id, .. }
            | Channel::TextChannel { id, .. } => id,
        }
    }

    /// This returns a Result because the recipient name can't be determined here without a db call,
    /// which can't be done since this is models, which can't reference the database crate.
    ///
    /// If it returns None, you need to fetch the name from the db.
    pub fn name(&self) -> Option<&str> {
        match self {
            Channel::DirectMessage { .. } => None,
            Channel::SavedMessages { .. } => Some("Saved Messages"),
            Channel::TextChannel { name, .. } | Channel::Group { name, .. } => Some(name),
        }
    }
}
