//! Rich embeds through webhooks and user messages: what is accepted, what a
//! webhook's permissions allow, limits on edits and what happens to embed
//! images when the message goes.

use revolt_database::{File, Metadata, User};
use revolt_permissions::ChannelPermission;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use crate::util::test::TestHarness;

fn auth(session: &revolt_database::Session) -> Header<'static> {
    Header::new("x-session-token", session.token.to_string())
}

async fn send(
    harness: &TestHarness,
    method: rocket::http::Method,
    path: &str,
    session: Option<&revolt_database::Session>,
    body: Value,
) -> (Status, Value) {
    let mut request = harness
        .client
        .req(method, path.to_string())
        .header(ContentType::JSON)
        .body(body.to_string());
    if let Some(session) = session {
        request = request.header(auth(session));
    }
    let response = request.dispatch().await;
    let status = response.status();
    let body = response
        .into_string()
        .await
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    (status, body)
}

/// The same as `send` for a test that sends more than the route's limit in a
/// row: a 429 says how long to wait, and the limit itself stays as it is
async fn send_waiting(
    harness: &TestHarness,
    method: rocket::http::Method,
    path: &str,
    body: Value,
) -> (Status, Value) {
    loop {
        let (status, answer) = send(harness, method, path, None, body.clone()).await;
        if status != Status::TooManyRequests {
            return (status, answer);
        }
        let wait = answer["retry_after"].as_u64().unwrap_or(1000).max(50) + 50;
        rocket::tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    }
}

use rocket::http::Method::{Patch, Post};

