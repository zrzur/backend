//! Bot platform audit of 2026-11-21: one test per backend bug (section A).

use std::collections::HashSet;
use std::time::Duration;

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_database::{events::client::EventV1, Bot, Channel, Member, PartialBot, PartialUser, Session, User};
use revolt_models::v0;
use revolt_permissions::ChannelPermission;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

async fn call(
    harness: &TestHarness,
    method: &str,
    path: String,
    auth: Header<'static>,
    body: Option<Value>,
) -> (Status, String) {
    let client = &harness.client;
    let request = match method {
        "GET" => client.get(path),
        "POST" => client.post(path),
        "PUT" => client.put(path),
        "PATCH" => client.patch(path),
        "DELETE" => client.delete(path),
        _ => unreachable!(),
    }
    .header(auth);
    let request = match body {
        Some(body) => request.header(ContentType::JSON).body(body.to_string()),
        None => request,
    };
    let response = request.dispatch().await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

fn session(session: &Session) -> Header<'static> {
    Header::new("x-session-token", session.token.clone())
}

fn bot_token(bot: &Bot) -> Header<'static> {
    Header::new("x-bot-token", bot.token.clone())
}

async fn invite(harness: &TestHarness, owner: &Session, bot: &Bot, server: &str, permissions: Option<i64>) -> (Status, String) {
    let query = permissions.map(|p| format!("?permissions={p}")).unwrap_or_default();
    call(
        harness,
        "POST",
        format!("/bots/{}/invite{query}", bot.id),
        session(owner),
        Some(json!({ "server": server })),
    )
    .await
}

async fn set_user_flags(harness: &TestHarness, user: &User, flags: i32) {
    harness
        .db
        .update_user(
            &user.id,
            &PartialUser {
                flags: Some(flags),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    // what the safety actions do after changing someone's standing
    revolt_database::forget_owner_standing(&user.id);
}

/// A4: a suspended or banned owner's bot stops: its token is refused, it is
/// added nowhere, and it works again when the owner's standing is restored
#[rocket::async_test]
async fn a_banned_owners_bot_is_refused() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    harness
        .db
        .update_bot(&bot.id, &PartialBot { public: Some(true), ..Default::default() }, vec![])
        .await
        .unwrap();
    let (server, _) = harness.new_server(&other).await;

    let (status, _) = call(&harness, "GET", "/users/@me".into(), bot_token(&bot), None).await;
    assert_eq!(status, Status::Ok);

    for flags in [v0::UserFlags::Banned as i32, v0::UserFlags::SuspendedUntil as i32] {
        set_user_flags(&harness, &owner, flags).await;
        let (status, body) = call(&harness, "GET", "/users/@me".into(), bot_token(&bot), None).await;
        assert_eq!(status, Status::Unauthorized, "flags {flags}: {body}");
        assert!(body.contains("InvalidSession"), "{body}");
        let (status, body) = invite(&harness, &other_session, &bot, &server.id, None).await;
        assert_eq!(status, Status::NotFound, "flags {flags}: {body}");
        assert!(harness.db.fetch_member(&server.id, &bot.id).await.is_err());
    }

    set_user_flags(&harness, &owner, 0).await;
    let (status, _) = call(&harness, "GET", "/users/@me".into(), bot_token(&bot), None).await;
    assert_eq!(status, Status::Ok, "the bot works again with its owner");
    let _ = owner_session;
}

/// A6: deleting a bot takes it out of its groups, closes its connections,
/// and is not stopped by a membership left behind by a server that is gone
#[rocket::async_test]
async fn deleting_a_bot_leaves_groups_and_survives_a_gone_server() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (bot, bot_user) = harness.new_bot(&owner).await;

    let group = Channel::create_group(
        &harness.db,
        v0::DataCreateGroup {
            name: TestHarness::rand_string(),
            users: HashSet::new(),
            ..Default::default()
        },
        owner.id.clone(),
    )
    .await
    .unwrap();
    let (status, body) = call(
        &harness,
        "POST",
        format!("/bots/{}/invite", bot.id),
        session(&owner_session),
        Some(json!({ "group": group.id() })),
    )
    .await;
    assert_eq!(status, Status::NoContent, "{body}");

    // a server that disappeared without taking the bot's membership along
    let (gone, _) = harness.new_server(&owner).await;
    Member::create(&harness.db, &gone, &bot_user, None).await.unwrap();
    harness.db.delete_server(&gone.id).await.unwrap();

    // Logout travels on the bot's private topic
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", bot.id)).await;
    // deleting needs the owner to sign in again
    let ticket = revolt_database::MFATicket::new(owner.id.to_string(), true);
    ticket.save(&harness.db).await.unwrap();
    let response = harness
        .client
        .delete(format!("/bots/{}", bot.id))
        .header(session(&owner_session))
        .header(Header::new("x-mfa-ticket", ticket.token.clone()))
        .dispatch()
        .await;
    let status = response.status();
    let body = response.into_string().await.unwrap_or_default();
    assert_eq!(status, Status::NoContent, "{body}");

    let channel = harness.db.fetch_channel(group.id()).await.unwrap();
    let Channel::Group { recipients, .. } = &channel else {
        unreachable!()
    };
    assert!(!recipients.contains(&bot.id), "the bot left the group");
    assert!(harness.db.fetch_member(&gone.id, &bot.id).await.is_err());
    tokio::time::timeout(
        Duration::from_secs(3),
        bot_events.wait_for_event(|event| matches!(event, EventV1::Logout)),
    )
    .await
    .expect("its connections were told to close");
}

