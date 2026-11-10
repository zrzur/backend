//! Sending a message as a webhook. Execute, Slack and GitHub all end here,
//! so every way in has the same permission, limit and AutoMod checks.

use std::collections::HashMap;

use revolt_config::config;
use revolt_database::{
    events::client::EventV1,
    tasks::process_embeds::queue,
    util::{automod, idempotency::IdempotencyKey, masquerade::masquerade_name_taken, threads},
    Channel, Database, Interactions, Message, PartialChannel, ThreadInformation, Webhook, AMQP,
};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, PermissionValue};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::{r#gen::OpenApiGenerator, response::OpenApiResponderInner};
use rocket::{
    http::Status,
    response::{self, Responder},
    serde::json::Json,
    Request,
};
use validator::Validate;

use crate::util::webhooks::{avatar_url_problem, WebhookCaller};

/// Answer of an execute route: the message with `wait=true`, else 204
pub struct MaybeMessage(pub Option<v0::Message>);

impl<'r> Responder<'r, 'static> for MaybeMessage {
    fn respond_to(self, request: &'r Request<'_>) -> response::Result<'static> {
        match self.0 {
            Some(message) => Json(message).respond_to(request),
            None => Status::NoContent.respond_to(request),
        }
    }
}

impl OpenApiResponderInner for MaybeMessage {
    fn responses(
        generator: &mut OpenApiGenerator,
    ) -> revolt_rocket_okapi::Result<revolt_rocket_okapi::revolt_okapi::openapi3::Responses> {
        let mut responses = Json::<v0::Message>::responses(generator)?;
        revolt_rocket_okapi::util::ensure_status_code_exists(&mut responses, 204);
        Ok(responses)
    }
}

/// Where a message goes and how it is sent, apart from its content
#[derive(Default)]
pub struct Delivery {
    /// An existing thread of the webhook's channel
    pub thread_id: Option<String>,
    /// Forum: a new thread with this name
    pub thread_name: Option<String>,
    /// Forum: tags of the new thread
    pub applied_tags: Vec<String>,
    /// Which mentions the request lets through
    pub allowed_mentions: Option<v0::AllowedMentions>,
    /// No link previews
    pub suppress_embeds: bool,
}

