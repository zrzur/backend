//! Mutual servers and friends, and the custom status with emoji and expiry,
//! through the real routes.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{
    Emoji, EmojiParent, Member, PartialUser, RelationshipStatus, Session, User,
    UserStatus,
};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Method, Status};
use ulid::Ulid;

use crate::util::test::TestHarness;

async fn call(
    harness: &TestHarness,
    method: Method,
    session: &Session,
    path: &str,
    body: Option<serde_json::Value>,
) -> (Status, String) {
    let mut request = harness
        .client
        .req(method, path.to_string())
        .header(Header::new("x-session-token", session.token.clone()));
    if let Some(body) = body {
        request = request.header(ContentType::JSON).body(body.to_string());
    }
    let response = request.dispatch().await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

async fn befriend(harness: &TestHarness, a: &mut User, b: &mut User) {
    #[allow(clippy::disallowed_methods)]
    a.apply_relationship(
        &harness.db,
        b,
        RelationshipStatus::Friend,
        RelationshipStatus::Friend,
    )
    .await
    .unwrap();
}

async fn set_status(
    harness: &TestHarness,
    session: &Session,
    body: serde_json::Value,
) -> (Status, String) {
    call(harness, Method::Patch, session, "/users/@me", Some(body)).await
}

async fn me(harness: &TestHarness, session: &Session) -> v0::User {
    let (status, body) = call(harness, Method::Get, session, "/users/@me", None).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// A server its owner is a member of (`Server::create` alone adds nobody)
async fn server_of(harness: &TestHarness, owner: &User) -> revolt_database::Server {
    let (server, _) = harness.new_server(owner).await;
    Member::create(&harness.db, &server, owner, None).await.unwrap();
    server
}

fn in_minutes(minutes: i64) -> serde_json::Value {
    serde_json::to_value(Timestamp::now_utc() + Duration::minutes(minutes)).unwrap()
}

#[rocket::async_test]
async fn mutual_lists_only_servers_and_friends_both_people_share() {
    let harness = TestHarness::new().await;
    let (_, viewer_session, mut viewer) = harness.new_user().await;
    let (_, _, mut target) = harness.new_user().await;
    let (_, _, mut both_friend) = harness.new_user().await;
    let (_, _, mut target_only_friend) = harness.new_user().await;

    // shared server: viewer owns it, target joins
    let shared = server_of(&harness, &viewer).await;
    Member::create(&harness.db, &shared, &target, None).await.unwrap();
    // a server only the target is in must never show up for the viewer
    let target_only = server_of(&harness, &target).await;
    // a server only the viewer is in is not mutual either
    let viewer_only = server_of(&harness, &viewer).await;

    befriend(&harness, &mut viewer, &mut target).await;
    befriend(&harness, &mut viewer, &mut both_friend).await;
    befriend(&harness, &mut target, &mut both_friend).await;
    befriend(&harness, &mut target, &mut target_only_friend).await;

    let (status, body) = call(
        &harness,
        Method::Get,
        &viewer_session,
        &format!("/users/{}/mutual", target.id),
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let mutual: v0::MutualResponse = serde_json::from_str(&body).unwrap();

    assert_eq!(mutual.servers, vec![shared.id.clone()]);
    assert!(!mutual.servers.contains(&target_only.id));
    assert!(!mutual.servers.contains(&viewer_only.id));
    assert_eq!(mutual.users, vec![both_friend.id.clone()]);
    assert!(!mutual.users.contains(&target_only_friend.id));
}

#[rocket::async_test]
async fn blocked_people_get_no_mutual_list_either_way() {
    let harness = TestHarness::new().await;
    let (_, viewer_session, mut viewer) = harness.new_user().await;
    let (_, target_session, mut target) = harness.new_user().await;
    let shared = server_of(&harness, &viewer).await;
    Member::create(&harness.db, &shared, &target, None).await.unwrap();

    #[allow(clippy::disallowed_methods)]
    viewer
        .apply_relationship(
            &harness.db,
            &mut target,
            RelationshipStatus::Blocked,
            RelationshipStatus::BlockedOther,
        )
        .await
        .unwrap();

    for (session, other) in [(&viewer_session, &target.id), (&target_session, &viewer.id)] {
        let (status, body) = call(
            &harness,
            Method::Get,
            session,
            &format!("/users/{other}/mutual"),
            None,
        )
        .await;
        assert_ne!(status, Status::Ok, "{body}");
        assert!(!body.contains(&shared.id), "{body}");
    }
}

#[rocket::async_test]
async fn mutual_with_yourself_is_refused() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (status, _) = call(
        &harness,
        Method::Get,
        &session,
        &format!("/users/{}/mutual", user.id),
        None,
    )
    .await;
    assert_ne!(status, Status::Ok);
}

#[rocket::async_test]
async fn a_custom_status_keeps_its_emoji_and_expiry() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": {
            "text": "At lunch",
            "emoji": "🍕",
            "expires_at": in_minutes(30),
        }}),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");

    let status = me(&harness, &session).await.status.unwrap();
    assert_eq!(status.text.as_deref(), Some("At lunch"));
    assert_eq!(status.emoji.as_deref(), Some("🍕"));
    assert!(status.expires_at.is_some());
}

