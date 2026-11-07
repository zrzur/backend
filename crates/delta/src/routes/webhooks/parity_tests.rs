//! What a webhook can do: the shape of each route, limits one either side of
//! the line, threads, components, files.

use revolt_database::{hash_token, Webhook, MAX_CHANNEL_WEBHOOKS, MAX_SERVER_WEBHOOKS};
use revolt_permissions::ChannelPermission;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use super::test_support::*;
use crate::routes::channels::webhook_create::assert_webhooks_enabled;
use crate::util::test::TestHarness;

/// A channel in the scene's server
async fn channel(harness: &TestHarness, scene: &Scene, body: Value) -> String {
    let reply = call(
        harness,
        Post,
        &format!("/servers/{}/channels", scene.server.id),
        Some(&scene.session),
        Some(body),
        None,
    )
    .await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    reply.body["_id"].as_str().unwrap().to_string()
}

/// A forum with one tag, required
async fn forum(harness: &TestHarness, scene: &Scene) -> String {
    let forum = channel(harness, scene, json!({ "type": "Forum", "name": "help" })).await;
    let reply = call(
        harness,
        Patch,
        &format!("/channels/{forum}"),
        Some(&scene.session),
        Some(json!({ "forum_tags": [{ "id": "bug", "name": "Bug" }] })),
        None,
    )
    .await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    forum
}

async fn stored_webhook(
    harness: &TestHarness,
    channel: &str,
    creator: &str,
    permissions: u64,
) -> (String, String) {
    let (token, hash) = Webhook::new_token();
    let webhook = Webhook {
        id: ulid::Ulid::new().to_string(),
        name: "Stored".to_string(),
        creator_id: creator.to_string(),
        channel_id: channel.to_string(),
        permissions,
        token_hash: Some(hash),
        ..Default::default()
    };
    harness.db.insert_webhook(&webhook).await.unwrap();
    (webhook.id, token)
}

#[test]
fn a_switched_off_instance_refuses_to_create_webhooks() {
    assert!(assert_webhooks_enabled(true).is_ok());
    let error = assert_webhooks_enabled(false).unwrap_err();
    assert!(matches!(
        error.error_type,
        revolt_result::ErrorType::FeatureDisabled { .. }
    ));
}

#[rocket::async_test]
async fn execute_answers_204_unless_asked_to_wait() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let path = format!("/webhooks/{id}/{token}");

    let plain = call(&harness, Post, &path, None, Some(json!({ "content": "a" })), None).await;
    assert_eq!(plain.status, Status::NoContent, "{}", plain.text);
    assert!(plain.text.is_empty());

    let no = call(&harness, Post, &format!("{path}?wait=false"), None, Some(json!({ "content": "b" })), None).await;
    assert_eq!(no.status, Status::NoContent);

    let yes = call(&harness, Post, &format!("{path}?wait=true"), None, Some(json!({ "content": "c" })), None).await;
    assert_eq!(yes.status, Status::Ok);
    assert_eq!(yes.body["content"], json!("c"));
    assert_eq!(yes.body["channel"], json!(scene.channel));
}

