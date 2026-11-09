use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::{
    util::idempotency::IdempotencyKey, util::reference::Reference, Database, File, User, AMQP,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// # Forward Message
///
/// Sends a copy of a message from another channel into this one, with its
/// text, files and rich embeds, and a note of where it came from.
///
/// You need to be able to read the original (view the channel and read its
/// history) and to send here, with the same rules as any message you send
/// (files need Upload Files, rich embeds need Send Embeds). A message from a
/// mature channel goes only into another mature channel. A forwarded copy
/// notifies nobody it mentions.
#[openapi(tag = "Messaging")]
#[post("/<target>/messages/forward", data = "<data>")]
pub async fn forward(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataMessageForward>,
    idempotency: IdempotencyKey,
) -> Result<Json<v0::Message>> {
    if !config().await.features.message_forwarding_enabled {
        return Err(create_error!(FeatureDisabled {
            feature: "message_forwarding".to_string()
        }));
    }

    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // People forward; bots post their own messages
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    // The original: readable by this user, or as good as missing
    let source = Reference::from_unchecked(&data.channel)
        .as_channel(db)
        .await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&source);
    let source_permissions = calculate_channel_permissions(&mut query).await;
    if !source_permissions.has_channel_permission(ChannelPermission::ViewChannel)
        || !source_permissions.has_channel_permission(ChannelPermission::ReadMessageHistory)
    {
        return Err(create_error!(NotFound));
    }
    revolt_database::util::age::assert_can_view_channel(db, &user, &source).await?;

    let original = Reference::from_unchecked(&data.message)
        .as_message(db)
        .await?;
    if original.channel != source.id() {
        return Err(create_error!(NotFound));
    }

    // Join notes, pins and other system lines are not messages to pass on
    if original.system.is_some() {
        return Err(create_error!(InvalidOperation));
    }

    // Mature content stays among adults: only into another mature channel
    let target_channel = target.as_channel(db).await?;
    if revolt_database::util::age::is_mature_channel(db, &source).await?
        && !revolt_database::util::age::is_mature_channel(db, &target_channel).await?
    {
        return Err(create_error!(ForwardMatureContent));
    }

    // Before copying anything: may this user post here at all (the full set
    // of send rules runs again below)
    let mut target_query = DatabasePermissionQuery::new(db, &user).channel(&target_channel);
    let target_permissions = calculate_channel_permissions(&mut target_query).await;
    target_permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
    if original.attachments.as_ref().is_some_and(|files| !files.is_empty()) {
        target_permissions.throw_if_lacking_channel_permission(ChannelPermission::UploadFiles)?;
    }

    // Files: a fresh record of the same stored file for each, claimed by the
    // new message like an upload, so the Upload Files and size rules of the
    // target apply. Files removed or reported since are left out.
    let mut attachments = Vec::new();
    for file in original.attachments.iter().flatten() {
        if let Ok(current) = db.fetch_attachment("attachments", &file.id).await {
            if current.deleted == Some(true) || current.reported == Some(true) {
                continue;
            }
            attachments.push(File::copy_unused(db, &current, &user.id).await?.id);
        }
    }

    // Rich embeds are sent again (they need Send Embeds); link previews are
    // made again from the text where embeds are allowed
    let mut embeds = Vec::new();
    for embed in original.embeds.iter().flatten() {
        if let v0::Embed::Text(text) = embed {
            embeds.push(sendable_from_text(db, text, &user.id).await?);
        }
    }

    let forwarded = v0::MessageForward {
        channel: source.id().to_string(),
        server: source.server().map(str::to_string),
        message: original.id.clone(),
        author: original.author.clone(),
        sent_at: sent_at(&original.id),
        note: data.note.filter(|note| !note.trim().is_empty()),
    };

    let send = v0::DataMessageSend {
        nonce: None,
        content: original.content.clone(),
        attachments: Some(attachments).filter(|list| !list.is_empty()),
        replies: None,
        embeds: Some(embeds).filter(|list| !list.is_empty()),
        masquerade: None,
        interactions: None,
        components: None,
        interaction: None,
        flags: None,
    };

    super::message_send::send_inner(
        db,
        amqp,
        user,
        Reference::from_unchecked(target_channel.id()),
        send,
        idempotency,
        false,
        Some(forwarded),
    )
    .await
}

/// When a message was sent, read from its id
fn sent_at(id: &str) -> Timestamp {
    ulid::Ulid::from_string(id)
        .ok()
        .and_then(|ulid| {
            Timestamp::UNIX_EPOCH.checked_add(iso8601_timestamp::Duration::milliseconds(
                ulid.timestamp_ms() as i64,
            ))
        })
        .unwrap_or_else(Timestamp::now_utc)
}

/// A rich embed as it would be sent, with copies of its images
async fn sendable_from_text(
    db: &Database,
    text: &v0::Text,
    user_id: &str,
) -> Result<v0::SendableEmbed> {
    async fn copy(db: &Database, file: &Option<v0::File>, user_id: &str) -> Result<Option<String>> {
        let Some(file) = file else {
            return Ok(None);
        };
        match db.fetch_attachment("attachments", &file.id).await {
            Ok(current) if current.deleted != Some(true) && current.reported != Some(true) => {
                Ok(Some(File::copy_unused(db, &current, user_id).await?.id))
            }
            _ => Ok(None),
        }
    }

    Ok(v0::SendableEmbed {
        icon_url: text.icon_url.clone(),
        url: text.url.clone(),
        title: text.title.clone(),
        description: text.description.clone(),
        media: copy(db, &text.media, user_id).await?,
        colour: text.colour.clone(),
        author: text.author.clone(),
        fields: text.fields.clone(),
        thumbnail: copy(db, &text.thumbnail, user_id).await?,
        footer: text.footer.clone(),
        timestamp: text.timestamp,
        image_url: text.image_url.clone(),
        thumbnail_url: text.thumbnail_url.clone(),
    })
}
