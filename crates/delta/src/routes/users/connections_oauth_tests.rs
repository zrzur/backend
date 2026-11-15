//! Linked Twitch, GitHub and Battle.net accounts, through the real routes.
//! The services answer in-process (`util::oauth_link::test_services`): no
//! network, no real ids or secrets. A code `c-<id>` signs in as `<id>`.

use std::collections::BTreeMap;

use iso8601_timestamp::Timestamp;
use revolt_database::{Account, AccountAge, Channel, RelationshipStatus, Session, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Method, Status};

use crate::util::{
    oauth_link::test_services::{DOWN, REFUSED},
    test::TestHarness,
    twitch::test_twitch,
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

/// A fresh numeric id, so parallel tests never share an account
fn fresh_id() -> String {
    let n: u64 = rand::random::<u32>() as u64 * 1000 + 1;
    n.to_string()
}

/// Start, check the sign-in page, and return the state
async fn start(harness: &TestHarness, session: &Session, service: &str) -> String {
    let (status, body) = call(
        harness,
        Method::Post,
        session,
        &format!("/users/@me/connections/{service}/start"),
        Some(serde_json::json!({ "origin": ORIGIN })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let answer: v0::ServiceStartResponse = serde_json::from_str(&body).unwrap();
    let url = url::Url::parse(&answer.url).unwrap();
    let host = match service {
        "twitch" => "id.twitch.tv",
        "github" => "github.com",
        _ => "oauth.battle.net",
    };
    assert_eq!(url.host_str(), Some(host));
    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["redirect_uri"], format!("{ORIGIN}/connections/{service}/return"));
    assert_eq!(query["client_id"], format!("test-{service}-client"));
    assert!(!answer.url.contains("secret"));
    assert_eq!(query.contains_key("code_challenge"), service == "github");
    query["state"].clone()
}

async fn finish(harness: &TestHarness, session: &Session, service: &str, state: &str, code: &str) -> (Status, String) {
    call(
        harness,
        Method::Post,
        session,
        &format!("/users/@me/connections/{service}/finish"),
        Some(serde_json::json!({ "state": state, "code": code })),
    )
    .await
}

async fn link(harness: &TestHarness, session: &Session, service: &str, id: &str) -> v0::ServiceAccount {
    let state = start(harness, session, service).await;
    let (status, body) = finish(harness, session, service, &state, &format!("c-{id}")).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

async fn edit(harness: &TestHarness, session: &Session, service: &str, body: serde_json::Value) -> (Status, String) {
    call(harness, Method::Patch, session, &format!("/users/@me/connections/{service}"), Some(body)).await
}

async fn profile(harness: &TestHarness, session: &Session, id: &str) -> v0::UserProfile {
    let (status, body) = call(harness, Method::Get, session, &format!("/users/{id}/profile"), None).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

async fn list(harness: &TestHarness, session: &Session) -> v0::LinkedAccounts {
    let (status, body) = call(harness, Method::Get, session, "/users/@me/connections", None).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[rocket::async_test]
async fn three_services_link_start_hidden_and_show_after_the_preview() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = person(&harness, 30).await;
    let (_, viewer_session, viewer) = person(&harness, 30).await;
    share(&harness, &owner, &viewer).await;

    let listed = list(&harness, &owner_session).await;
    assert!(listed.twitch.available && listed.github.available && listed.battlenet.available);
    assert!(listed.twitch.account.is_none());

    let (twitch_id, github_id, bnet_id) = (fresh_id(), fresh_id(), fresh_id());
    let twitch = link(&harness, &owner_session, "twitch", &twitch_id).await;
    assert_eq!(twitch.display_name, format!("Streamer{twitch_id}"));
    assert_eq!(twitch.profile_url, format!("https://www.twitch.tv/streamer_{twitch_id}"));
    assert!(!twitch.shown);
    assert_eq!(twitch.live, Some(false));
    assert!(test_twitch::called(&format!("subscribe {twitch_id}")));

    let github = link(&harness, &owner_session, "github", &github_id).await;
    assert_eq!(github.display_name, format!("dev-{github_id}"));
    let names: Vec<_> = github.repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["popular", "middle", "small", "tiny"]);
    // the top three are picked until the person chooses
    assert_eq!(github.shown_repos, vec![2, 3, 1]);

    let battlenet = link(&harness, &owner_session, "battlenet", &bnet_id).await;
    assert_eq!(battlenet.display_name, format!("Player#{bnet_id}"));
    assert_eq!(battlenet.profile_url, "");
    let expires = battlenet.expires_at.expect("expiry");
    assert!((expires - battlenet.linked_at - super::connections_oauth::BATTLENET_TTL_MS).abs() < 5_000);

    // nothing shows before the preview
    assert!(profile(&harness, &viewer_session, &owner.id).await.connections.is_empty());
    assert!(profile(&harness, &owner_session, &owner.id).await.connections.is_empty());

    for service in ["twitch", "github", "battlenet"] {
        let (status, body) = edit(&harness, &owner_session, service, serde_json::json!({ "shown": true })).await;
        assert_eq!(status, Status::Ok, "{body}");
    }
    let (status, body) = edit(&harness, &owner_session, "github", serde_json::json!({ "shown_repos": [3, 99, 3, 2] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let chosen: v0::ServiceAccount = serde_json::from_str(&body).unwrap();
    // unknown and repeated ids are dropped, the order is the person's
    assert_eq!(chosen.shown_repos, vec![3, 2]);

    let seen = profile(&harness, &viewer_session, &owner.id).await.connections;
    let services: Vec<_> = seen.iter().map(|c| c.service.as_str()).collect();
    assert_eq!(services, ["twitch", "github", "battlenet"]);
    let repos: Vec<_> = seen[1].repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(repos, ["middle", "popular"]);
    assert!(seen[2].repos.is_empty() && seen[2].live.is_none());

    // only GitHub has repositories to pick
    let (status, _) = edit(&harness, &owner_session, "twitch", serde_json::json!({ "shown_repos": [1] })).await;
    assert_eq!(status, Status::BadRequest);

    // the export carries the copy and nothing secret
    let stored = harness.db.fetch_linked_accounts(&owner.id).await.unwrap();
    assert_eq!(stored.len(), 3);
    let text = serde_json::to_string(&stored).unwrap();
    assert!(!text.contains("token") && !text.contains("secret"));
}

#[rocket::async_test]
async fn states_work_once_for_the_person_who_started() {
    let harness = TestHarness::new().await;
    let (_, session, _) = person(&harness, 30).await;
    let (_, other_session, _) = person(&harness, 30).await;

    // origin not on the list
    let (status, _) = call(
        &harness,
        Method::Post,
        &session,
        "/users/@me/connections/twitch/start",
        Some(serde_json::json!({ "origin": "https://evil.example" })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);

    // unknown service
    let (status, _) = call(
        &harness,
        Method::Post,
        &session,
        "/users/@me/connections/myspace/start",
        Some(serde_json::json!({ "origin": ORIGIN })),
    )
    .await;
    assert_eq!(status, Status::NotFound);

    // someone else's state
    let state = start(&harness, &session, "github").await;
    let (status, _) = finish(&harness, &other_session, "github", &state, &format!("c-{}", fresh_id())).await;
    assert_eq!(status, Status::BadRequest);
    // and it is used up now, for its owner too
    let (status, _) = finish(&harness, &session, "github", &state, &format!("c-{}", fresh_id())).await;
    assert_eq!(status, Status::BadRequest);

    // a state for one service does not work for another
    let state = start(&harness, &session, "twitch").await;
    let (status, _) = finish(&harness, &session, "battlenet", &state, &format!("c-{}", fresh_id())).await;
    assert_eq!(status, Status::BadRequest);

    // the service refuses the code, or fails
    let state = start(&harness, &session, "battlenet").await;
    let (status, body) = finish(&harness, &session, "battlenet", &state, REFUSED).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert!(body.contains("LinkedSignInFailed"));
    let state = start(&harness, &session, "battlenet").await;
    let (status, body) = finish(&harness, &session, "battlenet", &state, DOWN).await;
    assert_eq!(status, Status::BadGateway, "{body}");

    // a used state can't be replayed after success either
    let state = start(&harness, &session, "battlenet").await;
    let code = format!("c-{}", fresh_id());
    assert_eq!(finish(&harness, &session, "battlenet", &state, &code).await.0, Status::Ok);
    assert_eq!(finish(&harness, &session, "battlenet", &state, &code).await.0, Status::BadRequest);

    assert!(list(&harness, &other_session).await.github.account.is_none());
}

#[rocket::async_test]
async fn one_account_one_person_and_relinking_keeps_choices() {
    let harness = TestHarness::new().await;
    let (_, a_session, _) = person(&harness, 30).await;
    let (_, b_session, _) = person(&harness, 30).await;
    let id = fresh_id();

    link(&harness, &a_session, "github", &id).await;
    let state = start(&harness, &b_session, "github").await;
    let (status, body) = finish(&harness, &b_session, "github", &state, &format!("c-{id}")).await;
    assert_eq!(status, Status::Conflict, "{body}");

    // same account again: shown and the picks stay
    edit(&harness, &a_session, "github", serde_json::json!({ "shown": true, "shown_repos": [1] })).await;
    let again = link(&harness, &a_session, "github", &id).await;
    assert!(again.shown);
    assert_eq!(again.shown_repos, vec![1]);

    // another account: preview first again
    let other = link(&harness, &a_session, "github", &fresh_id()).await;
    assert!(!other.shown);
    assert_eq!(other.shown_repos, vec![2, 3, 1]);

    // Twitch: another channel ends the old channel's subscriptions
    let first = fresh_id();
    link(&harness, &a_session, "twitch", &first).await;
    link(&harness, &a_session, "twitch", &fresh_id()).await;
    assert!(test_twitch::called(&format!("unsubscribe sub-online-{first}")));
}

#[rocket::async_test]
async fn battlenet_is_deleted_after_thirty_days_unless_confirmed() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    let (_, viewer_session, viewer) = person(&harness, 30).await;
    share(&harness, &owner, &viewer).await;
    let id = fresh_id();
    link(&harness, &session, "battlenet", &id).await;
    edit(&harness, &session, "battlenet", serde_json::json!({ "shown": true })).await;
    assert_eq!(profile(&harness, &viewer_session, &owner.id).await.connections.len(), 1);

    // confirming again before the 30 days are over keeps it, still shown,
    // and starts the 30 days again
    let mut stored = harness.db.fetch_linked_account(&owner.id, "battlenet").await.unwrap().unwrap();
    stored.expires_at = Some(super::connections_oauth::now_ms() + 60_000);
    harness.db.save_linked_account(&stored).await.unwrap();
    let confirmed = link(&harness, &session, "battlenet", &id).await;
    assert!(confirmed.shown);
    assert!(confirmed.expires_at.unwrap() > super::connections_oauth::now_ms() + 29 * 86_400_000);

    // 30 days later: hidden at once, and gone after the upkeep
    let mut stored = harness.db.fetch_linked_account(&owner.id, "battlenet").await.unwrap().unwrap();
    stored.expires_at = Some(super::connections_oauth::now_ms() - 1);
    harness.db.save_linked_account(&stored).await.unwrap();
    assert!(profile(&harness, &viewer_session, &owner.id).await.connections.is_empty());
    crate::util::linked_upkeep::run_once(&harness.db, super::connections_oauth::now_ms())
        .await
        .unwrap();
    assert!(harness.db.fetch_linked_account(&owner.id, "battlenet").await.unwrap().is_none());

    // settings delete an expired copy on sight too, before the upkeep runs
    link(&harness, &session, "battlenet", &id).await;
    let mut stored = harness.db.fetch_linked_account(&owner.id, "battlenet").await.unwrap().unwrap();
    stored.expires_at = Some(super::connections_oauth::now_ms() - 1);
    harness.db.save_linked_account(&stored).await.unwrap();
    assert!(list(&harness, &session).await.battlenet.account.is_none());
    assert!(harness.db.fetch_linked_account(&owner.id, "battlenet").await.unwrap().is_none());
}

#[rocket::async_test]
async fn bots_never_get_linked_accounts() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    link(&harness, &session, "github", &fresh_id()).await;
    edit(&harness, &session, "github", serde_json::json!({ "shown": true })).await;

    let (bot, _) = harness.new_bot(&owner).await;
    let response = harness
        .client
        .get(format!("/users/{}/profile", owner.id))
        .header(Header::new("x-bot-token", bot.token.clone()))
        .dispatch()
        .await;
    let status = response.status();
    let body = response.into_string().await.unwrap_or_default();
    assert!(!body.contains("\"connections\""), "{status} {body}");
    assert!(!body.contains("\"games\""), "{status} {body}");
}

#[rocket::async_test]
async fn github_refresh_asks_who_the_id_is_now() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    // the fake renames ids ending in 7
    let id = format!("{}7", rand::random::<u32>());
    link(&harness, &session, "github", &id).await;
    let mut stored = harness.db.fetch_linked_account(&owner.id, "github").await.unwrap().unwrap();
    stored.refreshed_at -= super::connections_oauth::REFRESH_COOLDOWN_MS + 1;
    harness.db.save_linked_account(&stored).await.unwrap();
    let (status, body) = call(&harness, Method::Post, &session, "/users/@me/connections/github/refresh", None).await;
    assert_eq!(status, Status::Ok, "{body}");
    let refreshed: v0::ServiceAccount = serde_json::from_str(&body).unwrap();
    assert_eq!(refreshed.display_name, format!("renamed-{id}"));
    assert_eq!(refreshed.profile_url, format!("https://github.com/renamed-{id}"));
    assert!(refreshed
        .repos
        .iter()
        .all(|r| r.url.starts_with(&format!("https://github.com/renamed-{id}/"))));

    // an id GitHub no longer has: the link goes
    let (_, gone_session, gone) = person(&harness, 30).await;
    let gone_id = format!("{}404", rand::random::<u16>());
    link(&harness, &gone_session, "github", &gone_id).await;
    let mut stored = harness.db.fetch_linked_account(&gone.id, "github").await.unwrap().unwrap();
    stored.refreshed_at = 0;
    harness.db.save_linked_account(&stored).await.unwrap();
    let (status, _) = call(&harness, Method::Post, &gone_session, "/users/@me/connections/github/refresh", None).await;
    assert_eq!(status, Status::NotFound);
    assert!(harness.db.fetch_linked_account(&gone.id, "github").await.unwrap().is_none());
}

#[rocket::async_test]
async fn twitch_copies_older_than_a_day_are_blanked_when_twitch_is_down() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    // the fake fails for a batch with an id ending in 503
    let id = format!("{}503", rand::random::<u16>());
    link(&harness, &session, "twitch", &id).await;
    let now = super::connections_oauth::now_ms();
    let mut stored = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    stored.refreshed_at = now - super::connections_oauth::TWITCH_FRESH_MS - 1;
    stored.live = Some(true);
    stored.live_title = Some("Old".into());
    stored.live_at = Some(now - super::connections_oauth::TWITCH_FRESH_MS - 1);
    harness.db.save_linked_account(&stored).await.unwrap();
    crate::util::linked_upkeep::run_once(&harness.db, now).await.unwrap();
    let kept = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    assert_eq!(kept.external_id, id);
    assert_eq!(kept.display_name, "");
    assert_eq!(kept.profile_url, "");
    assert_eq!(kept.live_title, None);
    assert_eq!(kept.live, Some(false));
    assert!(test_twitch::called("subscribe revocations"));
}

#[rocket::async_test]
async fn orphan_twitch_subscriptions_are_ended() {
    let harness = TestHarness::new().await;
    let channel = format!("{}1", rand::random::<u32>());
    test_twitch::add_subscription(&format!("orphan-{channel}"), &channel);
    crate::util::linked_upkeep::run_once(&harness.db, super::connections_oauth::now_ms())
        .await
        .unwrap();
    assert!(test_twitch::called(&format!("unsubscribe orphan-{channel}")));
}

#[rocket::async_test]
async fn twitch_names_are_read_again_daily_and_stale_copies_hide() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    let (_, viewer_session, viewer) = person(&harness, 30).await;
    share(&harness, &owner, &viewer).await;
    let id = fresh_id();
    link(&harness, &session, "twitch", &id).await;
    edit(&harness, &session, "twitch", serde_json::json!({ "shown": true })).await;

    // a day old: the profile stops showing it (the job would have run)
    let now = super::connections_oauth::now_ms();
    let mut stored = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    stored.refreshed_at = now - super::connections_oauth::TWITCH_FRESH_MS - 1;
    stored.live = Some(true);
    stored.live_title = Some("Old title".into());
    stored.live_at = Some(now - super::connections_oauth::TWITCH_FRESH_MS - 1);
    harness.db.save_linked_account(&stored).await.unwrap();
    assert!(profile(&harness, &viewer_session, &owner.id).await.connections.is_empty());

    crate::util::linked_upkeep::run_once(&harness.db, now).await.unwrap();
    let seen = profile(&harness, &viewer_session, &owner.id).await.connections;
    assert_eq!(seen[0].display_name, format!("Renamed{id}"));
    assert_eq!(seen[0].profile_url, format!("https://www.twitch.tv/renamed_{id}"));
    // the fake says ids ending in 1 are live
    assert_eq!(seen[0].live, Some(true));
    assert_eq!(seen[0].live_title.as_deref(), Some(format!("Live title {id}").as_str()));

    // an account Twitch no longer returns is unlinked
    let (_, gone_session, gone) = person(&harness, 30).await;
    link(&harness, &gone_session, "twitch", "77404").await;
    let mut stored = harness.db.fetch_linked_account(&gone.id, "twitch").await.unwrap().unwrap();
    stored.refreshed_at = 0;
    harness.db.save_linked_account(&stored).await.unwrap();
    crate::util::linked_upkeep::run_once(&harness.db, now).await.unwrap();
    assert!(harness.db.fetch_linked_account(&gone.id, "twitch").await.unwrap().is_none());
}

#[rocket::async_test]
async fn minors_and_hidden_section_and_bots() {
    let harness = TestHarness::new().await;
    let (_, minor_session, mut minor) = person(&harness, 15).await;
    let (_, friend_session, mut friend) = person(&harness, 15).await;
    let (_, stranger_session, stranger) = person(&harness, 30).await;
    share(&harness, &minor, &stranger).await;
    #[allow(clippy::disallowed_methods)]
    minor
        .apply_relationship(&harness.db, &mut friend, RelationshipStatus::Friend, RelationshipStatus::Friend)
        .await
        .unwrap();
    link(&harness, &minor_session, "github", &fresh_id()).await;
    edit(&harness, &minor_session, "github", serde_json::json!({ "shown": true })).await;

    assert_eq!(profile(&harness, &friend_session, &minor.id).await.connections.len(), 1);
    assert!(profile(&harness, &stranger_session, &minor.id).await.connections.is_empty());

    // the section turned off hides it from everyone else
    let (status, body) = call(
        &harness,
        Method::Patch,
        &minor_session,
        "/users/@me",
        Some(serde_json::json!({ "profile": { "hidden_sections": ["connections"] } })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(profile(&harness, &friend_session, &minor.id).await.connections.is_empty());
}

#[rocket::async_test]
async fn refresh_waits_and_disconnect_deletes_everything() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    link(&harness, &session, "github", &fresh_id()).await;

    let (status, body) = call(&harness, Method::Post, &session, "/users/@me/connections/github/refresh", None).await;
    assert_eq!(status, Status::TooManyRequests, "{body}");
    assert!(body.contains("retry_after"));

    let mut stored = harness.db.fetch_linked_account(&owner.id, "github").await.unwrap().unwrap();
    stored.refreshed_at -= super::connections_oauth::REFRESH_COOLDOWN_MS + 1;
    stored.shown_repos = vec![1, 404];
    harness.db.save_linked_account(&stored).await.unwrap();
    let (status, body) = call(&harness, Method::Post, &session, "/users/@me/connections/github/refresh", None).await;
    assert_eq!(status, Status::Ok, "{body}");
    let refreshed: v0::ServiceAccount = serde_json::from_str(&body).unwrap();
    assert_eq!(refreshed.shown_repos, vec![1]);

    // Battle.net and Twitch have no Refresh
    let (status, _) = call(&harness, Method::Post, &session, "/users/@me/connections/battlenet/refresh", None).await;
    assert_eq!(status, Status::BadRequest);

    let twitch_id = fresh_id();
    link(&harness, &session, "twitch", &twitch_id).await;
    for service in ["github", "twitch"] {
        let (status, _) = call(&harness, Method::Delete, &session, &format!("/users/@me/connections/{service}"), None).await;
        assert_eq!(status, Status::NoContent);
    }
    assert!(harness.db.fetch_linked_accounts(&owner.id).await.unwrap().is_empty());
    assert!(test_twitch::called(&format!("unsubscribe sub-offline-{twitch_id}")));

    // account deletion takes linked accounts too
    link(&harness, &session, "battlenet", &fresh_id()).await;
    harness.db.delete_linked_accounts(&owner.id).await.unwrap();
    assert!(harness.db.fetch_linked_accounts(&owner.id).await.unwrap().is_empty());
}

#[rocket::async_test]
async fn twitch_links_from_before_the_transfer_text_wait_for_agreement() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    let (_, viewer_session, viewer) = person(&harness, 30).await;
    share(&harness, &owner, &viewer).await;
    let id = fresh_id();
    let linked = link(&harness, &session, "twitch", &id).await;
    assert!(linked.consent_current);
    edit(&harness, &session, "twitch", serde_json::json!({ "shown": true })).await;
    assert_eq!(profile(&harness, &viewer_session, &owner.id).await.connections.len(), 1);

    // as stored before the card named the US transfer, 23 hours old: a
    // current link would be read at Twitch in this round
    let now = super::connections_oauth::now_ms();
    let old = now - 23 * 60 * 60 * 1000 - 60_000;
    let mut stored = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    assert_eq!(stored.consent, revolt_database::LINKED_CONSENT_VERSION);
    stored.consent = 0;
    stored.refreshed_at = old;
    stored.live = Some(true);
    stored.live_title = Some("Old".into());
    stored.live_at = Some(old);
    harness.db.save_linked_account(&stored).await.unwrap();

    // hidden from other people, and the owner is asked
    assert!(profile(&harness, &viewer_session, &owner.id).await.connections.is_empty());
    assert!(!list(&harness, &session).await.twitch.account.unwrap().consent_current);

    // the daily read skips it: no new name, no live check
    crate::util::linked_upkeep::run_once(&harness.db, now).await.unwrap();
    let kept = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    assert_eq!(kept.display_name, format!("Streamer{id}"));
    assert_eq!(kept.live_title.as_deref(), Some("Old"));
    assert_eq!(kept.refreshed_at, old);
    // and no live-status messages: the subscriptions at Twitch end
    assert!(kept.subscriptions.is_empty());
    assert!(test_twitch::called(&format!("unsubscribe sub-online-{id}")));
    assert!(test_twitch::called(&format!("unsubscribe sub-offline-{id}")));

    // still nothing asked of Twitch once it is a day old, but nothing kept
    // over 24 hours
    let older = now - super::connections_oauth::TWITCH_FRESH_MS - 1;
    let mut stored = kept;
    stored.refreshed_at = older;
    stored.live_at = Some(older);
    harness.db.save_linked_account(&stored).await.unwrap();
    crate::util::linked_upkeep::run_once(&harness.db, now).await.unwrap();
    let kept = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    assert_eq!(kept.external_id, id);
    assert_eq!(kept.display_name, "");
    assert_eq!(kept.live, Some(false));
    assert_eq!(kept.live_title, None);

    // only the exact version in use counts
    for wrong in [0, 2, 99] {
        let (status, _) = edit(&harness, &session, "twitch", serde_json::json!({ "consent": wrong })).await;
        assert_eq!(status, Status::BadRequest, "consent {wrong}");
    }
    let (status, body) = edit(
        &harness,
        &session,
        "twitch",
        serde_json::json!({ "consent": revolt_database::LINKED_CONSENT_VERSION }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let agreed: v0::ServiceAccount = serde_json::from_str(&body).unwrap();
    assert!(agreed.consent_current);
    // agreeing sets up the live-status subscriptions again
    let stored = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    assert_eq!(
        stored.subscriptions,
        vec![format!("sub-online-{id}"), format!("sub-offline-{id}")]
    );

    // the next round reads it again, and it shows again
    crate::util::linked_upkeep::run_once(&harness.db, super::connections_oauth::now_ms())
        .await
        .unwrap();
    let seen = profile(&harness, &viewer_session, &owner.id).await.connections;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].display_name, format!("Renamed{id}"));
}

#[rocket::async_test]
async fn new_links_store_the_current_consent_and_old_github_links_agree_before_refresh() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    let (_, viewer_session, viewer) = person(&harness, 30).await;
    share(&harness, &owner, &viewer).await;
    let (github_id, bnet_id) = (fresh_id(), fresh_id());
    assert!(link(&harness, &session, "github", &github_id).await.consent_current);
    assert!(link(&harness, &session, "battlenet", &bnet_id).await.consent_current);
    for service in ["github", "battlenet"] {
        let stored = harness.db.fetch_linked_account(&owner.id, service).await.unwrap().unwrap();
        assert_eq!(stored.consent, revolt_database::LINKED_CONSENT_VERSION, "{service}");
    }
    edit(&harness, &session, "github", serde_json::json!({ "shown": true })).await;

    // GitHub linked before the new text
    let mut stored = harness.db.fetch_linked_account(&owner.id, "github").await.unwrap().unwrap();
    stored.consent = 0;
    stored.refreshed_at -= super::connections_oauth::REFRESH_COOLDOWN_MS + 1;
    harness.db.save_linked_account(&stored).await.unwrap();
    assert!(!list(&harness, &session).await.github.account.unwrap().consent_current);
    // the old text had no Art. 9 line: hidden from others until agreed
    assert!(profile(&harness, &viewer_session, &owner.id).await.connections.is_empty());

    // Refresh waits for the new text
    let (status, body) = call(&harness, Method::Post, &session, "/users/@me/connections/github/refresh", None).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    let (status, _) = edit(&harness, &session, "github", serde_json::json!({ "consent": 99 })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = edit(
        &harness,
        &session,
        "github",
        serde_json::json!({ "consent": revolt_database::LINKED_CONSENT_VERSION }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(profile(&harness, &viewer_session, &owner.id).await.connections.len(), 1);
    let (status, body) = call(&harness, Method::Post, &session, "/users/@me/connections/github/refresh", None).await;
    assert_eq!(status, Status::Ok, "{body}");

    // Battle.net: "Confirm on Battle.net" stores the text in use
    let mut stored = harness.db.fetch_linked_account(&owner.id, "battlenet").await.unwrap().unwrap();
    stored.consent = 0;
    harness.db.save_linked_account(&stored).await.unwrap();
    assert!(!list(&harness, &session).await.battlenet.account.unwrap().consent_current);
    assert!(link(&harness, &session, "battlenet", &bnet_id).await.consent_current);
    let stored = harness.db.fetch_linked_account(&owner.id, "battlenet").await.unwrap().unwrap();
    assert_eq!(stored.consent, revolt_database::LINKED_CONSENT_VERSION);
}

#[rocket::async_test]
async fn tabs_get_picture_bio_counts_and_game() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = person(&harness, 30).await;
    let (_, viewer_session, viewer) = person(&harness, 30).await;
    share(&harness, &owner, &viewer).await;

    let twitch_id = fresh_id();
    let twitch = link(&harness, &session, "twitch", &twitch_id).await;
    assert_eq!(
        twitch.avatar_url.as_deref(),
        Some(format!("https://static-cdn.jtvnw.net/jtv_user_pictures/{twitch_id}.png").as_str())
    );
    // control characters are gone
    assert_eq!(twitch.bio.as_deref(), Some("Ranked nights"));
    assert_eq!(twitch.kind.as_deref(), Some("partner"));
    assert_eq!(twitch.game.as_deref(), Some("Rust"));

    let github_id = fresh_id();
    let github = link(&harness, &session, "github", &github_id).await;
    assert_eq!(github.followers, Some(128));
    assert_eq!(github.public_repos, Some(42));
    assert_eq!(github.real_name.as_deref(), Some("Dev Person"));
    assert!(github.avatar_url.unwrap().starts_with("https://avatars.githubusercontent.com/"));

    for service in ["twitch", "github"] {
        edit(&harness, &session, service, serde_json::json!({ "shown": true })).await;
    }
    let seen = profile(&harness, &viewer_session, &owner.id).await.connections;
    assert_eq!(seen[0].kind.as_deref(), Some("partner"));
    assert_eq!(seen[0].game.as_deref(), Some("Rust"));
    assert_eq!(seen[1].followers, Some(128));
    assert!(seen[1].avatar_url.is_some());

    // GitHub Refresh reads the counts again
    let mut stored = harness.db.fetch_linked_account(&owner.id, "github").await.unwrap().unwrap();
    stored.refreshed_at -= super::connections_oauth::REFRESH_COOLDOWN_MS + 1;
    harness.db.save_linked_account(&stored).await.unwrap();
    let (status, body) = call(&harness, Method::Post, &session, "/users/@me/connections/github/refresh", None).await;
    assert_eq!(status, Status::Ok, "{body}");
    let refreshed: v0::ServiceAccount = serde_json::from_str(&body).unwrap();
    assert_eq!(refreshed.followers, Some(130));
    assert_eq!(refreshed.public_repos, Some(43));

    // the daily Twitch check renews picture, bio and kind
    let mut stored = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    stored.refreshed_at = 0;
    harness.db.save_linked_account(&stored).await.unwrap();
    crate::util::linked_upkeep::run_once(&harness.db, super::connections_oauth::now_ms())
        .await
        .unwrap();
    let renewed = harness.db.fetch_linked_account(&owner.id, "twitch").await.unwrap().unwrap();
    assert_eq!(renewed.kind.as_deref(), Some("affiliate"));
    assert_eq!(renewed.bio.as_deref(), Some("New bio"));
    assert!(renewed.avatar_url.unwrap().contains("/new-"));
}
