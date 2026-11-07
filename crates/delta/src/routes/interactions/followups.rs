//! Follow-up messages, and editing or deleting any message created with an
//! interaction token. Everything here works until the token expires.

use revolt_database::{events::client::EventV1, Database, Interaction, InteractionStatus, AMQP};
use revolt_models::v0::{FollowupMessage, InteractionResponseData};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

use super::respond::{
    authorised_interaction, edit_ephemeral, edit_public_message, ephemeral_message,
    send_public_message, validate_response,
};

/// Remove a private message and tell the invoking user's clients
async fn delete_ephemeral(db: &Database, interaction: &Interaction, message_id: &str) -> Result<()> {
    if !db
        .remove_interaction_ephemeral(&interaction.id, message_id)
        .await?
    {
        return Err(create_error!(UnknownMessage));
    }
    EventV1::EphemeralMessageDelete {
        id: message_id.to_string(),
        channel_id: interaction.channel_id.clone(),
    }
    .p(interaction.user_id.clone())
    .await;
    Ok(())
}

/// Delete a channel message this interaction created
async fn delete_public(db: &Database, interaction: &Interaction, message_id: &str) -> Result<()> {
    let message = db.fetch_message(message_id).await?;
    if message.author != interaction.application_id || message.channel != interaction.channel_id {
        return Err(create_error!(CannotDeleteMessage));
    }
    message.delete(db).await
}

/// # Send Follow-up
///
/// Another message for the same interaction, after the first answer (or
/// while it is deferred). At most 5 per interaction.
#[openapi(tag = "Interactions")]
#[post("/<interaction_id>/<token>/followups", data = "<data>")]
pub async fn send_followup(
    db: &State<Database>,
    amqp: &State<AMQP>,
    interaction_id: &str,
    token: &str,
    data: Json<InteractionResponseData>,
) -> Result<Json<FollowupMessage>> {
    let interaction = authorised_interaction(db, interaction_id, token).await?;
    if interaction.status == InteractionStatus::Pending || interaction.modal.is_some() {
        return Err(create_error!(InvalidOperation));
    }
    if interaction.followup_count() >= revolt_database::MAX_FOLLOWUPS {
        return Err(create_error!(TooManyFollowups {
            max: revolt_database::MAX_FOLLOWUPS
        }));
    }
    let data = data.into_inner();
    validate_response(db, &interaction, &data, true).await?;

    if data.ephemeral {
        let message = ephemeral_message(&interaction, data)?;
        if !db
            .push_interaction_followup(&interaction.id, Some(message.clone()), None)
            .await?
        {
            return Err(create_error!(InvalidOperation));
        }
        let id = message.id.clone();
        EventV1::EphemeralMessageCreate { message }
            .p(interaction.user_id.clone())
            .await;
        return Ok(Json(FollowupMessage {
            id,
            ephemeral: true,
        }));
    }

    let message = send_public_message(db, amqp, &interaction, data).await?;
    db.push_interaction_followup(&interaction.id, None, Some(message.id.clone()))
        .await?;
    Ok(Json(FollowupMessage {
        id: message.id,
        ephemeral: false,
    }))
}

/// # Edit Interaction Message
///
/// Edit the first answer or a follow-up by its message id.
#[openapi(tag = "Interactions")]
#[patch("/<interaction_id>/<token>/messages/<message_id>", data = "<data>")]
pub async fn edit_interaction_message(
    db: &State<Database>,
    interaction_id: &str,
    token: &str,
    message_id: &str,
    data: Json<InteractionResponseData>,
) -> Result<EmptyResponse> {
    let interaction = authorised_interaction(db, interaction_id, token).await?;
    let data = data.into_inner();
    validate_response(db, &interaction, &data, false).await?;

    if interaction.find_ephemeral(message_id).is_some() {
        edit_ephemeral(db, &interaction, message_id, data).await?;
    } else if interaction.owns_message(message_id) {
        edit_public_message(db, &interaction, message_id, data, true).await?;
    } else {
        return Err(create_error!(UnknownMessage));
    }
    Ok(EmptyResponse)
}

/// # Delete Interaction Message
///
/// Delete the first answer or a follow-up by its message id. For private
/// messages the invoking user's clients receive `EphemeralMessageDelete`.
#[openapi(tag = "Interactions")]
#[delete("/<interaction_id>/<token>/messages/<message_id>")]
pub async fn delete_interaction_message(
    db: &State<Database>,
    interaction_id: &str,
    token: &str,
    message_id: &str,
) -> Result<EmptyResponse> {
    let interaction = authorised_interaction(db, interaction_id, token).await?;
    if interaction.find_ephemeral(message_id).is_some() {
        delete_ephemeral(db, &interaction, message_id).await?;
    } else if interaction.owns_message(message_id) {
        delete_public(db, &interaction, message_id).await?;
    } else {
        return Err(create_error!(UnknownMessage));
    }
    Ok(EmptyResponse)
}

/// # Delete Original Response
#[openapi(tag = "Interactions")]
#[delete("/<interaction_id>/<token>/original")]
pub async fn delete_original_response(
    db: &State<Database>,
    interaction_id: &str,
    token: &str,
) -> Result<EmptyResponse> {
    let interaction = authorised_interaction(db, interaction_id, token).await?;
    if let Some(message) = &interaction.ephemeral_message {
        delete_ephemeral(db, &interaction, &message.id).await?;
    } else if let Some(message_id) = &interaction.original_message_id {
        delete_public(db, &interaction, message_id).await?;
    } else {
        return Err(create_error!(UnknownMessage));
    }
    Ok(EmptyResponse)
}
