use super::{Channel, File, LegacyServerChannelType, ServerSupercharge, RE_COLOUR};
use iso8601_timestamp::Timestamp;

use revolt_permissions::{Override, OverrideField};
use std::collections::HashMap;

#[cfg(feature = "validator")]
use validator::Validate;

#[cfg(feature = "rocket")]
use rocket::FromForm;

auto_derived_partial!(
    /// Server
    pub struct Server {
        /// Unique Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// User id of the owner
        pub owner: String,

        /// Name of the server
        pub name: String,
        /// Description for the server
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub description: Option<String>,

        /// Channels within this server
        // TODO: investigate if this is redundant and can be removed
        pub channels: Vec<String>,
        /// Categories for this server
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub categories: Option<Vec<Category>>,
        /// Configuration for sending system event messages
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub system_messages: Option<SystemMessageChannels>,

        /// Roles for this server
        #[cfg_attr(
            feature = "serde",
            serde(
                default = "HashMap::<String, Role>::new",
                skip_serializing_if = "HashMap::<String, Role>::is_empty"
            )
        )]
        pub roles: HashMap<String, Role>,
        /// Default set of server and channel permissions
        pub default_permissions: i64,

        /// Icon attachment
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub icon: Option<File>,
        /// Banner attachment
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub banner: Option<File>,

        /// Bitfield of server flags: 1 official (run by the Zrzur team),
        /// 2 verified, 4 partner (set by platform staff only)
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_zero_u32", default)
        )]
        pub flags: u32,

        /// Whether this server is flagged as not safe for work
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub nsfw: bool,
        /// Whether to enable analytics
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub analytics: bool,
        /// Whether this server should be publicly discoverable
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub discoverable: bool,

        /// Who can become a member (separate from Discover listing).
        /// Missing = `Open`, so existing servers keep their behaviour.
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub access: Option<ServerAccess>,

        /// What members without a role need before they can talk.
        /// Missing = `None`.
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub verification_level: Option<ServerVerificationLevel>,

        /// Every invite and Discover join is refused until then
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub invites_paused_until: Option<Timestamp>,
        /// Members who aren't friends can't DM each other until then
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub dms_paused_until: Option<Timestamp>,
        /// Rules screening: members who joined since then accept the rules
        /// (`rules_channel`) before they can talk
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub rules_screening_since: Option<Timestamp>,

        /// Supercharges on the server and the level they reach
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub supercharge: Option<ServerSupercharge>,

        /// Custom invite link (zrzur.cc/<code>), a Wings level perk
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub vanity_invite: Option<ServerVanityInvite>,

        /// Channel that holds the server rules
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub rules_channel: Option<String>,
        /// Live counters shown at the top of the channel list, in order
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub stats: Option<Vec<ServerStat>>,
        /// Welcome page shown before the channels
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub lobby: Option<ServerLobby>,
        /// Tickets: members open a private channel with the server's staff
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub tickets: Option<ServerTickets>,

        /// Threads are switched off on this server: nothing new, the
        /// threads it has stay readable. Missing = on
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub threads_disabled: bool,
        /// Forums are switched off on this server: nothing new, the forums
        /// and posts it has stay readable. Missing = on
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub forums_disabled: bool,
        /// Scheduled events are switched on in this server (missing = off)
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub events_enabled: bool,

        /// Notifications for members who never chose a level for this
        /// server. Missing = `Mentions`.
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub default_notifications: Option<ServerNotificationLevel>,

        /// Approximate amount of members in the server
        pub approximate_member_count: usize,
    },
    "PartialServer"
);

auto_derived_partial!(
    /// Role
    pub struct Role {
        /// Unique Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Role name
        pub name: String,
        /// Permissions available to this role
        pub permissions: OverrideField,
        /// Colour used for this role
        ///
        /// This can be any valid CSS colour
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub colour: Option<String>,
        /// Whether this role should be shown separately on the member sidebar
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub hoist: bool,
        /// Ranking of this role
        #[cfg_attr(feature = "serde", serde(default))]
        pub rank: i64,
        /// Role icon
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub icon: Option<File>,
        /// Id of the bot that owns this role, if it is a managed role
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub owner: Option<String>
    },
    "PartialRole"
);

