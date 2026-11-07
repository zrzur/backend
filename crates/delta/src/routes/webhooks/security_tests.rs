//! P0 security of webhooks: what a token can do, who sees it, and what a
//! webhook message may not do that a person's message may not do either.

use revolt_database::events::client::EventV1;
use revolt_database::{sha256_hex, PartialServer, PartialWebhook, Webhook};
use revolt_models::v0;
use revolt_permissions::ChannelPermission;
use rocket::http::Status;
use serde_json::{json, Value};

use super::test_support::*;
use crate::util::test::{PubSubTestHelper, TestHarness};

fn everyone_flag(message: &Value) -> bool {
    message["flags"].as_u64().unwrap_or(0) & (1 << 2) != 0
}

fn online_flag(message: &Value) -> bool {
    message["flags"].as_u64().unwrap_or(0) & (1 << 3) != 0
}

#[rocket::async_test]
async fn read_only_member_never_sees_a_token() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    assert_eq!(post(&harness, &id, &token, json!({ "content": "hi" })).await.status, Status::Ok);
    let (session, _) = member(&harness, &scene).await;

    let channel = &scene.channel;
    let server = &scene.server.id;
    for path in [
        format!("/channels/{channel}/webhooks"),
        format!("/webhooks/{id}"),
        format!("/servers/{server}/webhooks"),
    ] {
        let reply = call(&harness, Get, &path, Some(&session), None, None).await;
        assert_eq!(reply.status, Status::Forbidden, "{path}: {}", reply.text);
        assert!(!reply.text.contains(&token), "{path}");
    }

    for path in [
        format!("/channels/{channel}"),
        format!("/channels/{channel}/messages"),
        format!("/servers/{server}?include_channels=true"),
    ] {
        let reply = call(&harness, Get, &path, Some(&session), None, None).await;
        assert_eq!((path.as_str(), reply.status), (path.as_str(), Status::Ok));
        assert!(!reply.text.contains(&token), "{path}");
    }
}

#[rocket::async_test]
async fn managers_see_the_token_only_when_it_is_made() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let session = Some(&scene.session);

    let list = call(&harness, Get, &format!("/channels/{}/webhooks", scene.channel), session, None, None).await;
    assert_eq!(list.status, Status::Ok);
    assert!(!list.text.contains(&token));

    let one = call(&harness, Get, &format!("/webhooks/{id}"), session, None, None).await;
    assert_eq!(one.status, Status::Ok);
    assert!(!one.text.contains(&token));

    let edit = call(&harness, Patch, &format!("/webhooks/{id}"), session, Some(json!({ "name": "CI" })), None).await;
    assert_eq!(edit.status, Status::Ok, "{}", edit.text);
    assert!(!edit.text.contains(&token));

    let by_token = call(&harness, Get, &format!("/webhooks/{id}/{token}"), None, None, None).await;
    assert_eq!(by_token.status, Status::Ok);
    assert!(!by_token.text.contains(&token));
    assert!(by_token.body.get("creator_id").is_none(), "{}", by_token.text);

    let edited = call(&harness, Patch, &format!("/webhooks/{id}/{token}"), None, Some(json!({ "name": "Build" })), None).await;
    assert_eq!(edited.status, Status::Ok, "{}", edited.text);
    assert!(!edited.text.contains(&token));
    assert!(edited.body.get("creator_id").is_none(), "{}", edited.text);
}

#[rocket::async_test]
async fn gateway_events_carry_no_token() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let mut events = PubSubTestHelper::new(&scene.channel).await;

    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let created = events
        .wait_for_event(|event| matches!(event, EventV1::WebhookCreate(_)))
        .await;
    assert!(!serde_json::to_string(&created).unwrap().contains(&token));

    let edited = call(&harness, Patch, &format!("/webhooks/{id}"), Some(&scene.session), Some(json!({ "name": "CI" })), None).await;
    assert_eq!(edited.status, Status::Ok);
    let updated = events
        .wait_for_event(|event| matches!(event, EventV1::WebhookUpdate { .. }))
        .await;
    assert!(!serde_json::to_string(&updated).unwrap().contains(&token));

    let sent = post(&harness, &id, &token, json!({ "content": "hi" })).await;
    assert_eq!(sent.status, Status::Ok);
    let message = events
        .wait_for_event(|event| matches!(event, EventV1::Message(_)))
        .await;
    assert!(!serde_json::to_string(&message).unwrap().contains(&token));
}

