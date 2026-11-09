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
