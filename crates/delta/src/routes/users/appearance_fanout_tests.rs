//! Bug #10 (owner, 2026-11-22): a new avatar took a long time to show for
//! others. The change went only to the user's own topic (friends and DM
//! partners); people who only share a server never heard of it and kept the
//! old picture until they reloaded. Through the real route, as a person
//! changes their avatar or display name.

use std::time::Duration;

use revolt_database::{events::client::EventV1, File, Member, Metadata, Session, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};

use crate::util::test::{PubSubTestHelper, TestHarness};

async fn edit_me(harness: &TestHarness, session: &Session, body: serde_json::Value) {
    let response = harness
        .client
        .patch("/users/@me")
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    assert_eq!(status, Status::Ok, "{text}");
}

/// An uploaded avatar (what the file service records after an upload)
async fn uploaded_avatar(harness: &TestHarness, user: &User) -> String {
    let id = TestHarness::rand_string();
    harness
        .db
        .insert_attachment(&File {
            id: id.clone(),
            tag: "avatars".to_string(),
            filename: "me.png".to_string(),
            hash: None,
            uploaded_at: None,
            uploader_id: Some(user.id.clone()),
            used_for: None,
            deleted: None,
            reported: None,
            purged: None,
            metadata: Metadata::Image {
                width: 128,
                height: 128,
                thumbhash: None,
                animated: None,
            },
            content_type: "image/png".to_string(),
            size: 100,
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

/// A server both people are in; they are not friends
async fn shared_server(harness: &TestHarness, owner: &User, other: &User) -> String {
    let (server, _) = harness.new_server(owner).await;
    Member::create(&harness.db, &server, owner, None).await.unwrap();
    Member::create(&harness.db, &server, other, None).await.unwrap();
    server.id
}

#[rocket::async_test]
async fn a_new_avatar_reaches_everyone_in_a_shared_server() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, neighbour) = harness.new_user().await;
    let server = shared_server(&harness, &user, &neighbour).await;
    let mut server_events = PubSubTestHelper::new(&server).await;

    let avatar = uploaded_avatar(&harness, &user).await;
    edit_me(&harness, &session, serde_json::json!({ "avatar": avatar })).await;

    let event = tokio::time::timeout(
        Duration::from_secs(5),
        server_events.wait_for_event(|event| {
            matches!(event, EventV1::UserUpdate { id, data, .. }
                if id == &user.id && data.avatar.as_ref().is_some_and(|file| file.id == avatar))
        }),
    )
    .await
    .expect("the server's members are told about the new avatar");

    // only what they see anyway: no status, profile or other private parts
    let EventV1::UserUpdate { data, .. } = event else {
        unreachable!()
    };
    assert_eq!(data.status, None);
    assert_eq!(data.pronouns, None);
    assert_eq!(data.style, None);
    assert_eq!(data.online, None);
}

#[rocket::async_test]
async fn a_removed_avatar_and_a_new_display_name_reach_the_server() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, neighbour) = harness.new_user().await;
    let server = shared_server(&harness, &user, &neighbour).await;
    let avatar = uploaded_avatar(&harness, &user).await;
    edit_me(&harness, &session, serde_json::json!({ "avatar": avatar })).await;

    let mut server_events = PubSubTestHelper::new(&server).await;
    edit_me(
        &harness,
        &session,
        serde_json::json!({ "display_name": "Layla", "remove": ["Avatar"] }),
    )
    .await;

    tokio::time::timeout(
        Duration::from_secs(5),
        server_events.wait_for_event(|event| {
            matches!(event, EventV1::UserUpdate { id, data, clear, .. }
                if id == &user.id
                    && data.display_name.as_deref() == Some("Layla")
                    && clear.contains(&v0::FieldsUser::Avatar))
        }),
    )
    .await
    .expect("the server's members are told the avatar is gone and the new name");
}

#[rocket::async_test]
async fn a_status_change_stays_off_the_server_topic() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, neighbour) = harness.new_user().await;
    let server = shared_server(&harness, &user, &neighbour).await;
    let mut server_events = PubSubTestHelper::new(&server).await;

    edit_me(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "away for lunch" } }),
    )
    .await;

    let fanned = tokio::time::timeout(
        Duration::from_secs(2),
        server_events.wait_for_event(
            |event| matches!(event, EventV1::UserUpdate { id, .. } if id == &user.id),
        ),
    )
    .await;
    assert!(fanned.is_err(), "a status text is not sent to the whole server");
}
