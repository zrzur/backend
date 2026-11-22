//! Rich presence through the real routes: the sharing setting and its
//! default by age, who sees an activity, and Steam "now playing". Steam
//! answers in-process (`util::steam::test_steam`).

use iso8601_timestamp::Timestamp;
use revolt_database::{
    util::activity, Account, AccountAge, PartialUser, RelationshipStatus, Session, User,
};
use revolt_models::v0::{self, ActivitySource, ActivityType, UserActivity};
use rocket::http::{ContentType, Header, Method, Status};
use serde_json::json;

use crate::util::{
    steam::test_steam,
    steam_now_playing,
    test::TestHarness,
};

fn born(years: i32) -> (i32, u8) {
    let now = Timestamp::now_utc();
    (now.year() - years - 1, u8::from(now.month()))
}

async fn person(harness: &TestHarness, years: i32) -> (Account, Session, User) {
    let (mut account, session, user) = harness.new_user().await;
    account.age = Some(AccountAge::declare(born(years)));
    account.save(&harness.db).await.unwrap();
    (account, session, user)
}

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

async fn privacy(harness: &TestHarness, session: &Session) -> v0::UserPrivacy {
    let (status, body) = call(harness, Method::Get, session, "/users/@me/privacy", None).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

async fn set_privacy(harness: &TestHarness, session: &Session, body: serde_json::Value) -> v0::UserPrivacy {
    let (status, text) = call(harness, Method::Patch, session, "/users/@me/privacy", Some(body)).await;
    assert_eq!(status, Status::Ok, "{text}");
    serde_json::from_str(&text).unwrap()
}

async fn relate(harness: &TestHarness, a: &mut User, b: &mut User, ours: RelationshipStatus, theirs: RelationshipStatus) {
    #[allow(clippy::disallowed_methods)]
    a.apply_relationship(&harness.db, b, ours, theirs).await.unwrap();
}

fn game(name: &str) -> UserActivity {
    UserActivity {
        source: ActivitySource::Rpc,
        kind: ActivityType::Playing,
        name: name.into(),
        details: Some("Level 3".into()),
        state: None,
        started_at: Some(Timestamp::now_utc()),
        ends_at: None,
        application_id: None,
        large_image: None,
        large_text: None,
        small_image: None,
        small_text: None,
        buttons: vec![],
    }
}

async fn seen_by(harness: &TestHarness, viewer: &Session, target: &str) -> v0::User {
    let (status, body) = call(harness, Method::Get, viewer, &format!("/users/{target}"), None).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[rocket::async_test]
async fn sharing_is_on_for_adults_and_off_under_18_until_chosen() {
    let harness = TestHarness::new().await;
    let (_, adult, _) = person(&harness, 30).await;
    let (_, teen, _) = person(&harness, 16).await;

    let grown = privacy(&harness, &adult).await;
    assert_eq!(grown.share_activity, Some(true));
    assert!(!grown.share_steam_activity, "Steam is off until chosen");
    assert_eq!(privacy(&harness, &teen).await.share_activity, Some(false));

    // changing something else keeps "the default for your age" unsaved
    set_privacy(&harness, &teen, json!({ "group_invites": "Nobody" })).await;
    assert_eq!(privacy(&harness, &teen).await.share_activity, Some(false));

    // a teen may turn it on; an adult may turn it off
    assert_eq!(set_privacy(&harness, &teen, json!({ "share_activity": true })).await.share_activity, Some(true));
    assert_eq!(set_privacy(&harness, &adult, json!({ "share_activity": false })).await.share_activity, Some(false));
    let steam = set_privacy(&harness, &adult, json!({ "share_steam_activity": true })).await;
    assert!(steam.share_steam_activity);
}

#[rocket::async_test]
async fn turning_sharing_off_removes_what_others_see_at_once() {
    let harness = TestHarness::new().await;
    let (_, session, user) = person(&harness, 30).await;
    assert!(activity::store(&user.id, ActivitySource::Rpc, Some(game("Star Kebab")), Some("s")).await);
    assert_eq!(activity::list(&user.id).await.len(), 1);

    set_privacy(&harness, &session, json!({ "share_activity": false })).await;
    assert!(activity::list(&user.id).await.is_empty());
}

#[rocket::async_test]
async fn turning_steam_off_removes_only_the_steam_activity() {
    let harness = TestHarness::new().await;
    let (_, session, user) = person(&harness, 30).await;
    activity::store(&user.id, ActivitySource::Rpc, Some(game("Star Kebab")), Some("s")).await;
    let mut steam = game("Portal 2");
    steam.source = ActivitySource::Steam;
    activity::store(&user.id, ActivitySource::Steam, Some(steam), None).await;

    set_privacy(&harness, &session, json!({ "share_steam_activity": false })).await;
    let left = activity::list(&user.id).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].source, ActivitySource::Rpc);
}

