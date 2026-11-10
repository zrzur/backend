//! Stage channels: topic, raised hands, speakers, ending a stage, and who
//! may do what. Voice seats are written straight into Redis (there is no
//! LiveKit in tests), the way voice-ingress records them.
use revolt_database::{
    events::client::EventV1,
    iso8601_timestamp::Timestamp,
    voice::{
        create_voice_state, delete_voice_state, get_stage_state, get_voice_channel_members,
        UserVoiceChannel,
    },
    Channel, Member, Server, Session, User,
};
use revolt_models::v0;
use revolt_result::ErrorType;
use rocket::http::{ContentType, Header, Status};
use serde_json::json;

use crate::util::test::{PubSubTestHelper, TestHarness};

struct Setup {
    harness: TestHarness,
    owner: User,
    owner_session: Session,
    member: User,
    member_session: Session,
    stage: Channel,
    voice: Channel,
}

async fn channel(harness: &TestHarness, server: &mut Server, stage: bool) -> Channel {
    Channel::create_server_channel(
        &harness.db,
        server,
        v0::DataCreateServerChannel {
            channel_type: v0::LegacyServerChannelType::Voice,
            name: if stage { "Talks" } else { "Voice" }.to_string(),
            description: None,
            nsfw: Some(false),
            voice: Some(v0::VoiceInformation {
                max_users: None,
                stage,
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

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (mut server, _) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &member, None)
        .await
        .expect("member");
    let stage = channel(&harness, &mut server, true).await;
    let voice = channel(&harness, &mut server, false).await;
    Setup {
        harness,
        owner,
        owner_session,
        member,
        member_session,
        stage,
        voice,
    }
}

async fn sit_in(channel: &Channel, user_id: &str) -> UserVoiceChannel {
    let seat = UserVoiceChannel::from_channel(channel);
    create_voice_state(&seat, user_id, Timestamp::now_utc())
        .await
        .unwrap();
    seat
}

/// Status, the stage state on success, the error type otherwise
async fn call(
    harness: &TestHarness,
    session: &Session,
    method: &str,
    path: String,
    body: Option<serde_json::Value>,
) -> (Status, Option<v0::StageState>, Option<ErrorType>) {
    let client = &harness.client;
    let mut request = match method {
        "GET" => client.get(path),
        "PUT" => client.put(path),
        "PATCH" => client.patch(path),
        "DELETE" => client.delete(path),
        _ => unreachable!(),
    }
    .header(Header::new("x-session-token", session.token.clone()));
    if let Some(body) = body {
        request = request.header(ContentType::JSON).body(body.to_string());
    }
    let response = request.dispatch().await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    if status == Status::Ok {
        (status, serde_json::from_str(&text).ok(), None)
    } else {
        let error = serde_json::from_str::<revolt_result::Error>(&text)
            .ok()
            .map(|error| error.error_type);
        (status, None, error)
    }
}

#[rocket::async_test]
async fn a_stage_channel_says_so() {
    let s = setup().await;
    let response = s
        .harness
        .client
        .get(format!("/channels/{}", s.stage.id()))
        .header(Header::new("x-session-token", s.member_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let body: serde_json::Value =
        serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
    assert_eq!(body["voice"]["stage"], json!(true));

    // a plain voice channel carries no stage flag at all
    let response = s
        .harness
        .client
        .get(format!("/channels/{}", s.voice.id()))
        .header(Header::new("x-session-token", s.member_session.token.clone()))
        .dispatch()
        .await;
    let body: serde_json::Value =
        serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
    assert!(body["voice"].get("stage").is_none());
}

#[rocket::async_test]
async fn stage_routes_refuse_plain_voice_channels() {
    let s = setup().await;
    let (status, _, error) = call(
        &s.harness,
        &s.owner_session,
        "GET",
        format!("/channels/{}/stage", s.voice.id()),
        None,
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::InvalidOperation)));
}

#[rocket::async_test]
async fn moderators_set_the_topic_and_everyone_hears_of_it() {
    let s = setup().await;
    let mut events = PubSubTestHelper::new(s.stage.id()).await;

    let (status, state, _) = call(
        &s.harness,
        &s.owner_session,
        "PATCH",
        format!("/channels/{}/stage", s.stage.id()),
        Some(json!({ "topic": "  Release party  " })),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(state.unwrap().topic.as_deref(), Some("Release party"));

    let event = events
        .wait_for_event(|event| matches!(event, EventV1::StageUpdate { .. }))
        .await;
    let EventV1::StageUpdate { id, data } = event else {
        unreachable!()
    };
    assert_eq!(id, s.stage.id());
    assert_eq!(data.topic.as_deref(), Some("Release party"));

    // the audience can read it, not change it
    let (status, state, _) = call(
        &s.harness,
        &s.member_session,
        "GET",
        format!("/channels/{}/stage", s.stage.id()),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(state.unwrap().topic.as_deref(), Some("Release party"));

    let (status, _, error) = call(
        &s.harness,
        &s.member_session,
        "PATCH",
        format!("/channels/{}/stage", s.stage.id()),
        Some(json!({ "topic": "Mine now" })),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::MissingPermission { .. })));

    // too long a topic is refused
    let (status, _, _) = call(
        &s.harness,
        &s.owner_session,
        "PATCH",
        format!("/channels/{}/stage", s.stage.id()),
        Some(json!({ "topic": "x".repeat(121) })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn a_hand_goes_up_and_a_moderator_invites_to_speak() {
    let s = setup().await;
    let stage = s.stage.id();

    // only someone in the stage can raise a hand
    let (status, _, error) = call(
        &s.harness,
        &s.member_session,
        "PUT",
        format!("/channels/{stage}/stage/hand"),
        None,
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::NotConnected)));

    sit_in(&s.stage, &s.member.id).await;
    sit_in(&s.stage, &s.owner.id).await;
    let (status, state, _) = call(
        &s.harness,
        &s.member_session,
        "PUT",
        format!("/channels/{stage}/stage/hand"),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(state.unwrap().hands, vec![s.member.id.clone()]);

    // an audience member cannot make anyone a speaker
    let (status, _, _) = call(
        &s.harness,
        &s.member_session,
        "PUT",
        format!("/channels/{stage}/stage/speakers/{}", s.member.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);

    // the moderator invites them: the hand goes down, they speak
    let (status, state, _) = call(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/channels/{stage}/stage/speakers/{}", s.member.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    let state = state.unwrap();
    assert_eq!(state.speakers, vec![s.member.id.clone()]);
    assert!(state.hands.is_empty());

    // a speaker has nothing to raise a hand for
    let (status, _, error) = call(
        &s.harness,
        &s.member_session,
        "PUT",
        format!("/channels/{stage}/stage/hand"),
        None,
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::NoEffect)));

    // they step down themselves
    let (status, state, _) = call(
        &s.harness,
        &s.member_session,
        "DELETE",
        format!("/channels/{stage}/stage/speakers/@me"),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert!(state.unwrap().speakers.is_empty());
}

#[rocket::async_test]
async fn only_moderators_move_others_back_or_lower_their_hands() {
    let s = setup().await;
    let stage = s.stage.id();
    sit_in(&s.stage, &s.member.id).await;
    sit_in(&s.stage, &s.owner.id).await;

    // the moderator speaks; the audience can't move them back
    let (status, _, _) = call(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/channels/{stage}/stage/speakers/@me"),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    let (status, _, _) = call(
        &s.harness,
        &s.member_session,
        "DELETE",
        format!("/channels/{stage}/stage/speakers/{}", s.owner.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);

    // a moderator can lower a hand that stays up too long
    call(
        &s.harness,
        &s.member_session,
        "PUT",
        format!("/channels/{stage}/stage/hand"),
        None,
    )
    .await;
    let (status, state, _) = call(
        &s.harness,
        &s.owner_session,
        "DELETE",
        format!("/channels/{stage}/stage/hands/{}", s.member.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert!(state.unwrap().hands.is_empty());
}

#[rocket::async_test]
async fn leaving_the_call_takes_the_stage_seat_with_it() {
    let s = setup().await;
    let stage = s.stage.id();
    let seat = sit_in(&s.stage, &s.member.id).await;
    sit_in(&s.stage, &s.owner.id).await;
    call(
        &s.harness,
        &s.member_session,
        "PUT",
        format!("/channels/{stage}/stage/hand"),
        None,
    )
    .await;
    call(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/channels/{stage}/stage/speakers/@me"),
        None,
    )
    .await;

    delete_voice_state(&seat, &s.member.id).await.unwrap();
    let state = get_stage_state(stage).await.unwrap();
    assert!(state.hands.is_empty());
    assert_eq!(state.speakers, vec![s.owner.id.clone()]);
}

#[rocket::async_test]
async fn ending_the_stage_empties_it() {
    let s = setup().await;
    let stage = s.stage.id();
    let seat = sit_in(&s.stage, &s.member.id).await;
    sit_in(&s.stage, &s.owner.id).await;
    call(
        &s.harness,
        &s.owner_session,
        "PATCH",
        format!("/channels/{stage}/stage"),
        Some(json!({ "topic": "Q&A" })),
    )
    .await;
    call(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/channels/{stage}/stage/speakers/@me"),
        None,
    )
    .await;

    // the audience can't end it
    let (status, _, _) = call(
        &s.harness,
        &s.member_session,
        "DELETE",
        format!("/channels/{stage}/stage"),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);

    let mut events = PubSubTestHelper::new(stage).await;
    let (status, state, _) = call(
        &s.harness,
        &s.owner_session,
        "DELETE",
        format!("/channels/{stage}/stage"),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(state.unwrap(), v0::StageState::default());
    assert!(get_voice_channel_members(&seat).await.unwrap().is_none());
    events
        .wait_for_event(|event| {
            matches!(event, EventV1::VoiceChannelLeave { user, .. } if *user == s.member.id)
        })
        .await;
}

/// The event a stage sends, as JSON (what clients read)
async fn next_stage_event(events: &mut PubSubTestHelper) -> serde_json::Value {
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        events.wait_for_event(|event| matches!(event, EventV1::StageUpdate { .. })),
    )
    .await
    .expect("a StageUpdate");
    serde_json::to_value(&event).unwrap()["data"].clone()
}

/// A member holding a role of this rank with these permissions
async fn member_with(
    s: &Setup,
    rank: i64,
    allow: &[revolt_permissions::ChannelPermission],
) -> (User, Session) {
    let (_, session, user) = s.harness.new_user().await;
    let server = s.harness.db.fetch_server(s.stage.server().unwrap()).await.unwrap();
    let bits = allow.iter().fold(0i64, |bits, p| bits | *p as i64);
    let mut role = s
        .harness
        .new_role(&server, rank, Some(revolt_permissions::OverrideField { a: bits, d: 0 }))
        .await;
    // the harness leaves the rank as created: set it here
    role.update(
        &s.harness.db,
        &server.id,
        revolt_database::PartialRole {
            rank: Some(rank),
            ..Default::default()
        },
        vec![],
    )
    .await
    .unwrap();
    let server = s.harness.db.fetch_server(&server.id).await.unwrap();
    let (mut member, _) = Member::create(&s.harness.db, &server, &user, None).await.unwrap();
    member
        .update(
            &s.harness.db,
            revolt_database::PartialMember {
                roles: Some(vec![role.id.clone()]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    (user, session)
}

#[rocket::async_test]
async fn changing_the_user_limit_keeps_the_stage() {
    let s = setup().await;
    let response = s
        .harness
        .client
        .patch(format!("/channels/{}", s.stage.id()))
        .header(Header::new("x-session-token", s.owner_session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "voice": { "max_users": 5 } }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let body: serde_json::Value =
        serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
    assert_eq!(body["voice"]["max_users"], json!(5));
    assert_eq!(body["voice"]["stage"], json!(true), "the stage was lost: {body}");
}

#[rocket::async_test]
async fn a_stage_turns_on_or_off_only_while_the_call_is_empty() {
    let s = setup().await;
    let seat = sit_in(&s.stage, &s.member.id).await;
    let turn_off = || {
        s.harness
            .client
            .patch(format!("/channels/{}", s.stage.id()))
            .header(Header::new("x-session-token", s.owner_session.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "voice": { "stage": false } }).to_string())
    };
    assert_eq!(turn_off().dispatch().await.status(), Status::BadRequest);

    delete_voice_state(&seat, &s.member.id).await.unwrap();
    let response = turn_off().dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    let body: serde_json::Value =
        serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
    assert!(body["voice"].get("stage").is_none(), "{body}");
}

#[rocket::async_test]
async fn closing_the_stage_needs_move_members_too() {
    use revolt_permissions::ChannelPermission::*;
    let s = setup().await;
    let (moderator, session) = member_with(&s, 5, &[MuteMembers]).await;
    sit_in(&s.stage, &moderator.id).await;
    let (status, _, _) = call(
        &s.harness,
        &session,
        "DELETE",
        format!("/channels/{}/stage", s.stage.id()),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);
}

#[rocket::async_test]
async fn a_moderator_cannot_move_someone_ranked_higher() {
    use revolt_permissions::ChannelPermission::*;
    let s = setup().await;
    let stage = s.stage.id();
    let (moderator, session) = member_with(&s, 10, &[MuteMembers]).await;
    let (senior, _) = member_with(&s, 1, &[]).await;
    sit_in(&s.stage, &moderator.id).await;
    sit_in(&s.stage, &senior.id).await;

    // the owner gives the senior member the floor; the moderator can't take it
    let (status, _, _) = call(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/channels/{stage}/stage/speakers/{}", senior.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    let (status, _, error) = call(
        &s.harness,
        &session,
        "DELETE",
        format!("/channels/{stage}/stage/speakers/{}", senior.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::NotElevated)), "{error:?}");

    // nor give the floor to the owner, ranked above everyone
    sit_in(&s.stage, &s.owner.id).await;
    let (status, _, error) = call(
        &s.harness,
        &session,
        "PUT",
        format!("/channels/{stage}/stage/speakers/{}", s.owner.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::NotElevated)), "{error:?}");
}

#[rocket::async_test]
async fn a_hand_goes_up_at_most_once_every_twenty_seconds() {
    let s = setup().await;
    let stage = s.stage.id();
    sit_in(&s.stage, &s.member.id).await;
    let raise = || call(&s.harness, &s.member_session, "PUT", format!("/channels/{stage}/stage/hand"), None);
    assert_eq!(raise().await.0, Status::Ok);
    let (status, _, _) = call(
        &s.harness,
        &s.member_session,
        "DELETE",
        format!("/channels/{stage}/stage/hands/@me"),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(raise().await.0, Status::TooManyRequests);
}

#[rocket::async_test]
async fn stage_events_carry_only_what_changed() {
    let s = setup().await;
    let stage = s.stage.id();
    sit_in(&s.stage, &s.member.id).await;
    let mut events = PubSubTestHelper::new(stage).await;
    call(&s.harness, &s.member_session, "PUT", format!("/channels/{stage}/stage/hand"), None).await;
    let data = next_stage_event(&mut events).await;
    assert_eq!(data["hands_raised"], json!([s.member.id]), "{data}");
    assert!(data.get("speakers").is_none(), "the whole state was sent: {data}");
}

#[rocket::async_test]
async fn a_speaker_who_leaves_is_gone_from_the_stage_at_once() {
    let s = setup().await;
    let stage = s.stage.id();
    let seat = sit_in(&s.stage, &s.member.id).await;
    call(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/channels/{stage}/stage/speakers/{}", s.member.id),
        None,
    )
    .await;
    let mut events = PubSubTestHelper::new(stage).await;
    delete_voice_state(&seat, &s.member.id).await.unwrap();
    let data = next_stage_event(&mut events).await;
    assert_eq!(data["speakers_removed"], json!([s.member.id]), "{data}");
}

async fn edit_channel(s: &Setup, body: serde_json::Value) -> (Status, serde_json::Value) {
    let response = s
        .harness
        .client
        .patch(format!("/channels/{}", s.stage.id()))
        .header(Header::new("x-session-token", s.owner_session.token.clone()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

#[rocket::async_test]
async fn the_user_limit_can_be_cleared_again() {
    let s = setup().await;
    let (status, body) = edit_channel(&s, json!({ "voice": { "max_users": 5 } })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["voice"]["max_users"], json!(5));

    // 0 clears the limit
    let (status, body) = edit_channel(&s, json!({ "voice": { "max_users": 0 } })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["voice"].get("max_users").is_none(), "{body}");
    assert_eq!(body["voice"]["stage"], json!(true), "{body}");
}

#[rocket::async_test]
async fn removing_the_voice_info_cannot_drop_an_occupied_stage() {
    let s = setup().await;
    sit_in(&s.stage, &s.member.id).await;
    let (status, body) = edit_channel(
        &s,
        json!({ "remove": ["Voice"], "voice": { "max_users": 3 } }),
    )
    .await;
    // the edit builds on the stored voice information: still a stage
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["voice"]["stage"], json!(true), "the stage was dropped: {body}");
    assert_eq!(body["voice"]["max_users"], json!(3), "{body}");

    // the same payload on a plain voice channel keeps working
    let response = s
        .harness
        .client
        .patch(format!("/channels/{}", s.voice.id()))
        .header(Header::new("x-session-token", s.owner_session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "remove": ["Voice"], "voice": { "max_users": 3 } }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
}

#[rocket::async_test]
async fn a_moderator_cannot_lower_the_hand_of_someone_ranked_higher() {
    use revolt_permissions::ChannelPermission::*;
    let s = setup().await;
    let stage = s.stage.id();
    let (moderator, session) = member_with(&s, 10, &[MuteMembers]).await;
    let (senior, senior_session) = member_with(&s, 1, &[]).await;
    sit_in(&s.stage, &moderator.id).await;
    sit_in(&s.stage, &senior.id).await;
    let (status, _, _) = call(&s.harness, &senior_session, "PUT", format!("/channels/{stage}/stage/hand"), None).await;
    assert_eq!(status, Status::Ok);
    let (status, _, error) = call(
        &s.harness,
        &session,
        "DELETE",
        format!("/channels/{stage}/stage/hands/{}", senior.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::NotElevated)), "{error:?}");
}

#[rocket::async_test]
async fn a_speaker_the_voice_server_cannot_silence_is_reported_to_the_moderator() {
    let s = setup().await;
    let stage = s.stage.id();
    sit_in(&s.stage, &s.member.id).await;
    revolt_database::voice::add_stage_speaker(stage, &s.member.id).await.unwrap();
    // the room is on a node, but LiveKit knows no such participant: neither
    // the update nor taking them out works
    revolt_database::voice::set_channel_node(stage, "voicetest").await.unwrap();
    let (status, _, error) = call(
        &s.harness,
        &s.owner_session,
        "DELETE",
        format!("/channels/{stage}/stage/speakers/{}", s.member.id),
        None,
    )
    .await;
    assert_ne!(status, Status::Ok);
    assert!(matches!(error, Some(ErrorType::LiveKitUnavailable)), "{error:?}");
    revolt_database::voice::delete_channel_node(stage).await.unwrap();
}

#[rocket::async_test]
async fn a_database_error_takes_nobody_out_of_the_call() {
    let s = setup().await;
    let stage = s.stage.id();
    // a seat whose user is gone from the database
    let ghost = TestHarness::rand_string();
    let seat = sit_in(&s.stage, &ghost).await;
    revolt_database::voice::add_stage_speaker(stage, &ghost).await.unwrap();
    revolt_database::voice::set_channel_node(stage, "voicetest").await.unwrap();
    let (status, _, _) = call(
        &s.harness,
        &s.owner_session,
        "DELETE",
        format!("/channels/{stage}/stage/speakers/{ghost}"),
        None,
    )
    .await;
    assert_ne!(status, Status::Ok);
    assert!(
        get_voice_channel_members(&seat).await.unwrap().unwrap_or_default().contains(&ghost),
        "the seat was taken"
    );
    revolt_database::voice::delete_channel_node(stage).await.unwrap();
}
