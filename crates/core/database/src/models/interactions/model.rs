use iso8601_timestamp::{Duration, Timestamp};
use revolt_models::v0::{
    interaction_limits, CommandOptionType, EphemeralMessage, InteractionDataOption,
    InteractionType, Modal, ModalFieldValue,
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
    /// Commands: the options as the server checked them (never the bot's
    /// copy), so a route acting on the person's behalf reads what they chose.
    /// Gone with the interaction when it expires.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub command_options: Vec<InteractionDataOption>,
    /// The command's version when the person used it, kept with
    /// `command_options`: a route acting later checks the command was not
    /// changed in between
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub command_version: Option<String>,
    /// The language the person's client uses, kept with `command_options`
    /// (Zrzur Desk answers in it)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub locale: Option<String>,
    pub nonce: String,
    /// The ordinary session (sign-in) the person used. A route acting on
    /// the person's behalf can bind its own state to that sign-in, so
    /// signing out ends it. `None` on old documents.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_id: Option<String>,
    pub token_hash: String,
    pub status: InteractionStatus,
    /// Whether the first answer is (or will be, when deferred) private
    #[serde(default)]
    pub ephemeral: bool,
    /// Components and modal submits: where the component was
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<InteractionSource>,
    /// Components: the `custom_id` of the button or menu the person used;
    /// form submits: the form's `custom_id`. Kept so a route acting on the
    /// person's behalf can check what they really pressed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub custom_id: Option<String>,
    /// Form submits: the checked values the person sent
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub modal_fields: Vec<ModalFieldValue>,
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
    /// A one-use action was taken with this interaction (once)
    #[serde(default)]
    pub used: bool,
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

/// How long the stored values of a Zrzur Desk form (a sign-in code) may stay
pub const DESK_FIELDS_KEEP_SECS: i64 = 2 * 60;

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
                command_options: vec![],
                command_version: None,
                locale: None,
                nonce,
                session_id: None,
                token_hash: hash_interaction_token(&token),
                status: InteractionStatus::Pending,
                ephemeral: false,
                source: None,
                custom_id: None,
                modal_fields: vec![],
                updates_source: false,
                original_message_id: None,
                ephemeral_message: None,
                modal: None,
                modal_submitted: false,
                used: false,
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

    /// Interactions that expire at or before the returned time were created
    /// more than `DESK_FIELDS_KEEP_SECS` ago
    pub fn desk_fields_expiry_cutoff(now: Timestamp) -> Timestamp {
        now.checked_add(Duration::seconds(
            interaction_limits::TOKEN_LIFETIME_SECS as i64 - DESK_FIELDS_KEEP_SECS,
        ))
        .unwrap_or(now)
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

#[cfg(test)]
mod desk_fields_tests {
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};
    use revolt_models::v0::{InteractionType, ModalFieldValue};

    use super::*;

    fn with_fields(custom_id: &str, age_secs: i64) -> Interaction {
        let (mut i, _) = Interaction::new(
            InteractionType::ModalSubmit,
            "app".into(),
            "user".into(),
            None,
            "chan".into(),
            Ulid::new().to_string(),
        );
        i.custom_id = Some(custom_id.into());
        i.modal_fields = vec![ModalFieldValue {
            custom_id: "code".into(),
            value: Some("123456".into()),
            values: None,
            checked: None,
        }];
        // pretend it was created `age_secs` ago
        i.expires_at = Timestamp::now_utc()
            .checked_add(Duration::seconds(
                interaction_limits::TOKEN_LIFETIME_SECS as i64 - age_secs,
            ))
            .unwrap();
        i
    }

    /// Only desk forms older than two minutes lose their fields; a second run
    /// clears nothing more
    #[tokio::test]
    async fn stale_desk_fields_are_cleared_and_nothing_else() {
        database_test!(|db| async move {
            let old_desk = with_fields("zd:signin", 180);
            let young_desk = with_fields("zd:signin", 30);
            let old_other = with_fields("other:form", 180);
            for i in [&old_desk, &young_desk, &old_other] {
                db.insert_interaction(i).await.unwrap();
            }

            let cutoff = Interaction::desk_fields_expiry_cutoff(Timestamp::now_utc());
            assert_eq!(db.clear_stale_desk_modal_fields(cutoff).await.unwrap(), 1);
            assert!(db.fetch_interaction(&old_desk.id).await.unwrap().modal_fields.is_empty());
            assert_eq!(db.fetch_interaction(&young_desk.id).await.unwrap().modal_fields.len(), 1);
            assert_eq!(db.fetch_interaction(&old_other.id).await.unwrap().modal_fields.len(), 1);
            assert_eq!(db.clear_stale_desk_modal_fields(cutoff).await.unwrap(), 0);
        });
    }
}