#[rocket::async_test]
async fn tokens_are_long_prefixed_and_stored_hashed() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    let secret = token.strip_prefix("zrzur_wh_").expect("prefix");
    assert_eq!(secret.len(), 43, "256 bits as 6-bit characters");

    let stored = harness.db.fetch_webhook(&id).await.unwrap();
    assert_eq!(stored.token, None);
    assert_eq!(stored.token_hash, Some(sha256_hex(token.as_bytes())));
}

#[rocket::async_test]
async fn webhooks_from_before_hashing_keep_working() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let old = Webhook {
        id: ulid::Ulid::new().to_string(),
        name: "Old".to_string(),
        creator_id: scene.user.id.clone(),
        channel_id: scene.channel.clone(),
        permissions: *revolt_permissions::DEFAULT_WEBHOOK_PERMISSIONS,
        token: Some("x".repeat(64)),
        ..Default::default()
    };
    harness.db.insert_webhook(&old).await.unwrap();

    let reply = post(&harness, &old.id, &"x".repeat(64), json!({ "content": "still here" })).await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    let wrong = post(&harness, &old.id, &"y".repeat(64), json!({ "content": "no" })).await;
    assert_eq!(wrong.status, Status::Unauthorized);

    // the plain token never goes out again, even to a manager
    let list = call(&harness, Get, &format!("/channels/{}/webhooks", scene.channel), Some(&scene.session), None, None).await;
    assert!(!list.text.contains(&"x".repeat(64)));
}

#[rocket::async_test]
async fn error_bodies_have_no_token_and_no_source_path() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    let bad = post(&harness, &id, "zrzur_wh_wrong", json!({ "content": "x" })).await;
    assert_eq!(bad.status, Status::Unauthorized);
    assert!(!bad.text.contains("zrzur_wh_wrong"));
    assert!(bad.body.get("location").is_none(), "{}", bad.text);

    let empty = post(&harness, &id, &token, json!({})).await;
    assert_eq!(empty.status, Status::UnprocessableEntity);
    assert!(!empty.text.contains(&token));
    assert!(empty.body.get("location").is_none(), "{}", empty.text);
}

#[rocket::async_test]
async fn regenerating_stops_the_old_token_at_once() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let (session, _) = member(&harness, &scene).await;

    let refused = call(&harness, Post, &format!("/webhooks/{id}/token"), Some(&session), Some(json!({})), None).await;
    assert_eq!(refused.status, Status::Forbidden);

    let reply = call(&harness, Post, &format!("/webhooks/{id}/token"), Some(&scene.session), Some(json!({})), None).await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    let fresh = reply.body["token"].as_str().expect("new token").to_string();
    assert!(fresh.starts_with("zrzur_wh_"));
    assert_ne!(fresh, token);

    assert_eq!(post(&harness, &id, &token, json!({ "content": "x" })).await.status, Status::Unauthorized);
    assert_eq!(post(&harness, &id, &fresh, json!({ "content": "y" })).await.status, Status::Ok);
}

#[rocket::async_test]
async fn a_token_cannot_raise_the_webhooks_permissions() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let before = harness.db.fetch_webhook(&id).await.unwrap().permissions;

    let reply = call(
        &harness,
        Patch,
        &format!("/webhooks/{id}/{token}"),
        None,
        Some(json!({ "name": "New", "permissions": u64::MAX >> 12 })),
        None,
    )
    .await;
    assert_eq!(reply.status, Status::Forbidden, "{}", reply.text);
    assert_eq!(harness.db.fetch_webhook(&id).await.unwrap().permissions, before);
}

/// Give a webhook one more permission, as a manager would
async fn grant(harness: &TestHarness, id: &str, extra: ChannelPermission) {
    let mut webhook = harness.db.fetch_webhook(id).await.unwrap();
    let permissions = webhook.permissions | extra as u64;
    webhook
        .update(
            &harness.db,
            PartialWebhook { permissions: Some(permissions), ..Default::default() },
            vec![],
        )
        .await
        .unwrap();
}

