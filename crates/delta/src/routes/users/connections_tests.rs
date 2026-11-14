//! Linked Steam accounts, through the real routes. Steam itself answers
//! in-process (`util::steam::test_steam`): no network, no key.

use std::collections::BTreeMap;

use iso8601_timestamp::Timestamp;
use revolt_database::{Account, AccountAge, Channel, RelationshipStatus, Session, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Method, Status};

use crate::util::{
    steam::{self, test_steam},
    test::TestHarness,
};

const ORIGIN: &str = "http://localhost:5173";

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

async fn befriend(harness: &TestHarness, a: &mut User, b: &mut User) {
    #[allow(clippy::disallowed_methods)]
    a.apply_relationship(&harness.db, b, RelationshipStatus::Friend, RelationshipStatus::Friend)
        .await
        .unwrap();
}

async fn share(harness: &TestHarness, a: &User, b: &User) {
    let mut group = Channel::create_group(
        &harness.db,
        v0::DataCreateGroup { name: TestHarness::rand_string(), ..Default::default() },
        a.id.to_string(),
    )
    .await
    .unwrap();
    group.add_user_to_group(&harness.db, &harness.amqp, b, &a.id).await.unwrap();
}

/// Start, and the state Steam will send back
async fn start(harness: &TestHarness, session: &Session) -> String {
    let (status, body) = call(
        harness,
        Method::Post,
        session,
        "/users/@me/connections/steam/start",
        Some(serde_json::json!({ "origin": ORIGIN })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let answer: v0::SteamStartResponse = serde_json::from_str(&body).unwrap();
    let url = url::Url::parse(&answer.url).unwrap();
    assert_eq!(url.host_str(), Some("steamcommunity.com"));
    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    let back = url::Url::parse(&query["openid.return_to"]).unwrap();
    assert_eq!(back.origin().ascii_serialization(), ORIGIN);
    assert_eq!(back.path(), "/connections/steam/return");
    back.query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned()
}

async fn finish(
    harness: &TestHarness,
    session: &Session,
    state: &str,
    params: BTreeMap<String, String>,
) -> (Status, String) {
    call(
        harness,
        Method::Post,
        session,
        "/users/@me/connections/steam/finish",
        Some(serde_json::json!({ "state": state, "params": params })),
    )
    .await
}

/// Link a Steam account as `steam_id`, and return the owner's view
async fn link(harness: &TestHarness, session: &Session, steam_id: &str) -> v0::LinkedAccount {
    let state = start(harness, session).await;
    let back = steam::return_to(ORIGIN, &state);
    let (status, body) = finish(harness, session, &state, test_steam::assertion(steam_id, &back)).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

async fn show(harness: &TestHarness, session: &Session, body: serde_json::Value) -> v0::LinkedAccount {
    let (status, text) = call(harness, Method::Patch, session, "/users/@me/connections/steam", Some(body)).await;
    assert_eq!(status, Status::Ok, "{text}");
    serde_json::from_str(&text).unwrap()
}

async fn profile(harness: &TestHarness, session: &Session, id: &str) -> v0::UserProfile {
    let (status, body) = call(harness, Method::Get, session, &format!("/users/{id}/profile"), None).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[rocket::async_test]
async fn steam_links_through_steam_and_shows_after_the_preview() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = person(&harness, 30).await;
    let (_, viewer_session, viewer) = person(&harness, 30).await;
    share(&harness, &owner, &viewer).await;

    let (status, body) = call(&harness, Method::Get, &owner_session, "/users/@me/connections", None).await;
    assert_eq!(status, Status::Ok);
    let list: v0::LinkedAccounts = serde_json::from_str(&body).unwrap();
    assert!(list.steam.available);
    assert!(list.steam.account.is_none());

    let linked = link(&harness, &owner_session, test_steam::PUBLIC).await;
    assert!(!linked.shown, "a new link starts hidden");
    assert!(!linked.private);
    assert_eq!(linked.game_count, Some(3));
    assert_eq!(linked.total_hours, Some(110));
    let names: Vec<_> = linked.games.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, vec!["Dota 2", "Portal 2"], "unplayed games are left out");
    assert_eq!(linked.games[0].achievements_unlocked, Some(12));
    assert_eq!(linked.games[0].achievements_total, Some(30));

    // not shown yet: the viewer gets nothing
    assert!(profile(&harness, &viewer_session, &owner.id).await.games.is_none());

    let shown = show(&harness, &owner_session, serde_json::json!({ "shown": true, "hidden_titles": [620, 620] })).await;
    assert_eq!(shown.hidden_titles, vec![620]);

    let games = profile(&harness, &viewer_session, &owner.id).await.games.expect("games");
    assert_eq!(games.service, "steam");
    assert!(games.display_name.starts_with("Player "));
    assert!(games.profile_url.starts_with("https://steamcommunity.com/"));
    let names: Vec<_> = games.games.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, vec!["Dota 2"], "hidden games never reach viewers");

    // the raw profile carries no stored id fields, times or hidden list (the
    // SteamID shows only inside Steam's own profile link, which is public)
    let (_, raw) = call(&harness, Method::Get, &viewer_session, &format!("/users/{}/profile", owner.id), None).await;
    for field in ["external_id", "hidden_titles", "linked_at", "refreshed_at", "Portal 2"] {
        assert!(!raw.contains(field), "{field} in {raw}");
    }

    // the owner sees the same games on their own profile
    assert_eq!(profile(&harness, &owner_session, &owner.id).await.games.unwrap().games.len(), 1);
}

#[rocket::async_test]
async fn only_allowed_origins_and_one_use_states() {
    let harness = TestHarness::new().await;
    let (_, session, _) = person(&harness, 30).await;
    let (_, other_session, _) = person(&harness, 30).await;

    let (status, _) = call(
        &harness,
        Method::Post,
        &session,
        "/users/@me/connections/steam/start",
        Some(serde_json::json!({ "origin": "https://evil.example" })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);

    // a state belongs to the person who started it
    let state = start(&harness, &session).await;
    let back = steam::return_to(ORIGIN, &state);
    let (status, _) = finish(&harness, &other_session, &state, test_steam::assertion(test_steam::OTHER, &back)).await;
    assert_eq!(status, Status::BadRequest);
    // ... and that attempt used it up
    let (status, _) = finish(&harness, &session, &state, test_steam::assertion(test_steam::PUBLIC, &back)).await;
    assert_eq!(status, Status::BadRequest);

    // works once
    let state = start(&harness, &session).await;
    let back = steam::return_to(ORIGIN, &state);
    let params = test_steam::assertion(test_steam::PUBLIC, &back);
    assert_eq!(finish(&harness, &session, &state, params.clone()).await.0, Status::Ok);
    assert_eq!(finish(&harness, &session, &state, params).await.0, Status::BadRequest);

    // an unknown state
    let (status, _) = finish(&harness, &session, "0123456789abcdef0123", BTreeMap::new()).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn steam_must_vouch_for_the_answer() {
    let harness = TestHarness::new().await;
    let (_, session, _) = person(&harness, 30).await;

    // Steam refuses the signature
    let state = start(&harness, &session).await;
    let back = steam::return_to(ORIGIN, &state);
    let mut params = test_steam::assertion(test_steam::PUBLIC, &back);
    params.insert("openid.sig".into(), test_steam::BAD_SIGNATURE.into());
    assert_eq!(finish(&harness, &session, &state, params).await.0, Status::BadRequest);

    // an answer meant for another return address
    let state = start(&harness, &session).await;
    let elsewhere = steam::return_to("https://evil.example", &state);
    let params = test_steam::assertion(test_steam::PUBLIC, &elsewhere);
    assert_eq!(finish(&harness, &session, &state, params).await.0, Status::BadRequest);

    let (_, body) = call(&harness, Method::Get, &session, "/users/@me/connections", None).await;
    let list: v0::LinkedAccounts = serde_json::from_str(&body).unwrap();
    assert!(list.steam.account.is_none());
}

#[rocket::async_test]
async fn one_steam_account_links_to_one_person() {
    let harness = TestHarness::new().await;
    let (_, first, _) = person(&harness, 30).await;
    let (_, second, _) = person(&harness, 30).await;
    link(&harness, &first, test_steam::PUBLIC).await;

    let state = start(&harness, &second).await;
    let back = steam::return_to(ORIGIN, &state);
    let (status, body) = finish(&harness, &second, &state, test_steam::assertion(test_steam::PUBLIC, &back)).await;
    assert_eq!(status, Status::Conflict, "{body}");
    assert!(body.contains("LinkedAccountTaken"), "{body}");

    // the first can link it again (a re-link keeps the person's hidden games)
    show(&harness, &first, serde_json::json!({ "shown": true, "hidden_titles": [570] })).await;
    let again = link(&harness, &first, test_steam::PUBLIC).await;
    assert_eq!(again.hidden_titles, vec![570]);
    assert!(!again.shown, "re-linking asks for the preview again");
}

#[rocket::async_test]
async fn minors_show_games_to_friends_only_and_hidden_sections_hide_them() {
    let harness = TestHarness::new().await;
    let (_, minor_session, mut minor) = person(&harness, 16).await;
    let (_, friend_session, mut friend) = person(&harness, 30).await;
    let (_, stranger_session, stranger) = person(&harness, 30).await;
    befriend(&harness, &mut minor, &mut friend).await;
    share(&harness, &minor, &stranger).await;

    link(&harness, &minor_session, test_steam::PUBLIC).await;
    show(&harness, &minor_session, serde_json::json!({ "shown": true })).await;

    assert!(profile(&harness, &friend_session, &minor.id).await.games.is_some());
    let seen = profile(&harness, &stranger_session, &minor.id).await;
    assert!(seen.games.is_none());
    assert!(!seen.sections.unwrap_or_default().contains(&"games".to_string()));

    // the person hides the section: nobody else sees it
    let (status, body) = call(
        &harness,
        Method::Patch,
        &minor_session,
        "/users/@me",
        Some(serde_json::json!({ "profile": { "hidden_sections": ["games"] } })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(profile(&harness, &friend_session, &minor.id).await.games.is_none());
}

#[rocket::async_test]
async fn a_private_steam_account_shows_one_plain_line() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = person(&harness, 30).await;
    let (_, viewer_session, viewer) = person(&harness, 30).await;
    share(&harness, &owner, &viewer).await;

    let linked = link(&harness, &owner_session, test_steam::PRIVATE).await;
    assert!(linked.private);
    assert!(linked.games.is_empty());
    show(&harness, &owner_session, serde_json::json!({ "shown": true })).await;

    let games = profile(&harness, &viewer_session, &owner.id).await.games.expect("games");
    assert!(games.private);
    assert!(games.display_name.is_empty() && games.profile_url.is_empty());
    assert!(games.games.is_empty() && games.game_count.is_none());
}

#[rocket::async_test]
async fn refresh_waits_and_disconnect_removes_everything() {
    let harness = TestHarness::new().await;
    let (mut account, session, user) = person(&harness, 30).await;
    link(&harness, &session, test_steam::PUBLIC).await;

    let (status, body) = call(&harness, Method::Post, &session, "/users/@me/connections/steam/refresh", None).await;
    assert_eq!(status, Status::TooManyRequests, "{body}");
    assert!(body.contains("LinkedRefreshTooSoon"), "{body}");

    // ten minutes later
    let mut stored = harness.db.fetch_linked_account(&user.id, "steam").await.unwrap().unwrap();
    stored.refreshed_at -= super::connections::REFRESH_COOLDOWN_MS + 1;
    harness.db.save_linked_account(&stored).await.unwrap();
    let (status, body) = call(&harness, Method::Post, &session, "/users/@me/connections/steam/refresh", None).await;
    assert_eq!(status, Status::Ok, "{body}");

    let (status, _) = call(&harness, Method::Delete, &session, "/users/@me/connections/steam", None).await;
    assert_eq!(status, Status::NoContent);
    assert!(harness.db.fetch_linked_account(&user.id, "steam").await.unwrap().is_none());
    let (status, _) = call(&harness, Method::Patch, &session, "/users/@me/connections/steam",
        Some(serde_json::json!({ "shown": true }))).await;
    assert_eq!(status, Status::NotFound);

    // account erasure takes the link too
    link(&harness, &session, test_steam::PUBLIC).await;
    account.mark_deleted(&harness.db).await.unwrap();
    assert!(harness.db.fetch_linked_accounts(&user.id).await.unwrap().is_empty());
}
