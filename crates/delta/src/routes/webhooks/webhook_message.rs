//! A webhook's own messages: read, edit, delete with its token. The short
//! paths without `/messages` stay for tools written before these.

use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_database::{
    util::{automod::Check, reference::Reference},
    Database, Message, PartialMessage, Webhook,
};
use revolt_models::v0::{self, DataEditMessage, Embed};
use revolt_models::validator::Validate;
use revolt_permissions::{ChannelPermission, PermissionValue};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

use super::delivery::{check_automod, queue_previews};
use crate::util::webhooks::WebhookCaller;

async fn own_message(db: &Database, webhook: &Webhook, message_id: Reference<'_>) -> Result<Message> {
    let message = message_id.as_message(db).await?;
    if message.author != webhook.id {
        return Err(create_error!(CannotEditMessage));
    }
    Ok(message)
}

/// # Fetches a webhook message
///
/// Fetches a message this webhook sent
#[openapi(tag = "Webhooks")]
#[get("/<webhook_id>/<token>/messages/<message_id>")]
pub async fn webhook_fetch_message(
    db: &State<Database>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    message_id: Reference<'_>,
) -> Result<Json<v0::Message>> {
    let webhook = caller.authorise(db, webhook_id, &token).await?;
    let message = own_message(db, &webhook, message_id).await?;
    Ok(Json(message.into_model(None, None)))
}

async fn edit_message(
    db: &Database,
    caller: &WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: &str,
    message_id: Reference<'_>,
    data: v0::DataWebhookEditMessage,
) -> Result<v0::Message> {
    let webhook = caller.authorise(db, webhook_id, token).await?;

    let embeds = data
        .embeds
        .map(|embeds| {
            embeds
                .into_iter()
                .map(|embed| embed.into_sendable(|_| None))
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(|error| create_error!(FailedValidation { error }))?;
    let edit = DataEditMessage {
        content: data.content,
        embeds,
        components: data.components,
    };

    edit.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    Message::validate_sum(
        &edit.content,
        edit.embeds.as_deref().unwrap_or_default(),
        config().await.features.limits.default.message_length,
    )?;

    // an edit needs what sending needs: a webhook whose rights were taken
    // away can't change its old messages, nor add embeds by editing
    let permissions: PermissionValue = webhook.permissions.into();
    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
    if let Some(embeds) = &edit.embeds {
        if !embeds.is_empty() {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::SendEmbeds)?;
        }
        Message::validate_embed_count(embeds.len()).await?;
    }

    let mut message = own_message(db, &webhook, message_id).await?;

    let channel = db.fetch_channel(&message.channel).await?;

    // nor can a webhook change its messages in a closed thread
    if let Some(parent) = revolt_database::util::threads::thread_parent(db, &channel).await? {
        revolt_database::util::threads::assert_not_read_only(db, &channel).await?;
        super::delivery::check_thread_right(
            db,
            &webhook,
            &parent,
            ChannelPermission::SendMessageInThreads,
        )
        .await?;
    }

    // what AutoMod refuses in a new message it refuses in an edit
    check_automod(
        db,
        &webhook,
        &channel,
        edit.content.as_deref(),
        edit.embeds.as_deref().unwrap_or_default(),
        Check::Edit,
    )
    .await?;

    // Components: only ones nobody can use, unless a bot made the webhook; an
    // empty list removes them
    if let Some(components) = &edit.components {
        if !v0::components_are_static(components)
            && !db
                .fetch_user(&webhook.creator_id)
                .await
                .is_ok_and(|creator| creator.bot.is_some())
        {
            return Err(create_error!(InvalidOperation));
        }
        v0::validate_message_components(
            components,
            &message
                .attachments
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|file| file.id.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| create_error!(FailedValidation { error }))?;
    }

    caller.count_message(&webhook).await?;

    message.edited = Some(Timestamp::now_utc());
    let mut partial = PartialMessage {
        edited: message.edited,
        ..Default::default()
    };

    // 1. Handle content update. Mentions are not read again: an edit never
    // notifies anyone
    if let Some(content) = &edit.content {
        partial.content = Some(content.clone());
    }

    // 2. Clear any auto generated embeds
    let mut new_embeds = vec![];
    if let Some(embeds) = &message.embeds {
        for embed in embeds {
            if let Embed::Text(embed) = embed {
                new_embeds.push(Embed::Text(embed.clone()))
            }
        }
    }

    // 3. Replace if we are given new embeds
    if let Some(embeds) = edit.embeds {
        new_embeds.clear();

        for embed in embeds {
            new_embeds.push(message.create_embed(db, embed).await?);
        }
    }

    partial.embeds = Some(new_embeds);

    // 4. Replace components
    partial.components = edit.components;

    message.update(db, partial, vec![]).await?;

    // Link previews only for webhooks allowed to show embeds
    queue_previews(&permissions, &message, edit.content).await;

    webhook.touch(db).await;
    Ok(message.into_model(None, None))
}

/// # Edits a webhook message
///
/// Edits a message this webhook sent
#[openapi(tag = "Webhooks")]
#[patch("/<webhook_id>/<token>/messages/<message_id>", data = "<data>")]
pub async fn webhook_edit_message(
    db: &State<Database>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    message_id: Reference<'_>,
    data: Json<v0::DataWebhookEditMessage>,
) -> Result<Json<v0::Message>> {
    edit_message(db, &caller, webhook_id, &token, message_id, data.into_inner())
        .await
        .map(Json)
}

/// # Edits a webhook message (short path)
///
/// As `/messages/{message_id}`
#[openapi(tag = "Webhooks")]
#[patch("/<webhook_id>/<token>/<message_id>", data = "<data>", rank = 2)]
pub async fn webhook_edit_message_short(
    db: &State<Database>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    message_id: Reference<'_>,
    data: Json<v0::DataWebhookEditMessage>,
) -> Result<Json<v0::Message>> {
    edit_message(db, &caller, webhook_id, &token, message_id, data.into_inner())
        .await
        .map(Json)
}

async fn delete_message(
    db: &Database,
    caller: &WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: &str,
    message_id: Reference<'_>,
) -> Result<EmptyResponse> {
    let webhook = caller.authorise(db, webhook_id, token).await?;

    let message = message_id.as_message(db).await?;
    if message.author != webhook.id {
        return Err(create_error!(CannotDeleteMessage));
    }

    message.delete(db).await.map(|_| EmptyResponse)
}

/// # Deletes a webhook message
///
/// Deletes a message this webhook sent
#[openapi(tag = "Webhooks")]
#[delete("/<webhook_id>/<token>/messages/<message_id>")]
pub async fn webhook_delete_message(
    db: &State<Database>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    message_id: Reference<'_>,
) -> Result<EmptyResponse> {
    delete_message(db, &caller, webhook_id, &token, message_id).await
}

/// # Deletes a webhook message (short path)
///
/// As `/messages/{message_id}`
#[openapi(tag = "Webhooks")]
#[delete("/<webhook_id>/<token>/<message_id>", rank = 2)]
pub async fn webhook_delete_message_short(
    db: &State<Database>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    message_id: Reference<'_>,
) -> Result<EmptyResponse> {
    delete_message(db, &caller, webhook_id, &token, message_id).await
}
