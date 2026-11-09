use super::User;

auto_derived!(
    /// Bot
    #[derive(Default)]
    pub struct Bot {
        /// Bot Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,

        /// User Id of the bot owner
        #[cfg_attr(feature = "serde", serde(rename = "owner"))]
        pub owner_id: String,
        /// Token used to authenticate requests for this bot: only in the
        /// response to creating the bot or resetting its token, empty
        /// everywhere else (the server keeps a hash only)
        pub token: String,
        /// Whether the bot is public
        /// (may be invited by anyone)
        pub public: bool,
        /// The permissions the bot will ask to have upon being invited to a server,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_permissions: Option<i64>,

        /// Whether to enable analytics
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub analytics: bool,
        /// Whether this bot should be publicly discoverable
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub discoverable: bool,
        /// Reserved, not used for anything: bots set
        /// `interactions_endpoint` instead
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub interactions_url: String,
        /// URL for terms of service
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub terms_of_service_url: String,
        /// URL for privacy policy
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub privacy_policy_url: String,

        /// Enum of bot flags
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_zero_u32", default)
        )]
        pub flags: u32,

        /// Privileged gateway intents the owner has enabled
        /// (see [`GatewayIntents`]); a bot may only ask for these
        #[cfg_attr(feature = "serde", serde(default))]
        pub privileged_intents: i64,

        /// Privileged intents Trust & Safety approved; past 100 servers only
        /// these stay on
        #[cfg_attr(feature = "serde", serde(default))]
        pub approved_intents: i64,

        /// Checked HTTPS address that receives interactions instead of the
        /// gateway (`PUT /bots/{id}/interactions/endpoint`)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub interactions_endpoint: Option<String>,

        /// Generation of the signing secret, 0 for none yet
        #[cfg_attr(feature = "serde", serde(default))]
        pub interactions_secret_version: i32,

        /// Whether the bot's public page names who made it (off by default)
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub show_creator: bool,
    }

    /// Optional fields on bot object
    pub enum FieldsBot {
        Token,
        InteractionsURL,
        InteractionsEndpoint,
    }

    /// Flags that may be attributed to a bot
    #[repr(u32)]
    pub enum BotFlags {
        Verified = 1,
        Official = 2,
        /// Disabled by Trust & Safety: cannot sign in, not listed
        Suspended = 4,
    }

    /// Public Bot
    pub struct PublicBot {
        /// Bot Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,

        /// Bot Username
        pub username: String,
        /// Profile Avatar
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub avatar: String,
        /// Profile Description
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub description: String,
        /// URL for privacy policy
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub privacy_policy_url: String,
        /// URL for terms of service
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub terms_of_service_url: String,
        /// Permissions the bot will ask for
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub default_permissions: Option<i64>,
        /// Verified by Trust & Safety
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub verified: bool,
        /// Who made the bot, when its owner chose to show it
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub creator: Option<PublicBotCreator>,
        /// Servers the bot is in
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub server_count: Option<i64>,
    }

    /// The person who made a public bot, shown only when the owner chose so
    /// (never their id)
    pub struct PublicBotCreator {
        /// Username
        pub username: String,
        /// Display name
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub display_name: Option<String>,
    }

    /// Where a bot receives its interactions over HTTP
    pub struct DataInteractionsEndpoint {
        /// HTTPS address. It gets two test requests before it is saved.
        pub url: String,
    }

    /// A new signing secret; shown once
    pub struct InteractionsSecret {
        /// Secret for checking `X-Zrzur-Signature`
        pub secret: String,
        /// Its generation
        pub version: i32,
    }

    /// Bot Response
    pub struct FetchBotResponse {
        /// Bot object
        pub bot: Bot,
        /// User object
        pub user: User,
    }

    /// Bot Details
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataCreateBot {
        /// Bot username
        #[cfg_attr(
            feature = "validator",
            validate(length(min = 2, max = 32), regex = "super::RE_USERNAME")
        )]
        pub name: String,
    }

    /// New Bot Details
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataEditBot {
        /// Bot username
        #[cfg_attr(
            feature = "validator",
            validate(length(min = 2, max = 32), regex = "super::RE_USERNAME")
        )]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        /// Whether the bot can be added by anyone
        pub public: Option<bool>,
        /// The permissions the bot will ask to have upon being invited to a server,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_permissions: Option<i64>,

        /// Whether analytics should be gathered for this bot
        ///
        /// Must be enabled in order to show up on [Discover](/discover).
        pub analytics: Option<bool>,
        /// Interactions URL
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 2048)))]
        pub interactions_url: Option<String>,
        /// Privileged gateway intents to enable: any of
        /// `MESSAGE_CONTENT`, `SERVER_MEMBERS` and `PRESENCE`
        #[serde(skip_serializing_if = "Option::is_none")]
        pub privileged_intents: Option<i64>,
        /// Show who made the bot on its public page
        #[serde(skip_serializing_if = "Option::is_none")]
        pub show_creator: Option<bool>,
        /// Fields to remove from bot object
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsBot>,
    }

    /// Where we are inviting a bot to
    #[serde(untagged)]
    pub enum InviteBotDestination {
        /// Invite to a server
        Server {
            /// Server Id
            server: String,
        },
        /// Invite to a group
        Group {
            /// Group Id
            group: String,
        },
    }

    /// Owned Bots Response
    ///
    /// Both lists are sorted by their IDs.
    ///
    /// TODO: user should be in bot object
    pub struct OwnedBotsResponse {
        /// Bot objects
        pub bots: Vec<Bot>,
        /// User objects
        pub users: Vec<User>,
    }

    /// Bot with user response
    pub struct BotWithUserResponse {
        #[serde(flatten)]
        pub bot: Bot,
        pub user: User,
    }

    /// Ask for the verified mark
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataBotVerification {
        /// What the bot does, who uses it, how it handles data
        #[cfg_attr(feature = "validator", validate(length(min = 20, max = 1000)))]
        pub statement: String,
        /// Website or documentation (https)
        #[cfg_attr(feature = "validator", validate(length(max = 200)))]
        pub website: Option<String>,
        /// Where users get help (link or server invite)
        #[cfg_attr(feature = "validator", validate(length(max = 200)))]
        pub support: Option<String>,
        /// The bot's privacy policy (https, required)
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate(length(max = 200)))]
        pub privacy_policy_url: String,
        /// The bot's terms of service (https, required)
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate(length(max = 200)))]
        pub terms_of_service_url: String,
        /// Privileged gateway intents the bot needs (see [`GatewayIntents`]);
        /// approving the request approves them
        #[cfg_attr(feature = "serde", serde(default))]
        pub intents: Option<i64>,
    }

    /// A verification request as its owner sees it (no reviewer identity)
    pub struct BotVerificationRequest {
        #[serde(rename = "_id")]
        pub id: String,
        pub bot_id: String,
        pub statement: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub website: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub support: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub privacy_policy_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub terms_of_service_url: Option<String>,
        /// Privileged intents asked for
        #[serde(default)]
        pub intents: i64,
        /// Pending, Approved, Denied or Revoked
        pub status: String,
        pub created_at: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub decided_at: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub decision_note: Option<String>,
    }

    /// Verification state of a bot
    pub struct BotVerificationState {
        pub verified: bool,
        /// Latest request, if any
        #[serde(skip_serializing_if = "Option::is_none")]
        pub request: Option<BotVerificationRequest>,
        /// When the owner may apply (now if absent)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub can_apply_at: Option<i64>,
    }
);

