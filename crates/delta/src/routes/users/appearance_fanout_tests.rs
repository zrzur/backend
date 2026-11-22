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

/// Whether a `UserUpdate` for `user` with this display name arrives on the
/// topic within `seconds` (other events, and copies already seen, are skipped)
async fn hears_name(
    events: &mut PubSubTestHelper,
    user: &str,
    name: &str,
    seconds: u64,
    not: Option<String>,
) -> Option<EventV1> {
    tokio::time::timeout(
        Duration::from_secs(seconds),
        events.wait_for_event(|event| {
            matches!(event, EventV1::UserUpdate { id, data, event_id, .. }
                if id == user
                    && data.display_name.as_deref() == Some(name)
                    && (not.is_none() || event_id != &not))
        }),
    )
    .await
    .ok()
}

#[rocket::async_test]
async fn the_server_fan_out_is_limited_and_the_own_topic_is_not() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, neighbour) = harness.new_user().await;
    let server = shared_server(&harness, &user, &neighbour).await;
    let mut server_events = PubSubTestHelper::new(&server).await;
    let mut own_events = PubSubTestHelper::new(&user.id).await;

    // a few an hour go to the servers
    for n in 1..=5 {
        let name = format!("Name {n}");
        edit_me(&harness, &session, serde_json::json!({ "display_name": name })).await;
        assert!(
            hears_name(&mut server_events, &user.id, &name, 5, None).await.is_some(),
            "change {n} reaches the server"
        );
    }

    // past the limit the change still reaches the user's own topic (friends,
    // DM partners, the user's other sessions), not the servers
    edit_me(&harness, &session, serde_json::json!({ "display_name": "Name 6" })).await;
    assert!(hears_name(&mut own_events, &user.id, "Name 6", 5, None).await.is_some());
    assert!(
        hears_name(&mut server_events, &user.id, "Name 6", 2, None).await.is_none(),
        "the sixth change in an hour is not sent to every server"
    );
}

#[rocket::async_test]
async fn a_bot_rename_reaches_its_servers_once_with_the_new_name() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (bot, bot_user) = harness.new_bot(&owner).await;
    let (server, _) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &owner, None).await.unwrap();
    Member::create(&harness.db, &server, &bot_user, None).await.unwrap();
    let mut server_events = PubSubTestHelper::new(&server.id).await;

    let response = harness
        .client
        .patch(format!("/bots/{}", bot.id))
        .header(Header::new("x-session-token", owner_session.token.clone()))
        .header(ContentType::JSON)
        .body(serde_json::json!({ "name": "Liro Bot" }).to_string())
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    assert_eq!(status, Status::Ok, "{text}");

    let first = hears_name(&mut server_events, &bot.id, "Liro Bot", 5, None)
        .await
        .expect("the server hears the new name");
    let EventV1::UserUpdate { data, event_id, .. } = &first else {
        unreachable!()
    };
    assert_eq!(data.username.as_deref(), Some("Liro_Bot"), "the new username comes with it");

    // no second copy (the username and the display name are one rename)
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        server_events.wait_for_event(|event| {
            matches!(event, EventV1::UserUpdate { id, event_id: other, .. }
                if id == &bot.id && other != event_id)
        }),
    )
    .await;
    assert!(second.is_err(), "a bot rename went to the server twice");
}

#[rocket::async_test]
async fn a_server_the_user_left_hears_nothing() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, neighbour) = harness.new_user().await;
    let kept = shared_server(&harness, &user, &neighbour).await;
    let left = shared_server(&harness, &neighbour, &user).await;
    let timed_out = shared_server(&harness, &neighbour, &user).await;

    // a member who left while timed out stays as a pending deletion
    // (MongoDB's `pending_deletion_at`): no longer a member either
    let future = iso8601_timestamp::Timestamp::now_utc() + iso8601_timestamp::Duration::days(1);
    harness
        .db
        .update_member(
            &revolt_database::MemberCompositeKey {
                server: timed_out.clone(),
                user: user.id.clone(),
            },
            &revolt_database::PartialMember {
                timeout: Some(future),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    for server in [&left, &timed_out] {
        let response = harness
            .client
            .delete(format!("/servers/{server}"))
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NoContent, "leave {server}");
    }

    let mut kept_events = PubSubTestHelper::new(&kept).await;
    let mut left_events = PubSubTestHelper::new(&left).await;
    let mut timed_out_events = PubSubTestHelper::new(&timed_out).await;
    edit_me(&harness, &session, serde_json::json!({ "display_name": "Gone" })).await;

    assert!(hears_name(&mut kept_events, &user.id, "Gone", 5, None).await.is_some());
    assert!(hears_name(&mut left_events, &user.id, "Gone", 1, None).await.is_none());
    assert!(hears_name(&mut timed_out_events, &user.id, "Gone", 1, None).await.is_none());
}

#[rocket::async_test]
async fn a_user_in_no_servers_still_hears_the_own_change() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let mut own_events = PubSubTestHelper::new(&user.id).await;
    edit_me(&harness, &session, serde_json::json!({ "display_name": "Alone" })).await;
    assert!(hears_name(&mut own_events, &user.id, "Alone", 5, None).await.is_some());
}

#[rocket::async_test]
async fn a_change_held_back_by_the_limit_reaches_the_server_when_the_window_opens() {
    use std::sync::atomic::Ordering;
    // a short window for the test (each test runs in its own process)
    revolt_database::APPEARANCE_FANOUT_WINDOW_SECS.store(8, Ordering::Relaxed);
    revolt_database::APPEARANCE_FANOUT_RETRY_SECS.store(1, Ordering::Relaxed);

    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, neighbour) = harness.new_user().await;
    let server = shared_server(&harness, &user, &neighbour).await;
    let mut server_events = PubSubTestHelper::new(&server).await;

    for n in 1..=5 {
        let name = format!("Name {n}");
        edit_me(&harness, &session, serde_json::json!({ "display_name": name })).await;
        assert!(hears_name(&mut server_events, &user.id, &name, 5, None).await.is_some());
    }

    // the 6th and 7th change in the window: held back, one trailing fan-out
    edit_me(&harness, &session, serde_json::json!({ "display_name": "Name 6" })).await;
    assert!(hears_name(&mut server_events, &user.id, "Name 6", 1, None).await.is_none());
    edit_me(&harness, &session, serde_json::json!({ "display_name": "Name 7" })).await;

    // when the window opens the members get the newest look, once
    let late = hears_name(&mut server_events, &user.id, "Name 7", 20, None)
        .await
        .expect("the held-back change reaches the server after the window");
    let EventV1::UserUpdate { event_id, .. } = &late else {
        unreachable!()
    };
    assert!(
        hears_name(&mut server_events, &user.id, "Name 7", 3, event_id.clone())
            .await
            .is_none(),
        "one trailing fan-out per user"
    );
    assert!(
        hears_name(&mut server_events, &user.id, "Name 6", 1, None).await.is_none(),
        "an older look is never sent"
    );
}