#[rocket::async_test]
async fn execute_body_limits() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;

    for (body, ok) in [
        (json!({ "content": "x".repeat(2000) }), true),
        (json!({ "content": "x".repeat(2001) }), false),
        (json!({ "content": "ب".repeat(2000) }), true),
        (json!({ "content": "" }), false),
        (json!({}), false),
        (json!({ "username": "only a name" }), false),
        (json!({ "embeds": [] }), false),
        (json!({ "embeds": (0..10).map(|n| json!({ "title": format!("{n}") })).collect::<Vec<_>>() }), true),
        (json!({ "embeds": (0..11).map(|n| json!({ "title": format!("{n}") })).collect::<Vec<_>>() }), false),
        (json!({ "embeds": [{ "title": "x".repeat(256) }] }), true),
        (json!({ "embeds": [{ "title": "x".repeat(257) }] }), false),
        (json!({ "embeds": [{ "description": "x".repeat(4096) }] }), true),
        (json!({ "embeds": [{ "description": "x".repeat(4097) }] }), false),
        (json!({ "embeds": [{ "title": "t", "fields": (0..25).map(|n| json!({ "name": format!("{n}"), "value": "v" })).collect::<Vec<_>>() }] }), true),
        (json!({ "embeds": [{ "title": "t", "fields": (0..26).map(|n| json!({ "name": format!("{n}"), "value": "v" })).collect::<Vec<_>>() }] }), false),
        (json!({ "embeds": [{ "fields": [{ "name": "x".repeat(256), "value": "v" }] }] }), true),
        (json!({ "embeds": [{ "fields": [{ "name": "x".repeat(257), "value": "v" }] }] }), false),
        (json!({ "embeds": [{ "fields": [{ "name": "n", "value": "x".repeat(1024) }] }] }), true),
        (json!({ "embeds": [{ "fields": [{ "name": "n", "value": "x".repeat(1025) }] }] }), false),
        (json!({ "embeds": [{ "footer": { "text": "x".repeat(2048) } }] }), true),
        (json!({ "embeds": [{ "footer": { "text": "x".repeat(2049) } }] }), false),
        (json!({ "embeds": [{ "author": { "name": "x".repeat(256) } }] }), true),
        (json!({ "embeds": [{ "author": { "name": "x".repeat(257) } }] }), false),
        (json!({ "embeds": [{ "description": "x".repeat(3000) }, { "description": "x".repeat(3000) }] }), true),
        (json!({ "embeds": [{ "description": "x".repeat(3000) }, { "description": "x".repeat(3001) }] }), false),
    ] {
        // a webhook per case: the send limit is not what is tested here
        let (id, token) = webhook(&harness, &scene, "Deploys").await;
        let reply = post(&harness, &id, &token, body.clone()).await;
        assert_eq!(reply.status == Status::Ok, ok, "{}: {}", &body.to_string()[..60.min(body.to_string().len())], reply.text);
        // hooks are capped per channel
        harness.db.delete_webhook(&id).await.unwrap();
    }
}

#[rocket::async_test]
async fn a_body_written_for_another_platform_works_by_changing_the_host() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    let reply = post(
        &harness,
        &id,
        &token,
        json!({
            "content": "Deployed",
            "username": "Release Bot",
            "avatar_url": "https://example.com/bot.png",
            "tts": false,
            "embeds": [{
                "type": "rich",
                "title": "v1.2.3",
                "url": "https://example.com/r",
                "description": "Notes",
                "color": 14686559,
                "timestamp": "2026-11-04T10:00:00+00:00",
                "footer": { "text": "CI", "icon_url": "https://example.com/f.png" },
                "image": { "url": "https://example.com/i.png" },
                "thumbnail": { "url": "https://example.com/t.png" },
                "author": { "name": "ci", "url": "https://example.com", "icon_url": "https://example.com/a.png" },
                "fields": [{ "name": "Env", "value": "prod", "inline": true }],
            }],
            "allowed_mentions": { "parse": [] },
        }),
    )
    .await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    let embed = &reply.body["embeds"][0];
    assert_eq!(embed["type"], json!("Text"));
    assert_eq!(embed["colour"], json!("#E0195F"));
    assert_eq!(embed["image_url"], json!("https://example.com/i.png"));
    assert_eq!(embed["thumbnail_url"], json!("https://example.com/t.png"));
    assert_eq!(embed["fields"][0]["inline"], json!(true));
    assert_eq!(reply.body["masquerade"]["name"], json!("Release Bot"));
    assert_eq!(reply.body["webhook"]["name"], json!("Deploys"));
}

#[rocket::async_test]
async fn flags_mean_what_the_tools_mean() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    let silent = post(&harness, &id, &token, json!({ "content": "a", "flags": 4096 })).await;
    assert_eq!(silent.status, Status::Ok, "{}", silent.text);
    assert_eq!(silent.body["flags"], json!(2));

    // 4 hides previews; it must not turn into a mention of everyone
    let previews = post(&harness, &id, &token, json!({ "content": "b", "flags": 4 })).await;
    assert_eq!(previews.status, Status::Ok, "{}", previews.text);
    assert_eq!(previews.body["flags"].as_u64().unwrap_or(0) & 4, 0);
}

