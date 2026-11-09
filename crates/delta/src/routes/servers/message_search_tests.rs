//! Server-wide message search: only channels you may read, never a leak
//! from the others, and the filters through the API.

use crate::{rocket, util::test::TestHarness};
use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{AccountAge, Channel, Member, Message, PartialChannel, Server, Session, User};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, OverrideField};
use rocket::http::{Header, Status};
use serde_json::{json, Value};

/// A message id for a moment (ms since 1970), with `n` to keep ids apart
fn id_at(ms: u64, n: u128) -> String {
    ulid::Ulid::from_parts(ms, n).to_string()
}

async fn deny(harness: &TestHarness, channel: &mut Channel, permission: ChannelPermission) {
    channel
        .update(
            &harness.db,
            PartialChannel {
                default_permissions: Some(OverrideField {
                    a: 0,
                    d: permission as i64,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

async fn put(harness: &TestHarness, id: &str, channel: &str, author: &str) {
    harness
        .db
        .insert_message(&Message {
            id: id.to_string(),
            channel: channel.to_string(),
            author: author.to_string(),
            content: Some("hello".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
}

async fn search(harness: &TestHarness, path: String, token: &str, body: Value) -> (Status, Value) {
    let response = harness
        .client
        .post(path)
        .header(Header::new("x-session-token", token.to_string()))
        .header(Header::new("content-type", "application/json"))
        .body(body.to_string())
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn ids(body: &Value) -> Vec<String> {
    let messages = body.get("messages").unwrap_or(body);
    messages
        .as_array()
        .unwrap_or_else(|| panic!("no messages in {body}"))
        .iter()
        .map(|m| m["_id"].as_str().unwrap().to_string())
        .collect()
}

#[rocket::async_test]
async fn only_channels_you_may_read() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, session, me) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let open = channels[0].clone();
    let mut hidden = harness.new_channel(&server).await;
    deny(&harness, &mut hidden, ChannelPermission::ViewChannel).await;
    // new_channel adds to the list it was given: give it the current one
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    let mut no_history = harness.new_channel(&server).await;
    deny(&harness, &mut no_history, ChannelPermission::ReadMessageHistory).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    assert!(server.channels.contains(&hidden.id().to_string()));
    Member::create(&harness.db, &server, &me, None).await.unwrap();

    let now = 1_800_000_000_000;
    let (a, b, c) = (id_at(now, 1), id_at(now, 2), id_at(now, 3));
    put(&harness, &a, open.id(), &owner.id).await;
    put(&harness, &b, hidden.id(), &owner.id).await;
    put(&harness, &c, no_history.id(), &owner.id).await;
    // a message of another server never shows here
    let (_, other_channels) = harness.new_server(&owner).await;
    put(&harness, &id_at(now, 4), other_channels[0].id(), &owner.id).await;
    let path = format!("/servers/{}/messages/search", server.id);

    let (status, body) = search(&harness, path.clone(), &session.token, json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(ids(&body), vec![a.clone()]);

    // asking for the hidden channels by id changes nothing
    let (status, body) = search(
        &harness,
        path.clone(),
        &session.token,
        json!({ "channels": [hidden.id(), no_history.id(), other_channels[0].id()] }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(ids(&body).is_empty(), "{body}");

    // with users: still the same message only
    let (_, body) = search(
        &harness,
        path,
        &session.token,
        json!({ "author": owner.id, "include_users": true }),
    )
    .await;
    assert_eq!(ids(&body), vec![a]);
}

#[rocket::async_test]
async fn not_a_member_finds_no_server() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, session, _) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    put(&harness, &id_at(1_800_000_000_000, 1), channels[0].id(), &owner.id).await;
    let (status, _) = search(
        &harness,
        format!("/servers/{}/messages/search", server.id),
        &session.token,
        json!({}),
    )
    .await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn a_bot_lists_only_pins_server_wide() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    let (bot, _) = harness.new_bot(&owner).await;
    let response = harness
        .client
        .post(format!("/servers/{}/messages/search", server.id))
        .header(Header::new("x-bot-token", bot.token.clone()))
        .header(Header::new("content-type", "application/json"))
        .body(json!({ "author": owner.id }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
    assert!(response.into_string().await.unwrap().contains("IsBot"));
}

/// from, mentions, has and a day, through the channel route
#[rocket::async_test]
async fn filters_through_the_api() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (_, _, friend) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &friend, None).await.unwrap();
    let channel = channels[0].id().to_string();

    let day = 1_800_000_000_000_u64;
    let hour = 3_600_000;
    let yesterday = id_at(day - 24 * hour, 1);
    let today = id_at(day, 2);
    let today_link = id_at(day + hour, 3);
    put(&harness, &yesterday, &channel, &friend.id).await;
    put(&harness, &today, &channel, &friend.id).await;
    harness
        .db
        .insert_message(&Message {
            id: today_link.clone(),
            channel: channel.clone(),
            author: owner.id.clone(),
            content: Some("look http://zrzur.com".to_string()),
            mentions: Some(vec![friend.id.clone()]),
            ..Default::default()
        })
        .await
        .unwrap();

    let path = format!("/channels/{channel}/search");
    let at = |ms: u64| Timestamp::UNIX_EPOCH + Duration::milliseconds(ms as i64);
    let during = json!({
        "after_date": at(day - hour),
        "before_date": at(day + 2 * hour),
        "sort": "Oldest",
    });

    let (status, body) = search(&harness, path.clone(), &session.token, during.clone()).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(ids(&body), vec![today.clone(), today_link.clone()]);

    let mut from = during.clone();
    from["authors"] = json!([friend.id]);
    let (_, body) = search(&harness, path.clone(), &session.token, from).await;
    assert_eq!(ids(&body), vec![today.clone()]);

    let (_, body) = search(
        &harness,
        path.clone(),
        &session.token,
        json!({ "mentions": friend.id, "has": ["link"] }),
    )
    .await;
    assert_eq!(ids(&body), vec![today_link]);

    // a bad id in a list is refused
    let (status, _) = search(&harness, path, &session.token, json!({ "authors": ["x"] })).await;
    assert_eq!(status, Status::BadRequest);
}

/// Someone who turned `years` this month
async fn person(harness: &TestHarness, years: i32) -> (Session, User) {
    let (mut account, session, user) = harness.new_user().await;
    let now = Timestamp::now_utc();
    account.age = Some(AccountAge::declare((now.year() - years, u8::from(now.month()))));
    account.save(&harness.db).await.unwrap();
    (session, user)
}

/// Mature channels, and every channel of a mature server, stay out for
/// someone who may not open mature content; an adult finds them
#[rocket::async_test]
async fn mature_content_is_left_out_for_minors() {
    let harness = TestHarness::new().await;
    let (owner_session, owner) = person(&harness, 30).await;
    let (teen_session, teen) = person(&harness, 16).await;
    let (server, channels) = harness.new_server(&owner).await;
    let open = channels[0].clone();
    let mut mature = harness.new_channel(&server).await;
    mature
        .update(
            &harness.db,
            PartialChannel {
                nsfw: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    Member::create(&harness.db, &server, &owner, None).await.unwrap();
    Member::create(&harness.db, &server, &teen, None).await.unwrap();

    let now = 1_800_000_000_000;
    let (a, b) = (id_at(now, 1), id_at(now, 2));
    put(&harness, &a, open.id(), &owner.id).await;
    put(&harness, &b, mature.id(), &owner.id).await;
    let path = format!("/servers/{}/messages/search", server.id);

    let (status, body) = search(&harness, path.clone(), &teen_session.token, json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(ids(&body), vec![a.clone()]);
    let (_, body) = search(&harness, path, &owner_session.token, json!({})).await;
    assert_eq!(ids(&body), vec![b, a]);

    // a mature server: nothing at all for the teen, and no error either
    let (mature_server, mature_channels) = Server::create(
        &harness.db,
        v0::DataCreateServer {
            name: "Mature".to_string(),
            nsfw: Some(true),
            ..Default::default()
        },
        &owner,
        true,
    )
    .await
    .unwrap();
    Member::create(&harness.db, &mature_server, &teen, None).await.unwrap();
    put(&harness, &id_at(now, 3), mature_channels[0].id(), &owner.id).await;
    let (status, body) = search(
        &harness,
        format!("/servers/{}/messages/search", mature_server.id),
        &teen_session.token,
        json!({}),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(ids(&body).is_empty(), "{body}");
}

/// The channel route searches its own channel only, whatever `channels` says
#[rocket::async_test]
async fn a_channel_search_stays_in_its_channel() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let other = harness.new_channel(&server).await;
    let now = 1_800_000_000_000;
    let (a, b) = (id_at(now, 1), id_at(now, 2));
    put(&harness, &a, channels[0].id(), &owner.id).await;
    put(&harness, &b, other.id(), &owner.id).await;

    let (status, body) = search(
        &harness,
        format!("/channels/{}/search", channels[0].id()),
        &session.token,
        json!({ "channels": [other.id()] }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(ids(&body), vec![a]);
}

/// Ids in lower case find the same messages
#[rocket::async_test]
async fn ids_in_lower_case_work() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &owner, None).await.unwrap();
    let now = 1_800_000_000_000;
    let a = id_at(now, 1);
    put(&harness, &a, channels[0].id(), &owner.id).await;
    let (status, body) = search(
        &harness,
        format!("/servers/{}/messages/search", server.id),
        &session.token,
        json!({
            "authors": [owner.id.to_lowercase()],
            "channels": [channels[0].id().to_lowercase()],
        }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(ids(&body), vec![a]);
}

/// One search bucket per person, shared by every server
#[rocket::async_test]
async fn server_searches_share_one_bucket() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (first, _) = harness.new_server(&owner).await;
    let (second, _) = harness.new_server(&owner).await;
    Member::create(&harness.db, &first, &owner, None).await.unwrap();
    Member::create(&harness.db, &second, &owner, None).await.unwrap();
    let mut buckets = vec![];
    for server in [&first, &second] {
        let response = harness
            .client
            .post(format!("/servers/{}/messages/search", server.id))
            .header(Header::new("x-session-token", session.token.clone()))
            .header(Header::new("content-type", "application/json"))
            .body("{}")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        assert_eq!(response.headers().get_one("x-ratelimit-limit"), Some("5"));
        buckets.push(response.headers().get_one("x-ratelimit-bucket").map(str::to_string));
    }
    assert!(buckets[0].is_some());
    assert_eq!(buckets[0], buckets[1]);
}
