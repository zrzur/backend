//! Server default notification level: what a member who never chose a
//! level for the server is notified about.

use crate::{rocket, util::test::TestHarness};
use revolt_database::Member;
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};

async fn patch(harness: &TestHarness, token: &str, server: &str, body: serde_json::Value) -> Status {
    harness
        .client
        .patch(format!("/servers/{server}"))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", token.to_string()))
        .body(body.to_string())
        .dispatch()
        .await
        .status()
}

async fn fetch(harness: &TestHarness, token: &str, server: &str) -> serde_json::Value {
    harness
        .client
        .get(format!("/servers/{server}"))
        .header(Header::new("x-session-token", token.to_string()))
        .dispatch()
        .await
        .into_json()
        .await
        .expect("server")
}

/// A new server has no default: the field is left out, so old clients and
/// servers stored before the field existed read the same
#[rocket::async_test]
async fn missing_by_default() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    let body = fetch(&harness, &session.token, &server.id).await;
    assert!(body.get("default_notifications").is_none(), "{body}");
    let stored = harness.db.fetch_server(&server.id).await.unwrap();
    assert_eq!(stored.default_notifications, None);
}

/// The owner sets it, members read it, and it can be changed back
#[rocket::async_test]
async fn manager_sets_and_members_read() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &member, Some(channels)).await.unwrap();

    assert_eq!(
        patch(&harness, &owner_session.token, &server.id, json!({ "default_notifications": "All" })).await,
        Status::Ok
    );
    let stored = harness.db.fetch_server(&server.id).await.unwrap();
    assert_eq!(stored.default_notifications, Some(v0::ServerNotificationLevel::All));
    let body = fetch(&harness, &member_session.token, &server.id).await;
    assert_eq!(body["default_notifications"], "All");

    assert_eq!(
        patch(&harness, &owner_session.token, &server.id, json!({ "default_notifications": "Mentions" })).await,
        Status::Ok
    );
    let stored = harness.db.fetch_server(&server.id).await.unwrap();
    assert_eq!(stored.default_notifications, Some(v0::ServerNotificationLevel::Mentions));
}

/// Changing it needs Manage Server
#[rocket::async_test]
async fn members_cannot_change_it() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &member, Some(channels)).await.unwrap();

    assert_eq!(
        patch(&harness, &member_session.token, &server.id, json!({ "default_notifications": "All" })).await,
        Status::Forbidden
    );
    let stored = harness.db.fetch_server(&server.id).await.unwrap();
    assert_eq!(stored.default_notifications, None);
}

/// Only the two known levels are accepted
#[rocket::async_test]
async fn unknown_level_is_refused() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    let status = patch(&harness, &session.token, &server.id, json!({ "default_notifications": "Loud" })).await;
    assert!(matches!(status.code, 400 | 422), "{status}");
}
