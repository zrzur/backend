use std::collections::HashMap;

use revolt_config::config;
use revolt_database::{
    events::client::EventV1,
    util::{permissions::perms, reference::Reference},
    voice::{
        channel_older_than, get_voice_channel_members, give_temporary_room,
        remove_temporary_room_if_empty, sync_voice_permissions, temporary_room_owner,
        UserVoiceChannel, VoiceClient, ROOM_GUEST_PERMISSIONS, ROOM_OWNER_PERMISSIONS,
        UNUSED_ROOM_SECS,
    },
    Channel, Database, FieldsChannel, PartialChannel, PartialServer, Server, User,
    VoiceInformation,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, OverrideField};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use ulid::Ulid;

/// People a room's member can let into it while it is locked or hidden
const MAX_GUESTS: usize = 25;

/// # Make a Room
///
/// Asks a room maker (a voice channel with `voice.maker`) for a voice channel
/// of your own. The room is put right after the room maker, with the same
/// permissions and user limit, and carries your name; you can rename it, set
/// its user limit and remove it. It is removed when the last person leaves
/// it, or after a minute if nobody joins it.
///
/// A member has one room per server: asking again returns it. Needs Connect
/// in the room maker. Not for bots. Join the returned channel as any other
/// voice channel.
#[openapi(tag = "Voice")]
#[post("/<target>/rooms")]
pub async fn make_room(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::Channel>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let maker = target.as_channel(db).await?;
    let Channel::TextChannel {
        id: maker_id,
        server: server_id,
        default_permissions,
        role_permissions,
        nsfw,
        voice: Some(maker_voice),
        permissions_synced,
        ..
    } = &maker
    else {
        return Err(create_error!(NotAVoiceChannel));
    };
    if !maker_voice.maker {
        return Err(create_error!(InvalidOperation));
    }

    let mut query = perms(db, &user).channel(&maker);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::Connect)?;
    revolt_database::util::age::assert_can_view_channel(db, &user, &maker).await?;
    revolt_database::util::verification::assert_can_talk(db, &user, &maker).await?;

    let mut server = db.fetch_server(server_id).await?;

    // one room per member and server: the one they have is handed back.
    // Rooms nobody joined in time are cleared on the way.
    let mut mine = None;
    let mut cleared = false;
    for channel in db.fetch_channels(&server.channels).await? {
        match temporary_room_owner(&channel) {
            Some(owner) if owner == user.id => mine = Some(channel),
            Some(_) if channel_older_than(&channel, UNUSED_ROOM_SECS) => {
                cleared |= remove_temporary_room_if_empty(db, &channel).await?;
            }
            _ => {}
        }
    }
    if let Some(room) = mine {
        return Ok(Json(room.into()));
    }
    if cleared {
        // the channel list and the categories without the cleared rooms
        server = db.fetch_server(server_id).await?;
    }

    let limit = config().await.features.limits.global.server_channels;
    if server.channels.len() >= limit {
        return Err(create_error!(TooManyChannels { max: limit }));
    }

    // named after the member, as the server shows them
    let nickname = db
        .fetch_member(&server.id, &user.id)
        .await
        .ok()
        .and_then(|member| member.nickname);
    let name: String = nickname
        .or_else(|| user.display_name.clone())
        .unwrap_or_else(|| user.username.clone());
    // the room maker's own wording, when it has one
    let name: String = match &maker_voice.room_name {
        Some(template) if template.contains("{name}") => template.replace("{name}", &name),
        Some(template) => format!("{template} {name}"),
        None => name,
    }
    .chars()
    .take(32)
    .collect();

    let id = Ulid::new().to_string();
    let room = Channel::TextChannel {
        id: id.clone(),
        server: server.id.clone(),
        name,
        description: None,
        icon: None,
        last_message_id: None,
        // the room maker's permissions: its category's when it follows one
        // (the room lands in the same category), else a copy of its own
        default_permissions: *default_permissions,
        role_permissions: role_permissions.clone(),
        member_permissions: HashMap::from([(
            user.id.clone(),
            OverrideField {
                a: ROOM_OWNER_PERMISSIONS,
                d: 0,
            },
        )]),
        nsfw: *nsfw,
        voice: Some(VoiceInformation {
            max_users: maker_voice.max_users,
            stage: false,
            maker: false,
            temporary: Some(user.id.clone()),
            locked: false,
            hidden: false,
            room_name: None,
        }),
        slowmode: None,
        announcement: false,
        permissions_synced: *permissions_synced,
        parent: None,
        thread: None,
        forum: None,
        ticket: None,
    };
    db.insert_channel(&room).await?;

    // right after the room maker, in its category when it has one
    let mut channels = server.channels.clone();
    let at = channels
        .iter()
        .position(|channel| channel == maker_id)
        .map_or(channels.len(), |at| at + 1);
    channels.insert(at, id.clone());
    let categories = server.categories.clone().and_then(|mut categories| {
        let category = categories
            .iter_mut()
            .find(|category| category.channels.contains(maker_id))?;
        let at = category
            .channels
            .iter()
            .position(|channel| channel == maker_id)
            .map_or(category.channels.len(), |at| at + 1);
        category.channels.insert(at, id.clone());
        Some(categories)
    });
    server
        .update(
            db,
            PartialServer {
                channels: Some(channels),
                categories,
                ..Default::default()
            },
            vec![],
        )
        .await?;

    EventV1::ChannelCreate(room.clone().into())
        .p(server.id.clone())
        .await;

    // a room nobody joins is cleared after a minute
    let db = db.inner().clone();
    let unused = room.clone();
    rocket::tokio::spawn(async move {
        rocket::tokio::time::sleep(std::time::Duration::from_secs(UNUSED_ROOM_SECS)).await;
        if let Err(error) = remove_temporary_room_if_empty(&db, &unused).await {
            log::warn!("could not clear the unused room {}: {error:?}", unused.id());
        }
    });

    Ok(Json(room.into()))
}

