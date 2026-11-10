//! Scheduled server events: who may do what, who sees what, the interested
//! list, starting (by hand and as crond does it), completing with a repeat,
//! notifications and audit entries. Runs on REFERENCE and MONGODB (TEST_DB).

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::overwrite_config;
use revolt_database::events::client::EventV1;
use revolt_database::{
    AuditLogQuery, Channel, Member, PartialServer, Server, ServerEvent, Session,
    SystemMessageChannels, User,
};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Method, Status};
use serde_json::{json, Value};

use crate::util::test::{PubSubTestHelper, TestHarness};

struct Setup {
    harness: TestHarness,
    owner: User,
    owner_session: Session,
    member: User,
    member_session: Session,
    server: Server,
    text: Channel,
    voice: Channel,
    private: Channel,
}

async fn channel(harness: &TestHarness, server: &Server, voice: bool, private: bool) -> Channel {
    Channel::create_server_channel(
        &harness.db,
        &mut harness.db.fetch_server(&server.id).await.unwrap(),
        v0::DataCreateServerChannel {
            channel_type: if voice {
                v0::LegacyServerChannelType::Voice
            } else {
                v0::LegacyServerChannelType::Text
            },
            name: "place".to_string(),
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

/// A server with events switched on by its owner (they start off)
async fn setup(enabled: bool) -> Setup {
    let s = setup_off(enabled).await;
    if enabled {
        let (status, body) = call(
            &s.harness,
            Method::Patch,
            &format!("/servers/{}", s.server.id),
            &s.owner_session,
            Some(json!({ "events_enabled": true })),
        )
        .await;
        assert_eq!(status, Status::Ok, "{body}");
    }
    let server = s.harness.db.fetch_server(&s.server.id).await.unwrap();
    Setup { server, ..s }
}

/// A new server: events are off until its owner turns them on
async fn setup_off(enabled: bool) -> Setup {
    overwrite_config(move |config| config.features.server_events_enabled = enabled).await;
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
    let text = channels[0].clone();
    let voice = channel(&harness, &server, true, false).await;
    let private = channel(&harness, &server, false, true).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    Setup {
        harness,
        owner,
        owner_session,
        member,
        member_session,
        server,
        text,
        voice,
        private,
    }
}

async fn call(
    harness: &TestHarness,
    method: Method,
    path: &str,
    session: &Session,
    body: Option<Value>,
) -> (Status, Value) {
    let client = &harness.client;
    let request = match method {
        Method::Get => client.get(path.to_string()),
        Method::Post => client.post(path.to_string()),
        Method::Put => client.put(path.to_string()),
        Method::Patch => client.patch(path.to_string()),
        Method::Delete => client.delete(path.to_string()),
        other => panic!("unused method {other}"),
    }
    .header(Header::new("x-session-token", session.token.clone()))
    .header(ContentType::JSON);
    let request = match body {
        Some(body) => request.body(body.to_string()),
        None => request,
    };
    let response = request.dispatch().await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn at(hours: i64) -> Value {
    serde_json::to_value(Timestamp::now_utc() + Duration::hours(hours)).unwrap()
}

fn body(place: Value) -> Value {
    json!({
        "title": "Game night",
        "description": "Bring **snacks**",
        "starts_at": at(2),
        "ends_at": at(4),
        "place": place,
    })
}

async fn create(s: &Setup, place: Value) -> Value {
    let (status, event) = call(
        &s.harness,
        Method::Post,
        &format!("/servers/{}/events", s.server.id),
        &s.owner_session,
        Some(body(place)),
    )
    .await;
    assert_eq!(status, Status::Ok, "{event}");
    event
}

fn voice_place(s: &Setup) -> Value {
    json!({ "type": "Voice", "channel": s.voice.id() })
}

#[rocket::async_test]
async fn switched_off_every_route_says_feature_disabled() {
    let s = setup(false).await;
    let (status, error) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/events", s.server.id),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["type"], "FeatureDisabled");
    let (status, error) = call(
        &s.harness,
        Method::Post,
        &format!("/servers/{}/events", s.server.id),
        &s.owner_session,
        Some(body(json!({ "type": "External", "location": "Park" }))),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["type"], "FeatureDisabled");
}

#[rocket::async_test]
async fn a_manager_creates_and_members_see_it() {
    let s = setup(true).await;
    let mut gateway = PubSubTestHelper::new(s.voice.id()).await;
    let event = create(&s, voice_place(&s)).await;
    assert_eq!(event["status"], "Scheduled");
    assert_eq!(event["interested_count"], 0);
    assert_eq!(event["place"]["type"], "Voice");

    let id = event["_id"].as_str().unwrap().to_string();
    gateway
        .wait_for_event(|e| matches!(e, EventV1::ServerEventCreate(created) if created.id == id))
        .await;

    let (status, list) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/events", s.server.id),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list["events"].as_array().unwrap().len(), 1);
    assert_eq!(list["events"][0]["_id"], id.as_str());
    assert_eq!(list["interested"], json!([]));

    // the audit log names it
    let entries = s
        .harness
        .db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert!(entries.iter().any(|entry| matches!(
        &entry.action,
        revolt_database::AuditLogEntryAction::ServerEventCreate { event, .. } if event == &id
    )));
}

#[rocket::async_test]
async fn people_outside_the_server_find_nothing() {
    let s = setup(true).await;
    let event = create(&s, voice_place(&s)).await;
    let (_, stranger, _) = s.harness.new_user().await;
    for path in [
        format!("/servers/{}/events", s.server.id),
        format!("/servers/{}/events/{}", s.server.id, event["_id"].as_str().unwrap()),
    ] {
        let (status, _) = call(&s.harness, Method::Get, &path, &stranger, None).await;
        assert_eq!(status, Status::NotFound, "{path}");
    }
}

#[rocket::async_test]
async fn members_without_manage_server_cannot_create() {
    let s = setup(true).await;
    let (status, error) = call(
        &s.harness,
        Method::Post,
        &format!("/servers/{}/events", s.server.id),
        &s.member_session,
        Some(body(json!({ "type": "External", "location": "Park" }))),
    )
    .await;
    assert_eq!(status, Status::Forbidden, "{error}");
    assert_eq!(error["type"], "MissingPermission");
}

#[rocket::async_test]
async fn bad_input_is_refused() {
    let s = setup(true).await;
    let path = format!("/servers/{}/events", s.server.id);
    let mut cases = vec![];
    let mut past = body(voice_place(&s));
    past["starts_at"] = at(-2);
    past["ends_at"] = at(1);
    cases.push(past);
    let mut backwards = body(voice_place(&s));
    backwards["ends_at"] = at(1);
    cases.push(backwards);
    let mut long_title = body(voice_place(&s));
    long_title["title"] = json!("x".repeat(101));
    cases.push(long_title);
    let mut long_description = body(voice_place(&s));
    long_description["description"] = json!("x".repeat(1001));
    cases.push(long_description);
    cases.push(body(json!({ "type": "External", "location": "x".repeat(101) })));
    cases.push(body(json!({ "type": "External", "location": "   " })));
    // a text channel is not a voice place
    cases.push(body(json!({ "type": "Voice", "channel": s.text.id() })));
    for case in cases {
        let (status, error) =
            call(&s.harness, Method::Post, &path, &s.owner_session, Some(case.clone())).await;
        assert_eq!(status, Status::BadRequest, "{case} -> {error}");
        assert_eq!(error["type"], "FailedValidation", "{case} -> {error}");
    }
}

#[rocket::async_test]
async fn an_event_in_a_channel_you_cannot_see_is_hidden() {
    let s = setup(true).await;
    let event = create(&s, json!({ "type": "Text", "channel": s.private.id() })).await;
    let id = event["_id"].as_str().unwrap();

    let (status, list) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/events", s.server.id),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list["events"], json!([]));
    for (method, path) in [
        (Method::Get, format!("/servers/{}/events/{id}", s.server.id)),
        (Method::Put, format!("/servers/{}/events/{id}/interested", s.server.id)),
        (Method::Get, format!("/servers/{}/events/{id}/interested", s.server.id)),
    ] {
        let (status, _) = call(&s.harness, method, &path, &s.member_session, None).await;
        assert_eq!(status, Status::NotFound, "{path}");
    }

    // the owner sees it
    let (_, list) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/events", s.server.id),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(list["events"].as_array().unwrap().len(), 1);
}

#[rocket::async_test]
async fn interested_marks_count_once_and_page() {
    let s = setup(true).await;
    let event = create(&s, json!({ "type": "External", "location": "Central park" })).await;
    let id = event["_id"].as_str().unwrap().to_string();
    let mark = format!("/servers/{}/events/{id}/interested", s.server.id);
    let mut gateway = PubSubTestHelper::new(&s.server.id).await;

    let (status, answer) = call(&s.harness, Method::Put, &mark, &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{answer}");
    assert_eq!(answer["interested"], true);
    assert_eq!(answer["event"]["interested_count"], 1);
    gateway
        .wait_for_event(|e| {
            matches!(e, EventV1::ServerEventInterest { id: event, count: 1, interested: true, .. } if event == &id)
        })
        .await;
    // twice changes nothing
    let (_, answer) = call(&s.harness, Method::Put, &mark, &s.member_session, None).await;
    assert_eq!(answer["event"]["interested_count"], 1);

    // three people, pages of two
    let (_, owner_answer) = call(&s.harness, Method::Put, &mark, &s.owner_session, None).await;
    assert_eq!(owner_answer["event"]["interested_count"], 2);
    let (_, third_session, third) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &third, None)
        .await
        .unwrap();
    call(&s.harness, Method::Put, &mark, &third_session, None).await;

    let (status, page) =
        call(&s.harness, Method::Get, &format!("{mark}?limit=2"), &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{page}");
    assert_eq!(page["users"].as_array().unwrap().len(), 2);
    let next = page["next"].as_str().expect("a next page").to_string();
    let (_, rest) = call(
        &s.harness,
        Method::Get,
        &format!("{mark}?limit=2&after={next}"),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(rest["users"].as_array().unwrap().len(), 1);
    assert!(rest.get("next").is_none_or(Value::is_null));
    let mut seen: Vec<String> = page["users"]
        .as_array()
        .unwrap()
        .iter()
        .chain(rest["users"].as_array().unwrap())
        .map(|user| user["_id"].as_str().unwrap().to_string())
        .collect();
    seen.sort();
    let mut expected = vec![s.owner.id.clone(), s.member.id.clone(), third.id.clone()];
    expected.sort();
    assert_eq!(seen, expected);

    // the list shows it as yours
    let (_, list) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/events", s.server.id),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(list["interested"], json!([id.clone()]));

    // unmark
    let (_, answer) = call(&s.harness, Method::Delete, &mark, &s.member_session, None).await;
    assert_eq!(answer["interested"], false);
    assert_eq!(answer["event"]["interested_count"], 2);
}

#[rocket::async_test]
async fn starting_by_hand_announces_it_and_only_managers_may() {
    let s = setup(true).await;
    let system = s.text.id().to_string();
    s.harness
        .db
        .update_server(
            &s.server.id,
            &PartialServer {
                system_messages: Some(SystemMessageChannels {
                    user_joined: Some(system.clone()),
                    user_left: None,
                    user_kicked: None,
                    user_banned: None,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let event = create(&s, voice_place(&s)).await;
    let id = event["_id"].as_str().unwrap().to_string();
    let start = format!("/servers/{}/events/{id}/start", s.server.id);

    let (status, _) = call(&s.harness, Method::Post, &start, &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);

    let mut system_feed = PubSubTestHelper::new(&system).await;
    let (status, started) = call(&s.harness, Method::Post, &start, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{started}");
    assert_eq!(started["status"], "Active");
    let message = system_feed.wait_for_message().await;
    assert!(matches!(
        message.system,
        Some(v0::SystemMessage::EventStarted { id: ref started, .. }) if started == &id
    ));

    // twice: it is not scheduled any more
    let (status, _) = call(&s.harness, Method::Post, &start, &s.owner_session, None).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn a_private_place_is_not_announced_in_the_system_channel() {
    let s = setup(true).await;
    let server = s.harness.db.fetch_server(&s.server.id).await.unwrap();
    assert!(ServerEvent::place_open_to_everyone(&server, Some(&s.text)));
    assert!(ServerEvent::place_open_to_everyone(&server, Some(&s.voice)));
    assert!(!ServerEvent::place_open_to_everyone(&server, Some(&s.private)));
    assert!(ServerEvent::place_open_to_everyone(&server, None));
}

#[rocket::async_test]
async fn crond_starts_due_events_and_completing_a_weekly_one_schedules_the_next() {
    let s = setup(true).await;
    let event = create(&s, voice_place(&s)).await;
    let id = event["_id"].as_str().unwrap().to_string();
    // weekly, and marked by the member
    let (status, edited) = call(
        &s.harness,
        Method::Patch,
        &format!("/servers/{}/events/{id}", s.server.id),
        &s.owner_session,
        Some(json!({ "repeat": "Weekly", "title": "Weekly game night" })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{edited}");
    assert_eq!(edited["repeat"], "Weekly");
    call(
        &s.harness,
        Method::Put,
        &format!("/servers/{}/events/{id}/interested", s.server.id),
        &s.member_session,
        None,
    )
    .await;

    // nothing is due yet
    let now = Timestamp::now_utc();
    assert!(s.harness.db.fetch_server_events_to_start(now).await.unwrap().is_empty());
    // three hours on, it is (crond's own query)
    let later = now + Duration::hours(3);
    let due = s.harness.db.fetch_server_events_to_start(later).await.unwrap();
    assert_eq!(due.len(), 1);
    let mut due = due.into_iter().next().unwrap();
    assert!(due.start(&s.harness.db).await.unwrap());
    // a second crond pass finds nothing to start
    assert!(!due.clone().start(&s.harness.db).await.unwrap());
    let active = s.harness.db.fetch_active_server_events().await.unwrap();
    assert_eq!(active.len(), 1);

    assert!(due.complete(&s.harness.db).await.unwrap());
    let stored = s.harness.db.fetch_server_event(&id).await.unwrap();
    assert_eq!(stored.status, v0::ServerEventStatus::Completed);
    assert!(stored.ended_at.is_some());

    let upcoming = s
        .harness
        .db
        .fetch_upcoming_server_events(&s.server.id)
        .await
        .unwrap();
    assert_eq!(upcoming.len(), 1);
    let next = &upcoming[0];
    assert_eq!(next.previous.as_deref(), Some(id.as_str()));
    assert_eq!(next.title, "Weekly game night");
    assert_eq!(next.starts_at, stored.starts_at + Duration::days(7));
    assert_eq!(next.interested_count, 1);
    assert_eq!(
        s.harness
            .db
            .fetch_server_event_interested(&next.id, None, 10)
            .await
            .unwrap(),
        vec![s.member.id.clone()]
    );
}

#[rocket::async_test]
async fn cancel_edit_and_delete_by_managers_with_audit_entries() {
    let s = setup(true).await;
    let event = create(&s, voice_place(&s)).await;
    let id = event["_id"].as_str().unwrap().to_string();
    let base = format!("/servers/{}/events/{id}", s.server.id);

    // the member can't edit, cancel or delete
    for (method, path, body) in [
        (Method::Patch, base.clone(), Some(json!({ "title": "Mine" }))),
        (Method::Post, format!("{base}/cancel"), None),
        (Method::Delete, base.clone(), None),
    ] {
        let (status, _) = call(&s.harness, method, &path, &s.member_session, body).await;
        assert_eq!(status, Status::Forbidden, "{path}");
    }

    // moving it to a text place, clearing the end
    let (status, edited) = call(
        &s.harness,
        Method::Patch,
        &base,
        &s.owner_session,
        Some(json!({ "place": { "type": "Text", "channel": s.text.id() }, "remove": ["EndsAt"] })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{edited}");
    assert_eq!(edited["place"]["type"], "Text");
    assert!(edited.get("ends_at").is_none());

    let (status, cancelled) =
        call(&s.harness, Method::Post, &format!("{base}/cancel"), &s.owner_session, None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(cancelled["status"], "Cancelled");
    // gone from the list, readable by its link
    let (_, list) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/events", s.server.id),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(list["events"], json!([]));
    let (status, fetched) = call(&s.harness, Method::Get, &base, &s.member_session, None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(fetched["event"]["status"], "Cancelled");
    // a cancelled event can't be marked
    let (status, _) =
        call(&s.harness, Method::Put, &format!("{base}/interested"), &s.member_session, None).await;
    assert_eq!(status, Status::BadRequest);

    let (status, _) = call(&s.harness, Method::Delete, &base, &s.owner_session, None).await;
    assert_eq!(status, Status::NoContent);
    let (status, _) = call(&s.harness, Method::Get, &base, &s.owner_session, None).await;
    assert_eq!(status, Status::NotFound);

    let entries = s
        .harness
        .db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 20,
            },
        )
        .await
        .unwrap();
    let kinds: Vec<&str> = entries
        .iter()
        .filter_map(|entry| match &entry.action {
            revolt_database::AuditLogEntryAction::ServerEventCreate { .. } => Some("create"),
            revolt_database::AuditLogEntryAction::ServerEventEdit { .. } => Some("edit"),
            revolt_database::AuditLogEntryAction::ServerEventCancel { .. } => Some("cancel"),
            revolt_database::AuditLogEntryAction::ServerEventDelete { .. } => Some("delete"),
            _ => None,
        })
        .collect();
    for kind in ["create", "edit", "cancel", "delete"] {
        assert!(kinds.contains(&kind), "{kind} missing in {kinds:?}");
    }
}

#[test]
fn monthly_repeat_keeps_the_day_or_takes_the_last_one() {
    let parse = |text: &str| Timestamp::parse(text).unwrap();
    let next = |text: &str| {
        revolt_database::next_start(parse(text), &v0::ServerEventRepeat::Monthly)
    };
    assert_eq!(next("2026-01-15T18:00:00Z"), parse("2026-02-15T18:00:00Z"));
    assert_eq!(next("2026-01-31T18:00:00Z"), parse("2026-02-28T18:00:00Z"));
    assert_eq!(next("2028-01-31T18:00:00Z"), parse("2028-02-29T18:00:00Z"));
    assert_eq!(next("2026-12-10T09:30:00Z"), parse("2027-01-10T09:30:00Z"));
    assert_eq!(
        revolt_database::next_start(parse("2026-03-01T10:00:00Z"), &v0::ServerEventRepeat::EveryTwoWeeks),
        parse("2026-03-15T10:00:00Z")
    );
}

// ------------------------------------------------- review round (2026-11-10)

const VIEW: i64 = revolt_permissions::ChannelPermission::ViewChannel as i64;
const MANAGE_SERVER: i64 = revolt_permissions::ChannelPermission::ManageServer as i64;

fn deny_view() -> revolt_permissions::OverrideField {
    revolt_permissions::OverrideField { a: 0, d: VIEW }
}

/// The text channel with one of its permission fields changed
fn text_with(s: &Setup, change: impl FnOnce(&mut Channel)) -> Channel {
    let mut channel = s.text.clone();
    change(&mut channel);
    channel
}

#[rocket::async_test]
async fn announce_rule_refuses_a_mature_channel() {
    let s = setup(true).await;
    let channel = text_with(&s, |c| {
        if let Channel::TextChannel { nsfw, .. } = c {
            *nsfw = true;
        }
    });
    assert!(!ServerEvent::place_open_to_everyone(&s.server, Some(&channel)));
}

#[rocket::async_test]
async fn announce_rule_refuses_a_mature_server() {
    let s = setup(true).await;
    let mut server = s.server.clone();
    server.nsfw = true;
    assert!(!ServerEvent::place_open_to_everyone(&server, Some(&s.text)));
    assert!(!ServerEvent::place_open_to_everyone(&server, None));
}

#[rocket::async_test]
async fn announce_rule_refuses_a_channel_role_deny() {
    let s = setup(true).await;
    let channel = text_with(&s, |c| {
        if let Channel::TextChannel { role_permissions, .. } = c {
            role_permissions.insert("role".to_string(), deny_view());
        }
    });
    assert!(!ServerEvent::place_open_to_everyone(&s.server, Some(&channel)));
}

#[rocket::async_test]
async fn announce_rule_refuses_a_channel_member_deny() {
    let s = setup(true).await;
    let channel = text_with(&s, |c| {
        if let Channel::TextChannel { member_permissions, .. } = c {
            member_permissions.insert(s.member.id.clone(), deny_view());
        }
    });
    assert!(!ServerEvent::place_open_to_everyone(&s.server, Some(&channel)));
}

#[rocket::async_test]
async fn announce_rule_refuses_a_synced_category_default_deny() {
    let s = setup(true).await;
    let channel = text_with(&s, |c| {
        if let Channel::TextChannel { permissions_synced, .. } = c {
            *permissions_synced = true;
        }
    });
    let mut server = s.server.clone();
    server.categories = Some(vec![revolt_database::Category {
        id: "cat".into(),
        title: "Cat".into(),
        channels: vec![s.text.id().to_string()],
        default_permissions: Some(deny_view()),
        ..Default::default()
    }]);
    assert!(!ServerEvent::place_open_to_everyone(&server, Some(&channel)));
}

#[rocket::async_test]
async fn announce_rule_refuses_a_synced_category_role_deny() {
    let s = setup(true).await;
    let channel = text_with(&s, |c| {
        if let Channel::TextChannel { permissions_synced, .. } = c {
            *permissions_synced = true;
        }
    });
    let mut server = s.server.clone();
    let mut roles = std::collections::HashMap::new();
    roles.insert("role".to_string(), deny_view());
    server.categories = Some(vec![revolt_database::Category {
        id: "cat".into(),
        title: "Cat".into(),
        channels: vec![s.text.id().to_string()],
        role_permissions: roles,
        ..Default::default()
    }]);
    assert!(!ServerEvent::place_open_to_everyone(&server, Some(&channel)));
}

#[rocket::async_test]
async fn announce_rule_refuses_a_server_role_that_denies_view() {
    let s = setup(true).await;
    s.harness.new_role(&s.server, 5, Some(deny_view())).await;
    let server = s.harness.db.fetch_server(&s.server.id).await.unwrap();
    assert!(!ServerEvent::place_open_to_everyone(&server, Some(&s.text)));
    assert!(!ServerEvent::place_open_to_everyone(&server, None));
}

#[rocket::async_test]
async fn announce_rule_refuses_when_the_server_default_lacks_view() {
    let s = setup(true).await;
    let mut server = s.server.clone();
    server.default_permissions &= !VIEW;
    assert!(!ServerEvent::place_open_to_everyone(&server, Some(&s.text)));
    let channel = text_with(&s, |c| {
        if let Channel::TextChannel { default_permissions, .. } = c {
            *default_permissions = Some(deny_view());
        }
    });
    assert!(!ServerEvent::place_open_to_everyone(&s.server, Some(&channel)));
    // the plain case still announces
    assert!(ServerEvent::place_open_to_everyone(&s.server, Some(&s.text)));
}

/// Make an event active and complete it; the next occurrence
async fn complete_and_next(s: &Setup, id: &str) -> ServerEvent {
    let db = &s.harness.db;
    db.set_server_event_status(
        id,
        v0::ServerEventStatus::Scheduled,
        v0::ServerEventStatus::Active,
        Timestamp::now_utc(),
    )
    .await
    .unwrap();
    let mut event = db.fetch_server_event(id).await.unwrap();
    assert!(event.complete(db).await.unwrap());
    db.fetch_upcoming_server_events(&s.server.id)
        .await
        .unwrap()
        .into_iter()
        .find(|next| next.previous.as_deref() == Some(id))
        .expect("a next occurrence")
}

async fn insert_series(
    s: &Setup,
    start: &str,
    repeat: v0::ServerEventRepeat,
    zone: Option<&str>,
) -> ServerEvent {
    let mut event = ServerEvent::new(
        &s.server.id,
        &s.owner.id,
        "Series".into(),
        None,
        Timestamp::parse(start).unwrap(),
        None,
        v0::ServerEventPlace::External {
            location: "Park".into(),
        },
        Some(repeat),
    );
    event.time_zone = zone.map(str::to_string);
    s.harness.db.insert_server_event(&event).await.unwrap();
    event
}

#[rocket::async_test]
async fn monthly_repeats_follow_the_first_day_of_the_series() {
    let s = setup(true).await;
    let first = insert_series(&s, "2027-01-31T18:00:00Z", v0::ServerEventRepeat::Monthly, None).await;
    let feb = complete_and_next(&s, &first.id).await;
    assert_eq!(feb.starts_at, Timestamp::parse("2027-02-28T18:00:00Z").unwrap());
    let mar = complete_and_next(&s, &feb.id).await;
    assert_eq!(mar.starts_at, Timestamp::parse("2027-03-31T18:00:00Z").unwrap());
    let apr = complete_and_next(&s, &mar.id).await;
    assert_eq!(apr.starts_at, Timestamp::parse("2027-04-30T18:00:00Z").unwrap());

    // leap year
    let leap = insert_series(&s, "2028-01-31T09:00:00Z", v0::ServerEventRepeat::Monthly, None).await;
    let feb = complete_and_next(&s, &leap.id).await;
    assert_eq!(feb.starts_at, Timestamp::parse("2028-02-29T09:00:00Z").unwrap());
    let mar = complete_and_next(&s, &feb.id).await;
    assert_eq!(mar.starts_at, Timestamp::parse("2028-03-31T09:00:00Z").unwrap());
}

#[rocket::async_test]
async fn weekly_repeats_keep_the_local_time_across_daylight_saving_changes() {
    let s = setup(true).await;
    if revolt_database::util::tz::Zone::load("Europe/Vienna").is_none() {
        return; // no zone database here (the tz unit tests cover the rules)
    }
    // 18:00 in Vienna, the Thursday before summer time starts (28 March 2027)
    let spring = insert_series(
        &s,
        "2027-03-25T17:00:00Z",
        v0::ServerEventRepeat::Weekly,
        Some("Europe/Vienna"),
    )
    .await;
    let next = complete_and_next(&s, &spring.id).await;
    assert_eq!(next.starts_at, Timestamp::parse("2027-04-01T16:00:00Z").unwrap());
    assert_eq!(next.time_zone.as_deref(), Some("Europe/Vienna"));
    // and before it ends (31 October 2027)
    let autumn = insert_series(
        &s,
        "2027-10-28T16:00:00Z",
        v0::ServerEventRepeat::EveryTwoWeeks,
        Some("Europe/Vienna"),
    )
    .await;
    let next = complete_and_next(&s, &autumn.id).await;
    assert_eq!(next.starts_at, Timestamp::parse("2027-11-11T17:00:00Z").unwrap());
}

#[rocket::async_test]
async fn the_creators_time_zone_is_stored() {
    let s = setup(true).await;
    let path = format!("/servers/{}/events", s.server.id);
    let mut data = body(voice_place(&s));
    data["time_zone"] = json!("Europe/Vienna");
    let (status, event) = call(&s.harness, Method::Post, &path, &s.owner_session, Some(data)).await;
    assert_eq!(status, Status::Ok, "{event}");
    assert_eq!(event["time_zone"], "Europe/Vienna");
    let mut data = body(voice_place(&s));
    data["time_zone"] = json!("../../etc/passwd");
    let (status, error) = call(&s.harness, Method::Post, &path, &s.owner_session, Some(data)).await;
    assert_eq!(status, Status::BadRequest, "{error}");
}

#[rocket::async_test]
async fn a_creator_without_manage_server_cannot_manage_their_event() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let role = s
        .harness
        .new_role(
            &s.server,
            1,
            Some(revolt_permissions::OverrideField { a: MANAGE_SERVER, d: 0 }),
        )
        .await;
    let mut member = db.fetch_member(&s.server.id, &s.member.id).await.unwrap();
    member
        .update(
            db,
            revolt_database::PartialMember {
                roles: Some(vec![role.id.clone()]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (status, event) = call(
        &s.harness,
        Method::Post,
        &format!("/servers/{}/events", s.server.id),
        &s.member_session,
        Some(body(voice_place(&s))),
    )
    .await;
    assert_eq!(status, Status::Ok, "{event}");
    let base = format!("/servers/{}/events/{}", s.server.id, event["_id"].as_str().unwrap());

    // timed out: refused even with the role
    member
        .update(
            db,
            revolt_database::PartialMember {
                timeout: Some(Timestamp::now_utc() + Duration::hours(1)),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (status, _) = call(&s.harness, Method::Patch, &base, &s.member_session, Some(json!({ "title": "Mine" }))).await;
    assert_eq!(status, Status::Forbidden);

    // demoted: the creator keeps no rights over the event
    member
        .update(
            db,
            revolt_database::PartialMember {
                roles: Some(vec![]),
                ..Default::default()
            },
            vec![revolt_database::FieldsMember::Timeout],
        )
        .await
        .unwrap();
    for (method, path, body) in [
        (Method::Patch, base.clone(), Some(json!({ "title": "Mine" }))),
        (Method::Post, format!("{base}/start"), None),
        (Method::Post, format!("{base}/cancel"), None),
    ] {
        let (status, _) = call(&s.harness, method, &path, &s.member_session, body).await;
        assert_eq!(status, Status::Forbidden, "{path}");
    }
}

#[rocket::async_test]
async fn managers_still_see_an_event_whose_channel_is_gone() {
    let s = setup(true).await;
    let place = channel(&s.harness, &s.server, false, false).await;
    let event = create(&s, json!({ "type": "Text", "channel": place.id() })).await;
    let path = format!("/servers/{}/events/{}", s.server.id, event["_id"].as_str().unwrap());
    place.delete(&s.harness.db).await.unwrap();
    let (status, _) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(status, Status::NotFound);
    let (status, fetched) = call(&s.harness, Method::Get, &path, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{fetched}");
    let (status, _) = call(&s.harness, Method::Delete, &path, &s.owner_session, None).await;
    assert_eq!(status, Status::NoContent);
}

#[rocket::async_test]
async fn cancelling_one_occurrence_keeps_the_series() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let mut data = body(voice_place(&s));
    data["repeat"] = json!("Weekly");
    let (_, event) = call(&s.harness, Method::Post, &format!("/servers/{}/events", s.server.id), &s.owner_session, Some(data)).await;
    let id = event["_id"].as_str().unwrap().to_string();
    let (status, _) = call(&s.harness, Method::Post, &format!("/servers/{}/events/{id}/cancel", s.server.id), &s.owner_session, None).await;
    assert_eq!(status, Status::Ok);
    let upcoming = db.fetch_upcoming_server_events(&s.server.id).await.unwrap();
    let next = upcoming
        .iter()
        .find(|e| e.previous.as_deref() == Some(id.as_str()))
        .expect("the series goes on")
        .clone();

    // deleting one occurrence keeps it too
    let (status, _) = call(&s.harness, Method::Delete, &format!("/servers/{}/events/{}", s.server.id, next.id), &s.owner_session, None).await;
    assert_eq!(status, Status::NoContent);
    let upcoming = db.fetch_upcoming_server_events(&s.server.id).await.unwrap();
    let third = upcoming
        .iter()
        .find(|e| e.previous.as_deref() == Some(next.id.as_str()))
        .expect("still going on")
        .clone();

    // the whole series stops
    let (status, cancelled) = call(
        &s.harness,
        Method::Post,
        &format!("/servers/{}/events/{}/cancel?series=true", s.server.id, third.id),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{cancelled}");
    assert!(db.fetch_upcoming_server_events(&s.server.id).await.unwrap().is_empty());
}

#[rocket::async_test]
async fn external_places_take_only_web_links() {
    let s = setup(true).await;
    let path = format!("/servers/{}/events", s.server.id);
    for location in ["javascript:alert(1)", "data:text/html,hi", "file:///etc/passwd", "ftp://example.org/x"] {
        let data = body(json!({ "type": "External", "location": location }));
        let (status, error) = call(&s.harness, Method::Post, &path, &s.owner_session, Some(data)).await;
        assert_eq!(status, Status::BadRequest, "{location}: {error}");
    }
    for location in ["https://example.org/meet", "Main square 1", "Room 5: second floor"] {
        let data = body(json!({ "type": "External", "location": location }));
        let (status, error) = call(&s.harness, Method::Post, &path, &s.owner_session, Some(data)).await;
        assert_eq!(status, Status::Ok, "{location}: {error}");
    }
}

#[rocket::async_test]
async fn a_member_who_leaves_is_no_longer_counted() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create(&s, json!({ "type": "External", "location": "Park" })).await;
    let id = event["_id"].as_str().unwrap().to_string();
    call(&s.harness, Method::Put, &format!("/servers/{}/events/{id}/interested", s.server.id), &s.member_session, None).await;
    let member = db.fetch_member(&s.server.id, &s.member.id).await.unwrap();
    member
        .remove(db, &s.server, revolt_database::RemovalIntention::Leave, true)
        .await
        .unwrap();
    let stored = db.fetch_server_event(&id).await.unwrap();
    assert_eq!(stored.interested_count, 0);
    assert!(db.fetch_server_event_interested(&id, None, 10).await.unwrap().is_empty());
}

#[rocket::async_test]
async fn the_crond_pass_gives_a_finished_series_its_next_occurrence_once() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let first = insert_series(&s, "2027-02-04T18:00:00Z", v0::ServerEventRepeat::Weekly, None).await;
    // completed without its next one (a crash between the two steps)
    let now = Timestamp::now_utc();
    db.set_server_event_status(&first.id, v0::ServerEventStatus::Scheduled, v0::ServerEventStatus::Active, now)
        .await
        .unwrap();
    db.set_server_event_status(&first.id, v0::ServerEventStatus::Active, v0::ServerEventStatus::Completed, now)
        .await
        .unwrap();
    let report = ServerEvent::run_pass(db, now).await.unwrap();
    assert_eq!(report.scheduled, 1, "{report:?}");
    let again = ServerEvent::run_pass(db, now).await.unwrap();
    assert_eq!(again.scheduled, 0, "{again:?}");
    let upcoming = db.fetch_upcoming_server_events(&s.server.id).await.unwrap();
    assert_eq!(upcoming.len(), 1);
    assert_eq!(upcoming[0].starts_at, Timestamp::parse("2027-02-11T18:00:00Z").unwrap());
    assert_eq!(upcoming[0].occurrence, 1);
}

#[rocket::async_test]
async fn two_writers_make_one_next_occurrence() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let first = insert_series(&s, "2027-02-04T18:00:00Z", v0::ServerEventRepeat::Weekly, None).await;
    let now = Timestamp::now_utc();
    let a = first.schedule_next(db, now).await.unwrap().unwrap();
    let b = first.clone().schedule_next(db, now).await.unwrap().unwrap();
    assert_eq!(a.id, b.id);
    let upcoming = db.fetch_upcoming_server_events(&s.server.id).await.unwrap();
    assert_eq!(upcoming.len(), 2, "the first and one next");
}

#[rocket::async_test]
async fn the_crond_pass_cancels_events_whose_channel_is_gone() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let place = channel(&s.harness, &s.server, false, false).await;
    let mut event = insert_series(&s, "2027-02-04T18:00:00Z", v0::ServerEventRepeat::Weekly, None).await;
    event.place = v0::ServerEventPlace::Text {
        channel: place.id().to_string(),
    };
    db.update_server_event(&event).await.unwrap();
    db.set_server_event_status(&event.id, v0::ServerEventStatus::Scheduled, v0::ServerEventStatus::Active, Timestamp::now_utc())
        .await
        .unwrap();
    place.delete(db).await.unwrap();
    let report = ServerEvent::run_pass(db, Timestamp::now_utc()).await.unwrap();
    assert_eq!(report.cancelled, 1, "{report:?}");
    let stored = db.fetch_server_event(&event.id).await.unwrap();
    assert_eq!(stored.status, v0::ServerEventStatus::Cancelled);
    // a place that is gone ends the series
    assert!(db.fetch_upcoming_server_events(&s.server.id).await.unwrap().is_empty());
}

/// Events start off in every server; only Manage Server turns them on (the
/// gateway tells members), and while off no event can be created. Events
/// made before stay stored.
#[rocket::async_test]
async fn events_are_off_until_a_manager_turns_them_on() {
    let s = setup_off(true).await;
    assert!(!s.server.events_enabled, "a new server has events off");
    let path = format!("/servers/{}/events", s.server.id);
    let server_path = format!("/servers/{}", s.server.id);

    let (status, error) = call(&s.harness, Method::Post, &path, &s.owner_session, Some(body(voice_place(&s)))).await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "FeatureDisabled", "{error}");
    assert_eq!(error["feature"], "server_events_server", "{error}");

    // a member without Manage Server can't switch them on
    let switch = |on: bool| Some(json!({ "events_enabled": on }));
    let (status, _) = call(&s.harness, Method::Patch, &server_path, &s.member_session, switch(true)).await;
    assert_eq!(status, Status::Forbidden);
    assert!(!s.harness.db.fetch_server(&s.server.id).await.unwrap().events_enabled);

    let mut gateway = PubSubTestHelper::new(&s.server.id).await;
    let (status, server) = call(&s.harness, Method::Patch, &server_path, &s.owner_session, switch(true)).await;
    assert_eq!(status, Status::Ok, "{server}");
    assert_eq!(server["events_enabled"], true, "{server}");
    let id = s.server.id.clone();
    gateway
        .wait_for_event(|e| {
            matches!(e, EventV1::ServerUpdate { id: server, data, .. }
                if server == &id && data.events_enabled == Some(true))
        })
        .await;

    let event = create(&s, voice_place(&s)).await;
    let event_id = event["_id"].as_str().unwrap().to_string();

    // off again: nothing new, the stored event stays
    let (status, server) = call(&s.harness, Method::Patch, &server_path, &s.owner_session, switch(false)).await;
    assert_eq!(status, Status::Ok, "{server}");
    assert!(server.get("events_enabled").is_none() || server["events_enabled"] == false, "{server}");
    let (status, error) = call(&s.harness, Method::Post, &path, &s.owner_session, Some(body(voice_place(&s)))).await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["feature"], "server_events_server", "{error}");
    assert!(s.harness.db.fetch_server_event(&event_id).await.is_ok());

    // bots and clients see what the app shows: nothing while off
    let one = format!("{path}/{event_id}");
    for (who, url) in [(&s.member_session, &path), (&s.owner_session, &path), (&s.member_session, &one)] {
        let (status, error) = call(&s.harness, Method::Get, url, who, None).await;
        assert_eq!(status, Status::BadRequest, "{url} {error}");
        assert_eq!(error["feature"], "server_events_server", "{error}");
    }

    // on again: the stored event is back
    let (status, _) = call(&s.harness, Method::Patch, &server_path, &s.owner_session, switch(true)).await;
    assert_eq!(status, Status::Ok);
    let (status, list) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{list}");
    assert_eq!(list["events"][0]["_id"], event_id, "{list}");
}

/// While a server has events off, crond leaves its events alone: nothing
/// starts or is announced, and an event whose whole time passed while off is
/// marked Completed without a word. Turned on again, what is still due starts.
#[rocket::async_test]
async fn the_crond_pass_skips_servers_with_events_off() {
    let s = setup_off(true).await;
    let db = &s.harness.db;
    let now = Timestamp::now_utc();
    let place = || v0::ServerEventPlace::External { location: "Park".into() };
    // due a minute ago, still running
    let due = ServerEvent::new(&s.server.id, &s.owner.id, "Due".into(), None, now - Duration::minutes(1), Some(now + Duration::hours(1)), place(), None);
    // its whole time passed while events were off
    let missed = ServerEvent::new(&s.server.id, &s.owner.id, "Missed".into(), None, now - Duration::hours(3), Some(now - Duration::hours(1)), place(), None);
    // started before events went off; its end has passed
    let running = ServerEvent::new(&s.server.id, &s.owner.id, "Running".into(), None, now - Duration::hours(3), Some(now - Duration::minutes(5)), place(), None);
    for event in [&due, &missed, &running] {
        db.insert_server_event(event).await.unwrap();
    }
    db.set_server_event_status(&running.id, v0::ServerEventStatus::Scheduled, v0::ServerEventStatus::Active, now - Duration::hours(3))
        .await
        .unwrap();

    let report = ServerEvent::run_pass(db, now).await.unwrap();
    assert_eq!((report.started, report.completed), (0, 0), "{report:?}");
    assert_eq!(report.missed, 2, "{report:?}");
    let status = |id: String| async move { db.fetch_server_event(&id).await.unwrap().status };
    assert_eq!(status(due.id.clone()).await, v0::ServerEventStatus::Scheduled);
    assert_eq!(status(missed.id.clone()).await, v0::ServerEventStatus::Completed);
    assert_eq!(status(running.id.clone()).await, v0::ServerEventStatus::Completed);

    // a second pass changes nothing
    let again = ServerEvent::run_pass(db, now).await.unwrap();
    assert_eq!(again, Default::default(), "{again:?}");

    // turned on: the event that is still due starts
    db.update_server(&s.server.id, &PartialServer { events_enabled: Some(true), ..Default::default() }, vec![])
        .await
        .unwrap();
    let report = ServerEvent::run_pass(db, Timestamp::now_utc()).await.unwrap();
    assert_eq!(report.started, 1, "{report:?}");
    assert_eq!(status(due.id.clone()).await, v0::ServerEventStatus::Active);
}

/// Deleting with `series=true` ends a repeating series: this occurrence goes
/// and nothing next is scheduled. Without it, the next one is still written.
#[rocket::async_test]
async fn deleting_the_whole_series_schedules_nothing_next() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let base = format!("/servers/{}/events", s.server.id);
    let weekly = || {
        let mut data = body(voice_place(&s));
        data["repeat"] = json!("Weekly");
        Some(data)
    };

    // one occurrence: the series goes on
    let (_, first) = call(&s.harness, Method::Post, &base, &s.owner_session, weekly()).await;
    let first = first["_id"].as_str().unwrap().to_string();
    let (status, _) = call(&s.harness, Method::Delete, &format!("{base}/{first}"), &s.owner_session, None).await;
    assert_eq!(status, Status::NoContent);
    let upcoming = db.fetch_upcoming_server_events(&s.server.id).await.unwrap();
    let next = upcoming
        .iter()
        .find(|e| e.previous.as_deref() == Some(first.as_str()))
        .expect("the series goes on")
        .clone();

    // the whole series: nothing of it is scheduled any more
    let (status, _) = call(&s.harness, Method::Delete, &format!("{base}/{}?series=true", next.id), &s.owner_session, None).await;
    assert_eq!(status, Status::NoContent);
    let upcoming = db.fetch_upcoming_server_events(&s.server.id).await.unwrap();
    assert!(upcoming.is_empty(), "{:?}", upcoming.iter().map(|e| (&e.id, &e.previous)).collect::<Vec<_>>());
    assert!(db.fetch_server_event(&next.id).await.is_err());

    // a member may not
    let (_, other) = call(&s.harness, Method::Post, &base, &s.owner_session, weekly()).await;
    let other = other["_id"].as_str().unwrap().to_string();
    let (status, _) = call(&s.harness, Method::Delete, &format!("{base}/{other}?series=true"), &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);
}
