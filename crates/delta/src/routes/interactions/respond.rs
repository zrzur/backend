//! Turning a bot's answer into a channel message, a message update or a private message.

use iso8601_timestamp::Timestamp;
use revolt_database::{
    events::client::EventV1, tasks, util::idempotency::IdempotencyKey,
    util::permissions::DatabasePermissionQuery, Database, Interaction, InteractionSource,
    Message, PartialMessage, AMQP,
};
use revolt_models::v0::{
    self, validate_message_components, DataMessageSend, EphemeralMessage,
    InteractionResponseData, InteractionState,
};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, PermissionQuery};
use revolt_result::{create_error, Result};
use validator::Validate;

use super::options::invalid;

fn message_data(data: &InteractionResponseData) -> DataMessageSend {
    DataMessageSend {
        nonce: None,
        content: data.content.clone(),
        attachments: None,
        replies: None,
        embeds: data.embeds.clone(),
        masquerade: None,
        interactions: None,
        components: data.components.clone(),
        interaction: None,
        poll: None,
        flags: None,
    }
}

/// Look up an interaction by id and token; a wrong token looks like a missing interaction
pub async fn authorised_interaction(db: &Database, id: &str, token: &str) -> Result<Interaction> {
    let interaction = db.fetch_interaction(id).await?;
    if !interaction.token_matches(token) {
        return Err(create_error!(UnknownInteraction));
    }
    if interaction.is_expired() {
        return Err(create_error!(InteractionExpired));
    }
    Ok(interaction)
}

/// Files of the public messages an update of this interaction may change: the
/// message whose component was used, and the bot's first answer.
async fn edited_message_files(db: &Database, interaction: &Interaction) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(InteractionSource::Message { id }) = &interaction.source {
        ids.push(id.clone());
    }
    if let Some(id) = &interaction.original_message_id {
        ids.push(id.clone());
    }
    let mut files = Vec::new();
    for id in ids {
        if let Ok(message) = db.fetch_message(&id).await {
            files.extend(
                message
                    .attachments
                    .unwrap_or_default()
                    .into_iter()
                    .map(|file| file.id),
            );
        }
    }
    files
}

/// Check the content of an answer with the same rules as a normal message.
///
/// A new message must not be empty; an update may change only some parts.
pub async fn validate_response(
    db: &Database,
    interaction: &Interaction,
    data: &InteractionResponseData,
    new_message: bool,
) -> Result<()> {
    message_data(data).validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let bot_user = db.fetch_user(&interaction.application_id).await?;
    Message::validate_sum(
        &data.content,
        data.embeds.as_deref().unwrap_or_default(),
        bot_user.limits().await.message_length,
    )?;
    if let Some(components) = &data.components {
        // New and private answers carry no files for blocks to point at. An
        // update changes a public message that may have files: check against
        // them, like a normal message edit does.
        let files = if new_message {
            Vec::new()
        } else {
            edited_message_files(db, interaction).await
        };
        validate_message_components(components, &files).map_err(invalid)?;
    }

    if new_message
        && data.content.as_ref().is_none_or(|c| c.trim().is_empty())
        && data.embeds.as_ref().is_none_or(|e| e.is_empty())
        && data.components.as_ref().is_none_or(|c| c.is_empty())
    {
        return Err(create_error!(EmptyMessage));
    }
    Ok(())
}

pub fn state_update(
    interaction: &Interaction,
    state: InteractionState,
    message_id: Option<String>,
) -> EventV1 {
    EventV1::InteractionStateUpdate {
        id: interaction.id.clone(),
        channel_id: interaction.channel_id.clone(),
        state,
        message_id,
    }
}

/// Private answer: built here, sent to the invoking user's private topic only
pub fn ephemeral_message(
    interaction: &Interaction,
    data: InteractionResponseData,
) -> Result<EphemeralMessage> {
    let embeds = data.embeds.unwrap_or_default();
    // Media references point at uploaded files and would need their own access
    // checks; private answers do not take them for now.
    if embeds.iter().any(|embed| embed.media.is_some()) {
        return Err(invalid("ephemeral responses cannot attach media"));
    }
    Ok(EphemeralMessage {
        id: ulid::Ulid::new().to_string(),
        interaction_id: interaction.id.clone(),
        author: interaction.application_id.clone(),
        channel_id: interaction.channel_id.clone(),
        content: data.content,
        embeds,
        components: data.components.unwrap_or_default(),
        edited: None,
    })
}

