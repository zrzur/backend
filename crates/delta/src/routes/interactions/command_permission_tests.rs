//! Command permission rules (Server settings → Apps → a bot) and commands in
//! a DM with the bot.

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_database::{
    events::client::EventV1, AuditLogEntryAction, AuditLogQuery, Bot, Channel, Member,
    PartialMember, Server,
};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

struct Setup {
    harness: TestHarness,
    bot: Bot,
    owner_session: String,
    user_id: String,
    user_session: String,
    other_id: String,
    other_session: String,
    server: Server,
    channel: Channel,
    second_channel: String,
    role: String,
    command: v0::ApplicationCommand,
}

/// Owner, a member with the role "Helpers", a bystander, a bot with `/hello`
async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, user_session, user) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let (bot, bot_user) = harness.new_bot(&owner).await;
    for member in [&owner, &user, &other, &bot_user] {
        Member::create(&harness.db, &server, member, None).await.unwrap();
    }
    revolt_presence::create_session(&bot.id, 0).await;

    let response = harness
        .client
        .post(format!("/servers/{}/roles", server.id))
        .header(Header::new("x-session-token", owner_session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "name": "Helpers" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let role: Value = response.into_json().await.unwrap();
    let role = role["id"].as_str().unwrap().to_string();
    let mut member = harness.db.fetch_member(&server.id, &user.id).await.unwrap();
    member
        .update(
            &harness.db,
            PartialMember {
                roles: Some(vec![role.clone()]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let response = harness
        .client
        .post(format!("/servers/{}/channels", server.id))
        .header(Header::new("x-session-token", owner_session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "name": "bots" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let second: Value = response.into_json().await.unwrap();

    let command = revolt_database::create_application_command(
        &harness.db,
        &bot.id,
        None,
        v0::DataApplicationCommand {
            name: "hello".into(),
            description: "Say hello".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    Setup {
        harness,
        bot,
        owner_session: owner_session.token,
        user_id: user.id,
        user_session: user_session.token,
        other_id: other.id.clone(),
        other_session: other_session.token,
        server,
        channel: channels.into_iter().next().unwrap(),
        second_channel: second["_id"].as_str().unwrap().to_string(),
        role,
        command,
    }
}

impl Setup {
    async fn invoke_in(&self, channel: &str, command: &v0::ApplicationCommand) -> Status {
        self.harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", self.user_session.clone()))
            .header(ContentType::JSON)
            .body(
                json!({
                    "type": "Command",
                    "application_id": self.bot.id,
                    "channel_id": channel,
                    "command_id": command.id,
                    "nonce": TestHarness::rand_string(),
                })
                .to_string(),
            )
            .dispatch()
            .await
            .status()
    }

    async fn invoke(&self) -> Status {
        self.invoke_in(self.channel.id(), &self.command).await
    }

    async fn put(&self, session: &str, command: Option<&str>, rules: Value) -> Status {
        let path = match command {
            Some(command) => format!(
                "/servers/{}/applications/{}/commands/{command}/permissions",
                self.server.id, self.bot.id
            ),
            None => format!(
                "/servers/{}/applications/{}/permissions",
                self.server.id, self.bot.id
            ),
        };
        self.harness
            .client
            .put(path)
            .header(Header::new("x-session-token", session.to_string()))
            .header(ContentType::JSON)
            .body(json!({ "permissions": rules }).to_string())
            .dispatch()
            .await
            .status()
    }

    /// Names in the command list the member sees here
    async fn listed(&self, channel: Option<&str>) -> Vec<String> {
        let query = channel.map(|c| format!("?channel={c}")).unwrap_or_default();
        let response = self
            .harness
            .client
            .get(format!("/servers/{}/commands{query}", self.server.id))
            .header(Header::new("x-session-token", self.user_session.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let list: v0::ServerCommands = response.into_json().await.unwrap();
        list.commands.into_iter().map(|c| c.name).collect()
    }
}

#[rocket::async_test]
async fn rules_decide_who_may_use_a_command() {
    let s = setup().await;
    assert_eq!(s.invoke().await, Status::Ok);
    assert_eq!(s.listed(None).await, vec!["hello".to_string()]);

    // the whole app: deny everyone
    let everyone = s.server.id.clone();
    assert!(s
        .put(&s.owner_session, None, json!([{ "id": everyone, "type": "Role", "permission": false }]))
        .await
        .code
        < 300);
    assert_eq!(s.invoke().await, Status::Forbidden);
    assert!(s.listed(None).await.is_empty());

    // the command: allow Helpers, which the member has
    assert!(s
        .put(
            &s.owner_session,
            Some(&s.command.id),
            json!([{ "id": s.role, "type": "Role", "permission": true }])
        )
        .await
        .code
        < 300);
    assert_eq!(s.invoke().await, Status::Ok);
    assert_eq!(s.listed(None).await, vec!["hello".to_string()]);

    // a member rule beats the role rule
    assert!(s
        .put(
            &s.owner_session,
            Some(&s.command.id),
            json!([
                { "id": s.role, "type": "Role", "permission": true },
                { "id": s.user_id, "type": "Member", "permission": false }
            ])
        )
        .await
        .code
        < 300);
    assert_eq!(s.invoke().await, Status::Forbidden);

    // reading them back
    let response = s
        .harness
        .client
        .get(format!("/servers/{}/applications/{}/permissions", s.server.id, s.bot.id))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let rules: Vec<v0::CommandPermissions> = response.into_json().await.unwrap();
    assert_eq!(rules.len(), 2);

    // each change is in the audit log, against the bot
    let mut logged = 0;
    for _ in 0..40 {
        logged = s
            .harness
            .db
            .get_server_audit_logs(
                &s.server.id,
                AuditLogQuery {
                    user: None,
                    target: None,
                    r#type: None,
                    before: None,
                    after: None,
                    limit: 100,
                },
            )
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| {
                matches!(&entry.action, AuditLogEntryAction::CommandPermissionsUpdate { bot, .. } if bot == &s.bot.id)
            })
            .count();
        if logged >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(logged, 3);
}

#[rocket::async_test]
async fn channel_rules_limit_where_a_command_works() {
    let s = setup().await;
    let everywhere = s.server.id.clone();
    assert!(s
        .put(
            &s.owner_session,
            None,
            json!([
                { "id": everywhere, "type": "Channel", "permission": false },
                { "id": s.second_channel, "type": "Channel", "permission": true }
            ])
        )
        .await
        .code
        < 300);
    assert_eq!(s.invoke().await, Status::Forbidden);
    let in_bots = s.invoke_in(&s.second_channel, &s.command).await;
    assert_eq!(in_bots, Status::Ok);
    assert!(s.listed(Some(s.channel.id())).await.is_empty());
    assert_eq!(s.listed(Some(&s.second_channel)).await, vec!["hello".to_string()]);
    // without a channel, channel rules are not applied to the list
    assert_eq!(s.listed(None).await, vec!["hello".to_string()]);
}

#[rocket::async_test]
async fn only_managers_change_rules_and_bad_rules_are_refused() {
    let s = setup().await;
    let rules = json!([{ "id": s.role, "type": "Role", "permission": false }]);
    assert_eq!(s.put(&s.other_session, None, rules.clone()).await, Status::Forbidden);
    assert_eq!(s.put(&s.user_session, None, rules).await, Status::Forbidden);

    let twice = json!([
        { "id": s.role, "type": "Role", "permission": false },
        { "id": s.role, "type": "Role", "permission": true }
    ]);
    assert_eq!(s.put(&s.owner_session, None, twice).await, Status::BadRequest);

    // a command of another app is not this app's
    let (_, _, stranger) = s.harness.new_user().await;
    let (other_bot, _) = s.harness.new_bot(&stranger).await;
    let foreign = revolt_database::create_application_command(
        &s.harness.db,
        &other_bot.id,
        None,
        v0::DataApplicationCommand {
            name: "x".into(),
            description: "X".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        s.put(&s.owner_session, Some(&foreign.id), json!([])).await,
        Status::NotFound
    );
}

#[rocket::async_test]
async fn removing_the_app_removes_its_rules() {
    let s = setup().await;
    let everyone = s.server.id.clone();
    assert!(s
        .put(&s.owner_session, None, json!([{ "id": everyone, "type": "Role", "permission": false }]))
        .await
        .code
        < 300);
    let response = s
        .harness
        .client
        .delete(format!("/servers/{}/applications/{}", s.server.id, s.bot.id))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert!(response.status().code < 300);
    let left = s
        .harness
        .db
        .fetch_command_permissions(&s.server.id, Some(&s.bot.id))
        .await
        .unwrap();
    assert!(left.is_empty());
}

/// The DM between the member and the bot
async fn dm_with_bot(s: &Setup) -> String {
    let response = s
        .harness
        .client
        .get(format!("/users/{}/dm", s.bot.id))
        .header(Header::new("x-session-token", s.user_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let channel: Value = response.into_json().await.unwrap();
    channel["_id"].as_str().unwrap().to_string()
}

#[rocket::async_test]
async fn global_commands_work_in_a_dm_with_the_bot() {
    let s = setup().await;
    let dm = dm_with_bot(&s).await;
    let mut events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;

    assert_eq!(s.invoke_in(&dm, &s.command).await, Status::Ok);
    let interaction = match events
        .wait_for_event(|event| matches!(event, EventV1::InteractionCreate { .. }))
        .await
    {
        EventV1::InteractionCreate { interaction } => interaction,
        _ => unreachable!(),
    };
    assert_eq!(interaction.server_id, None);
    assert_eq!(interaction.channel_id, dm);
    assert_eq!(interaction.command.unwrap().name, "hello");

    // a command limited to a server is not offered in the DM
    let scoped = revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        Some(&s.server.id),
        v0::DataApplicationCommand {
            name: "local".into(),
            description: "Local".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(s.invoke_in(&dm, &scoped).await, Status::NotFound);

    // nor one that needs server permissions
    let moderation = revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        None,
        v0::DataApplicationCommand {
            name: "ban".into(),
            description: "Ban".into(),
            default_member_permissions: Some(1 << 7),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(s.invoke_in(&dm, &moderation).await, Status::NotFound);
}

#[rocket::async_test]
async fn commands_do_not_work_in_a_dm_with_someone_else() {
    let s = setup().await;
    // a group or a DM with a person is not the bot's DM: refused as before
    let (_, _, stranger) = s.harness.new_user().await;
    let (other_bot, _) = s.harness.new_bot(&stranger).await;
    revolt_presence::create_session(&other_bot.id, 0).await;
    let dm = dm_with_bot(&s).await;
    let response = s
        .harness
        .client
        .post("/interactions")
        .header(Header::new("x-session-token", s.user_session.clone()))
        .header(ContentType::JSON)
        .body(
            json!({
                "type": "Command",
                "application_id": other_bot.id,
                "channel_id": dm,
                "command_id": s.command.id,
                "nonce": "other",
            })
            .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
}

// ----- review round: who may write which rules, threads, cleanup -----

const MANAGE_SERVER: u64 = 1 << 1;
const MANAGE_ROLE: u64 = 1 << 3;
const BAN_MEMBERS: u64 = 1 << 7;

/// A member with a "Mods" role (Manage Server and Manage Roles), ranked
/// below Helpers; returns their session token
async fn moderator(s: &Setup) -> String {
    let (_, session, user) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &user, None).await.unwrap();
    let response = s
        .harness
        .client
        .post(format!("/servers/{}/roles", s.server.id))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .header(ContentType::JSON)
        .body(json!({ "name": "Mods" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let role: Value = response.into_json().await.unwrap();
    let role = role["id"].as_str().unwrap().to_string();
    let response = s
        .harness
        .client
        .put(format!("/servers/{}/permissions/{role}", s.server.id))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .header(ContentType::JSON)
        .body(json!({ "permissions": { "allow": MANAGE_SERVER | MANAGE_ROLE, "deny": 0 } }).to_string())
        .dispatch()
        .await;
    assert!(response.status().code < 300, "{}", response.status());
    let mut member = s.harness.db.fetch_member(&s.server.id, &user.id).await.unwrap();
    member
        .update(
            &s.harness.db,
            PartialMember {
                roles: Some(vec![role]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    session.token
}

async fn command_needing(s: &Setup, name: &str, permissions: u64) -> v0::ApplicationCommand {
    revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        None,
        v0::DataApplicationCommand {
            name: name.into(),
            description: "Needs permissions".into(),
            default_member_permissions: Some(permissions as i64),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

/// B1: an Allow can only hand out what the editor may use themselves
#[rocket::async_test]
async fn an_allow_needs_the_permissions_the_command_asks_for() {
    let s = setup().await;
    let mods = moderator(&s).await;
    let ban = command_needing(&s, "ban", BAN_MEMBERS).await;
    let everyone = s.server.id.clone();
    let allow = json!([{ "id": everyone, "type": "Role", "permission": true }]);
    let deny = json!([{ "id": everyone, "type": "Role", "permission": false }]);

    // the moderator cannot Ban, so cannot open /ban to everyone
    assert_eq!(s.put(&mods, Some(&ban.id), allow.clone()).await, Status::Forbidden);
    // nor every command of the app at once (the app has /ban)
    assert_eq!(s.put(&mods, None, allow.clone()).await, Status::Forbidden);
    // a Deny hands out nothing
    assert!(s.put(&mods, Some(&ban.id), deny).await.code < 300);
    // /hello needs nothing, so an Allow there is fine
    assert!(s.put(&mods, Some(&s.command.id), allow.clone()).await.code < 300);
    // the owner may do anything
    assert!(s.put(&s.owner_session, Some(&ban.id), allow).await.code < 300);
}

/// B1: no rules for roles at or above the editor, or members who outrank them
#[rocket::async_test]
async fn rules_stop_at_the_editors_rank() {
    let s = setup().await;
    let mods = moderator(&s).await;
    // Helpers was made first, so it ranks above Mods
    let higher_role = json!([{ "id": s.role, "type": "Role", "permission": false }]);
    assert_eq!(s.put(&mods, None, higher_role).await, Status::Forbidden);
    // the member holding Helpers outranks the moderator
    let higher_member = json!([{ "id": s.user_id, "type": "Member", "permission": false }]);
    assert_eq!(s.put(&mods, None, higher_member).await, Status::Forbidden);
    // a member without roles ranks below
    let lower_member = json!([{ "id": s.other_id, "type": "Member", "permission": false }]);
    assert!(s.put(&mods, None, lower_member).await.code < 300);
    // removing a rule about a higher role is refused too
    assert!(s
        .put(
            &s.owner_session,
            Some(&s.command.id),
            json!([{ "id": s.role, "type": "Role", "permission": false }])
        )
        .await
        .code
        < 300);
    assert_eq!(s.put(&mods, Some(&s.command.id), json!([])).await, Status::Forbidden);
}

/// S6: ids must be roles, channels and members of this server
#[rocket::async_test]
async fn unknown_targets_are_refused() {
    let s = setup().await;
    for rule in [
        json!({ "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "type": "Role", "permission": false }),
        json!({ "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "type": "Channel", "permission": false }),
        json!({ "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "type": "Member", "permission": false }),
    ] {
        assert_eq!(s.put(&s.owner_session, None, json!([rule])).await, Status::BadRequest);
    }
}

/// S4: rules for things that are gone are dropped, not refused
#[rocket::async_test]
async fn rules_for_deleted_things_go_away() {
    let s = setup().await;
    let rules = json!([
        { "id": s.second_channel, "type": "Channel", "permission": false },
        { "id": s.role, "type": "Role", "permission": true }
    ]);
    assert!(s.put(&s.owner_session, Some(&s.command.id), rules.clone()).await.code < 300);

    // deleting the channel removes its rule at once
    let response = s
        .harness
        .client
        .delete(format!("/channels/{}", s.second_channel))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert!(response.status().code < 300);
    let left = s
        .harness
        .db
        .fetch_command_permissions(&s.server.id, Some(&s.bot.id))
        .await
        .unwrap();
    assert_eq!(left[0].permissions.len(), 1);

    // a deleted role's rule is dropped on the next save, not refused
    let response = s
        .harness
        .client
        .delete(format!("/servers/{}/roles/{}", s.server.id, s.role))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert!(response.status().code < 300);
    let response = s
        .harness
        .client
        .put(format!(
            "/servers/{}/applications/{}/commands/{}/permissions",
            s.server.id, s.bot.id, s.command.id
        ))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .header(ContentType::JSON)
        .body(json!({ "permissions": rules }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let saved: v0::CommandPermissions = response.into_json().await.unwrap();
    assert!(saved.permissions.is_empty());

    // deleting the command removes its rules
    assert!(s
        .put(
            &s.owner_session,
            Some(&s.command.id),
            json!([{ "id": s.server.id, "type": "Role", "permission": false }])
        )
        .await
        .code
        < 300);
    let response = s
        .harness
        .client
        .delete(format!("/applications/{}/commands/{}", s.bot.id, s.command.id))
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .dispatch()
        .await;
    assert!(response.status().code < 300, "{}", response.status());
    let left = s
        .harness
        .db
        .fetch_command_permissions(&s.server.id, Some(&s.bot.id))
        .await
        .unwrap();
    assert!(left.iter().all(|rules| rules.command_id.is_none()));
}

/// S5: the audit entry says what the rules were and became
#[rocket::async_test]
async fn the_audit_log_keeps_before_and_after() {
    let s = setup().await;
    let first = json!([{ "id": s.role, "type": "Role", "permission": true }]);
    let second = json!([{ "id": s.role, "type": "Role", "permission": false }]);
    assert!(s.put(&s.owner_session, None, first).await.code < 300);
    assert!(s.put(&s.owner_session, None, second).await.code < 300);
    let mut found = None;
    for _ in 0..40 {
        found = s
            .harness
            .db
            .get_server_audit_logs(
                &s.server.id,
                AuditLogQuery {
                    user: None,
                    target: None,
                    r#type: None,
                    before: None,
                    after: None,
                    limit: 100,
                },
            )
            .await
            .unwrap()
            .into_iter()
            .find_map(|entry| match entry.action {
                AuditLogEntryAction::CommandPermissionsUpdate { before, after, .. }
                    if !before.is_empty() =>
                {
                    Some((before, after))
                }
                _ => None,
            });
        if found.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (before, after) = found.expect("an entry with the earlier rules");
    assert!(before[0].permission);
    assert!(!after[0].permission);
}

/// B2: a thread follows the rules of the channel it is in
#[rocket::async_test]
async fn threads_use_their_channels_rules() {
    let s = setup().await;
    let response = s
        .harness
        .client
        .post(format!("/channels/{}/threads", s.channel.id()))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .header(ContentType::JSON)
        .body(json!({ "name": "Talk", "content": "first" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let thread: Value = response.into_json().await.unwrap();
    let thread = thread["_id"].as_str().unwrap().to_string();

    // denied in the parent channel: denied in its thread too
    let deny_parent = json!([{ "id": s.channel.id(), "type": "Channel", "permission": false }]);
    assert!(s.put(&s.owner_session, None, deny_parent).await.code < 300);
    assert_eq!(s.invoke_in(&thread, &s.command).await, Status::Forbidden);
    assert!(s.listed(Some(&thread)).await.is_empty());

    // only in the parent channel: allowed in its thread as well
    let only_parent = json!([
        { "id": s.server.id, "type": "Channel", "permission": false },
        { "id": s.channel.id(), "type": "Channel", "permission": true }
    ]);
    assert!(s.put(&s.owner_session, None, only_parent).await.code < 300);
    assert_eq!(s.invoke_in(&thread, &s.command).await, Status::Ok);
    assert_eq!(s.listed(Some(&thread)).await, vec!["hello".to_string()]);
}

/// B5: managers see every command of the app here, unfiltered
#[rocket::async_test]
async fn managers_list_every_command_of_an_app() {
    let s = setup().await;
    command_needing(&s, "ban", BAN_MEMBERS).await;
    revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        Some(&s.server.id),
        v0::DataApplicationCommand {
            name: "hello".into(),
            description: "Server copy".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // even a rule that keeps everyone out does not hide them here
    assert!(s
        .put(
            &s.owner_session,
            None,
            json!([{ "id": s.server.id, "type": "Role", "permission": false }])
        )
        .await
        .code
        < 300);
    let path = format!("/servers/{}/applications/{}/commands", s.server.id, s.bot.id);
    let response = s
        .harness
        .client
        .get(path.clone())
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let commands: Vec<v0::ApplicationCommand> = response.into_json().await.unwrap();
    let mut names: Vec<_> = commands.iter().map(|c| c.name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["ban", "hello", "hello"]);

    let response = s
        .harness
        .client
        .get(path)
        .header(Header::new("x-session-token", s.user_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);
}

/// S9: buttons on a command's answer follow that command's rules
#[rocket::async_test]
async fn buttons_on_a_commands_answer_follow_its_rules() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut channel_events = PubSubTestHelper::new(s.channel.id()).await;
    assert_eq!(s.invoke().await, Status::Ok);
    let interaction = match bot_events
        .wait_for_event(|event| matches!(event, EventV1::InteractionCreate { .. }))
        .await
    {
        EventV1::InteractionCreate { interaction } => interaction,
        _ => unreachable!(),
    };
    let response = s
        .harness
        .client
        .post(format!(
            "/interactions/{}/{}/callback",
            interaction.id,
            interaction.token.unwrap()
        ))
        .header(ContentType::JSON)
        .body(
            json!({ "type": "Message", "data": { "content": "Vote", "components": [
                { "type": "ActionRow", "components": [
                    { "type": "Button", "style": "Primary", "label": "Yes", "custom_id": "yes" }
                ]}
            ]}})
            .to_string(),
        )
        .dispatch()
        .await;
    assert!(response.status().code < 300);
    let message = channel_events.wait_for_message().await;

    let press = |nonce: &'static str| {
        let body = json!({
            "type": "Component",
            "channel_id": s.channel.id(),
            "message_id": message.id,
            "custom_id": "yes",
            "nonce": nonce,
        })
        .to_string();
        let client = &s.harness.client;
        let session = s.user_session.clone();
        async move {
            client
                .post("/interactions")
                .header(Header::new("x-session-token", session))
                .header(ContentType::JSON)
                .body(body)
                .dispatch()
                .await
                .status()
        }
    };
    assert_eq!(press("click-1").await, Status::Ok);
    assert!(s
        .put(
            &s.owner_session,
            Some(&s.command.id),
            json!([{ "id": s.user_id, "type": "Member", "permission": false }])
        )
        .await
        .code
        < 300);
    assert_eq!(press("click-2").await, Status::Forbidden);
}

/// B3: the maker of a bot is only shown when they chose so, and never by id
#[rocket::async_test]
async fn the_bot_page_shows_its_maker_only_when_asked() {
    let s = setup().await;
    let edit = |body: Value| {
        let client = &s.harness.client;
        let session = s.owner_session.clone();
        let path = format!("/bots/{}", s.bot.id);
        async move {
            client
                .patch(path)
                .header(Header::new("x-session-token", session))
                .header(ContentType::JSON)
                .body(body.to_string())
                .dispatch()
                .await
                .status()
        }
    };
    let page = || {
        let client = &s.harness.client;
        let path = format!("/bots/{}/invite", s.bot.id);
        async move {
            let response = client.get(path).dispatch().await;
            assert_eq!(response.status(), Status::Ok);
            response.into_json::<Value>().await.unwrap()
        }
    };
    assert!(edit(json!({ "public": true })).await.code < 300);
    let hidden = page().await;
    assert!(hidden.get("creator").is_none(), "{hidden}");

    assert!(edit(json!({ "show_creator": true })).await.code < 300);
    let shown = page().await;
    assert!(shown["creator"]["username"].is_string(), "{shown}");
    assert!(shown["creator"].get("_id").is_none(), "{shown}");
}