/// A temporary room, for the member it was made for or someone who manages
/// permissions in it
async fn room_of(
    db: &Database,
    user: &User,
    target: Reference<'_>,
) -> Result<(Channel, VoiceInformation, Server)> {
    let channel = target.as_channel(db).await?;
    let Channel::TextChannel {
        server,
        voice: Some(voice),
        ..
    } = &channel
    else {
        return Err(create_error!(NotAVoiceChannel));
    };
    let Some(owner) = &voice.temporary else {
        return Err(create_error!(InvalidOperation));
    };
    let voice = voice.clone();
    if owner != &user.id {
        let mut query = perms(db, user).channel(&channel);
        calculate_channel_permissions(&mut query)
            .await
            .throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;
    }
    let server = db.fetch_server(server).await?;
    Ok((channel, voice, server))
}

/// People in the call get the room's new permissions at once. Best effort:
/// the change itself is stored, and the next join applies it anyway.
async fn apply_in_call(db: &Database, voice_client: &VoiceClient, channel: &Channel, server: &Server) {
    if let Err(error) = sync_voice_permissions(db, voice_client, channel, Some(server), None).await {
        log::warn!("room {}: permissions not applied in the call: {error:?}", channel.id());
    }
}

/// # Change a Room
///
/// Locks, hides or hands over a temporary room. Locked: only the people let
/// in (`PUT /channels/{target}/room/guests/{member}`) can join; hidden: only
/// they can see it. People already in the room stay. `owner` hands the room
/// to someone who is in it. For the member the room was made for, or someone
/// who manages permissions in it.
#[openapi(tag = "Voice")]
#[patch("/<target>/room", data = "<data>")]
pub async fn edit_room(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataEditRoom>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    let (mut channel, mut voice, server) = room_of(db, &user, target).await?;

    if data.locked.is_some() || data.hidden.is_some() {
        // the room stops following its category: the lock is its own
        channel.unsync_permissions(db, &server).await?;
        let Channel::TextChannel {
            default_permissions,
            ..
        } = &channel
        else {
            return Err(create_error!(InvalidOperation));
        };
        let mut permissions = default_permissions.unwrap_or(OverrideField { a: 0, d: 0 });
        let mut set = |on: bool, bit: i64| {
            if on {
                permissions.a &= !bit;
                permissions.d |= bit;
            } else {
                permissions.d &= !bit;
            }
        };
        if let Some(locked) = data.locked {
            set(locked, ChannelPermission::Connect as i64);
            voice.locked = locked;
        }
        if let Some(hidden) = data.hidden {
            set(hidden, ChannelPermission::ViewChannel as i64);
            voice.hidden = hidden;
        }
        let empty = permissions.a == 0 && permissions.d == 0;
        channel
            .update(
                db,
                PartialChannel {
                    default_permissions: (!empty).then_some(permissions),
                    voice: Some(voice.clone()),
                    ..Default::default()
                },
                if empty {
                    vec![FieldsChannel::DefaultPermissions]
                } else {
                    vec![]
                },
            )
            .await?;
    }

    if let Some(owner) = data.owner {
        // only to someone who is in the room right now
        let inside = get_voice_channel_members(&UserVoiceChannel::from_channel(&channel))
            .await?
            .unwrap_or_default();
        if !inside.contains(&owner) {
            return Err(create_error!(InvalidOperation));
        }
        if db.fetch_user(&owner).await?.bot.is_some() {
            return Err(create_error!(IsBot));
        }
        give_temporary_room(db, &mut channel, &owner).await?;
    }

    apply_in_call(db, voice_client, &channel, &server).await;
    Ok(Json(channel.into()))
}

