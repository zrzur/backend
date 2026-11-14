//! Twitch's live/offline messages through the real route, signed as Twitch
//! signs them with the test secret from Revolt.test.toml.

use iso8601_timestamp::Timestamp;
use revolt_database::{util::linked_crypto::twitch_signature, LinkedAccount};
use rocket::http::{ContentType, Header, Status};

use crate::{routes::users::connections_oauth::now_ms, util::test::TestHarness};

const SECRET: &str = "test-eventsub-secret-0123456789";

fn now_text() -> String {
    Timestamp::now_utc().format().to_string()
}

async fn send(
    harness: &TestHarness,
    kind: &str,
    id: &str,
    timestamp: &str,
    body: &serde_json::Value,
    secret: &str,
) -> (Status, String) {
    let body = body.to_string();
    let signature = twitch_signature(secret, id, timestamp, body.as_bytes());
    let response = harness
        .client
        .post("/connections/twitch/events")
        .header(ContentType::JSON)
        .header(Header::new("Twitch-Eventsub-Message-Id", id.to_string()))
        .header(Header::new("Twitch-Eventsub-Message-Timestamp", timestamp.to_string()))
        .header(Header::new("Twitch-Eventsub-Message-Signature", signature))
        .header(Header::new("Twitch-Eventsub-Message-Type", kind.to_string()))
        .body(body)
        .dispatch()
        .await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

fn message(kind: &str, broadcaster: &str) -> serde_json::Value {
    serde_json::json!({
        "subscription": {
            "id": format!("sub-{kind}-{broadcaster}"),
            "type": kind,
            "condition": { "broadcaster_user_id": broadcaster },
        },
        "event": { "broadcaster_user_id": broadcaster, "type": "live" },
    })
}

async fn linked(harness: &TestHarness, broadcaster: &str) -> LinkedAccount {
    let (_, _, user) = harness.new_user().await;
    let account = LinkedAccount {
        id: LinkedAccount::key(&user.id, "twitch"),
        user_id: user.id.clone(),
        service: "twitch".into(),
        external_id: broadcaster.into(),
        display_name: "Streamer".into(),
        profile_url: "https://www.twitch.tv/streamer".into(),
        shown: true,
        linked_at: now_ms(),
        refreshed_at: now_ms(),
        live: Some(false),
        ..Default::default()
    };
    harness.db.save_linked_account(&account).await.unwrap();
    account
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", nanoid::nanoid!(12))
}

#[rocket::async_test]
async fn verification_answers_the_challenge_only_when_signed() {
    let harness = TestHarness::new().await;
    let body = serde_json::json!({
        "challenge": "pogchamp-kappa-360noscope-vohiyo",
        "subscription": { "id": "s1", "type": "stream.online" },
    });
    let (status, text) = send(&harness, "webhook_callback_verification", &unique("v"), &now_text(), &body, SECRET).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(text, "pogchamp-kappa-360noscope-vohiyo");

    let (status, text) = send(&harness, "webhook_callback_verification", &unique("v"), &now_text(), &body, "wrong-secret-000").await;
    assert_eq!(status, Status::Forbidden);
    assert!(!text.contains("pogchamp"));

    // no headers at all
    let response = harness.client.post("/connections/twitch/events").body("{}").dispatch().await;
    assert_eq!(response.status(), Status::Forbidden);
}

#[rocket::async_test]
async fn live_and_offline_set_the_flag_once() {
    let harness = TestHarness::new().await;
    let broadcaster = format!("{}1", rand::random::<u32>());
    let account = linked(&harness, &broadcaster).await;

    let id = unique("m");
    let (status, _) = send(&harness, "notification", &id, &now_text(), &message("stream.online", &broadcaster), SECRET).await;
    assert_eq!(status, Status::Ok);
    let stored = harness.db.fetch_linked_account(&account.user_id, "twitch").await.unwrap().unwrap();
    assert_eq!(stored.live, Some(true));
    assert_eq!(stored.live_title.as_deref(), Some(format!("Live title {broadcaster}").as_str()));

    // offline, then the same online message again: retries do nothing
    let (status, _) = send(&harness, "notification", &unique("m"), &now_text(), &message("stream.offline", &broadcaster), SECRET).await;
    assert_eq!(status, Status::Ok);
    let (status, _) = send(&harness, "notification", &id, &now_text(), &message("stream.online", &broadcaster), SECRET).await;
    assert_eq!(status, Status::Ok);
    let stored = harness.db.fetch_linked_account(&account.user_id, "twitch").await.unwrap().unwrap();
    assert_eq!(stored.live, Some(false));
    assert_eq!(stored.live_title, None);
}

#[rocket::async_test]
async fn old_or_changed_messages_are_refused() {
    let harness = TestHarness::new().await;
    let broadcaster = format!("{}1", rand::random::<u32>());
    let account = linked(&harness, &broadcaster).await;

    // 11 minutes old
    let old = Timestamp::UNIX_EPOCH
        .checked_add(iso8601_timestamp::Duration::milliseconds(now_ms() - 11 * 60 * 1000))
        .unwrap()
        .format()
        .to_string();
    let (status, _) = send(&harness, "notification", &unique("m"), &old, &message("stream.online", &broadcaster), SECRET).await;
    assert_eq!(status, Status::Forbidden);

    // signed for one body, sent with another
    let body = message("stream.online", &broadcaster).to_string();
    let id = unique("m");
    let timestamp = now_text();
    let signature = twitch_signature(SECRET, &id, &timestamp, body.as_bytes());
    let response = harness
        .client
        .post("/connections/twitch/events")
        .header(Header::new("Twitch-Eventsub-Message-Id", id))
        .header(Header::new("Twitch-Eventsub-Message-Timestamp", timestamp))
        .header(Header::new("Twitch-Eventsub-Message-Signature", signature))
        .header(Header::new("Twitch-Eventsub-Message-Type", "notification"))
        .body(body.replace("stream.online", "stream.offline"))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);

    let stored = harness.db.fetch_linked_account(&account.user_id, "twitch").await.unwrap().unwrap();
    assert_eq!(stored.live, Some(false));
}

#[rocket::async_test]
async fn removing_zrzur_at_twitch_unlinks_and_unknown_channels_are_dropped() {
    let harness = TestHarness::new().await;
    let broadcaster = format!("{}1", rand::random::<u32>());
    let account = linked(&harness, &broadcaster).await;

    let revoke = serde_json::json!({
        "subscription": { "id": "rev", "type": "user.authorization.revoke", "condition": {} },
        "event": { "client_id": "test-twitch-client", "user_id": broadcaster },
    });
    let (status, _) = send(&harness, "notification", &unique("m"), &now_text(), &revoke, SECRET).await;
    assert_eq!(status, Status::Ok);
    assert!(harness.db.fetch_linked_account(&account.user_id, "twitch").await.unwrap().is_none());

    // a channel nobody links: its subscription is ended
    let nobody = format!("{}1", rand::random::<u32>());
    let (status, _) = send(&harness, "notification", &unique("m"), &now_text(), &message("stream.online", &nobody), SECRET).await;
    assert_eq!(status, Status::Ok);
    assert!(crate::util::twitch::test_twitch::called(&format!("unsubscribe sub-stream.online-{nobody}")));
}

#[test]
fn twitch_times_with_any_fraction_parse() {
    use super::twitch_events::twitch_time_ms;
    let a = twitch_time_ms("2026-11-14T10:00:00.123456789Z").unwrap();
    let b = twitch_time_ms("2026-11-14T10:00:00Z").unwrap();
    assert_eq!(a, b);
    assert!(twitch_time_ms("yesterday").is_none());
}
