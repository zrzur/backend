use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{delete_voice_channel, get_voice_channel_members, UserVoiceChannel, VoiceClient},
    AuditLogEntryAction, Channel, Database, FieldsChannel, File, PartialChannel, SystemMessage,
    User, AMQP,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// # Edit Channel
///
/// Edit a channel object by its id.
#[openapi(tag = "Channel Information")]
#[patch("/<target>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    amqp: &State<AMQP>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    data: Json<v0::DataEditChannel>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    if let Some(voice) = &data.voice {
        voice.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    }

    let mut channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;

    if data.name.is_none()
        && data.description.is_none()
        && data.icon.is_none()
        && data.nsfw.is_none()
        && data.owner.is_none()
        && data.voice.is_none()
        && data.slowmode.is_none()
        && data.announcement.is_none()
        && data.forum_tags.is_none()
        && data.require_tag.is_none()
        && data.remove.is_empty()
    {
        return Ok(Json(channel.into()));
    }

    let mut partial: PartialChannel = Default::default();

    // Transfer group ownership
    if let Some(new_owner) = data.owner {
        if let Channel::Group {
            owner, recipients, ..
        } = &mut channel
        {
            // Make sure we are the owner of this group
            if owner != &user.id {
                return Err(create_error!(NotOwner));
            }

            // Ensure user is part of group
            if !recipients.contains(&new_owner) {
                return Err(create_error!(NotInGroup));
            }

            // Ensure new owner is not a bot
            let new_owner_user = db.fetch_user(&new_owner).await?;
            if new_owner_user.bot.is_some() {
                return Err(create_error!(IsBot))
            }

            // Transfer ownership
            partial.owner = Some(new_owner.to_string());
            let old_owner = std::mem::replace(owner, new_owner.to_string());

            // Notify clients
            SystemMessage::ChannelOwnershipChanged {
                from: old_owner,
                to: new_owner,
            }
        } else {
            return Err(create_error!(InvalidOperation));
        }
        .into_message(channel.id().to_string())
        .send(
            db,
            Some(amqp),
            user.as_author_for_system(),
            None,
            None,
            &channel,
            false,
        )
        .await
        .ok();
    }

    let before_channel = channel.clone();

    match &mut channel {
        Channel::Group {
            id,
            name,
            description,
            icon,
            nsfw,
            ..
        } => {
            if data.remove.contains(&v0::FieldsChannel::Icon) {
                if let Some(icon) = &icon {
                    db.mark_attachment_as_deleted(&icon.id).await?;
                }
            }

            for field in &data.remove {
                match field {
                    v0::FieldsChannel::Description => {
                        description.take();
                    }
                    v0::FieldsChannel::Icon => {
                        icon.take();
                    }
                    _ => {}
                }
            }

            if let Some(icon_id) = data.icon {
                partial.icon = Some(File::use_channel_icon(db, &icon_id, id, &user.id).await?);
                *icon = partial.icon.clone();
            }

            if let Some(new_name) = data.name {
                *name = new_name.clone();
                partial.name = Some(new_name);
            }

            if let Some(new_description) = data.description {
                partial.description = Some(new_description);
                *description = partial.description.clone();
            }

            if let Some(new_nsfw) = data.nsfw {
                *nsfw = new_nsfw;
                partial.nsfw = Some(new_nsfw);
            }

            // Send out mutation system messages.
            if let Some(name) = &partial.name {
                SystemMessage::ChannelRenamed {
                    name: name.to_string(),
                    by: user.id.clone(),
                }
                .into_message(channel.id().to_string())
                .send(
                    db,
                    Some(amqp),
                    user.as_author_for_system(),
                    None,
                    None,
                    &channel,
                    false,
                )
                .await
                .ok();
            }

            if partial.description.is_some() {
                SystemMessage::ChannelDescriptionChanged {
                    by: user.id.clone(),
                }
                .into_message(channel.id().to_string())
                .send(
                    db,
                    Some(amqp),
                    user.as_author_for_system(),
                    None,
                    None,
                    &channel,
                    false,
                )
                .await
                .ok();
            }

            if partial.icon.is_some() {
                SystemMessage::ChannelIconChanged {
                    by: user.id.clone(),
                }
                .into_message(channel.id().to_string())
                .send(
                    db,
                    Some(amqp),
                    user.as_author_for_system(),
                    None,
                    None,
                    &channel,
                    false,
                )
                .await
                .ok();
            }
        }
        Channel::TextChannel {
            id,
            name,
            description,
            icon,
            nsfw,
            voice,
            slowmode,
            announcement,
            forum,
            ..
        } => {
            // a forum's tags: names 1-20 characters, ids unique
            if data.forum_tags.is_some() || data.require_tag.is_some() {
                let Some(current) = forum.as_ref() else {
                    return Err(create_error!(InvalidOperation));
                };
                let mut next = current.clone();
                if let Some(tags) = &data.forum_tags {
                    let mut ids = std::collections::HashSet::new();
                    for tag in tags {
                        tag.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
                        if !ids.insert(tag.id.clone())
                            || tag.name.trim().is_empty()
                            || tag.emoji.as_deref().is_some_and(|emoji| !is_tag_emoji(emoji))
                        {
                            return Err(create_error!(InvalidOperation));
                        }
                    }
                    next.tags = tags.iter().cloned().map(Into::into).collect();
                }
                if let Some(require) = data.require_tag {
                    next.require_tag = require;
                }
                *forum = Some(next.clone());
                partial.forum = Some(next);
            }

            if data.remove.contains(&v0::FieldsChannel::Icon) {
                if let Some(icon) = &icon {
                    db.mark_attachment_as_deleted(&icon.id).await?;
                }
            }

            for field in &data.remove {
                match field {
                    v0::FieldsChannel::Description => {
                        description.take();
                    }
                    v0::FieldsChannel::Icon => {
                        icon.take();
                    }
                    v0::FieldsChannel::Voice => {
                        voice.take();
                    }
                    v0::FieldsChannel::Slowmode => {
                        slowmode.take();
                    }
                    _ => {}
                }
            }

            if let Some(icon_id) = data.icon {
                partial.icon = Some(File::use_channel_icon(db, &icon_id, id, &user.id).await?);
                *icon = partial.icon.clone();
            }

            if let Some(new_name) = data.name {
                *name = new_name.clone();
                partial.name = Some(new_name);
            }

            if let Some(new_description) = data.description {
                partial.description = Some(new_description);
                *description = partial.description.clone();
            }

            if let Some(new_nsfw) = data.nsfw {
                *nsfw = new_nsfw;
                partial.nsfw = Some(new_nsfw);
            }

            if let Some(edit) = data.voice {
                // fields left out keep their value (a user limit change must
                // not turn a stage back into a plain voice channel)
                let mut new_voice = voice.clone().unwrap_or_default();
                if let Some(max_users) = edit.max_users {
                    new_voice.max_users = Some(max_users);
                }
                if let Some(stage) = edit.stage {
                    // who may publish changes with it: only while the call is empty
                    if stage != new_voice.stage
                        && get_voice_channel_members(&UserVoiceChannel {
                            id: id.clone(),
                            server_id: None,
                        })
                        .await?
                        .is_some()
                    {
                        return Err(create_error!(InvalidOperation));
                    }
                    new_voice.stage = stage;
                }
                *voice = Some(new_voice.clone());
                partial.voice = Some(new_voice);
            }

            if let Some(new_slowmode) = data.slowmode {
                *slowmode = Some(new_slowmode);
                partial.slowmode = Some(new_slowmode);
            }

            // voice rooms have no message feed to announce in
            if let Some(new_announcement) = data.announcement {
                if new_announcement && voice.is_some() {
                    return Err(create_error!(InvalidOperation));
                }
                *announcement = new_announcement;
                partial.announcement = Some(new_announcement);
            }
        }
        _ => return Err(create_error!(InvalidOperation)),
    };

    let remove = data
        .remove
        .into_iter()
        .map(|f| f.into())
        .collect::<Vec<FieldsChannel>>();

    let before = if before_channel.server().is_some() {
        Some(before_channel.generate_diff(&partial, &remove))
    } else {
        None
    };

    channel.update(db, partial.clone(), remove).await?;

    // threads are mature with their channel
    if let (Some(nsfw), Channel::TextChannel { parent: None, .. }) = (partial.nsfw, &channel) {
        for archived in [false, true] {
            let mut before: Option<String> = None;
            loop {
                let threads = db
                    .fetch_threads(channel.id(), archived, before.as_deref(), 100)
                    .await?;
                let last = threads.last().map(|t| t.id().to_string());
                for mut thread in threads {
                    thread
                        .update(
                            db,
                            PartialChannel {
                                nsfw: Some(nsfw),
                                ..Default::default()
                            },
                            vec![],
                        )
                        .await?;
                }
                match last {
                    Some(last) => before = Some(last),
                    None => break,
                }
            }
        }
    }

    if channel.voice().is_none() {
        delete_voice_channel(voice_client, &UserVoiceChannel::from_channel(&channel)).await?;
    }

    if let Some(before) = before {
        AuditLogEntryAction::ChannelEdit {
            channel: channel.id().to_string(),
            before,
            after: partial,
        }
        .insert(db, channel.server().unwrap().to_string(), reason, user.id, None)
        .await;
    };

    Ok(Json(channel.into()))
}

