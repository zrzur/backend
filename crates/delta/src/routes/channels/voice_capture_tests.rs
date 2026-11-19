//! Voice clips and recorded rooms: the flags, who may switch what, consent,
//! the monthly limits, and who can delete. Voice seats are written straight
//! into Redis (there is no LiveKit in tests), the way voice-ingress records
//! them; "uploaded" files are inserted the way the file server records them.
use revolt_config::overwrite_config;
use revolt_database::{
    events::client::EventV1,
    iso8601_timestamp::Timestamp,
    voice::{capture_after_leave, create_voice_state, delete_voice_state, UserVoiceChannel},
    Channel, File, Member, Metadata, PartialServer, Server, Session, User,
};
use revolt_models::v0;
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
    room: Channel,
    text: Channel,
}

/// Both features on, billing in its built-in test mode so Wings levels count
async fn features_on() {
    overwrite_config(|config| {
        config.features.voice_clips_enabled = true;
        config.features.voice_recordings_enabled = true;
        config.features.billing.enabled = true;
        config.features.billing.test_mode = true;
        config.features.billing.provider.name = String::new();
    })
    .await;
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (mut server, channels) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &member, None)
        .await
        .expect("member");
    let room = Channel::create_server_channel(
        &harness.db,
        &mut server,
        v0::DataCreateServerChannel {
            channel_type: v0::LegacyServerChannelType::Voice,
            name: "Voice".to_string(),
            description: None,
            nsfw: Some(false),
            voice: Some(v0::VoiceInformation::default()),
            announcement: None,
            category: None,
            private: None,
        },
        true,
    )
    .await
    .expect("`Channel`");
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    Setup {
        harness,
        owner,
        owner_session,
        member,
        member_session,
        server,
        room,
        text: channels[0].clone(),
    }
}

async fn sit_in(channel: &Channel, user_id: &str) -> UserVoiceChannel {
    let seat = UserVoiceChannel::from_channel(channel);
    create_voice_state(&seat, user_id, Timestamp::now_utc())
        .await
        .unwrap();
    seat
}

