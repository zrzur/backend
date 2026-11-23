use iso8601_timestamp::Timestamp;
use revolt_models::v0::{EphemeralMessage, Modal};
use revolt_result::Result;

use crate::{Interaction, InteractionStatus};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

/// Fields set when an interaction moves to a new status
#[derive(Default, Clone)]
pub struct InteractionTransition {
    pub ephemeral: Option<bool>,
    pub updates_source: Option<bool>,
    pub original_message_id: Option<String>,
    pub ephemeral_message: Option<EphemeralMessage>,
    pub modal: Option<Modal>,
}

#[async_trait]
pub trait AbstractInteractions: Sync + Send {
    /// Insert a new interaction; a repeated (user, nonce) pair fails with `DuplicateNonce`
    async fn insert_interaction(&self, interaction: &Interaction) -> Result<()>;

    /// Fetch an interaction by id
    async fn fetch_interaction(&self, id: &str) -> Result<Interaction>;

    /// Fetch a user's interaction by the nonce their client sent
    async fn fetch_interaction_by_nonce(
        &self,
        user_id: &str,
        nonce: &str,
    ) -> Result<Option<Interaction>>;

    /// Fetch the unexpired interaction of `user_id` whose ephemeral answer has this id
    async fn fetch_interaction_by_ephemeral_message(
        &self,
        user_id: &str,
        message_id: &str,
    ) -> Result<Option<Interaction>>;

    /// Atomically move an interaction from one of `from` to `to`.
    ///
    /// Returns `false` when it was not in any of `from` (someone else won the race).
    async fn transition_interaction(
        &self,
        id: &str,
        from: &[InteractionStatus],
        to: InteractionStatus,
        set: InteractionTransition,
    ) -> Result<bool>;

    /// Mark the form of an interaction as submitted; `false` if it already was
    async fn claim_modal_submission(&self, id: &str) -> Result<bool>;

    /// Mark an interaction as used for a one-use action; `false` if it already was
    async fn claim_interaction_use(&self, id: &str) -> Result<bool>;

    /// Forget the values of a submitted form (a secret such as a code must
    /// not stay stored once it was read)
    async fn clear_interaction_modal_fields(&self, id: &str) -> Result<()>;

    /// Forget the stored form values of Zrzur Desk forms (`custom_id` starting
    /// with `zd:`) whose interaction expires at or before `expires_before`
    /// (the caller turns "older than 2 minutes" into that time). Other forms
    /// are untouched. Returns how many interactions were cleared.
    async fn clear_stale_desk_modal_fields(&self, expires_before: Timestamp) -> Result<u64>;

    /// Record a follow-up (a private message, or the id of a channel message);
    /// `false` once the interaction has `MAX_FOLLOWUPS` of them
    async fn push_interaction_followup(
        &self,
        id: &str,
        ephemeral: Option<EphemeralMessage>,
        message_id: Option<String>,
    ) -> Result<bool>;

    /// Replace a private message of the interaction (first answer or follow-up) by its id
    async fn replace_interaction_ephemeral(&self, id: &str, message: &EphemeralMessage)
        -> Result<bool>;

    /// Remove a private message of the interaction by its id
    async fn remove_interaction_ephemeral(&self, id: &str, message_id: &str) -> Result<bool>;
}
