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

async fn setup(enabled: bool) -> Setup {
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