/// # Let Someone into a Room
///
/// Lets a member of the server see and join a temporary room while it is
/// locked or hidden (at most 25 people).
#[openapi(tag = "Voice")]
#[put("/<target>/room/guests/<member>")]
pub async fn add_room_guest(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    member: String,
) -> Result<Json<v0::Channel>> {
    let (mut channel, voice, server) = room_of(db, &user, target).await?;
    db.fetch_member(&server.id, &member).await?;
    if voice.temporary.as_deref() == Some(member.as_str()) {
        return Ok(Json(channel.into()));
    }
    if let Channel::TextChannel {
        member_permissions, ..
    } = &channel
    {
        if !member_permissions.contains_key(&member) && member_permissions.len() > MAX_GUESTS {
            return Err(create_error!(InvalidOperation));
        }
    }
    channel
        .set_member_permission(
            db,
            &member,
            OverrideField {
                a: ROOM_GUEST_PERMISSIONS,
                d: 0,
            },
        )
        .await?;
    apply_in_call(db, voice_client, &channel, &server).await;
    Ok(Json(channel.into()))
}

/// # Take Someone off a Room's Guests
///
/// Takes back what `PUT /channels/{target}/room/guests/{member}` gave. The
/// member stays in the room if they are in it.
#[openapi(tag = "Voice")]
#[delete("/<target>/room/guests/<member>")]
pub async fn remove_room_guest(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    member: String,
) -> Result<Json<v0::Channel>> {
    let (mut channel, voice, server) = room_of(db, &user, target).await?;
    if voice.temporary.as_deref() == Some(member.as_str()) {
        return Err(create_error!(InvalidOperation));
    }
    channel
        .set_member_permission(db, &member, OverrideField { a: 0, d: 0 })
        .await?;
    apply_in_call(db, voice_client, &channel, &server).await;
    Ok(Json(channel.into()))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        iso8601_timestamp::Timestamp,
        voice::{
            create_voice_state, delete_voice_state, hand_over_temporary_room,
            remove_temporary_room_if_empty, set_channel_node, UserVoiceChannel,
        },
        util::permissions::DatabasePermissionQuery,
        Channel, Member, PartialChannel, Server, Session, User, VoiceInformation,
    };
    use revolt_models::v0;
    use revolt_permissions::{calculate_channel_permissions, ChannelPermission, OverrideField};
    use rocket::http::{ContentType, Header, Status};

    async fn voice_channel(harness: &TestHarness, server: &mut Server, maker: bool) -> Channel {
        Channel::create_server_channel(
            &harness.db,
            server,
            v0::DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Voice,
                name: "Make a room".to_string(),
                description: None,
                nsfw: Some(false),
                voice: Some(v0::VoiceInformation {
                    max_users: Some(4),
                    maker,
                    ..Default::default()
                }),
                announcement: None,
                category: None,
                private: None,
            },
            true,
        )
        .await
        .expect("`Channel`")
    }

    async fn ask(harness: &TestHarness, session: &Session, maker: &Channel) -> (Status, String) {
        let response = harness
            .client
            .post(format!("/channels/{}/rooms", maker.id()))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn room(harness: &TestHarness, session: &Session, maker: &Channel) -> Channel {
        let (status, body) = ask(harness, session, maker).await;
        assert_eq!(status, Status::Ok, "{body}");
        let room: v0::Channel = serde_json::from_str(&body).unwrap();
        room.into()
    }

    async fn can(harness: &TestHarness, user: &User, channel: &Channel, permission: ChannelPermission) -> bool {
        let channel = harness.db.fetch_channel(channel.id()).await.unwrap();
        let mut query = DatabasePermissionQuery::new(&harness.db, user).channel(&channel);
        calculate_channel_permissions(&mut query)
            .await
            .has_channel_permission(permission)
    }

    async fn sit_in(channel: &Channel, user_id: &str) -> UserVoiceChannel {
        let seat = UserVoiceChannel::from_channel(channel);
        set_channel_node(channel.id(), "voicetest").await.unwrap();
        create_voice_state(&seat, user_id, Timestamp::now_utc())
            .await
            .unwrap();
        seat
    }

    /// An owner, a member, and a server with a room maker
    async fn setup() -> (TestHarness, Server, Channel, Session, User, Session, User) {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, member_session, member) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&owner).await;
        let maker = voice_channel(&harness, &mut server, true).await;
        Member::create(&harness.db, &server, &member, None)
            .await
            .unwrap();
        (harness, server, maker, owner_session, owner, member_session, member)
    }

    #[rocket::async_test]
    async fn a_member_gets_a_room_of_their_own_next_to_the_maker() {
        let (harness, server, maker, _, owner, member_session, member) = setup().await;

        let made = room(&harness, &member_session, &maker).await;
        let Channel::TextChannel { name, voice, member_permissions, .. } = &made else {
            panic!("not a server channel");
        };
        assert_eq!(name, &member.username);
        let voice = voice.as_ref().expect("a voice channel");
        assert_eq!(voice.temporary.as_deref(), Some(member.id.as_str()));
        assert!(!voice.maker);
        assert_eq!(voice.max_users, Some(4));
        assert!(member_permissions.contains_key(&member.id));

        // right after the room maker
        let stored = harness.db.fetch_server(&server.id).await.unwrap();
        let at = stored.channels.iter().position(|id| id == maker.id()).unwrap();
        assert_eq!(stored.channels.get(at + 1).map(String::as_str), Some(made.id()));

        // its member manages it; other members only join it
        assert!(can(&harness, &member, &made, ChannelPermission::ManageChannel).await);
        let (_, _, other) = harness.new_user().await;
        Member::create(&harness.db, &stored, &other, None).await.unwrap();
        assert!(can(&harness, &other, &made, ChannelPermission::Connect).await);
        assert!(!can(&harness, &other, &made, ChannelPermission::ManageChannel).await);
        assert!(can(&harness, &owner, &made, ChannelPermission::ManageChannel).await);
    }

    #[rocket::async_test]
    async fn asking_again_returns_the_same_room() {
        let (harness, server, maker, _, _, member_session, _) = setup().await;
        let first = room(&harness, &member_session, &maker).await;
        let second = room(&harness, &member_session, &maker).await;
        assert_eq!(first.id(), second.id());

        let stored = harness.db.fetch_server(&server.id).await.unwrap();
        assert_eq!(stored.channels.iter().filter(|id| *id == first.id()).count(), 1);
    }

    #[rocket::async_test]
    async fn the_room_goes_when_the_last_person_leaves() {
        let (harness, server, maker, _, owner, member_session, member) = setup().await;
        let made = room(&harness, &member_session, &maker).await;

        let seat = sit_in(&made, &member.id).await;
        sit_in(&made, &owner.id).await;

        // one of two leaves: the room stays
        delete_voice_state(&seat, &member.id).await.unwrap();
        assert!(!remove_temporary_room_if_empty(&harness.db, &made).await.unwrap());
        assert!(harness.db.fetch_channel(made.id()).await.is_ok());

        // the last one leaves: the room and its place in the server go
        delete_voice_state(&seat, &owner.id).await.unwrap();
        assert!(remove_temporary_room_if_empty(&harness.db, &made).await.unwrap());
        assert!(harness.db.fetch_channel(made.id()).await.is_err());
        let stored = harness.db.fetch_server(&server.id).await.unwrap();
        assert!(!stored.channels.iter().any(|id| id == made.id()));
        // the room maker itself is never removed
        assert!(!remove_temporary_room_if_empty(&harness.db, &maker).await.unwrap());
        assert!(harness.db.fetch_channel(maker.id()).await.is_ok());
    }

    async fn patch_room(
        harness: &TestHarness,
        session: &Session,
        room: &Channel,
        body: serde_json::Value,
    ) -> (Status, String) {
        let response = harness
            .client
            .patch(format!("/channels/{}/room", room.id()))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.to_string()))
            .body(body.to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    #[rocket::async_test]
    async fn a_locked_or_hidden_room_is_for_its_member_and_their_guests() {
        let (harness, server, maker, owner_session, _, member_session, member) = setup().await;
        let made = room(&harness, &member_session, &maker).await;
        let (_, other_session, other) = harness.new_user().await;
        let (_, _, guest) = harness.new_user().await;
        for user in [&other, &guest] {
            Member::create(&harness.db, &server, user, None).await.unwrap();
        }

        // someone else in the server has no say over the room
        let (status, body) = patch_room(&harness, &other_session, &made, json!({ "locked": true })).await;
        assert_eq!(status, Status::Forbidden, "{body}");

        let (status, body) = patch_room(&harness, &member_session, &made, json!({ "locked": true })).await;
        assert_eq!(status, Status::Ok, "{body}");
        assert!(harness.db.fetch_channel(made.id()).await.unwrap().voice().unwrap().locked);
        assert!(can(&harness, &other, &made, ChannelPermission::ViewChannel).await);
        assert!(!can(&harness, &other, &made, ChannelPermission::Connect).await);
        assert!(can(&harness, &member, &made, ChannelPermission::Connect).await);

        // a guest is let in; the others stay out
        let response = harness
            .client
            .put(format!("/channels/{}/room/guests/{}", made.id(), guest.id))
            .header(Header::new("x-session-token", member_session.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        assert!(can(&harness, &guest, &made, ChannelPermission::Connect).await);
        assert!(!can(&harness, &other, &made, ChannelPermission::Connect).await);

        // hidden as well: gone for the others, there for the guest
        let (status, body) = patch_room(&harness, &member_session, &made, json!({ "hidden": true })).await;
        assert_eq!(status, Status::Ok, "{body}");
        assert!(!can(&harness, &other, &made, ChannelPermission::ViewChannel).await);
        assert!(can(&harness, &guest, &made, ChannelPermission::ViewChannel).await);
        assert!(can(&harness, &member, &made, ChannelPermission::ViewChannel).await);

        // the guest is taken off again
        let response = harness
            .client
            .delete(format!("/channels/{}/room/guests/{}", made.id(), guest.id))
            .header(Header::new("x-session-token", member_session.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        assert!(!can(&harness, &guest, &made, ChannelPermission::ViewChannel).await);

        // open again: as it was, with nothing left on the channel
        let (status, body) = patch_room(
            &harness,
            &member_session,
            &made,
            json!({ "locked": false, "hidden": false }),
        )
        .await;
        assert_eq!(status, Status::Ok, "{body}");
        assert!(can(&harness, &other, &made, ChannelPermission::Connect).await);
        let stored = harness.db.fetch_channel(made.id()).await.unwrap();
        let voice = stored.voice().unwrap().into_owned();
        assert!(!voice.locked && !voice.hidden);

        // the server's owner manages permissions, so they may lock it too;
        // a plain channel or the room maker is no room
        let (status, _) = patch_room(&harness, &owner_session, &made, json!({ "locked": true })).await;
        assert_eq!(status, Status::Ok);
        let (status, body) = patch_room(&harness, &owner_session, &maker, json!({ "locked": true })).await;
        assert_eq!(status, Status::BadRequest, "{body}");
    }

    #[rocket::async_test]
    async fn a_room_is_handed_over_by_its_member_or_when_they_leave() {
        let (harness, server, maker, _, owner, member_session, member) = setup().await;
        let made = room(&harness, &member_session, &maker).await;
        let (_, _, late) = harness.new_user().await;
        Member::create(&harness.db, &server, &late, None).await.unwrap();

        // not to someone who is not in the room
        let (status, body) = patch_room(&harness, &member_session, &made, json!({ "owner": owner.id })).await;
        assert_eq!(status, Status::BadRequest, "{body}");

        let seat = sit_in(&made, &member.id).await;
        sit_in(&made, &owner.id).await;
        let (status, body) = patch_room(&harness, &member_session, &made, json!({ "owner": owner.id })).await;
        assert_eq!(status, Status::Ok, "{body}");
        let stored = harness.db.fetch_channel(made.id()).await.unwrap();
        assert_eq!(stored.voice().unwrap().temporary.as_deref(), Some(owner.id.as_str()));
        // the member before no longer manages it
        assert!(!can(&harness, &member, &made, ChannelPermission::ManageChannel).await);

        // the new member leaves while others are inside: the one who has
        // been there longest gets the room
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        sit_in(&made, &late.id).await;
        delete_voice_state(&seat, &owner.id).await.unwrap();
        let next = hand_over_temporary_room(&harness.db, &stored, &owner.id).await.unwrap();
        assert_eq!(next.as_deref(), Some(member.id.as_str()));
        assert!(can(&harness, &member, &made, ChannelPermission::ManageChannel).await);

        // someone else leaving changes nothing
        let stored = harness.db.fetch_channel(made.id()).await.unwrap();
        assert_eq!(hand_over_temporary_room(&harness.db, &stored, &late.id).await.unwrap(), None);
    }

    #[rocket::async_test]
    async fn rooms_are_named_as_the_room_maker_says() {
        let (harness, _, maker, owner_session, _, member_session, member) = setup().await;
        let response = harness
            .client
            .patch(format!("/channels/{}", maker.id()))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", owner_session.token.to_string()))
            .body(json!({ "voice": { "room_name": "Room of {name}" } }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);

        let made = room(&harness, &member_session, &maker).await;
        let expected: String = format!("Room of {}", member.username).chars().take(32).collect();
        assert!(matches!(&made, Channel::TextChannel { name, .. } if name == &expected));
    }

    #[rocket::async_test]
    async fn only_a_room_maker_makes_rooms() {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (mut server, channels) = harness.new_server(&owner).await;
        let plain = voice_channel(&harness, &mut server, false).await;

        let (status, body) = ask(&harness, &session, &plain).await;
        assert_eq!(status, Status::BadRequest, "{body}");
        assert!(body.contains("InvalidOperation"), "{body}");

        let (status, body) = ask(&harness, &session, &channels[0]).await;
        assert_ne!(status, Status::Ok, "{body}");
        assert!(body.contains("NotAVoiceChannel"), "{body}");
    }

    #[rocket::async_test]
    async fn someone_who_cannot_join_the_maker_gets_no_room() {
        let (harness, _, mut maker, _, _, member_session, _) = setup().await;
        maker
            .update(
                &harness.db,
                PartialChannel {
                    default_permissions: Some(OverrideField {
                        a: 0,
                        d: ChannelPermission::Connect as i64,
                    }),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        let (status, body) = ask(&harness, &member_session, &maker).await;
        assert_eq!(status, Status::Forbidden, "{body}");

        // nor someone outside the server
        let (_, outsider_session, _) = harness.new_user().await;
        let (status, body) = ask(&harness, &outsider_session, &maker).await;
        assert_ne!(status, Status::Ok, "{body}");
    }

    #[rocket::async_test]
    async fn a_client_cannot_mark_a_channel_temporary_or_make_a_stage_a_maker() {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;

        let create = |voice: serde_json::Value| {
            harness
                .client
                .post(format!("/servers/{}/channels", server.id))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", session.token.to_string()))
                .body(json!({ "type": "Voice", "name": "Room", "voice": voice }).to_string())
        };
        let response = create(json!({ "temporary": owner.id, "maker": true })).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let made: v0::Channel = response.into_json().await.unwrap();
        let made: Channel = made.into();
        let voice: VoiceInformation = made.voice().unwrap().into_owned();
        assert_eq!(voice.temporary, None);
        assert!(voice.maker);

        // the switch in the channel's settings: on, off, and not for a stage
        let edit = |id: String, voice: serde_json::Value| {
            harness
                .client
                .patch(format!("/channels/{id}"))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", session.token.to_string()))
                .body(json!({ "voice": voice }).to_string())
        };
        let response = edit(made.id().to_string(), json!({ "maker": false })).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let stored = harness.db.fetch_channel(made.id()).await.unwrap();
        assert!(!stored.voice().unwrap().maker);

        let response = edit(made.id().to_string(), json!({ "stage": true, "maker": true })).dispatch().await;
        assert_eq!(response.status(), Status::BadRequest);
    }
}
