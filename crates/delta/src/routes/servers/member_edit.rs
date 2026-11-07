use std::collections::HashSet;

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_database::{
    AuditLogEntryAction, Database, FieldsMember, File, PartialMember, User,
    events::client::EventV1,
    util::{
        permissions::{DatabasePermissionQuery, perms},
        reference::Reference,
    },
    voice::{
        UserVoiceChannel, VoiceClient, clear_voice_reentry, delete_voice_state, get_channel_node,
        get_user_voice_channel_in_server, get_voice_channel_members,
        remove_user_from_voice_channel, revoke_voice_reentry, set_channel_node,
        set_user_moved_from_voice, set_user_moved_to_voice, sync_user_voice_permissions,
    },
};
use revolt_models::v0;

use revolt_permissions::{
    ChannelPermission, calculate_channel_permissions, calculate_server_permissions,
};
use revolt_result::{Result, create_error};
use rocket::{State, form::validate::Contains, serde::json::Json};
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// # Edit Member
///
/// Edit a member by their id.
#[openapi(tag = "Server Members")]
#[patch("/<server_id>/members/<member_id>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    reason: AuditLogReason,
    server_id: Reference<'_>,
    member_id: Reference<'_>,
    data: Json<v0::DataMemberEdit>,
) -> Result<Json<v0::Member>> {
    let mut data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // Fetch server and member
    let server = server_id.as_server(db).await?;
    let target_user = member_id.as_user(db).await?;
    let mut member = member_id.as_member(db, &server.id).await?;

    // Fetch our currrent permissions
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    let permissions = calculate_server_permissions(&mut query).await;

    // Nobody may pass for Zrzur; a nickname kept from before this rule may be
    // sent back unchanged
    if data.nickname.as_deref().is_some_and(|name| {
        revolt_database::passes_for_official(name) && member.nickname.as_deref() != Some(name)
    }) {
        return Err(create_error!(InvalidUsername));
    }

    // Check permissions in server
    if data.nickname.is_some() || data.remove.contains(&v0::FieldsMember::Nickname) {
        if user.id == member.id.user {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::ChangeNickname)?;
        } else {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageNicknames)?;
        }
    }

    if (data.pronouns.is_some() || data.remove.contains(&v0::FieldsMember::Pronouns))
        && user.id != member.id.user
    {
        return Err(create_error!(InvalidOperation));
    }

    if data.avatar.is_some() || data.remove.contains(&v0::FieldsMember::Avatar) {
        if user.id == member.id.user {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::ChangeAvatar)?;
        } else if data.remove.contains(&v0::FieldsMember::Avatar) {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::RemoveAvatars)?;
        } else {
            return Err(create_error!(InvalidOperation));
        }
    }

    if data.roles.is_some() || data.remove.contains(&v0::FieldsMember::Roles) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::AssignRoles)?;
    }

    if data.timeout.is_some() || data.remove.contains(&v0::FieldsMember::Timeout) {
        if data.timeout.is_some() {
            if member.id.user == user.id {
                return Err(create_error!(CannotTimeoutYourself));
            }
        }

        // A timeout ends in the future and at most `max_timeout_days` from
        // now. Who may be timed out is decided by the rank check below.
        if let Some(until) = data.timeout {
            let now = Timestamp::now_utc();
            let max_days = config().await.features.limits.global.max_timeout_days as i64;
            if until <= now || until > now + Duration::days(max_days) {
                return Err(create_error!(InvalidProperty));
            }
        }

        permissions.throw_if_lacking_channel_permission(ChannelPermission::TimeoutMembers)?;
    }

    if data.can_publish.is_some() {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::MuteMembers)?;
    }

    if data.can_receive.is_some() {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::DeafenMembers)?;
    }

    if data.voice_channel.is_some() && data.remove.contains(&v0::FieldsMember::VoiceChannel) {
        return Err(create_error!(InvalidOperation));
    }

    let current_voice_channel =
        get_user_voice_channel_in_server(&target_user.id, &server.id).await?;

    if data.voice_channel.is_some() || data.remove.contains(&v0::FieldsMember::VoiceChannel) {
        if !voice_client.is_enabled() {
            return Err(create_error!(LiveKitUnavailable));
        };

        if member.id.user != user.id {
            // Moving or disconnecting someone needs Move Members in the channel
            // they are in, so a channel override can grant or deny it there.
            match &current_voice_channel {
                Some(source_id) => {
                    let source = Reference::from_unchecked(source_id).as_channel(db).await?;
                    calculate_channel_permissions(&mut query.clone().channel(&source))
                        .await
                        .throw_if_lacking_channel_permission(ChannelPermission::MoveMembers)?;
                }
                None => permissions
                    .throw_if_lacking_channel_permission(ChannelPermission::MoveMembers)?,
            }
        }
    }

    let new_voice_channel = if let Some(new_channel) = &data.voice_channel {
        // ensure the channel we are moving them to is in the server and is a voice channel

        let channel = Reference::from_unchecked(new_channel)
            .as_channel(db)
            .await
            .map_err(|_| create_error!(UnknownChannel))?;

        if channel.server().is_none_or(|v| v != member.id.server) {
            Err(create_error!(UnknownChannel))?
        }

        if channel.voice().is_none() {
            return Err(create_error!(NotAVoiceChannel));
        }

        let channel_permissions =
            calculate_channel_permissions(&mut query.clone().channel(&channel)).await;
        channel_permissions.throw_if_lacking_channel_permission(ChannelPermission::Connect)?;
        if member.id.user != user.id {
            channel_permissions
                .throw_if_lacking_channel_permission(ChannelPermission::MoveMembers)?;
        }

        let mut target_query = perms(db, &target_user).server(&server).channel(&channel);
        calculate_channel_permissions(&mut target_query)
            .await
            .throw_if_lacking_channel_permission(ChannelPermission::Connect)?;

        match &current_voice_channel {
            None => Err(create_error!(NotConnected))?,
            Some(current) if current == channel.id() => Err(create_error!(AlreadyConnected))?,
            Some(_) => {}
        };

        if let Some(max_users) = channel.voice().and_then(|voice| voice.max_users) {
            let occupancy = get_voice_channel_members(&UserVoiceChannel::from_channel(&channel))
                .await?
                .map(|members| {
                    members
                        .iter()
                        .filter(|id| id.as_str() != target_user.id)
                        .count()
                })
                .unwrap_or(0);

            if occupancy >= max_users
                && !channel_permissions.has(ChannelPermission::ManageChannel as u64)
            {
                return Err(create_error!(CannotJoinCall));
            }
        }

        Some(channel)
    } else {
        None
    };

    // The owner may hold no roles. get_ranking is then i64::MAX, which is a
    // worse rank than every role, so a moderator would pass the check below.
    // Kick and ban already refuse this target.
    if member.id.user == server.owner && user.id != server.owner {
        return Err(create_error!(InvalidOperation));
    }

    // Resolve our ranking
    let our_ranking = query.get_member_rank().unwrap_or(i64::MIN);

    // Check that we have permissions to act against this member
    if member.id.user != user.id
        && member.get_ranking(query.server_ref().as_ref().unwrap()) <= our_ranking
    {
        return Err(create_error!(NotElevated));
    }

    // Check permissions against roles in diff
    if let Some(roles) = &data.roles {
        let current_roles = member.roles.iter().collect::<HashSet<&String>>();

        let new_roles = roles.iter().collect::<HashSet<&String>>();
        let added_roles: Vec<&&String> = new_roles.difference(&current_roles).collect();

        for role_id in added_roles {
            if let Some(role) = server.roles.get(*role_id) {
                if role.rank <= our_ranking {
                    return Err(create_error!(NotElevated));
                }

                if role.is_managed() {
                    return Err(create_error!(InvalidOperation));
                }
            } else {
                return Err(create_error!(InvalidRole));
            }
        }

        // A bot's managed role cannot be given by hand, so it cannot be taken
        // away by hand either: nobody could restore it afterwards.
        for role_id in current_roles.difference(&new_roles) {
            if server.roles.get(*role_id).is_some_and(|role| role.is_managed()) {
                return Err(create_error!(InvalidOperation));
            }
        }

        data.roles = Some(new_roles.into_iter().cloned().collect()) // deduplicate the roles.
    }

    if data.remove.contains(&v0::FieldsMember::Roles)
        && member
            .roles
            .iter()
            .any(|id| server.roles.get(id).is_some_and(|role| role.is_managed()))
    {
        return Err(create_error!(InvalidOperation));
    }

    if data.remove.contains(&v0::FieldsMember::VoiceChannel)
        && get_user_voice_channel_in_server(&target_user.id, &server.id)
            .await?
            .is_none()
    {
        return Err(create_error!(NotConnected));
    }

    // Apply edits to the member object
    let v0::DataMemberEdit {
        nickname,
        pronouns,
        avatar,
        roles,
        timeout,
        remove,
        can_publish,
        can_receive,
        voice_channel: _,
    } = data;

    let mut partial = PartialMember {
        nickname,
        pronouns,
        roles,
        timeout,
        can_publish,
        can_receive,
        ..Default::default()
    };

    // 1. Remove fields from object
    if remove.contains(&v0::FieldsMember::Avatar)
        && let Some(avatar) = &member.avatar
    {
        db.mark_attachment_as_deleted(&avatar.id).await?;
    }

    // 2. Apply new avatar
    if let Some(avatar) = avatar {
        partial.avatar = Some(File::use_user_avatar(db, &avatar, &user.id, &user.id).await?);
    }

    let remove = remove
        .into_iter()
        .map(Into::into)
        .collect::<Vec<FieldsMember>>();

    let before = member.generate_diff(&partial, &remove);

    member.update(db, partial.clone(), remove.clone()).await?;

    AuditLogEntryAction::MemberEdit {
        user: member.id.user.clone(),
        before,
        after: partial,
    }
    .insert(
        db,
        server.id.clone(),
        reason,
        user.id.clone(),
        Some(member.id.user.clone()),
    )
    .await;

    if let Some(new_voice_channel) = new_voice_channel {
        if let Some(channel) = get_user_voice_channel_in_server(&target_user.id, &server.id).await?
        {
            let old_node = get_channel_node(&channel)
                .await?
                .ok_or_else(|| create_error!(NotConnected))?;

            let new_node = match get_channel_node(new_voice_channel.id()).await? {
                Some(node) => node,
                None => {
                    set_channel_node(new_voice_channel.id(), &old_node).await?;
                    old_node.clone()
                }
            };

            let new_user_voice_channel = UserVoiceChannel::from_channel(&new_voice_channel);
            let old_user_voice_channel = UserVoiceChannel {
                id: channel.clone(),
                server_id: new_user_voice_channel.server_id.clone(),
            };

            set_user_moved_from_voice(&channel, &new_user_voice_channel, &target_user.id).await?;
            set_user_moved_to_voice(
                new_voice_channel.id(),
                &old_user_voice_channel,
                &target_user.id,
            )
            .await?;

            let mut query = perms(db, &target_user).channel(&new_voice_channel);
            let permissions = calculate_channel_permissions(&mut query).await;

            voice_client
                .create_room(&new_node, &new_voice_channel)
                .await?;
            let token = voice_client
                .create_token(&new_node, db, &target_user, permissions, &new_voice_channel)
                .await?;

            revoke_voice_reentry(&channel, &target_user.id).await?;
            clear_voice_reentry(new_voice_channel.id(), &target_user.id).await?;

            // A seat LiveKit no longer knows is stale: drop it here, since no
            // participant_left webhook will follow.
            if voice_client
                .remove_user(&old_node, &target_user.id, &channel)
                .await
                .is_err()
            {
                delete_voice_state(&old_user_voice_channel, &target_user.id).await?;
            }

            EventV1::UserMoveVoiceChannel {
                node: new_node,
                from: channel,
                to: new_voice_channel.id().to_string(),
                token,
            }
            .p(target_user.id.clone())
            .await;
        };
    } else if (can_publish.is_some()
        || can_receive.is_some()
        || remove.contains(FieldsMember::CanPublish)
        || remove.contains(FieldsMember::CanReceive))
        && let Some(channel) = get_user_voice_channel_in_server(&target_user.id, &server.id).await?
        // no node (the call just ended): nothing live to update
        && let Some(node) = get_channel_node(&channel).await?
    {
        let channel = Reference::from_unchecked(&channel).as_channel(db).await?;

        sync_user_voice_permissions(
            db,
            voice_client,
            &node,
            &target_user,
            &channel,
            Some(&server),
            None,
        )
        .await?;
    };

    if remove.contains(&FieldsMember::VoiceChannel)
        && let Some(channel) = get_user_voice_channel_in_server(&target_user.id, &server.id).await?
    {
        revoke_voice_reentry(&channel, &target_user.id).await?;
        remove_user_from_voice_channel(
            voice_client,
            &UserVoiceChannel {
                id: channel,
                server_id: Some(server.id.clone()),
            },
            &target_user.id,
        )
        .await?;
    };

    Ok(Json(member.into()))
}

