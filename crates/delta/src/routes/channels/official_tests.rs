//! Official accounts: what the server refuses, however it is asked, and the
//! rule that keeps everyone else from passing for one.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{
    official_account, seed_official_accounts, send_official_message, Channel, Member, Message,
    OfficialAccount, PartialUser, Server, User,
};
use revolt_permissions::{ChannelPermission as P, OverrideField};
use rocket::http::{ContentType, Header, Method, Status};
use serde_json::{json, Value};
use ulid::Ulid;

/// `%F0%9F%91%8D`: a thumbs up
const EMOJI: &str = "%F0%9F%91%8D";

enum Auth {
    Session(String),
    Bot(String),
}

async fn call(
    harness: &TestHarness,
    auth: &Auth,
    method: Method,
    path: String,
    body: Option<Value>,
) -> (Status, Value) {
    let header = match auth {
        Auth::Session(token) => Header::new("x-session-token", token.clone()),
        Auth::Bot(token) => Header::new("x-bot-token", token.clone()),
    };
    let mut request = harness.client.req(method, path).header(header);
    if let Some(body) = body {
        request = request.header(ContentType::JSON).body(body.to_string());
    }
    let response = request.dispatch().await;
    let status = response.status();
    (status, response.into_json().await.unwrap_or(Value::Null))
}

fn refused(result: (Status, Value), what: &str) {
    assert_eq!(result.0, Status::Forbidden, "{what}: {}", result.1);
    assert_eq!(result.1["type"], "OfficialAccount", "{what}: {}", result.1);
}

struct Scene {
    harness: TestHarness,
    session: Auth,
    bot: Auth,
    owner: User,
    server: Server,
    channel: Channel,
    /// In the person's conversation with Zrzur Safety
    dm_channel: String,
    dm_message: String,
    /// Written in a server channel by a marked account
    server_message: Message,
    /// Written in the same channel by the owner
    ordinary: Message,
}

async fn scene() -> Scene {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let channel = channels.into_iter().next().unwrap();
    let (bot, bot_user) = harness.new_bot(&owner).await;
    Member::create(&harness.db, &server, &owner, None).await.unwrap();
    let (mut bot_member, _) = Member::create(&harness.db, &server, &bot_user, None).await.unwrap();
    // the bot may do all of it, so a refusal is the official rule and nothing else
    let bits = [P::React, P::ManageMessages, P::SendMessage]
        .iter()
        .fold(0i64, |bits, permission| bits | *permission as i64);
    let role = harness.new_role(&server, 1, Some(OverrideField { a: bits, d: 0 })).await;
    bot_member
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

    let message = send_official_message(
        &harness.db,
        None,
        OfficialAccount::Safety,
        &owner.id,
        "A decision".to_string(),
    )
    .await
    .unwrap();

    let official = official_account(&harness.db, OfficialAccount::Updates).await.unwrap();
    let in_channel = |author: &str, text: &str| Message {
        id: Ulid::new().to_string(),
        channel: channel.id().to_string(),
        author: author.to_string(),
        content: Some(text.to_string()),
        ..Default::default()
    };
    let server_message = in_channel(&official.id, "News");
    let ordinary = in_channel(&owner.id, "Hello");
    harness.db.insert_message(&server_message).await.unwrap();
    harness.db.insert_message(&ordinary).await.unwrap();

    Scene {
        session: Auth::Session(session.token),
        bot: Auth::Bot(bot.token.clone()),
        dm_channel: message.channel.clone(),
        dm_message: message.id,
        harness,
        owner,
        server,
        channel,
        server_message,
        ordinary,
    }
}

