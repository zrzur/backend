//! The sound library: listing it, previews, and adding a library sound to a
//! server (permissions, limits, unknown ids, adding twice).
use revolt_database::{
    events::client::EventV1,
    sound_library::{library_sound, sound_library},
    Member, PartialUser, Server, Session, User,
};
use revolt_result::ErrorType;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use crate::util::test::{PubSubTestHelper, TestHarness};

struct Setup {
    harness: TestHarness,
    owner: User,
    owner_session: Session,
    member_session: Session,
    server: Server,
}

/// What autumn does at start: every library sound stored once as a file hash
async fn seed_library(harness: &TestHarness) {
    for sound in &sound_library().sounds {
        if harness.db.fetch_attachment_hash(&sound.sha256).await.is_err() {
            harness
                .db
                .insert_attachment_hash(&sound.file_hash("revolt-uploads".into(), "nonce".into()))
                .await
                .expect("hash");
        }
    }
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    seed_library(&harness).await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &member, None)
        .await
        .expect("member");
    Setup {
        harness,
        owner,
        owner_session,
        member_session,
        server,
    }
}

async fn send(
    harness: &TestHarness,
    session: Option<&Session>,
    method: &str,
    path: String,
    body: Option<Value>,
) -> (Status, Value) {
    let client = &harness.client;
    let mut request = match method {
        "GET" => client.get(path),
        "PUT" => client.put(path),
        "DELETE" => client.delete(path),
        _ => unreachable!(),
    };
    if let Some(session) = session {
        request = request.header(Header::new("x-session-token", session.token.clone()));
    }
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

async fn add(s: &Setup, session: &Session, id: &str, body: Value) -> (Status, Value) {
    send(
        &s.harness,
        Some(session),
        "PUT",
        format!("/servers/{}/soundboard/library/{id}", s.server.id),
        Some(body),
    )
    .await
}

#[rocket::async_test]
async fn the_library_is_listed_for_signed_in_people() {
    let s = setup().await;
    let response = s
        .harness
        .client
        .get("/soundboard/library")
        .header(Header::new("x-session-token", s.member_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let cache = response.headers().get_one("Cache-Control").unwrap_or_default().to_string();
    assert!(cache.contains("max-age="), "{cache}");
    let body: Value = serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
    let sounds = body["sounds"].as_array().unwrap();
    assert_eq!(sounds.len(), sound_library().sounds.len());
    let go = sounds.iter().find(|sound| sound["id"] == "voice-go").unwrap();
    assert_eq!(go["category"], "voice");
    assert_eq!(go["names"]["ar"], library_sound("voice-go").unwrap().names["ar"]);
    assert_eq!(go["version"].as_str().unwrap().len(), 12);
    assert!(body["categories"].as_array().unwrap().len() >= 5);

    // not signed in: nothing
    let (status, _) = send(&s.harness, None, "GET", "/soundboard/library".into(), None).await;
    assert_eq!(status, Status::Unauthorized);
}

#[rocket::async_test]
async fn a_preview_is_the_file_and_never_changes() {
    let s = setup().await;
    let go = library_sound("voice-go").unwrap();
    let response = s
        .harness
        .client
        .get(format!("/soundboard/library/voice-go/audio?v={}", &go.sha256[..12]))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    assert_eq!(response.content_type(), Some(ContentType::new("audio", "ogg")));
    let cache = response.headers().get_one("Cache-Control").unwrap_or_default().to_string();
    assert!(cache.contains("immutable") && cache.contains("max-age=31536000"), "{cache}");
    assert_eq!(response.into_bytes().await.unwrap(), go.bytes);

    let (status, _) = send(&s.harness, None, "GET", "/soundboard/library/nope/audio".into(), None).await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn a_manager_adds_a_library_sound_like_an_upload() {
    let s = setup().await;
    let mut events = PubSubTestHelper::new(&s.server.id).await;
    let go = library_sound("voice-go").unwrap();

    let (status, body) = add(&s, &s.owner_session, "voice-go", json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["name"], go.names["en"]);
    assert_eq!(body["emoji"], go.emoji);
    assert_eq!(body["duration_ms"], go.duration_ms);
    assert_eq!(body["library_id"], "voice-go");
    assert_eq!(body["creator_id"], s.owner.id.as_str());

    // its own file, of the library's hash, used by this server
    let id = body["_id"].as_str().unwrap();
    let file = s.harness.db.fetch_attachment("sounds", id).await.expect("file");
    assert_eq!(file.hash.as_deref(), Some(go.sha256.as_str()));
    assert_eq!(file.used_for.map(|used| used.id), Some(s.server.id.clone()));

    let event = events
        .wait_for_event(|event| matches!(event, EventV1::SoundboardSoundCreate(_)))
        .await;
    let EventV1::SoundboardSoundCreate(sound) = event else {
        unreachable!()
    };
    assert_eq!(sound.id, id);

    let (_, list) = send(
        &s.harness,
        Some(&s.member_session),
        "GET",
        format!("/servers/{}/soundboard", s.server.id),
        None,
    )
    .await;
    assert_eq!(list["sounds"].as_array().unwrap().len(), 1);
}

#[rocket::async_test]
async fn the_name_is_in_the_adders_language_unless_given() {
    let s = setup().await;
    s.harness
        .db
        .update_user(
            &s.owner.id,
            &PartialUser {
                locale: Some("ar".into()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (status, body) = add(&s, &s.owner_session, "voice-ready", json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["name"], library_sound("voice-ready").unwrap().names["ar"]);

    let (status, body) = add(
        &s,
        &s.owner_session,
        "alert-success",
        json!({ "name": "  Nice  ", "emoji": "🎉", "volume": 40 }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["name"], "Nice");
    assert_eq!(body["emoji"], "🎉");
    assert_eq!(body["volume"], 40);
}

#[rocket::async_test]
async fn members_without_manage_soundboard_cannot_add_from_the_library() {
    let s = setup().await;
    let (status, body) = add(&s, &s.member_session, "voice-go", json!({})).await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error_type(&body), ErrorType::MissingPermission { .. }));
}

#[rocket::async_test]
async fn an_unknown_library_sound_is_not_found() {
    let s = setup().await;
    let (status, body) = add(&s, &s.owner_session, "no-such-sound", json!({})).await;
    assert_eq!(status, Status::NotFound, "{body}");
}

#[rocket::async_test]
async fn a_library_sound_is_added_once_and_again_after_removal() {
    let s = setup().await;
    let (status, first) = add(&s, &s.owner_session, "voice-go", json!({})).await;
    assert_eq!(status, Status::Ok);
    let (status, body) = add(&s, &s.owner_session, "voice-go", json!({ "name": "Again" })).await;
    assert_eq!(status, Status::Conflict, "{body}");
    assert!(matches!(error_type(&body), ErrorType::SoundAlreadyAdded));
    // the refused second copy left no file behind
    let used = s.harness.db.count_file_hash_references(&library_sound("voice-go").unwrap().sha256).await.unwrap();
    assert_eq!(used, 1);

    let (status, _) = send(
        &s.harness,
        Some(&s.owner_session),
        "DELETE",
        format!("/servers/{}/soundboard/{}", s.server.id, first["_id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent);
    let (status, body) = add(&s, &s.owner_session, "voice-go", json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn the_library_counts_towards_the_server_limit() {
    let s = setup().await;
    let ids: Vec<&str> = sound_library().sounds.iter().take(9).map(|sound| sound.id.as_str()).collect();
    for id in &ids[..8] {
        let (status, body) = add(&s, &s.owner_session, id, json!({})).await;
        assert_eq!(status, Status::Ok, "{body}");
    }
    let (status, body) = add(&s, &s.owner_session, ids[8], json!({})).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert!(matches!(error_type(&body), ErrorType::TooManySounds { max: 8 }));
}

#[rocket::async_test]
async fn a_name_that_is_too_long_is_refused() {
    let s = setup().await;
    let (status, _) = add(&s, &s.owner_session, "voice-go", json!({ "name": "x".repeat(33) })).await;
    assert_eq!(status, Status::BadRequest);
}