/// A7 (owner's decision): an install without `?permissions=` gives the bot
/// its default permissions through a role of its own; an explicit value still
/// wins
#[rocket::async_test]
async fn an_install_without_permissions_gives_the_default_ones() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    let defaults = ChannelPermission::SendMessage as i64 | ChannelPermission::ManageMessages as i64;
    harness
        .db
        .update_bot(
            &bot.id,
            &PartialBot {
                default_permissions: Some(defaults),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let role_bits = |harness: &TestHarness, server: String| {
        let db = harness.db.clone();
        let bot = bot.id.clone();
        async move {
            let member = db.fetch_member(&server, &bot).await.expect("the bot joined");
            let server = db.fetch_server(&server).await.unwrap();
            member
                .roles
                .iter()
                .filter_map(|id| server.roles.get(id))
                .map(|role| role.permissions.a)
                .fold(0, |all, bits| all | bits)
        }
    };

    let (server, _) = harness.new_server(&owner).await;
    let (status, body) = invite(&harness, &owner_session, &bot, &server.id, None).await;
    assert_eq!(status, Status::NoContent, "{body}");
    assert_eq!(role_bits(&harness, server.id.clone()).await, defaults);

    let (server, _) = harness.new_server(&owner).await;
    let explicit = ChannelPermission::SendMessage as i64;
    let (status, body) = invite(&harness, &owner_session, &bot, &server.id, Some(explicit)).await;
    assert_eq!(status, Status::NoContent, "{body}");
    assert_eq!(role_bits(&harness, server.id.clone()).await, explicit);
}

/// A12: a sync that changes nothing does not tell every server to refetch
#[rocket::async_test]
async fn an_unchanged_command_sync_tells_nobody() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    let (server, _) = harness.new_server(&owner).await;
    let (status, body) = invite(&harness, &owner_session, &bot, &server.id, None).await;
    assert_eq!(status, Status::NoContent, "{body}");

    let commands = json!([{ "name": "hello", "description": "Say hello" }]);
    let mut server_events = PubSubTestHelper::new(&server.id).await;
    let (status, body) = call(
        &harness,
        "PUT",
        format!("/applications/{}/commands", bot.id),
        bot_token(&bot),
        Some(commands.clone()),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    tokio::time::timeout(
        Duration::from_secs(3),
        server_events.wait_for_event(|event| matches!(event, EventV1::ApplicationCommandsUpdate { .. })),
    )
    .await
    .expect("a new command is announced");

    let mut quiet = PubSubTestHelper::new(&server.id).await;
    let (status, _) = call(
        &harness,
        "PUT",
        format!("/applications/{}/commands", bot.id),
        bot_token(&bot),
        Some(commands),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(700),
            quiet.wait_for_event(|event| matches!(event, EventV1::ApplicationCommandsUpdate { .. })),
        )
        .await
        .is_err(),
        "the same list again tells nobody"
    );
}

