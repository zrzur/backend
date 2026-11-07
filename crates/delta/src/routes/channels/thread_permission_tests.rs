//! Threads and forums have permissions of their own, read on the parent
//! channel, and a switch per server. Each is tried allowed and denied, on
//! every way in: people, bots and webhooks.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{Bot, Member, PartialMember, Server, User, Webhook};
use revolt_permissions::{
    ChannelPermission, OverrideField, DEFAULT_PERMISSION_SERVER, DEFAULT_WEBHOOK_PERMISSIONS,
    THREAD_PERMISSIONS,
};
use rocket::http::{
    ContentType, Header,
    Method::{self, Delete, Get, Patch, Post, Put},
    Status,
};
use serde_json::{json, Value};

const CREATE_THREADS: i64 = ChannelPermission::CreateThreads as i64;
const IN_THREADS: i64 = ChannelPermission::SendMessageInThreads as i64;
const CREATE_POSTS: i64 = ChannelPermission::CreateForumPosts as i64;
const SEND: i64 = ChannelPermission::SendMessage as i64;
const VIEW: i64 = ChannelPermission::ViewChannel as i64;

struct Setup {
    harness: TestHarness,
    server: Server,
    owner: String,
    owner_user: User,
    member: String,
    member_user: User,
    bot: Bot,
}

/// A server with its owner, one member and one bot
async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner_user) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner_user).await;
    let (_, member_session, member_user) = harness.new_user().await;
    Member::create(&harness.db, &server, &member_user, None).await.unwrap();
    let (bot, bot_user) = harness.new_bot(&owner_user).await;
    Member::create(&harness.db, &server, &bot_user, None).await.unwrap();
    Setup {
        harness,
        server,
        owner: owner_session.token,
        owner_user,
        member: member_session.token,
        member_user,
        bot,
    }
}

/// Status and body of a call; `token` is a session, or a bot token for "bot:…"
async fn call_with(
    s: &Setup,
    token: Option<&str>,
    method: Method,
    path: String,
    body: Option<Value>,
) -> (Status, Value) {
    let mut request = s.harness.client.req(method, path);
    match token {
        Some(token) => {
            let header = match token.strip_prefix("bot:") {
                Some(bot) => Header::new("x-bot-token", bot.to_string()),
                None => Header::new("x-session-token", token.to_string()),
            };
            request = request.header(header);
        }
        None => {}
    }
    if let Some(body) = body {
        request = request.header(ContentType::JSON).body(body.to_string());
    }
    let response = request.dispatch().await;
    let status = response.status();
    (status, response.into_json().await.unwrap_or(Value::Null))
}

fn id(value: &Value) -> String {
    value["_id"].as_str().unwrap().to_string()
}

impl Setup {
    fn bot_token(&self) -> String {
        format!("bot:{}", self.bot.token)
    }

    async fn call(&self, token: &str, method: Method, path: String, body: Option<Value>) -> (Status, Value) {
        call_with(self, Some(token), method, path, body).await
    }

    async fn channel(&self, kind: &str, name: &str) -> String {
        let (status, channel) = self
            .call(
                &self.owner,
                Post,
                format!("/servers/{}/channels", self.server.id),
                Some(json!({ "type": kind, "name": name })),
            )
            .await;
        assert_eq!(status, Status::Ok, "{channel}");
        id(&channel)
    }

    async fn send(&self, token: &str, channel: &str, content: &str) -> (Status, Value) {
        self.call(token, Post, format!("/channels/{channel}/messages"), Some(json!({ "content": content })))
            .await
    }

    async fn start(&self, token: &str, parent: &str, name: &str) -> (Status, Value) {
        self.call(
            token,
            Post,
            format!("/channels/{parent}/threads"),
            Some(json!({ "name": name, "content": "first" })),
        )
        .await
    }