/// A server, its first channel and a webhook in it (default permissions)
async fn setup(harness: &TestHarness) -> (revolt_database::Session, User, String, String, String) {
    let (_, session, user) = harness.new_user().await;
    let (_, channels) = harness.new_server(&user).await;
    let channel = channels[0].id().to_string();
    let (status, webhook) = send(
        harness,
        Post,
        &format!("/channels/{channel}/webhooks"),
        Some(&session),
        json!({ "name": "Match bot" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{webhook}");
    (
        session,
        user,
        channel,
        webhook["id"].as_str().unwrap().to_string(),
        webhook["token"].as_str().unwrap().to_string(),
    )
}

fn rich() -> Value {
    json!({
        "title": "Ranked match ready",
        "url": "https://zrzur.com/match/1",
        "description": "Team **Falcons** vs Team **Wolves**",
        "colour": "#E0195F",
        "author": { "name": "Match bot", "url": "https://zrzur.com", "icon_url": "https://zrzur.com/i.png" },
        "fields": [
            { "name": "Map", "value": "Dust", "inline": true },
            { "name": "Mode", "value": "5v5", "inline": true }
        ],
        "footer": { "text": "Season 3", "icon_url": "https://zrzur.com/f.png" },
        "timestamp": "2026-11-02T18:00:00Z"
    })
}

async fn upload(harness: &TestHarness, user_id: &str) -> String {
    let id = TestHarness::rand_string();
    harness
        .db
        .insert_attachment(&File {
            id: id.clone(),
            tag: "attachments".to_string(),
            filename: "shot.png".to_string(),
            hash: None,
            uploaded_at: None,
            uploader_id: Some(user_id.to_string()),
            used_for: None,
            deleted: None,
            reported: None,
            purged: None,
            metadata: Metadata::Image {
                width: 64,
                height: 64,
                thumbhash: None,
                animated: None,
            },
            content_type: "image/png".to_string(),
            size: 100,
            message_id: None,
            user_id: None,
            server_id: None,
            object_id: None,
            forwarded_from: None,
        })
        .await
        .unwrap();
    id
}

#[rocket::async_test]
async fn webhook_sends_a_full_rich_embed() {
    let harness = TestHarness::new().await;
    let (_, _, _, id, token) = setup(&harness).await;
    let (status, message) = send(
        &harness,
        Post,
        &format!("/webhooks/{id}/{token}?wait=true"),
        None,
        json!({ "embeds": [rich()] }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{message}");
    let embed = &message["embeds"][0];
    assert_eq!(embed["type"], "Text");
    assert_eq!(embed["author"]["name"], "Match bot");
    assert_eq!(embed["fields"].as_array().unwrap().len(), 2);
    assert_eq!(embed["footer"]["text"], "Season 3");
    assert_eq!(embed["colour"], "#E0195F");
}

#[rocket::async_test]
async fn script_links_and_broken_embeds_are_refused() {
    let harness = TestHarness::new().await;
    let (session, _, channel, id, token) = setup(&harness).await;
    let mut javascript = rich();
    javascript["url"] = json!("javascript:alert(document.cookie)");
    let mut data_icon = rich();
    data_icon["author"]["icon_url"] = json!("data:image/svg+xml,<svg onload=alert(1)>");
    let mut file_footer = rich();
    file_footer["footer"]["icon_url"] = json!("file:///etc/passwd");
    let mut css = rich();
    css["colour"] = json!("red; background:url(https://evil.example/x)");
    let mut too_many_fields = rich();
    too_many_fields["fields"] = json!(vec![json!({ "name": "a", "value": "b" }); 26]);
    let mut long_value = rich();
    long_value["fields"][0]["value"] = json!("x".repeat(1025));
    for (what, embed) in [
        ("javascript url", javascript),
        ("data icon", data_icon),
        ("file footer", file_footer),
        ("css in colour", css),
        ("26 fields", too_many_fields),
        ("long field", long_value),
    ] {
        let (status, _) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "embeds": [embed.clone()] })).await;
        assert_eq!(status, Status::BadRequest, "webhook: {what}");
        let (status, _) = send(&harness, Post, &format!("/channels/{channel}/messages"), Some(&session), json!({ "embeds": [embed] })).await;
        assert_eq!(status, Status::BadRequest, "user: {what}");
    }
}

#[rocket::async_test]
async fn webhook_without_embed_permission_cannot_add_embeds_by_editing() {
    let harness = TestHarness::new().await;
    let (session, _, _, id, token) = setup(&harness).await;
    // the owner takes the embed permission away
    let (status, _) = send(
        &harness,
        Patch,
        &format!("/webhooks/{id}"),
        Some(&session),
        json!({ "permissions": ChannelPermission::SendMessage as u64 }),
    )
    .await;
    assert_eq!(status, Status::Ok);
    let (status, _) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "embeds": [rich()] })).await;
    assert_eq!(status, Status::Forbidden, "sending refuses embeds");
    let (status, message) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "content": "plain" })).await;
    assert_eq!(status, Status::Ok);
    let message_id = message["_id"].as_str().unwrap();
    let (status, _) = send(
        &harness,
        Patch,
        &format!("/webhooks/{id}/{token}/{message_id}"),
        None,
        json!({ "embeds": [rich()] }),
    )
    .await;
    assert_eq!(status, Status::Forbidden, "editing must refuse them too");
    let stored = harness.db.fetch_message(message_id).await.unwrap();
    assert!(stored.embeds.unwrap_or_default().is_empty());
}