#[rocket::async_test]
async fn every_way_of_touching_an_official_message_is_refused() {
    let s = scene().await;
    let channel = s.channel.id();
    let message = &s.server_message.id;

    for auth in [&s.session, &s.bot] {
        for (method, path, what) in [
            (Method::Put, format!("/channels/{channel}/messages/{message}/reactions/{EMOJI}"), "react"),
            (Method::Delete, format!("/channels/{channel}/messages/{message}/reactions/{EMOJI}"), "unreact"),
            (Method::Delete, format!("/channels/{channel}/messages/{message}/reactions"), "clear reactions"),
            (Method::Post, format!("/channels/{channel}/messages/{message}/pin"), "pin"),
            (Method::Delete, format!("/channels/{channel}/messages/{message}/pin"), "unpin"),
        ] {
            refused(call(&s.harness, auth, method, path, None).await, what);
        }

        // a reply from anywhere, the only permission it needs is to write
        let reply = json!({
            "content": "hi",
            "replies": [{ "id": message, "mention": true }]
        });
        refused(
            call(&s.harness, auth, Method::Post, format!("/channels/{channel}/messages"), Some(reply)).await,
            "reply",
        );
    }

    // the owner has every permission: the refusal is the official rule
    let reply = json!({ "content": "hi", "replies": [{ "id": message, "mention": false }] });
    refused(
        call(&s.harness, &s.session, Method::Post, format!("/channels/{channel}/messages"), Some(reply)).await,
        "reply as the owner",
    );

    // the same actions on an ordinary message still work
    let ordinary = &s.ordinary.id;
    let (status, body) = call(
        &s.harness,
        &s.session,
        Method::Put,
        format!("/channels/{channel}/messages/{ordinary}/reactions/{EMOJI}"),
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent, "{body}");
    let (status, body) = call(&s.harness, &s.session, Method::Post, format!("/channels/{channel}/messages/{ordinary}/pin"), None).await;
    assert_eq!(status, Status::NoContent, "{body}");
    let reply = json!({ "content": "hi", "replies": [{ "id": ordinary, "mention": false }] });
    let (status, body) = call(&s.harness, &s.session, Method::Post, format!("/channels/{channel}/messages"), Some(reply)).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn the_conversation_with_an_official_account_is_read_only() {
    let s = scene().await;
    let (channel, message) = (&s.dm_channel, &s.dm_message);

    refused(
        call(&s.harness, &s.session, Method::Post, format!("/channels/{channel}/messages/{message}/pin"), None).await,
        "pin",
    );
    let (status, _) = call(&s.harness, &s.session, Method::Put, format!("/channels/{channel}/messages/{message}/reactions/{EMOJI}"), None).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = call(&s.harness, &s.session, Method::Post, format!("/channels/{channel}/messages"), Some(json!({ "content": "hi" }))).await;
    assert_eq!(status, Status::Forbidden);

    // reading and marking as read stay
    let (status, _) = call(&s.harness, &s.session, Method::Get, format!("/channels/{channel}/messages"), None).await;
    assert_eq!(status, Status::Ok);
    let (status, body) = call(&s.harness, &s.session, Method::Put, format!("/channels/{channel}/ack/{message}"), None).await;
    assert_eq!(status, Status::NoContent, "{body}");
}

#[rocket::async_test]
async fn official_accounts_take_no_friends_blocks_reports_or_new_conversations() {
    let s = scene().await;
    let official = OfficialAccount::Updates.id();

    for auth in [&s.session, &s.bot] {
        refused(call(&s.harness, auth, Method::Get, format!("/users/{official}/dm"), None).await, "new DM");
    }
    refused(call(&s.harness, &s.session, Method::Put, format!("/users/{official}/block"), None).await, "block");
    refused(call(&s.harness, &s.session, Method::Put, format!("/users/{official}/friend"), None).await, "friend");

    // every official account, not only the first one
    let safety = OfficialAccount::Safety.id();
    refused(call(&s.harness, &s.session, Method::Put, format!("/users/{safety}/block"), None).await, "block safety");
}

#[rocket::async_test]
async fn the_mark_decides_not_the_id() {
    let s = scene().await;
    // an ordinary account with the mark behaves the same
    let (_, _, mut marked) = s.harness.new_user().await;
    marked
        .update(
            &s.harness.db,
            PartialUser {
                official: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    refused(call(&s.harness, &s.session, Method::Put, format!("/users/{}/block", marked.id), None).await, "block");

    // and the app reads it
    let (status, user) = call(&s.harness, &s.session, Method::Get, format!("/users/{}", OfficialAccount::Safety.id()), None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(user["official"], true);
    assert_eq!(user["official_names"]["ar"], "أمان زرزور");
    assert_eq!(user["official_names"]["de"], "Zrzur-Sicherheit");
    assert_eq!(user["official_names"]["en"], "Zrzur Safety");

    // nobody can set it through the public edit
    let (status, body) = call(&s.harness, &s.session, Method::Patch, "/users/@me".to_string(), Some(json!({ "official": true }))).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body.get("official").is_none(), "{body}");
    assert!(!s.harness.db.fetch_user(&s.owner.id).await.unwrap().official);
}

#[rocket::async_test]
async fn seeding_is_safe_twice_and_at_the_same_moment() {
    let harness = TestHarness::new().await;
    tokio::join!(
        seed_official_accounts(&harness.db),
        seed_official_accounts(&harness.db),
        seed_official_accounts(&harness.db)
    );
    seed_official_accounts(&harness.db).await;

    for account in OfficialAccount::ALL {
        let user = harness.db.fetch_user(account.id()).await.expect("seeded");
        assert!(user.official, "{account:?}");
        assert!(harness.db.fetch_account(account.id()).await.is_err(), "no sign-in");
        assert_eq!(user.discriminator, "0001", "{account:?}");
        assert_eq!(user.official_names.as_ref().map(|names| names.len()), Some(3));
    }
}

#[rocket::async_test]
async fn the_old_account_keeps_its_conversations() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    // what a database from before the mark holds
    let old = OfficialAccount::Zrzur.id();
    let mut before = harness.db.fetch_user(old).await.unwrap();
    harness
        .db
        .update_user(
            old,
            &PartialUser {
                official: Some(false),
                ..Default::default()
            },
            vec![revolt_database::FieldsUser::Avatar],
        )
        .await
        .unwrap();
    before.official = false;
    let channel = Channel::create_dm(&harness.db, &before, &person).await.unwrap();

    seed_official_accounts(&harness.db).await;

    assert!(harness.db.fetch_user(old).await.unwrap().official);
    let same = harness.db.find_direct_message_channel(old, &person.id).await.unwrap();
    assert_eq!(same.id(), channel.id());
    let message = send_official_message(&harness.db, None, OfficialAccount::Zrzur, &person.id, "Hello".to_string())
        .await
        .unwrap();
    assert_eq!(message.channel, channel.id());
}

#[rocket::async_test]
async fn only_official_pictures_change_quickly() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    for account in OfficialAccount::ALL {
        let response = harness
            .client
            .get(format!("/users/{}/default_avatar", account.id()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let cache = response.headers().get_one("Cache-Control").unwrap().to_string();
        assert_eq!(cache, "public, max-age=3600", "{account:?}");
        assert!(!cache.contains("immutable"));
        assert_eq!(
            response.into_bytes().await.unwrap(),
            include_bytes!("../users/avatars/official.png").to_vec(),
            "{account:?}"
        );
    }

    // the address the apps use for browsers that hold the old picture
    let response = harness
        .client
        .get(format!("/users/{}/default_avatar?v=2", OfficialAccount::Zrzur.id()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    assert_eq!(response.headers().get_one("Cache-Control"), Some("public, max-age=3600"));

    let response = harness.client.get(format!("/users/{}/default_avatar", person.id)).dispatch().await;
    assert_eq!(response.headers().get_one("Cache-Control"), Some("public, max-age=31536000, immutable"));
}

#[test]
fn the_name_rule() {
    use revolt_database::passes_for_official as refused;
    for name in [
        "Zrzur",
        "ZRZUR",
        "zrzur support",
        "Zrzur Official",
        "Official Zrzur",
        "Zrzur Safety",
        "my zrzur fans",
        "Z r z u r",
        "z.r.z.u.r",
        "zzrrzzuurr",
        "Zrzuг",
        "Ｚｒｚｕｒ",
        "ℤrzur",
        "zr2ur",
        "2rzur",
        "zrzvr",
        "Zarzour",
        "Zarzur",
        "زرزور",
        "دعم زرزور",
        "الزرزور",
        "ز ر ز و ر",
        "زَرْزُور",
        "زرزـــور",
        "زرزوور",
        "ﺯﺭﺯﻭﺭ",
        "ژرزور",
    ] {
        assert!(refused(name), "{name} must be refused");
    }
    for name in [
        "Support",
        "Official",
        "Safety",
        "Customer Support",
        "Official Fan Club",
        "الدعم",
        "رسمي",
        "Hazel",
        "Razor",
        "زيد",
        "زهور",
    ] {
        assert!(!refused(name), "{name} must stay free");
    }
}

#[rocket::async_test]
async fn the_name_rule_covers_every_place_a_name_is_set() {
    let s = scene().await;
    let channel = s.channel.id();
    let names = ["Zrzur Fans", "زرزور", "Zrzuг Support"];

    for name in names {
        // display name
        let (status, body) = call(&s.harness, &s.session, Method::Patch, "/users/@me".to_string(), Some(json!({ "display_name": name }))).await;
        assert_eq!(status, Status::BadRequest, "display name {name}: {body}");
        // username
        let (status, body) = call(&s.harness, &s.session, Method::Patch, "/users/@me/username".to_string(), Some(json!({ "username": name, "password": "password" }))).await;
        assert_ne!(status, Status::Ok, "username {name}: {body}");
        // server nickname
        let path = format!("/servers/{}/members/{}", s.server.id, s.owner.id);
        let (status, body) = call(&s.harness, &s.session, Method::Patch, path, Some(json!({ "nickname": name }))).await;
        assert_eq!(status, Status::BadRequest, "nickname {name}: {body}");
        // masquerade
        let (status, body) = call(&s.harness, &s.session, Method::Post, format!("/channels/{channel}/messages"),
            Some(json!({ "content": "hi", "masquerade": { "name": name } }))).await;
        assert_eq!(status, Status::BadRequest, "masquerade {name}: {body}");
        // webhook, on create
        let (status, body) = call(&s.harness, &s.session, Method::Post, format!("/channels/{channel}/webhooks"), Some(json!({ "name": name }))).await;
        assert_ne!(status, Status::Ok, "webhook {name}: {body}");
        // bot, on create and on edit
        let (status, body) = call(&s.harness, &s.session, Method::Post, "/bots/create".to_string(), Some(json!({ "name": name }))).await;
        assert_ne!(status, Status::Ok, "new bot {name}: {body}");
    }

    let bot = s.harness.new_bot(&s.owner).await.0;
    let (status, body) = call(&s.harness, &s.session, Method::Patch, format!("/bots/{}", bot.id), Some(json!({ "name": "ZrzurBot" }))).await;
    assert_ne!(status, Status::Ok, "bot edit: {body}");

    // the plain words stay free
    let (status, body) = call(&s.harness, &s.session, Method::Patch, "/users/@me".to_string(), Some(json!({ "display_name": "Support Crew" }))).await;
    assert_eq!(status, Status::Ok, "{body}");
    let path = format!("/servers/{}/members/{}", s.server.id, s.owner.id);
    let (status, body) = call(&s.harness, &s.session, Method::Patch, path, Some(json!({ "nickname": "Safety" }))).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn a_name_from_before_the_rule_stays_until_it_changes() {
    let s = scene().await;
    let mut owner = s.harness.db.fetch_user(&s.owner.id).await.unwrap();
    owner
        .update(
            &s.harness.db,
            PartialUser {
                display_name: Some("Zrzur Fan".to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // saving the profile with the same name works, nothing is changed
    let (status, body) = call(&s.harness, &s.session, Method::Patch, "/users/@me".to_string(), Some(json!({ "display_name": "Zrzur Fan" }))).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(s.harness.db.fetch_user(&s.owner.id).await.unwrap().display_name.as_deref(), Some("Zrzur Fan"));

    // another name that breaks the rule does not
    let (status, _) = call(&s.harness, &s.session, Method::Patch, "/users/@me".to_string(), Some(json!({ "display_name": "Zrzur Fan 2" }))).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn only_the_server_gives_a_system_bot_its_name() {
    let s = scene().await;

    // nobody asks for the name, or a lookalike, through the API
    for name in ["zrzur-assistant", "Zrzur-Assistant", "zrzuг-assistant", "zarzur-assistant"] {
        let (status, body) = call(&s.harness, &s.session, Method::Post, "/bots/create".to_string(), Some(json!({ "name": name }))).await;
        assert_eq!(status, Status::BadRequest, "new bot {name}: {body}");
    }
    let plain = s.harness.new_bot(&s.owner).await.0;
    let (status, _) = call(&s.harness, &s.session, Method::Patch, format!("/bots/{}", plain.id), Some(json!({ "name": "zrzur-assistant" }))).await;
    assert_eq!(status, Status::BadRequest);

    // the allowlist does not stretch
    assert!(revolt_database::create_system_bot(&s.harness.db, "zrzur-helper", &s.owner).await.is_err());

    // the server can, once: the bot is marked and its token works
    let (bot, user) = revolt_database::create_system_bot(&s.harness.db, "zrzur-assistant", &s.owner)
        .await
        .unwrap();
    assert_eq!(user.username, "zrzur-assistant");
    assert_eq!(user.display_name.as_deref(), Some("زرزور المساعد"));
    assert!(user.official_bot && !user.official);
    let (status, seen) = call(&s.harness, &Auth::Bot(bot.token.clone()), Method::Get, "/users/@me".to_string(), None).await;
    assert_eq!(status, Status::Ok, "{seen}");
    assert_eq!(seen["official_bot"], true);
    assert!(seen.get("official").is_none(), "{seen}");

    // an official bot is an ordinary bot to talk to
    let (status, shown) = call(&s.harness, &s.session, Method::Get, format!("/users/{}", user.id), None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(shown["official_bot"], true);
    let (status, body) = call(&s.harness, &s.session, Method::Get, format!("/users/{}/dm", user.id), None).await;
    assert_ne!(body["type"], "OfficialAccount", "{status} {body}");

    // its name stays what the server gave it, even for its owner
    let (status, body) = call(&s.harness, &s.session, Method::Patch, format!("/bots/{}", bot.id), Some(json!({ "name": "somethingelse" }))).await;
    refused((status, body), "rename");
}

#[rocket::async_test]
async fn the_official_bot_mark_is_only_for_the_allowlist() {
    let s = scene().await;
    let (bot, _) = s.harness.new_bot(&s.owner).await;

    // an ordinary bot can't get it
    assert!(revolt_database::set_official_bot(&s.harness.db, &bot.id, true).await.is_err());
    // nor a person
    assert!(revolt_database::set_official_bot(&s.harness.db, &s.owner.id, true).await.is_err());
    assert!(!s.harness.db.fetch_user(&bot.id).await.unwrap().official_bot);

    // no public route writes it
    let (status, body) = call(&s.harness, &s.session, Method::Patch, format!("/users/{}", bot.id), Some(json!({ "official_bot": true }))).await;
    assert_eq!(status, Status::Ok, "{body}");
    let (status, body) = call(&s.harness, &s.session, Method::Patch, format!("/bots/{}", bot.id), Some(json!({ "official_bot": true }))).await;
    assert!(status == Status::Ok || status.code >= 400, "{body}");
    assert!(!s.harness.db.fetch_user(&bot.id).await.unwrap().official_bot);

    // a bot already called that, from before the rule, can be marked and unmarked
    let (old, mut old_user) = s.harness.new_bot(&s.owner).await;
    s.harness
        .db
        .update_user(
            &old.id,
            &PartialUser { username: Some("zrzur-assistant".to_string()), ..Default::default() },
            vec![],
        )
        .await
        .unwrap();
    old_user.username = "zrzur-assistant".to_string();
    assert!(revolt_database::set_official_bot(&s.harness.db, &old.id, true).await.unwrap().official_bot);
    assert!(!revolt_database::set_official_bot(&s.harness.db, &old.id, false).await.unwrap().official_bot);
}
