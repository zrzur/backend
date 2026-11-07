use revolt_result::Result;

use revolt_models::v0::EphemeralMessage;

use crate::{Interaction, InteractionStatus, ReferenceDb, MAX_FOLLOWUPS};

use super::{AbstractInteractions, InteractionTransition};

#[async_trait]
impl AbstractInteractions for ReferenceDb {
    async fn insert_interaction(&self, interaction: &Interaction) -> Result<()> {
        let mut interactions = self.interactions.lock().await;
        if interactions.values().any(|other| {
            other.user_id == interaction.user_id && other.nonce == interaction.nonce
        }) {
            return Err(create_error!(DuplicateNonce));
        }
        interactions.insert(interaction.id.clone(), interaction.clone());
        Ok(())
    }

    async fn fetch_interaction(&self, id: &str) -> Result<Interaction> {
        self.interactions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(UnknownInteraction))
    }

    async fn fetch_interaction_by_nonce(
        &self,
        user_id: &str,
        nonce: &str,
    ) -> Result<Option<Interaction>> {
        Ok(self
            .interactions
            .lock()
            .await
            .values()
            .find(|i| i.user_id == user_id && i.nonce == nonce)
            .cloned())
    }

    async fn fetch_interaction_by_ephemeral_message(
        &self,
        user_id: &str,
        message_id: &str,
    ) -> Result<Option<Interaction>> {
        Ok(self
            .interactions
            .lock()
            .await
            .values()
            .find(|i| {
                i.user_id == user_id
                    && !i.is_expired()
                    && i.find_ephemeral(message_id).is_some()
            })
            .cloned())
    }

    async fn transition_interaction(
        &self,
        id: &str,
        from: &[InteractionStatus],
        to: InteractionStatus,
        set: InteractionTransition,
    ) -> Result<bool> {
        let mut interactions = self.interactions.lock().await;
        let Some(interaction) = interactions.get_mut(id) else {
            return Ok(false);
        };
        if !from.contains(&interaction.status) {
            return Ok(false);
        }
        interaction.status = to;
        if let Some(ephemeral) = set.ephemeral {
            interaction.ephemeral = ephemeral;
        }
        if let Some(updates_source) = set.updates_source {
            interaction.updates_source = updates_source;
        }
        if let Some(message_id) = set.original_message_id {
            interaction.original_message_id = Some(message_id);
        }
        if let Some(message) = set.ephemeral_message {
            interaction.ephemeral_message = Some(message);
        }
        if let Some(modal) = set.modal {
            interaction.modal = Some(modal);
        }
        Ok(true)
    }

    async fn push_interaction_followup(
        &self,
        id: &str,
        ephemeral: Option<EphemeralMessage>,
        message_id: Option<String>,
    ) -> Result<bool> {
        let mut interactions = self.interactions.lock().await;
        let Some(interaction) = interactions.get_mut(id) else {
            return Ok(false);
        };
        if interaction.followup_count() >= MAX_FOLLOWUPS {
            return Ok(false);
        }
        match (ephemeral, message_id) {
            (Some(message), _) => interaction.followups.push(message),
            (None, Some(message_id)) => interaction.followup_message_ids.push(message_id),
            (None, None) => return Ok(false),
        }
        Ok(true)
    }

    async fn replace_interaction_ephemeral(
        &self,
        id: &str,
        message: &EphemeralMessage,
    ) -> Result<bool> {
        let mut interactions = self.interactions.lock().await;
        let Some(interaction) = interactions.get_mut(id) else {
            return Ok(false);
        };
        let slot = interaction
            .ephemeral_message
            .iter_mut()
            .chain(interaction.followups.iter_mut())
            .find(|existing| existing.id == message.id);
        match slot {
            Some(slot) => {
                *slot = message.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn remove_interaction_ephemeral(&self, id: &str, message_id: &str) -> Result<bool> {
        let mut interactions = self.interactions.lock().await;
        let Some(interaction) = interactions.get_mut(id) else {
            return Ok(false);
        };
        if interaction
            .ephemeral_message
            .as_ref()
            .is_some_and(|message| message.id == message_id)
        {
            interaction.ephemeral_message = None;
            return Ok(true);
        }
        let before = interaction.followups.len();
        interaction.followups.retain(|message| message.id != message_id);
        Ok(before != interaction.followups.len())
    }

    async fn claim_modal_submission(&self, id: &str) -> Result<bool> {
        let mut interactions = self.interactions.lock().await;
        match interactions.get_mut(id) {
            Some(interaction) if interaction.modal.is_some() && !interaction.modal_submitted => {
                interaction.modal_submitted = true;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
