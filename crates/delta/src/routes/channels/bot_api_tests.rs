//! API-5: a bot, with its own token, uses threads, forums, invites, typing
//! and reactions through the API; permissions and intents still hold.

use crate::{rocket, util::test::{PubSubTestHelper, TestHarness}};
use revolt_database::{events::client::EventV1, Bot, Channel, Member, Server, User};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission as P, OverrideField};
use rocket::http::{ContentType, Header, Method, Status};
use serde_json::{json, Value};

struct Setup {
    harness: TestHarness,
    server: Server,
    channel: Channel,
    bot: Bot,
    owner_token: String,
}

impl Setup {
    /// A server with a bot in it, whose role allows `allow` on top of the defaults
    async fn new(allow: &[P]) -> (Setup, User) {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        let (bot, bot_user) = harness.new_bot(&owner).await;
        Member::create(&harness.db, &server, &owner, None).await.unwrap();
        let (mut member, _) = Member::create(&harness.db, &server, &bot_user, None).await.unwrap();

        let bits = allow.iter().fold(0i64, |bits, permission| bits | *permission as i64);
        let role = harness
            .new_role(&server, 1, Some(OverrideField { a: bits, d: 0 }))
            .await;
        member
            .update(
                &harness.db,
                revolt_database::PartialMember {
                    roles: Some(vec![role.id.clone()]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        // the role has to be in the server the permission check reads
        let channel = channels.into_iter().next().unwrap();
        (
            Setup {
                harness,
                server,
                channel,
                bot,
                owner_token: session.token,
            },
            bot_user,
        )
    }

    async fn bot(&self, method: Method, path: String, body: Option<Value>) -> (Status, Value) {
        let mut request = self
            .harness
            .client
            .req(method, path)
            .header(Header::new("x-bot-token", self.bot.token.clone()));
        if let Some(body) = body {
            request = request.header(ContentType::JSON).body(body.to_string());
        }
        let response = request.dispatch().await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    async fn owner(&self, method: Method, path: String, body: Option<Value>) -> (Status, Value) {
        let mut request = self
            .harness
            .client
            .req(method, path)
            .header(Header::new("x-session-token", self.owner_token.clone()));
        if let Some(body) = body {
            request = request.header(ContentType::JSON).body(body.to_string());
        }
        let response = request.dispatch().await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    fn channel_path(&self, rest: &str) -> String {
        format!("/channels/{}/{rest}", self.channel.id())
    }
}

fn id(value: &Value) -> String {
    value["_id"].as_str().unwrap_or_default().to_string()
}

#[rocket::async_test]
async fn a_bot_starts_lists_edits_and_writes_in_a_thread() {
    let (s, _) = Setup::new(&[P::ManageMessages]).await;

    let (status, thread) = s
        .bot(
            Method::Post,
            s.channel_path("threads"),
            Some(json!({ "name": "Release notes", "content": "first message" })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{thread}");
    let thread_id = id(&thread);

    let (status, list) = s.bot(Method::Get, s.channel_path("threads"), None).await;
    assert_eq!(status, Status::Ok);
    assert!(list.as_array().unwrap().iter().any(|t| id(t) == thread_id));

    let (status, sent) = s
        .bot(
            Method::Post,
            format!("/channels/{thread_id}/messages"),
            Some(json!({ "content": "in the thread" })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{sent}");

    let (status, edited) = s
        .bot(
            Method::Patch,
            format!("/channels/{thread_id}/thread"),
            Some(json!({ "name": "Renamed", "archived": true })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{edited}");
    assert_eq!(edited["name"], "Renamed");

    let (_, archived) = s
        .bot(Method::Get, s.channel_path("threads?archived=true"), None)
        .await;
    assert!(archived.as_array().unwrap().iter().any(|t| id(t) == thread_id));
}

#[rocket::async_test]
async fn a_bot_posts_in_a_forum_with_tags() {
    let (s, _) = Setup::new(&[P::ManageMessages]).await;

    let (status, forum) = s
        .owner(
            Method::Post,
            format!("/servers/{}/channels", s.server.id),
            Some(json!({ "name": "questions", "type": "Forum" })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{forum}");
    let forum_id = id(&forum);
    let (status, tagged) = s
        .owner(
            Method::Patch,
            format!("/channels/{forum_id}"),
            Some(json!({ "forum_tags": [{ "id": "help", "name": "Help" }] })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{tagged}");

    let (status, post) = s
        .bot(
            Method::Post,
            format!("/channels/{forum_id}/threads"),
            Some(json!({ "name": "How do I start?", "content": "Read the guide.", "tags": ["help"] })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{post}");
    assert_eq!(post["thread"]["tags"][0], "help");

    // a tag the forum does not have is refused
    let (status, _) = s
        .bot(
            Method::Post,
            format!("/channels/{forum_id}/threads"),
            Some(json!({ "name": "Other", "content": "text", "tags": ["nope"] })),
        )
        .await;
    assert_ne!(status, Status::Ok);
}

#[rocket::async_test]
async fn a_bot_manages_invites_with_the_right_permissions_only() {
    let (s, _) = Setup::new(&[P::InviteOthers, P::ManageServer]).await;

    let (status, invite) = s.bot(Method::Post, s.channel_path("invites"), None).await;
    assert_eq!(status, Status::Ok, "{invite}");
    let code = id(&invite);

    let (status, public) = s.bot(Method::Get, format!("/invites/{code}"), None).await;
    assert_eq!(status, Status::Ok, "{public}");

    let (status, list) = s
        .bot(Method::Get, format!("/servers/{}/invites", s.server.id), None)
        .await;
    assert_eq!(status, Status::Ok);
    assert!(list.as_array().unwrap().iter().any(|i| id(i) == code));

    // bots are added by people; they never join through a link
    let (status, body) = s.bot(Method::Post, format!("/invites/{code}"), None).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["type"], "IsBot");

    let (status, _) = s.bot(Method::Delete, format!("/invites/{code}"), None).await;
    assert_eq!(status, Status::NoContent);
}

#[rocket::async_test]
async fn a_bot_without_invite_others_makes_no_invites() {
    let (s, _) = Setup::new(&[]).await;
    let (_, removed) = s
        .owner(
            Method::Patch,
            format!("/servers/{}", s.server.id),
            Some(json!({ "name": "Still here" })),
        )
        .await;
    assert_eq!(removed["name"], "Still here");

    // @everyone may invite by default: take that away
    let everyone = (*revolt_permissions::DEFAULT_PERMISSION_SERVER as i64) & !(P::InviteOthers as i64);
    let (status, body) = s
        .owner(
            Method::Put,
            format!("/servers/{}/permissions/default", s.server.id),
            Some(json!({ "permissions": everyone })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{body}");

    let (status, _) = s.bot(Method::Post, s.channel_path("invites"), None).await;
    assert_eq!(status, Status::Forbidden);
}

#[rocket::async_test]
async fn typing_over_rest_reaches_the_channel_and_needs_send_messages() {
    let (s, bot_user) = Setup::new(&[]).await;
    let mut events = PubSubTestHelper::new(s.channel.id()).await;

    let (status, _) = s.bot(Method::Post, s.channel_path("typing"), None).await;
    assert_eq!(status, Status::NoContent);
    let event = events
        .wait_for_event(|event| matches!(event, EventV1::ChannelStartTyping { .. }))
        .await;
    let EventV1::ChannelStartTyping { user, .. } = event else { panic!() };
    assert_eq!(user, bot_user.id);

    // nobody types in a channel they may not write in
    let (_, other_session, _) = s.harness.new_user().await;
    let response = s
        .harness
        .client
        .post(s.channel_path("typing"))
        .header(Header::new("x-session-token", other_session.token))
        .dispatch()
        .await;
    assert_ne!(response.status(), Status::NoContent);
}

#[rocket::async_test]
async fn a_bot_reacts_and_removes_its_reaction() {
    let (s, _) = Setup::new(&[]).await;
    let (status, message) = s
        .bot(Method::Post, s.channel_path("messages"), Some(json!({ "content": "vote" })))
        .await;
    assert_eq!(status, Status::Ok, "{message}");
    let path = s.channel_path(&format!("messages/{}/reactions/%F0%9F%91%8D", id(&message)));

    let (status, _) = s.bot(Method::Put, path.clone(), None).await;
    assert_eq!(status, Status::NoContent);
    let (_, fetched) = s
        .bot(Method::Get, s.channel_path(&format!("messages/{}", id(&message))), None)
        .await;
    assert!(fetched["reactions"]["\u{1F44D}"].is_array(), "{fetched}");

    let (status, _) = s.bot(Method::Delete, path, None).await;
    assert_eq!(status, Status::NoContent);
    let (_, fetched) = s
        .bot(Method::Get, s.channel_path(&format!("messages/{}", id(&message))), None)
        .await;
    assert!(fetched["reactions"].get("\u{1F44D}").is_none(), "{fetched}");
}

#[rocket::async_test]
async fn a_bot_lists_pins_but_cannot_search_text() {
    let (s, _) = Setup::new(&[P::ManageMessages]).await;
    let (status, message) = s
        .owner(Method::Post, s.channel_path("messages"), Some(json!({ "content": "pin me" })))
        .await;
    assert_eq!(status, Status::Ok, "{message}");
    let (status, _) = s
        .bot(Method::Post, s.channel_path(&format!("messages/{}/pin", id(&message))), None)
        .await;
    assert_eq!(status, Status::NoContent);

    let (status, pins) = s
        .bot(Method::Post, s.channel_path("search"), Some(json!({ "pinned": true })))
        .await;
    assert_eq!(status, Status::Ok, "{pins}");
    let pins = pins.as_array().unwrap();
    assert_eq!(pins.len(), 1);
    // no Message Content intent: the bot sees that it is pinned, not the text
    assert_eq!(pins[0]["content"].as_str().unwrap_or_default(), "");

    let (status, _) = s
        .bot(Method::Post, s.channel_path("search"), Some(json!({ "query": "pin" })))
        .await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn a_thread_message_keeps_hiding_content_from_bots_without_the_intent() {
    let (s, _) = Setup::new(&[]).await;
    let (status, thread) = s
        .owner(
            Method::Post,
            s.channel_path("threads"),
            Some(json!({ "name": "Private talk", "content": "secret plans" })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{thread}");
    let (status, messages) = s
        .bot(Method::Get, format!("/channels/{}/messages", id(&thread)), None)
        .await;
    assert_eq!(status, Status::Ok, "{messages}");
    let messages: Vec<v0::Message> = serde_json::from_value(messages).unwrap();
    assert!(!messages.is_empty());
    assert!(messages.iter().all(|m| m.content.as_deref().unwrap_or("").is_empty()));
}
