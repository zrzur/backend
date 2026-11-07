//! Zrzur developer platform: interactions.
//!
//! A user triggers an interaction (a command, a button click, a select, a
//! modal submit or an autocomplete request); the server checks it and sends
//! `InteractionCreate` to the target bot only, with a token valid for this
//! one interaction. The bot answers through the callback endpoint with that
//! token.

use std::collections::HashMap;

use iso8601_timestamp::Timestamp;

use super::{
    ApplicationCommandType, Channel, CommandOptionChoice, CommandOptionType, ComponentBlock,
    File, Member, Modal, ModalFieldValue, Role, SendableEmbed, User,
};

/// Same derives as `auto_derived!` without `Eq`: option values may be `f64`.
macro_rules! interaction_derived {
    ( $( $item:item )+ ) => {
        $(
            #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
            #[cfg_attr(feature = "schemas", derive(JsonSchema))]
            #[cfg_attr(feature = "utoipa", derive(ToSchema))]
            #[derive(Debug, Clone, PartialEq)]
            $item
        )+
    };
}

/// Interaction protocol version, sent with every interaction
pub const INTERACTION_VERSION: u32 = 1;

/// Limits and deadlines of the interaction protocol
pub mod interaction_limits {
    /// The bot must acknowledge within this many seconds
    pub const ACK_DEADLINE_SECS: u64 = 3;
    /// The token (edits, follow-ups, modal submits) stays valid this long after creation
    pub const TOKEN_LIFETIME_SECS: u64 = 15 * 60;
    pub const NONCE_LENGTH: usize = 64;
    pub const MAX_STRING_VALUE: usize = 6000;
    /// Characters of a partially typed autocomplete value
    pub const MAX_FOCUSED_VALUE: usize = 100;
    pub const MAX_AUTOCOMPLETE_CHOICES: usize = 25;
}

