//! Threads and forum posts: permissions follow the parent channel.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{util::reference::Reference, Channel, Member, Server};
use revolt_permissions::ChannelPermission;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

const VIEW: i64 = ChannelPermission::ViewChannel as i64;

struct Setup {
    harness: TestHarness,
    owner: String,
    server: Server,
    member: String,
}

/// A server with its owner and one member
async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    let (_, member_session, member) = harness.new_user().await;
    Member::create(&harness.db, &server, &member, None).await.unwrap();
    Setup {
        harness,
        owner: owner_session.token,
        server,
        member: member_session.token,
    }
}

impl Setup {
    async fn call(&self, token: &str, method: rocket::http::Method, path: String, body: Option<Value>) -> (Status, Value) {
        let mut req = self
            .harness
            .client
            .req(method, path)
            .header(Header::new("x-session-token", token.to_string()));
        if let Some(body) = body {
            req = req.header(ContentType::JSON).body(body.to_string());
        }
        let res = req.dispatch().await;
        let status = res.status();
        (status, res.into_json().await.unwrap_or(Value::Null))
    }

    async fn channel(&self, body: Value) -> String {
        let (status, channel) = self
            .call(&self.owner, rocket::http::Method::Post, format!("/servers/{}/channels", self.server.id), Some(body))
            .await;
        assert_eq!(status, Status::Ok, "{channel}");
        channel["_id"].as_str().unwrap().to_string()
    }

    async fn send(&self, token: &str, channel: &str, content: &str) -> (Status, Value) {
        self.call(token, rocket::http::Method::Post, format!("/channels/{channel}/messages"), Some(json!({ "content": content })))
            .await
    }

    async fn thread(&self, token: &str, parent: &str, body: Value) -> (Status, Value) {
        self.call(token, rocket::http::Method::Post, format!("/channels/{parent}/threads"), Some(body)).await
    }

    async fn stored(&self, id: &str) -> Channel {
        Reference::from_unchecked(id).as_channel(&self.harness.db).await.unwrap()
    }
}

fn thread_info(channel: &Channel) -> revolt_database::ThreadInformation {
    match channel {
        Channel::TextChannel { thread: Some(thread), .. } => thread.clone(),
        _ => panic!("not a thread"),
    }
}

#[rocket::async_test]
async fn a_thread_starts_from_a_message_once_and_counts_its_messages() {
    use rocket::http::Method::Get;
    let s = setup().await;
    let general = s.channel(json!({ "type": "Text", "name": "general" })).await;
    let (_, message) = s.send(&s.member, &general, "who's up for a match?").await;
    let message = message["_id"].as_str().unwrap().to_string();

    let (status, thread) = s.thread(&s.member, &general, json!({ "name": "Match tonight", "message": message })).await;
    assert_eq!(status, Status::Ok, "{thread}");
    let thread_id = thread["_id"].as_str().unwrap().to_string();
    assert_eq!(thread["parent"], general.as_str());
    assert_eq!(thread["thread"]["starter_message"], message.as_str());

    // one thread per message
    let (status, _) = s.thread(&s.member, &general, json!({ "name": "Again", "message": message })).await;
    assert_eq!(status, Status::BadRequest);

    // listed, and its messages are counted
    let (status, list) = s.call(&s.member, Get, format!("/channels/{general}/threads"), None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list.as_array().unwrap().len(), 1);
    for text in ["I'm in", "me too"] {
        let (status, _) = s.send(&s.member, &thread_id, text).await;
        assert_eq!(status, Status::Ok);
    }
    assert_eq!(thread_info(&s.stored(&thread_id).await).message_count, 2);

    // threads are not in the server's channel list
    let server = Reference::from_unchecked(&s.server.id).as_server(&s.harness.db).await.unwrap();
    assert!(!server.channels.contains(&thread_id));
}

#[rocket::async_test]
async fn a_thread_uses_its_parent_channels_permissions() {
    use rocket::http::Method::Get;
    let s = setup().await;
    let staff = s.channel(json!({ "type": "Text", "name": "staff" })).await;
    let (status, thread) = s.thread(&s.owner, &staff, json!({ "name": "Plans", "content": "first" })).await;
    assert_eq!(status, Status::Ok, "{thread}");
    let thread_id = thread["_id"].as_str().unwrap().to_string();
    assert_eq!(thread_info(&s.stored(&thread_id).await).message_count, 1);

    // the member sees the parent, so the thread too
    let (status, _) = s.send(&s.member, &thread_id, "hello").await;
    assert_eq!(status, Status::Ok);

    // hide the parent: the thread is hidden as well
    let (status, _) = s
        .call(
            &s.owner,
            rocket::http::Method::Put,
            format!("/channels/{staff}/permissions/default"),
            Some(json!({ "permissions": { "allow": 0, "deny": VIEW } })),
        )
        .await;
    assert_eq!(status, Status::Ok);
    let (status, _) = s.send(&s.member, &thread_id, "still here?").await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = s.call(&s.member, Get, format!("/channels/{staff}/threads"), None).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = s.thread(&s.member, &staff, json!({ "name": "Sneaky", "content": "x" })).await;
    assert_eq!(status, Status::Forbidden);
}