#[rocket::async_test]
async fn default_webhooks_ping_nobody_in_bulk() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let role = harness.new_role(&scene.server, 1, None).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    let reply = post(&harness, &id, &token, json!({ "content": format!("@everyone @online <%{}>", role.id) })).await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    assert!(!everyone_flag(&reply.body), "{}", reply.text);
    assert!(!online_flag(&reply.body), "{}", reply.text);
    assert!(reply.body.get("role_mentions").is_none(), "{}", reply.text);
}

#[rocket::async_test]
async fn mass_pings_need_the_permission_and_the_request() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let role = harness.new_role(&scene.server, 1, None).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    grant(&harness, &id, ChannelPermission::MentionEveryone).await;
    grant(&harness, &id, ChannelPermission::MentionRoles).await;

    let allowed = post(&harness, &id, &token, json!({ "content": "@everyone" })).await;
    assert!(everyone_flag(&allowed.body), "{}", allowed.text);

    let silenced = post(
        &harness,
        &id,
        &token,
        json!({ "content": format!("@everyone <%{}>", role.id), "allowed_mentions": { "parse": [] } }),
    )
    .await;
    assert_eq!(silenced.status, Status::Ok, "{}", silenced.text);
    assert!(!everyone_flag(&silenced.body), "{}", silenced.text);
    assert!(silenced.body.get("role_mentions").is_none(), "{}", silenced.text);

    let roles_only = post(
        &harness,
        &id,
        &token,
        json!({ "content": format!("@everyone <%{}>", role.id), "allowed_mentions": { "parse": ["roles"] } }),
    )
    .await;
    assert!(!everyone_flag(&roles_only.body), "{}", roles_only.text);
    assert_eq!(roles_only.body["role_mentions"], json!([role.id]));
}

#[rocket::async_test]
async fn an_edit_cannot_add_a_ping() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let sent = post(&harness, &id, &token, json!({ "content": "quiet" })).await;
    let message = sent.body["_id"].as_str().unwrap();

    let reply = call(
        &harness,
        Patch,
        &format!("/webhooks/{id}/{token}/{message}"),
        None,
        Some(json!({ "content": "@everyone" })),
        None,
    )
    .await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    assert!(!everyone_flag(&reply.body), "{}", reply.text);
}

#[rocket::async_test]
async fn names_are_checked_everywhere() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let create = format!("/channels/{}/webhooks", scene.channel);
    let session = Some(&scene.session);

    let refused = [
        "".to_string(),
        "x".repeat(81),
        "Zrzur".to_string(),
        "زرزور".to_string(),
        "ZRZUR Support".to_string(),
        "System".to_string(),
        "admin".to_string(),
        "ⓩⓡⓩⓤⓡ".to_string(),
        "Zr\u{200B}zur".to_string(),
        "Build\u{202E}gnp".to_string(),
        "a\u{2066}b".to_string(),
        "a\u{200D}b".to_string(),
    ];
    for name in &refused {
        for (method, path, auth) in [
            (Post, create.clone(), session),
            (Patch, format!("/webhooks/{id}"), session),
            (Patch, format!("/webhooks/{id}/{token}"), None),
        ] {
            let reply = call(&harness, method, &path, auth, Some(json!({ "name": name })), None).await;
            assert!(reply.status.class().is_client_error(), "{path} {name:?}: {}", reply.text);
        }
        if !name.is_empty() {
            let reply = post(&harness, &id, &token, json!({ "content": "x", "username": name })).await;
            assert!(reply.status.class().is_client_error(), "username {name:?}: {}", reply.text);
        }
        if !name.is_empty() && name.chars().count() <= 32 {
            let reply = post(&harness, &id, &token, json!({ "content": "x", "masquerade": { "name": name } })).await;
            assert!(reply.status.class().is_client_error(), "masquerade {name:?}: {}", reply.text);
        }
    }

    for name in ["x".repeat(80), "نشر تلقائي".to_string(), "GitHub".to_string(), "a".to_string()] {
        let reply = call(&harness, Post, &create, session, Some(json!({ "name": name })), None).await;
        assert_eq!(reply.status, Status::Ok, "{name:?}: {}", reply.text);
    }
}

#[rocket::async_test]
async fn the_webhook_mark_cannot_be_overridden() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let reply = post(
        &harness,
        &id,
        &token,
        json!({ "content": "x", "username": "Someone", "webhook": null, "author": scene.user.id }),
    )
    .await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    assert_eq!(reply.body["author"], json!(id));
    assert_eq!(reply.body["webhook"]["name"], json!("Deploys"));
}