/// A forum tag's emoji: a custom emoji id (a ULID) or one Unicode emoji,
/// never text such as "csc"
fn is_tag_emoji(emoji: &str) -> bool {
    let is_ulid = emoji.len() == 26
        && emoji
            .chars()
            .all(|c| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c)));
    // keycaps (1️⃣, #️⃣) keep an ASCII digit or sign; letters and spaces never
    // belong to an emoji
    let is_unicode = emoji.chars().count() <= 16
        && emoji.chars().any(|c| !c.is_ascii())
        && !emoji
            .chars()
            .any(|c| c.is_ascii_alphabetic() || c.is_whitespace() || c.is_control());
    is_ulid || is_unicode
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Channel};
    use revolt_models::v0::{DataCreateGroup, SystemMessage};
    use rocket::http::{ContentType, Header, Status};
    use revolt_database::events::client::EventV1;
    use crate::util::test::PubSubTestHelper;

    #[rocket::async_test]
    async fn success_transfer_group_owner() {
        let harness = TestHarness::new().await;
        let (_, session, mut user) = harness.new_user().await;
        let (_, _, mut other_user) = harness.new_user().await;

        // Create a group chat
        let group = Channel::create_group(
            &harness.db,
            DataCreateGroup {
                users: HashSet::from([other_user.id.clone()]),
                ..Default::default()
            },
            user.id.clone(),
        )
            .await
            .expect("`Channel`");

        let mut pubsub = PubSubTestHelper::new(group.id()).await;

        // Make other user the owner
        let owner_response = harness
            .client
            .patch(format!("/channels/{}", group.id()))
            .header(ContentType::JSON)
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(
                json!({
                    "owner": other_user.id,
                })
                    .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(owner_response.status(), Status::Ok);
        drop(owner_response);

        pubsub
            .wait_for_event(|event| match event {
                EventV1::Message(message) => match &message.system {
                    Some(SystemMessage::ChannelOwnershipChanged { from, to }) => {
                        assert_eq!(from, &user.id);
                        assert_eq!(to, &other_user.id);

                        true
                    }
                    _ => false,
                },
                _ => false,
            })
            .await;
    }

    #[rocket::async_test]
    async fn fail_transfer_owner_to_bot() {
        let harness = TestHarness::new().await;
        let (_, session, mut user) = harness.new_user().await;
        let (_, mut bot_user) = harness.new_bot(&user).await;

        // Create a group chat
        let group = Channel::create_group(
            &harness.db,
            DataCreateGroup {
                users: HashSet::from([bot_user.id.clone()]),
                ..Default::default()
            },
            user.id.clone(),
        )
            .await
            .expect("`Channel`");

        // Make bot the owner
        let owner_response = harness
            .client
            .patch(format!("/channels/{}", group.id()))
            .header(ContentType::JSON)
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(
                json!({
                    "owner": bot_user.id,
                })
                    .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(owner_response.status(), Status::BadRequest);
        drop(owner_response);
    }
}