    async fn deny_for_everyone(&self, channel: &str, deny: i64) {
        let (status, body) = self
            .call(
                &self.owner,
                Put,
                format!("/channels/{channel}/permissions/default"),
                Some(json!({ "permissions": { "allow": 0, "deny": deny } })),
            )
            .await;
        assert_eq!(status, Status::Ok, "{body}");
    }

    /// A role on the server, held by the member, with an override on `channel`
    async fn role_in_channel(&self, channel: &str, allow: i64, deny: i64) {
        let role = self.harness.new_role(&self.server, 1, None).await;
        let (status, body) = self
            .call(
                &self.owner,
                Put,
                format!("/channels/{channel}/permissions/{}", role.id),
                Some(json!({ "permissions": { "allow": allow, "deny": deny } })),
            )
            .await;
        assert_eq!(status, Status::Ok, "{body}");
        self.give_role(role.id).await;
    }

    /// A role on the server, held by the member, with these server permissions
    async fn server_role(&self, allow: i64, deny: i64) {
        let role = self
            .harness
            .new_role(&self.server, 1, Some(OverrideField { a: allow, d: deny }))
            .await;
        self.give_role(role.id).await;
    }

    async fn give_role(&self, role: String) {
        let mut member = self
            .harness
            .db
            .fetch_member(&self.server.id, &self.member_user.id)
            .await
            .unwrap();
        member
            .update(
                &self.harness.db,
                PartialMember {
                    roles: Some(vec![role]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
    }

    async fn switch(&self, field: &str, disabled: bool) {
        let (status, body) = self
            .call(
                &self.owner,
                Patch,
                format!("/servers/{}", self.server.id),
                Some(json!({ field: disabled })),
            )
            .await;
        assert_eq!(status, Status::Ok, "{body}");
        assert_eq!(body[field].as_bool().unwrap_or(false), disabled, "{body}");
    }

    async fn webhook(&self, channel: &str, creator: &str, permissions: u64) -> String {
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
        self.harness.db.insert_webhook(&webhook).await.unwrap();
        format!("/webhooks/{}/{token}", webhook.id)
    }

    async fn post_webhook(&self, path: String, body: Value) -> (Status, Value) {
        call_with(self, None, Post, path, Some(body)).await
    }
}

fn refused(reply: &(Status, Value), permission: &str) {
    assert_eq!(reply.0, Status::Forbidden, "{}", reply.1);
    assert_eq!(reply.1["type"], "MissingPermission", "{}", reply.1);
    assert_eq!(reply.1["permission"], permission, "{}", reply.1);
}

fn switched_off(reply: &(Status, Value), error: &str) {
    assert_eq!(reply.0, Status::Forbidden, "{}", reply.1);
    assert_eq!(reply.1["type"], error, "{}", reply.1);
}

#[test]
fn new_servers_and_webhooks_start_with_threads_open() {
    assert_eq!(*THREAD_PERMISSIONS as i64, CREATE_THREADS | IN_THREADS | CREATE_POSTS);
    assert_eq!(*DEFAULT_PERMISSION_SERVER & *THREAD_PERMISSIONS, *THREAD_PERMISSIONS);
    assert_ne!(*DEFAULT_WEBHOOK_PERMISSIONS & IN_THREADS as u64, 0);
    assert_ne!(*DEFAULT_WEBHOOK_PERMISSIONS & CREATE_POSTS as u64, 0);
    // a webhook has no way to start a thread in a text channel
    assert_eq!(*DEFAULT_WEBHOOK_PERMISSIONS & CREATE_THREADS as u64, 0);
}

#[rocket::async_test]
async fn create_threads_is_read_on_the_channel_and_its_overrides() {
    let s = setup().await;
    let closed = s.channel("Text", "closed").await;
    let open = s.channel("Text", "open").await;
    s.deny_for_everyone(&closed, CREATE_THREADS).await;

    refused(&s.start(&s.member, &closed, "No").await, "CreateThreads");
    assert_eq!(s.start(&s.member, &open, "Yes").await.0, Status::Ok);
    assert_eq!(s.start(&s.owner, &closed, "Owner").await.0, Status::Ok);

    // starting from a message is the same action
    let (_, message) = s.send(&s.member, &closed, "start here").await;
    let reply = s
        .call(
            &s.member,
            Post,
            format!("/channels/{closed}/threads"),
            Some(json!({ "name": "From it", "message": id(&message) })),
        )
        .await;
    refused(&reply, "CreateThreads");

    // a role override on the channel opens it again
    s.role_in_channel(&closed, CREATE_THREADS, 0).await;
    assert_eq!(s.start(&s.member, &closed, "With a role").await.0, Status::Ok);
}

#[rocket::async_test]
async fn denying_a_role_on_the_server_closes_it_everywhere() {
    let s = setup().await;
    let general = s.channel("Text", "general").await;
    let forum = s.channel("Forum", "help").await;
    let (_, thread) = s.start(&s.owner, &general, "Plans").await;

    s.server_role(0, CREATE_THREADS | IN_THREADS | CREATE_POSTS).await;

    refused(&s.start(&s.member, &general, "No").await, "CreateThreads");
    refused(&s.start(&s.member, &forum, "No").await, "CreateForumPosts");
    refused(&s.send(&s.member, &id(&thread), "No").await, "SendMessageInThreads");
    // the channel itself stays open
    assert_eq!(s.send(&s.member, &general, "here").await.0, Status::Ok);
}

#[rocket::async_test]
async fn a_closed_thread_takes_nothing_new_and_stays_readable() {
    let s = setup().await;
    let general = s.channel("Text", "general").await;
    let (_, thread) = s.start(&s.member, &general, "Plans").await;
    let thread = id(&thread);
    let (status, message) = s.send(&s.member, &thread, "before it closed").await;
    assert_eq!(status, Status::Ok, "{message}");
    let message = id(&message);

    s.deny_for_everyone(&general, IN_THREADS).await;

    refused(&s.send(&s.member, &thread, "after").await, "SendMessageInThreads");
    let edit = s
        .call(
            &s.member,
            Patch,
            format!("/channels/{thread}/messages/{message}"),
            Some(json!({ "content": "rewritten" })),
        )
        .await;
    refused(&edit, "SendMessageInThreads");
    refused(
        &s.call(&s.member, Post, format!("/channels/{thread}/typing"), None).await,
        "SendMessageInThreads",
    );
    // its owner can't rename it either
    refused(
        &s.call(&s.member, Patch, format!("/channels/{thread}/thread"), Some(json!({ "name": "x" }))).await,
        "SendMessageInThreads",
    );

    // reading is untouched
    assert_eq!(s.call(&s.member, Get, format!("/channels/{thread}/messages"), None).await.0, Status::Ok);
    assert_eq!(s.call(&s.member, Get, format!("/channels/{general}/threads"), None).await.0, Status::Ok);
    // the channel still takes messages
    assert_eq!(s.send(&s.member, &general, "here").await.0, Status::Ok);
    // the owner keeps access, as in any channel
    assert_eq!(s.send(&s.owner, &thread, "mine").await.0, Status::Ok);

    // a role allowed on the parent writes again: overrides work as in channels
    s.role_in_channel(&general, IN_THREADS, 0).await;
    assert_eq!(s.send(&s.member, &thread, "with a role").await.0, Status::Ok);
}

/// As in any channel: a moderator's role keeps them in only if it holds the
/// permission itself. The migration gives it to roles that manage channels.
#[rocket::async_test]
async fn a_moderator_stays_in_a_closed_thread_through_their_role() {
    let s = setup().await;
    let general = s.channel("Text", "general").await;
    let (_, thread) = s.start(&s.owner, &general, "Plans").await;
    let thread = id(&thread);
    s.deny_for_everyone(&general, IN_THREADS).await;

    s.server_role(ChannelPermission::ManageChannel as i64 | IN_THREADS, 0).await;
    assert_eq!(s.send(&s.member, &thread, "moderating").await.0, Status::Ok);
}

#[rocket::async_test]
async fn taking_send_messages_from_a_channel_still_closes_its_threads() {
    let s = setup().await;
    let general = s.channel("Text", "general").await;
    let forum = s.channel("Forum", "help").await;
    let (_, thread) = s.start(&s.owner, &general, "Plans").await;
    s.deny_for_everyone(&general, SEND).await;
    s.deny_for_everyone(&forum, SEND).await;

    refused(&s.send(&s.member, &id(&thread), "No").await, "SendMessage");
    refused(&s.start(&s.member, &general, "No").await, "SendMessage");
    refused(&s.start(&s.member, &forum, "No").await, "SendMessage");
}

#[rocket::async_test]
async fn forum_posts_have_their_own_permission_and_replies_use_the_thread_one() {
    let s = setup().await;
    let forum = s.channel("Forum", "help").await;
    let other = s.channel("Forum", "other").await;
    let (_, post) = s.start(&s.owner, &forum, "Rules").await;
    let post = id(&post);

    s.deny_for_everyone(&forum, CREATE_POSTS).await;
    refused(&s.start(&s.member, &forum, "No").await, "CreateForumPosts");
    // replying was not closed
    assert_eq!(s.send(&s.member, &post, "reply").await.0, Status::Ok);

    // replies closed: a new post still gets its first message
    s.deny_for_everyone(&other, IN_THREADS).await;
    let (status, opened) = s.start(&s.member, &other, "Mine").await;
    assert_eq!(status, Status::Ok, "{opened}");
    assert_eq!(opened["thread"]["message_count"], 1);
    refused(&s.send(&s.member, &id(&opened), "reply").await, "SendMessageInThreads");

    // Create Threads has no say in a forum, nor the forum one in a text channel
    let text = s.channel("Text", "text").await;
    s.deny_for_everyone(&text, CREATE_POSTS).await;
    assert_eq!(s.start(&s.member, &text, "Fine").await.0, Status::Ok);
    let third = s.channel("Forum", "third").await;
    s.deny_for_everyone(&third, CREATE_THREADS).await;
    assert_eq!(s.start(&s.member, &third, "Fine").await.0, Status::Ok);
}

#[rocket::async_test]
async fn threads_switched_off_are_read_only_for_everyone() {
    let s = setup().await;
    let general = s.channel("Text", "general").await;
    let forum = s.channel("Forum", "help").await;
    let (_, thread) = s.start(&s.member, &general, "Plans").await;
    let thread = id(&thread);
    let (_, message) = s.send(&s.member, &thread, "before").await;
    let message = id(&message);
    let (_, own) = s.send(&s.member, &thread, "mine").await;
    let own = id(&own);
    let reaction = format!("/channels/{thread}/messages/{message}/reactions/%F0%9F%91%8D");
    let (status, body) = s.call(&s.member, Put, reaction.clone(), None).await;
    assert_eq!(status, Status::NoContent, "{body}");
    let (_, post) = s.start(&s.owner, &forum, "Rules").await;

    s.switch("threads_disabled", true).await;

    for token in [&s.member, &s.owner] {
        switched_off(&s.start(token, &general, "No").await, "ThreadsDisabled");
        switched_off(&s.send(token, &thread, "No").await, "ThreadsDisabled");
    }
    switched_off(
        &s.call(&s.member, Patch, format!("/channels/{thread}/messages/{message}"), Some(json!({ "content": "x" }))).await,
        "ThreadsDisabled",
    );
    switched_off(
        &s.call(&s.member, Put, format!("/channels/{thread}/messages/{message}/reactions/01JNOSUCHEMOJINOSUCHEMOJI0"), None).await,
        "ThreadsDisabled",
    );
    switched_off(
        &s.call(&s.owner, Post, format!("/channels/{thread}/messages/{message}/pin"), None).await,
        "ThreadsDisabled",
    );
    switched_off(
        &s.call(&s.member, Patch, format!("/channels/{thread}/thread"), Some(json!({ "name": "x" }))).await,
        "ThreadsDisabled",
    );

    // readable; a moderator can still tidy up; nothing was deleted
    let (status, list) = s.call(&s.member, Get, format!("/channels/{general}/threads"), None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, messages) = s.call(&s.member, Get, format!("/channels/{thread}/messages"), None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(messages.as_array().unwrap().len(), 3);

    // taking away stays allowed: people keep control of what they wrote
    let (status, body) = s.call(&s.member, Delete, reaction, None).await;
    assert_eq!(status, Status::NoContent, "{body}");
    assert_eq!(
        s.call(&s.member, Delete, format!("/channels/{thread}/messages/{own}"), None).await.0,
        Status::NoContent
    );
    assert_eq!(
        s.call(&s.owner, Patch, format!("/channels/{thread}/thread"), Some(json!({ "archived": true }))).await.0,
        Status::Ok
    );
    assert_eq!(
        s.call(&s.owner, Delete, format!("/channels/{thread}/messages/{message}"), None).await.0,
        Status::NoContent
    );

    // forums and plain channels are not part of it
    assert_eq!(s.send(&s.member, &general, "here").await.0, Status::Ok);
    assert_eq!(s.start(&s.member, &forum, "Mine").await.0, Status::Ok);
    assert_eq!(s.send(&s.member, &id(&post), "reply").await.0, Status::Ok);

    // back on, as it was
    s.switch("threads_disabled", false).await;
    assert_eq!(s.send(&s.member, &thread, "again").await.0, Status::Ok);
    assert_eq!(s.start(&s.member, &general, "Again").await.0, Status::Ok);
}

#[rocket::async_test]
async fn forums_switched_off_are_read_only_and_no_forum_can_be_added() {
    let s = setup().await;
    let general = s.channel("Text", "general").await;
    let forum = s.channel("Forum", "help").await;
    let (_, post) = s.start(&s.member, &forum, "Rules").await;
    let post = id(&post);

    s.switch("forums_disabled", true).await;

    switched_off(
        &s.call(
            &s.owner,
            Post,
            format!("/servers/{}/channels", s.server.id),
            Some(json!({ "type": "Forum", "name": "new" })),
        )
        .await,
        "ForumsDisabled",
    );
    for token in [&s.member, &s.owner] {
        switched_off(&s.start(token, &forum, "No").await, "ForumsDisabled");
        switched_off(&s.send(token, &post, "No").await, "ForumsDisabled");
    }

    // text channels can still be made; the forum and its posts stay
    s.channel("Text", "more").await;
    let (status, list) = s.call(&s.member, Get, format!("/channels/{forum}/threads"), None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(s.call(&s.member, Get, format!("/channels/{post}/messages"), None).await.0, Status::Ok);

    // threads in text channels are not part of it
    assert_eq!(s.start(&s.member, &general, "Fine").await.0, Status::Ok);

    s.switch("forums_disabled", false).await;
    assert_eq!(s.send(&s.member, &post, "again").await.0, Status::Ok);
    s.channel("Forum", "second").await;
}

#[rocket::async_test]
async fn only_people_who_manage_the_server_flip_the_switches() {
    let s = setup().await;
    let path = format!("/servers/{}", s.server.id);
    for field in ["threads_disabled", "forums_disabled"] {
        refused(
            &s.call(&s.member, Patch, path.clone(), Some(json!({ field: true }))).await,
            "ManageServer",
        );
    }

    s.server_role(ChannelPermission::ManageServer as i64, 0).await;
    let (status, body) = s
        .call(&s.member, Patch, path.clone(), Some(json!({ "threads_disabled": true })))
        .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["threads_disabled"], true);

    // on by default: an untouched server does not carry the fields at all
    let (_, plain) = s.call(&s.owner, Patch, path, Some(json!({ "forums_disabled": false }))).await;
    assert!(plain.get("forums_disabled").is_none(), "{plain}");
}

#[rocket::async_test]
async fn bots_follow_the_same_permissions_and_switches() {
    let s = setup().await;
    let bot = s.bot_token();
    let general = s.channel("Text", "general").await;
    let forum = s.channel("Forum", "help").await;

    // allowed by default
    let (status, thread) = s.start(&bot, &general, "Bot thread").await;
    assert_eq!(status, Status::Ok, "{thread}");
    let thread = id(&thread);
    assert_eq!(s.send(&bot, &thread, "hello").await.0, Status::Ok);
    let (status, post) = s.start(&bot, &forum, "Bot post").await;
    assert_eq!(status, Status::Ok, "{post}");
    let post = id(&post);

    s.deny_for_everyone(&general, CREATE_THREADS | IN_THREADS).await;
    s.deny_for_everyone(&forum, CREATE_POSTS | IN_THREADS).await;
    refused(&s.start(&bot, &general, "No").await, "CreateThreads");
    refused(&s.send(&bot, &thread, "No").await, "SendMessageInThreads");
    refused(&s.start(&bot, &forum, "No").await, "CreateForumPosts");
    refused(&s.send(&bot, &post, "No").await, "SendMessageInThreads");

    s.deny_for_everyone(&general, 0).await;
    s.deny_for_everyone(&forum, 0).await;
    s.switch("threads_disabled", true).await;
    s.switch("forums_disabled", true).await;
    switched_off(&s.start(&bot, &general, "No").await, "ThreadsDisabled");
    switched_off(&s.send(&bot, &thread, "No").await, "ThreadsDisabled");
    switched_off(&s.start(&bot, &forum, "No").await, "ForumsDisabled");
    switched_off(&s.send(&bot, &post, "No").await, "ForumsDisabled");
}

#[rocket::async_test]
async fn a_webhook_needs_the_rights_itself() {
    let s = setup().await;
    let forum = s.channel("Forum", "help").await;
    let (_, post) = s.start(&s.owner, &forum, "Rules").await;
    let post = id(&post);

    // a default webhook posts and replies
    let hook = s.webhook(&forum, &s.member_user.id, *DEFAULT_WEBHOOK_PERMISSIONS).await;
    let new = json!({ "content": "report", "thread_name": "Nightly" });
    let reply = s.post_webhook(format!("{hook}?wait=true"), new.clone()).await;
    assert_eq!(reply.0, Status::Ok, "{}", reply.1);
    let reply = s
        .post_webhook(format!("{hook}?wait=true&thread_id={post}"), json!({ "content": "more" }))
        .await;
    assert_eq!(reply.0, Status::Ok, "{}", reply.1);

    // a webhook from before these permissions that was not migrated, or one
    // that had them taken away
    let bare = *DEFAULT_WEBHOOK_PERMISSIONS & !(*THREAD_PERMISSIONS);
    let hook = s.webhook(&forum, &s.member_user.id, bare).await;
    refused(&s.post_webhook(hook.clone(), new).await, "CreateForumPosts");
    refused(
        &s.post_webhook(format!("{hook}?thread_id={post}"), json!({ "content": "more" })).await,
        "SendMessageInThreads",
    );
}

#[rocket::async_test]
async fn a_webhook_is_closed_out_with_its_creator() {
    let s = setup().await;
    let forum = s.channel("Forum", "help").await;
    let (_, post) = s.start(&s.owner, &forum, "Rules").await;
    let post = id(&post);
    let new = json!({ "content": "report", "thread_name": "Nightly" });
    let reply = json!({ "content": "more" });

    let member_hook = s.webhook(&forum, &s.member_user.id, *DEFAULT_WEBHOOK_PERMISSIONS).await;
    let owner_hook = s.webhook(&forum, &s.owner_user.id, *DEFAULT_WEBHOOK_PERMISSIONS).await;
    let gone_hook = s.webhook(&forum, "01JNOSUCHUSERNOSUCHUSER000", *DEFAULT_WEBHOOK_PERMISSIONS).await;
    let (_, _, stranger) = s.harness.new_user().await;
    let left_hook = s.webhook(&forum, &stranger.id, *DEFAULT_WEBHOOK_PERMISSIONS).await;

    // a deny on the creator's role
    s.role_in_channel(&forum, 0, CREATE_POSTS | IN_THREADS).await;
    refused(&s.post_webhook(member_hook.clone(), new.clone()).await, "CreateForumPosts");
    refused(
        &s.post_webhook(format!("{member_hook}?thread_id={post}"), reply.clone()).await,
        "SendMessageInThreads",
    );
    // another creator's webhook isn't touched by that role
    let result = s.post_webhook(left_hook.clone(), new.clone()).await;
    assert_eq!(result.0, Status::NoContent, "{}", result.1);

    // closed for everyone: creators who left or no longer exist count as everyone
    s.deny_for_everyone(&forum, CREATE_POSTS | IN_THREADS).await;
    for hook in [&gone_hook, &left_hook] {
        refused(&s.post_webhook(hook.clone(), new.clone()).await, "CreateForumPosts");
        refused(
            &s.post_webhook(format!("{hook}?thread_id={post}"), reply.clone()).await,
            "SendMessageInThreads",
        );
    }
    // the server owner keeps access, as in any channel
    let result = s.post_webhook(owner_hook.clone(), new.clone()).await;
    assert_eq!(result.0, Status::NoContent, "{}", result.1);

    // the switch stops every webhook, the owner's too, through every route
    s.switch("forums_disabled", true).await;
    switched_off(&s.post_webhook(owner_hook.clone(), new.clone()).await, "ForumsDisabled");
    switched_off(
        &s.post_webhook(format!("{owner_hook}?thread_id={post}"), reply.clone()).await,
        "ForumsDisabled",
    );
    switched_off(
        &s.post_webhook(format!("{owner_hook}/slack?thread_id={post}"), json!({ "text": "from slack" })).await,
        "ForumsDisabled",
    );
}

/// Threads don't change what an unrelated state did to a webhook: a creator
/// who can't see the channel any more leaves its webhook working
#[rocket::async_test]
async fn a_webhook_keeps_working_when_its_creator_lost_sight_of_the_channel() {
    let s = setup().await;
    let forum = s.channel("Forum", "help").await;
    let hook = s.webhook(&forum, &s.member_user.id, *DEFAULT_WEBHOOK_PERMISSIONS).await;
    s.deny_for_everyone(&forum, VIEW).await;
    let result = s
        .post_webhook(hook, json!({ "content": "report", "thread_name": "Nightly" }))
        .await;
    assert_eq!(result.0, Status::NoContent, "{}", result.1);
}

#[rocket::async_test]
async fn a_webhook_cant_rewrite_its_messages_in_a_closed_thread() {
    let s = setup().await;
    let general = s.channel("Text", "general").await;
    let (_, thread) = s.start(&s.owner, &general, "Plans").await;
    let thread = id(&thread);
    let hook = s.webhook(&general, &s.member_user.id, *DEFAULT_WEBHOOK_PERMISSIONS).await;
    let (status, message) = s
        .post_webhook(format!("{hook}?wait=true&thread_id={thread}"), json!({ "content": "report" }))
        .await;
    assert_eq!(status, Status::Ok, "{message}");
    let path = format!("{hook}/messages/{}", id(&message));

    s.deny_for_everyone(&general, IN_THREADS).await;
    refused(
        &call_with(&s, None, Patch, path.clone(), Some(json!({ "content": "rewritten" }))).await,
        "SendMessageInThreads",
    );

    s.deny_for_everyone(&general, 0).await;
    let (status, body) = call_with(&s, None, Patch, path, Some(json!({ "content": "rewritten" }))).await;
    assert_eq!(status, Status::Ok, "{body}");
}

/// A member runs a bot command in `channel`
async fn run_command(
    s: &Setup,
    command: &revolt_models::v0::ApplicationCommand,
    channel: &str,
    nonce: &str,
) -> (Status, Value) {
    s.call(
        &s.member,
        Post,
        "/interactions".to_string(),
        Some(json!({
            "type": "Command",
            "application_id": s.bot.id,
            "channel_id": channel,
            "command_id": command.id,
            "command_version": command.version,
            "options": [],
            "nonce": nonce,
        })),
    )
    .await
}

/// The bot's answer lands in the thread, so a command counts as writing there
#[rocket::async_test]
async fn a_command_is_refused_where_its_user_cant_write() {
    let s = setup().await;
    revolt_presence::create_session(&s.bot.id, 0).await;
    let command = revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        None,
        serde_json::from_value(json!({ "name": "hello", "description": "Say hello" })).unwrap(),
    )
    .await
    .unwrap();
    let general = s.channel("Text", "general").await;
    let forum = s.channel("Forum", "help").await;
    let thread = id(&s.start(&s.owner, &general, "Plans").await.1);
    let post = id(&s.start(&s.owner, &forum, "Question").await.1);

    let (status, body) = run_command(&s, &command, &thread, "1").await;
    assert_eq!(status, Status::Ok, "{body}");

    s.deny_for_everyone(&general, IN_THREADS).await;
    s.deny_for_everyone(&forum, IN_THREADS).await;
    refused(&run_command(&s, &command, &thread, "2").await, "SendMessageInThreads");
    refused(&run_command(&s, &command, &post, "3").await, "SendMessageInThreads");

    s.deny_for_everyone(&general, 0).await;
    s.deny_for_everyone(&forum, 0).await;
    s.switch("threads_disabled", true).await;
    s.switch("forums_disabled", true).await;
    switched_off(&run_command(&s, &command, &thread, "4").await, "ThreadsDisabled");
    switched_off(&run_command(&s, &command, &post, "5").await, "ForumsDisabled");

    s.switch("threads_disabled", false).await;
    let (status, body) = run_command(&s, &command, &thread, "6").await;
    assert_eq!(status, Status::Ok, "{body}");
}

/// What the gateway asks before it passes on `BeginTyping`
async fn member_can_type(s: &Setup, channel: &str) -> bool {
    let channel = s.harness.db.fetch_channel(channel).await.unwrap();
    revolt_database::util::threads::assert_can_type(&s.harness.db, &s.member_user, &channel)
        .await
        .is_ok()
}

/// The REST route and the gateway share this rule, so showing as typing is
/// refused wherever writing is
#[rocket::async_test]
async fn typing_follows_the_right_to_write() {
    let s = setup().await;
    let general = s.channel("Text", "general").await;
    let forum = s.channel("Forum", "help").await;
    let thread = id(&s.start(&s.owner, &general, "Plans").await.1);
    let post = id(&s.start(&s.owner, &forum, "Question").await.1);
    let typing = |channel: &str| format!("/channels/{channel}/typing");

    assert_eq!(s.call(&s.member, Post, typing(&thread), None).await.0, Status::NoContent);
    assert!(member_can_type(&s, &thread).await);

    s.deny_for_everyone(&general, IN_THREADS).await;
    s.deny_for_everyone(&forum, IN_THREADS).await;
    refused(&s.call(&s.member, Post, typing(&thread), None).await, "SendMessageInThreads");
    assert!(!member_can_type(&s, &thread).await);
    assert!(!member_can_type(&s, &post).await);
    // a thread permission says nothing about the channel itself
    assert!(member_can_type(&s, &general).await);

    s.deny_for_everyone(&general, SEND).await;
    refused(&s.call(&s.member, Post, typing(&general), None).await, "SendMessage");
    assert!(!member_can_type(&s, &general).await);

    s.deny_for_everyone(&general, 0).await;
    s.deny_for_everyone(&forum, 0).await;
    s.switch("forums_disabled", true).await;
    assert!(member_can_type(&s, &thread).await);
    assert!(!member_can_type(&s, &post).await);
}
