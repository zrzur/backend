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

#[rocket::async_test]
async fn sending_the_same_text_again_keeps_its_emoji_and_expiry() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "Lunch", "emoji": "🍕", "expires_at": in_minutes(30) } }),
    )
    .await;
    // an older client (or an approved change request) sends the same text
    let (status, body) =
        set_status(&harness, &session, serde_json::json!({ "status": { "text": "Lunch" } })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let kept = me(&harness, &session).await.status.unwrap();
    assert_eq!(kept.emoji.as_deref(), Some("🍕"));
    assert!(kept.expires_at.is_some());

    // a different text still starts afresh
    set_status(&harness, &session, serde_json::json!({ "status": { "text": "Back" } })).await;
    let fresh = me(&harness, &session).await.status.unwrap();
    assert_eq!((fresh.emoji, fresh.expires_at), (None, None));
}

#[rocket::async_test]
async fn an_emoji_only_status_replaces_the_old_text() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "Old", "emoji": "🌙", "presence": "Idle" } }),
    )
    .await;
    // what the app sends for an emoji with no text
    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "emoji": "🎧", "expires_at": in_minutes(60) }, "remove": ["StatusText"] }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");

    let shown = me(&harness, &session).await.status.unwrap();
    assert_eq!(shown.text, None);
    assert_eq!(shown.emoji.as_deref(), Some("🎧"));
    assert!(shown.expires_at.is_some());
    assert_eq!(shown.presence, Some(v0::Presence::Idle));

    let stored = harness.db.fetch_user(&session.user_id).await.unwrap().status.unwrap();
    assert_eq!((stored.text, stored.emoji.as_deref()), (None, Some("🎧")));
}

#[rocket::async_test]
async fn a_slow_clock_gets_two_minutes_of_grace_past_a_week() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "x", "expires_at": in_minutes(60 * 24 * 7 + 1) } }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");

    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "x", "expires_at": in_minutes(60 * 24 * 7 + 5) } }),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{body}");
}

#[rocket::async_test]
async fn the_sweep_never_clears_a_status_set_after_it_looked() {
    let harness = TestHarness::new().await;
    let (_, session, mut racer) = harness.new_user().await;
    let (_, _, mut stale) = harness.new_user().await;
    store_expired(&harness, &mut racer).await;
    store_expired(&harness, &mut stale).await;

    let now = Timestamp::now_utc();
    let seen = harness.db.fetch_expired_status_ids(now, 1000).await.unwrap();
    assert!(seen.contains(&racer.id) && seen.contains(&stale.id));

    // between the sweep's look and its write, the person sets a new status
    set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "Fresh", "expires_at": in_minutes(30) } }),
    )
    .await;

    assert!(!harness.db.clear_expired_status(&racer.id, now).await.unwrap());
    assert!(harness.db.clear_expired_status(&stale.id, now).await.unwrap());
    // a second time there is nothing left to clear
    assert!(!harness.db.clear_expired_status(&stale.id, now).await.unwrap());

    let kept = harness.db.fetch_user(&racer.id).await.unwrap().status.unwrap();
    assert_eq!(kept.text.as_deref(), Some("Fresh"));
}