#[rocket::async_test]
async fn locked_threads_are_for_moderators_and_messages_reopen_archived_ones() {
    use rocket::http::Method::Patch;
    let s = setup().await;
    let general = s.channel(json!({ "type": "Text", "name": "general" })).await;
    let (_, thread) = s.thread(&s.member, &general, json!({ "name": "Ideas", "content": "first" })).await;
    let thread_id = thread["_id"].as_str().unwrap().to_string();
    let path = format!("/channels/{thread_id}/thread");

    // the owner renames and archives their thread, but can't lock it
    let (status, _) = s.call(&s.member, Patch, path.clone(), Some(json!({ "name": "Good ideas", "archived": true }))).await;
    assert_eq!(status, Status::Ok);
    assert!(thread_info(&s.stored(&thread_id).await).archived);
    let (status, _) = s.call(&s.member, Patch, path.clone(), Some(json!({ "locked": true }))).await;
    assert_eq!(status, Status::Forbidden);

    // a message reopens it
    let (status, _) = s.send(&s.member, &thread_id, "one more").await;
    assert_eq!(status, Status::Ok);
    assert!(!thread_info(&s.stored(&thread_id).await).archived);

    // locked by a moderator (the owner here): members can't write
    let (status, _) = s.call(&s.owner, Patch, path.clone(), Some(json!({ "locked": true }))).await;
    assert_eq!(status, Status::Ok);
    let (status, _) = s.send(&s.member, &thread_id, "can I?").await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = s.send(&s.owner, &thread_id, "closing this").await;
    assert_eq!(status, Status::Ok);

    // someone else's thread: not theirs to change
    let (_, other, _) = s.harness.new_user().await;
    let (status, _) = s.call(&other.token, Patch, path, Some(json!({ "name": "Mine now" }))).await;
    assert_eq!(status, Status::Forbidden);
}

#[rocket::async_test]
async fn forum_posts_are_threads_with_a_first_message_and_tags() {
    use rocket::http::Method::Patch;
    let s = setup().await;
    let forum = s.channel(json!({ "type": "Forum", "name": "help" })).await;
    assert!(matches!(s.stored(&forum).await, Channel::TextChannel { forum: Some(_), .. }));

    // nobody writes in the forum itself
    let (status, _) = s.send(&s.member, &forum, "hi").await;
    assert_eq!(status, Status::BadRequest);

    // tags: set by someone who can manage the channel
    let (status, body) = s
        .call(
            &s.owner,
            Patch,
            format!("/channels/{forum}"),
            Some(json!({ "forum_tags": [ { "id": "bug", "name": "Bug" }, { "id": "idea", "name": "فكرة" } ], "require_tag": true })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{body}");

    // a post needs a first message and, here, a known tag
    let (status, _) = s.thread(&s.member, &forum, json!({ "name": "Crash on start" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = s.thread(&s.member, &forum, json!({ "name": "Crash on start", "content": "It crashes." })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = s.thread(&s.member, &forum, json!({ "name": "Crash on start", "content": "It crashes.", "tags": ["nope"] })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, post) = s.thread(&s.member, &forum, json!({ "name": "Crash on start", "content": "It crashes.", "tags": ["bug"] })).await;
    assert_eq!(status, Status::Ok, "{post}");
    assert_eq!(post["thread"]["tags"], json!(["bug"]));
    assert_eq!(post["thread"]["message_count"], 1);
}

/// A tag's emoji is an emoji (Unicode or a custom emoji id), never text
#[rocket::async_test]
async fn forum_tag_emoji_is_an_emoji() {
    use rocket::http::Method::Patch;
    let s = setup().await;
    let forum = s.channel(json!({ "type": "Forum", "name": "help" })).await;
    let tags = |emoji: &str| json!({ "forum_tags": [ { "id": "bug", "name": "Bug", "emoji": emoji } ] });

    for good in ["🐞", "1️⃣", "👩‍💻", "01JABCDEFGHJKMNPQRSTVWXYZ0"] {
        let (status, body) = s.call(&s.owner, Patch, format!("/channels/{forum}"), Some(tags(good))).await;
        assert_eq!(status, Status::Ok, "{good}: {body}");
    }
    for bad in ["csc", "", "🐞 bug", "a🐞", "01jabcdefghjkmnpqrstvwxyz0"] {
        let (status, _) = s.call(&s.owner, Patch, format!("/channels/{forum}"), Some(tags(bad))).await;
        assert_eq!(status, Status::BadRequest, "{bad}");
    }
}

#[rocket::async_test]
async fn threads_go_with_their_channel_and_follow_its_mature_flag() {
    let s = setup().await;
    let general = s.channel(json!({ "type": "Text", "name": "general" })).await;
    let (_, thread) = s.thread(&s.owner, &general, json!({ "name": "Side", "content": "x" })).await;
    let thread_id = thread["_id"].as_str().unwrap().to_string();

    // mature with its channel
    let (status, _) = s
        .call(&s.owner, rocket::http::Method::Patch, format!("/channels/{general}"), Some(json!({ "nsfw": true })))
        .await;
    assert_eq!(status, Status::Ok);
    assert!(s.stored(&thread_id).await.is_nsfw());

    // deleting the channel deletes its threads
    let (status, _) = s.call(&s.owner, rocket::http::Method::Delete, format!("/channels/{general}"), None).await;
    assert_eq!(status, Status::NoContent);
    assert!(Reference::from_unchecked(&thread_id).as_channel(&s.harness.db).await.is_err());
}
