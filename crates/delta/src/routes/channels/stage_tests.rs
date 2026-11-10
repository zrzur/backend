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
