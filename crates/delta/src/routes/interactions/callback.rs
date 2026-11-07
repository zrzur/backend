use revolt_database::{
    events::client::EventV1, Database, Interaction, InteractionStatus, InteractionTransition,
    AMQP,
};
use revolt_models::v0::{
    interaction_limits, CommandOptionChoice, CommandOptionType, CommandOptionValue,
    DataInteractionCallback, InteractionCallbackType, InteractionResponseData, InteractionState,
    InteractionType,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

use super::{
    options::invalid,
    respond::{
        authorised_interaction, edit_ephemeral, edit_public_message, ephemeral_message,
        send_public_message, state_update, update_source, validate_response,
    },
};

/// Claim a pending interaction, so exactly one first answer wins
async fn claim(
    db: &Database,
    interaction: &Interaction,
    to: InteractionStatus,
    set: InteractionTransition,
) -> Result<()> {
    if db
        .transition_interaction(&interaction.id, &[InteractionStatus::Pending], to, set)
        .await?
    {
        Ok(())
    } else {
        Err(create_error!(InteractionAlreadyAcknowledged))
    }
}

/// Suggestions must fit the option they are for
fn check_choices(interaction: &Interaction, choices: &[CommandOptionChoice]) -> Result<()> {
    if choices.len() > interaction_limits::MAX_AUTOCOMPLETE_CHOICES {
        return Err(invalid("at most 25 choices"));
    }
    let kind = interaction
        .autocomplete_type
        .ok_or(create_error!(InvalidOperation))?;
    for choice in choices {
        let len = choice.name.chars().count();
        if len == 0 || len > 100 {
            return Err(invalid("choice names must be 1-100 characters"));
        }
        let fits = match (&choice.value, kind) {
            (CommandOptionValue::String(value), CommandOptionType::String) => {
                (1..=100).contains(&value.chars().count())
            }
            (CommandOptionValue::Integer(_), CommandOptionType::Integer)
            | (CommandOptionValue::Integer(_), CommandOptionType::Number) => true,
            (CommandOptionValue::Number(value), CommandOptionType::Number) => value.is_finite(),
            _ => false,
        };
        if !fits {
            return Err(invalid("a choice value does not fit the option"));
        }
    }
    Ok(())
}

/// # Respond to Interaction
///
/// First answer of the bot, authenticated by the interaction token. It must
/// arrive before `respond_by` and can only happen once.
///
/// - `Message`: reply now; `data.ephemeral` makes it visible to the invoking
///   user only, and it is never stored as a channel message.
/// - `DeferredMessage`: show that the bot is working; finish later with
///   `PATCH /interactions/<id>/<token>/original` before the token expires.
/// - `UpdateMessage` / `DeferredUpdate` (components and forms opened from one):
///   change the message the component is on, now or later.
/// - `Modal`: open a form for the user (not as an answer to a form).
/// - `AutocompleteResult` (autocomplete only): up to 25 suggestions.
#[openapi(tag = "Interactions")]
#[post("/<interaction_id>/<token>/callback", data = "<data>")]
pub async fn interaction_callback(
    db: &State<Database>,
    amqp: &State<AMQP>,
    interaction_id: &str,
    token: &str,
    data: Json<DataInteractionCallback>,
) -> Result<EmptyResponse> {
    let interaction = authorised_interaction(db, interaction_id, token).await?;
    answer(db, amqp, interaction, data.into_inner()).await?;
    Ok(EmptyResponse)
}

/// The bot's first answer, from the callback route or from the body of its
/// endpoint's reply
pub async fn answer(
    db: &Database,
    amqp: &AMQP,
    interaction: Interaction,
    data: DataInteractionCallback,
) -> Result<()> {
    if interaction.status != InteractionStatus::Pending {
        return Err(create_error!(InteractionAlreadyAcknowledged));
    }
    if interaction.past_ack_deadline() {
        return Err(create_error!(InteractionExpired));
    }

    let is_autocomplete = interaction.kind == InteractionType::Autocomplete;
    if is_autocomplete != (data.kind == InteractionCallbackType::AutocompleteResult) {
        return Err(create_error!(InvalidOperation));
    }

    match data.kind {
        InteractionCallbackType::AutocompleteResult => {
            let choices = data.choices.unwrap_or_default();
            check_choices(&interaction, &choices)?;
            claim(db, &interaction, InteractionStatus::Responded, Default::default()).await?;
            EventV1::InteractionAutocompleteResult {
                id: interaction.id.clone(),
                choices,
            }
            .p(interaction.user_id.clone())
            .await;
        }
        InteractionCallbackType::Modal => {
            if interaction.kind == InteractionType::ModalSubmit {
                return Err(create_error!(InvalidOperation));
            }
            let modal = data.modal.ok_or_else(|| invalid("modal is required"))?;
            modal.validate_modal().map_err(invalid)?;
            claim(
                db,
                &interaction,
                InteractionStatus::Responded,
                InteractionTransition {
                    modal: Some(modal.clone()),
                    ..Default::default()
                },
            )
            .await?;
            EventV1::InteractionModal {
                id: interaction.id.clone(),
                channel_id: interaction.channel_id.clone(),
                modal,
            }
            .p(interaction.user_id.clone())
            .await;
            state_update(&interaction, InteractionState::Responded, None)
                .p(interaction.user_id.clone())
                .await;
        }
        InteractionCallbackType::UpdateMessage => {
            if interaction.source.is_none() {
                return Err(create_error!(InvalidOperation));
            }
            let content = data.data.ok_or_else(|| invalid("data is required"))?;
            validate_response(db, &interaction, &content, false).await?;
            claim(
                db,
                &interaction,
                InteractionStatus::Responded,
                InteractionTransition {
                    updates_source: Some(true),
                    ..Default::default()
                },
            )
            .await?;
            update_source(db, &interaction, content).await?;
            state_update(&interaction, InteractionState::Responded, None)
                .p(interaction.user_id.clone())
                .await;
        }
        InteractionCallbackType::DeferredUpdate => {
            if interaction.source.is_none() {
                return Err(create_error!(InvalidOperation));
            }
            claim(
                db,
                &interaction,
                InteractionStatus::Deferred,
                InteractionTransition {
                    updates_source: Some(true),
                    ..Default::default()
                },
            )
            .await?;
            state_update(&interaction, InteractionState::DeferredUpdate, None)
                .p(interaction.user_id.clone())
                .await;
        }
        InteractionCallbackType::DeferredMessage => {
            let ephemeral = data.data.is_some_and(|d| d.ephemeral);
            claim(
                db,
                &interaction,
                InteractionStatus::Deferred,
                InteractionTransition {
                    ephemeral: Some(ephemeral),
                    ..Default::default()
                },
            )
            .await?;
            state_update(&interaction, InteractionState::Deferred, None)
                .p(interaction.user_id.clone())
                .await;
        }
        InteractionCallbackType::Message => {
            let content = data.data.ok_or(create_error!(EmptyMessage))?;
            respond(db, amqp, interaction, content, &[InteractionStatus::Pending]).await?;
        }
    }

    Ok(())
}

/// Deliver a new message as the answer, claiming the interaction first so it happens once
pub async fn respond(
    db: &Database,
    amqp: &AMQP,
    interaction: Interaction,
    data: InteractionResponseData,
    from: &[InteractionStatus],
) -> Result<()> {
    validate_response(db, &interaction, &data, true).await?;

    // A deferred answer keeps the visibility chosen when deferring
    let ephemeral = if interaction.status == InteractionStatus::Deferred {
        interaction.ephemeral
    } else {
        data.ephemeral
    };

    if ephemeral {
        let message = ephemeral_message(&interaction, data)?;
        if !db
            .transition_interaction(
                &interaction.id,
                from,
                InteractionStatus::Responded,
                InteractionTransition {
                    ephemeral: Some(true),
                    ephemeral_message: Some(message.clone()),
                    ..Default::default()
                },
            )
            .await?
        {
            return Err(create_error!(InteractionAlreadyAcknowledged));
        }
        let message_id = message.id.clone();
        EventV1::EphemeralMessageCreate { message }
            .p(interaction.user_id.clone())
            .await;
        state_update(&interaction, InteractionState::Responded, Some(message_id))
            .p(interaction.user_id.clone())
            .await;
        return Ok(());
    }

    if !db
        .transition_interaction(
            &interaction.id,
            from,
            InteractionStatus::Responded,
            InteractionTransition {
                ephemeral: Some(false),
                ..Default::default()
            },
        )
        .await?
    {
        return Err(create_error!(InteractionAlreadyAcknowledged));
    }

    let message = send_public_message(db, amqp, &interaction, data).await?;
    db.transition_interaction(
        &interaction.id,
        &[InteractionStatus::Responded],
        InteractionStatus::Responded,
        InteractionTransition {
            original_message_id: Some(message.id.clone()),
            ..Default::default()
        },
    )
    .await?;
    state_update(&interaction, InteractionState::Responded, Some(message.id))
        .p(interaction.user_id.clone())
        .await;
    Ok(())
}

/// # Edit Original Response
///
/// Finish a deferred interaction, or edit the answer, until the token expires.
/// After `UpdateMessage` or `DeferredUpdate` this edits the component's message.
#[openapi(tag = "Interactions")]
#[patch("/<interaction_id>/<token>/original", data = "<data>")]
pub async fn edit_original_response(
    db: &State<Database>,
    amqp: &State<AMQP>,
    interaction_id: &str,
    token: &str,
    data: Json<InteractionResponseData>,
) -> Result<EmptyResponse> {
    let interaction = authorised_interaction(db, interaction_id, token).await?;
    let data = data.into_inner();

    match interaction.status {
        InteractionStatus::Pending => Err(create_error!(InvalidOperation)),
        InteractionStatus::Deferred if interaction.updates_source => {
            validate_response(db, &interaction, &data, false).await?;
            if !db
                .transition_interaction(
                    &interaction.id,
                    &[InteractionStatus::Deferred],
                    InteractionStatus::Responded,
                    Default::default(),
                )
                .await?
            {
                return Err(create_error!(InteractionAlreadyAcknowledged));
            }
            update_source(db, &interaction, data).await?;
            state_update(&interaction, InteractionState::Responded, None)
                .p(interaction.user_id.clone())
                .await;
            Ok(EmptyResponse)
        }
        InteractionStatus::Deferred => {
            respond(db, amqp, interaction, data, &[InteractionStatus::Deferred]).await?;
            Ok(EmptyResponse)
        }
        InteractionStatus::Responded => {
            validate_response(db, &interaction, &data, false).await?;
            if interaction.updates_source {
                update_source(db, &interaction, data).await?;
            } else if let Some(message) = interaction.ephemeral_message.clone() {
                edit_ephemeral(db, &interaction, &message.id, data).await?;
            } else if let Some(message_id) = &interaction.original_message_id {
                edit_public_message(db, &interaction, message_id, data, true).await?;
            } else {
                // Nothing to edit: a form, suggestions, or a message still being created
                return Err(create_error!(InvalidOperation));
            }
            Ok(EmptyResponse)
        }
    }
}
