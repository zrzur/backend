use std::time::Duration;

use redis_kiss::{get_connection, redis, AsyncCommands};
use revolt_database::events::client::EventV1;
use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::{
    util::idempotency::IdempotencyKey, util::reference::Reference, Database, User,
};
use revolt_database::{Channel, Interactions, Message, AMQP};
use revolt_models::v0;
use revolt_models::v0::ChannelSlowmode;
use revolt_permissions::PermissionQuery;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// # Send Message
///
/// Sends a message to the given channel.
#[openapi(tag = "Messaging")]
#[post("/<target>/messages", data = "<data>")]
pub async fn message_send(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataMessageSend>,
    idempotency: IdempotencyKey,
) -> Result<Json<v0::Message>> {
    send(db, amqp, user, target, data, idempotency, false).await
}

/// Send a message. `opens_thread` is the first message of a thread that was
/// just allowed to start: a post needs no right to reply to itself.
pub async fn send(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataMessageSend>,
    idempotency: IdempotencyKey,
    opens_thread: bool,
) -> Result<Json<v0::Message>> {
    send_inner(db, amqp, user, target, data.into_inner(), idempotency, opens_thread, None).await
}

/// [`send`] with every rule of a normal send; `forwarded` makes the message
/// a forwarded copy (see `message_forward`)
#[allow(clippy::too_many_arguments)]
pub async fn send_inner(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: v0::DataMessageSend,
    idempotency: IdempotencyKey,
    opens_thread: bool,
    forwarded: Option<v0::MessageForward>,
) -> Result<Json<v0::Message>> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // Ensure we have permissions to send a message
    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;

    // In a direct message, say when the other person's privacy settings
    // are the reason (blocks keep their own error)
    if matches!(channel, Channel::DirectMessage { .. })
        && !permissions.has_channel_permission(ChannelPermission::SendMessage)
        && query.are_we_part_of_the_channel().await
        && !matches!(
            query.user_relationship().await,
            revolt_permissions::RelationshipStatus::Blocked
                | revolt_permissions::RelationshipStatus::BlockedOther
        )
    {
        return Err(create_error!(DirectMessagesRestricted));
    }

    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;

    // A shared server paused DMs between members who aren't friends
    if let Channel::DirectMessage { recipients, .. } = &channel {
        if let Some(other) = recipients.iter().find(|id| **id != user.id) {
            let other = db.fetch_user(other).await?;
            if revolt_database::util::raid::dms_paused_between(db, &user, &other).await? {
                return Err(create_error!(DirectMessagesPaused));
            }
        }
    }

    // Mature channels are for adults only
    revolt_database::util::age::assert_can_view_channel(db, &user, &channel).await?;

    // The server's verification level (members without a role), then AutoMod
    if let Some(server_id) = channel.server() {
        let server = db.fetch_server(server_id).await?;
        revolt_database::util::verification::assert_can_talk_in(db, &user, &server).await?;

        if server.automod.as_ref().is_some_and(|rules| !rules.is_empty()) {
            let member = db.fetch_member(server_id, &user.id).await.ok();
            revolt_database::util::automod::check_message(
                db,
                &server,
                channel.id(),
                &user,
                member.as_ref(),
                data.content.as_deref().unwrap_or_default(),
                revolt_database::util::automod::Check::Send,
            )
            .await?;

            // the forwarder's own words pass the same rules
            if let Some(note) = forwarded.as_ref().and_then(|origin| origin.note.as_deref()) {
                revolt_database::util::automod::check_message(
                    db,
                    &server,
                    channel.id(),
                    &user,
                    member.as_ref(),
                    note,
                    revolt_database::util::automod::Check::Send,
                )
                .await?;
            }
        }
    }

    // Announcement channels: only people who can manage messages post
    if matches!(
        channel,
        Channel::TextChannel {
            announcement: true,
            ..
        }
    ) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;
    }

    // Forums: every post is a thread, nobody writes in the forum itself
    if matches!(channel, Channel::TextChannel { forum: Some(_), .. }) {
        return Err(create_error!(InvalidOperation));
    }

    // Threads and forum posts: closed by their parent's permission or the
    // server's switch
    if !opens_thread {
        revolt_database::util::threads::assert_can_write(db, &channel, &permissions).await?;
    }

    // Threads: a locked one only for people who can manage messages; a
    // message reopens an archived one and is counted
    let thread_update = match &channel {
        Channel::TextChannel {
            thread: Some(thread),
            ..
        } => {
            if thread.locked {
                permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;
            }
            let mut next = thread.clone();
            next.archived = false;
            next.message_count = next.message_count.saturating_add(1);
            Some((channel.clone(), next))
        }
        _ => None,
    };

    // Verify permissions for masquerade
    if let Some(masq) = &data.masquerade {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::Masquerade)?;

        // nobody may post as Zrzur
        if masq.name.as_deref().is_some_and(revolt_database::passes_for_official) {
            return Err(create_error!(InvalidUsername));
        }

        if masq.colour.is_some() {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageRole)?;
        }

        // Nobody (bot or person) posts under the name of someone else in
        // the server
        if let (Some(name), Some(server)) = (&masq.name, channel.server()) {
            if revolt_database::util::masquerade::masquerade_name_taken(
                db, server, &user.id, name,
            )
            .await?
            {
                return Err(create_error!(MasqueradeNameTaken));
            }
        }
    }

    // Check permissions for embeds
    if data.embeds.as_ref().is_some_and(|v| !v.is_empty()) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::SendEmbeds)?;
    }

    // Check permissions for files
    if data.attachments.as_ref().is_some_and(|v| !v.is_empty()) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::UploadFiles)?;
    }

    if !permissions.has_channel_permission(ChannelPermission::BypassSlowmode) {
        if let Channel::TextChannel {
            slowmode: Some(channel_slowmode),
            id: channel_id,
            ..
        } = &channel
        {
            if *channel_slowmode > 0 {
                if let Ok(conn) = get_connection().await {
                    let mut conn = conn.into_inner();

                    let slowmode_key = format!("slowmode:{}:{}", user.id, channel_id);

                    // Atomic check-and-set: only set if absent and apply expiry in one command.
                    let set_result: Option<String> = conn
                        .set_options(
                            &slowmode_key,
                            "1", // The value doesn't matter, only the key's existence
                            redis::SetOptions::default()
                                .conditional_set(redis::ExistenceCheck::NX)
                                .with_expiration(redis::SetExpiry::EX(*channel_slowmode as usize)),
                        )
                        .await
                        .unwrap_or(None);

                    if set_result.is_some() {
                        let idx_key = format!("slowmode_idx:{}", user.id);
                        conn.sadd::<_, _, ()>(&idx_key, channel_id.as_str())
                            .await
                            .ok();
                        conn.expire::<_, ()>(&idx_key, *channel_slowmode as usize)
                            .await
                            .ok();
                    }

                    // If `set_result` is None, the `NX` condition failed because the key already exists.
                    // This means the user is currently in slowmode.
                    if set_result.is_none() {
                        // Fetch the remaining TTL to accurately populate the retry_after field
                        let ttl: i64 = conn.ttl(&slowmode_key).await.unwrap_or(0);

                        // Redis returns positive integers for valid TTLs
                        if ttl > 0 {
                            EventV1::UserSlowmodes {
                                slowmodes: vec![ChannelSlowmode {
                                    channel_id: channel_id.to_string(),
                                    duration: *channel_slowmode,
                                    retry_after: ttl as u64,
                                }],
                            }
                            .p(user.id.clone())
                            .await;
                            return Err(create_error!(InSlowmode {
                                retry_after: ttl as u64
                            }));
                        }
                    } else {
                        EventV1::UserSlowmodes {
                            slowmodes: vec![ChannelSlowmode {
                                channel_id: channel_id.to_string(),
                                duration: *channel_slowmode,
                                retry_after: *channel_slowmode,
                            }],
                        }
                        .p(user.id.clone())
                        .await;
                    }
                }
                // If Redis connection fails, just skip the slowmode check
            }
        }
    }

    // Ensure interactions information is correct
    if let Some(interactions) = &data.interactions {
        let interactions: Interactions = interactions.clone().into();
        interactions.validate(db, &permissions).await?;
    }

    // Disallow mentions for new users (TRUST-0: <12 hours age) in public servers
    let allow_mentions = if let Some(server) = query.server_ref() {
        if server.discoverable {
            (ulid::Ulid::from_string(&user.id)
                .unwrap()
                .datetime()
                .elapsed()
                .expect("Time went backwards"))
                >= Duration::from_hours(12)
        } else {
            true
        }
    } else {
        true
    };

    // Create the message
    let author: v0::User = user.clone().into(db, Some(&user)).await;

    // Make sure we have server member (edge case if server owner)
    query.are_we_a_member().await;

    // Create model user / members
    let model_user = user
        .clone()
        .into_known_static(revolt_presence::is_online(&user.id).await)
        .await;

    let model_member: Option<v0::Member> = query
        .member_ref()
        .as_ref()
        .map(|member| member.clone().into_owned().into());

    let message = Message::create_full(
        db,
        Some(amqp),
        channel,
        data,
        v0::MessageAuthor::User(&author),
        Some(model_user.clone()),
        model_member.clone(),
        user.limits().await,
        idempotency,
        permissions.has_channel_permission(ChannelPermission::SendEmbeds),
        allow_mentions,
        None,
        forwarded,
    )
    .await?;

    // what you send yourself is read: every session of yours, and the unread
    // state after a reconnect, start after this message
    if user.bot.is_none() {
        if let Err(err) = revolt_database::util::acker::ack_channel(
            &user.id,
            &message.channel,
            &message.id,
            amqp,
        )
        .await
        {
            revolt_config::capture_error(&err);
        }
    }

    if let Some((mut thread_channel, next)) = thread_update {
        thread_channel
            .update(
                db,
                revolt_database::PartialChannel {
                    thread: Some(next),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
    }

    Ok(Json(message.into_model(Some(model_user), model_member)))
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        util::{idempotency::IdempotencyKey, reference::Reference},
        Channel, Member, Message, MessageFlagsValue, PartialChannel, PartialMember, Role, Server,
    };
    use revolt_models::v0::{self, DataCreateServerChannel, MessageFlags};
    use revolt_permissions::{ChannelPermission, OverrideField};
    use revolt_result::ErrorType;

    #[rocket::async_test]
    async fn message_mention_constraints() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (_, _, second_user) = harness.new_user().await;

        let (server, channels) = Server::create(
            &harness.db,
            v0::DataCreateServer {
                name: "Test Server".to_string(),
                ..Default::default()
            },
            &user,
            true,
        )
        .await
        .expect("Failed to create test server");

        let server_mut: &mut Server = &mut server.clone();
        let mut locked_channel = Channel::create_server_channel(
            &harness.db,
            server_mut,
            DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Text,
                name: "Hidden Channel".to_string(),
                description: None,
                nsfw: Some(false),
                voice: None,
                announcement: None,
                category: None,
                private: None,
            },
            true,
        )
        .await
        .expect("Failed to make new channel");

        let role = Role::create(&harness.db, &server, "Show Hidden Channel".to_string())
            .await
            .expect("Failed to create the role");

        let mut overrides = HashMap::new();
        overrides.insert(
            role.id.clone(),
            OverrideField {
                a: (ChannelPermission::ViewChannel) as i64,
                d: 0,
            },
        );

        let partial = PartialChannel {
            name: None,
            owner: None,
            description: None,
            icon: None,
            nsfw: None,
            active: None,
            permissions: None,
            role_permissions: Some(overrides),
            member_permissions: None,
            default_permissions: Some(OverrideField {
                a: 0,
                d: ChannelPermission::ViewChannel as i64,
            }),
            last_message_id: None,
            voice: None,
            slowmode: None,
            announcement: None,
            permissions_synced: None,
            thread: None,
            forum: None,
        };
        locked_channel
            .update(&harness.db, partial, vec![])
            .await
            .expect("Failed to update the channel permissions for special role");

        Member::create(&harness.db, &server, &user, Some(channels.clone()))
            .await
            .expect("Failed to create member");
        let member = Reference::from_unchecked(&user.id)
            .as_member(&harness.db, &server.id)
            .await
            .expect("Failed to get member");

        // Second user is not part of the server
        let message = Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            locked_channel.clone(),
            v0::DataMessageSend {
                content: Some(format!("<@{}>", second_user.id)),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(&user)).await),
            Some(user.clone().into(&harness.db, Some(&user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("0".to_string()),
            false,
            true,
        )
        .await
        .expect("Failed to create message");

        // The mention should not go through here
        assert!(
            message.mentions.is_none() || message.mentions.unwrap().is_empty(),
            "Mention failed to be scrubbed when the user is not part of the server"
        );

        Member::create(&harness.db, &server, &second_user, Some(channels.clone()))
            .await
            .expect("Failed to create second member");
        let mut second_member = Reference::from_unchecked(&second_user.id)
            .as_member(&harness.db, &server.id)
            .await
            .expect("Failed to get second member");

        // Second user cannot see the channel
        let message = Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            locked_channel.clone(),
            v0::DataMessageSend {
                content: Some(format!("<@{}>", second_user.id)),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(&user)).await),
            Some(user.clone().into(&harness.db, Some(&user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("1".to_string()),
            false,
            true,
        )
        .await
        .expect("Failed to create message");

        // The mention should not go through here
        assert!(
            message.mentions.is_none() || message.mentions.unwrap().is_empty(),
            "Mention failed to be scrubbed when the user cannot see the channel"
        );

        let second_member_roles = vec![role.id.clone()];
        let partial = PartialMember {
            id: None,
            joined_at: None,
            nickname: None,
            pronouns: None,
            avatar: None,
            timeout: None,
            roles: Some(second_member_roles),
            can_publish: None,
            can_receive: None,
            accepted_rules_at: None,
        };
        second_member
            .update(&harness.db, partial, vec![])
            .await
            .expect("Failed to update the second user's roles");

        // This time the mention SHOULD go through
        let message = Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            locked_channel.clone(),
            v0::DataMessageSend {
                content: Some(format!("<@{}>", second_user.id)),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(&user)).await),
            Some(user.clone().into(&harness.db, Some(&user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("2".to_string()),
            false,
            true,
        )
        .await
        .expect("Failed to create message");

        // The mention SHOULD go through here
        assert!(
            message.mentions.is_some() && !message.mentions.unwrap().is_empty(),
            "Mention was scrubbed when the user can see the channel"
        );
    }

    #[rocket::async_test]
    async fn message_reply() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let channel = harness.new_channel(&server).await;
        let (_, member, message) = harness.new_message(&user, &server, channels).await;

        // Send a message with a reply
        // Should succeed
        let message_with_reply = Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            channel.clone(),
            v0::DataMessageSend {
                content: Some("Message with reply".to_string()),
                nonce: None,
                attachments: None,
                replies: Some(vec![v0::ReplyIntent {
                    id: message.id.clone(),
                    mention: false,
                    fail_if_not_exists: Some(true),
                }]),
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(&user)).await),
            Some(user.clone().into(&harness.db, Some(&user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("1".to_string()),
            false,
            false,
        )
        .await
        .expect("Failed to create message with reply");

        assert!(
            message_with_reply.replies.is_some(),
            "Message replies is None",
        );

        let replies = message_with_reply.replies.unwrap();

        assert!(!replies.is_empty(), "Message replies is empty",);

        assert_eq!(replies[0], message.id, "Message reply ID does not match",);

        // Delete the message
        message
            .clone()
            .delete(&harness.db)
            .await
            .expect("Failed to delete message");

        // Attempt to create messages with a reply to a deleted message

        // fail_if_not_exists is set to false
        // Should send the message without a reply
        let message_with_missing_reply = Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            channel.clone(),
            v0::DataMessageSend {
                content: Some("Message with missing reply".to_string()),
                nonce: None,
                attachments: None,
                replies: Some(vec![v0::ReplyIntent {
                    id: message.id.clone(),
                    mention: false,
                    fail_if_not_exists: Some(false),
                }]),
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(&user)).await),
            Some(user.clone().into(&harness.db, Some(&user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("3".to_string()),
            false,
            false,
        )
        .await
        .expect("Failed to create message with missing reply");

        assert!(
            message_with_missing_reply.replies.is_none()
                || message_with_missing_reply.replies.unwrap().is_empty(),
            "Message replies exist when they shouldn't",
        );

        // fail_if_not_exists is set to true
        // Should fail to send the message
        Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            channel.clone(),
            v0::DataMessageSend {
                content: Some("Message with missing reply".to_string()),
                nonce: None,
                attachments: None,
                replies: Some(vec![v0::ReplyIntent {
                    id: message.id.clone(),
                    mention: false,
                    fail_if_not_exists: Some(true),
                }]),
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(&user)).await),
            Some(user.clone().into(&harness.db, Some(&user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("4".to_string()),
            false,
            false,
        )
        .await
        .expect_err("Created message with missing reply and true fail");

        // fail_if_not_exists is not set
        // Should fail to send the message
        Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            channel.clone(),
            v0::DataMessageSend {
                content: Some("Message with missing reply".to_string()),
                nonce: None,
                attachments: None,
                replies: Some(vec![v0::ReplyIntent {
                    id: message.id.clone(),
                    mention: false,
                    fail_if_not_exists: None,
                }]),
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(&user)).await),
            Some(user.clone().into(&harness.db, Some(&user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("4".to_string()),
            false,
            false,
        )
        .await
        .expect_err("Created message with missing reply and none fail");
    }

    #[rocket::async_test]
    async fn mass_mentions_test() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (_, _, other_user) = harness.new_user().await;
        let (server, _) = harness.new_server(&user).await;
        let channel = harness.new_channel(&server).await;
        let role = harness
            .new_role(
                &server,
                1,
                Some(OverrideField {
                    a: (ChannelPermission::MentionEveryone as i64)
                        | (ChannelPermission::MentionRoles as i64),
                    d: 0,
                }),
            )
            .await;
        let (mut other_member, _) = Member::create(&harness.db, &server, &other_user, None)
            .await
            .expect("Failed to add test member");

        // Send a message with an everyone and role mention.
        // Should fail
        let bad_message_with_mentions = Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            channel.clone(),
            v0::DataMessageSend {
                content: Some(format!("Mentioning @everyone and role <%{}>", &role.id)),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(
                &other_user
                    .clone()
                    .into(&harness.db, Some(&other_user))
                    .await,
            ),
            Some(
                other_user
                    .clone()
                    .into(&harness.db, Some(&other_user))
                    .await,
            ),
            Some(other_member.clone().into()),
            other_user.limits().await,
            IdempotencyKey::unchecked_from_string("1".to_string()),
            false,
            true,
        )
        .await
        .expect_err("Should not have created message with everyone and role pings");

        assert!(
            matches!(
                bad_message_with_mentions.error_type,
                ErrorType::MissingPermission { .. }
            ),
            "Intentional permissions error did not return MissingPermission"
        );

        // Send a mass mention inside a codeblock.
        // Should be undetected and therefor pass
        let message_with_codeblock = Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            channel.clone(),
            v0::DataMessageSend {
                content: Some(format!("Mentioning `@everyone` and role `<%{}>`", &role.id)),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(
                &other_user
                    .clone()
                    .into(&harness.db, Some(&other_user))
                    .await,
            ),
            Some(
                other_user
                    .clone()
                    .into(&harness.db, Some(&other_user))
                    .await,
            ),
            Some(other_member.clone().into()),
            other_user.limits().await,
            IdempotencyKey::unchecked_from_string("1".to_string()),
            false,
            true,
        )
        .await
        .expect("Failed to create message with everyone and role pings in codeblocks");

        assert!(
            message_with_codeblock.flags.is_none()
                || !MessageFlagsValue(message_with_codeblock.flags.unwrap())
                    .has(MessageFlags::MentionsEveryone),
            "Message flags mentions everyone when inside codeblock",
        );

        assert!(
            message_with_codeblock.role_mentions.is_none(),
            "Role mentions detected when inside codeblock"
        );

        other_member.roles.push(role.id.clone());
        harness
            .db
            .update_member(
                &other_member.id,
                &PartialMember {
                    avatar: None,
                    id: None,
                    joined_at: None,
                    nickname: None,
                    pronouns: None,
                    roles: Some(vec![role.id.clone()]),
                    timeout: None,
                    can_publish: None,
                    can_receive: None,
                    accepted_rules_at: None,
                },
                vec![],
            )
            .await
            .expect("Failed to add role to user");

        // Send a message with an everyone and role mention.
        // Should succeed
        let message_with_mentions = Message::create_from_api(
            &harness.db,
            Some(&harness.amqp),
            channel.clone(),
            v0::DataMessageSend {
                content: Some(format!("Mentioning @everyone and role <%{}>", &role.id)),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                flags: None,
            },
            v0::MessageAuthor::User(
                &other_user
                    .clone()
                    .into(&harness.db, Some(&other_user))
                    .await,
            ),
            Some(
                other_user
                    .clone()
                    .into(&harness.db, Some(&other_user))
                    .await,
            ),
            Some(other_member.clone().into()),
            other_user.limits().await,
            IdempotencyKey::unchecked_from_string("1".to_string()),
            false,
            true,
        )
        .await
        .expect("Failed to create message with everyone and role pings");

        assert!(
            message_with_mentions.flags.is_some(),
            "Message flags is None",
        );

        assert!(
            MessageFlagsValue(message_with_mentions.flags.unwrap())
                .has(MessageFlags::MentionsEveryone),
            "Message flags does not mention everyone. Flag value is {}",
            message_with_mentions.flags.unwrap()
        );

        assert!(
            message_with_mentions.role_mentions.is_some(),
            "Message has no role mentions"
        );
    }
}

#[cfg(test)]
mod flags_test {
    use crate::util::test::TestHarness;
    use revolt_database::{Member, Server};
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    /// Message flags are bit positions: SuppressNotifications = bit 1 (2),
    /// MentionsEveryone = bit 2 (4), MentionsOnline = bit 3 (8). The input
    /// check must let every defined flag through.
    #[rocket::async_test]
    async fn every_message_flag_passes_validation() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = Server::create(
            &harness.db,
            v0::DataCreateServer {
                name: "Flags".to_string(),
                ..Default::default()
            },
            &user,
            true,
        )
        .await
        .unwrap();
        Member::create(&harness.db, &server, &user, None).await.unwrap();
        let channel = channels[0].id().to_string();
        let send = |flags: u32| {
            harness
                .client
                .post(format!("/channels/{channel}/messages"))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", session.token.clone()))
                .body(json!({ "content": "flags", "flags": flags }).to_string())
                .dispatch()
        };

        // silent: stored as bit 1
        let response = send(1 << 1).await;
        assert_eq!(response.status(), Status::Ok);
        let message: v0::Message = response.into_json().await.unwrap();
        assert_eq!(message.flags & (1 << 1), 1 << 1);

        // MentionsOnline is understood (a person may not set it: IsNotBot), not rejected as unknown
        let response = send(1 << 3).await;
        let body = response.into_string().await.unwrap_or_default();
        assert!(body.contains("IsNotBot"), "{body}");

        // bits above the defined flags are still refused
        let response = send(1 << 4).await;
        assert_eq!(response.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn sending_marks_the_channel_read_for_the_author() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = Server::create(
            &harness.db,
            v0::DataCreateServer {
                name: "Own messages".to_string(),
                ..Default::default()
            },
            &user,
            true,
        )
        .await
        .unwrap();
        Member::create(&harness.db, &server, &user, None).await.unwrap();
        let channel = channels[0].id().to_string();

        let response = harness
            .client
            .post(format!("/channels/{channel}/messages"))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(json!({ "content": "mine" }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let message: v0::Message = response.into_json().await.unwrap();

        // the ack waits in Redis for crond, as one from PUT /ack does
        let mut redis = redis_kiss::get_connection().await.unwrap();
        let acked = revolt_database::util::acker::take_ack(&mut redis, &user.id, &channel)
            .await
            .unwrap();
        assert_eq!(acked.as_deref(), Some(message.id.as_str()));
    }
}
