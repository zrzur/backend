//! Soundboard: listing, adding (limits, files), editing, removing, playing
//! into a call, and who may do what.
use revolt_database::{
    events::client::EventV1,
    iso8601_timestamp::Timestamp,
    voice::{add_stage_speaker, create_voice_state, UserVoiceChannel},
    Channel, File, Member, Metadata, PartialMember, Server, Session, User,
};
use revolt_models::v0;
use revolt_permissions::ChannelPermission;
use revolt_result::ErrorType;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use crate::util::test::{PubSubTestHelper, TestHarness};

struct Setup {
    harness: TestHarness,
    owner: User,
    owner_session: Session,
    member: User,
    member_session: Session,
    server: Server,
    voice: Channel,
}

async fn voice_channel(harness: &TestHarness, server: &mut Server, stage: bool) -> Channel {
    Channel::create_server_channel(
        &harness.db,
        server,
        v0::DataCreateServerChannel {
            channel_type: v0::LegacyServerChannelType::Voice,
            name: "Voice".to_string(),
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
    let voice = voice_channel(&harness, &mut server, false).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    Setup {
        harness,
        owner,
        owner_session,
        member,
        member_session,
        server,
        voice,
    }
}

/// A file as autumn stores it after an upload to `tag`
/// A file as autumn stores it after an upload to `tag` (autumn measures
/// every sound: 1.8 s here)
async fn upload(harness: &TestHarness, user: &User, tag: &str, audio: bool) -> String {
    upload_measured(harness, user, tag, audio, audio.then_some(1800)).await
}

/// An upload whose length autumn measured
async fn upload_measured(
    harness: &TestHarness,
    user: &User,
    tag: &str,
    audio: bool,
    measured: Option<u32>,
) -> String {
    let id = ulid::Ulid::new().to_string();
    harness
        .db
        .insert_attachment(&File {
            id: id.clone(),
            tag: tag.to_string(),
            filename: "tada.mp3".to_string(),
            hash: None,
            uploaded_at: None,
            uploader_id: Some(user.id.clone()),
            used_for: None,
            deleted: None,
            reported: None,
            purged: None,
            metadata: if audio {
                Metadata::Audio { duration_ms: measured }
            } else {
                Metadata::Image {
                    width: 16,
                    height: 16,
                    thumbhash: None,
                    animated: None,
                }
            },
            content_type: if audio { "audio/mpeg" } else { "image/png" }.to_string(),
            size: 20_000,
            message_id: None,
            user_id: None,
            server_id: None,
            object_id: None,
            forwarded_from: None,
        })
        .await
        .expect("attachment");
    id
}

async fn send(
    harness: &TestHarness,
    session: &Session,
    method: &str,
    path: String,
    body: Option<Value>,
) -> (Status, Value) {
    let client = &harness.client;
    let mut request = match method {
        "GET" => client.get(path),
        "PUT" => client.put(path),
        "POST" => client.post(path),
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
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn error_type(body: &Value) -> ErrorType {
    serde_json::from_value::<revolt_result::Error>(body.clone())
        .expect("an error")
        .error_type
}

async fn add_sound(s: &Setup, name: &str) -> (Status, Value) {
    let file = upload(&s.harness, &s.owner, "sounds", true).await;
    send(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/servers/{}/soundboard/{file}", s.server.id),
        Some(json!({ "name": name, "emoji": "🎉", "duration_ms": 1800 })),
    )
    .await
}

async fn sit_in(channel: &Channel, user_id: &str) {
    create_voice_state(
        &UserVoiceChannel::from_channel(channel),
        user_id,
        Timestamp::now_utc(),
    )
    .await
    .unwrap();
}

#[rocket::async_test]
async fn members_see_the_soundboard_and_its_limit() {
    let s = setup().await;
    let (status, body) = send(
        &s.harness,
        &s.member_session,
        "GET",
        format!("/servers/{}/soundboard", s.server.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(body, json!({ "sounds": [], "limit": 8 }));

    // someone outside the server sees nothing
    let (_, outsider, _) = s.harness.new_user().await;
    let (status, _) = send(
        &s.harness,
        &outsider,
        "GET",
        format!("/servers/{}/soundboard", s.server.id),
        None,
    )
    .await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn managers_add_a_sound_and_the_server_hears_of_it() {
    let s = setup().await;
    let mut events = PubSubTestHelper::new(&s.server.id).await;

    let (status, body) = add_sound(&s, " Tada ").await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["name"], "Tada");
    assert_eq!(body["volume"], 100);
    assert_eq!(body["duration_ms"], 1800);
    assert_eq!(body["creator_id"], s.owner.id.as_str());

    let event = events
        .wait_for_event(|event| matches!(event, EventV1::SoundboardSoundCreate(_)))
        .await;
    let EventV1::SoundboardSoundCreate(sound) = event else {
        unreachable!()
    };
    assert_eq!(sound.server, s.server.id);

    let (_, list) = send(
        &s.harness,
        &s.member_session,
        "GET",
        format!("/servers/{}/soundboard", s.server.id),
        None,
    )
    .await;
    assert_eq!(list["sounds"].as_array().unwrap().len(), 1);
}

#[rocket::async_test]
async fn members_without_manage_soundboard_cannot_add() {
    let s = setup().await;
    let file = upload(&s.harness, &s.member, "sounds", true).await;
    let (status, body) = send(
        &s.harness,
        &s.member_session,
        "PUT",
        format!("/servers/{}/soundboard/{file}", s.server.id),
        Some(json!({ "name": "mine", "duration_ms": 900 })),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error_type(&body), ErrorType::MissingPermission { .. }));
}

#[rocket::async_test]
async fn only_short_sounds_from_the_sounds_tag() {
    let s = setup().await;

    // too long
    let file = upload(&s.harness, &s.owner, "sounds", true).await;
    let (status, _) = send(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/servers/{}/soundboard/{file}", s.server.id),
        Some(json!({ "name": "long", "duration_ms": 9000 })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);

    // a file uploaded for something else
    let picture = upload(&s.harness, &s.owner, "emojis", false).await;
    let (status, _) = send(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/servers/{}/soundboard/{picture}", s.server.id),
        Some(json!({ "name": "pic", "duration_ms": 900 })),
    )
    .await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn a_server_without_wings_has_eight_sounds() {
    let s = setup().await;
    for n in 0..8 {
        let (status, body) = add_sound(&s, &format!("s{n}")).await;
        assert_eq!(status, Status::Ok, "{body}");
    }
    let (status, body) = add_sound(&s, "one too many").await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error_type(&body), ErrorType::TooManySounds { max: 8 }));
}

#[rocket::async_test]
async fn managers_edit_and_remove_sounds() {
    let s = setup().await;
    let (_, sound) = add_sound(&s, "Tada").await;
    let id = sound["_id"].as_str().unwrap().to_string();

    let (status, body) = send(
        &s.harness,
        &s.owner_session,
        "PATCH",
        format!("/servers/{}/soundboard/{id}", s.server.id),
        Some(json!({ "name": "Fanfare", "volume": 40 })),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(body["name"], "Fanfare");
    assert_eq!(body["volume"], 40);

    let (status, _) = send(
        &s.harness,
        &s.member_session,
        "DELETE",
        format!("/servers/{}/soundboard/{id}", s.server.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);

    let (status, _) = send(
        &s.harness,
        &s.owner_session,
        "DELETE",
        format!("/servers/{}/soundboard/{id}", s.server.id),
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent);
    assert!(s.harness.db.fetch_soundboard_sound(&id).await.is_err());
    let file = s.harness.db.fetch_attachment("sounds", &id).await;
    assert!(file.map(|file| file.deleted == Some(true)).unwrap_or(true));
}

#[rocket::async_test]
async fn playing_a_sound_reaches_the_call() {
    let s = setup().await;
    let (_, sound) = add_sound(&s, "Tada").await;
    let id = sound["_id"].as_str().unwrap().to_string();
    let play = format!("/channels/{}/soundboard/{id}", s.voice.id());

    // only from inside the call
    let (status, body) = send(&s.harness, &s.member_session, "POST", play.clone(), None).await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error_type(&body), ErrorType::NotConnected));

    sit_in(&s.voice, &s.member.id).await;
    // the people in the call hear it (here: the one playing)
    let mut events = PubSubTestHelper::new(&s.member.id).await;
    let (status, _) = send(&s.harness, &s.member_session, "POST", play.clone(), None).await;
    assert_eq!(status, Status::NoContent);
    let event = events
        .wait_for_event(|event| matches!(event, EventV1::SoundboardPlay { .. }))
        .await;
    let EventV1::SoundboardPlay {
        channel_id,
        sound_id,
        user,
    } = event
    else {
        unreachable!()
    };
    assert_eq!(channel_id, s.voice.id());
    assert_eq!(sound_id, id);
    assert_eq!(user, s.member.id);

    // a server-muted member stays silent
    Member::update(
        &mut s.harness.db.fetch_member(&s.server.id, &s.member.id).await.unwrap(),
        &s.harness.db,
        PartialMember {
            can_publish: Some(false),
            ..Default::default()
        },
        vec![],
    )
    .await
    .unwrap();
    let (status, _) = send(&s.harness, &s.member_session, "POST", play, None).await;
    assert_eq!(status, Status::Forbidden);
}

#[rocket::async_test]
async fn use_soundboard_can_be_taken_away_in_a_channel() {
    let s = setup().await;
    let (_, sound) = add_sound(&s, "Tada").await;
    let id = sound["_id"].as_str().unwrap().to_string();
    sit_in(&s.voice, &s.member.id).await;

    let (status, _) = send(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/channels/{}/permissions/default", s.voice.id()),
        Some(json!({ "permissions": { "allow": 0, "deny": ChannelPermission::UseSoundboard as u64 } })),
    )
    .await;
    assert_eq!(status, Status::Ok);

    let (status, body) = send(
        &s.harness,
        &s.member_session,
        "POST",
        format!("/channels/{}/soundboard/{id}", s.voice.id()),
        None,
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error_type(&body), ErrorType::MissingPermission { .. }));
}

#[rocket::async_test]
async fn on_a_stage_only_speakers_play_sounds() {
    let mut s = setup().await;
    let stage = voice_channel(&s.harness, &mut s.server, true).await;
    let (_, sound) = add_sound(&s, "Tada").await;
    let id = sound["_id"].as_str().unwrap().to_string();
    sit_in(&stage, &s.member.id).await;
    let play = format!("/channels/{}/soundboard/{id}", stage.id());

    let (status, _) = send(&s.harness, &s.member_session, "POST", play.clone(), None).await;
    assert_eq!(status, Status::Forbidden);

    add_stage_speaker(stage.id(), &s.member.id).await.unwrap();
    let (status, _) = send(&s.harness, &s.member_session, "POST", play, None).await;
    assert_eq!(status, Status::NoContent);
}

#[rocket::async_test]
async fn a_sound_plays_only_in_its_own_server() {
    let s = setup().await;
    let (_, sound) = add_sound(&s, "Tada").await;
    let id = sound["_id"].as_str().unwrap().to_string();

    let (mut other, _) = s.harness.new_server(&s.member).await;
    let elsewhere = voice_channel(&s.harness, &mut other, false).await;
    Member::create(&s.harness.db, &other, &s.member, None)
        .await
        .ok();
    sit_in(&elsewhere, &s.member.id).await;

    let (status, _) = send(
        &s.harness,
        &s.member_session,
        "POST",
        format!("/channels/{}/soundboard/{id}", elsewhere.id()),
        None,
    )
    .await;
    assert_eq!(status, Status::NotFound);
}

#[test]
fn new_servers_may_use_the_soundboard() {
    let default = *revolt_permissions::DEFAULT_PERMISSION_SERVER;
    assert_eq!(
        default & ChannelPermission::UseSoundboard as u64,
        ChannelPermission::UseSoundboard as u64
    );
    assert_eq!(default & ChannelPermission::ManageSoundboard as u64, 0);
}

#[rocket::async_test]
async fn a_sound_is_heard_by_the_people_in_the_call() {
    let s = setup().await;
    let (_, sound) = add_sound(&s, "Tada").await;
    let id = sound["_id"].as_str().unwrap().to_string();
    sit_in(&s.voice, &s.member.id).await;
    sit_in(&s.voice, &s.owner.id).await;

    // the listener gets it on their own topic, not through the channel
    let mut listener = PubSubTestHelper::new(&s.owner.id).await;
    let (status, _) = send(
        &s.harness,
        &s.member_session,
        "POST",
        format!("/channels/{}/soundboard/{id}", s.voice.id()),
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        listener.wait_for_event(|event| matches!(event, EventV1::SoundboardPlay { .. })),
    )
    .await
    .expect("the listener heard nothing");
}

#[rocket::async_test]
async fn the_measured_length_wins_over_the_one_sent() {
    let s = setup().await;
    let file = upload_measured(&s.harness, &s.owner, "sounds", true, Some(1200)).await;
    let (status, body) = send(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/servers/{}/soundboard/{file}", s.server.id),
        Some(json!({ "name": "short", "duration_ms": 4000 })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["duration_ms"], 1200);

    // a file autumn measured as too long is refused, whatever the client says
    let long = upload_measured(&s.harness, &s.owner, "sounds", true, Some(6000)).await;
    let (status, _) = send(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/servers/{}/soundboard/{long}", s.server.id),
        Some(json!({ "name": "long", "duration_ms": 1000 })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn adding_and_removing_a_sound_is_in_the_audit_log() {
    let s = setup().await;
    let (_, sound) = add_sound(&s, "Tada").await;
    let id = sound["_id"].as_str().unwrap().to_string();
    let (status, _) = send(
        &s.harness,
        &s.owner_session,
        "DELETE",
        format!("/servers/{}/soundboard/{id}", s.server.id),
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent);

    let (status, log) = send(
        &s.harness,
        &s.owner_session,
        "GET",
        format!("/servers/{}/audit_logs", s.server.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{log}");
    let kinds: Vec<String> = log["audit_logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["action"]["type"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(kinds.contains(&"SoundboardSoundCreate".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"SoundboardSoundDelete".to_string()), "{kinds:?}");
}

#[rocket::async_test]
async fn a_sound_autumn_did_not_measure_is_refused() {
    let s = setup().await;
    let file = upload_measured(&s.harness, &s.owner, "sounds", true, None).await;
    let (status, body) = send(
        &s.harness,
        &s.owner_session,
        "PUT",
        format!("/servers/{}/soundboard/{file}", s.server.id),
        Some(json!({ "name": "unmeasured", "duration_ms": 1000 })),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{body}");
}

#[rocket::async_test]
async fn two_uploads_at_once_cannot_pass_the_limit() {
    let s = setup().await;
    for n in 0..7 {
        let (status, _) = add_sound(&s, &format!("s{n}")).await;
        assert_eq!(status, Status::Ok);
    }
    // one place left, two managers add a sound at the same moment
    let first = upload(&s.harness, &s.owner, "sounds", true).await;
    let second = upload(&s.harness, &s.owner, "sounds", true).await;
    let put = |file: String| {
        let harness = &s.harness;
        let session = &s.owner_session;
        let server = s.server.id.clone();
        async move {
            send(
                harness,
                session,
                "PUT",
                format!("/servers/{server}/soundboard/{file}"),
                Some(json!({ "name": "race", "duration_ms": 900 })),
            )
            .await
            .0
        }
    };
    let (a, b) = futures::join!(put(first), put(second));
    let ok = [a, b].iter().filter(|status| **status == Status::Ok).count();
    assert!(ok <= 1, "both took the last place: {a} {b}");
    // counted in the database (the API's read bucket may be spent by now)
    let count = s.harness.db.count_soundboard_sounds(&s.server.id).await.unwrap();
    assert_eq!(count, 8, "a: {a}, b: {b}");
}