#[rocket::async_test]
async fn caps_per_channel_and_per_server_have_their_own_errors() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let create = format!("/channels/{}/webhooks", scene.channel);

    for n in 0..MAX_CHANNEL_WEBHOOKS {
        let reply = call(&harness, Post, &create, Some(&scene.session), Some(json!({ "name": format!("Hook {n}") })), None).await;
        assert_eq!(reply.status, Status::Ok, "{n}");
    }
    let reply = call(&harness, Post, &create, Some(&scene.session), Some(json!({ "name": "one too many" })), None).await;
    assert_eq!(reply.status, Status::BadRequest, "{}", reply.text);
    assert_eq!(reply.body, json!({ "type": "TooManyChannelWebhooks", "max": 15 }));

    // the server's cap counts every channel
    let second = harness.new_channel(&scene.server).await;
    let server = harness.db.fetch_server(&scene.server.id).await.unwrap();
    let third = harness.new_channel(&server).await;
    let mut count = MAX_CHANNEL_WEBHOOKS;
    while count < MAX_SERVER_WEBHOOKS - 1 {
        stored_webhook(&harness, &scene.channel, &scene.user.id, 0).await;
        count += 1;
    }
    let path = format!("/channels/{}/webhooks", second.id());
    let last = call(&harness, Post, &path, Some(&scene.session), Some(json!({ "name": "the last" })), None).await;
    assert_eq!(last.status, Status::Ok, "{}", last.text);

    let path = format!("/channels/{}/webhooks", third.id());
    let reply = call(&harness, Post, &path, Some(&scene.session), Some(json!({ "name": "too many" })), None).await;
    assert_eq!(reply.status, Status::BadRequest, "{}", reply.text);
    assert_eq!(reply.body, json!({ "type": "TooManyServerWebhooks", "max": 1000 }));
}

#[rocket::async_test]
async fn webhooks_live_in_channels_not_threads() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let thread = call(
        &harness,
        Post,
        &format!("/channels/{}/threads", scene.channel),
        Some(&scene.session),
        Some(json!({ "name": "A thread" })),
        None,
    )
    .await;
    assert_eq!(thread.status, Status::Ok, "{}", thread.text);
    let thread = thread.body["_id"].as_str().unwrap();
    let reply = call(
        &harness,
        Post,
        &format!("/channels/{thread}/webhooks"),
        Some(&scene.session),
        Some(json!({ "name": "Nope" })),
        None,
    )
    .await;
    assert_eq!(reply.status, Status::BadRequest, "{}", reply.text);
}

#[rocket::async_test]
async fn a_server_list_shows_what_you_manage() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, _) = webhook(&harness, &scene, "Deploys").await;
    let other = harness.new_channel(&scene.server).await;
    let (second, _) = webhook_in(&harness, &scene, other.id(), "Second").await;

    let reply = call(&harness, Get, &format!("/servers/{}/webhooks", scene.server.id), Some(&scene.session), None, None).await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    let ids: Vec<&str> = reply.body.as_array().unwrap().iter().map(|hook| hook["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![second.as_str(), id.as_str()]);
    assert_eq!(reply.body[0]["channel_id"], json!(other.id()));
    assert_eq!(reply.body[0]["creator_id"], json!(scene.user.id));
    assert!(reply.body[0].get("token").is_none());
}

#[rocket::async_test]
async fn last_used_is_kept() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    let list = format!("/webhooks/{id}");
    let before = call(&harness, Get, &list, Some(&scene.session), None, None).await;
    assert!(before.body.get("last_used").is_none());
    post(&harness, &id, &token, json!({ "content": "x" })).await;
    let after = call(&harness, Get, &list, Some(&scene.session), None, None).await;
    assert!(after.body["last_used"].is_string(), "{}", after.text);
}

#[rocket::async_test]
async fn moving_a_webhook() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let target = harness.new_channel(&scene.server).await;
    let body = json!({ "channel_id": target.id() });

    // a token cannot
    let refused = call(&harness, Patch, &format!("/webhooks/{id}/{token}"), None, Some(body.clone()), None).await;
    assert_eq!(refused.status, Status::Forbidden, "{}", refused.text);
    assert_eq!(harness.db.fetch_webhook(&id).await.unwrap().channel_id, scene.channel);

    // nor to a channel of another server
    let (_, _, stranger) = harness.new_user().await;
    let (_, channels) = harness.new_server(&stranger).await;
    let foreign = call(
        &harness,
        Patch,
        &format!("/webhooks/{id}"),
        Some(&scene.session),
        Some(json!({ "channel_id": channels[0].id() })),
        None,
    )
    .await;
    assert!(foreign.status.class().is_client_error(), "{}", foreign.text);

    let moved = call(&harness, Patch, &format!("/webhooks/{id}"), Some(&scene.session), Some(body), None).await;
    assert_eq!(moved.status, Status::Ok, "{}", moved.text);
    assert_eq!(moved.body["channel_id"], json!(target.id()));

    let sent = post(&harness, &id, &token, json!({ "content": "here now" })).await;
    assert_eq!(sent.body["channel"], json!(target.id()));
}

