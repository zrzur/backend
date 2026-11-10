use iso8601_timestamp::Timestamp;
use revolt_database::{
    tasks,
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, Message, PartialMessage, User,
};
use revolt_models::v0::{self, Embed};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

/// # Edit Message
///
/// Edits a message that you've previously sent.
#[openapi(tag = "Messaging")]
#[patch("/<target>/messages/<msg>", data = "<edit>")]
pub async fn edit(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
    edit: Json<v0::DataEditMessage>,
) -> Result<Json<v0::Message>> {
    let edit = edit.into_inner();
    edit.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    Message::validate_sum(
        &edit.content,
        edit.embeds.as_deref().unwrap_or_default(),
        user.limits().await.message_length,
    )?;

    // Ensure we have permissions to send a message
    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;

    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
    revolt_database::util::threads::assert_can_write(db, &channel, &permissions).await?;

    let mut message = msg.as_message_in_channel(db, channel.id()).await?;
    // A forwarded copy shows someone else's words: it can be deleted, not
    // edited. A poll can't change under the people who voted on it.
    if message.author != user.id || message.forwarded.is_some() || message.poll.is_some() {
        return Err(create_error!(CannotEditMessage));
    }

    message.edited = Some(Timestamp::now_utc());
    let mut partial = PartialMessage {
        edited: message.edited,
        ..Default::default()
    };

    // AutoMod checks the new text too
    if let (Some(content), Some(server_id)) = (&edit.content, channel.server()) {
        let server = db.fetch_server(server_id).await?;
        if server.automod.as_ref().is_some_and(|rules| !rules.is_empty()) {
            let member = db.fetch_member(server_id, &user.id).await.ok();
            revolt_database::util::automod::check_message(
                db,
                &server,
                channel.id(),
                &user,
                member.as_ref(),
                content,
                revolt_database::util::automod::Check::Edit,
            )
            .await?;
        }
    }

    // 1. Handle content update
    if let Some(content) = &edit.content {
        partial.content = Some(content.clone());
    }

    // 2. Clear any auto generated embeds
    let mut new_embeds: Vec<Embed> = vec![];
    if let Some(embeds) = &message.embeds {
        for embed in embeds {
            if let Embed::Text(embed) = embed {
                new_embeds.push(Embed::Text(embed.clone()))
            }
        }
    }

    // 3. Replace if we are given new embeds
    if let Some(embeds) = edit.embeds {
        // Ensure we have permissions to send embeds
        permissions.throw_if_lacking_channel_permission(ChannelPermission::SendEmbeds)?;
        Message::validate_embed_count(embeds.len()).await?;

        new_embeds.clear();

        for embed in embeds {
            new_embeds.push(message.create_embed(db, embed).await?);
        }
    }

    partial.embeds = Some(new_embeds);

    // 4. Replace components; only bots send them, an empty list removes them
    if let Some(components) = edit.components {
        if user.bot.is_none() {
            return Err(create_error!(InvalidOperation));
        }
        v0::validate_message_components(
            &components,
            &message
                .attachments
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|file| file.id.clone())
                .collect::<Vec<_>>(),
        )
            .map_err(|error| create_error!(FailedValidation { error }))?;
        partial.components = Some(components);
    }

    message.update(db, partial, vec![]).await?;

    // Queue up a task for processing embeds if the we have sufficient permissions
    if permissions.has_channel_permission(ChannelPermission::SendEmbeds) {
        if let Some(content) = edit.content {
            tasks::process_embeds::queue(
                message.channel.to_string(),
                message.id.to_string(),
                content,
            )
            .await;
        }
    }

    Ok(Json(message.into_model(None, None)))
}
