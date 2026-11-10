//! Server insights: joins by source and leaves (kick, ban, deleted account)
//! as they happen, the hourly message roll-up (once per hour however often
//! it runs, bots left out, no user id in MongoDB), voice time, closing days,
//! retention when the roll-up fails, the race of two first writes, who may
//! read the page and what it shows (hidden channels never, small numbers as
//! null). Runs on REFERENCE and MONGODB (TEST_DB).

use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::voice::{create_voice_state, delete_voice_state, UserVoiceChannel};
use revolt_database::{
    authors_key, hour_of, last_rollable_hour, Channel, Database, InsightCounter, InsightHour,
    Invite, Member, Message, PartialMember, RemovalIntention, Server, ServerInsightDay, Session,
    SystemMessage, User,
};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, OverrideField};
use revolt_result::create_error;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};
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

async fn text_channel(harness: &TestHarness, server: &Server, name: &str, private: bool, voice: bool) -> Channel {
    Channel::create_server_channel(
        &harness.db,
        &mut harness.db.fetch_server(&server.id).await.unwrap(),
        v0::DataCreateServerChannel {
            channel_type: if voice {
                v0::LegacyServerChannelType::Voice
            } else {
                v0::LegacyServerChannelType::Text
            },
            name: name.to_string(),
            description: None,
            nsfw: Some(false),
            voice: None,
            announcement: None,
            category: None,
            private: Some(private),
        },
        true,
    )
    .await
    .expect("channel")
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
    let second = text_channel(&harness, &server, "quiet", false, false).await;
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

fn rand_part() -> u128 {
    Ulid::new().random()
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

async fn today_row(db: &Database, server: &str) -> ServerInsightDay {
    let day = hour_of(Timestamp::now_utc()).div_euclid(24);
    db.fetch_server_insights(server, day, day)
        .await
        .unwrap()
        .pop()
        .unwrap_or_default()
}

fn total(map: &std::collections::BTreeMap<String, u32>) -> u32 {
    map.values().sum()
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
    assert_eq!(page["totals"]["joins"], 3);
    assert_eq!(page["join_sources"]["invite"], 1);
    assert_eq!(page["join_sources"]["other"], 2);
    assert_eq!(page["join_sources"]["discover"], 0);
    // the member count is read again after joins and leaves
    assert_eq!(today["members"], 2);
    // no message rolled up yet
    assert_eq!(today["messages"], 0);
    assert_eq!(today["active_members"], 0);
    assert_eq!(page["top_channels"].as_array().unwrap().len(), 0);
}

#[rocket::async_test]
async fn kicks_bans_and_deleted_accounts_count_as_leaves() {
    let s = setup().await;
    let (_, _, kicked) = s.harness.new_user().await;
    let (_, _, banned) = s.harness.new_user().await;
    let (_, _, gone) = s.harness.new_user().await;
    for user in [&kicked, &banned, &gone] {
        Member::create(&s.harness.db, &s.server, user, None).await.unwrap();
    }
    let base = total(&today_row(&s.harness.db, &s.server.id).await.leaves);

    let kick = s
        .harness
        .client
        .delete(format!("/servers/{}/members/{}", s.server.id, kicked.id))
        .header(Header::new("x-session-token", s.owner_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(kick.status(), Status::NoContent);
    let ban = s
        .harness
        .client
        .put(format!("/servers/{}/bans/{}", s.server.id, banned.id))
        .header(Header::new("x-session-token", s.owner_session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({}).to_string())
        .dispatch()
        .await;
    assert_eq!(ban.status(), Status::Ok);
    let mut gone = gone;
    gone.delete(&s.harness.db).await.unwrap();

    let row = today_row(&s.harness.db, &s.server.id).await;
    assert_eq!(total(&row.leaves), base + 3);
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
async fn the_page_has_its_own_rate_limit() {
    let s = setup().await;
    let response = s
        .harness
        .client
        .get(format!("/servers/{}/insights", s.server.id))
        .header(Header::new("x-session-token", s.owner_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    assert_eq!(response.headers().get_one("x-ratelimit-limit"), Some("10"));
    let bucket = response.headers().get_one("x-ratelimit-bucket").map(str::to_string);
    // server actions keep their own bucket
    let other = s
        .harness
        .client
        .get(format!("/servers/{}", s.server.id))
        .header(Header::new("x-session-token", s.owner_session.token.clone()))
        .dispatch()
        .await;
    assert_ne!(other.headers().get_one("x-ratelimit-bucket").map(str::to_string), bucket);
}

#[rocket::async_test]
async fn an_hour_of_messages_is_rolled_up_once() {
    let s = setup().await;
    // well before the hours the crond tests roll up (tests share the database)
    let hour = hour_of(Timestamp::now_utc()) - 200;
    let start = (hour as u64) * 3_600_000;
    for i in 0..6 {
        message_at(&s, &s.text, if i % 2 == 0 { &s.owner } else { &s.member }, start + 1_000 + i).await;
    }
    for i in 0..2 {
        message_at(&s, &s.second, &s.member, start + 2_000 + i).await;
    }
    // a bot's message counts as a message, the bot not as an active member
    let (_, bot_user) = s.harness.new_bot(&s.owner).await;
    Member::create(&s.harness.db, &s.server, &bot_user, None).await.unwrap();
    message_at(&s, &s.text, &bot_user, start + 2_500).await;
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

    assert_eq!(ServerInsightDay::roll_hour(&s.harness.db, hour).await.unwrap(), (1, 0));
    // a second run of the same hour adds nothing
    assert_eq!(ServerInsightDay::roll_hour(&s.harness.db, hour).await.unwrap(), (0, 0));

    let day = hour.div_euclid(24);
    let rows = s.harness.db.fetch_server_insights(&s.server.id, day, day).await.unwrap();
    assert_eq!(rows.len(), 1);
    let hour_key = hour.rem_euclid(24).to_string();
    let counted = &rows[0].messages[&hour_key];
    assert_eq!(counted.get(s.text.id()), Some(&7));
    assert_eq!(counted.get(s.second.id()), Some(&2));
    assert_eq!(rows[0].active, 2, "owner and member, not the bot");
    assert!(rows[0].open);

    // no user id is written to MongoDB, only the number
    if let Database::MongoDb(mongo) = &s.harness.db {
        let raw = mongo
            .db()
            .collection::<revolt_database::mongodb::bson::Document>("server_insights")
            .find_one(revolt_database::mongodb::bson::doc! { "_id": ServerInsightDay::key(&s.server.id, day) })
            .await
            .unwrap()
            .unwrap();
        let text = raw.to_string();
        for id in [&s.owner.id, &s.member.id, &bot_user.id] {
            assert!(!text.contains(id.as_str()), "{text}");
        }
        assert!(raw.get("authors").is_none());
    }

    // the page in UTC: the busiest channel first, 2 messages hidden as "fewer than 5"
    let (status, page) = get(
        &s.harness,
        &format!("/servers/{}/insights?days=30&time_zone=UTC", s.server.id),
        &s.owner_session,
    )
    .await;
    assert_eq!(status, Status::Ok, "{page}");
    assert_eq!(page["totals"]["messages"], 9);
    let top = page["top_channels"].as_array().unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0]["channel"], s.text.id());
    assert_eq!(top[0]["messages"], 7);
    assert_eq!(top[1]["channel"], s.second.id());
    assert_eq!(top[1]["messages"], Value::Null);
    // two active members that day: fewer than 5
    let busy = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["messages"] == 9)
        .unwrap();
    assert_eq!(busy["active_members"], Value::Null);
    // nothing names a person
    let text = page.to_string();
    assert!(!text.contains(&s.member.id) && !text.contains(&s.owner.id));
}

#[rocket::async_test]
async fn channels_the_viewer_can_not_see_never_show_up() {
    let s = setup().await;
    let secret = text_channel(&s.harness, &s.server, "secret", true, false).await;
    let hour = hour_of(Timestamp::now_utc()) - 210;
    let start = (hour as u64) * 3_600_000;
    for i in 0..6 {
        message_at(&s, &s.text, &s.owner, start + 10 + i).await;
    }
    for i in 0..20 {
        message_at(&s, &secret, &s.owner, start + 100 + i).await;
    }
    ServerInsightDay::roll_hour(&s.harness.db, hour).await.unwrap();

    // the member manages the server but may not see the private channel
    let server = s.harness.db.fetch_server(&s.server.id).await.unwrap();
    let role = s
        .harness
        .new_role(
            &server,
            1,
            Some(OverrideField {
                a: ChannelPermission::ManageServer as i64,
                d: 0,
            }),
        )
        .await;
    let mut member = s.harness.db.fetch_member(&s.server.id, &s.member.id).await.unwrap();
    member
        .update(
            &s.harness.db,
            PartialMember {
                roles: Some(vec![role.id.clone()]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let path = format!("/servers/{}/insights?days=30&time_zone=UTC", s.server.id);
    let (status, mine) = get(&s.harness, &path, &s.member_session).await;
    assert_eq!(status, Status::Ok, "{mine}");
    assert_eq!(mine["totals"]["messages"], 6);
    assert!(!mine.to_string().contains(secret.id()));
    // the owner sees both
    let (_, theirs) = get(&s.harness, &path, &s.owner_session).await;
    assert_eq!(theirs["totals"]["messages"], 26);
}

#[rocket::async_test]
async fn voice_time_is_counted_once_when_a_seat_is_left() {
    let s = setup().await;
    let voice = text_channel(&s.harness, &s.server, "talk", false, true).await;
    let seat = UserVoiceChannel {
        id: voice.id().to_string(),
        server_id: Some(s.server.id.clone()),
    };
    create_voice_state(&seat, &s.member.id, Timestamp::now_utc() - Duration::minutes(10))
        .await
        .unwrap();
    delete_voice_state(&seat, &s.member.id).await.unwrap();
    // a second leave of the same seat (a late webhook) adds nothing
    delete_voice_state(&seat, &s.member.id).await.unwrap();
    ServerInsightDay::flush_voice(&s.harness.db).await.unwrap();

    let seconds = total(&today_row(&s.harness.db, &s.server.id).await.voice);
    assert!((599..=601).contains(&seconds), "{seconds}");
    let (_, page) = get(
        &s.harness,
        &format!("/servers/{}/insights?days=7", s.server.id),
        &s.owner_session,
    )
    .await;
    assert_eq!(page["totals"]["voice_minutes"], 10);
}

/// Retention and closing run when the roll-up fails (part of the pass
/// test: both move crond's one cursor)
async fn closing_and_retention_run_when_the_roll_up_fails(s: &Setup) {
    let now = Timestamp::now_utc();
    let current = last_rollable_hour(now);
    // an open day from last week and a row past the retention
    let open_day = current.div_euclid(24) - 7;
    s.harness
        .db
        .add_server_insight_hour(
            &s.server.id,
            open_day * 24 + 3,
            &InsightHour {
                active: Some(9),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.harness
        .db
        .add_server_insight(&s.server.id, current - 24 * 100, &InsightCounter::Leave)
        .await
        .unwrap();
    s.harness.db.set_server_insight_cursor(current - 5, None).await.unwrap();

    let report = ServerInsightDay::run_pass_with(&s.harness.db, now, |_| async {
        Err(create_error!(InternalError))
    })
    .await;
    assert!(report.failures >= 1);
    assert_eq!(report.hours, 0);
    assert!(report.deleted >= 1, "{report:?}");
    assert!(report.closed >= 1, "{report:?}");
    // the failed hour is tried again next time
    assert_eq!(s.harness.db.fetch_server_insight_cursor().await.unwrap(), Some(current - 5));
    let rows = s
        .harness
        .db
        .fetch_server_insights(&s.server.id, open_day, open_day)
        .await
        .unwrap();
    assert!(!rows[0].open);
    assert_eq!(rows[0].active, 9);
}

#[rocket::async_test]
async fn a_pass_rolls_up_closes_days_and_deletes_old_rows() {
    let s = setup().await;
    closing_and_retention_run_when_the_roll_up_fails(&s).await;
    let now = Timestamp::now_utc();
    let current = last_rollable_hour(now);
    // a message two days ago
    message_at(&s, &s.text, &s.member, ((current - 48) as u64) * 3_600_000 + 10).await;
    // a row past the retention
    s.harness
        .db
        .add_server_insight(&s.server.id, current - 24 * 100, &InsightCounter::Leave)
        .await
        .unwrap();
    // crond had rolled up to three days ago
    s.harness.db.set_server_insight_cursor(current - 72, None).await.unwrap();

    let report = ServerInsightDay::run_pass(&s.harness.db, now).await;
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

    // the next pass catches up to the last finished hour, then closes
    let report = ServerInsightDay::run_pass(&s.harness.db, now).await;
    assert_eq!(report.hours, 24);
    assert_eq!(s.harness.db.fetch_server_insight_cursor().await.unwrap(), Some(current));
    ServerInsightDay::run_pass(&s.harness.db, now).await;
    let day = (current - 48).div_euclid(24);
    let rows = s.harness.db.fetch_server_insights(&s.server.id, day, day).await.unwrap();
    assert_eq!(rows[0].messages.values().map(total).sum::<u32>(), 1);
    // the day is closed: its count stays, the Redis key is gone
    assert_eq!(rows[0].active, 1);
    assert!(!rows[0].open);
    let exists: bool = redis_kiss::redis::cmd("EXISTS")
        .arg(authors_key(&s.server.id, day))
        .query_async(&mut redis_kiss::get_connection().await.unwrap().into_inner())
        .await
        .unwrap();
    assert!(!exists);

    // nothing left to do
    let report = ServerInsightDay::run_pass(&s.harness.db, now).await;
    assert_eq!(report.hours, 0);
}

#[rocket::async_test]
async fn two_first_writes_of_a_day_both_count() {
    let s = setup().await;
    let hour = hour_of(Timestamp::now_utc()) - 300;
    let db = &s.harness.db;
    for n in 0..20 {
        let server = format!("{}-race-{n}", s.server.id);
        let counts = InsightHour {
            channels: [(s.text.id().to_string(), 3)].into_iter().collect(),
            active: Some(1),
        };
        let (join, rolled) = rocket::tokio::join!(
            db.add_server_insight(&server, hour, &InsightCounter::Join(revolt_database::InsightJoinSource::Invite)),
            db.add_server_insight_hour(&server, hour, &counts),
        );
        join.unwrap();
        assert!(rolled.unwrap(), "the hour was not counted ({n})");
        let day = hour.div_euclid(24);
        let row = db.fetch_server_insights(&server, day, day).await.unwrap().pop().unwrap();
        assert_eq!(total(&row.joins), 1);
        assert_eq!(row.messages.values().map(total).sum::<u32>(), 3);
        // and the hour still counts once
        assert!(!db.add_server_insight_hour(&server, hour, &counts).await.unwrap());
        db.delete_server_insights_of(&server).await.unwrap();
    }
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
    let hour = hour_of(Timestamp::now_utc()) - 220;
    message_at(&s, &s.text, &s.owner, (hour as u64) * 3_600_000 + 1).await;
    s.server.clone().delete(&s.harness.db).await.unwrap();
    assert!(s
        .harness
        .db
        .fetch_server_insights(&s.server.id, day - 1, day + 1)
        .await
        .unwrap()
        .is_empty());
    // a message left behind is not counted for a server that is gone
    assert_eq!(ServerInsightDay::roll_hour(&s.harness.db, hour).await.unwrap(), (0, 0));
}