/// Status, the JSON body on success, the error type otherwise
async fn call(
    harness: &TestHarness,
    session: &Session,
    method: &str,
    path: String,
    body: Option<Value>,
) -> (Status, Value, Option<ErrorType>) {
    let client = &harness.client;
    let mut request = match method {
        "GET" => client.get(path),
        "PUT" => client.put(path),
        "POST" => client.post(path),
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
    if status.class().is_success() {
        (status, serde_json::from_str(&text).unwrap_or(Value::Null), None)
    } else {
        let error = serde_json::from_str::<revolt_result::Error>(&text)
            .ok()
            .map(|error| error.error_type);
        (status, Value::Null, error)
    }
}

/// An audio file the user "uploaded" and has not used yet
async fn audio(harness: &TestHarness, uploader: &User, size: isize) -> String {
    file(harness, uploader, size, "audio/ogg").await
}

async fn file(harness: &TestHarness, uploader: &User, size: isize, content_type: &str) -> String {
    let id = ulid::Ulid::new().to_string();
    harness
        .db
        .insert_attachment(&File {
            id: id.clone(),
            tag: "attachments".to_string(),
            filename: "clip.ogg".to_string(),
            hash: None,
            uploaded_at: Some(Timestamp::now_utc()),
            uploader_id: Some(uploader.id.clone()),
            used_for: None,
            deleted: None,
            reported: None,
            purged: None,
            metadata: Metadata::Audio { duration_ms: None },
            content_type: content_type.to_string(),
            size,
            message_id: None,
            user_id: None,
            server_id: None,
            object_id: None,
            forwarded_from: None,
        })
        .await
        .unwrap();
    id
}

fn path(s: &Setup, tail: &str) -> String {
    format!("/channels/{}/capture{tail}", s.room.id())
}

async fn clips_on(s: &Setup) {
    sit_in(&s.room, &s.owner.id).await;
    let (status, ..) = call(
        &s.harness,
        &s.owner_session,
        "PUT",
        path(s, "/clips"),
        Some(json!({ "on": true })),
    )
    .await;
    assert_eq!(status, Status::Ok);
}

async fn consent(s: &Setup, session: &Session, include: bool) -> (Status, Value, Option<ErrorType>) {
    call(
        &s.harness,
        session,
        "PUT",
        path(s, "/consent"),
        Some(json!({ "include": include })),
    )
    .await
}

async fn share(s: &Setup, session: &Session, body: Value) -> (Status, Value, Option<ErrorType>) {
    call(&s.harness, session, "POST", path(s, "/clips"), Some(body)).await
}

async fn wings(s: &Setup, level: u32) {
    let mut server = s.harness.db.fetch_server(&s.server.id).await.unwrap();
    server
        .update(
            &s.harness.db,
            PartialServer {
                supercharge: Some(v0::ServerSupercharge {
                    count: level * 5,
                    level,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

async fn root_config(harness: &TestHarness) -> Value {
    let text = harness
        .client
        .get("/")
        .dispatch()
        .await
        .into_string()
        .await
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

async fn start_episode(s: &Setup, session: &Session) -> (Status, Value, Option<ErrorType>) {
    call(
        &s.harness,
        session,
        "POST",
        path(s, "/recording"),
        Some(json!({ "title": "Episode one" })),
    )
    .await
}

#[rocket::async_test]
async fn everything_is_off_until_the_flags_are_set() {
    let s = setup().await;
    sit_in(&s.room, &s.owner.id).await;
    for (method, tail, body) in [
        ("GET", "", None),
        ("PUT", "/clips", Some(json!({ "on": true }))),
        ("PUT", "/consent", Some(json!({ "include": true }))),
        ("POST", "/recording", Some(json!({}))),
        ("GET", "/items", None),
    ] {
        let (status, _, error) =
            call(&s.harness, &s.owner_session, method, path(&s, tail), body).await;
        assert_eq!(status, Status::BadRequest, "{method} {tail}");
        assert!(
            matches!(error, Some(ErrorType::FeatureDisabled { .. })),
            "{method} {tail}: {error:?}"
        );
    }
}

#[rocket::async_test]
async fn the_root_says_which_voice_features_are_on() {
    let s = setup().await;
    let before = root_config(&s.harness).await;
    assert_eq!(before["features"]["voice_clips"], json!(false));
    assert_eq!(before["features"]["voice_recordings"], json!(false));
    assert_eq!(before["features"]["voice_weak_network"]["enabled"], json!(false));
    assert_eq!(
        before["features"]["voice_weak_network"]["weak_audio_bitrate"],
        json!(24_000)
    );

    overwrite_config(|config| {
        config.features.voice_weak_network_enabled = true;
        config.features.voice_clips_enabled = true;
    })
    .await;
    let after = root_config(&s.harness).await;
    assert_eq!(after["features"]["voice_clips"], json!(true));
    assert_eq!(after["features"]["voice_weak_network"]["enabled"], json!(true));
}

#[rocket::async_test]
async fn only_a_manager_in_the_room_turns_clips_on_and_everyone_is_told() {
    features_on().await;
    let s = setup().await;
    sit_in(&s.room, &s.member.id).await;

    // a member without Manage Channel
    let (status, _, error) = call(
        &s.harness,
        &s.member_session,
        "PUT",
        path(&s, "/clips"),
        Some(json!({ "on": true })),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::MissingPermission { .. })));

    // the owner, but not in the room
    let (status, _, error) = call(
        &s.harness,
        &s.owner_session,
        "PUT",
        path(&s, "/clips"),
        Some(json!({ "on": true })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::NotConnected)));

    sit_in(&s.room, &s.owner.id).await;
    let mut events = PubSubTestHelper::new(s.room.id()).await;
    let (status, state, _) = call(
        &s.harness,
        &s.owner_session,
        "PUT",
        path(&s, "/clips"),
        Some(json!({ "on": true })),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(state["clips"], json!(s.owner.id));
    // nobody agreed yet: nobody is included, the manager neither
    assert_eq!(state["included"], json!([]));

    let event = events
        .wait_for_event(|event| matches!(event, EventV1::VoiceCaptureUpdate { .. }))
        .await;
    match event {
        EventV1::VoiceCaptureUpdate { id, data } => {
            assert_eq!(id, s.room.id());
            assert_eq!(data.clips.as_deref(), Some(s.owner.id.as_str()));
            assert!(data.included.is_empty());
        }
        _ => unreachable!(),
    }
}

#[rocket::async_test]
async fn a_voice_is_included_only_after_its_owner_agreed_and_until_they_change_it() {
    features_on().await;
    let s = setup().await;
    clips_on(&s).await;
    sit_in(&s.room, &s.member.id).await;

    // someone outside the room has no say in it
    let (_, outsider_session, _) = s.harness.new_user().await;
    let (status, ..) = consent(&s, &outsider_session, true).await;
    assert_ne!(status, Status::Ok);

    let (status, state, _) = consent(&s, &s.member_session, true).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(state["included"], json!([s.member.id]));

    // a clip that names the owner, who never agreed, is refused
    let clip = audio(&s.harness, &s.owner, 100_000).await;
    let (status, _, error) = share(
        &s,
        &s.owner_session,
        json!({ "file": clip, "duration": 30, "included": [s.owner.id, s.member.id] }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::VoiceNotAgreed)));

    // the member changes their mind: left out from now on
    let (_, state, _) = consent(&s, &s.member_session, false).await;
    assert_eq!(state["included"], json!([]));
    let (status, _, error) = share(
        &s,
        &s.owner_session,
        json!({ "file": clip, "duration": 30, "included": [s.member.id] }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::VoiceNotAgreed)));
}

#[rocket::async_test]
async fn a_clip_is_shared_as_a_message_and_those_in_it_can_delete_it() {
    features_on().await;
    let s = setup().await;
    clips_on(&s).await;
    sit_in(&s.room, &s.member.id).await;
    consent(&s, &s.owner_session, true).await;
    consent(&s, &s.member_session, true).await;

    let clip = audio(&s.harness, &s.owner, 100_000).await;
    let (status, capture, error) = share(
        &s,
        &s.owner_session,
        json!({
            "file": clip,
            "duration": 30,
            "included": [s.owner.id, s.member.id],
            "share_in": s.text.id(),
        }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    assert_eq!(capture["kind"], json!("Clip"));
    assert_eq!(capture["shared_in"], json!(s.text.id()));
    let message_id = capture["message"].as_str().unwrap().to_string();
    let message = s.harness.db.fetch_message(&message_id).await.unwrap();
    assert_eq!(message.author, s.owner.id);
    assert_eq!(message.attachments.as_ref().unwrap()[0].id, clip);

    // the file is used now: it cannot be shared a second time
    let (status, ..) = share(
        &s,
        &s.owner_session,
        json!({ "file": clip, "duration": 30, "included": [] }),
    )
    .await;
    assert_eq!(status, Status::NotFound);

    // listed in the room
    let (_, items, _) = call(&s.harness, &s.member_session, "GET", path(&s, "/items"), None).await;
    assert_eq!(items.as_array().unwrap().len(), 1);

    // a third member whose voice is not in it cannot delete it
    let (_, third_session, third) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &third, None)
        .await
        .unwrap();
    let item = format!("/items/{}", capture["_id"].as_str().unwrap());
    let (status, ..) = call(&s.harness, &third_session, "DELETE", path(&s, &item), None).await;
    assert_eq!(status, Status::Forbidden);

    // the member whose voice is in it can: message and record go
    let (status, ..) = call(&s.harness, &s.member_session, "DELETE", path(&s, &item), None).await;
    assert_eq!(status, Status::NoContent);
    assert!(s.harness.db.fetch_message(&message_id).await.is_err());
    let (_, items, _) = call(&s.harness, &s.member_session, "GET", path(&s, "/items"), None).await;
    assert!(items.as_array().unwrap().is_empty());
    let (status, ..) = call(&s.harness, &s.member_session, "DELETE", path(&s, &item), None).await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn a_clip_must_be_the_persons_own_small_audio_file_of_the_right_length() {
    features_on().await;
    let s = setup().await;
    clips_on(&s).await;

    // too long
    let clip = audio(&s.harness, &s.owner, 100_000).await;
    let (status, _, error) = share(
        &s,
        &s.owner_session,
        json!({ "file": clip, "duration": 120, "included": [] }),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::FailedValidation { .. })));

    // too large
    let big = audio(&s.harness, &s.owner, 5_000_000).await;
    let (_, _, error) = share(
        &s,
        &s.owner_session,
        json!({ "file": big, "duration": 30, "included": [] }),
    )
    .await;
    assert!(matches!(error, Some(ErrorType::FileTooLarge { .. })), "{error:?}");

    // not audio
    let picture = file(&s.harness, &s.owner, 1_000, "image/png").await;
    let (_, _, error) = share(
        &s,
        &s.owner_session,
        json!({ "file": picture, "duration": 30, "included": [] }),
    )
    .await;
    assert!(matches!(error, Some(ErrorType::FileTypeNotAllowed)), "{error:?}");

    // somebody else's upload
    let theirs = audio(&s.harness, &s.member, 100_000).await;
    let (status, ..) = share(
        &s,
        &s.owner_session,
        json!({ "file": theirs, "duration": 30, "included": [] }),
    )
    .await;
    assert_eq!(status, Status::NotFound);

    // a channel of another server
    let (_, _, stranger) = s.harness.new_user().await;
    let (_, other_channels) = s.harness.new_server(&stranger).await;
    let (status, _, error) = share(
        &s,
        &s.owner_session,
        json!({ "file": clip, "duration": 30, "included": [], "share_in": other_channels[0].id() }),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::InvalidOperation)));

    // clips off again: nothing is shared
    call(
        &s.harness,
        &s.owner_session,
        "PUT",
        path(&s, "/clips"),
        Some(json!({ "on": false })),
    )
    .await;
    let (_, _, error) = share(
        &s,
        &s.owner_session,
        json!({ "file": clip, "duration": 30, "included": [] }),
    )
    .await;
    assert!(matches!(error, Some(ErrorType::InvalidOperation)));
}

#[rocket::async_test]
async fn clips_have_a_monthly_limit_that_deleting_does_not_reset() {
    features_on().await;
    overwrite_config(|config| {
        config.features.voice_capture.clips_per_month = 2;
    })
    .await;
    let s = setup().await;
    clips_on(&s).await;

    let mut first = String::new();
    for _ in 0..2 {
        let clip = audio(&s.harness, &s.owner, 1_000).await;
        let (status, capture, error) = share(
            &s,
            &s.owner_session,
            json!({ "file": clip, "duration": 10, "included": [] }),
        )
        .await;
        assert_eq!(status, Status::Ok, "{error:?}");
        first = capture["_id"].as_str().unwrap().to_string();
    }

    let (_, info, _) = call(&s.harness, &s.owner_session, "GET", path(&s, ""), None).await;
    assert_eq!(info["allowance"]["clips_left"], json!(0));
    assert_eq!(info["allowance"]["opt_in"], json!(true));

    call(
        &s.harness,
        &s.owner_session,
        "DELETE",
        path(&s, &format!("/items/{first}")),
        None,
    )
    .await;
    let clip = audio(&s.harness, &s.owner, 1_000).await;
    let (status, _, error) = share(
        &s,
        &s.owner_session,
        json!({ "file": clip, "duration": 10, "included": [] }),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::TooManyVoiceClips { max: 2 })), "{error:?}");
}