#[rocket::async_test]
async fn audit_log_has_every_change_with_its_reason() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, _) = webhook(&harness, &scene, "Deploys").await;
    let session = Header::new("x-session-token", scene.session.token.to_string());
    let reason = Header::new("X-Audit-Log-Reason", "rotation");

    let response = harness
        .client
        .post(format!("/webhooks/{id}/token"))
        .header(session.clone())
        .header(reason.clone())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);

    let response = harness
        .client
        .patch(format!("/webhooks/{id}"))
        .header(session.clone())
        .header(reason)
        .header(ContentType::JSON)
        .body(json!({ "name": "CI" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);

    let logs = call(&harness, Get, &format!("/servers/{}/audit_logs", scene.server.id), Some(&scene.session), None, None).await;
    assert_eq!(logs.status, Status::Ok, "{}", logs.text);
    let entries = logs.body["audit_logs"].as_array().unwrap();
    for kind in ["WebhookCreate", "WebhookTokenReset", "WebhookUpdate"] {
        let entry = entries
            .iter()
            .find(|entry| entry["action"]["type"] == json!(kind))
            .unwrap_or_else(|| panic!("no {kind}: {}", logs.text));
        if kind != "WebhookCreate" {
            assert_eq!(entry["reason"], json!("rotation"), "{kind}");
        }
    }
}

#[rocket::async_test]
async fn deleting_a_channel_takes_its_webhooks() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let target = harness.new_channel(&scene.server).await;
    let (id, token) = webhook_in(&harness, &scene, target.id(), "Deploys").await;

    let reply = call(&harness, Delete, &format!("/channels/{}", target.id()), Some(&scene.session), None, None).await;
    assert_eq!(reply.status, Status::NoContent, "{}", reply.text);
    assert!(harness.db.fetch_webhook(&id).await.is_err());
    assert!(post(&harness, &id, &token, json!({ "content": "x" })).await.status.class().is_client_error());
}

#[rocket::async_test]
async fn a_forum_needs_a_thread() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let forum = forum(&harness, &scene).await;
    let (id, token) = webhook_in(&harness, &scene, &forum, "Forum poster").await;

    let plain = post(&harness, &id, &token, json!({ "content": "x" })).await;
    assert_eq!(plain.status, Status::BadRequest, "{}", plain.text);
    assert_eq!(plain.body["type"], json!("ForumThreadRequired"));

    let wrong_tag = post(
        &harness,
        &id,
        &token,
        json!({ "content": "x", "thread_name": "Bug", "applied_tags": ["nope"] }),
    )
    .await;
    assert_eq!(wrong_tag.status, Status::BadRequest, "{}", wrong_tag.text);

    let created = post(
        &harness,
        &id,
        &token,
        json!({ "content": "First post", "thread_name": "Bug report", "applied_tags": ["bug"] }),
    )
    .await;
    assert_eq!(created.status, Status::Ok, "{}", created.text);
    let thread_id = created.body["channel"].as_str().unwrap();
    assert_ne!(thread_id, forum);
    let thread = harness.db.fetch_channel(thread_id).await.unwrap();
    let revolt_database::Channel::TextChannel { parent, thread: Some(info), name, .. } = thread else {
        panic!("not a thread");
    };
    assert_eq!(parent.as_deref(), Some(forum.as_str()));
    assert_eq!(name, "Bug report");
    assert_eq!(info.tags, vec!["bug".to_string()]);
    assert_eq!(info.message_count, 1);

    let second = call(
        &harness,
        Post,
        &format!("/webhooks/{id}/{token}?wait=true&thread_id={thread_id}"),
        None,
        Some(json!({ "content": "Second post" })),
        None,
    )
    .await;
    assert_eq!(second.status, Status::Ok, "{}", second.text);
    assert_eq!(second.body["channel"], json!(thread_id));
}