#[rocket::async_test]
async fn webhook_without_send_permission_cannot_edit() {
    let harness = TestHarness::new().await;
    let (session, _, _, id, token) = setup(&harness).await;
    let (_, message) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "content": "before" })).await;
    let message_id = message["_id"].as_str().unwrap().to_string();
    send(&harness, Patch, &format!("/webhooks/{id}"), Some(&session), json!({ "permissions": 0 })).await;
    let (status, _) = send(
        &harness,
        Patch,
        &format!("/webhooks/{id}/{token}/{message_id}"),
        None,
        json!({ "content": "after" }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(
        harness.db.fetch_message(&message_id).await.unwrap().content.as_deref(),
        Some("before")
    );
}

#[rocket::async_test]
async fn edits_keep_the_embed_limit() {
    let harness = TestHarness::new().await;
    let (session, _, channel, id, token) = setup(&harness).await;
    let max = revolt_config::config().await.features.limits.global.message_embeds;
    let many = json!(vec![json!({ "title": "x" }); max + 1]);

    // webhook message
    let (_, message) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "content": "hi" })).await;
    let message_id = message["_id"].as_str().unwrap();
    let (status, body) = send(&harness, Patch, &format!("/webhooks/{id}/{token}/{message_id}"), None, json!({ "embeds": many })).await;
    assert_eq!(status, Status::BadRequest, "webhook edit: {body}");

    // own message
    let (_, message) = send(&harness, Post, &format!("/channels/{channel}/messages"), Some(&session), json!({ "content": "hi" })).await;
    let message_id = message["_id"].as_str().unwrap();
    let (status, body) = send(&harness, Patch, &format!("/channels/{channel}/messages/{message_id}"), Some(&session), json!({ "embeds": many })).await;
    assert_eq!(status, Status::BadRequest, "user edit: {body}");

    // exactly the limit is fine
    let enough = json!(vec![json!({ "title": "x" }); max]);
    let (status, _) = send(&harness, Patch, &format!("/channels/{channel}/messages/{message_id}"), Some(&session), json!({ "embeds": enough })).await;
    assert_eq!(status, Status::Ok);
}

#[rocket::async_test]
async fn embed_images_go_with_their_message() {
    let harness = TestHarness::new().await;
    let (session, user, channel, _, _) = setup(&harness).await;
    let media = upload(&harness, &user.id).await;
    let thumbnail = upload(&harness, &user.id).await;
    let mut embed = rich();
    embed["media"] = json!(media);
    embed["thumbnail"] = json!(thumbnail);
    let (status, message) = send(&harness, Post, &format!("/channels/{channel}/messages"), Some(&session), json!({ "embeds": [embed] })).await;
    assert_eq!(status, Status::Ok, "{message}");
    let message_id = message["_id"].as_str().unwrap().to_string();

    // a used file can't be put in another message
    let mut again = rich();
    again["media"] = json!(media);
    let (status, _) = send(&harness, Post, &format!("/channels/{channel}/messages"), Some(&session), json!({ "embeds": [again] })).await;
    assert_ne!(status, Status::Ok, "a file is used once");

    let response = harness
        .client
        .delete(format!("/channels/{channel}/messages/{message_id}"))
        .header(auth(&session))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);
    for id in [&media, &thumbnail] {
        let file = harness.db.fetch_attachment("attachments", id).await;
        assert!(
            file.is_err() || file.unwrap().deleted == Some(true),
            "embed file {id} must be deleted with the message"
        );
    }
}

#[rocket::async_test]
async fn replaced_embed_images_are_deleted() {
    let harness = TestHarness::new().await;
    let (session, user, channel, _, _) = setup(&harness).await;
    let old = upload(&harness, &user.id).await;
    let mut embed = rich();
    embed["media"] = json!(old);
    let (_, message) = send(&harness, Post, &format!("/channels/{channel}/messages"), Some(&session), json!({ "embeds": [embed] })).await;
    let message_id = message["_id"].as_str().unwrap().to_string();
    let (status, _) = send(&harness, Patch, &format!("/channels/{channel}/messages/{message_id}"), Some(&session), json!({ "embeds": [rich()] })).await;
    assert_eq!(status, Status::Ok);
    let file = harness.db.fetch_attachment("attachments", &old).await;
    assert!(file.is_err() || file.unwrap().deleted == Some(true), "the replaced image is gone");
}

