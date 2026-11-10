//! Server insights: joins by source and leaves as they happen, the hourly
//! message roll-up (once per hour however often it runs), closing days,
//! retention, who may read the page and what it shows. Runs on REFERENCE and
//! MONGODB (TEST_DB).

use iso8601_timestamp::Timestamp;
use revolt_database::{
    hour_of, Channel, InsightCounter, Invite, Member, Message, RemovalIntention, Server,
    ServerInsightDay, Session, SystemMessage, User,
};
use revolt_models::v0;
use rocket::http::{Header, Status};
use serde_json::Value;
use ulid::Ulid;

use crate::util::test::TestHarness;

struct Setup {
    harness: TestHarness,
    owner: User,
    owner_session: Session,
    member: User,
    member_session: Session,
    server: Server,
    text: Channel,
    second: Channel,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &owner, None)
        .await
        .expect("owner");
    Member::create(&harness.db, &server, &member, None)
        .await
        .expect("member");
    let second = Channel::create_server_channel(
        &harness.db,
        &mut harness.db.fetch_server(&server.id).await.unwrap(),
        v0::DataCreateServerChannel {
            channel_type: v0::LegacyServerChannelType::Text,
            name: "quiet".to_string(),
            description: None,
            nsfw: Some(false),
            voice: None,
            announcement: None,
            category: None,
            private: Some(false),
        },
        true,
    )
    .await
    .expect("channel");
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    Setup {
        harness,
        owner,
        owner_session,
        member,
        member_session,
        text: channels[0].clone(),
        second,
        server,
    }
}

async fn get(harness: &TestHarness, path: &str, session: &Session) -> (Status, Value) {
    let response = harness
        .client
        .get(path.to_string())
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn last_row(page: &Value) -> &Value {
    page["rows"].as_array().unwrap().last().unwrap()
}

/// A message sent at a given time (ids carry their time)
async fn message_at(s: &Setup, channel: &Channel, author: &User, at_ms: u64) -> Message {
    let message = Message {
        id: Ulid::from_parts(at_ms, rand_part()).to_string(),
        channel: channel.id().to_string(),
        author: author.id.clone(),
        content: Some("hello".to_string()),
        ..Default::default()
    };
    s.harness.db.insert_message(&message).await.expect("message");
    message
}

fn rand_part() -> u128 {
    Ulid::new().random()
}

#[rocket::async_test]
async fn joins_count_by_where_they_came_from_and_leaves_too() {
    let s = setup().await;
    // an invite of the server
    let invite = Invite::create_channel_invite(&s.harness.db, &s.owner, &s.text, None, None)
        .await
        .expect("invite");
    let (_, joiner_session, joiner) = s.harness.new_user().await;
    let response = s
        .harness
        .client
        .post(format!("/invites/{}", invite.code()))
        .header(Header::new("x-session-token", joiner_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);

    // the joiner leaves again
    let member = s.harness.db.fetch_member(&s.server.id, &joiner.id).await.unwrap();
    member
        .remove(&s.harness.db, &s.server, RemovalIntention::Leave, true)
        .await
        .unwrap();

    let (status, page) = get(
        &s.harness,
        &format!("/servers/{}/insights?days=7", s.server.id),
        &s.owner_session,
    )
    .await;
    assert_eq!(status, Status::Ok, "{page}");
    assert_eq!(page["days"], 7);
    assert_eq!(page["rows"].as_array().unwrap().len(), 7);
    let today = last_row(&page);
    // owner and member (made directly: "other") and the invite join
    assert_eq!(today["joins"], 3);
    assert_eq!(today["leaves"], 1);
    // the cached member count follows joins and leaves (owner and member)
    assert_eq!(today["members"], 2);
    assert_eq!(page["join_sources"]["invite"], 1);
    assert_eq!(page["join_sources"]["other"], 2);
    assert_eq!(page["join_sources"]["discover"], 0);
    // no message rolled up yet
    assert_eq!(today["messages"], 0);
    assert_eq!(page["top_channels"].as_array().unwrap().len(), 0);
}

#[rocket::async_test]
async fn only_managers_read_the_page() {
    let s = setup().await;
    let path = format!("/servers/{}/insights", s.server.id);

    let (status, error) = get(&s.harness, &path, &s.member_session).await;
    assert_eq!(status, Status::Forbidden, "{error}");
    assert_eq!(error["type"], "MissingPermission");
    assert_eq!(error["permission"], "ManageServer");

    let (_, outsider_session, _) = s.harness.new_user().await;
    let (status, _) = get(&s.harness, &path, &outsider_session).await;
    assert_eq!(status, Status::NotFound);

    // the owner, with the default range of 30 days
    let (status, page) = get(&s.harness, &path, &s.owner_session).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(page["rows"].as_array().unwrap().len(), 30);

    // other ranges are refused
    let (status, _) = get(&s.harness, &format!("{path}?days=14"), &s.owner_session).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = get(&s.harness, &format!("{path}?utc_offset=9999"), &s.owner_session).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn an_hour_of_messages_is_rolled_up_once() {
    let s = setup().await;
    // well before the hours the crond test rolls up (tests share the database)
    let hour = hour_of(Timestamp::now_utc()) - 200;
    let start = (hour as u64) * 3_600_000;
    for i in 0..6 {
        message_at(&s, &s.text, if i % 2 == 0 { &s.owner } else { &s.member }, start + 1_000 + i).await;
    }
    for i in 0..2 {
        message_at(&s, &s.second, &s.member, start + 2_000 + i).await;
    }
    // not counted: a system message, and a message of the next hour
    let system = Message {
        id: Ulid::from_parts(start + 3_000, rand_part()).to_string(),
        channel: s.text.id().to_string(),
        author: "00000000000000000000000000".to_string(),
        system: Some(SystemMessage::Text {
            content: "pinned".to_string(),
        }),
        ..Default::default()
    };
    s.harness.db.insert_message(&system).await.unwrap();
    message_at(&s, &s.text, &s.owner, start + 3_600_000 + 5).await;

    assert_eq!(ServerInsightDay::roll_hour(&s.harness.db, hour).await.unwrap(), 1);
    // a second run of the same hour adds nothing
    assert_eq!(ServerInsightDay::roll_hour(&s.harness.db, hour).await.unwrap(), 0);

    let day = hour.div_euclid(24);
    let rows = s
        .harness
        .db
        .fetch_server_insights(&s.server.id, day, day)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].messages.values().sum::<u32>(), 8);
    assert_eq!(rows[0].active, 2);
    assert_eq!(rows[0].channels.get(s.text.id()), Some(&6));
    assert_eq!(rows[0].channels.get(s.second.id()), Some(&2));

    // the page in UTC: the busiest channel first, 2 messages hidden as "fewer than 5"
    let (status, page) = get(
        &s.harness,
        &format!("/servers/{}/insights?days=30&time_zone=UTC", s.server.id),
        &s.owner_session,
    )
    .await;
    assert_eq!(status, Status::Ok, "{page}");
    let total: u64 = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["messages"].as_u64().unwrap())
        .sum();
    assert_eq!(total, 8);
    let top = page["top_channels"].as_array().unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0]["channel"], s.text.id());
    assert_eq!(top[0]["messages"], 6);
    assert_eq!(top[1]["channel"], s.second.id());
    assert_eq!(top[1]["messages"], Value::Null);
    // nothing names a person
    let text = page.to_string();
    assert!(!text.contains(&s.member.id) && !text.contains(&s.owner.id));
}