#[rocket::async_test]
async fn recording_needs_a_manager_time_left_and_the_room() {
    features_on().await;
    overwrite_config(|config| {
        config.features.voice_capture.trial_minutes = 0;
    })
    .await;
    let s = setup().await;
    sit_in(&s.room, &s.owner.id).await;
    sit_in(&s.room, &s.member.id).await;


    // no Wings and no trial: no recording
    let (status, _, error) = start_episode(&s, &s.owner_session).await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::RecordingTimeUsedUp { max: 0 })), "{error:?}");

    wings(&s, 1).await;
    let (status, _, error) = start_episode(&s, &s.member_session).await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::MissingPermission { .. })));

    let mut events = PubSubTestHelper::new(s.room.id()).await;
    let (status, state, error) = start_episode(&s, &s.owner_session).await;
    assert_eq!(status, Status::Ok, "{error:?}");
    assert_eq!(state["recording"]["by"], json!(s.owner.id));
    assert_eq!(state["recording"]["title"], json!("Episode one"));
    assert_eq!(state["included"], json!([]));
    events
        .wait_for_event(|event| {
            matches!(event, EventV1::VoiceCaptureUpdate { data, .. } if data.recording.is_some())
        })
        .await;

    // one recording at a time
    let (status, _, error) = start_episode(&s, &s.owner_session).await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::InvalidOperation)));

    // the member cannot end it, the owner can
    let (status, ..) =
        call(&s.harness, &s.member_session, "DELETE", path(&s, "/recording"), None).await;
    assert_eq!(status, Status::Forbidden);
    let (status, state, _) =
        call(&s.harness, &s.owner_session, "DELETE", path(&s, "/recording"), None).await;
    assert_eq!(status, Status::Ok);
    assert!(state.get("recording").is_none());
}