#[rocket::async_test]
async fn a_friend_sees_it_while_online_and_visible_never_when_blocked() {
    let harness = TestHarness::new().await;
    let (_, _, mut owner) = person(&harness, 30).await;
    let (_, friend_session, mut friend) = person(&harness, 30).await;
    relate(&harness, &mut owner, &mut friend, RelationshipStatus::Friend, RelationshipStatus::Friend).await;

    activity::store(&owner.id, ActivitySource::Rpc, Some(game("Star Kebab")), Some("s")).await;

    // offline: nothing
    assert!(seen_by(&harness, &friend_session, &owner.id).await.activities.is_empty());

    let (_, presence) = revolt_presence::create_session(&owner.id, 0).await;
    let seen = seen_by(&harness, &friend_session, &owner.id).await;
    assert!(seen.online);
    assert_eq!(seen.activities.len(), 1);
    assert_eq!(seen.activities[0].name, "Star Kebab");

    // lists of users carry it too (members, DMs)
    let many = User::fetch_many_ids_as_mutuals(&harness.db, &friend, &[owner.id.clone()]).await.unwrap();
    assert_eq!(many[0].activities[0].name, "Star Kebab");

    // invisible: nothing, though the activity is still kept for its owner
    harness
        .db
        .update_user(
            &owner.id,
            &PartialUser {
                status: Some(revolt_database::UserStatus {
                    presence: Some(revolt_database::Presence::Invisible),
                    ..Default::default()
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    assert!(seen_by(&harness, &friend_session, &owner.id).await.activities.is_empty());
    let own = harness.db.fetch_user(&owner.id).await.unwrap().into_self(false).await;
    assert_eq!(own.activities.len(), 1, "you still see your own");

    // blocked by the owner: nothing
    harness
        .db
        .update_user(
            &owner.id,
            &PartialUser {
                status: Some(revolt_database::UserStatus::default()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(seen_by(&harness, &friend_session, &owner.id).await.activities.len(), 1);
    let mut owner = harness.db.fetch_user(&owner.id).await.unwrap();
    let mut friend = harness.db.fetch_user(&friend.id).await.unwrap();
    relate(&harness, &mut owner, &mut friend, RelationshipStatus::Blocked, RelationshipStatus::BlockedOther).await;
    let viewer = harness.db.fetch_user(&friend.id).await.unwrap();
    let many = User::fetch_many_ids_as_mutuals(&harness.db, &viewer, &[owner.id.clone()]).await.unwrap();
    assert!(many[0].activities.is_empty());

    // the last session takes it away
    revolt_presence::delete_session(&owner.id, presence).await;
    assert!(activity::list(&owner.id).await.is_empty());
}

#[rocket::async_test]
async fn activities_are_never_written_to_the_database() {
    let harness = TestHarness::new().await;
    let (_, _, user) = person(&harness, 30).await;
    activity::store(&user.id, ActivitySource::Rpc, Some(game("Star Kebab")), Some("s")).await;
    let stored = serde_json::to_string(&harness.db.fetch_user(&user.id).await.unwrap()).unwrap();
    assert!(!stored.contains("Star Kebab"));
}

async fn link_steam(harness: &TestHarness, user: &User, steam_id: &str) {
    let account: revolt_database::LinkedAccount = serde_json::from_value(json!({
        "_id": revolt_database::LinkedAccount::key(&user.id, "steam"),
        "user_id": user.id,
        "service": "steam",
        "external_id": steam_id,
        "consent": revolt_database::LINKED_CONSENT_VERSION,
        "linked_at": 1,
        "refreshed_at": 1,
    }))
    .unwrap();
    harness.db.save_linked_account(&account).await.unwrap();
}

#[rocket::async_test]
async fn steam_now_playing_is_read_only_for_people_who_share_it_and_are_online() {
    let harness = TestHarness::new().await;
    let (_, sharing_session, sharing) = person(&harness, 30).await;
    let (_, other_session, other) = person(&harness, 30).await;
    let (_, _, quiet) = person(&harness, 30).await;
    let (_, offline_session, offline) = person(&harness, 30).await;
    link_steam(&harness, &sharing, test_steam::PUBLIC).await;
    link_steam(&harness, &other, test_steam::OTHER).await;
    link_steam(&harness, &quiet, test_steam::PRIVATE).await;
    link_steam(&harness, &offline, "76561197960287999").await;

    set_privacy(&harness, &sharing_session, json!({ "share_steam_activity": true })).await;
    set_privacy(&harness, &other_session, json!({ "share_steam_activity": true })).await;
    set_privacy(&harness, &offline_session, json!({ "share_steam_activity": true })).await;
    for user in [&sharing, &other, &quiet] {
        revolt_presence::create_session(&user.id, 0).await;
    }

    let before = steam_now_playing::calls_today().await;
    let calls = steam_now_playing::run_once(&harness.db, Timestamp::now_utc()).await.unwrap();
    assert!(calls >= 1);
    assert!(steam_now_playing::calls_today().await >= before + 1);

    let portal = activity::list(&sharing.id).await;
    assert_eq!(portal.len(), 1);
    assert_eq!(portal[0].source, ActivitySource::Steam);
    assert_eq!(portal[0].name, "Portal 2");
    assert_eq!(portal[0].large_image, None, "a game whose maker asked: no picture");
    let started = portal[0].started_at;
    assert!(started.is_some());

    let half_life = activity::list(&other.id).await;
    assert_eq!(half_life[0].name, "Half-Life 2", "trimmed");
    let picture = half_life[0].large_image.clone().unwrap();
    assert!(picture.contains("/proxy?url=https%3A%2F%2F"), "{picture}");

    assert!(activity::list(&quiet.id).await.is_empty(), "did not choose Steam");
    assert!(activity::list(&offline.id).await.is_empty(), "offline");

    // a second round keeps the time the game started
    steam_now_playing::run_once(&harness.db, Timestamp::now_utc()).await.unwrap();
    assert_eq!(activity::list(&sharing.id).await[0].started_at, started);

    // turning sharing off stops it and removes it
    set_privacy(&harness, &sharing_session, json!({ "share_activity": false })).await;
    steam_now_playing::run_once(&harness.db, Timestamp::now_utc()).await.unwrap();
    assert!(activity::list(&sharing.id).await.is_empty());
}
