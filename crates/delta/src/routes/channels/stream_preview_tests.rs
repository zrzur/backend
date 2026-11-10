//! Stream previews: only a sharer sends one, at most every 25 seconds, and
//! only people who can see the channel fetch it.
use revolt_database::{
    iso8601_timestamp::Timestamp,
    voice::{
        create_voice_state, delete_stream_preview, delete_voice_state, update_voice_state,
        UserVoiceChannel,
    },
    Channel, Member, Server, Session, User,
};
use revolt_models::v0::{self, PartialUserVoiceState};
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use crate::util::test::TestHarness;

async fn voice_channel(harness: &TestHarness, server: &mut Server) -> Channel {
    Channel::create_server_channel(
        &harness.db,
        server,
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
    .expect("`Channel`")
}

/// Standard base64 (delta has no base64 crate of its own)
fn encode(bytes: impl AsRef<[u8]>) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.as_ref().chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn jpeg() -> String {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.resize(4_000, 7);
    encode(bytes)
}

async fn send(
    harness: &TestHarness,
    session: &Session,
    put: bool,
    path: String,
    body: Option<Value>,
) -> (Status, Value) {
    let request = if put {
        harness.client.put(path)
    } else {
        harness.client.get(path)
    };
    let mut request = request.header(Header::new("x-session-token", session.token.clone()));
    if let Some(body) = body {
        request = request.header(ContentType::JSON).body(body.to_string());
    }
    let response = request.dispatch().await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn setup() -> (TestHarness, User, Session, Session, Channel) {
    let harness = TestHarness::new().await;
    let (_, sharer_session, sharer) = harness.new_user().await;
    let (_, viewer_session, viewer) = harness.new_user().await;
    let (mut server, _) = harness.new_server(&sharer).await;
    Member::create(&harness.db, &server, &viewer, None)
        .await
        .unwrap();
    let channel = voice_channel(&harness, &mut server).await;
    (harness, sharer, sharer_session, viewer_session, channel)
}

#[rocket::async_test]
async fn a_sharer_sends_a_preview_others_fetch_it() {
    let (harness, sharer, sharer_session, viewer_session, channel) = setup().await;
    let seat = UserVoiceChannel::from_channel(&channel);
    let put = format!("/channels/{}/stream_preview", channel.id());
    let get = format!("/channels/{}/stream_preview/{}", channel.id(), sharer.id);
    delete_stream_preview(channel.id(), &sharer.id).await.unwrap();

    // not in the call: nothing to preview
    let (status, _) = send(&harness, &sharer_session, true, put.clone(), Some(json!({ "image": jpeg() }))).await;
    assert_eq!(status, Status::BadRequest);

    // in the call but not sharing
    create_voice_state(&seat, &sharer.id, Timestamp::now_utc())
        .await
        .unwrap();
    let (status, _) = send(&harness, &sharer_session, true, put.clone(), Some(json!({ "image": jpeg() }))).await;
    assert_eq!(status, Status::BadRequest);

    update_voice_state(
        &seat,
        &sharer.id,
        &PartialUserVoiceState {
            screensharing: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // nothing yet
    let (status, _) = send(&harness, &viewer_session, false, get.clone(), None).await;
    assert_eq!(status, Status::NotFound);

    let (status, _) = send(&harness, &sharer_session, true, put.clone(), Some(json!({ "image": jpeg() }))).await;
    assert_eq!(status, Status::NoContent);
    let (status, body) = send(&harness, &viewer_session, false, get.clone(), None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(body["content_type"], "image/jpeg");
    assert_eq!(body["image"], jpeg());

    // a second one right away is refused: at most one every 25 seconds
    let (status, _) = send(&harness, &sharer_session, true, put, Some(json!({ "image": jpeg() }))).await;
    assert_eq!(status, Status::BadRequest);

    // someone outside the server can't look
    let (_, outsider, _) = harness.new_user().await;
    let (status, _) = send(&harness, &outsider, false, get, None).await;
    assert_ne!(status, Status::Ok);
}

#[rocket::async_test]
async fn a_preview_is_small_and_a_picture() {
    let (harness, sharer, sharer_session, _, channel) = setup().await;
    let seat = UserVoiceChannel::from_channel(&channel);
    delete_stream_preview(channel.id(), &sharer.id).await.unwrap();
    create_voice_state(&seat, &sharer.id, Timestamp::now_utc())
        .await
        .unwrap();
    update_voice_state(
        &seat,
        &sharer.id,
        &PartialUserVoiceState {
            screensharing: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let put = format!("/channels/{}/stream_preview", channel.id());

    let mut big = vec![0xFF, 0xD8, 0xFF];
    big.resize(30_000, 1);
    let (status, _) = send(&harness, &sharer_session, true, put.clone(), Some(json!({ "image": encode(big) }))).await;
    assert_eq!(status, Status::BadRequest);

    let png = encode([0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]);
    let (status, _) = send(&harness, &sharer_session, true, put, Some(json!({ "image": png }))).await;
    assert_eq!(status, Status::BadRequest);
}

/// A sharer with a preview stored, and the viewer's session
async fn sharing() -> (TestHarness, User, Session, Session, Channel, UserVoiceChannel) {
    let (harness, sharer, sharer_session, viewer_session, channel) = setup().await;
    let seat = UserVoiceChannel::from_channel(&channel);
    delete_stream_preview(channel.id(), &sharer.id).await.unwrap();
    create_voice_state(&seat, &sharer.id, Timestamp::now_utc())
        .await
        .unwrap();
    update_voice_state(
        &seat,
        &sharer.id,
        &PartialUserVoiceState {
            screensharing: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (status, _) = send(
        &harness,
        &sharer_session,
        true,
        format!("/channels/{}/stream_preview", channel.id()),
        Some(json!({ "image": jpeg() })),
    )
    .await;
    assert_eq!(status, Status::NoContent);
    (harness, sharer, sharer_session, viewer_session, channel, seat)
}

#[rocket::async_test]
async fn a_stopped_share_has_no_preview() {
    let (harness, sharer, _, viewer_session, channel, seat) = sharing().await;
    let get = format!("/channels/{}/stream_preview/{}", channel.id(), sharer.id);
    assert_eq!(send(&harness, &viewer_session, false, get.clone(), None).await.0, Status::Ok);

    update_voice_state(
        &seat,
        &sharer.id,
        &PartialUserVoiceState {
            screensharing: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(send(&harness, &viewer_session, false, get, None).await.0, Status::NotFound);
}

#[rocket::async_test]
async fn leaving_the_call_takes_the_preview_away() {
    let (harness, sharer, _, viewer_session, channel, seat) = sharing().await;
    let get = format!("/channels/{}/stream_preview/{}", channel.id(), sharer.id);
    delete_voice_state(&seat, &sharer.id).await.unwrap();
    assert_eq!(send(&harness, &viewer_session, false, get, None).await.0, Status::NotFound);
}

#[rocket::async_test]
async fn a_preview_is_for_those_who_may_listen() {
    let (harness, sharer, sharer_session, viewer_session, channel, _) = sharing().await;
    let get = format!("/channels/{}/stream_preview/{}", channel.id(), sharer.id);

    // the owner takes Listen away in this channel
    let response = harness
        .client
        .put(format!("/channels/{}/permissions/default", channel.id()))
        .header(Header::new("x-session-token", sharer_session.token.clone()))
        .header(ContentType::JSON)
        .body(
            json!({ "permissions": { "allow": 0, "deny": revolt_permissions::ChannelPermission::Listen as u64 } })
                .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    assert_eq!(send(&harness, &viewer_session, false, get, None).await.0, Status::Forbidden);
}

#[rocket::async_test]
async fn a_server_deafened_member_sees_no_preview() {
    let (harness, sharer, _, viewer_session, channel, _) = sharing().await;
    let get = format!("/channels/{}/stream_preview/{}", channel.id(), sharer.id);
    let viewer = harness
        .db
        .fetch_session_by_token(&viewer_session.token)
        .await
        .unwrap()
        .user_id;
    let mut member = harness
        .db
        .fetch_member(channel.server().unwrap(), &viewer)
        .await
        .unwrap();
    member
        .update(
            &harness.db,
            revolt_database::PartialMember {
                can_receive: Some(false),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(send(&harness, &viewer_session, false, get, None).await.0, Status::Forbidden);
}
