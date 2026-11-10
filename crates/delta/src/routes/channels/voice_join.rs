use revolt_config::config;
use revolt_database::{
    util::{permissions::perms, reference::Reference},
    voice::{
        clear_voice_reentry, get_channel_node, get_voice_channel_members,
        is_voice_reentry_revoked, is_voice_seat_taken_over, leave_other_voice_channels,
        raise_if_in_voice, set_call_notification_recipients, set_channel_node,
        UserVoiceChannel, VoiceClient,
    },
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};

use rocket::{serde::json::Json, State};

/// # Join Call
///
/// Asks the voice server for a token to join the call.
#[openapi(tag = "Voice")]
#[post("/<target>/join_call", data = "<data>")]
pub async fn call(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataJoinCall>,
) -> Result<Json<v0::CreateVoiceUserResponse>> {
    if !voice_client.is_enabled() {
        return Err(create_error!(LiveKitUnavailable));
    }

    let v0::DataJoinCall {
        node,
        force_disconnect,
        recipients,
        previous_session,
    } = data.into_inner();

    if user.bot.is_some() && force_disconnect == Some(true) {
        return Err(create_error!(IsBot));
    }

    let channel = target.as_channel(db).await?;

    let Some(voice_info) = channel.voice() else {
        return Err(create_error!(NotAVoiceChannel));
    };

    let mut permissions = perms(db, &user).channel(&channel);

    let current_permissions = calculate_channel_permissions(&mut permissions).await;
    current_permissions.throw_if_lacking_channel_permission(ChannelPermission::Connect)?;

    // Mature channels are for adults only
    revolt_database::util::age::assert_can_view_channel(db, &user, &channel).await?;

    // The server's verification level (members without a role)
    revolt_database::util::verification::assert_can_talk(db, &user, &channel).await?;

    // A device rejoining after a network drop must not push out the tab or
    // device the user joined from meanwhile. LiveKit would otherwise accept
    // its old token and replace the newer session of the same identity.
    // Nor may it undo a moderator disconnect/move or a switch to another
    // room that happened while it was offline (those revoke re-entry).
    if let Some(previous_session) = &previous_session {
        if is_voice_seat_taken_over(&user.id, previous_session).await? {
            return Err(create_error!(AlreadyConnected));
        }
        if is_voice_reentry_revoked(channel.id(), &user.id).await? {
            return Err(create_error!(NotConnected));
        }
    }

    // A moderator disconnect revokes the previous token. An authorized join_call
    // is the supported way back in, and is not a cooldown.
    clear_voice_reentry(channel.id(), &user.id).await?;

    let user_voice_channel = UserVoiceChannel::from_channel(&channel);

    // Occupancy is Redis `vc_members` (updated by voice-ingress on LiveKit
    // participant_joined). Exclude this user so a reconnect is not blocked by
    // their own seat. Users with ManageChannel may exceed the limit.
    if let Some(max_users) = voice_info.max_users {
        let occupancy = get_voice_channel_members(&user_voice_channel)
            .await?
            .map(|members| {
                members
                    .iter()
                    .filter(|id| id.as_str() != user.id)
                    .count()
            })
            .unwrap_or(0);

        if occupancy >= max_users
            && !current_permissions.has(ChannelPermission::ManageChannel as u64)
        {
            return Err(create_error!(CannotJoinCall));
        }
    }

    let existing_node = get_channel_node(channel.id()).await?;
    let has_existing_node = existing_node.is_some(); // we move existing_node in the next statement so this is the quickest way to know if we need to set it.

    let node = existing_node
        .or(node)
        .ok_or_else(|| create_error!(UnknownNode))?;

    let config = config().await;

    let node_host = config
        .hosts
        .livekit
        .get(&node)
        .ok_or_else(|| create_error!(UnknownNode))?
        .clone();

    if force_disconnect == Some(true) {
        // Joining a channel leaves the previous one, in any server, DM or
        // group, also when it was joined from another tab or device. A
        // rejoin of the same channel keeps its seat: LiveKit replaces the
        // older session of the same identity itself.
        leave_other_voice_channels(voice_client, &user.id, channel.id(), None).await?;
    } else {
        raise_if_in_voice(&user, &user_voice_channel).await?;
    }

    let token = voice_client
        .create_token(&node, db, &user, current_permissions, &channel)
        .await?;

    let room = voice_client.create_room(&node, &channel).await?;

    if !has_existing_node {
        set_channel_node(channel.id(), &node).await?;
    }

    log::debug!("Created room {}", room.name);

    if let Some(recipients) = recipients {
        if room.num_participants == 0 && !recipients.is_empty() {
            set_call_notification_recipients(channel.id(), &user.id, &recipients).await?;
        }
    }

    Ok(Json(v0::CreateVoiceUserResponse {
        token,
        url: node_host.clone(),
    }))
}

