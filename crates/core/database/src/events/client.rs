use iso8601_timestamp::Timestamp;
use revolt_result::Error;
use serde::{Deserialize, Serialize};

use revolt_models::v0::{
    AppendMessage, Channel, ChannelSlowmode, ChannelUnread, ChannelVoiceState, Emoji,
    FieldsChannel, FieldsMember, FieldsMessage, FieldsRole, FieldsServer, FieldsUser,
    FieldsWebhook, Member, MemberCompositeKey, Message, PartialChannel, PartialEmoji,
    PartialMember, PartialMessage, PartialRole, PartialServer, PartialUser, PartialUserVoiceState,
    PartialWebhook, PolicyChange, PollAnswerCount, RemovalIntention, Report, Server, ServerEvent,
    SoundboardSound, User, UserSettings, UserVoiceState, Webhook,
};

use crate::{Account, Database, Session, amqp::get_amqp};

/// Ping Packet
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "rocket-impl", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Ping {
    Binary(Vec<u8>),
    Number(usize),
}

/// Fields provided in Ready payload
#[derive(PartialEq, Debug, Clone, Deserialize)]
pub struct ReadyPayloadFields {
    pub users: bool,
    pub servers: bool,
    pub channels: bool,
    pub members: bool,
    pub emojis: bool,
    pub voice_states: bool,
    pub user_settings: Vec<String>,
    pub channel_unreads: bool,
    pub policy_changes: bool,
}

impl Default for ReadyPayloadFields {
    fn default() -> Self {
        Self {
            users: true,
            servers: true,
            channels: true,
            members: true,
            emojis: true,
            voice_states: true,
            user_settings: Vec::new(),
            channel_unreads: false,
            policy_changes: true,
        }
    }
}

