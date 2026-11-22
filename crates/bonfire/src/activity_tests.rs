//! Rich presence on real sockets: `SetActivity` from a person's own
//! session, who receives it, the rate limit, checks, invisibility, and
//! clearing when a session or the last session goes.

use std::time::Duration as StdDuration;

use async_tungstenite::tungstenite::Message;
use futures::{SinkExt, StreamExt};
use revolt_database::{
    events::client::EventV1, iso8601_timestamp::Timestamp, util::activity, Account, AccountAge,
    Database, DatabaseInfo, PartialUser, RelationshipStatus, Session, User, AMQP,
};
use revolt_models::v0::{self, ActivitySource};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::compat::TokioAsyncReadCompatExt;

use super::client_with_limits;
use crate::resume::ResumeLimits;

const NEVER: StdDuration = StdDuration::from_secs(3600);
const PATIENCE: StdDuration = StdDuration::from_secs(10);

type Socket = async_tungstenite::WebSocketStream<tokio_util::compat::Compat<TcpStream>>;

async fn database() -> &'static Database {
    AMQP::new_auto().await;
    let db = DatabaseInfo::Test(format!("bonfire_activity_{}", ulid::Ulid::new()))
        .connect()
        .await
        .expect("database");
    Box::leak(Box::new(db))
}

async fn person(db: &Database, birth_month: &str) -> (Session, User) {
    let age = AccountAge {
        birth_month: birth_month.to_owned(),
        declared_at: Timestamp::now_utc(),
        source: v0::AgeSource::SelfDeclared,
        raised: false,
        changes: vec![],
    };
    let account = Account::new_with_age(
        db,
        format!("{}@zrzur.cc", ulid::Ulid::new()),
        "password_insecure".to_owned(),
        false,
        Some(age),
    )
    .await
    .unwrap();
    let name = format!("a{}", ulid::Ulid::new().to_string().to_lowercase());
    let user = User::create(db, name[..20].to_owned(), account.id.clone(), None)
        .await
        .unwrap();
    (account.create_session(db, "test".to_owned()).await.unwrap(), user)
}

/// An adult who turned sharing on (it is off until chosen)
async fn adult(db: &Database) -> (Session, User) {
    let (session, user) = person(db, "1990-01").await;
    share(db, &user).await;
    (session, user)
}

