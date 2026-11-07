//! Threads: side conversations of a text channel, and the posts of a forum.
//! A thread uses its parent channel's permissions: Create Threads and Create
//! Forum Posts start one, Send Messages In Threads writes in one (forum posts
//! are threads, so they share it). All come on top of Send Messages, so a
//! channel closed to writing has closed threads, as it always had. Reading
//! never depends on them: what a server switched off stays readable.

use std::collections::HashMap;

use revolt_database::{
    events::client::EventV1,
    util::{idempotency::IdempotencyKey, permissions::DatabasePermissionQuery, reference::Reference},
    Channel, Database, ThreadInformation, User, AMQP,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

/// Threads listed per page
const PAGE: i64 = 50;
const PAGE_MAX: i64 = 100;

/// # Start Thread
///
/// Start a thread in a text channel, from a message or with a first
/// message, or a post in a forum (a title, a first message and its tags).
/// Needs Send Messages in the channel, and Create Threads (Create Forum Posts
/// in a forum). Refused with `ThreadsDisabled` or `ForumsDisabled` when the
/// server has them switched off.
#[openapi(tag = "Threads")]
#[post("/<target>/threads", data = "<data>")]
pub async fn create_thread(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataCreateThread>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let name = data.name.trim().to_string();
    if name.is_empty() {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("name", "required"),
        ]));
    }

    let parent = target.as_channel(db).await?;
    let Channel::TextChannel {
        id: parent_id,
        server,
        voice,
        parent: grandparent,
        forum,
        nsfw,
        ..
    } = &parent
    else {
        return Err(create_error!(InvalidOperation));
    };
    // threads live in text channels and forums, never in voice rooms or
    // other threads
    if voice.is_some() || grandparent.is_some() {
        return Err(create_error!(InvalidOperation));
    }

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&parent);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
    permissions.throw_if_lacking_channel_permission(if forum.is_some() {
        ChannelPermission::CreateForumPosts
    } else {
        ChannelPermission::CreateThreads
    })?;
    revolt_database::util::age::assert_can_view_channel(db, &user, &parent).await?;
    revolt_database::util::threads::assert_feature_on(&db.fetch_server(server).await?, &parent)?;

    let tags = match forum {
        Some(forum) => {
            // a forum post has a first message, no starter message
            if data.content.as_deref().is_none_or(|c| c.trim().is_empty()) || data.message.is_some() {
                return Err(create_error!(InvalidOperation));
            }
            let mut tags = data.tags.clone();
            tags.dedup();
            if tags.len() > 5 || tags.iter().any(|tag| !forum.tags.iter().any(|t| &t.id == tag)) {
                return Err(create_error!(InvalidOperation));
            }
            if forum.require_tag && tags.is_empty() {
                return Err(create_error!(InvalidOperation));
            }
            tags
        }
        None => {
            if !data.tags.is_empty() {
                return Err(create_error!(InvalidOperation));
            }
            vec![]
        }
    };

    // one thread per message, and only from a message of this channel
    if let Some(message_id) = &data.message {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;
        let message = db.fetch_message(message_id).await?;
        if &message.channel != parent_id {
            return Err(create_error!(NotFound));
        }
        let mut before = None;
        loop {
            let page = db.fetch_threads(parent_id, false, before.as_deref(), 100).await?;
            let archived = db.fetch_threads(parent_id, true, before.as_deref(), 100).await?;
            if page.iter().chain(archived.iter()).any(|thread| {
                matches!(thread, Channel::TextChannel { thread: Some(t), .. } if t.starter_message.as_ref() == Some(message_id))
            }) {
                return Err(create_error!(InvalidOperation));
            }
            if page.len() < 100 && archived.len() < 100 {
                break;
            }
            before = page.iter().chain(archived.iter()).map(|c| c.id().to_string()).min();
        }
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
        // mature with its parent, so the age rules carry over
        nsfw: *nsfw,
        voice: None,
        slowmode: None,
        announcement: false,
        permissions_synced: false,
        parent: Some(parent_id.clone()),
        thread: Some(ThreadInformation {
            owner: user.id.clone(),
            starter_message: data.message.clone(),
            tags,
            ..Default::default()
        }),
        forum: None,
    };
    db.insert_channel(&thread).await?;
    // announced to the server; the gateway forwards it to those who can see
    // the parent
    EventV1::ChannelCreate(thread.clone().into())
        .p(server.clone())
        .await;

    if let Some(content) = data.content {
        let first = super::message_send::send(
            db,
            amqp,
            user,
            Reference::from_unchecked(thread.id()),
            Json(v0::DataMessageSend {
                content: Some(content),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                interaction: None,
                flags: None,
                components: None,
            }),
            IdempotencyKey::unchecked_from_string(ulid::Ulid::new().to_string()),
            true,
        )
        .await;
        if let Err(error) = first {
            // the first message was refused (slow mode, AutoMod, ...): no
            // empty thread is left behind
            thread.delete(db).await?;
            return Err(error);
        }
        let thread = db.fetch_channel(thread.id()).await?;
        return Ok(Json(thread.into()));
    }

    Ok(Json(thread.into()))
}