#[rocket::async_test]
async fn arabic_messages_get_the_full_length() {
    let harness = TestHarness::new().await;
    let (session, _, channel, _, _) = setup(&harness).await;
    let max = revolt_config::config().await.features.limits.default.message_length;
    // counted in characters: an Arabic letter is two bytes but one character
    let (status, body) = send(&harness, Post, &format!("/channels/{channel}/messages"), Some(&session), json!({ "content": "ع".repeat(max) })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let (status, _) = send(&harness, Post, &format!("/channels/{channel}/messages"), Some(&session), json!({ "content": "ع".repeat(max + 1) })).await;
    assert_eq!(status, Status::UnprocessableEntity);
}

#[rocket::async_test]
async fn embeds_have_their_own_budget() {
    let harness = TestHarness::new().await;
    let (_, _, _, id, token) = setup(&harness).await;
    let max = revolt_config::config().await.features.limits.default.message_length;
    // full text next to a full-length embed
    let big = json!({ "title": "t".repeat(256), "description": "d".repeat(4096) });
    let (status, body) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "content": "c".repeat(max), "embeds": [big] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    // all embeds of a message together: at most 6000 characters
    let part = json!({ "description": "d".repeat(3000) });
    let (status, body) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "embeds": [part, part] })).await;
    assert_eq!(status, Status::Ok, "6000 exactly: {body}");
    let (status, _) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "embeds": [part, part, { "title": "x" }] })).await;
    assert_eq!(status, Status::UnprocessableEntity, "6001");
    // titles count too (they used to be free)
    let titles = json!(vec![json!({ "title": "t".repeat(256), "description": "d".repeat(1000) }); 5]);
    let (status, _) = send(&harness, Post, &format!("/webhooks/{id}/{token}?wait=true"), None, json!({ "embeds": titles })).await;
    assert_eq!(status, Status::UnprocessableEntity, "5 x 1256 = 6280");
}

/// Webhooks post components nobody can use (links, text, cards), as on
/// Discord; buttons and menus that need a bot to answer are refused
#[rocket::async_test]
async fn webhooks_post_static_components_only() {
    let harness = TestHarness::new().await;
    let (_, _, _, webhook, token) = setup(&harness).await;
    let path = format!("/webhooks/{webhook}/{token}");
    let link = json!({ "type": "Button", "style": "Link", "label": "Match page", "url": "https://zrzur.com/match/1" });

    let (status, message) = send_waiting(&harness, Post, &format!("{path}?wait=true"), json!({
        "content": "Final score",
        "components": [
            { "type": "ActionRow", "components": [link] },
            { "type": "Container", "accent": "#55DCC8", "components": [
                { "type": "Text", "content": "**Falcons** 2 : 1 Wolves" },
                { "type": "Section", "text": ["Highlights"], "button": link }
            ]}
        ]
    }))
    .await;
    assert_eq!(status, Status::Ok, "{message}");
    let id = message["_id"].as_str().unwrap().to_string();

    for components in [
        json!([{ "type": "ActionRow", "components": [{ "type": "Button", "style": "Primary", "label": "Join", "custom_id": "join" }] }]),
        json!([{ "type": "ActionRow", "components": [{ "type": "StringSelect", "custom_id": "m", "options": [{ "label": "A", "value": "a" }] }] }]),
        json!([{ "type": "Container", "components": [{ "type": "Section", "text": ["x"], "button": { "type": "Button", "style": "Success", "label": "Go", "custom_id": "go" } }] }]),
    ] {
        let (status, _) = send_waiting(&harness, Post, &path, json!({ "content": "x", "components": components })).await;
        assert_eq!(status, Status::BadRequest, "send {components}");
        let (status, _) = send_waiting(&harness, Patch, &format!("{path}/{id}"), json!({ "components": components })).await;
        assert_eq!(status, Status::BadRequest, "edit {components}");
    }

    // editing swaps the link, and an empty list removes the components
    let (status, edited) = send_waiting(&harness, Patch, &format!("{path}/{id}"), json!({
        "components": [{ "type": "ActionRow", "components": [
            { "type": "Button", "style": "Link", "label": "Replay", "url": "https://zrzur.com/replay/1" }
        ]}]
    }))
    .await;
    assert_eq!(status, Status::Ok, "{edited}");
    assert_eq!(edited["components"][0]["components"][0]["label"], "Replay");
    let (status, cleared) = send_waiting(&harness, Patch, &format!("{path}/{id}"), json!({ "components": [] })).await;
    assert_eq!(status, Status::Ok);
    assert!(cleared["components"].as_array().is_none_or(|c| c.is_empty()), "{cleared}");
}
