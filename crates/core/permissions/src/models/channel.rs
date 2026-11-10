use once_cell::sync::Lazy;
use std::{fmt, ops::Add};

/// Abstract channel type
pub enum ChannelType {
    SavedMessages,
    DirectMessage,
    Group,
    ServerChannel,
    Unknown,
}

/// Permission value on Revolt
///
/// This should be restricted to the lower 52 bits to prevent any
/// potential issues with Javascript. Also leave empty spaces for
/// future permission flags to be added.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "try-from-primitive", derive(num_enum::TryFromPrimitive))]
#[repr(u64)]
pub enum ChannelPermission {
    // * Generic permissions
    /// Manage the channel or channels on the server
    ManageChannel = 1 << 0,
    /// Manage the server
    ManageServer = 1 << 1,
    /// Manage permissions on servers or channels
    ManagePermissions = 1 << 2,
    /// Manage roles on server
    ManageRole = 1 << 3,
    /// Manage server customisation (includes emoji)
    ManageCustomisation = 1 << 4,
    /// Allow external emoji usage
    UseExternalEmojis = 1 << 41,

    // % 1 bit reserved

    // * Member permissions
    /// Kick other members below their ranking
    KickMembers = 1 << 6,
    /// Ban other members below their ranking
    BanMembers = 1 << 7,
    /// Timeout other members below their ranking
    TimeoutMembers = 1 << 8,
    /// Assign roles to members below their ranking
    AssignRoles = 1 << 9,
    /// Change own nickname
    ChangeNickname = 1 << 10,
    /// Change or remove other's nicknames below their ranking
    ManageNicknames = 1 << 11,
    /// Change own avatar
    ChangeAvatar = 1 << 12,
    /// Remove other's avatars below their ranking
    RemoveAvatars = 1 << 13,

    // % 7 bits reserved

    // * Channel permissions
    /// View a channel
    ViewChannel = 1 << 20,
    /// Read a channel's past message history
    ReadMessageHistory = 1 << 21,
    /// Send a message in a channel
    SendMessage = 1 << 22,
    /// Delete messages in a channel
    ManageMessages = 1 << 23,
    /// Manage webhook entries on a channel
    ManageWebhooks = 1 << 24,
    /// Create invites to this channel
    InviteOthers = 1 << 25,
    /// Send embedded content in this channel
    SendEmbeds = 1 << 26,
    /// Send attachments and media in this channel
    UploadFiles = 1 << 27,
    /// Masquerade messages using custom nickname and avatar
    Masquerade = 1 << 28,
    /// React to messages with emojis
    React = 1 << 29,
    /// Bypass slowmode
    BypassSlowmode = 1 << 39,

    // * Voice permissions
    /// Connect to a voice channel
    Connect = 1 << 30,
    /// Speak in a voice call
    Speak = 1 << 31,
    /// Share video in a voice call
    Video = 1 << 32,
    /// Mute other members with lower ranking in a voice call
    MuteMembers = 1 << 33,
    /// Deafen other members with lower ranking in a voice call
    DeafenMembers = 1 << 34,
    /// Move members between voice channels
    MoveMembers = 1 << 35,
    /// Listen to other users
    Listen = 1 << 36,

    // * Channel permissions two electric boogaloo
    /// Mention everyone and online members
    MentionEveryone = 1 << 37,
    /// Mention roles
    MentionRoles = 1 << 38,

    /// Access server audit logs
    ViewAuditLogs = 1 << 40,

    /// Approve or reject requests to join a server that needs approval
    /// (never in the default permissions; the owner grants it to roles)
    ManageJoinRequests = 1 << 42,

    // * Thread permissions, read on the parent channel. They come on top of
    // * Send Message: taking that away from a channel still closes its threads
    /// Start a thread in a text channel
    CreateThreads = 1 << 43,
    /// Write in threads and in forum posts (a forum post is a thread)
    SendMessageInThreads = 1 << 44,
    /// Start a post in a forum
    CreateForumPosts = 1 << 45,

    // * Administrator. Never a default; only someone who has it can give it,
    // * and role ranks still decide which roles and members they can change.
    /// Every right in every channel of the server, whatever its overrides say
    Administrator = 1 << 46,

    // * Soundboard
    /// Play the server's soundboard sounds in a voice call
    UseSoundboard = 1 << 47,
    /// Add, change and remove the server's soundboard sounds
    ManageSoundboard = 1 << 48,

    // * Misc. permissions
    // % Bits 49 to 52: free area
    // % Bits 53 to 64: do not use

    // * Grant all permissions
    /// Safely grant all permissions
    GrantAllSafe = 0x000F_FFFF_FFFF_FFFF,

    /// Grant all permissions
    GrantAll = u64::MAX,
}

impl fmt::Display for ChannelPermission {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl_op_ex!(+ |a: &ChannelPermission, b: &ChannelPermission| -> u64 { *a as u64 | *b as u64 });
impl_op_ex_commutative!(+ |a: &u64, b: &ChannelPermission| -> u64 { *a | *b as u64 });

/// The permissions that belong to threads and forums
pub static THREAD_PERMISSIONS: Lazy<u64> = Lazy::new(|| {
    ChannelPermission::CreateThreads
        + ChannelPermission::SendMessageInThreads
        + ChannelPermission::CreateForumPosts
});

pub static ALLOW_IN_TIMEOUT: Lazy<u64> =
    Lazy::new(|| ChannelPermission::ViewChannel + ChannelPermission::ReadMessageHistory);

pub static DEFAULT_PERMISSION_VIEW_ONLY: Lazy<u64> =
    Lazy::new(|| ChannelPermission::ViewChannel + ChannelPermission::ReadMessageHistory);

pub static DEFAULT_PERMISSION: Lazy<u64> = Lazy::new(|| {
    DEFAULT_PERMISSION_VIEW_ONLY.add(
        ChannelPermission::SendMessage
            + ChannelPermission::InviteOthers
            + ChannelPermission::SendEmbeds
            + ChannelPermission::UploadFiles
            + ChannelPermission::Connect
            + ChannelPermission::Speak
            + ChannelPermission::Listen
            + ChannelPermission::Video
    )
});

pub static DEFAULT_PERMISSION_SAVED_MESSAGES: u64 = ChannelPermission::GrantAllSafe as u64;

pub static DEFAULT_PERMISSION_DIRECT_MESSAGE: Lazy<u64> = Lazy::new(|| {
    DEFAULT_PERMISSION.add(ChannelPermission::ManageChannel + ChannelPermission::React + ChannelPermission::Masquerade)
});

pub static DEFAULT_PERMISSION_SERVER: Lazy<u64> = Lazy::new(|| {
    DEFAULT_PERMISSION.add(
        ChannelPermission::React
            + ChannelPermission::ChangeNickname
            + ChannelPermission::ChangeAvatar
            + ChannelPermission::UseExternalEmojis
            + ChannelPermission::UseSoundboard
            + *THREAD_PERMISSIONS,
    )
});

/// What a new webhook may do. UploadFiles is in: a webhook's files pass the
/// same checks as a member's, and a tool that sends a report as a file should
/// work without a separate setting. Mass mentions are not: a token can leak.
pub static DEFAULT_WEBHOOK_PERMISSIONS: Lazy<u64> = Lazy::new(|| {
    ChannelPermission::SendMessage
        + ChannelPermission::SendEmbeds
        + ChannelPermission::UploadFiles
        + ChannelPermission::Masquerade
        + ChannelPermission::React
        + ChannelPermission::SendMessageInThreads
        + ChannelPermission::CreateForumPosts
});