/// Gateway intents: what a bot asks to receive when it connects.
///
/// The first six are free to use. The last three are privileged: the owner
/// enables them on the bot first, and asking for one that is not enabled
/// closes the connection.
pub struct GatewayIntents;

impl GatewayIntents {
    /// Servers, channels, roles and emojis
    pub const SERVERS: u64 = 1 << 0;
    /// Messages in server channels (content needs `MESSAGE_CONTENT`)
    pub const SERVER_MESSAGES: u64 = 1 << 1;
    /// Messages in direct messages and groups
    pub const DIRECT_MESSAGES: u64 = 1 << 2;
    /// Reactions being added and removed
    pub const REACTIONS: u64 = 1 << 3;
    /// People joining, leaving and moving between voice channels
    pub const VOICE_STATES: u64 = 1 << 4;
    /// Commands, buttons, menus and forms used with the bot
    pub const INTERACTIONS: u64 = 1 << 5;
    /// Privileged: text, embeds, files and components of messages
    pub const MESSAGE_CONTENT: u64 = 1 << 10;
    /// Privileged: members joining, leaving and changing, and the member list
    pub const SERVER_MEMBERS: u64 = 1 << 11;
    /// Privileged: who is online and their status
    pub const PRESENCE: u64 = 1 << 12;

    /// Every intent that needs no approval; the default when a bot sends none
    pub const DEFAULT: u64 = Self::SERVERS
        | Self::SERVER_MESSAGES
        | Self::DIRECT_MESSAGES
        | Self::REACTIONS
        | Self::VOICE_STATES
        | Self::INTERACTIONS;
    /// The intents an owner must enable first
    pub const PRIVILEGED: u64 = Self::MESSAGE_CONTENT | Self::SERVER_MEMBERS | Self::PRESENCE;
    /// Every known intent
    pub const ALL: u64 = Self::DEFAULT | Self::PRIVILEGED;

    /// Intents for a connection: the requested ones (or the default), as
    /// long as every privileged one is enabled on the bot. Returns the names
    /// of the privileged intents that are not enabled otherwise.
    pub fn resolve(requested: Option<u64>, enabled: i64) -> Result<u64, Vec<&'static str>> {
        let requested = requested.unwrap_or(Self::DEFAULT) & Self::ALL;
        let missing = requested & Self::PRIVILEGED & !(enabled as u64);
        if missing == 0 {
            return Ok(requested);
        }
        Err(Self::privileged_names(missing))
    }

    /// Names of the privileged intents among `bits`
    pub fn privileged_names(bits: u64) -> Vec<&'static str> {
        [
            (Self::MESSAGE_CONTENT, "MESSAGE_CONTENT"),
            (Self::SERVER_MEMBERS, "SERVER_MEMBERS"),
            (Self::PRESENCE, "PRESENCE"),
        ]
        .into_iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| name)
        .collect()
    }
}

#[cfg(test)]
mod intent_tests {
    use super::GatewayIntents as I;

    #[test]
    fn default_has_no_privileged_intent() {
        assert_eq!(I::DEFAULT & I::PRIVILEGED, 0);
        assert_eq!(I::resolve(None, 0), Ok(I::DEFAULT));
    }

    #[test]
    fn privileged_intents_need_to_be_enabled() {
        let wants = I::DEFAULT | I::MESSAGE_CONTENT | I::PRESENCE;
        assert_eq!(I::resolve(Some(wants), 0), Err(vec!["MESSAGE_CONTENT", "PRESENCE"]));
        assert_eq!(I::resolve(Some(wants), I::MESSAGE_CONTENT as i64), Err(vec!["PRESENCE"]));
        assert_eq!(I::resolve(Some(wants), I::PRIVILEGED as i64), Ok(wants));
        // unknown bits are ignored
        assert_eq!(I::resolve(Some(1 << 40 | I::SERVERS), 0), Ok(I::SERVERS));
    }
}