/// Public answer: a normal channel message from the bot, under the bot's own permissions
pub async fn send_public_message(
    db: &Database,
    amqp: &AMQP,
    interaction: &Interaction,
    data: InteractionResponseData,
) -> Result<v0::Message> {
    let bot_user = db.fetch_user(&interaction.application_id).await?;
    let channel = db.fetch_channel(&interaction.channel_id).await?;
    let mut query = DatabasePermissionQuery::new(db, &bot_user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
    revolt_database::util::threads::assert_can_write(db, &channel, &permissions).await?;
    if data.embeds.as_ref().is_some_and(|embeds| !embeds.is_empty()) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::SendEmbeds)?;
    }

    let author: v0::User = bot_user.clone().into(db, Some(&bot_user)).await;
    query.are_we_a_member().await;
    let model_user = bot_user
        .clone()
        .into_known_static(revolt_presence::is_online(&bot_user.id).await)
        .await;
    let model_member: Option<v0::Member> = query
        .member_ref()
        .as_ref()
        .map(|member| member.clone().into_owned().into());

    let message = Message::create_from_api(
        db,
        Some(amqp),
        channel,
        DataMessageSend {
            interaction: Some(v0::MessageInteraction {
                id: interaction.id.clone(),
                kind: interaction.kind,
                user: interaction.user_id.clone(),
                name: interaction.command_name.clone(),
            }),
            ..message_data(&data)
        },
        v0::MessageAuthor::User(&author),
        Some(model_user.clone()),
        model_member.clone(),
        bot_user.limits().await,
        // one public answer per interaction, even if the bot retries
        IdempotencyKey::unchecked_from_string(format!("interaction:{}", interaction.id)),
        permissions.has_channel_permission(ChannelPermission::SendEmbeds),
        true,
    )
    .await?;

    Ok(message.into_model(Some(model_user), model_member))
}

/// Edit a channel message of the bot in the interaction's channel.
///
/// `mark_edited` sets the edit time, so people see "(edited)" and know the
/// message changed (also when a button or menu changed it).
pub async fn edit_public_message(
    db: &Database,
    interaction: &Interaction,
    message_id: &str,
    data: InteractionResponseData,
    mark_edited: bool,
) -> Result<()> {
    let bot_user = db.fetch_user(&interaction.application_id).await?;
    let channel = db.fetch_channel(&interaction.channel_id).await?;
    let mut query = DatabasePermissionQuery::new(db, &bot_user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
    revolt_database::util::threads::assert_can_write(db, &channel, &permissions).await?;

    let mut message = db.fetch_message(message_id).await?;
    if message.author != bot_user.id || message.channel != interaction.channel_id {
        return Err(create_error!(CannotEditMessage));
    }

    if mark_edited {
        message.edited = Some(Timestamp::now_utc());
    }
    let mut partial = PartialMessage {
        edited: if mark_edited { message.edited } else { None },
        content: data.content.clone(),
        components: data.components,
        ..Default::default()
    };
    if let Some(embeds) = data.embeds {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::SendEmbeds)?;
        Message::validate_embed_count(embeds.len()).await?;
        let mut new_embeds = vec![];
        for embed in embeds {
            new_embeds.push(message.create_embed(db, embed).await?);
        }
        partial.embeds = Some(new_embeds);
    }
    message.update(db, partial, vec![]).await?;

    if permissions.has_channel_permission(ChannelPermission::SendEmbeds) {
        if let Some(content) = data.content {
            tasks::process_embeds::queue(message.channel.clone(), message.id.clone(), content)
                .await;
        }
    }
    Ok(())
}

/// Edit a private message kept on `holder` (its first answer or a follow-up)
pub async fn edit_ephemeral(
    db: &Database,
    holder: &Interaction,
    message_id: &str,
    data: InteractionResponseData,
) -> Result<()> {
    let mut message = holder
        .find_ephemeral(message_id)
        .cloned()
        .ok_or(create_error!(UnknownMessage))?;
    if data
        .embeds
        .as_ref()
        .is_some_and(|embeds| embeds.iter().any(|embed| embed.media.is_some()))
    {
        return Err(invalid("ephemeral responses cannot attach media"));
    }
    if let Some(content) = data.content {
        message.content = Some(content);
    }
    if let Some(embeds) = data.embeds {
        message.embeds = embeds;
    }
    if let Some(components) = data.components {
        message.components = components;
    }
    message.edited = Some(Timestamp::now_utc());

    if !db.replace_interaction_ephemeral(&holder.id, &message).await? {
        return Err(create_error!(UnknownMessage));
    }
    EventV1::EphemeralMessageUpdate { message }
        .p(holder.user_id.clone())
        .await;
    Ok(())
}

/// Change the message a component interaction came from
pub async fn update_source(
    db: &Database,
    interaction: &Interaction,
    data: InteractionResponseData,
) -> Result<()> {
    match interaction
        .source
        .as_ref()
        .ok_or(create_error!(InvalidOperation))?
    {
        InteractionSource::Message { id } => {
            edit_public_message(db, interaction, id, data, true).await
        }
        InteractionSource::Ephemeral {
            interaction_id,
            message_id,
        } => {
            let holder = db.fetch_interaction(interaction_id).await?;
            // Only the same user's private message, and only while it lives
            if holder.user_id != interaction.user_id || holder.is_expired() {
                return Err(create_error!(UnknownMessage));
            }
            edit_ephemeral(db, &holder, message_id, data).await
        }
    }
}