auto_derived!(
    /// Optional fields on server object
    pub enum FieldsServer {
        Description,
        Categories,
        SystemMessages,
        Icon,
        Banner,
        RulesChannel,
        RulesScreening,
        Stats,
        Lobby,
        VanityInvite,
        Tickets,
    }

    /// What a server does when someone joins. Bots are left out of both.
    #[derive(Default)]
    pub struct ServerOnJoin {
        /// Roles given to everyone who joins (at most five)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub roles: Vec<String>,
        /// A message the system posts when someone joins; missing = none
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub welcome: Option<ServerWelcomeMessage>,
    }

    /// The welcome message of a server
    pub struct ServerWelcomeMessage {
        /// Text channel of the server it is posted in
        pub channel: String,
        /// The text: `{member}` mentions the person, `{server}` is the
        /// server's name, a channel is written `<#id>`
        pub content: String,
    }

    /// # On-Join Settings
    pub struct DataEditServerOnJoin {
        /// Roles given to everyone who joins; replaces the list
        #[cfg_attr(feature = "serde", serde(default))]
        pub roles: Vec<String>,
        /// The welcome message; leave out to switch it off
        #[cfg_attr(feature = "serde", serde(default))]
        pub welcome: Option<ServerWelcomeMessage>,
        /// Needed when one of the roles can manage the server or act on
        /// members
        #[cfg_attr(feature = "serde", serde(default))]
        pub confirm_powerful_roles: bool,
    }

    /// Tickets of a server: a member opens a private channel that only they
    /// and the ticket staff can see
    #[derive(Default)]
    pub struct ServerTickets {
        /// Members can open tickets
        #[cfg_attr(feature = "serde", serde(default))]
        pub enabled: bool,
        /// Category new tickets are put in (none: outside the categories)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub category: Option<String>,
        /// Roles that see and answer tickets, besides the people who can
        /// already see every channel (at most 10)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub staff_roles: Vec<String>,
        /// First message of every new ticket (at most 500 characters)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub message: Option<String>,
        /// Name of the "Open a ticket" entry members see, when the server
        /// wants its own (at most 32 characters)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub label: Option<String>,
        /// Tickets opened so far. Set by the server only
        #[cfg_attr(feature = "serde", serde(default))]
        pub opened: u32,
        /// Kinds of ticket a member chooses from (at most 5; none: one kind)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub types: Vec<TicketType>,
    }

    /// A kind of ticket
    #[derive(Default)]
    pub struct TicketType {
        /// Unique id within the server (1-32 characters)
        pub id: String,
        /// Shown name (1-32 characters)
        pub name: String,
        /// Roles that answer this kind; none: the server's ticket roles
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub staff_roles: Vec<String>,
    }

    /// A server's welcome page: what the server is about, where to start,
    /// common questions and who runs it. Every text is shown as plain text
    /// or message Markdown.
    #[derive(Default)]
    pub struct ServerLobby {
        /// Show the lobby as the server's first page
        #[cfg_attr(feature = "serde", serde(default))]
        pub enabled: bool,
        /// One line under the server name
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub tagline: Option<String>,
        /// What the server is about (Markdown)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub about: Option<String>,
        /// Colour of headings and buttons, `#RRGGBB`
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub accent: Option<String>,
        /// Bounded visual settings; never arbitrary CSS.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub appearance: Option<LobbyAppearance>,
        /// Distinguish hiding all sections from the legacy default order.
        #[cfg_attr(feature = "serde", serde(default))]
        pub hide_sections: bool,
        /// Public interest questions that grant explicitly configured roles.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub onboarding: Vec<LobbyRoleQuestion>,
        /// Up to three buttons that open a channel
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub actions: Vec<LobbyAction>,
        /// Channels to point new members to, in order; empty picks the busiest
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub channels: Vec<String>,
        /// Common questions
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub faq: Vec<LobbyQuestion>,
        /// Roles listed under "Who runs it", highest first
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub staff_roles: Vec<String>,
        /// Sections shown, in order; empty shows every section in the default order
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub sections: Vec<LobbySection>,
        /// Channel whose newest message is shown as the latest news;
        /// none picks the first announcement channel
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub news_channel: Option<String>,
        /// Where new members introduce themselves ("Say hello" step)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub intro_channel: Option<String>,
    }

    /// A part of the lobby page
    #[derive(Copy, Hash)]
    pub enum LobbySection {
        /// Steps for new members: rules, hello, voice, profile
        Start,
        /// The newest announcement
        News,
        /// The managers' welcome text
        About,
        /// Voice rooms with people in them
        Live,
        /// Main channels
        Channels,
        /// Questions and answers
        Questions,
        /// Members who joined this week
        Newcomers,
        /// Who runs the server
        Team,
    }

    /// A lobby button that opens a channel of the server
    pub struct LobbyAction {
        pub label: String,
        pub channel: String,
        /// Unicode emoji shown before the label
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub emoji: Option<String>,
    }

    /// A question and its answer (Markdown)
    pub struct LobbyQuestion {
        pub question: String,
        pub answer: String,
    }

    /// Server-specific lobby appearance. Missing values inherit the app theme.
    #[derive(Default)]
    #[cfg_attr(feature = "serde", serde(default))]
    pub struct LobbyAppearance {
        pub secondary: Option<String>,
        pub background: Option<String>,
        pub surface: Option<String>,
        pub text: Option<String>,
        pub banner_height: Option<u16>,
        pub radius: Option<u16>,
        pub blend: Option<u16>,
        pub angle: Option<u16>,
        pub layout: Option<LobbyLayout>,
        pub density: Option<LobbyDensity>,
        pub banner_mode: Option<LobbyBannerMode>,
        pub show_greeting: Option<bool>,
        pub show_member_count: Option<bool>,
        pub show_chat: Option<bool>,
        pub show_invite: Option<bool>,
        pub channel_limit: Option<u16>,
        pub newcomer_limit: Option<u16>,
    }

    pub enum LobbyLayout { Split, Single }
    pub enum LobbyDensity { Comfortable, Compact }
    pub enum LobbyBannerMode { Server, Gradient, Hidden }

    /// Role choices are public opt-ins, not verification or moderation gates.
    pub struct LobbyRoleQuestion {
        pub id: String,
        pub question: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub description: Option<String>,
        pub options: Vec<LobbyRoleOption>,
    }

    pub struct LobbyRoleOption {
        pub id: String,
        pub label: String,
        pub role: String,
    }

    /// A server's custom invite link. A server that stops meeting the rule
    /// (official, partner, or enough members and activity) keeps the link for
    /// 30 days, then the name is released. A server that held it by its Wings
    /// level keeps the older timeline: the link works for 7 days after the
    /// drop, then pauses; the name stays reserved for 90 days.
    pub struct ServerVanityInvite {
        /// Lowercase name after zrzur.cc/
        pub code: String,
        /// Channel new members land in
        pub channel: String,
        pub claimed_at: Timestamp,
        /// When the server stopped meeting the rule (none while it meets it)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub below_level_since: Option<Timestamp>,
        /// The way the server last met the rule; decides the timeline after a
        /// drop (none on links from before the rule: the 30-day one)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub via: Option<VanityPath>,
        /// When the owner was told the link will end (none until then)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub notice_sent_at: Option<Timestamp>,
    }

    /// How a server may hold a custom invite link
    #[derive(Copy)]
    pub enum VanityPath {
        /// Run by the Zrzur team (server flag 1)
        Official,
        /// A Zrzur partner (server flag 4)
        Partner,
        /// Enough members and an active community
        Community,
        /// The Wings level that unlocks it (only while billing is on)
        Wings,
    }

    /// What a server lacks for a custom invite link
    #[derive(Copy)]
    pub enum VanityMissing {
        Members,
        Activity,
        Both,
    }

    /// Whether a server may hold a custom invite link, and the numbers behind it
    pub struct VanityEligibility {
        /// The server may claim or keep a custom link
        pub eligible: bool,
        /// The way it qualifies (none while not eligible)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub via: Option<VanityPath>,
        /// What it lacks (none while eligible)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub missing: Option<VanityMissing>,
        /// Members now, and the members needed
        pub members: usize,
        pub min_members: usize,
        /// Messages from members (no bots, webhooks or system messages) in
        /// the last `days` days, and the messages needed
        pub messages: usize,
        pub min_messages: usize,
        /// Different members who wrote them, and the number needed
        pub authors: usize,
        pub min_authors: usize,
        pub days: u32,
        /// When the server's current link stops working, if it is counting
        /// down (the 30-day notice, or the Wings grace)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub link_ends_at: Option<Timestamp>,
    }

    /// Claim or change a custom invite link
    pub struct DataVanityInvite {
        pub code: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub channel: Option<String>,
    }

    /// The server resolves role IDs from this choice; clients cannot supply roles.
    pub struct DataLobbyAnswer {
        pub question: String,
        pub option: String,
    }

    /// A live counter shown at the top of a server's channel list
    #[derive(Copy, Hash)]
    pub enum ServerStat {
        /// Members of the server
        Members,
        /// Channels in the server
        Channels,
        /// Roles in the server
        Roles,
        /// Wings given to the server
        Wings,
    }

    /// Optional fields on server object
    pub enum FieldsRole {
        Colour,
        Icon,
    }

    /// Channel category
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct Category {
        /// Unique ID for this category
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub id: String,
        /// Title for this category
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub title: String,
        /// Channels in this category
        pub channels: Vec<String>,
        /// A short note about this category. Set through the category's own
        /// edit route; a category list sent through server edit keeps the
        /// stored note
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 120)))]
        pub description: Option<String>,
        /// Default permissions for the channels synced with this category
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub default_permissions: Option<OverrideField>,
        /// Role permissions for the channels synced with this category
        #[cfg_attr(
            feature = "serde",
            serde(
                default = "HashMap::<String, OverrideField>::new",
                skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
            )
        )]
        pub role_permissions: HashMap<String, OverrideField>,
    }

    /// System message channel assignments
    pub struct SystemMessageChannels {
        /// ID of channel to send user join messages in
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub user_joined: Option<String>,
        /// ID of channel to send user left messages in
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub user_left: Option<String>,
        /// ID of channel to send user kicked messages in
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub user_kicked: Option<String>,
        /// ID of channel to send user banned messages in
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub user_banned: Option<String>,
    }

    /// Information about new server to create
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateServer {
        /// Server name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Server description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        pub description: Option<String>,
        /// Whether this server is age-restricted
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub nsfw: Option<bool>,
        /// Who can become a member (default `Open`); never lists on Discover
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub access: Option<ServerAccess>,
        /// What to build into the server in this one request. The server
        /// starts complete, or the request fails and nothing is left behind.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        #[cfg_attr(feature = "validator", validate)]
        pub blueprint: Option<DataServerBlueprint>,
    }

    /// Everything a new server starts with. Channels without a category come
    /// first in the server, then each category with its channels, in the
    /// order given. No channels at all: the server gets the default one.
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataServerBlueprint {
        /// Id of an uploaded icon (tag `icons`)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub icon: Option<String>,
        /// Channels outside any category
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate)]
        pub channels: Vec<BlueprintChannel>,
        /// Categories with their channels
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate(length(max = 20)))]
        #[cfg_attr(feature = "validator", validate)]
        pub categories: Vec<BlueprintCategory>,
        /// Roles, highest first. They can't hold server-wide control.
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate)]
        pub roles: Vec<BlueprintRole>,
        /// `false`: members can't create invites, only roles allowed to
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub member_invites: Option<bool>,
    }

    /// A category of a new server
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct BlueprintCategory {
        /// Category title
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub title: String,
        /// Channels in this category
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate)]
        pub channels: Vec<BlueprintChannel>,
    }

    /// A channel of a new server
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct BlueprintChannel {
        /// Channel type: `Text` or `Voice`
        #[serde(rename = "type", default = "LegacyServerChannelType::default")]
        pub channel_type: LegacyServerChannelType,
        /// Channel name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Everyone reads; only roles with `posts_in_read_only` write. The
        /// channel is closed from the moment it exists.
        #[cfg_attr(feature = "serde", serde(default))]
        pub read_only: bool,
        /// This is the server's rules channel (one text channel at most)
        #[cfg_attr(feature = "serde", serde(default))]
        pub rules: bool,
    }

    /// A role of a new server
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct BlueprintRole {
        /// Role name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Role colour
        #[cfg_attr(
            feature = "validator",
            validate(length(min = 1, max = 128), regex = "RE_COLOUR")
        )]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub colour: Option<String>,
        /// Whether this role should be displayed separately
        #[cfg_attr(feature = "serde", serde(default))]
        pub hoist: bool,
        /// Permissions the role allows across the server
        pub allow: u64,
        /// This role writes in the read-only channels
        #[cfg_attr(feature = "serde", serde(default))]
        pub posts_in_read_only: bool,
    }

    /// Who can become a member of a server. Discover listing is a separate,
    /// explicit setting (`discoverable`) and is only allowed for `Open`.
    #[derive(Default, Copy)]
    pub enum ServerAccess {
        /// Anyone with a join link or invite joins right away
        #[default]
        Open,
        /// Join links create a request; someone with Manage Join Requests
        /// approves it before the person becomes a member
        Approval,
        /// No requests and no vanity-link joins: only invites made by
        /// people allowed to invite let someone in
        Locked,
    }

    /// Pause invites and / or DMs between members who aren't friends, for
    /// 1, 6, 12 or 24 hours; 0 ends a pause, a missing field leaves it as is
    pub struct DataServerPause {
        #[cfg_attr(feature = "serde", serde(default))]
        pub invites_hours: Option<u8>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub dms_hours: Option<u8>,
    }

    /// What members without a role need before they can send messages,
    /// react or speak (members with any role, bots and the owner are exempt)
    #[derive(Default, Copy)]
    pub enum ServerVerificationLevel {
        /// Nothing
        #[default]
        None,
        /// A verified email address
        Low,
        /// Low, and an account older than 5 minutes
        Medium,
        /// Medium, and a member of the server for 10 minutes
        High,
    }

    /// What a member who never chose a level for a server is notified about
    #[derive(Default, Copy)]
    pub enum ServerNotificationLevel {
        /// Every message
        All,
        /// Only messages that mention them
        #[default]
        Mentions,
    }

    /// Where a join request stands
    pub enum JoinRequestStatus {
        Pending,
        Approved,
        Rejected,
    }

    /// A request to join an `Approval` server
    pub struct ServerJoinRequest {
        /// Server id
        pub server: String,
        /// Applicant user id
        pub user: String,
        /// Current state
        pub status: JoinRequestStatus,
        /// When the request was made (or last renewed)
        pub created_at: Timestamp,
        /// Who approved or rejected it
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub reviewed_by: Option<String>,
        /// When it was approved or rejected
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub reviewed_at: Option<Timestamp>,
    }

    /// Pending join requests of a server, oldest first, with their users
    pub struct JoinRequestList {
        /// The requests
        pub requests: Vec<ServerJoinRequest>,
        /// Public profile of each applicant (name and avatar only)
        pub users: Vec<super::BannedUser>,
    }

    /// Information about new role to create
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateRole {
        /// Role name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Ranking position
        ///
        /// Smaller values take priority.
        ///
        /// **Removed** - no effect, use the edit server role positions route
        pub rank: Option<i64>,
    }

    /// Response after creating new role
    // TODO: remove this in favor of just Role
    pub struct NewRoleResponse {
        /// Id of the role
        pub id: String,
        /// New role
        pub role: Role,
    }

    /// Information returned when creating server
    pub struct CreateServerLegacyResponse {
        /// Server object
        pub server: Server,
        /// Default channels
        pub channels: Vec<Channel>,
    }

    /// A whole server layout, built in one request: wider than a blueprint
    /// (topics, own permissions per channel and category). Channels without
    /// a category come first, then each category, in the order given. Roles
    /// and channels are named by a `key` that only lives in this document.
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataServerStructure {
        /// Server description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub description: Option<String>,
        /// What every member may do (missing: the usual default)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_permissions: Option<u64>,
        /// Channels outside any category
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate)]
        pub channels: Vec<StructureChannel>,
        /// Categories with their channels
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate)]
        pub categories: Vec<StructureCategory>,
        /// Roles, highest first. They can't hold control of the server.
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate)]
        pub roles: Vec<StructureRole>,
        /// `key` of the channel that shows who joined
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub system_channel: Option<String>,
    }

    /// A category of a server layout
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct StructureCategory {
        /// Category title
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub title: String,
        /// Channels in this category
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate)]
        pub channels: Vec<StructureChannel>,
        /// What every member may do in the channels that follow the category
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_permissions: Option<Override>,
        /// The same per role, by role `key`
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "HashMap::is_empty"))]
        pub role_permissions: HashMap<String, Override>,
    }

    /// A channel of a server layout
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct StructureChannel {
        /// Name of this channel inside the layout
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub key: String,
        /// Channel type
        #[serde(rename = "type", default = "LegacyServerChannelType::default")]
        pub channel_type: LegacyServerChannelType,
        /// Channel name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Channel description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub description: Option<String>,
        /// Age-restricted
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "crate::if_false"))]
        pub nsfw: bool,
        /// Announcement channel (text only)
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "crate::if_false"))]
        pub announcement: bool,
        /// Slow mode delay in seconds, up to 6 hours
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 21600)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub slowmode: Option<u64>,
        /// Most people in the voice channel at once
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 99)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub max_users: Option<usize>,
        /// Follows its category's permissions; its own are then ignored
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "crate::if_false"))]
        pub synced: bool,
        /// What every member may do here
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_permissions: Option<Override>,
        /// The same per role, by role `key`
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "HashMap::is_empty"))]
        pub role_permissions: HashMap<String, Override>,
    }

    /// A role of a server layout
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct StructureRole {
        /// Name of this role inside the layout
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub key: String,
        /// Role name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Role colour
        #[cfg_attr(
            feature = "validator",
            validate(length(min = 1, max = 128), regex = "RE_COLOUR")
        )]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub colour: Option<String>,
        /// Whether this role should be displayed separately
        #[cfg_attr(feature = "serde", serde(default))]
        pub hoist: bool,
        /// Permissions the role allows across the server
        #[cfg_attr(feature = "serde", serde(default))]
        pub allow: u64,
    }

    /// Where a server layout is read from: a template link, or the
    /// template's text as the owner copied it. One of the two.
    #[derive(Default)]
    pub struct DataImportPreview {
        /// Server template link, or its code
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub link: Option<String>,
        /// The template document itself
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub text: Option<String>,
    }

    /// What was left out of a layout while reading it, for the preview
    #[derive(Default)]
    pub struct ImportReport {
        /// Categories, channels and roles the layout holds
        pub categories: usize,
        pub channels: usize,
        pub roles: usize,
        /// Roles that held control of the server at the source (it is not
        /// given here; the owner grants it afterwards)
        pub held_back: Vec<String>,
        /// Names that were too long and are cut
        pub shortened: Vec<String>,
        /// Channels of a kind that can't be built, by name
        pub skipped_channels: Vec<String>,
        /// Channels and roles beyond this server's limits
        pub over_limit_channels: usize,
        pub over_limit_roles: usize,
        /// Source permissions with no match here, by their source name
        pub unmatched_permissions: Vec<String>,
        /// Permission settings for single members (members do not move)
        pub member_overrides: usize,
        /// Channels nobody but chosen roles can see
        pub private_channels: usize,
        /// Age-restricted channels
        pub mature_channels: usize,
    }

    /// A layout as read, with the name the source gave the server
    pub struct ImportPreviewResponse {
        /// Suggested server name
        pub name: String,
        /// The layout that `POST /servers/import` builds
        pub structure: DataServerStructure,
        /// What was left out
        pub report: ImportReport,
    }

    /// Build a server from a layout
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataImportServer {
        /// Server name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Id of an uploaded icon (tag `icons`)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub icon: Option<String>,
        /// Who can become a member (default `Open`)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub access: Option<ServerAccess>,
        /// The layout
        #[cfg_attr(feature = "validator", validate)]
        pub structure: DataServerStructure,
        /// Build into this empty server of the caller instead of a new one
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub into: Option<String>,
    }

    /// Options when fetching server
    #[cfg_attr(feature = "rocket", derive(FromForm))]
    pub struct OptionsFetchServer {
        /// Whether to include channels
        pub include_channels: Option<bool>,
    }

    /// Fetch server information
    #[serde(untagged)]
    pub enum FetchServerResponse {
        JustServer(Server),
        ServerWithChannels {
            #[serde(flatten)]
            server: Server,
            channels: Vec<Channel>,
        },
    }

    /// New category information
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataEditCategory {
        /// Title for this category
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub title: Option<String>,
        /// A short note about this category; an empty one removes it
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 120)))]
        pub description: Option<String>,
    }

    /// New server information
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataEditServer {
        /// Server name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: Option<String>,
        /// Server description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        pub description: Option<String>,

        /// Attachment Id for icon
        pub icon: Option<String>,
        /// Attachment Id for banner
        pub banner: Option<String>,

        /// Category structure for server
        #[cfg_attr(feature = "validator", validate)]
        pub categories: Option<Vec<Category>>,
        /// System message configuration
        pub system_messages: Option<SystemMessageChannels>,

        /// Bitfield of server flags
        #[cfg_attr(feature = "validator", serde(skip_serializing_if = "Option::is_none"))]
        pub flags: Option<i32>,

        // Whether this server is age-restricted
        // nsfw: Option<bool>,
        /// Whether this server is public and should show up on [Discover](/discover)
        pub discoverable: Option<bool>,
        /// Who can become a member; anything but `Open` also unlists the
        /// server from Discover
        pub access: Option<ServerAccess>,
        /// What members without a role need before they can talk
        pub verification_level: Option<ServerVerificationLevel>,
        /// New members must accept the rules before they can talk
        /// (needs a rules channel)
        pub rules_screening: Option<bool>,
        /// Whether analytics should be collected for this server
        ///
        /// Must be enabled in order to show up on [Discover](/discover).
        pub analytics: Option<bool>,

        /// User id of the new owner
        pub owner: Option<String>,

        /// Channel that holds the server rules (a text channel of this server)
        pub rules_channel: Option<String>,
        /// Live counters to show at the top of the channel list, in order
        #[cfg_attr(feature = "validator", validate(length(max = 4)))]
        pub stats: Option<Vec<ServerStat>>,
        /// Welcome page; replaces the whole lobby
        pub lobby: Option<ServerLobby>,
        /// Ticket settings; replaces them all (`opened` keeps its value)
        pub tickets: Option<ServerTickets>,
        /// Switch threads off (or back on) on this server
        pub threads_disabled: Option<bool>,
        /// Switch forums off (or back on) on this server
        pub forums_disabled: Option<bool>,
        /// Switch scheduled events on (or back off) in this server; needs
        /// Manage Server
        pub events_enabled: Option<bool>,
        /// Notifications for members who never chose a level for this server
        pub default_notifications: Option<ServerNotificationLevel>,

        /// Fields to remove from server object
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsServer>,
    }

    /// New role information
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataEditRole {
        /// Role name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: Option<String>,
        /// Role colour
        #[cfg_attr(
            feature = "validator",
            validate(length(min = 1, max = 128), regex = "RE_COLOUR")
        )]
        pub colour: Option<String>,
        /// Whether this role should be displayed separately
        pub hoist: Option<bool>,
        /// Ranking position
        ///
        /// **Removed** - no effect, use the edit server role positions route
        pub rank: Option<i64>,
        /// Role icon
        ///
        /// Provide an Autumn attachment Id.
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub icon: Option<String>,
        /// Fields to remove from role object
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsRole>,
    }

    /// New role permissions
    pub struct DataSetServerRolePermission {
        /// Allow / deny values for the role in this server.
        pub permissions: Override,
    }

    /// Options when leaving a server
    #[cfg_attr(feature = "rocket", derive(FromForm))]
    pub struct OptionsServerDelete {
        /// Whether to not send a leave message
        pub leave_silently: Option<bool>,
    }

    /// New role positions
    pub struct DataEditRoleRanks {
        pub ranks: Vec<String>,
    }
);