async fn share(db: &Database, user: &User) {
    db.update_user(
        &user.id,
        &PartialUser {
            privacy: Some(v0::UserPrivacy {
                share_activity: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        vec![],
    )
    .await
    .unwrap();
}

async fn friends(db: &Database, a: &User, b: &User) {
    let mut a = db.fetch_user(&a.id).await.unwrap();
    let mut b = db.fetch_user(&b.id).await.unwrap();
    #[allow(clippy::disallowed_methods)]
    a.apply_relationship(db, &mut b, RelationshipStatus::Friend, RelationshipStatus::Friend)
        .await
        .unwrap();
}

async fn connect(db: &'static Database) -> Socket {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        client_with_limits(db, stream, peer, NEVER, ResumeLimits::default()).await;
    });
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let url = format!("ws://127.0.0.1:{port}/?format=json&version=1");
    async_tungstenite::client_async(url, stream.compat())
        .await
        .unwrap()
        .0
}

async fn next_event(socket: &mut Socket, patience: StdDuration) -> Option<Value> {
    loop {
        let message = tokio::time::timeout(patience, socket.next()).await.ok()??;
        match message {
            Ok(Message::Text(text)) => return serde_json::from_str(&text).ok(),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

async fn send(socket: &mut Socket, message: Value) {
    socket.send(Message::Text(message.to_string())).await.unwrap();
}

/// Wait until the session follows its topics (an event on its private
/// topic gets through)
async fn until_following(socket: &mut Socket, user: &str) {
    for n in 0..50 {
        EventV1::ChannelAck {
            id: "channel".to_owned(),
            user: user.to_owned(),
            message_id: format!("probe{n}"),
        }
        .private(user.to_owned())
        .await;
        if let Some(event) = next_event(socket, StdDuration::from_millis(200)).await {
            if event["type"] == "ChannelAck" {
                while next_event(socket, StdDuration::from_millis(300)).await.is_some() {}
                return;
            }
        }
    }
    panic!("the session never followed its topics");
}

/// Signed in and following; gives back the Ready event too
async fn signed_in(db: &'static Database, session: &Session) -> (Socket, Value) {
    let mut socket = connect(db).await;
    send(&mut socket, json!({ "type": "Authenticate", "token": session.token })).await;
    let ready = loop {
        let event = next_event(&mut socket, PATIENCE).await.expect("Ready");
        if event["type"] == "Ready" {
            break event;
        }
    };
    until_following(&mut socket, &session.user_id).await;
    (socket, ready)
}

/// The next update of `user`'s activities, if one comes in `patience`
async fn activities_of(socket: &mut Socket, user: &str, patience: StdDuration) -> Option<Value> {
    let until = tokio::time::Instant::now() + patience;
    loop {
        let left = until.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        let event = next_event(socket, left).await?;
        if event["type"] == "UserUpdate" && event["id"] == user && event["data"].get("activities").is_some() {
            return Some(event["data"]["activities"].clone());
        }
    }
}

/// The next refusal: its own event, never the connection's `Error`
async fn error_of(socket: &mut Socket) -> Value {
    loop {
        let event = next_event(socket, PATIENCE).await.expect("a refusal");
        assert_ne!(event["type"], "Error", "a refusal is not an error of the connection");
        if event["type"] == "ActivityRefused" {
            return event["error"].clone();
        }
    }
}

/// The socket still answers a ping
async fn still_open(socket: &mut Socket) {
    send(socket, json!({ "type": "Ping", "data": 7 })).await;
    loop {
        let event = next_event(socket, PATIENCE).await.expect("the socket stayed open");
        if event["type"] == "Pong" {
            return;
        }
    }
}

fn set(name: &str) -> Value {
    json!({
        "type": "SetActivity",
        "source": "Rpc",
        "activity": {
            "type": "Playing",
            "name": name,
            "details": "Level 3",
            "started_at": "2026-11-22T10:00:00Z",
            "buttons": [{ "label": "Join", "url": "https://example.com/join" }]
        }
    })
}

#[tokio::test]
async fn a_friend_sees_the_activity_and_a_new_session_gets_it_in_ready() {
    let db = database().await;
    let (a_session, a) = adult(db).await;
    let (b_session, b) = adult(db).await;
    friends(db, &a, &b).await;
    let (mut a_socket, _) = signed_in(db, &a_session).await;
    let (mut b_socket, _) = signed_in(db, &b_session).await;

    send(&mut a_socket, set("Star Kebab")).await;
    let seen = activities_of(&mut b_socket, &a.id, PATIENCE).await.expect("the friend hears it");
    assert_eq!(seen[0]["name"], "Star Kebab");
    assert_eq!(seen[0]["source"], "Rpc");
    assert_eq!(seen[0]["details"], "Level 3");
    assert_eq!(seen[0]["buttons"][0]["url"], "https://example.com/join");
    let own = activities_of(&mut a_socket, &a.id, PATIENCE).await.expect("your own sessions hear it");
    assert_eq!(own[0]["name"], "Star Kebab");

    // a session that starts later finds it in Ready
    let (_, ready) = signed_in(db, &b_session).await;
    let listed = ready["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|user| user["_id"] == a.id.as_str())
        .expect("the friend is in Ready")
        .clone();
    assert_eq!(listed["activities"][0]["name"], "Star Kebab");

    // clearing reaches the friend
    send(&mut a_socket, json!({ "type": "SetActivity", "source": "Rpc", "activity": null })).await;
    let cleared = activities_of(&mut b_socket, &a.id, PATIENCE).await.expect("cleared");
    assert_eq!(cleared, json!([]));
}

#[tokio::test]
async fn the_sixth_change_in_twenty_seconds_is_refused() {
    let db = database().await;
    let (session, _) = adult(db).await;
    let (mut socket, _) = signed_in(db, &session).await;
    for n in 0..5 {
        send(&mut socket, set(&format!("Game {n}"))).await;
    }
    send(&mut socket, set("Game 5")).await;
    let error = error_of(&mut socket).await;
    assert_eq!(error["type"], "RateLimited");
    assert!(error["retry_after"].as_u64().unwrap() > 0);
    let kept = activity::list(&session.user_id).await;
    assert_eq!(kept[0].name, "Game 4", "the refused one changed nothing");
}

#[tokio::test]
async fn bad_activities_are_refused_with_the_reason() {
    let db = database().await;
    let (session, user) = adult(db).await;
    let (mut socket, _) = signed_in(db, &session).await;

    send(&mut socket, json!({ "type": "SetActivity", "source": "Rpc", "activity": { "name": "" } })).await;
    assert_eq!(error_of(&mut socket).await["type"], "FailedValidation");

    send(
        &mut socket,
        json!({ "type": "SetActivity", "source": "Rpc", "activity": {
            "name": "Game", "buttons": [{ "label": "x", "url": "http://plain.example" }]
        } }),
    )
    .await;
    assert_eq!(error_of(&mut socket).await["type"], "FailedValidation");

    send(&mut socket, json!({ "type": "SetActivity", "source": "Steam", "activity": { "name": "Game" } })).await;
    assert_eq!(error_of(&mut socket).await["type"], "FailedValidation", "Steam is the server's");

    // hidden characters: a right-to-left override, a zero-width space, a line separator
    for hidden in ["Game\u{202E}gnp.exe", "Ga\u{200B}me", "Line\u{2028}break"] {
        send(&mut socket, json!({ "type": "SetActivity", "source": "Rpc", "activity": { "name": hidden } })).await;
        assert_eq!(error_of(&mut socket).await["type"], "FailedValidation", "{hidden:?}");
    }
    // refusals never end the session
    still_open(&mut socket).await;

    assert!(activity::list(&user.id).await.is_empty());
}

#[tokio::test]
async fn a_session_takes_its_own_activity_when_it_goes_and_the_last_takes_all() {
    let db = database().await;
    let (a_session, a) = adult(db).await;
    let (b_session, b) = adult(db).await;
    friends(db, &a, &b).await;
    // a second login of the same person (another device)
    let second = db
        .fetch_account(&a.id)
        .await
        .unwrap()
        .create_session(db, "phone".to_owned())
        .await
        .unwrap();
    let (mut desktop, _) = signed_in(db, &a_session).await;
    let (mut phone, _) = signed_in(db, &second).await;
    let (mut b_socket, _) = signed_in(db, &b_session).await;

    send(&mut desktop, set("Star Kebab")).await;
    activities_of(&mut b_socket, &a.id, PATIENCE).await.expect("set");
    send(
        &mut phone,
        json!({ "type": "SetActivity", "source": "Desktop", "activity": { "type": "Using", "name": "Code" } }),
    )
    .await;
    let both = activities_of(&mut b_socket, &a.id, PATIENCE).await.expect("both");
    assert_eq!(both.as_array().unwrap().len(), 2);
    assert_eq!(both[0]["source"], "Rpc", "the app's own report comes first");

    // the desktop goes: only what it set goes with it
    desktop.close(None).await.ok();
    drop(desktop);
    let left = activities_of(&mut b_socket, &a.id, PATIENCE).await.expect("one went");
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["name"], "Code");

    // the last one: offline, and nothing left
    phone.close(None).await.ok();
    drop(phone);
    let gone = activities_of(&mut b_socket, &a.id, PATIENCE).await.expect("offline");
    assert_eq!(gone, json!([]));
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    assert!(activity::list(&a.id).await.is_empty());
}

#[tokio::test]
async fn an_invisible_person_shows_others_nothing() {
    let db = database().await;
    let (a_session, a) = adult(db).await;
    let (b_session, b) = adult(db).await;
    friends(db, &a, &b).await;
    db.update_user(
        &a.id,
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
    let (mut a_socket, _) = signed_in(db, &a_session).await;
    let (mut b_socket, _) = signed_in(db, &b_session).await;

    send(&mut a_socket, set("Secret Game")).await;
    if let Some(seen) = activities_of(&mut b_socket, &a.id, StdDuration::from_secs(3)).await {
        assert_eq!(seen, json!([]), "others get nothing to see");
    }
    // their own sessions still see it
    let mut own = None;
    for _ in 0..2 {
        if let Some(list) = activities_of(&mut a_socket, &a.id, PATIENCE).await {
            if list.as_array().is_some_and(|l| !l.is_empty()) {
                own = Some(list);
                break;
            }
        }
    }
    assert_eq!(own.expect("own session")[0]["name"], "Secret Game");
}

#[tokio::test]
async fn nothing_is_shared_until_they_choose() {
    let db = database().await;
    let now = Timestamp::now_utc();
    let teen_birth = format!("{}-{:02}", now.year() - 16, u8::from(now.month()));
    // under 18, and an adult: both off until chosen (legal RP-01)
    for birth in [teen_birth.as_str(), "1990-01"] {
        let (session, user) = person(db, birth).await;
        let (mut socket, _) = signed_in(db, &session).await;
        send(&mut socket, set("Star Kebab")).await;
        // no refusal: the app may send, the server keeps nothing
        assert!(activities_of(&mut socket, &user.id, StdDuration::from_secs(2)).await.is_none());
        assert!(activity::list(&user.id).await.is_empty());
    }
}

#[tokio::test]
async fn under_18_who_share_reach_only_their_friends() {
    let db = database().await;
    let now = Timestamp::now_utc();
    let teen_birth = format!("{}-{:02}", now.year() - 16, u8::from(now.month()));
    let (teen_session, teen) = person(db, &teen_birth).await;
    share(db, &teen).await;
    let (friend_session, friend) = adult(db).await;
    friends(db, &teen, &friend).await;
    let (mut teen_socket, _) = signed_in(db, &teen_session).await;
    let (mut friend_socket, _) = signed_in(db, &friend_session).await;

    send(&mut teen_socket, set("Star Kebab")).await;
    let seen = activities_of(&mut friend_socket, &teen.id, PATIENCE).await.expect("a friend hears it");
    assert_eq!(seen[0]["name"], "Star Kebab");
    let own = activities_of(&mut teen_socket, &teen.id, PATIENCE).await.expect("their own sessions hear it");
    assert_eq!(own[0]["name"], "Star Kebab");
}

#[tokio::test]
async fn a_viewer_who_blocked_someone_gets_no_activity_events() {
    let db = database().await;
    let (a_session, a) = adult(db).await;
    let (c_session, c) = adult(db).await;
    friends(db, &a, &c).await;
    let (mut a_socket, _) = signed_in(db, &a_session).await;
    let (mut c_socket, _) = signed_in(db, &c_session).await;
    {
        // c blocks a after both sessions started
        let mut c_user = db.fetch_user(&c.id).await.unwrap();
        let mut a_user = db.fetch_user(&a.id).await.unwrap();
        #[allow(clippy::disallowed_methods)]
        c_user
            .apply_relationship(db, &mut a_user, RelationshipStatus::Blocked, RelationshipStatus::BlockedOther)
            .await
            .unwrap();
    }
    tokio::time::sleep(StdDuration::from_millis(500)).await;
    send(&mut a_socket, set("Star Kebab")).await;
    if let Some(seen) = activities_of(&mut c_socket, &a.id, StdDuration::from_secs(2)).await {
        panic!("someone who blocked a still saw {seen}");
    }
}

#[tokio::test]
async fn a_bot_session_cannot_set_a_person_activity() {
    let db = database().await;
    let (_, user) = adult(db).await;
    let refused = activity::set_from_session(
        db,
        &user.id,
        "",
        ActivitySource::Rpc,
        Some(v0::DataActivity {
            name: "Game".into(),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(refused.error_type, revolt_result::ErrorType::IsBot));
}

#[tokio::test]
async fn a_blocked_viewer_gets_no_activity_events() {
    let db = database().await;
    let (a_session, a) = adult(db).await;
    let (b_session, b) = adult(db).await;
    let (c_session, c) = adult(db).await;
    friends(db, &a, &b).await;
    friends(db, &a, &c).await;
    let (mut a_socket, _) = signed_in(db, &a_session).await;
    // a blocks c after c's session started: c's cache still lists a
    let (mut c_socket, _) = signed_in(db, &c_session).await;
    let (mut b_socket, _) = signed_in(db, &b_session).await;
    {
        let mut a_user = db.fetch_user(&a.id).await.unwrap();
        let mut c_user = db.fetch_user(&c.id).await.unwrap();
        #[allow(clippy::disallowed_methods)]
        a_user
            .apply_relationship(db, &mut c_user, RelationshipStatus::Blocked, RelationshipStatus::BlockedOther)
            .await
            .unwrap();
    }
    tokio::time::sleep(StdDuration::from_millis(500)).await;
    send(&mut a_socket, set("Star Kebab")).await;
    assert!(activities_of(&mut b_socket, &a.id, PATIENCE).await.is_some(), "a friend sees it");
    if let Some(seen) = activities_of(&mut c_socket, &a.id, StdDuration::from_secs(2)).await {
        panic!("a blocked person saw {seen}");
    }
}