interaction_derived!(
    /// Kind of interaction
    #[derive(Copy, Eq)]
    pub enum InteractionType {
        /// Slash command, user or message context command
        Command,
        /// Button click or select choice on a bot's message
        Component,
        /// A form the bot opened was submitted
        ModalSubmit,
        /// The user is typing into an option with `autocomplete`
        Autocomplete,
    }

    /// Value a user gave for an option
    #[cfg_attr(feature = "serde", serde(untagged))]
    pub enum InteractionOptionValue {
        Boolean(bool),
        Integer(i64),
        Number(f64),
        String(String),
    }

    /// Option as filled in by the user
    pub struct InteractionDataOption {
        pub name: String,
        #[cfg_attr(feature = "serde", serde(rename = "type"))]
        pub kind: CommandOptionType,
        /// Absent for sub-commands and groups
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub value: Option<InteractionOptionValue>,
        /// Options of a sub-command or group
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub options: Option<Vec<InteractionDataOption>>,
    }

    /// Objects referenced by options and selects,
    /// limited to what the invoking user can see
    #[derive(Default)]
    pub struct InteractionResolved {
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty", default))]
        pub users: HashMap<String, User>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty", default))]
        pub members: HashMap<String, Member>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty", default))]
        pub roles: HashMap<String, Role>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty", default))]
        pub channels: HashMap<String, Channel>,
        /// Files given to `Attachment` options, by file id
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty", default))]
        pub attachments: HashMap<String, File>,
    }

    /// Which command was used, with the user's values
    pub struct InteractionCommandData {
        /// Command id
        pub id: String,
        pub name: String,
        #[cfg_attr(feature = "serde", serde(rename = "type"))]
        pub kind: ApplicationCommandType,
        /// Command revision the user saw
        pub version: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub options: Vec<InteractionDataOption>,
        /// Target user or message of a context menu command
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub target_id: Option<String>,
        /// Autocomplete only: the option being typed into (its value is the partial text)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub focused: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub resolved: Option<InteractionResolved>,
    }

    /// Which component was used
    pub struct InteractionComponentData {
        pub custom_id: String,
        /// `Button`, `StringSelect`, `UserSelect`, `RoleSelect`, `MentionableSelect` or `ChannelSelect`
        pub component_type: String,
        /// Chosen values of a select
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub values: Vec<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub resolved: Option<InteractionResolved>,
    }

    /// Submitted form
    pub struct InteractionModalData {
        pub custom_id: String,
        pub fields: Vec<ModalFieldValue>,
        /// Users, roles, channels and files chosen in the form
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub resolved: Option<InteractionResolved>,
    }

    /// Interaction as delivered to the bot
    pub struct Interaction {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Protocol version
        pub version: u32,
        pub application_id: String,
        #[cfg_attr(feature = "serde", serde(rename = "type"))]
        pub kind: InteractionType,
        /// Token for the callback, edits and follow-ups; only sent to the bot
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub token: Option<String>,
        /// User who triggered the interaction
        pub user: User,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub member: Option<Member>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub server_id: Option<String>,
        pub channel_id: String,
        /// The user's effective permissions in the channel
        pub user_permissions: u64,
        /// The bot's effective permissions in the channel
        pub app_permissions: u64,
        /// `user_permissions` as a decimal string. Use this one in JavaScript:
        /// a number cannot hold every permission bit exactly.
        #[cfg_attr(feature = "serde", serde(default))]
        pub user_permissions_string: String,
        /// `app_permissions` as a decimal string
        #[cfg_attr(feature = "serde", serde(default))]
        pub app_permissions_string: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub locale: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub command: Option<InteractionCommandData>,
        /// Message the component sits on (components, and modals opened from one)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub message_id: Option<String>,
        /// Whether `message_id` is an ephemeral message
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub message_ephemeral: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub component: Option<InteractionComponentData>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub modal: Option<InteractionModalData>,
        /// The bot must call back before this time
        pub respond_by: Timestamp,
        /// The token stops working at this time
        pub expires_at: Timestamp,
    }

    /// Interaction sent by a user
    pub struct DataCreateInteraction {
        #[cfg_attr(feature = "serde", serde(rename = "type"))]
        pub kind: InteractionType,
        /// Required for commands; components and modals take it from their message or form
        #[cfg_attr(feature = "serde", serde(default))]
        pub application_id: Option<String>,
        pub channel_id: String,
        /// Commands and autocomplete
        #[cfg_attr(feature = "serde", serde(default))]
        pub command_id: Option<String>,
        /// Revision the user picked; rejected when the command changed since
        #[cfg_attr(feature = "serde", serde(default))]
        pub command_version: Option<String>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub options: Vec<InteractionDataOption>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub target_id: Option<String>,
        /// Autocomplete: name of the option being typed into
        #[cfg_attr(feature = "serde", serde(default))]
        pub focused: Option<String>,
        /// Components: message the component is on (public or ephemeral)
        #[cfg_attr(feature = "serde", serde(default))]
        pub message_id: Option<String>,
        /// Components: which component; modal submits: which form
        #[cfg_attr(feature = "serde", serde(default))]
        pub custom_id: Option<String>,
        /// Selects: chosen values
        #[cfg_attr(feature = "serde", serde(default))]
        pub values: Option<Vec<String>>,
        /// Modal submits: the interaction that opened the form
        #[cfg_attr(feature = "serde", serde(default))]
        pub parent_id: Option<String>,
        /// Modal submits: field values
        #[cfg_attr(feature = "serde", serde(default))]
        pub fields: Option<Vec<ModalFieldValue>>,
        /// Client-generated, unique per user; a retry with the same nonce
        /// returns the first interaction instead of creating a second one
        pub nonce: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub locale: Option<String>,
    }

    /// What the invoking user gets back right away
    pub struct InteractionAccepted {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub application_id: String,
        pub channel_id: String,
        pub respond_by: Timestamp,
        pub expires_at: Timestamp,
    }

    /// How the bot answers
    #[derive(Copy, Eq)]
    pub enum InteractionCallbackType {
        /// Reply with a new message now
        Message,
        /// "Thinking..." now, the message follows by editing the original
        DeferredMessage,
        /// Components only: change the message the component is on
        UpdateMessage,
        /// Components only: acknowledge now, change the message later by editing the original
        DeferredUpdate,
        /// Open a form (not as an answer to a form or an autocomplete)
        Modal,
        /// Autocomplete only: suggestions
        AutocompleteResult,
    }

    /// Message content of a response
    #[derive(Default)]
    pub struct InteractionResponseData {
        #[cfg_attr(feature = "serde", serde(default))]
        pub content: Option<String>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub embeds: Option<Vec<SendableEmbed>>,
        /// Replaces the components; an empty list removes them
        #[cfg_attr(feature = "serde", serde(default))]
        pub components: Option<Vec<ComponentBlock>>,
        /// Only the invoking user sees the response; it is never stored as a channel message
        #[cfg_attr(feature = "serde", serde(default))]
        pub ephemeral: bool,
    }

    /// Body of the callback endpoint
    pub struct DataInteractionCallback {
        #[cfg_attr(feature = "serde", serde(rename = "type"))]
        pub kind: InteractionCallbackType,
        #[cfg_attr(feature = "serde", serde(default))]
        pub data: Option<InteractionResponseData>,
        /// For `Modal`
        #[cfg_attr(feature = "serde", serde(default))]
        pub modal: Option<Modal>,
        /// For `AutocompleteResult`
        #[cfg_attr(feature = "serde", serde(default))]
        pub choices: Option<Vec<CommandOptionChoice>>,
    }

    /// Message only the invoking user can see
    pub struct EphemeralMessage {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub interaction_id: String,
        /// The bot that sent it
        pub author: String,
        pub channel_id: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub content: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub embeds: Vec<SendableEmbed>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub components: Vec<ComponentBlock>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub edited: Option<Timestamp>,
    }

    /// Progress of an interaction, sent to the invoking user only
    #[derive(Copy, Eq)]
    pub enum InteractionState {
        /// The bot acknowledged and is working on an answer ("thinking")
        Deferred,
        /// The bot acknowledged a component and will change its message later
        DeferredUpdate,
        /// The bot answered
        Responded,
        /// The bot did not answer in time
        Failed,
    }

    /// Created follow-up message
    pub struct FollowupMessage {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Only the invoking user sees it
        pub ephemeral: bool,
    }

    /// Commands a member can pick from in a server
    pub struct ServerCommands {
        pub applications: Vec<CommandApplication>,
        pub commands: Vec<super::ApplicationCommand>,
    }

    /// Public profile of an application, for showing its commands
    #[derive(Eq)]
    pub struct CommandApplication {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub username: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub display_name: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub avatar: Option<super::File>,
    }
);

/// What a bot without the Presence intent sees of the people in an
/// interaction: everything but who is online and their status.
impl Interaction {
    pub fn hide_presence(&mut self) {
        let resolved = [
            self.command.as_mut().and_then(|data| data.resolved.as_mut()),
            self.component.as_mut().and_then(|data| data.resolved.as_mut()),
            self.modal.as_mut().and_then(|data| data.resolved.as_mut()),
        ];
        let picked = resolved
            .into_iter()
            .flatten()
            .flat_map(|resolved| resolved.users.values_mut());
        for user in std::iter::once(&mut self.user).chain(picked) {
            user.online = false;
            user.status = None;
        }
    }
}
