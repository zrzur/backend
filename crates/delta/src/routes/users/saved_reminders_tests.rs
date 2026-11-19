//! Saved messages and message reminders: who may save what, that the list is
//! private, that a message which is gone or closed leaks nothing, search,
//! limits (Feather included) and delivery as crond does it. Runs on
//! REFERENCE and MONGODB (TEST_DB).

use revolt_config::overwrite_config;
use revolt_database::events::client::EventV1;
use revolt_database::{
    deliver_due_reminders, now_ms, Channel, Member, Message, PartialChannel, Server, Session, User,
};
use revolt_permissions::{ChannelPermission, OverrideField};
use rocket::http::{ContentType, Header, Method, Status};
use serde_json::{json, Value};

use crate::util::test::{PubSubTestHelper, TestHarness};

const MINUTE: i64 = 60 * 1000;
const DAY: i64 = 24 * 60 * MINUTE;

struct Setup {
    harness: TestHarness,
    author: User,
    me: User,
    session: Session,
    other: User,
    other_session: Session,
    server: Server,
    open: Channel,
}

async fn setup_with(enabled: bool) -> Setup {
    overwrite_config(move |config| config.features.saved_reminders.enabled = enabled).await;
    let harness = TestHarness::new().await;
    let (_, _, author) = harness.new_user().await;
    let (_, session, me) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (server, channels) = harness.new_server(&author).await;
    for user in [&author, &me, &other] {
        Member::create(&harness.db, &server, user, Some(channels.clone()))
            .await
            .expect("member");
    }
    Setup {
        open: channels[0].clone(),
        harness,
        author,
        me,
        session,
        other,
        other_session,
        server,
    }
}

async fn setup() -> Setup {
    setup_with(true).await
}

/// A message by the author in a channel, with an id of its own
async fn message(s: &Setup, channel: &Channel, text: &str) -> Message {
    let message = Message {
        id: ulid::Ulid::new().to_string(),
        channel: channel.id().to_string(),
        author: s.author.id.clone(),
        content: Some(text.to_string()),
        ..Default::default()
    };
    s.harness.db.insert_message(&message).await.expect("message");
    message
}