#[rocket::async_test]
async fn the_status_emoji_must_be_an_emoji() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    for bad in ["abc", "<b>", "🍕 🍕", "", ":smile:"] {
        let (status, body) = set_status(
            &harness,
            &session,
            serde_json::json!({ "status": { "text": "x", "emoji": bad } }),
        )
        .await;
        assert_eq!(status, Status::BadRequest, "{bad}: {body}");
    }

    // keycaps and flags are emoji too
    for good in ["1️⃣", "🇦🇹", "👩‍💻", "❤️"] {
        let (status, body) = set_status(
            &harness,
            &session,
            serde_json::json!({ "status": { "text": "x", "emoji": good } }),
        )
        .await;
        assert_eq!(status, Status::Ok, "{good}: {body}");
    }
}

#[rocket::async_test]
async fn a_custom_emoji_must_come_from_a_server_you_are_in() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, other) = harness.new_user().await;

    let mine = server_of(&harness, &user).await;
    let theirs = server_of(&harness, &other).await;

    let make = |server: String, creator: String| Emoji {
        id: Ulid::new().to_string(),
        parent: EmojiParent::Server { id: server },
        creator_id: creator,
        name: "wave".to_string(),
        animated: false,
        nsfw: false,
    };
    let usable = make(mine.id.clone(), user.id.clone());
    usable.create(&harness.db).await.unwrap();
    let foreign = make(theirs.id.clone(), other.id.clone());
    foreign.create(&harness.db).await.unwrap();

    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "x", "emoji": foreign.id } }),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{body}");

    // an id that names no emoji at all
    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "x", "emoji": Ulid::new().to_string() } }),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{body}");

    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "x", "emoji": usable.id } }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(
        me(&harness, &session).await.status.unwrap().emoji,
        Some(usable.id.clone())
    );
}

#[rocket::async_test]
async fn the_expiry_must_be_in_the_next_week() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    for when in [in_minutes(-5), in_minutes(60 * 24 * 8)] {
        let (status, body) = set_status(
            &harness,
            &session,
            serde_json::json!({ "status": { "text": "x", "expires_at": when } }),
        )
        .await;
        assert_eq!(status, Status::BadRequest, "{body}");
    }

    // an expiry with nothing to clear makes no sense
    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "presence": "Busy", "expires_at": in_minutes(30) } }),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{body}");
}