#[rocket::async_test]
async fn nobody_posts_under_a_members_name() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let (_, someone) = member(&harness, &scene).await;
    let reply = post(&harness, &id, &token, json!({ "content": "x", "username": someone.username })).await;
    assert_eq!(reply.status.code, 400, "{}", reply.text);
    assert_eq!(reply.body["type"], json!("MasqueradeNameTaken"));
}

#[rocket::async_test]
async fn a_webhook_is_limited_to_five_in_two_seconds() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    for n in 0..5 {
        let reply = post(&harness, &id, &token, json!({ "content": format!("{n}") })).await;
        assert_eq!(reply.status, Status::Ok, "message {n}: {}", reply.text);
        assert_eq!(reply.header("X-RateLimit-Limit"), Some("5"), "message {n}");
    }
    let reply = post(&harness, &id, &token, json!({ "content": "sixth" })).await;
    assert_eq!(reply.status, Status::TooManyRequests, "{}", reply.text);
    assert_eq!(reply.body["type"], json!("RateLimited"));
    let wait = reply.body["retry_after"].as_u64().expect("retry_after");
    assert!(wait > 0 && wait <= 2000, "{wait}");
    assert_eq!(reply.header("X-RateLimit-Limit"), Some("5"));
    assert_eq!(reply.header("X-RateLimit-Remaining"), Some("0"));
    assert!(reply.header("X-RateLimit-Reset-After").is_some());
    assert!(reply.header("Retry-After").is_some());

    // another webhook in the channel has its own room
    let (other, other_token) = webhook(&harness, &scene, "Other").await;
    assert_eq!(post(&harness, &other, &other_token, json!({ "content": "x" })).await.status, Status::Ok);
}

#[rocket::async_test]
async fn five_webhooks_share_the_channel_limit() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let mut hooks = vec![];
    for n in 0..5 {
        hooks.push(webhook(&harness, &scene, &format!("Hook {n}")).await);
    }

    // 5 × 5 inside the per-webhook window, then the same again after it
    let mut delivered = 0;
    let mut refused = None;
    for round in 0..2 {
        for (id, token) in &hooks {
            for n in 0..5 {
                let reply = post(&harness, id, token, json!({ "content": format!("{round}.{n}") })).await;
                match reply.status.code {
                    200 => delivered += 1,
                    429 => {
                        refused.get_or_insert(reply);
                    }
                    code => panic!("{code}: {}", reply.text),
                }
            }
        }
        rocket::tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    }
    assert_eq!(delivered, 30);
    let refused = refused.expect("the channel limit was hit");
    assert_eq!(refused.header("X-RateLimit-Limit"), Some("30"));
    assert_eq!(refused.body["type"], json!("RateLimited"));
}

#[rocket::async_test]
async fn guessing_tokens_from_one_address_gets_throttled() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let path = format!("/webhooks/{id}/{}", "x".repeat(52));
    let good = format!("/webhooks/{id}/{token}");
    let guesser = format!("203.0.113.{}:4000", 1 + rand_octet());
    let bystander = format!("198.51.100.{}:4000", 1 + rand_octet());

    let mut refused = 0;
    for _ in 0..40 {
        let reply = call(&harness, Post, &path, None, Some(json!({ "content": "x" })), Some(&guesser)).await;
        match reply.status.code {
            401 => {}
            429 => {
                refused += 1;
                assert!(reply.body["retry_after"].as_u64().is_some(), "{}", reply.text);
                assert!(reply.header("Retry-After").is_some());
            }
            code => panic!("{code}"),
        }
    }
    assert!(refused > 0, "no throttle after 40 wrong tokens");

    // a right token from that address waits too, others are not affected
    let blocked = call(&harness, Post, &good, None, Some(json!({ "content": "x" })), Some(&guesser)).await;
    assert_eq!(blocked.status, Status::TooManyRequests);
    let other = call(&harness, Post, &good, None, Some(json!({ "content": "x" })), Some(&bystander)).await;
    assert!(other.status.class().is_success(), "{}", other.text);
}

fn rand_octet() -> u8 {
    (ulid::Ulid::new().random() % 250) as u8
}

