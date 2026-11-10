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
/// notifies nobody it mentions. A poll is forwarded as text: its question and
/// answers, without votes.
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

    // Zrzur's own messages are read where they were sent, not passed on
    revolt_database::assert_not_official_message(db, &original).await?;

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

    // Before copying files: may this user post them here at all, so a refusal
    // leaves no copies behind (the full set of send rules runs again below,
    // with its own clearer errors for direct messages)
    let has_files = original.attachments.as_ref().is_some_and(|files| !files.is_empty());
    let has_rich_embeds = original
        .embeds
        .iter()
        .flatten()
        .any(|embed| matches!(embed, v0::Embed::Text(_)));
    if (has_files || has_rich_embeds)
        && !matches!(target_channel, revolt_database::Channel::DirectMessage { .. })
    {
        let mut target_query = DatabasePermissionQuery::new(db, &user).channel(&target_channel);
        let target_permissions = calculate_channel_permissions(&mut target_query).await;
        target_permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
        if has_files {
            target_permissions
                .throw_if_lacking_channel_permission(ChannelPermission::UploadFiles)?;
        }
        if has_rich_embeds {
            target_permissions
                .throw_if_lacking_channel_permission(ChannelPermission::SendEmbeds)?;
        }
    }

    // Files: a fresh record of the same stored file for each, claimed by the
    // new message like an upload, so the Upload Files and size rules of the
    // target apply. Files removed or reported since, and old files without a
    // hash, are left out.
    let mut copies = Vec::new();
    let mut attachments = Vec::new();
    for file in original.attachments.iter().flatten() {
        if let Ok(current) = db.fetch_attachment("attachments", &file.id).await {
            if !can_share(&current) {
                continue;
            }
            let copy = File::copy_unused(db, &current, &user.id).await?.id;
            copies.push(copy.clone());
            attachments.push(copy);
        }
    }

    // Rich embeds are sent again (they need Send Embeds); link previews are
    // made again from the text where embeds are allowed
    let mut embeds = Vec::new();
    for embed in original.embeds.iter().flatten() {
        if let v0::Embed::Text(text) = embed {
            let sendable = sendable_from_text(db, text, &user.id).await?;
            copies.extend(sendable.media.iter().chain(sendable.thumbnail.iter()).cloned());
            embeds.push(sendable);
        }
    }

    // Where it came from: a server channel only, and only when the original
    // was sent under its author's own name (direct messages, groups and
    // masquerades stay private)
    let names_place = matches!(source, revolt_database::Channel::TextChannel { .. })
        && original.masquerade.is_none();
    let forwarded = v0::MessageForward {
        channel: names_place.then(|| source.id().to_string()),
        server: if names_place { source.server().map(str::to_string) } else { None },
        message: original.id.clone(),
        sent_at: sent_at(&original.id),
        note: data.note.filter(|note| !note.trim().is_empty()),
    };

    // A poll goes as text: its question and answers, without votes (they
    // belong to the original, and nobody could end the copy)
    let content = match (&original.content, &original.poll) {
        (text, Some(poll)) => {
            let summary = v0::Poll::from(poll.clone()).summary();
            Some(match text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                Some(text) => format!("{text}\n\n{summary}"),
                None => summary,
            })
        }
        (text, None) => text.clone(),
    };

    let send = v0::DataMessageSend {
        nonce: None,
        content,
        attachments: Some(attachments).filter(|list| !list.is_empty()),
        replies: None,
        embeds: Some(embeds).filter(|list| !list.is_empty()),
        masquerade: None,
        interactions: None,
        components: None,
        interaction: None,
        poll: None,
        flags: None,
    };

    let result = super::message_send::send_inner(
        db,
        amqp,
        user,
        Reference::from_unchecked(target_channel.id()),
        send,
        idempotency,
        false,
        Some(forwarded),
    )
    .await;

    // Refused after all: the copies made for it go at once
    if result.is_err() && !copies.is_empty() {
        db.mark_attachments_as_deleted(&copies).await.ok();
    }
    result
}

/// Whether a file can go along: still there, not under report, and stored
/// under a hash (old uploads without one can't be shared)
fn can_share(file: &File) -> bool {
    file.deleted != Some(true) && file.reported != Some(true) && file.hash.is_some()
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
            Ok(current) if can_share(&current) => {
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