#[rocket::async_test]
async fn removing_the_status_text_clears_emoji_and_expiry_too() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    set_status(
        &harness,
        &session,
        serde_json::json!({ "status": {
            "text": "Away", "emoji": "🌙", "expires_at": in_minutes(60), "presence": "Idle",
        }}),
    )
    .await;

    let (status, body) =
        set_status(&harness, &session, serde_json::json!({ "remove": ["StatusText"] })).await;
    assert_eq!(status, Status::Ok, "{body}");

    let status = me(&harness, &session).await.status.unwrap();
    assert_eq!(status.text, None);
    assert_eq!(status.emoji, None);
    assert_eq!(status.expires_at, None);
    assert_eq!(status.presence, Some(v0::Presence::Idle));

    // and the stored user agrees
    let session_user = harness.db.fetch_user(&session.user_id).await.unwrap();
    let stored = session_user.status.unwrap();
    assert_eq!((stored.text, stored.emoji, stored.expires_at), (None, None, None));
}

#[rocket::async_test]
async fn a_new_text_without_expiry_never_inherits_the_old_one() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "Short", "emoji": "⏳", "expires_at": in_minutes(30) } }),
    )
    .await;
    // an older client only knows text
    set_status(&harness, &session, serde_json::json!({ "status": { "text": "Long" } })).await;

    let status = me(&harness, &session).await.status.unwrap();
    assert_eq!(status.text.as_deref(), Some("Long"));
    assert_eq!(status.expires_at, None);

    // presence changes keep the expiry
    set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "Again", "expires_at": in_minutes(30) } }),
    )
    .await;
    set_status(&harness, &session, serde_json::json!({ "status": { "presence": "Busy" } })).await;
    assert!(me(&harness, &session).await.status.unwrap().expires_at.is_some());
}

/// Store a status that ran out a minute ago, without the route's checks
async fn store_expired(harness: &TestHarness, user: &mut User) {
    user.update(
        &harness.db,
        PartialUser {
            status: Some(UserStatus {
                text: Some("Old news".to_string()),
                emoji: Some("📰".to_string()),
                expires_at: Some(Timestamp::now_utc() + Duration::minutes(-1)),
                presence: Some(revolt_database::Presence::Busy),
            }),
            ..Default::default()
        },
        vec![],
    )
    .await
    .unwrap();
}

#[rocket::async_test]
async fn an_expired_status_is_not_shown_even_before_it_is_cleared() {
    let harness = TestHarness::new().await;
    let (_, session, mut user) = harness.new_user().await;
    let (_, friend_session, mut friend) = harness.new_user().await;
    befriend(&harness, &mut user, &mut friend).await;
    store_expired(&harness, &mut user).await;

    let status = me(&harness, &session).await.status.unwrap();
    assert_eq!((status.text, status.emoji, status.expires_at), (None, None, None));
    assert_eq!(status.presence, Some(v0::Presence::Busy));

    let (_, body) = call(
        &harness,
        Method::Get,
        &friend_session,
        &format!("/users/{}", user.id),
        None,
    )
    .await;
    assert!(!body.contains("Old news"), "{body}");
}

#[rocket::async_test]
async fn expired_statuses_are_cleared_from_storage() {
    let harness = TestHarness::new().await;
    let (_, _, mut expired) = harness.new_user().await;
    let (_, live_session, _) = harness.new_user().await;
    store_expired(&harness, &mut expired).await;
    set_status(
        &harness,
        &live_session,
        serde_json::json!({ "status": { "text": "Still here", "expires_at": in_minutes(30) } }),
    )
    .await;

    let cleared = User::clear_expired_statuses(&harness.db).await.unwrap();
    assert!(cleared >= 1);

    let stored = harness.db.fetch_user(&expired.id).await.unwrap().status.unwrap();
    assert_eq!((stored.text, stored.emoji, stored.expires_at), (None, None, None));
    assert_eq!(stored.presence, Some(revolt_database::Presence::Busy));

    let live = harness.db.fetch_user(&live_session.user_id).await.unwrap();
    assert_eq!(live.status.unwrap().text.as_deref(), Some("Still here"));

    // safe to run again
    User::clear_expired_statuses(&harness.db).await.unwrap();
}