#[cfg(test)]
mod test {
    use crate::{
        rocket,
        util::test::{PubSubTestHelper, TestHarness},
    };
    use iso8601_timestamp::Timestamp;
    use revolt_database::{
        Channel, Member, PartialMember, Server, Session, User,
        events::client::EventV1,
        voice::{
            UserVoiceChannel, create_voice_state, get_user_voice_channel_in_server,
            set_channel_node,
        },
    };
    use revolt_models::v0;
    use revolt_permissions::{ChannelPermission, OverrideField};
    use rocket::http::{ContentType, Header, Status};

    /// A moderator role (Move Members), a plain member and a target member
    /// sitting in the first of two voice channels.
    struct VoiceServer {
        server: Server,
        from: Channel,
        to: Channel,
        moderator: Session,
        member: Session,
        target: User,
        moderator_role: String,
    }

    async fn voice_channel(
        harness: &TestHarness,
        server: &mut Server,
        max_users: Option<usize>,
    ) -> Channel {
        Channel::create_server_channel(
            &harness.db,
            server,
            v0::DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Voice,
                name: "Voice".to_string(),
                description: None,
                nsfw: Some(false),
                voice: Some(v0::VoiceInformation { max_users }),
                announcement: None,
                category: None,
                private: None,
            },
            true,
        )
        .await
        .expect("`Channel`")
    }

    async fn setup(harness: &TestHarness, max_users: Option<usize>) -> VoiceServer {
        let (_, _, owner) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&owner).await;
        let from = voice_channel(harness, &mut server, None).await;
        let to = voice_channel(harness, &mut server, max_users).await;

        let role = harness
            .new_role(
                &server,
                1,
                Some(OverrideField {
                    a: ChannelPermission::MoveMembers as i64,
                    d: 0,
                }),
            )
            .await;
        let server = harness.db.fetch_server(&server.id).await.unwrap();

        let (_, moderator, moderator_user) = harness.new_user().await;
        let (mut moderator_member, _) = Member::create(&harness.db, &server, &moderator_user, None)
            .await
            .unwrap();
        moderator_member
            .update(
                &harness.db,
                PartialMember {
                    roles: Some(vec![role.id.clone()]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        let (_, member, member_user) = harness.new_user().await;
        Member::create(&harness.db, &server, &member_user, None)
            .await
            .unwrap();

        let (_, _, target) = harness.new_user().await;
        Member::create(&harness.db, &server, &target, None)
            .await
            .unwrap();

        set_channel_node(from.id(), "voicetest").await.unwrap();
        create_voice_state(
            &UserVoiceChannel::from_channel(&from),
            &target.id,
            Timestamp::now_utc(),
        )
        .await
        .unwrap();

        VoiceServer {
            server,
            from,
            to,
            moderator,
            member,
            target,
            moderator_role: role.id,
        }
    }

    async fn edit_target(
        harness: &TestHarness,
        session: &Session,
        setup: &VoiceServer,
        body: serde_json::Value,
    ) -> (Status, String) {
        let response = harness
            .client
            .patch(format!(
                "/servers/{}/members/{}",
                setup.server.id, setup.target.id
            ))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.to_string()))
            .body(body.to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn deny_move_members(harness: &TestHarness, channel: &mut Channel, role: &str) {
        channel
            .set_role_permission(
                &harness.db,
                role,
                OverrideField {
                    a: 0,
                    d: ChannelPermission::MoveMembers as i64,
                },
            )
            .await
            .unwrap();
    }

    #[rocket::async_test]
    async fn member_without_move_members_cannot_move() {
        let harness = TestHarness::new().await;
        let setup = setup(&harness, None).await;

        let (status, body) = edit_target(
            &harness,
            &setup.member,
            &setup,
            json!({ "voice_channel": setup.to.id() }),
        )
        .await;

        assert_eq!(status, Status::Forbidden, "{body}");
        assert!(body.contains("MissingPermission"), "{body}");
        assert_eq!(
            get_user_voice_channel_in_server(&setup.target.id, &setup.server.id)
                .await
                .unwrap()
                .as_deref(),
            Some(setup.from.id())
        );
    }

    #[rocket::async_test]
    async fn member_without_move_members_cannot_disconnect() {
        let harness = TestHarness::new().await;
        let setup = setup(&harness, None).await;

        let (status, body) = edit_target(
            &harness,
            &setup.member,
            &setup,
            json!({ "remove": ["VoiceChannel"] }),
        )
        .await;

        assert_eq!(status, Status::Forbidden, "{body}");
    }

    #[rocket::async_test]
    async fn move_needs_move_members_in_target_channel() {
        let harness = TestHarness::new().await;
        let mut setup = setup(&harness, None).await;
        let role = setup.moderator_role.clone();
        deny_move_members(&harness, &mut setup.to, &role).await;

        let (status, body) = edit_target(
            &harness,
            &setup.moderator,
            &setup,
            json!({ "voice_channel": setup.to.id() }),
        )
        .await;

        assert_eq!(status, Status::Forbidden, "{body}");
        assert!(body.contains("MissingPermission"), "{body}");
    }

    #[rocket::async_test]
    async fn move_needs_move_members_in_source_channel() {
        let harness = TestHarness::new().await;
        let mut setup = setup(&harness, None).await;
        let role = setup.moderator_role.clone();
        deny_move_members(&harness, &mut setup.from, &role).await;

        let (status, body) = edit_target(
            &harness,
            &setup.moderator,
            &setup,
            json!({ "voice_channel": setup.to.id() }),
        )
        .await;

        assert_eq!(status, Status::Forbidden, "{body}");
        assert!(body.contains("MissingPermission"), "{body}");
    }

    #[rocket::async_test]
    async fn move_respects_user_limit() {
        let harness = TestHarness::new().await;
        let setup = setup(&harness, Some(1)).await;

        let (_, _, sitting) = harness.new_user().await;
        Member::create(&harness.db, &setup.server, &sitting, None)
            .await
            .unwrap();
        create_voice_state(
            &UserVoiceChannel::from_channel(&setup.to),
            &sitting.id,
            Timestamp::now_utc(),
        )
        .await
        .unwrap();

        let (status, body) = edit_target(
            &harness,
            &setup.moderator,
            &setup,
            json!({ "voice_channel": setup.to.id() }),
        )
        .await;

        assert!(body.contains("CannotJoinCall"), "{status} {body}");
    }

    #[rocket::async_test]
    async fn move_to_current_channel_is_rejected() {
        let harness = TestHarness::new().await;
        let setup = setup(&harness, None).await;

        let (status, body) = edit_target(
            &harness,
            &setup.moderator,
            &setup,
            json!({ "voice_channel": setup.from.id() }),
        )
        .await;

        assert!(body.contains("AlreadyConnected"), "{status} {body}");
    }

    #[rocket::async_test]
    async fn moderator_moves_member_to_other_channel() {
        let harness = TestHarness::new().await;
        let setup = setup(&harness, None).await;
        // private user events are published on "<user>!"
        let mut pubsub = PubSubTestHelper::new(&format!("{}!", setup.target.id)).await;

        let (status, body) = edit_target(
            &harness,
            &setup.moderator,
            &setup,
            json!({ "voice_channel": setup.to.id() }),
        )
        .await;

        assert_eq!(status, Status::Ok, "{body}");

        let event = pubsub
            .wait_for_event(|event| matches!(event, EventV1::UserMoveVoiceChannel { .. }))
            .await;
        let EventV1::UserMoveVoiceChannel {
            from, to, token, ..
        } = event
        else {
            unreachable!()
        };
        assert_eq!(from, setup.from.id());
        assert_eq!(to, setup.to.id());
        assert!(!token.is_empty());
    }
}

#[cfg(test)]
mod managed_role_test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Member, PartialMember, Role};
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    /// A bot's managed role cannot be given by hand, so it must not be taken
    /// away by hand either: once removed, nobody could restore it and the bot
    /// would stay without its permissions until it is removed and invited again.
    #[rocket::async_test]
    async fn managed_role_cannot_be_removed() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        let (_, _, bot_user) = harness.new_user().await;
        let managed = Role::create_managed(
            &harness.db,
            &server,
            "bot".to_string(),
            bot_user.id.clone(),
        )
        .await
        .unwrap();
        let other = harness.new_role(&server, 5, None).await;
        let server = harness.db.fetch_server(&server.id).await.unwrap();
        let (mut member, _) = Member::create(&harness.db, &server, &bot_user, None)
            .await
            .unwrap();
        member
            .update(
                &harness.db,
                PartialMember {
                    roles: Some(vec![managed.id.clone(), other.id.clone()]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        let edit = |body: serde_json::Value| {
            harness
                .client
                .patch(format!("/servers/{}/members/{}", server.id, bot_user.id))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", owner_session.token.clone()))
                .body(body.to_string())
                .dispatch()
        };

        // dropping the managed role from the list
        let response = edit(json!({ "roles": [other.id] })).await;
        assert_eq!(response.status(), Status::BadRequest);
        assert!(response
            .into_string()
            .await
            .unwrap_or_default()
            .contains("InvalidOperation"));

        // clearing every role
        let response = edit(json!({ "remove": ["Roles"] })).await;
        assert_eq!(response.status(), Status::BadRequest);

        // other roles can still be changed as long as the managed one stays
        let response = edit(json!({ "roles": [managed.id] })).await;
        assert_eq!(response.status(), Status::Ok);

        let member = harness
            .db
            .fetch_member(&server.id, &bot_user.id)
            .await
            .unwrap();
        assert_eq!(member.roles, vec![managed.id.clone()]);
    }
}