#[cfg(test)]
mod test {
    use crate::{
        rocket,
        util::test::{PubSubTestHelper, TestHarness},
    };
    use revolt_database::{
        Channel, Member, Server, Session,
        events::client::EventV1,
        iso8601_timestamp::Timestamp,
        voice::{
            UserVoiceChannel, create_voice_state, delete_voice_state, get_user_voice_channels,
            get_voice_state, is_voice_reentry_revoked, revoke_voice_reentry, set_channel_node,
            set_voice_session,
        },
    };
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};

    async fn voice_channel(harness: &TestHarness, server: &mut Server) -> Channel {
        Channel::create_server_channel(
            &harness.db,
            server,
            v0::DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Voice,
                name: "Voice".to_string(),
                description: None,
                nsfw: Some(false),
                voice: Some(v0::VoiceInformation { max_users: None, stage: false }),
                announcement: None,
                category: None,
                private: None,
            },
            true,
        )
        .await
        .expect("`Channel`")
    }

    async fn join(harness: &TestHarness, session: &Session, channel: &Channel) -> Status {
        rejoin(harness, session, channel, None).await.0
    }

    async fn rejoin(
        harness: &TestHarness,
        session: &Session,
        channel: &Channel,
        previous_session: Option<&str>,
    ) -> (Status, String) {
        let response = harness
            .client
            .post(format!("/channels/{}/join_call", channel.id()))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.to_string()))
            .body(
                json!({
                    "node": "voicetest",
                    "force_disconnect": true,
                    "previous_session": previous_session,
                })
                .to_string(),
            )
            .dispatch()
            .await;
        let status = response.status();
        let body = response.into_string().await.unwrap_or_default();
        if status != Status::Ok {
            eprintln!("join_call: {body}");
        }
        (status, body)
    }

    async fn member_in_voice_server(harness: &TestHarness) -> (Session, String, Channel) {
        let (_, session, user) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&user).await;
        let channel = voice_channel(harness, &mut server).await;
        Member::create(&harness.db, &server, &user, None)
            .await
            .unwrap();
        sit_in(&channel, &user.id).await;
        (session, user.id, channel)
    }

    #[rocket::async_test]
    async fn rejoin_after_drop_keeps_own_seat() {
        let harness = TestHarness::new().await;
        let (session, user_id, channel) = member_in_voice_server(&harness).await;
        set_voice_session(channel.id(), &user_id, "PA_this_device")
            .await
            .unwrap();

        let (status, body) = rejoin(&harness, &session, &channel, Some("PA_this_device")).await;
        assert_eq!(status, Status::Ok, "{body}");
    }

    #[rocket::async_test]
    async fn rejoin_after_drop_does_not_push_out_newer_device() {
        let harness = TestHarness::new().await;
        let (session, user_id, channel) = member_in_voice_server(&harness).await;
        // another tab/device joined while this one was offline
        set_voice_session(channel.id(), &user_id, "PA_newer_device")
            .await
            .unwrap();

        let (status, body) = rejoin(&harness, &session, &channel, Some("PA_old_device")).await;
        assert_ne!(status, Status::Ok);
        assert!(body.contains("AlreadyConnected"), "{body}");
        // the newer device keeps its seat
        assert_eq!(
            get_user_voice_channels(&user_id).await.unwrap(),
            vec![UserVoiceChannel::from_channel(&channel)]
        );
    }

    #[rocket::async_test]
    async fn rejoin_after_drop_respects_moderator_disconnect() {
        let harness = TestHarness::new().await;
        let (session, user_id, channel) = member_in_voice_server(&harness).await;
        delete_voice_state(&UserVoiceChannel::from_channel(&channel), &user_id)
            .await
            .unwrap();
        revoke_voice_reentry(channel.id(), &user_id).await.unwrap();

        let (status, body) = rejoin(&harness, &session, &channel, Some("PA_old_device")).await;
        assert_ne!(status, Status::Ok);
        assert!(body.contains("NotConnected"), "{body}");
        // still revoked: the stale token stays useless
        assert!(
            is_voice_reentry_revoked(channel.id(), &user_id)
                .await
                .unwrap()
        );

        // an explicit join is still the way back in
        assert_eq!(join(&harness, &session, &channel).await, Status::Ok);
    }

    async fn sit_in(channel: &Channel, user_id: &str) -> UserVoiceChannel {
        let seat = UserVoiceChannel::from_channel(channel);
        set_channel_node(channel.id(), "voicetest").await.unwrap();
        create_voice_state(&seat, user_id, Timestamp::now_utc())
            .await
            .unwrap();
        seat
    }

    #[rocket::async_test]
    async fn joining_leaves_voice_channel_in_other_server() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (mut first, _) = harness.new_server(&user).await;
        let (mut second, _) = harness.new_server(&user).await;
        let old = voice_channel(&harness, &mut first).await;
        let new = voice_channel(&harness, &mut second).await;
        Member::create(&harness.db, &second, &user, None)
            .await
            .unwrap();

        sit_in(&old, &user.id).await;
        let mut pubsub = PubSubTestHelper::new(old.id()).await;

        assert_eq!(join(&harness, &session, &new).await, Status::Ok);

        assert!(
            get_user_voice_channels(&user.id)
                .await
                .unwrap()
                .iter()
                .all(|channel| channel.id != old.id())
        );
        // the old tab's LiveKit reconnect must not take the seat back
        assert!(is_voice_reentry_revoked(old.id(), &user.id).await.unwrap());
        assert!(!is_voice_reentry_revoked(new.id(), &user.id).await.unwrap());

        pubsub
            .wait_for_event(|event| {
                matches!(event, EventV1::VoiceChannelLeave { id, user: left }
                    if id == old.id() && left == &user.id)
            })
            .await;
    }

    #[rocket::async_test]
    async fn joining_server_voice_leaves_group_call() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let group = Channel::create_group(
            &harness.db,
            v0::DataCreateGroup {
                ..Default::default()
            },
            user.id.clone(),
        )
        .await
        .unwrap();
        let (mut server, _) = harness.new_server(&user).await;
        let new = voice_channel(&harness, &mut server).await;
        Member::create(&harness.db, &server, &user, None)
            .await
            .unwrap();

        sit_in(&group, &user.id).await;

        assert_eq!(join(&harness, &session, &new).await, Status::Ok);
        assert!(get_user_voice_channels(&user.id).await.unwrap().is_empty());
    }

    #[rocket::async_test]
    async fn late_leave_of_previous_channel_keeps_current_state() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&user).await;
        let old = voice_channel(&harness, &mut server).await;
        let new = voice_channel(&harness, &mut server).await;

        let old_seat = sit_in(&old, &user.id).await;
        let new_seat = sit_in(&new, &user.id).await;

        // participant_left for the old room arrives after the new join
        delete_voice_state(&old_seat, &user.id).await.unwrap();

        assert!(
            get_voice_state(&new_seat, &user.id)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            get_user_voice_channels(&user.id).await.unwrap(),
            vec![new_seat.clone()]
        );

        delete_voice_state(&new_seat, &user.id).await.unwrap();
        assert!(
            get_voice_state(&new_seat, &user.id)
                .await
                .unwrap()
                .is_none()
        );
    }
}