/// The webhook body in Zrzur's message form. `uploads` are the files of this
/// request as (file name, file id).
pub fn message_from_body(
    body: v0::DataWebhookExecute,
    uploads: &[(String, String)],
) -> Result<(v0::DataMessageSend, Delivery)> {
    let invalid = |error: String| create_error!(FailedValidation { error });

    let embeds = body
        .embeds
        .map(|embeds| {
            embeds
                .into_iter()
                .map(|embed| {
                    embed.into_sendable(|name| {
                        uploads
                            .iter()
                            .find(|(file, _)| file == name)
                            .map(|(_, id)| id.clone())
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(invalid)?;

    // ids named in the body, then the files sent with it
    let mut attachments: Vec<String> = body
        .attachments
        .unwrap_or_default()
        .into_iter()
        .filter_map(|attachment| match attachment {
            v0::WebhookAttachment::Id(id) => Some(id),
            v0::WebhookAttachment::Described { .. } => None,
        })
        .collect();
    let in_embeds: Vec<&String> = embeds
        .iter()
        .flatten()
        .flat_map(|embed| embed.media.iter().chain(embed.thumbnail.iter()))
        .collect();
    attachments.extend(
        uploads
            .iter()
            .map(|(_, id)| id)
            .filter(|id| !in_embeds.contains(id))
            .cloned(),
    );

    // `username` and `avatar_url` win over a masquerade in the same request
    let mut masquerade = body.masquerade;
    if body.username.is_some() || body.avatar_url.is_some() {
        let base = masquerade.take();
        masquerade = Some(v0::Masquerade {
            name: body.username.or(base.as_ref().and_then(|m| m.name.clone())),
            avatar: body.avatar_url.or(base.as_ref().and_then(|m| m.avatar.clone())),
            colour: base.and_then(|m| m.colour),
        });
    }

    // In the Discord-style flags 4 is "no previews" and 4096 "silent"; in
    // Zrzur's own 2 is "silent" and 4 "mention everyone". A webhook never asks
    // for a mass ping by flag: `@everyone` in the text does that.
    let flags = body.flags.unwrap_or(0);
    let silent = flags & v0::WEBHOOK_FLAG_SILENT != 0
        || flags & (1 << v0::MessageFlags::SuppressNotifications as u32) != 0;

    Ok((
        v0::DataMessageSend {
            nonce: None,
            content: body.content,
            attachments: (!attachments.is_empty()).then_some(attachments),
            replies: body.replies,
            embeds,
            masquerade,
            interactions: body.interactions,
            components: body.components,
            interaction: None,
            poll: None,
            flags: silent.then_some(1 << v0::MessageFlags::SuppressNotifications as u32),
        },
        Delivery {
            thread_id: None,
            thread_name: body.thread_name,
            applied_tags: body.applied_tags.unwrap_or_default(),
            allowed_mentions: body.allowed_mentions,
            suppress_embeds: flags & v0::WEBHOOK_FLAG_SUPPRESS_EMBEDS != 0,
        },
    ))
}

/// What AutoMod reads of a message: its text and its embeds
fn automod_text(content: Option<&str>, embeds: &[v0::SendableEmbed]) -> String {
    let mut text = content.unwrap_or_default().to_string();
    for embed in embeds {
        text.push('\n');
        text.push_str(&embed.visible_text());
    }
    text
}

/// Run the server's AutoMod rules on a webhook's message or edit
pub async fn check_automod(
    db: &Database,
    webhook: &Webhook,
    channel: &Channel,
    content: Option<&str>,
    embeds: &[v0::SendableEmbed],
    check: automod::Check,
) -> Result<()> {
    if let Some(server_id) = channel.server() {
        let server = db.fetch_server(server_id).await?;
        automod::check_webhook_message(
            db,
            &server,
            channel.id(),
            &webhook.id,
            &webhook.creator_id,
            &automod_text(content, embeds),
            check,
        )
        .await?;
    }
    Ok(())
}

/// Find the channel a message goes to: a thread if the request names one
async fn target_channel(db: &Database, webhook: &Webhook, delivery: &Delivery) -> Result<Target> {
    let channel = db.fetch_channel(&webhook.channel_id).await?;
    let forum = matches!(channel, Channel::TextChannel { forum: Some(_), .. });

    if let Some(thread_id) = &delivery.thread_id {
        let thread = db.fetch_channel(thread_id).await?;
        let of_this_channel = matches!(
            &thread,
            Channel::TextChannel { parent: Some(parent), thread: Some(_), .. }
                if *parent == webhook.channel_id
        );
        if !of_this_channel {
            return Err(create_error!(UnknownChannel));
        }
        return Ok(Target::Existing(thread));
    }

    if let Some(name) = &delivery.thread_name {
        if !forum {
            return Err(create_error!(FailedValidation {
                error: "thread_name is for forum channels".to_string()
            }));
        }
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(create_error!(FailedValidation {
                error: "thread_name must be 1 to 100 characters".to_string()
            }));
        }
        return Ok(Target::NewThread {
            forum_channel: channel,
            name: name.to_string(),
        });
    }

    if forum {
        return Err(create_error!(ForumThreadRequired));
    }
    Ok(Target::Existing(channel))
}

enum Target {
    Existing(Channel),
    NewThread { forum_channel: Channel, name: String },
}

/// A thread right needs both the webhook and its creator to have it
pub async fn check_thread_right(
    db: &Database,
    webhook: &Webhook,
    parent: &Channel,
    right: ChannelPermission,
) -> Result<()> {
    PermissionValue::from(webhook.permissions).throw_if_lacking_channel_permission(right)?;
    threads::webhook_creator_permissions(db, &webhook.creator_id, parent)
        .await?
        .throw_if_lacking_channel_permission(right)
}

/// Make the forum thread a webhook's first message goes into
async fn create_thread(
    db: &Database,
    webhook: &Webhook,
    forum_channel: &Channel,
    name: String,
    tags: &[String],
) -> Result<Channel> {
    let Channel::TextChannel {
        id: parent_id,
        server,
        nsfw,
        forum: Some(forum),
        ..
    } = forum_channel
    else {
        return Err(create_error!(InvalidOperation));
    };

    let mut tags = tags.to_vec();
    tags.dedup();
    if tags.len() > 5 || tags.iter().any(|tag| !forum.tags.iter().any(|t| &t.id == tag)) {
        return Err(create_error!(FailedValidation {
            error: "applied_tags must be up to 5 tags of this forum".to_string()
        }));
    }
    if forum.require_tag && tags.is_empty() {
        return Err(create_error!(FailedValidation {
            error: "this forum requires a tag in applied_tags".to_string()
        }));
    }

    let thread = Channel::TextChannel {
        id: ulid::Ulid::new().to_string(),
        server: server.clone(),
        name,
        description: None,
        icon: None,
        last_message_id: None,
        default_permissions: None,
        role_permissions: HashMap::new(),
        member_permissions: HashMap::new(),
        nsfw: *nsfw,
        voice: None,
        slowmode: None,
        announcement: false,
        permissions_synced: false,
        parent: Some(parent_id.clone()),
        thread: Some(ThreadInformation {
            owner: webhook.creator_id.clone(),
            tags,
            ..Default::default()
        }),
        forum: None,
    };
    db.insert_channel(&thread).await?;
    EventV1::ChannelCreate(thread.clone().into())
        .p(server.clone())
        .await;
    Ok(thread)
}

/// Check and send one webhook message
pub async fn deliver(
    db: &Database,
    amqp: &AMQP,
    caller: &WebhookCaller<'_>,
    webhook: &Webhook,
    mut data: v0::DataMessageSend,
    delivery: Delivery,
    idempotency: IdempotencyKey,
) -> Result<Message> {
    // A webhook's name override may be longer than a person's masquerade, and
    // its avatar address too; both are checked below
    let masquerade = data.masquerade.take();
    let invalid = |error: validator::ValidationErrors| {
        revolt_database::util::validation::validation_failed(&error)
    };
    data.validate().map_err(invalid)?;
    if let Some(masquerade) = masquerade {
        v0::Masquerade {
            name: None,
            avatar: None,
            ..masquerade.clone()
        }
        .validate()
        .map_err(invalid)?;
        data.masquerade = Some(masquerade);
    }

    let permissions: PermissionValue = webhook.permissions.into();
    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;

    if data.attachments.as_ref().is_some_and(|v| !v.is_empty()) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::UploadFiles)?;
    }
    if data.embeds.as_ref().is_some_and(|v| !v.is_empty()) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::SendEmbeds)?;
    }

    if let Some(masquerade) = &data.masquerade {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::Masquerade)?;
        if let Some(name) = &masquerade.name {
            revolt_database::assert_webhook_name(db, name).await?;
        }
        if let Some(problem) = masquerade.avatar.as_deref().and_then(avatar_url_problem) {
            return Err(create_error!(FailedValidation {
                error: problem.to_string()
            }));
        }
    }

    if let Some(interactions) = &data.interactions {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::React)?;
        let interactions: Interactions = interactions.clone().into();
        interactions.validate(db, &permissions).await?;
    }

    // Webhooks from before the name rules: an old name that passes for
    // Zrzur stays refused, whatever else it may do
    if revolt_database::passes_for_official(&webhook.name) {
        return Err(create_error!(InvalidUsername));
    }

    let target = target_channel(db, webhook, &delivery).await?;
    let channel = match &target {
        Target::Existing(channel) => channel,
        Target::NewThread { forum_channel, .. } => forum_channel,
    };

    if let (Some(name), Some(server)) = (
        data.masquerade.as_ref().and_then(|m| m.name.as_deref()),
        channel.server(),
    ) {
        if masquerade_name_taken(db, server, &webhook.id, name).await? {
            return Err(create_error!(MasqueradeNameTaken));
        }
    }

    // A locked thread is for people who can manage messages
    if let Channel::TextChannel {
        thread: Some(thread),
        ..
    } = channel
    {
        if thread.locked {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;
        }
    }

    // Threads and forums close for a webhook as for its creator
    match &target {
        Target::NewThread { forum_channel, .. } => {
            let server_id = forum_channel
                .server()
                .ok_or_else(|| create_error!(InvalidOperation))?;
            threads::assert_feature_on(&db.fetch_server(server_id).await?, forum_channel)?;
            check_thread_right(db, webhook, forum_channel, ChannelPermission::CreateForumPosts)
                .await?;
        }
        Target::Existing(channel) => {
            if let Some(parent) = threads::thread_parent(db, channel).await? {
                threads::assert_not_read_only(db, channel).await?;
                check_thread_right(db, webhook, &parent, ChannelPermission::SendMessageInThreads)
                    .await?;
            }
        }
    }

    check_automod(
        db,
        webhook,
        channel,
        data.content.as_deref(),
        data.embeds.as_deref().unwrap_or_default(),
        automod::Check::Send,
    )
    .await?;

    caller.count_message(webhook).await?;

    let (channel, new_thread) = match target {
        Target::Existing(channel) => (channel, false),
        Target::NewThread { forum_channel, name } => (
            create_thread(db, webhook, &forum_channel, name, &delivery.applied_tags).await?,
            true,
        ),
    };

    let previews =
        !delivery.suppress_embeds && permissions.has_channel_permission(ChannelPermission::SendEmbeds);

    let sent = Message::create_with_mentions(
        db,
        Some(amqp),
        channel.clone(),
        data,
        v0::MessageAuthor::Webhook(&webhook.clone().into()),
        None,
        None,
        config().await.features.limits.default,
        idempotency,
        previews,
        true,
        delivery.allowed_mentions.as_ref(),
    )
    .await;

    let message = match sent {
        Ok(message) => message,
        Err(error) => {
            // no empty thread is left behind
            if new_thread {
                channel.delete(db).await?;
            }
            return Err(error);
        }
    };

    // A message reopens an archived thread and is counted
    if let Channel::TextChannel {
        thread: Some(thread),
        ..
    } = &channel
    {
        let mut next = thread.clone();
        next.archived = false;
        next.message_count = next.message_count.saturating_add(1);
        let mut channel = channel.clone();
        channel
            .update(
                db,
                PartialChannel {
                    thread: Some(next),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
    }

    webhook.touch(db).await;
    Ok(message)
}

/// Queue link previews for edited text, if the webhook may show embeds
pub async fn queue_previews(permissions: &PermissionValue, message: &Message, content: Option<String>) {
    if permissions.has_channel_permission(ChannelPermission::SendEmbeds) {
        if let Some(content) = content {
            queue(message.channel.to_string(), message.id.to_string(), content).await;
        }
    }
}