/// Protocol Events
///
/// `GET /gateway/schema` publishes these as JSON Schema; variants marked
/// `schemars(skip)` only travel between our own services.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "rocket-impl", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum EventV1 {
    /// Multiple events
    Bulk {
        v: Vec<EventV1>,
    },
    /// Error event
    Error {
        data: Error,
    },

    /// Successfully authenticated
    Authenticated,
    /// Logged out
    Logout,
    /// Basic data to cache
    Ready {
        #[serde(skip_serializing_if = "Option::is_none")]
        users: Option<Vec<User>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        servers: Option<Vec<Server>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        channels: Option<Vec<Channel>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        members: Option<Vec<Member>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        emojis: Option<Vec<Emoji>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        voice_states: Option<Vec<ChannelVoiceState>>,

        #[serde(skip_serializing_if = "Option::is_none")]
        user_settings: Option<UserSettings>,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel_unreads: Option<Vec<ChannelUnread>>,

        #[serde(skip_serializing_if = "Option::is_none")]
        policy_changes: Option<Vec<PolicyChange>>,

        /// Id of this gateway session, for `Resume`. Only given when the
        /// client asked for a resumable session.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        session_id: Option<String>,
    },

    /// The session was resumed: every missed event the client may still
    /// see was sent before this, in order
    Resumed,

    /// The session can't be resumed; send `Authenticate` on this socket
    /// for a new one (and catch up over the API)
    InvalidSession {
        resumable: bool,
    },

    /// Ping response
    Pong {
        data: Ping,
    },

    /// Release the bees.
    Bees,

    /// New message
    Message(Message),

    /// Update existing message
    MessageUpdate {
        id: String,
        channel: String,
        data: PartialMessage,
        #[serde(default)]
        clear: Vec<FieldsMessage>,
    },

    /// Append information to existing message
    MessageAppend {
        id: String,
        channel: String,
        append: AppendMessage,
    },

    /// Delete message
    MessageDelete {
        id: String,
        channel: String,
    },

    /// New reaction to a message
    MessageReact {
        id: String,
        channel_id: String,
        user_id: String,
        emoji_id: String,
    },

    /// Remove user's reaction from message
    MessageUnreact {
        id: String,
        channel_id: String,
        user_id: String,
        emoji_id: String,
    },

    /// Remove a reaction from message
    MessageRemoveReaction {
        id: String,
        channel_id: String,
        emoji_id: String,
    },

    /// Someone chose an answer of a poll: the counts of every answer after
    /// it (who voted is read on request, never sent to everyone)
    MessagePollVoteAdd {
        id: String,
        channel_id: String,
        answer_id: u32,
        answers: Vec<PollAnswerCount>,
        /// Votes of every answer together
        total: u32,
        /// Poll version after this change; older events can be dropped
        version: u64,
    },

    /// Someone took back their choice of an answer: the counts after it
    MessagePollVoteRemove {
        id: String,
        channel_id: String,
        answer_id: u32,
        answers: Vec<PollAnswerCount>,
        /// Votes of every answer together
        total: u32,
        /// Poll version after this change; older events can be dropped
        version: u64,
    },

    /// Your own choice on a poll changed (to your sessions only)
    MessagePollChoice {
        id: String,
        channel_id: String,
        answers: Vec<u32>,
        /// Poll version after this change
        version: u64,
    },

    /// A poll closed (ended early or after its time): final counts
    MessagePollEnd {
        id: String,
        channel_id: String,
        answers: Vec<PollAnswerCount>,
        /// Votes of every answer together
        total: u32,
        /// Poll version after this change; older events can be dropped
        version: u64,
    },

    /// Bulk delete messages
    BulkMessageDelete {
        channel: String,
        ids: Vec<String>,
    },

    /// New server
    ServerCreate {
        id: String,
        server: Server,
        channels: Vec<Channel>,
        emojis: Vec<Emoji>,
        voice_states: Vec<ChannelVoiceState>,
    },

    /// Update existing server
    ServerUpdate {
        id: String,
        data: PartialServer,
        #[serde(default)]
        clear: Vec<FieldsServer>,
    },

    /// Delete server
    ServerDelete {
        id: String,
    },

    /// Update existing server member
    ServerMemberUpdate {
        id: MemberCompositeKey,
        data: PartialMember,
        #[serde(default)]
        clear: Vec<FieldsMember>,
    },

    /// User joins server
    ServerMemberJoin {
        id: String,
        // Deprecated: use member.id.user
        #[deprecated = "Use member.id.user instead"]
        user: String,
        member: Member,
    },

    /// User left server
    ServerMemberLeave {
        id: String,
        user: String,
        reason: RemovalIntention,
    },

    /// Server role created or updated
    ServerRoleUpdate {
        id: String,
        role_id: String,
        data: PartialRole,
        #[serde(default)]
        clear: Vec<FieldsRole>,
    },

    /// Server role deleted
    ServerRoleDelete {
        id: String,
        role_id: String,
    },

    /// Server roles ranks updated
    ServerRoleRanksUpdate {
        id: String,
        ranks: Vec<String>,
    },

    /// Update existing user
    UserUpdate {
        id: String,
        data: PartialUser,
        #[serde(default)]
        clear: Vec<FieldsUser>,
        event_id: Option<String>,
    },

    /// Relationship with another user changed
    UserRelationship {
        id: String,
        user: User,
    },
    /// Settings updated remotely
    UserSettingsUpdate {
        id: String,
        update: UserSettings,
    },

    /// User has been platform banned or deleted their account
    ///
    /// Clients should remove the following associated data:
    /// - Messages
    /// - DM Channels
    /// - Relationships
    /// - Server Memberships
    ///
    /// User flags are specified to explain why a wipe is occurring though not all reasons will necessarily ever appear.
    UserPlatformWipe {
        user_id: String,
        flags: i32,
    },
    /// New emoji
    EmojiCreate(Emoji),

    /// Update existing emoji
    EmojiUpdate {
        id: String,
        data: PartialEmoji,
    },

    /// Delete emoji
    EmojiDelete {
        id: String,
    },

    /// New soundboard sound (sent to the server)
    SoundboardSoundCreate(SoundboardSound),
    /// A soundboard sound changed (the whole sound)
    SoundboardSoundUpdate(SoundboardSound),
    /// A soundboard sound was removed
    SoundboardSoundDelete {
        id: String,
        server: String,
    },

    /// New report
    ReportCreate(Report),
    /// New channel
    ChannelCreate(Channel),

    /// Update existing channel
    ChannelUpdate {
        id: String,
        data: PartialChannel,
        #[serde(default)]
        clear: Vec<FieldsChannel>,
    },

    /// Delete channel
    ChannelDelete {
        id: String,
    },

    /// User joins a group
    ChannelGroupJoin {
        id: String,
        user: String,
    },

    /// User leaves a group
    ChannelGroupLeave {
        id: String,
        user: String,
    },

    /// User started typing in a channel
    ChannelStartTyping {
        id: String,
        user: String,
    },

    /// User stopped typing in a channel
    ChannelStopTyping {
        id: String,
        user: String,
    },

    /// User acknowledged message in channel
    ChannelAck {
        id: String,
        user: String,
        message_id: String,
    },

    /// New webhook
    WebhookCreate(Webhook),

    /// Update existing webhook
    WebhookUpdate {
        id: String,
        channel_id: String,
        data: PartialWebhook,
        remove: Vec<FieldsWebhook>,
    },

    /// Delete webhook
    WebhookDelete {
        id: String,
        channel_id: String,
    },

    /// New scheduled server event (sent to the place's channel, or to the
    /// server for external places)
    ServerEventCreate(ServerEvent),

    /// A scheduled server event changed (status included)
    ServerEventUpdate {
        event: ServerEvent,
    },

    /// A scheduled server event is gone, or no longer visible to you
    ServerEventDelete {
        id: String,
        server: String,
    },

    /// Someone marked or unmarked a scheduled server event "Interested"
    ServerEventInterest {
        id: String,
        server: String,
        /// The place's channel, for channel places
        #[serde(skip_serializing_if = "Option::is_none", default)]
        channel: Option<String>,
        user: String,
        interested: bool,
        count: u32,
    },

    /// Auth events
    #[cfg_attr(feature = "rocket-impl", schemars(skip))]
    CreateAccount {
        account: Account,
    },
    #[cfg_attr(feature = "rocket-impl", schemars(skip))]
    CreateSession {
        session: Session,
    },
    #[cfg_attr(feature = "rocket-impl", schemars(skip))]
    DeleteSession {
        user_id: String,
        session_id: String,
    },
    /// The age group of your account changed (sent only to you)
    AccountAgeUpdate {
        band: revolt_models::v0::AgeBand,
    },
    #[cfg_attr(feature = "rocket-impl", schemars(skip))]
    DeleteAllSessions {
        user_id: String,
        exclude_session_id: Option<String>,
    },

    /// Voice events
    VoiceChannelJoin {
        id: String,
        state: UserVoiceState,
    },
    VoiceChannelLeave {
        id: String,
        user: String,
    },
    VoiceChannelMove {
        user: String,
        from: String,
        to: String,
        state: UserVoiceState,
    },
    UserVoiceStateUpdate {
        id: String,
        channel_id: String,
        data: PartialUserVoiceState,
    },
    UserMoveVoiceChannel {
        node: String,
        from: String,
        to: String,
        token: String,
    },
    /// The live state of a stage changed (topic, speakers or raised hands)
    StageUpdate {
        id: String,
        data: revolt_models::v0::StageState,
    },
    /// Someone played a soundboard sound in a voice channel
    SoundboardPlay {
        channel_id: String,
        sound_id: String,
        user: String,
    },
    /// User's active slowmodes
    UserSlowmodes {
        slowmodes: Vec<ChannelSlowmode>,
    },
    VoiceCallUpdate {
        initiator_id: String,
        channel_id: String,
        started_at: Option<Timestamp>,
        ended: bool,
    },

    /// A user used one of this bot's commands (sent to the bot only)
    InteractionCreate {
        interaction: revolt_models::v0::Interaction,
    },
    /// Progress of an interaction (sent to the invoking user only)
    InteractionStateUpdate {
        id: String,
        channel_id: String,
        state: revolt_models::v0::InteractionState,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        message_id: Option<String>,
    },
    /// Message only the invoking user sees; never stored in the channel
    EphemeralMessageCreate {
        message: revolt_models::v0::EphemeralMessage,
    },
    /// Edit of an ephemeral message
    EphemeralMessageUpdate {
        message: revolt_models::v0::EphemeralMessage,
    },
    /// An ephemeral message was deleted by its bot
    EphemeralMessageDelete {
        id: String,
        channel_id: String,
    },
    /// The bot opened a form for the invoking user (sent to that user only)
    InteractionModal {
        id: String,
        channel_id: String,
        modal: revolt_models::v0::Modal,
    },
    /// Suggestions for the option being typed into (sent to the invoking user only)
    InteractionAutocompleteResult {
        id: String,
        choices: Vec<revolt_models::v0::CommandOptionChoice>,
    },
    /// An application changed its commands; clients refetch them for this server
    ApplicationCommandsUpdate {
        application_id: String,
        server_id: String,
    },
}