/// A14: a refused edit changes nothing; someone else gets "not found" before
/// anything else is looked at; unknown permission bits are refused
#[rocket::async_test]
async fn a_refused_bot_edit_changes_nothing() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, other_session, _) = harness.new_user().await;
    let (bot, bot_user) = harness.new_bot(&owner).await;

    let (status, body) = call(
        &harness,
        "PATCH",
        format!("/bots/{}", bot.id),
        session(&owner_session),
        Some(json!({ "name": "renamedbot", "interactions_url": "javascript:alert(1)" })),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{body}");
    let user = harness.db.fetch_user(&bot.id).await.unwrap();
    assert_eq!(user.username, bot_user.username, "the name was not changed");

    let (status, body) = call(
        &harness,
        "PATCH",
        format!("/bots/{}", bot.id),
        session(&owner_session),
        Some(json!({ "default_permissions": 1i64 << 62 })),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{body}");

    // not the owner: not found, even when the request also lacks a ticket
    let (status, body) = call(
        &harness,
        "PATCH",
        format!("/bots/{}", bot.id),
        session(&other_session),
        Some(json!({ "remove": ["Token"] })),
    )
    .await;
    assert_eq!(status, Status::NotFound, "{body}");
}

/// The Apps list of a server, as its owner sees it
async fn apps(harness: &TestHarness, owner: &Session, server: &str) -> Vec<Value> {
    let (status, body) = call(harness, "GET", format!("/servers/{server}/applications"), session(owner), None).await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// A22: a bot that is kicked is no longer listed as installed
#[rocket::async_test]
async fn a_kicked_bot_is_no_longer_installed() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    let (server, _) = harness.new_server(&owner).await;
    let (status, body) = invite(&harness, &owner_session, &bot, &server.id, None).await;
    assert_eq!(status, Status::NoContent, "{body}");

    assert_eq!(apps(&harness, &owner_session, &server.id).await.len(), 1);

    let (status, body) = call(
        &harness,
        "DELETE",
        format!("/servers/{}/members/{}", server.id, bot.id),
        session(&owner_session),
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent, "{body}");
    assert!(
        apps(&harness, &owner_session, &server.id).await.is_empty(),
        "the Apps list does not show a bot that is gone"
    );
}

/// A server of `owner` where everyone holds only `everyone` (as its default
/// permissions), with `installer` as a member
async fn server_for_installer(harness: &TestHarness, owner: &User, installer: &User, everyone: i64) -> String {
    let (server, _) = harness.new_server(owner).await;
    harness
        .db
        .update_server(
            &server.id,
            &revolt_database::PartialServer {
                default_permissions: Some(everyone),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    Member::create(&harness.db, &server, installer, None).await.unwrap();
    server.id
}

/// A7, second review: "no permissions" ticked in the add dialog (an explicit
/// 0) gives no role, and an installer without Manage Roles adds the bot
/// without its defaults but cannot name permissions
#[rocket::async_test]
async fn an_install_with_nothing_ticked_gives_no_role() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, installer_session, installer) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    let defaults = ChannelPermission::SendMessage as i64;
    harness
        .db
        .update_bot(
            &bot.id,
            &PartialBot {
                public: Some(true),
                default_permissions: Some(defaults),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let roles = |server: String| {
        let db = harness.db.clone();
        let bot = bot.id.clone();
        async move { db.fetch_member(&server, &bot).await.expect("the bot joined").roles }
    };

    // everything unticked: no role, nothing recorded as given
    let (server, _) = harness.new_server(&owner).await;
    let (status, body) = invite(&harness, &owner_session, &bot, &server.id, Some(0)).await;
    assert_eq!(status, Status::NoContent, "{body}");
    assert!(roles(server.id.clone()).await.is_empty(), "no role for an explicit 0");
    let installs = harness.db.fetch_app_installs(&server.id).await.unwrap();
    assert_eq!(installs[0].permissions, 0);

    // Manage Server without Manage Roles
    let everyone = ChannelPermission::ManageServer as i64
        | ChannelPermission::ViewChannel as i64
        | ChannelPermission::SendMessage as i64;
    for (permissions, expected) in [(None, Status::NoContent), (Some(0), Status::NoContent), (Some(defaults), Status::Forbidden)] {
        let server = server_for_installer(&harness, &owner, &installer, everyone).await;
        let (status, body) = invite(&harness, &installer_session, &bot, &server, permissions).await;
        assert_eq!(status, expected, "permissions {permissions:?}: {body}");
        if expected == Status::NoContent {
            assert!(roles(server).await.is_empty(), "{permissions:?}: no role without Manage Roles");
        } else {
            assert!(body.contains("MissingPermission"), "{body}");
        }
    }
}

/// The owner's report of 2026-11-21: a bot named "Liro Bot" could not be
/// made. A name with single spaces is the bot's display name, its username
/// has `_` for the spaces; anything else is refused as before
#[rocket::async_test]
async fn a_bot_name_may_have_spaces_between_words() {
    let harness = TestHarness::new().await;
    let (_, owner_session, _) = harness.new_user().await;
    let create = |name: &str| {
        call(
            &harness,
            "POST",
            "/bots/create".into(),
            session(&owner_session),
            Some(json!({ "name": name })),
        )
    };

    let (status, body) = create("Liro Bot").await;
    assert_eq!(status, Status::Ok, "{body}");
    let made: Value = serde_json::from_str(&body).unwrap();
    let id = made["_id"].as_str().unwrap().to_string();
    let user = harness.db.fetch_user(&id).await.unwrap();
    assert_eq!(user.username, "Liro_Bot");
    assert_eq!(user.display_name.as_deref(), Some("Liro Bot"));

    // a plain name is as before: the username, no display name
    let (status, body) = create("Plainbot").await;
    assert_eq!(status, Status::Ok, "{body}");
    let made: Value = serde_json::from_str(&body).unwrap();
    let plain = harness.db.fetch_user(made["_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(plain.username, "Plainbot");
    assert_eq!(plain.display_name, None);

    for refused in ["Liro  Bot", " Liro", "Liro ", "Liro\u{200B}Bot", "Liro\nBot", "L"] {
        let (status, body) = create(refused).await;
        assert_eq!(status, Status::BadRequest, "{refused:?}: {body}");
        assert!(body.contains("FailedValidation"), "{body}");
    }

    // renaming: spaces again, then a plain name clears the display name
    let rename = |name: &str| {
        call(
            &harness,
            "PATCH",
            format!("/bots/{id}"),
            session(&owner_session),
            Some(json!({ "name": name })),
        )
    };
    let (status, body) = rename("Mira Bot").await;
    assert_eq!(status, Status::Ok, "{body}");
    let user = harness.db.fetch_user(&id).await.unwrap();
    assert_eq!((user.username.as_str(), user.display_name.as_deref()), ("Mira_Bot", Some("Mira Bot")));
    let (status, body) = rename("Mira").await;
    assert_eq!(status, Status::Ok, "{body}");
    let user = harness.db.fetch_user(&id).await.unwrap();
    assert_eq!((user.username.as_str(), user.display_name.as_deref()), ("Mira", None));
    let (status, _) = rename("Mira  Bot").await;
    assert_eq!(status, Status::BadRequest);
}