async fn automod(harness: &TestHarness, scene: &mut Scene, trigger: Value) {
    let rule: v0::AutoModRule = serde_json::from_value(json!({
        "id": "rule",
        "name": "House rules",
        "enabled": true,
        "trigger": trigger,
        "actions": { "block": true },
    }))
    .unwrap();
    scene
        .server
        .update(
            &harness.db,
            PartialServer { automod: Some(vec![rule]), ..Default::default() },
            vec![],
        )
        .await
        .unwrap();
}

#[rocket::async_test]
async fn automod_blocks_webhooks_like_members() {
    let harness = TestHarness::new().await;
    let mut scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    automod(&harness, &mut scene, json!({ "type": "Keywords", "words": ["forbidden"] })).await;

    let clean = post(&harness, &id, &token, json!({ "content": "fine" })).await;
    assert_eq!(clean.status, Status::Ok, "{}", clean.text);
    let message = clean.body["_id"].as_str().unwrap();

    for body in [
        json!({ "content": "a forbidden word" }),
        json!({ "embeds": [{ "description": "a forbidden word" }] }),
        json!({ "embeds": [{ "title": "t", "fields": [{ "name": "n", "value": "forbidden" }] }] }),
    ] {
        let reply = post(&harness, &id, &token, body.clone()).await;
        assert_eq!(reply.status, Status::Forbidden, "{body}: {}", reply.text);
        assert_eq!(reply.body["type"], json!("AutoModBlocked"));
    }

    let edit = call(
        &harness,
        Patch,
        &format!("/webhooks/{id}/{token}/{message}"),
        None,
        Some(json!({ "content": "forbidden" })),
        None,
    )
    .await;
    assert_eq!(edit.status, Status::Forbidden, "{}", edit.text);
}

#[rocket::async_test]
async fn automod_link_rules_cover_webhooks() {
    let harness = TestHarness::new().await;
    let mut scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    automod(
        &harness,
        &mut scene,
        json!({ "type": "LinkSpam", "allowed_domains": ["zrzur.com"] }),
    )
    .await;
    let ok = post(&harness, &id, &token, json!({ "content": "https://zrzur.com/a" })).await;
    assert_eq!(ok.status, Status::Ok, "{}", ok.text);
    let bad = post(&harness, &id, &token, json!({ "content": "https://evil.example/a" })).await;
    assert_eq!(bad.status, Status::Forbidden, "{}", bad.text);
}

#[rocket::async_test]
async fn moderators_delete_webhook_messages() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let sent = post(&harness, &id, &token, json!({ "content": "spam" })).await;
    let message = sent.body["_id"].as_str().unwrap();

    let reply = call(
        &harness,
        Delete,
        &format!("/channels/{}/messages/{message}", scene.channel),
        Some(&scene.session),
        None,
        None,
    )
    .await;
    assert_eq!(reply.status, Status::NoContent, "{}", reply.text);
    assert!(harness.db.fetch_message(message).await.is_err());
}

#[rocket::async_test]
async fn avatar_urls_are_checked_and_never_fetched_here() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    for url in [
        "http://127.0.0.1/a.png",
        "https://127.0.0.1/a.png",
        "https://169.254.169.254/latest/meta-data/",
        "https://[::1]/a.png",
        "https://localhost/a.png",
        "https://10.0.0.1/a.png",
        "file:///etc/passwd",
        "data:image/png;base64,AAAA",
        "javascript:alert(1)",
        "ftp://example.com/a.png",
        "//example.com/a.png",
        "https://user:pass@example.com/a.png",
    ] {
        let reply = post(&harness, &id, &token, json!({ "content": "x", "avatar_url": url })).await;
        assert_eq!(reply.status, Status::BadRequest, "{url}: {}", reply.text);
    }

    let long = format!("https://example.com/{}", "a".repeat(2100));
    assert_eq!(post(&harness, &id, &token, json!({ "content": "x", "avatar_url": long })).await.status, Status::BadRequest);

    let reply = post(
        &harness,
        &id,
        &token,
        json!({ "content": "x", "username": "Bot", "avatar_url": "https://example.com/a.png" }),
    )
    .await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    // stored as given: clients load it through the media proxy
    assert_eq!(reply.body["masquerade"]["avatar"], json!("https://example.com/a.png"));
    assert_eq!(reply.body["masquerade"]["name"], json!("Bot"));
}