impl EventV1 {
    // Shim for legacy redis channel names
    fn redis_suffix(&self, channel: String) -> String {
        use EventV1::*;
        match self {
            ChannelAck { .. }
            | Logout
            | DeleteSession { .. }
            | DeleteAllSessions { .. }
            | AccountAgeUpdate { .. }
            | UserRelationship { .. }
            | UserSettingsUpdate { .. }
            | UserSlowmodes { .. }
            | MessagePollChoice { .. }
            | UserMoveVoiceChannel { .. }
            | InteractionCreate { .. }
            | InteractionStateUpdate { .. }
            | EphemeralMessageCreate { .. }
            | EphemeralMessageUpdate { .. }
            | EphemeralMessageDelete { .. }
            | InteractionModal { .. }
            | InteractionAutocompleteResult { .. }
            | ChannelCreate(Channel::DirectMessage { .. } | Channel::Group { .. })
            | ServerCreate { .. } => format!("{channel}!"),

            _ => channel,
        }
    }

    /// Publish helper wrapper
    pub async fn p(self, channel: String) {
        #[cfg(debug_assertions)]
        info!("Publishing event to {channel}: {self:?}");

        let redis_channel = self.redis_suffix(channel.clone());

        redis_kiss::publish(redis_channel, &self).await.unwrap();

        if let Err(e) = get_amqp().publish_event(channel, &self).await {
            if cfg!(debug_assertions) {
                panic!("{e:?}");
            } else {
                log::error!("{e:?}");
            };
        };
    }

    pub async fn p_broadcast(self, channels: Vec<String>) {
        #[cfg(debug_assertions)]
        info!("Broadcasting event to channels: {channels:?}: {self:?}");

        for channel in &channels {
            let redis_channel = self.redis_suffix(channel.clone());
            redis_kiss::publish(redis_channel, &self).await.unwrap();
        }

        if let Err(e) = get_amqp().publish_event_broadcast(channels, &self).await {
            if cfg!(debug_assertions) {
                panic!("{e:?}");
            } else {
                log::error!("{e:?}");
            };
        };
    }

    /// Publish internal global event
    pub async fn global(self) {
        self.p("global".to_string()).await;
    }
}