#[rocket::async_test]
async fn a_thread_must_be_of_the_webhooks_channel() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let other = harness.new_channel(&scene.server).await;

    let reply = call(
        &harness,
        Post,
        &format!("/webhooks/{id}/{token}?wait=true&thread_id={}", other.id()),
        None,
        Some(json!({ "content": "x" })),
        None,
    )
    .await;
    assert_eq!(reply.status, Status::NotFound, "{}", reply.text);

    let name = post(&harness, &id, &token, json!({ "content": "x", "thread_name": "new" })).await;
    assert_eq!(name.status, Status::BadRequest, "{}", name.text);
}

fn button() -> Value {
    json!([{ "type": "ActionRow", "components": [
        { "type": "Button", "custom_id": "go", "label": "Go", "style": "Primary" }
    ] }])
}

#[rocket::async_test]
async fn interactive_components_are_for_webhooks_made_by_a_bot() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (_, bot_user) = harness.new_bot(&scene.user).await;
    let everything = *revolt_permissions::DEFAULT_WEBHOOK_PERMISSIONS;

    let (by_person, token) = webhook(&harness, &scene, "Deploys").await;
    let refused = post(&harness, &by_person, &token, json!({ "content": "x", "components": button() })).await;
    assert_eq!(refused.status, Status::BadRequest, "{}", refused.text);

    let (by_bot, token) = stored_webhook(&harness, &scene.channel, &bot_user.id, everything).await;
    let accepted = post(&harness, &by_bot, &token, json!({ "content": "x", "components": button() })).await;
    assert_eq!(accepted.status, Status::Ok, "{}", accepted.text);
}

#[rocket::async_test]
async fn the_message_routes_touch_only_its_own_messages() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;
    let (other, other_token) = webhook(&harness, &scene, "Other").await;
    let sent = post(&harness, &id, &token, json!({ "content": "mine" })).await;
    let message = sent.body["_id"].as_str().unwrap();
    let path = format!("/webhooks/{id}/{token}/messages/{message}");
    let foreign = format!("/webhooks/{other}/{other_token}/messages/{message}");

    let got = call(&harness, Get, &path, None, None, None).await;
    assert_eq!(got.status, Status::Ok, "{}", got.text);
    assert_eq!(got.body["content"], json!("mine"));
    assert!(call(&harness, Get, &foreign, None, None, None).await.status.class().is_client_error());

    let edited = call(&harness, Patch, &path, None, Some(json!({ "content": "edited", "embeds": [{ "title": "t", "color": 255 }] })), None).await;
    assert_eq!(edited.status, Status::Ok, "{}", edited.text);
    assert_eq!(edited.body["content"], json!("edited"));
    assert_eq!(edited.body["embeds"][0]["colour"], json!("#0000FF"));
    let refused = call(&harness, Patch, &foreign, None, Some(json!({ "content": "x" })), None).await;
    assert!(refused.status.class().is_client_error());

    assert!(call(&harness, Delete, &foreign, None, None, None).await.status.class().is_client_error());
    let deleted = call(&harness, Delete, &path, None, None, None).await;
    assert_eq!(deleted.status, Status::NoContent, "{}", deleted.text);
    assert!(harness.db.fetch_message(message).await.is_err());
}

fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (ContentType, Vec<u8>) {
    let boundary = "testboundary";
    let mut body = vec![];
    for (name, file, bytes) in parts {
        let filename = file.map(|file| format!("; filename=\"{file}\"")).unwrap_or_default();
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"{filename}\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (ContentType::new("multipart", "form-data").with_params(("boundary", boundary)), body)
}

async fn send_form(
    harness: &TestHarness,
    id: &str,
    token: &str,
    parts: &[(&str, Option<&str>, &[u8])],
) -> Reply {
    let (kind, body) = multipart(parts);
    let response = harness
        .client
        .post(format!("/webhooks/{id}/{token}?wait=true"))
        .header(kind)
        .body(body)
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    Reply {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
        headers: vec![],
    }
}

#[rocket::async_test]
async fn a_form_without_files_sends_like_json() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    let reply = send_form(&harness, &id, &token, &[("payload_json", None, br#"{"content":"from json","username":"Bot"}"#)]).await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    assert_eq!(reply.body["content"], json!("from json"));
    assert_eq!(reply.body["masquerade"]["name"], json!("Bot"));

    let fields = send_form(&harness, &id, &token, &[("content", None, b"plain field")]).await;
    assert_eq!(fields.status, Status::Ok, "{}", fields.text);
    assert_eq!(fields.body["content"], json!("plain field"));

    let broken = send_form(&harness, &id, &token, &[("payload_json", None, b"{oops")]).await;
    assert_eq!(broken.status, Status::BadRequest, "{}", broken.text);
    assert_eq!(broken.body["type"], json!("FailedValidation"));
}

#[rocket::async_test]
async fn files_without_the_permission_are_refused_not_dropped() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = stored_webhook(
        &harness,
        &scene.channel,
        &scene.user.id,
        ChannelPermission::SendMessage as u64,
    )
    .await;

    let reply = send_form(
        &harness,
        &id,
        &token,
        &[("payload_json", None, br#"{"content":"x"}"#), ("files[0]", Some("a.png"), b"png")],
    )
    .await;
    assert_eq!(reply.status, Status::Forbidden, "{}", reply.text);
    assert_eq!(reply.body["permission"], json!("UploadFiles"));
}

#[rocket::async_test]
async fn new_webhooks_may_send_files() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, _) = webhook(&harness, &scene, "Deploys").await;
    assert!(harness.db.fetch_webhook(&id).await.unwrap().can_upload_files());
    // and a mass ping is still not among the defaults
    let permissions = harness.db.fetch_webhook(&id).await.unwrap().permissions;
    assert_eq!(permissions & ChannelPermission::MentionEveryone as u64, 0);
    assert_eq!(permissions & ChannelPermission::MentionRoles as u64, 0);
}

#[rocket::async_test]
async fn too_many_or_too_big_files_get_a_clear_answer() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Deploys").await;

    let names: Vec<String> = (0..6).map(|n| format!("files[{n}]")).collect();
    let parts: Vec<(&str, Option<&str>, &[u8])> =
        names.iter().map(|name| (name.as_str(), Some("a.png"), b"x" as &[u8])).collect();
    let reply = send_form(&harness, &id, &token, &parts).await;
    assert_eq!(reply.status, Status::BadRequest, "{}", reply.text);
    assert_eq!(reply.body["type"], json!("TooManyAttachments"));

    let big = vec![0u8; 20_000_001];
    let reply = send_form(&harness, &id, &token, &[("files[0]", Some("big.bin"), big.as_slice())]).await;
    assert_eq!(reply.status.code, 422, "{}", reply.text);
    assert_eq!(reply.body["type"], json!("FileTooLarge"));
}

#[rocket::async_test]
async fn managers_send_a_test_message_without_the_token() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, _) = webhook(&harness, &scene, "Deploys").await;
    let (session, _) = member(&harness, &scene).await;
    let path = format!("/webhooks/{id}/test");
    let body = json!({ "content": "رسالة تجريبية" });

    let refused = call(&harness, Post, &path, Some(&session), Some(body.clone()), None).await;
    assert_eq!(refused.status, Status::Forbidden, "{}", refused.text);

    let reply = call(&harness, Post, &path, Some(&scene.session), Some(body), None).await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    assert_eq!(reply.body["webhook"]["name"], json!("Deploys"));

    let long = call(&harness, Post, &path, Some(&scene.session), Some(json!({ "content": "x".repeat(201) })), None).await;
    assert_eq!(long.status, Status::BadRequest, "{}", long.text);
}

#[rocket::async_test]
async fn the_names_of_staff_and_verified_bots_are_taken() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;

    // before the first name check: the names are kept for five minutes
    let (bot, bot_user) = harness.new_bot(&scene.user).await;
    harness
        .db
        .update_bot(
            &bot.id,
            &revolt_database::PartialBot {
                flags: Some(revolt_models::v0::BotFlags::Verified as i32),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (_, _, staff) = harness.new_user().await;
    harness
        .db
        .save_staff_grant(
            &revolt_database::StaffGrant {
                id: staff.id.clone(),
                roles: vec![],
                disabled: false,
                expires_at: None,
                last_totp_step: 0,
                version: 1,
                updated_at: 0,
                updated_by: "test".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    let create = format!("/channels/{}/webhooks", scene.channel);
    for name in [&bot_user.username, &staff.username] {
        let reply = call(&harness, Post, &create, Some(&scene.session), Some(json!({ "name": name })), None).await;
        assert_eq!(reply.status, Status::BadRequest, "{name}: {}", reply.text);
        assert_eq!(reply.body["type"], json!("InvalidUsername"));
    }
}

#[test]
fn hashing_is_shared_with_bots() {
    assert_eq!(hash_token("a").len(), 64);
}
