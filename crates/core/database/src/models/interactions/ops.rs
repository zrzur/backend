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