#[rocket::async_test]
async fn a_custom_status_emoji_never_reveals_a_server_the_viewer_is_not_in() {
    let harness = TestHarness::new().await;
    let (_, owner_session, mut owner) = harness.new_user().await;
    let (_, outsider_session, mut outsider) = harness.new_user().await;
    let (_, insider_session, insider) = harness.new_user().await;

    let server = server_of(&harness, &owner).await;
    Member::create(&harness.db, &server, &insider, None).await.unwrap();
    // a second shared server keeps the profile visible after the owner
    // leaves the emoji's server
    let other = server_of(&harness, &owner).await;
    Member::create(&harness.db, &other, &insider, None).await.unwrap();
    // the outsider sees the owner's profile as a friend, but is not in the server
    befriend(&harness, &mut owner, &mut outsider).await;

    let emoji = Emoji {
        id: Ulid::new().to_string(),
        parent: EmojiParent::Server { id: server.id.clone() },
        creator_id: owner.id.clone(),
        name: "secret".to_string(),
        animated: false,
        nsfw: false,
    };
    emoji.create(&harness.db).await.unwrap();
    let (status, body) = set_status(
        &harness,
        &owner_session,
        serde_json::json!({ "status": { "text": "Hi", "emoji": emoji.id } }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");

    let seen_by = |session: Session| {
        let harness = &harness;
        let id = owner.id.clone();
        async move {
            let (status, body) =
                call(harness, Method::Get, &session, &format!("/users/{id}"), None).await;
            assert_eq!(status, Status::Ok, "{body}");
            serde_json::from_str::<v0::User>(&body).unwrap().status.unwrap()
        }
    };

    // the owner always sees their own emoji
    assert_eq!(me(&harness, &owner_session).await.status.unwrap().emoji, Some(emoji.id.clone()));
    // someone in the server sees it
    assert_eq!(seen_by(insider_session.clone()).await.emoji, Some(emoji.id.clone()));
    // someone outside the server sees the text, and no emoji
    let outside = seen_by(outsider_session.clone()).await;
    assert_eq!(outside.text.as_deref(), Some("Hi"));
    assert_eq!(outside.emoji, None);
    assert!(!body_mentions(&harness, &outsider_session, &owner.id, &emoji.id).await);

    // once the owner leaves the server, nobody else sees it
    let member = harness.db.fetch_member(&server.id, &owner.id).await.unwrap();
    member
        .remove(&harness.db, &server, revolt_database::RemovalIntention::Leave, true)
        .await
        .unwrap();
    assert_eq!(seen_by(insider_session).await.emoji, None);
}

/// Whether the raw user object the viewer gets names this id anywhere
async fn body_mentions(harness: &TestHarness, session: &Session, user: &str, needle: &str) -> bool {
    let (_, body) = call(harness, Method::Get, session, &format!("/users/{user}"), None).await;
    body.contains(needle)
}

#[rocket::async_test]
async fn the_dialogs_explicit_replacement_clears_emoji_and_expiry_with_the_same_text() {
    let harness = TestHarness::new().await;
    let (_, session, _) = harness.new_user().await;

    set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "Lunch", "emoji": "🍕", "expires_at": in_minutes(30) } }),
    )
    .await;
    // "remove the emoji" and "Never", same text: what the dialog sends
    let (status, body) = set_status(
        &harness,
        &session,
        serde_json::json!({ "status": { "text": "Lunch" }, "remove": ["StatusText"] }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let shown = me(&harness, &session).await.status.unwrap();
    assert_eq!(shown.text.as_deref(), Some("Lunch"));
    assert_eq!((shown.emoji, shown.expires_at), (None, None));
}

#[rocket::async_test]
async fn changing_your_username_keeps_your_own_custom_status_emoji() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let server = server_of(&harness, &user).await;
    let emoji = Emoji {
        id: Ulid::new().to_string(),
        parent: EmojiParent::Server { id: server.id.clone() },
        creator_id: user.id.clone(),
        name: "mine".to_string(),
        animated: false,
        nsfw: false,
    };
    emoji.create(&harness.db).await.unwrap();
    set_status(&harness, &session, serde_json::json!({ "status": { "text": "Hi", "emoji": emoji.id } })).await;

    let (status, body) = call(
        &harness,
        Method::Patch,
        &session,
        "/users/@me/username",
        Some(serde_json::json!({ "username": "renamed", "password": "password_insecure" })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let renamed: v0::User = serde_json::from_str(&body).unwrap();
    assert_eq!(renamed.status.unwrap().emoji, Some(emoji.id));
}

#[rocket::async_test]
async fn a_member_list_never_shows_a_status_emoji_from_a_server_the_viewer_is_not_in() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, outsider_session, outsider) = harness.new_user().await;
    let (_, insider_session, insider) = harness.new_user().await;

    // everyone is in the listed server; the emoji comes from another one
    let listed = server_of(&harness, &owner).await;
    Member::create(&harness.db, &listed, &outsider, None).await.unwrap();
    Member::create(&harness.db, &listed, &insider, None).await.unwrap();
    let private = server_of(&harness, &owner).await;
    Member::create(&harness.db, &private, &insider, None).await.unwrap();

    let emoji = Emoji {
        id: Ulid::new().to_string(),
        parent: EmojiParent::Server { id: private.id.clone() },
        creator_id: owner.id.clone(),
        name: "secret".to_string(),
        animated: false,
        nsfw: false,
    };
    emoji.create(&harness.db).await.unwrap();
    set_status(&harness, &owner_session, serde_json::json!({ "status": { "text": "Hi", "emoji": emoji.id } })).await;

    let owner_status = |session: Session| {
        let harness = &harness;
        let (server, owner) = (listed.id.clone(), owner.id.clone());
        async move {
            let (status, body) =
                call(harness, Method::Get, &session, &format!("/servers/{server}/members"), None).await;
            assert_eq!(status, Status::Ok, "{body}");
            let list: serde_json::Value = serde_json::from_str(&body).unwrap();
            let user = list["users"]
                .as_array()
                .unwrap()
                .iter()
                .find(|user| user["_id"] == owner.as_str())
                .cloned()
                .unwrap();
            (user["status"]["text"].clone(), user["status"]["emoji"].clone(), body)
        }
    };

    let (text, emoji_seen, _) = owner_status(insider_session).await;
    assert_eq!((text.as_str(), emoji_seen.as_str()), (Some("Hi"), Some(emoji.id.as_str())));
    let (text, emoji_seen, body) = owner_status(outsider_session).await;
    assert_eq!(text.as_str(), Some("Hi"));
    assert!(emoji_seen.is_null(), "{body}");
    assert!(!body.contains(&emoji.id), "{body}");
}