/// Close a channel to everyone but the owner
async fn hide(s: &Setup, channel: &Channel) {
    let mut channel = s.harness.db.fetch_channel(channel.id()).await.unwrap();
    channel
        .update(
            &s.harness.db,
            PartialChannel {
                default_permissions: Some(OverrideField {
                    a: 0,
                    d: ChannelPermission::ViewChannel as i64,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .expect("hide");
}

async fn call(
    harness: &TestHarness,
    method: Method,
    path: &str,
    session: &Session,
    body: Option<Value>,
) -> (Status, Value, String) {
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
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    )
}

async fn save(s: &Setup, session: &Session, message: &Message) -> (Status, Value) {
    let (status, body, _) = call(
        &s.harness,
        Method::Put,
        &format!("/users/@me/saved/{}", message.id),
        session,
        None,
    )
    .await;
    (status, body)
}

async fn saved(s: &Setup, session: &Session, query: &str) -> (Value, String) {
    let (status, body, text) = call(
        &s.harness,
        Method::Get,
        &format!("/users/@me/saved{query}"),
        session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{text}");
    (body, text)
}

async fn remind(s: &Setup, session: &Session, message: &str, due_at: i64) -> (Status, Value) {
    let (status, body, _) = call(
        &s.harness,
        Method::Post,
        "/users/@me/reminders",
        session,
        Some(json!({ "message": message, "due_at": due_at })),
    )
    .await;
    (status, body)
}

async fn reminders(s: &Setup, session: &Session) -> (Value, String) {
    let (status, body, text) =
        call(&s.harness, Method::Get, "/users/@me/reminders", session, None).await;
    assert_eq!(status, Status::Ok, "{text}");
    (body, text)
}

fn ids(list: &Value, field: &str) -> Vec<String> {
    list.as_array()
        .expect("a list")
        .iter()
        .map(|item| item[field].as_str().expect("an id").to_string())
        .collect()
}

#[rocket::async_test]
async fn off_by_default_every_route_is_refused() {
    let defaults = revolt_config::config().await.features.saved_reminders;
    assert!(!defaults.enabled, "the flag ships off");

    let s = setup_with(false).await;
    let m = message(&s, &s.open, "hello").await;
    for (method, path, body) in [
        (Method::Get, "/users/@me/saved".to_string(), None),
        (Method::Put, format!("/users/@me/saved/{}", m.id), None),
        (Method::Delete, format!("/users/@me/saved/{}", m.id), None),
        (Method::Get, "/users/@me/reminders".to_string(), None),
        (
            Method::Post,
            "/users/@me/reminders".to_string(),
            Some(json!({ "message": m.id, "due_at": now_ms() + MINUTE })),
        ),
        (
            Method::Patch,
            "/users/@me/reminders/x".to_string(),
            Some(json!({ "due_at": now_ms() + MINUTE })),
        ),
        (Method::Delete, "/users/@me/reminders/x".to_string(), None),
    ] {
        let (status, body, text) = call(&s.harness, method, &path, &s.session, body).await;
        assert_eq!(status, Status::BadRequest, "{method} {path}: {text}");
        assert_eq!(body["type"], "FeatureDisabled", "{method} {path}");
        assert_eq!(body["feature"], "saved_reminders");
    }
    assert!(s.harness.db.fetch_saved_messages(&s.me.id).await.unwrap().is_empty());
}

#[rocket::async_test]
async fn bots_have_no_saved_list_and_no_reminders() {
    let s = setup().await;
    let (bot, _) = s.harness.new_bot(&s.me).await;
    for path in ["/users/@me/saved", "/users/@me/reminders"] {
        let response = s
            .harness
            .client
            .get(path)
            .header(Header::new("x-bot-token", bot.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest, "{path}");
        assert!(response.into_string().await.unwrap().contains("IsBot"));
    }
}

#[rocket::async_test]
async fn save_list_and_remove() {
    let s = setup().await;
    let first = message(&s, &s.open, "the first one").await;
    let second = message(&s, &s.open, "the second one").await;

    let (status, body) = save(&s, &s.session, &first).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["message"], first.id);
    assert_eq!(body["channel"], s.open.id());
    assert_eq!(body["available"], true);
    let saved_at = body["saved_at"].as_i64().unwrap();

    // saving again keeps the first entry
    let (status, again) = save(&s, &s.session, &first).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(again["saved_at"].as_i64().unwrap(), saved_at);

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(save(&s, &s.session, &second).await.0, Status::Ok);

    let (list, _) = saved(&s, &s.session, "").await;
    assert_eq!(list["total"], 2);
    assert_eq!(
        ids(&list["entries"], "message"),
        vec![second.id.clone(), first.id.clone()],
        "newest first"
    );
    assert_eq!(list["messages"].as_array().unwrap().len(), 2);
    assert_eq!(ids(&list["users"], "_id"), vec![s.author.id.clone()]);
    let text = list["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["_id"] == first.id)
        .unwrap();
    assert_eq!(text["content"], "the first one");
    assert_eq!(text["channel"], s.open.id());

    let path = format!("/users/@me/saved/{}", first.id);
    let (status, _, _) = call(&s.harness, Method::Delete, &path, &s.session, None).await;
    assert_eq!(status, Status::NoContent);
    // removing what is not there is fine too
    let (status, _, _) = call(&s.harness, Method::Delete, &path, &s.session, None).await;
    assert_eq!(status, Status::NoContent);

    let (list, _) = saved(&s, &s.session, "").await;
    assert_eq!(ids(&list["entries"], "message"), vec![second.id]);
}

#[rocket::async_test]
async fn only_readable_messages_can_be_saved_or_reminded() {
    let s = setup().await;
    let hidden = s.harness.new_channel(&s.server).await;
    hide(&s, &hidden).await;
    let secret = message(&s, &hidden, "secret plans").await;
    let due = now_ms() + MINUTE;

    // a message in a channel the person cannot view, and one that is not
    // there, give the same answer
    let (status, body) = save(&s, &s.session, &secret).await;
    assert_eq!(status, Status::NotFound, "{body}");
    let (status, missing, _) = call(
        &s.harness,
        Method::Put,
        "/users/@me/saved/01NOTAMESSAGE0000000000000",
        &s.session,
        None,
    )
    .await;
    assert_eq!(status, Status::NotFound);
    assert_eq!(body, missing, "no difference between closed and missing");

    assert_eq!(remind(&s, &s.session, &secret.id, due).await.0, Status::NotFound);
    assert_eq!(
        remind(&s, &s.session, "01NOTAMESSAGE0000000000000", due).await.0,
        Status::NotFound
    );

    // someone outside the server cannot save its messages
    let (_, outsider_session, _) = s.harness.new_user().await;
    let open = message(&s, &s.open, "members only").await;
    assert_eq!(save(&s, &outsider_session, &open).await.0, Status::NotFound);
    assert_eq!(
        remind(&s, &outsider_session, &open.id, due).await.0,
        Status::NotFound
    );
}

#[rocket::async_test]
async fn a_closed_or_deleted_message_is_unavailable_and_leaks_nothing() {
    let s = setup().await;
    let room = s.harness.new_channel(&s.server).await;
    let closed = message(&s, &room, "quarterly-numbers-7731").await;
    let gone = message(&s, &s.open, "deleted-words-4409").await;
    let kept = message(&s, &s.open, "still here").await;
    for m in [&closed, &gone, &kept] {
        assert_eq!(save(&s, &s.session, m).await.0, Status::Ok);
    }
    let due = now_ms() + MINUTE;
    assert_eq!(remind(&s, &s.session, &closed.id, due).await.0, Status::Ok);
    assert_eq!(remind(&s, &s.session, &gone.id, due).await.0, Status::Ok);

    hide(&s, &room).await;
    s.harness
        .db
        .fetch_message(&gone.id)
        .await
        .unwrap()
        .delete(&s.harness.db)
        .await
        .expect("delete");

    let (list, text) = saved(&s, &s.session, "").await;
    assert_eq!(list["total"], 3);
    for entry in list["entries"].as_array().unwrap() {
        let available = entry["message"] == kept.id;
        assert_eq!(entry["available"], available, "{entry}");
    }
    assert_eq!(ids(&list["messages"], "_id"), vec![kept.id.clone()]);
    assert!(!text.contains("quarterly-numbers-7731"), "{text}");
    assert!(!text.contains("deleted-words-4409"), "{text}");

    // a search cannot find them either
    for query in ["quarterly", "deleted-words"] {
        let (found, text) = saved(&s, &s.session, &format!("?query={query}")).await;
        assert!(found["entries"].as_array().unwrap().is_empty(), "{text}");
        assert!(found["messages"].as_array().unwrap().is_empty(), "{text}");
    }

    let (list, text) = reminders(&s, &s.session).await;
    assert_eq!(list["reminders"].as_array().unwrap().len(), 2);
    for reminder in list["reminders"].as_array().unwrap() {
        assert_eq!(reminder["available"], false);
    }
    assert!(list["messages"].as_array().unwrap().is_empty());
    assert!(!text.contains("quarterly-numbers-7731") && !text.contains("deleted-words-4409"));

    // the entries can still be removed
    for m in [&closed, &gone] {
        let path = format!("/users/@me/saved/{}", m.id);
        let (status, _, _) = call(&s.harness, Method::Delete, &path, &s.session, None).await;
        assert_eq!(status, Status::NoContent);
    }
    let (list, _) = saved(&s, &s.session, "").await;
    assert_eq!(list["total"], 1);
}

#[rocket::async_test]
async fn the_list_and_the_reminders_are_private() {
    let s = setup().await;
    let m = message(&s, &s.open, "mine to keep").await;
    assert_eq!(save(&s, &s.session, &m).await.0, Status::Ok);
    let (status, mine) = remind(&s, &s.session, &m.id, now_ms() + MINUTE).await;
    assert_eq!(status, Status::Ok);
    let id = mine["_id"].as_str().unwrap();

    // another member of the same server sees nothing of it
    let (list, _) = saved(&s, &s.other_session, "").await;
    assert_eq!(list["total"], 0);
    assert!(list["entries"].as_array().unwrap().is_empty());
    let (list, _) = reminders(&s, &s.other_session).await;
    assert!(list["reminders"].as_array().unwrap().is_empty());

    // and can change nothing of it
    let path = format!("/users/@me/saved/{}", m.id);
    call(&s.harness, Method::Delete, &path, &s.other_session, None).await;
    let (list, _) = saved(&s, &s.session, "").await;
    assert_eq!(list["total"], 1, "their remove did not touch my entry");

    let path = format!("/users/@me/reminders/{id}");
    let (status, _, _) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.other_session,
        Some(json!({ "due_at": now_ms() + 2 * MINUTE })),
    )
    .await;
    assert_eq!(status, Status::NotFound);
    let (status, _, _) = call(&s.harness, Method::Delete, &path, &s.other_session, None).await;
    assert_eq!(status, Status::NotFound);
    let (list, _) = reminders(&s, &s.session).await;
    assert_eq!(ids(&list["reminders"], "_id"), vec![id.to_string()]);
    assert_eq!(list["reminders"][0]["due_at"], mine["due_at"]);

    // without a session there is no answer at all
    let response = s.harness.client.get("/users/@me/saved").dispatch().await;
    assert_eq!(response.status(), Status::Unauthorized);
    assert_ne!(s.other.id, s.me.id);
}

#[rocket::async_test]
async fn search_and_pages() {
    let s = setup().await;
    let mut all = vec![];
    for text in ["Buy bread", "موعد الطبيب غداً", "bread recipe", "nothing else"] {
        let m = message(&s, &s.open, text).await;
        assert_eq!(save(&s, &s.session, &m).await.0, Status::Ok);
        all.push(m);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let (found, _) = saved(&s, &s.session, "?query=BREAD").await;
    assert_eq!(
        ids(&found["entries"], "message"),
        vec![all[2].id.clone(), all[0].id.clone()]
    );
    assert_eq!(found["messages"].as_array().unwrap().len(), 2);
    assert_eq!(found["total"], 4, "the total is the whole list");

    let (found, _) = saved(&s, &s.session, "?query=%D8%A7%D9%84%D8%B7%D8%A8%D9%8A%D8%A8").await;
    assert_eq!(ids(&found["entries"], "message"), vec![all[1].id.clone()]);

    // by the author's name
    let name = s.author.username.to_uppercase();
    let (found, _) = saved(&s, &s.session, &format!("?query={name}")).await;
    assert_eq!(found["entries"].as_array().unwrap().len(), 4);

    let (status, _, _) = call(
        &s.harness,
        Method::Get,
        &format!("/users/@me/saved?query={}", "x".repeat(101)),
        &s.session,
        None,
    )
    .await;
    assert_eq!(status, Status::BadRequest);

    // pages: two, then the two before them
    let (page, _) = saved(&s, &s.session, "?limit=2").await;
    assert_eq!(
        ids(&page["entries"], "message"),
        vec![all[3].id.clone(), all[2].id.clone()]
    );
    assert_eq!(page["messages"].as_array().unwrap().len(), 2);
    let before = page["entries"][1]["saved_at"].as_i64().unwrap();
    let (page, _) = saved(&s, &s.session, &format!("?limit=2&before={before}")).await;
    assert_eq!(
        ids(&page["entries"], "message"),
        vec![all[1].id.clone(), all[0].id.clone()]
    );
}

#[rocket::async_test]
async fn the_saved_list_has_a_ceiling() {
    overwrite_config(|config| config.features.saved_reminders.max_saved = 2).await;
    let s = setup().await;
    let a = message(&s, &s.open, "a").await;
    let b = message(&s, &s.open, "b").await;
    let c = message(&s, &s.open, "c").await;
    assert_eq!(save(&s, &s.session, &a).await.0, Status::Ok);
    assert_eq!(save(&s, &s.session, &b).await.0, Status::Ok);
    let (status, body) = save(&s, &s.session, &c).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["type"], "TooManySavedMessages");
    assert_eq!(body["max"], 2);
    // one that is saved already is still answered
    assert_eq!(save(&s, &s.session, &a).await.0, Status::Ok);
    let (list, _) = saved(&s, &s.session, "").await;
    assert_eq!(list["max"], 2);
    assert_eq!(list["total"], 2);
}

#[rocket::async_test]
async fn set_list_move_and_cancel_a_reminder() {
    let s = setup().await;
    let m = message(&s, &s.open, "call the bank").await;
    let later = message(&s, &s.open, "later").await;
    let now = now_ms();

    let (status, far) = remind(&s, &s.session, &later.id, now + 3 * DAY).await;
    assert_eq!(status, Status::Ok, "{far}");
    let (status, soon) = remind(&s, &s.session, &m.id, now + 20 * MINUTE).await;
    assert_eq!(status, Status::Ok, "{soon}");
    assert_eq!(soon["message"], m.id);
    assert_eq!(soon["channel"], s.open.id());
    assert_eq!(soon["due_at"], now + 20 * MINUTE);
    assert_eq!(soon["available"], true);
    assert!(soon.get("delivered_at").is_none());
    let soon_id = soon["_id"].as_str().unwrap().to_string();
    let far_id = far["_id"].as_str().unwrap().to_string();

    let (list, _) = reminders(&s, &s.session).await;
    assert_eq!(
        ids(&list["reminders"], "_id"),
        vec![soon_id.clone(), far_id.clone()],
        "soonest first"
    );
    assert_eq!(list["messages"].as_array().unwrap().len(), 2);
    assert_eq!(ids(&list["users"], "_id"), vec![s.author.id.clone()]);
    assert_eq!(list["max_active"], 10);
    assert_eq!(list["max_active_plus"], 100);

    // move it behind the other one
    let path = format!("/users/@me/reminders/{soon_id}");
    let (status, moved, _) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.session,
        Some(json!({ "due_at": now + 5 * DAY })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{moved}");
    assert_eq!(moved["due_at"], now + 5 * DAY);
    assert_eq!(moved["available"], true);
    let (list, _) = reminders(&s, &s.session).await;
    assert_eq!(ids(&list["reminders"], "_id"), vec![far_id.clone(), soon_id.clone()]);

    let (status, _, _) = call(&s.harness, Method::Delete, &path, &s.session, None).await;
    assert_eq!(status, Status::NoContent);
    let (status, _, _) = call(&s.harness, Method::Delete, &path, &s.session, None).await;
    assert_eq!(status, Status::NotFound);
    let (list, _) = reminders(&s, &s.session).await;
    assert_eq!(ids(&list["reminders"], "_id"), vec![far_id]);
}

#[rocket::async_test]
async fn a_reminder_needs_a_time_ahead_within_the_range() {
    let s = setup().await;
    let m = message(&s, &s.open, "when").await;
    let now = now_ms();

    for due in [now - MINUTE, 0, -5, now + 367 * DAY, i64::MAX] {
        let (status, body) = remind(&s, &s.session, &m.id, due).await;
        assert_eq!(status, Status::BadRequest, "{due}: {body}");
        assert_eq!(body["type"], "FailedValidation", "{due}");
    }
    let (status, ok) = remind(&s, &s.session, &m.id, now + 365 * DAY).await;
    assert_eq!(status, Status::Ok, "{ok}");

    let path = format!("/users/@me/reminders/{}", ok["_id"].as_str().unwrap());
    let (status, body, _) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.session,
        Some(json!({ "due_at": now - DAY })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["type"], "FailedValidation");
}

#[rocket::async_test]
async fn waiting_reminders_are_limited_and_feather_raises_it() {
    overwrite_config(|config| {
        config.features.saved_reminders.max_active = 2;
        config.features.saved_reminders.max_active_plus = 4;
        // billing in test mode: a simulated Feather counts
        config.features.billing.enabled = true;
        config.features.billing.test_mode = true;
    })
    .await;
    let s = setup().await;
    let m = message(&s, &s.open, "many").await;
    let due = now_ms() + DAY;

    assert_eq!(remind(&s, &s.session, &m.id, due).await.0, Status::Ok);
    assert_eq!(remind(&s, &s.session, &m.id, due).await.0, Status::Ok);
    let (status, body) = remind(&s, &s.session, &m.id, due).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["type"], "TooManyReminders");
    assert_eq!(body["max"], 2);
    let (list, _) = reminders(&s, &s.session).await;
    assert_eq!(list["max_active"], 2);
    assert_eq!(list["max_active_plus"], 4);

    // someone else's count is their own
    assert_eq!(remind(&s, &s.other_session, &m.id, due).await.0, Status::Ok);

    // Feather
    s.harness
        .db
        .update_user(
            &s.me.id,
            &revolt_database::PartialUser {
                plus: Some(revolt_database::UserPlus {
                    until: Some(
                        iso8601_timestamp::Timestamp::now_utc()
                            + iso8601_timestamp::Duration::days(30),
                    ),
                    hide_badge: false,
                    live: false,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(remind(&s, &s.session, &m.id, due).await.0, Status::Ok);
    assert_eq!(remind(&s, &s.session, &m.id, due).await.0, Status::Ok);
    let (status, body) = remind(&s, &s.session, &m.id, due).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["max"], 4);
    let (list, _) = reminders(&s, &s.session).await;
    assert_eq!(list["max_active"], 4);
}

#[rocket::async_test]
async fn a_due_reminder_is_delivered_once() {
    overwrite_config(|config| config.features.saved_reminders.max_active = 1).await;
    let s = setup().await;
    let m = message(&s, &s.open, "pay the rent").await;
    let now = now_ms();
    let (status, set) = remind(&s, &s.session, &m.id, now + 20 * MINUTE).await;
    assert_eq!(status, Status::Ok);
    let id = set["_id"].as_str().unwrap().to_string();

    // before its time nothing happens
    let report = deliver_due_reminders(&s.harness.db, now).await.unwrap();
    assert_eq!(report, Default::default());

    let mut events = PubSubTestHelper::new(&format!("{}!", s.me.id)).await;
    let at = now + 21 * MINUTE;
    let report = deliver_due_reminders(&s.harness.db, at).await.unwrap();
    assert_eq!((report.delivered, report.unavailable, report.failed), (1, 0, 0));

    let event = events
        .wait_for_event(|event| matches!(event, EventV1::MessageReminderDue(_)))
        .await;
    let EventV1::MessageReminderDue(due) = event else {
        unreachable!()
    };
    assert_eq!(due.reminder.id, id);
    assert_eq!(due.reminder.delivered_at, Some(at));
    assert!(due.reminder.available);
    assert_eq!(due.message.as_ref().unwrap().id, m.id);
    assert_eq!(due.message.unwrap().content.as_deref(), Some("pay the rent"));
    assert_eq!(due.user.unwrap().id, s.author.id);

    // a second pass, or a second crond, does not deliver it again
    let report = deliver_due_reminders(&s.harness.db, at + MINUTE).await.unwrap();
    assert_eq!(report.delivered, 0);
    assert!(!s.harness.db.claim_reminder(&id, at).await.unwrap());

    // it stays listed as delivered, no longer counts, and cannot be moved
    let (list, _) = reminders(&s, &s.session).await;
    assert_eq!(list["reminders"][0]["delivered_at"], at);
    let path = format!("/users/@me/reminders/{id}");
    let (status, _, _) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.session,
        Some(json!({ "due_at": now_ms() + DAY })),
    )
    .await;
    assert_eq!(status, Status::NotFound);
    let (status, next) = remind(&s, &s.session, &m.id, now_ms() + DAY).await;
    assert_eq!(status, Status::Ok, "a delivered reminder frees its place: {next}");
    let (list, _) = reminders(&s, &s.session).await;
    assert_eq!(
        ids(&list["reminders"], "_id"),
        vec![next["_id"].as_str().unwrap().to_string(), id.clone()],
        "waiting first, then delivered"
    );

    // the person clears it from the list
    let (status, _, _) = call(&s.harness, Method::Delete, &path, &s.session, None).await;
    assert_eq!(status, Status::NoContent);
}

#[rocket::async_test]
async fn a_reminder_for_a_closed_or_deleted_message_carries_nothing() {
    let s = setup().await;
    let room = s.harness.new_channel(&s.server).await;
    let closed = message(&s, &room, "board-minutes-9914").await;
    let gone = message(&s, &s.open, "erased-note-5520").await;
    let now = now_ms();
    for m in [&closed, &gone] {
        assert_eq!(remind(&s, &s.session, &m.id, now + MINUTE).await.0, Status::Ok);
    }
    hide(&s, &room).await;
    s.harness
        .db
        .fetch_message(&gone.id)
        .await
        .unwrap()
        .delete(&s.harness.db)
        .await
        .unwrap();

    let mut events = PubSubTestHelper::new(&format!("{}!", s.me.id)).await;
    let report = deliver_due_reminders(&s.harness.db, now + 2 * MINUTE)
        .await
        .unwrap();
    assert_eq!((report.delivered, report.unavailable), (2, 2));

    for wanted in [&closed, &gone] {
        let event = events
            .wait_for_event(|event| match event {
                EventV1::MessageReminderDue(due) => due.reminder.message == wanted.id,
                _ => false,
            })
            .await;
        let text = serde_json::to_string(&event).unwrap();
        assert!(!text.contains("board-minutes-9914") && !text.contains("erased-note-5520"));
        let EventV1::MessageReminderDue(due) = event else {
            unreachable!()
        };
        assert!(!due.reminder.available);
        assert!(due.message.is_none() && due.user.is_none());
    }
}

#[rocket::async_test]
async fn old_delivered_reminders_are_removed_and_waiting_ones_stay() {
    overwrite_config(|config| config.features.saved_reminders.keep_delivered_days = 30).await;
    let s = setup().await;
    let m = message(&s, &s.open, "old").await;
    let now = now_ms();
    assert_eq!(remind(&s, &s.session, &m.id, now + MINUTE).await.0, Status::Ok);
    let (_, waiting) = remind(&s, &s.session, &m.id, now + 200 * DAY).await;

    let at = now + 2 * MINUTE;
    assert_eq!(deliver_due_reminders(&s.harness.db, at).await.unwrap().delivered, 1);
    // 29 days later it is still listed
    let report = deliver_due_reminders(&s.harness.db, at + 29 * DAY).await.unwrap();
    assert_eq!(report.removed, 0);
    let report = deliver_due_reminders(&s.harness.db, at + 31 * DAY).await.unwrap();
    assert_eq!(report.removed, 1);

    let left = s.harness.db.fetch_reminders(&s.me.id).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, waiting["_id"].as_str().unwrap());
    assert!(left[0].delivered_at.is_none());
}

#[rocket::async_test]
async fn deleting_the_account_removes_the_list_and_the_reminders() {
    let s = setup().await;
    let (mut account, session, user) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &user, None)
        .await
        .unwrap();
    let m = message(&s, &s.open, "keep").await;
    assert_eq!(save(&s, &session, &m).await.0, Status::Ok);
    assert_eq!(remind(&s, &session, &m.id, now_ms() + DAY).await.0, Status::Ok);
    // someone else's stay
    assert_eq!(save(&s, &s.session, &m).await.0, Status::Ok);

    account.mark_deleted(&s.harness.db).await.expect("deleted");

    assert!(s.harness.db.fetch_saved_messages(&user.id).await.unwrap().is_empty());
    assert!(s.harness.db.fetch_reminders(&user.id).await.unwrap().is_empty());
    assert_eq!(s.harness.db.fetch_saved_messages(&s.me.id).await.unwrap().len(), 1);
}