/// # Threads Of A Channel
///
/// Active threads (or archived ones with `archived=true`), newest first,
/// 50 per page (`limit` up to 100); `before` is a thread id.
#[openapi(tag = "Threads")]
#[get("/<target>/threads?<archived>&<before>&<limit>")]
pub async fn list_threads(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    archived: Option<bool>,
    before: Option<String>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::Channel>>> {
    let parent = target.as_channel(db).await?;
    if !matches!(parent, Channel::TextChannel { parent: None, voice: None, .. }) {
        return Err(create_error!(InvalidOperation));
    }
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&parent);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    revolt_database::util::age::assert_can_view_channel(db, &user, &parent).await?;

    let threads = db
        .fetch_threads(
            parent.id(),
            archived.unwrap_or(false),
            before.as_deref(),
            limit.unwrap_or(PAGE).clamp(1, PAGE_MAX),
        )
        .await?;
    Ok(Json(threads.into_iter().map(Into::into).collect()))
}

/// # Edit Thread
///
/// Rename, archive, reopen or tag a thread: its owner or anyone who can
/// manage messages. Locking (only moderators write) needs Manage Messages.
#[openapi(tag = "Threads")]
#[patch("/<target>/thread", data = "<data>")]
pub async fn edit_thread(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataEditThread>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let mut channel = target.as_channel(db).await?;
    let Channel::TextChannel {
        thread: Some(thread),
        parent: Some(parent_id),
        ..
    } = &channel
    else {
        return Err(create_error!(InvalidOperation));
    };
    let (mut next, parent_id) = (thread.clone(), parent_id.clone());

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    let moderator = permissions.has_channel_permission(ChannelPermission::ManageMessages);
    if !moderator && next.owner != user.id {
        return Err(create_error!(MissingPermission {
            permission: "ManageMessages".to_string()
        }));
    }
    // a thread's owner edits it like they write in it; moderators keep order
    // even in a closed or switched-off thread
    if !moderator {
        revolt_database::util::threads::assert_can_write(db, &channel, &permissions).await?;
    }

    if let Some(locked) = data.locked {
        if !moderator {
            return Err(create_error!(MissingPermission {
                permission: "ManageMessages".to_string()
            }));
        }
        next.locked = locked;
    }
    if let Some(archived) = data.archived {
        // a locked thread is reopened by moderators only
        if !archived && next.locked && !moderator {
            return Err(create_error!(MissingPermission {
                permission: "ManageMessages".to_string()
            }));
        }
        next.archived = archived;
    }
    if let Some(tags) = data.tags {
        let parent = db.fetch_channel(&parent_id).await?;
        let Channel::TextChannel {
            forum: Some(forum), ..
        } = &parent
        else {
            return Err(create_error!(InvalidOperation));
        };
        let mut tags = tags;
        tags.dedup();
        if tags.len() > 5
            || tags.iter().any(|tag| !forum.tags.iter().any(|t| &t.id == tag))
            || (forum.require_tag && tags.is_empty())
        {
            return Err(create_error!(InvalidOperation));
        }
        next.tags = tags;
    }

    let name = match data.name.map(|name| name.trim().to_string()) {
        Some(name) if name.is_empty() => {
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::new("name", "required"),
            ]))
        }
        other => other,
    };

    channel
        .update(
            db,
            revolt_database::PartialChannel {
                name,
                thread: Some(next),
                ..Default::default()
            },
            vec![],
        )
        .await?;
    Ok(Json(channel.into()))
}