auto_derived!(
    /// An AutoMod rule: what to look for in messages, and what to do
    pub struct AutoModRule {
        /// Short id, unique within the server
        pub id: String,
        /// Name shown to moderators and, as the reason, to the member
        pub name: String,
        /// Switched on
        #[cfg_attr(feature = "serde", serde(default = "crate::if_true_default"))]
        pub enabled: bool,
        /// What the rule looks for
        pub trigger: AutoModTrigger,
        /// What happens when it matches
        pub actions: AutoModActions,
        /// Members with one of these roles are not checked
        #[cfg_attr(feature = "serde", serde(default))]
        pub exempt_roles: Vec<String>,
        /// Messages in these channels are not checked
        #[cfg_attr(feature = "serde", serde(default))]
        pub exempt_channels: Vec<String>,
    }

    /// What an AutoMod rule looks for
    #[serde(tag = "type")]
    pub enum AutoModTrigger {
        /// Words and phrases. `*` at the start or end matches part of a
        /// word; Arabic letters are compared without hamza forms, ta
        /// marbuta, alif maqsura, tashkeel and tatweel. `allow` lists words
        /// that never match.
        Keywords {
            words: Vec<String>,
            #[cfg_attr(feature = "serde", serde(default))]
            allow: Vec<String>,
        },
        /// More than `max` different mentions in one message
        MentionSpam { max: u32 },
        /// Links from members who joined less than `new_member_minutes`
        /// ago, and / or links to domains outside `allowed_domains`
        LinkSpam {
            #[cfg_attr(feature = "serde", serde(default))]
            new_member_minutes: Option<u32>,
            #[cfg_attr(feature = "serde", serde(default))]
            allowed_domains: Vec<String>,
        },
        /// The same text in `channels` channels within `seconds`
        RepeatedMessages { channels: u32, seconds: u32 },
    }

    /// What happens when an AutoMod rule matches
    #[derive(Default)]
    pub struct AutoModActions {
        /// Don't post the message (the member sees the rule's name)
        #[cfg_attr(feature = "serde", serde(default))]
        pub block: bool,
        /// Tell moderators in this channel
        #[cfg_attr(feature = "serde", serde(default))]
        pub alert_channel: Option<String>,
        /// Time the member out for this many minutes
        #[cfg_attr(feature = "serde", serde(default))]
        pub timeout_minutes: Option<u32>,
    }

    /// A server's AutoMod rules (replaces all of them)
    pub struct DataAutoModRules {
        pub rules: Vec<AutoModRule>,
    }
);