#[rocket::async_test]
async fn a_pass_rolls_up_closes_days_and_deletes_old_rows() {
    let s = setup().await;
    let now = Timestamp::now_utc();
    let current = hour_of(now);
    // a message two days ago
    message_at(&s, &s.text, &s.member, ((current - 48) as u64) * 3_600_000 + 10).await;
    // a row past the retention
    s.harness
        .db
        .add_server_insight(&s.server.id, current - 24 * 100, &InsightCounter::Leave)
        .await
        .unwrap();
    // crond had rolled up to three days ago
    s.harness.db.set_server_insight_cursor(current - 72).await.unwrap();

    let report = ServerInsightDay::run_pass(&s.harness.db, now).await.unwrap();
    assert_eq!(report.hours, 48, "at most a pass's worth of hours");
    assert!(report.deleted >= 1);
    assert_eq!(
        s.harness.db.fetch_server_insight_cursor().await.unwrap(),
        Some(current - 72 + 48)
    );
    let old_day = (current - 24 * 100).div_euclid(24);
    assert!(s
        .harness
        .db
        .fetch_server_insights(&s.server.id, old_day, old_day)
        .await
        .unwrap()
        .is_empty());

    // the next pass catches up to the last finished hour
    let report = ServerInsightDay::run_pass(&s.harness.db, now).await.unwrap();
    assert_eq!(report.hours, 23);
    assert_eq!(s.harness.db.fetch_server_insight_cursor().await.unwrap(), Some(current - 1));
    let day = (current - 48).div_euclid(24);
    let rows = s
        .harness
        .db
        .fetch_server_insights(&s.server.id, day, day)
        .await
        .unwrap();
    assert_eq!(rows[0].messages.values().sum::<u32>(), 1);
    // the day is closed: its count stays
    assert_eq!(rows[0].active, 1);

    // nothing left to do
    let report = ServerInsightDay::run_pass(&s.harness.db, now).await.unwrap();
    assert_eq!(report.hours, 0);
}

#[rocket::async_test]
async fn a_deleted_server_takes_its_insights_along() {
    let s = setup().await;
    let day = hour_of(Timestamp::now_utc()).div_euclid(24);
    assert!(!s
        .harness
        .db
        .fetch_server_insights(&s.server.id, day - 1, day + 1)
        .await
        .unwrap()
        .is_empty());
    s.server.clone().delete(&s.harness.db).await.unwrap();
    assert!(s
        .harness
        .db
        .fetch_server_insights(&s.server.id, day - 1, day + 1)
        .await
        .unwrap()
        .is_empty());
}