#[rocket::async_test]
async fn an_episode_keeps_who_agreed_while_it_ran_and_counts_towards_the_month() {
    features_on().await;
    let s = setup().await;
    wings(&s, 1).await;
    sit_in(&s.room, &s.owner.id).await;
    let member_seat = sit_in(&s.room, &s.member.id).await;

    let (status, ..) = call(
        &s.harness,
        &s.owner_session,
        "POST",
        path(&s, "/recording"),
        Some(json!({ "title": "Episode one" })),
    )
    .await;
    assert_eq!(status, Status::Ok);
    consent(&s, &s.owner_session, true).await;
    consent(&s, &s.member_session, true).await;

    // the member leaves before the end: still part of the episode
    delete_voice_state(&member_seat, &s.member.id).await.unwrap();
    capture_after_leave(&member_seat, &s.member.id, true).await.unwrap();

    let episode = audio(&s.harness, &s.owner, 2_000_000).await;
    let finish = |body: Value| {
        call(
            &s.harness,
            &s.owner_session,
            "POST",
            path(&s, "/recording/finish"),
            Some(body),
        )
    };

    // longer than it ran
    let (status, _, error) = finish(json!({
        "file": episode, "duration": 3_000, "included": [s.owner.id]
    }))
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::FailedValidation { .. })), "{error:?}");

    // someone who never agreed
    let (_, _, stranger) = s.harness.new_user().await;
    let (status, _, error) = finish(json!({
        "file": episode, "duration": 3, "included": [s.owner.id, stranger.id]
    }))
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::VoiceNotAgreed)));

    let (status, capture, error) = finish(json!({
        "file": episode, "duration": 3, "included": [s.owner.id, s.member.id]
    }))
    .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    assert_eq!(capture["kind"], json!("Episode"));
    assert_eq!(capture["title"], json!("Episode one"));
    assert_eq!(capture["shared_in"], json!(s.room.id()));
    let message = s
        .harness
        .db
        .fetch_message(capture["message"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(message.content.as_deref(), Some("Episode one"));

    // the recording is over and its seconds are counted
    let (_, info, _) = call(&s.harness, &s.owner_session, "GET", path(&s, ""), None).await;
    assert!(info["state"].get("recording").is_none());
    assert_eq!(info["allowance"]["recording_seconds_per_month"], json!(5 * 3600));
    assert_eq!(info["allowance"]["recording_seconds_left"], json!(5 * 3600 - 3));

    // the member who left can still delete the episode their voice is in
    let item = format!("/items/{}", capture["_id"].as_str().unwrap());
    let (status, ..) = call(&s.harness, &s.member_session, "DELETE", path(&s, &item), None).await;
    assert_eq!(status, Status::NoContent);
    let (_, info, _) = call(&s.harness, &s.owner_session, "GET", path(&s, ""), None).await;
    assert_eq!(info["allowance"]["recording_seconds_left"], json!(5 * 3600 - 3));
}

#[rocket::async_test]
async fn leaving_takes_the_choice_along_and_ends_the_recorders_recording() {
    features_on().await;
    let s = setup().await;
    wings(&s, 1).await;
    let owner_seat = sit_in(&s.room, &s.owner.id).await;
    sit_in(&s.room, &s.member.id).await;
    clips_on(&s).await;
    call(
        &s.harness,
        &s.owner_session,
        "POST",
        path(&s, "/recording"),
        Some(json!({})),
    )
    .await;
    consent(&s, &s.owner_session, true).await;

    let mut events = PubSubTestHelper::new(s.room.id()).await;
    delete_voice_state(&owner_seat, &s.owner.id).await.unwrap();
    capture_after_leave(&owner_seat, &s.owner.id, true).await.unwrap();
    let event = events
        .wait_for_event(|event| matches!(event, EventV1::VoiceCaptureUpdate { .. }))
        .await;
    match event {
        EventV1::VoiceCaptureUpdate { data, .. } => {
            assert!(data.recording.is_none(), "the recorder left: nothing to save");
            assert!(data.included.is_empty());
            assert!(data.clips.is_some(), "clips stay on for those still there");
        }
        _ => unreachable!(),
    }

    // back in the room, the owner has to agree again
    sit_in(&s.room, &s.owner.id).await;
    let (_, info, _) = call(&s.harness, &s.owner_session, "GET", path(&s, ""), None).await;
    assert_eq!(info["state"]["included"], json!([]));
}

#[rocket::async_test]
async fn direct_calls_and_text_channels_are_never_captured() {
    features_on().await;
    let s = setup().await;
    let (status, _, error) = call(
        &s.harness,
        &s.owner_session,
        "GET",
        format!("/channels/{}/capture", s.text.id()),
        None,
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert!(matches!(error, Some(ErrorType::NotAVoiceChannel)));

    // someone who cannot see the room learns nothing about it
    let (_, outsider_session, _) = s.harness.new_user().await;
    let (status, ..) = call(&s.harness, &outsider_session, "GET", path(&s, ""), None).await;
    assert!(status == Status::Forbidden || status == Status::NotFound, "{status}");
}
