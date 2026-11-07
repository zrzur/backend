use iso8601_timestamp::{Duration, Timestamp};
use revolt_models::v0::{
    interaction_limits, CommandOptionType, EphemeralMessage, InteractionType, Modal,
};
use sha2::{Digest, Sha256};
use ulid::Ulid;

/// Where an interaction stands
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionStatus {
    /// Waiting for the bot's first answer
    Pending,
    /// The bot said it is working on it
    Deferred,
    /// The bot answered
    Responded,
}

/// Message a component interaction came from
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum InteractionSource {
    /// A channel message (by the bot)
    Message { id: String },
    /// An ephemeral message, kept on the interaction that created it
    Ephemeral { interaction_id: String, message_id: String },
}

/// Stored interaction.
///
/// Only a hash of the token is stored. Documents expire on their own through
/// the TTL index on `expires_at`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Interaction {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "type")]
    pub kind: InteractionType,
    pub application_id: String,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub server_id: Option<String>,
    pub channel_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub command_id: Option<String>,
    /// Command name, for the "used /name" line on the answer
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub command_name: Option<String>,
    pub nonce: String,
    pub token_hash: String,
    pub status: InteractionStatus,
    /// Whether the first answer is (or will be, when deferred) private
    #[serde(default)]
    pub ephemeral: bool,
    /// Components and modal submits: where the component was
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<InteractionSource>,
    /// The answer changes `source` instead of creating a message (`UpdateMessage`, `DeferredUpdate`)
    #[serde(default)]
    pub updates_source: bool,
    /// Channel message created as the first answer
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub original_message_id: Option<String>,
    /// Private first answer, kept so it can be edited and its components used until expiry
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ephemeral_message: Option<EphemeralMessage>,
    /// Form the bot opened; a submit is checked against it
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub modal: Option<Modal>,
    /// The form was submitted (once)
    #[serde(default)]
    pub modal_submitted: bool,
    /// Autocomplete: type of the focused option, to check the bot's suggestions
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub autocomplete_type: Option<CommandOptionType>,
    /// Private follow-up messages, kept like the private first answer
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub followups: Vec<EphemeralMessage>,
    /// Channel messages sent as follow-ups (so the token may edit them)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub followup_message_ids: Vec<String>,
    pub respond_by: Timestamp,
    pub expires_at: Timestamp,
}

/// Follow-up messages allowed per interaction
pub const MAX_FOLLOWUPS: usize = 5;

impl Interaction {
    /// A private message of this interaction (the first answer or a follow-up)
    pub fn find_ephemeral(&self, message_id: &str) -> Option<&EphemeralMessage> {
        self.ephemeral_message
            .iter()
            .chain(self.followups.iter())
            .find(|message| message.id == message_id)
    }

    /// Whether this interaction's token created this channel message
    pub fn owns_message(&self, message_id: &str) -> bool {
        self.original_message_id.as_deref() == Some(message_id)
            || self.followup_message_ids.iter().any(|id| id == message_id)
    }

    pub fn followup_count(&self) -> usize {
        self.followups.len() + self.followup_message_ids.len()
    }
}

pub fn hash_interaction_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Interaction {
    /// New pending interaction and its plain token (only ever given to the bot)
    pub fn new(
        kind: InteractionType,
        application_id: String,
        user_id: String,
        server_id: Option<String>,
        channel_id: String,
        nonce: String,
    ) -> (Interaction, String) {
        let id = Ulid::new();
        let created: Timestamp = id.datetime().into();
        let token = nanoid::nanoid!(64);
        (
            Interaction {
                id: id.to_string(),
                kind,
                application_id,
                user_id,
                server_id,
                channel_id,
                command_id: None,
                command_name: None,
                nonce,
                token_hash: hash_interaction_token(&token),
                status: InteractionStatus::Pending,
                ephemeral: false,
                source: None,
                updates_source: false,
                original_message_id: None,
                ephemeral_message: None,
                modal: None,
                modal_submitted: false,
                autocomplete_type: None,
                followups: vec![],
                followup_message_ids: vec![],
                respond_by: created
                    .checked_add(Duration::seconds(
                        interaction_limits::ACK_DEADLINE_SECS as i64,
                    ))
                    .unwrap(),
                expires_at: created
                    .checked_add(Duration::seconds(
                        interaction_limits::TOKEN_LIFETIME_SECS as i64,
                    ))
                    .unwrap(),
            },
            token,
        )
    }

    pub fn token_matches(&self, token: &str) -> bool {
        hash_interaction_token(token) == self.token_hash
    }

    pub fn is_expired(&self) -> bool {
        Timestamp::now_utc() >= self.expires_at
    }

    pub fn past_ack_deadline(&self) -> bool {
        Timestamp::now_utc() >= self.respond_by
    }
}
