use std::time::Duration;

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_database::{events::client::EventV1, Bot, Channel, Member, Server, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::json;

pub(super) struct Setup {
    pub(super) harness: TestHarness,
    pub(super) bot: Bot,
    pub(super) user: User,
    pub(super) user_session: String,
    other_user: User,
    server: Server,
    pub(super) channel: Channel,
    command: v0::ApplicationCommand,
}

/// Server with an installed, online bot that registered `/hello [name]`,
/// one member who uses it and one bystander.
pub(super) async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, other_user) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let (bot, bot_user) = harness.new_bot(&owner).await;
    for member in [&owner, &user, &other_user, &bot_user] {
        Member::create(&harness.db, &server, member, None).await.unwrap();
    }
    revolt_presence::create_session(&bot.id, 0).await;

    let command = revolt_database::create_application_command(
        &harness.db,
        &bot.id,
        None,
        serde_json::from_value(json!({
            "name": "hello",
            "description": "Say hello",
            "options": [
                { "type": "String", "name": "name", "description": "Who", "max_length": 10 },
                { "type": "Channel", "name": "where", "description": "Where" }
            ]
        }))
        .unwrap(),
    )
    .await
    .unwrap();

    Setup {
        harness,
        bot,
        user,
        user_session: session.token,
        other_user,
        server,
        channel: channels.into_iter().next().unwrap(),
        command,
    }
}

impl Setup {
    pub(super) fn body(&self, nonce: &str, options: serde_json::Value) -> String {
        json!({
            "type": "Command",
            "application_id": self.bot.id,
            "channel_id": self.channel.id(),
            "command_id": self.command.id,
            "command_version": self.command.version,
            "options": options,
            "nonce": nonce,
        })
        .to_string()
    }

    pub(super) async fn invoke(&self, body: String) -> (Status, Option<v0::InteractionAccepted>) {
        let response = self
            .harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", self.user_session.clone()))
            .header(ContentType::JSON)
            .body(body)
            .dispatch()
            .await;
        let status = response.status();
        let accepted = if status == Status::Ok {
            response.into_json().await
        } else {
            None
        };
        (status, accepted)
    }

    async fn callback(&self, id: &str, token: &str, body: serde_json::Value) -> Status {
        self.harness
            .client
            .post(format!("/interactions/{id}/{token}/callback"))
            .header(ContentType::JSON)
            .body(body.to_string())
            .dispatch()
            .await
            .status()
    }
}

async fn next_interaction(bot_events: &mut PubSubTestHelper) -> v0::Interaction {
    match bot_events
        .wait_for_event(|event| matches!(event, EventV1::InteractionCreate { .. }))
        .await
    {
        EventV1::InteractionCreate { interaction } => interaction,
        _ => unreachable!(),
    }
}

/// Nothing matching arrives on the topic within a short window
async fn assert_silent<F: Fn(&EventV1) -> bool>(events: &mut PubSubTestHelper, predicate: F) {
    assert!(
        tokio::time::timeout(Duration::from_millis(700), events.wait_for_event(predicate))
            .await
            .is_err(),
        "an event leaked"
    );
}

/// P-25: picking a person shows the bot that person, never how they relate
/// to whoever picked them, and never anyone's friends or blocks
#[rocket::async_test]
async fn people_picked_are_shown_from_the_bots_view() {
    use revolt_database::{PartialUser, Relationship, RelationshipStatus};

    let s = setup().await;
    let friend = |id: &str| Relationship {
        id: id.to_string(),
        status: RelationshipStatus::Friend,
    };
    s.harness
        .db
        .update_user(
            &s.user.id,
            &PartialUser {
                relations: Some(vec![friend(&s.other_user.id)]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    s.harness
        .db
        .update_user(
            &s.other_user.id,
            &PartialUser {
                relations: Some(vec![friend(&s.user.id)]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let command = revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        None,
        serde_json::from_value(json!({
            "name": "who",
            "description": "Pick someone",
            "options": [{ "type": "User", "name": "person", "description": "Who" }]
        }))
        .unwrap(),
    )
    .await
    .unwrap();

    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    for (nonce, picked) in [("self", &s.user.id), ("friend", &s.other_user.id)] {
        let body = json!({
            "type": "Command",
            "application_id": s.bot.id,
            "channel_id": s.channel.id(),
            "command_id": command.id,
            "command_version": command.version,
            "options": [{ "type": "User", "name": "person", "value": picked }],
            "nonce": nonce,
        })
        .to_string();
        let (status, _) = s.invoke(body).await;
        assert_eq!(status, Status::Ok, "{nonce}");

        let interaction = next_interaction(&mut bot_events).await;
        let resolved = interaction.command.unwrap().resolved.unwrap();
        let user = serde_json::to_value(&resolved.users[picked]).unwrap();
        assert!(user.get("relations").is_none_or(|r| r == &json!([])), "{user}");
        assert_eq!(user["relationship"], "None", "{user}");
    }
}

#[rocket::async_test]
async fn bot_answers_hello_publicly() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let mut channel_events = PubSubTestHelper::new(s.channel.id()).await;

    let (status, accepted) = s
        .invoke(s.body("n1", json!([
            { "type": "String", "name": "name", "value": "Alice" },
            { "type": "Channel", "name": "where", "value": s.channel.id() }
        ])))
        .await;
    assert_eq!(status, Status::Ok);
    let accepted = accepted.unwrap();

    let interaction = next_interaction(&mut bot_events).await;
    assert_eq!(interaction.id, accepted.id);
    assert_eq!(interaction.user.id, s.user.id);
    // Permissions are also sent as strings, so JavaScript bots lose no bits
    assert_eq!(interaction.user_permissions_string, interaction.user_permissions.to_string());
    assert_eq!(interaction.app_permissions_string, interaction.app_permissions.to_string());
    assert!(!interaction.app_permissions_string.is_empty());
    let token = interaction.token.clone().expect("token for the bot");
    let command = interaction.command.unwrap();
    assert_eq!(command.name, "hello");
    assert_eq!(
        command.options[0].value,
        Some(v0::InteractionOptionValue::String("Alice".into()))
    );
    assert!(command
        .resolved
        .unwrap()
        .channels
        .contains_key(s.channel.id()));

    // The same nonce returns the same interaction and is not delivered twice
    let (_, again) = s.invoke(s.body("n1", json!([]))).await;
    assert_eq!(again.unwrap().id, accepted.id);
    assert_silent(&mut bot_events, |e| matches!(e, EventV1::InteractionCreate { .. })).await;

    let status = s
        .callback(&accepted.id, &token, json!({ "type": "Message", "data": { "content": "أهلاً بك في زرزور!" } }))
        .await;
    assert_eq!(status, Status::NoContent);

    let message = channel_events.wait_for_message().await;
    assert_eq!(message.author, s.bot.id);
    assert_eq!(message.content.as_deref(), Some("أهلاً بك في زرزور!"));
    // The answer says who used which command
    let used = message.interaction.clone().expect("interaction on the answer");
    assert_eq!(used.id, accepted.id);
    assert_eq!(used.kind, v0::InteractionType::Command);
    assert_eq!(used.name.as_deref(), Some("hello"));

    match user_events
        .wait_for_event(|e| matches!(e, EventV1::InteractionStateUpdate { .. }))
        .await
    {
        EventV1::InteractionStateUpdate { state, message_id, .. } => {
            assert_eq!(state, v0::InteractionState::Responded);
            assert_eq!(message_id, Some(message.id));
        }
        _ => unreachable!(),
    }

    // A second answer is refused, and so is a wrong token
    let status = s
        .callback(&accepted.id, &token, json!({ "type": "Message", "data": { "content": "again" } }))
        .await;
    assert_eq!(status, Status::Conflict);
    let status = s
        .callback(&accepted.id, &"x".repeat(64), json!({ "type": "Message", "data": { "content": "x" } }))
        .await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn ephemeral_answer_reaches_only_the_invoker() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let mut other_events = PubSubTestHelper::new(&format!("{}!", s.other_user.id)).await;
    let mut channel_events = PubSubTestHelper::new(s.channel.id()).await;
    let mut server_events = PubSubTestHelper::new(&s.server.id).await;

    let (_, accepted) = s.invoke(s.body("n2", json!([]))).await;
    let accepted = accepted.unwrap();
    let token = next_interaction(&mut bot_events).await.token.unwrap();

    let status = s
        .callback(&accepted.id, &token, json!({ "type": "Message", "data": { "content": "secret", "ephemeral": true } }))
        .await;
    assert_eq!(status, Status::NoContent);

    match user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageCreate { .. }))
        .await
    {
        EventV1::EphemeralMessageCreate { message } => {
            assert_eq!(message.content.as_deref(), Some("secret"));
            assert_eq!(message.interaction_id, accepted.id);
        }
        _ => unreachable!(),
    }

    // Not on the channel or server topics, not to other users, not stored as a message
    assert_silent(&mut channel_events, |_| true).await;
    assert_silent(&mut server_events, |e| {
        matches!(e, EventV1::Message(_) | EventV1::EphemeralMessageCreate { .. })
    })
    .await;
    assert_silent(&mut other_events, |_| true).await;
    let stored = s
        .harness
        .db
        .fetch_messages(revolt_database::MessageQuery {
            filter: revolt_database::MessageFilter {
                channel: Some(s.channel.id().to_string()),
                ..Default::default()
            },
            limit: Some(10),
            time_period: revolt_database::MessageTimePeriod::Absolute {
                before: None,
                after: None,
                sort: None,
            },
        })
        .await
        .unwrap();
    assert!(stored.is_empty());
}

#[rocket::async_test]
async fn deferred_answer_is_finished_by_editing_the_original() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let mut channel_events = PubSubTestHelper::new(s.channel.id()).await;

    let (_, accepted) = s.invoke(s.body("n3", json!([]))).await;
    let accepted = accepted.unwrap();
    let token = next_interaction(&mut bot_events).await.token.unwrap();

    let status = s.callback(&accepted.id, &token, json!({ "type": "DeferredMessage" })).await;
    assert_eq!(status, Status::NoContent);
    user_events
        .wait_for_event(|e| {
            matches!(e, EventV1::InteractionStateUpdate { state: v0::InteractionState::Deferred, .. })
        })
        .await;

    // Past the first-answer deadline is fine once deferred
    tokio::time::sleep(Duration::from_millis(3200)).await;

    let response = s
        .harness
        .client
        .patch(format!("/interactions/{}/{token}/original", accepted.id))
        .header(ContentType::JSON)
        .body(json!({ "content": "done" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);

    let message = channel_events.wait_for_message().await;
    assert_eq!(message.content.as_deref(), Some("done"));

    let response = s
        .harness
        .client
        .patch(format!("/interactions/{}/{token}/original", accepted.id))
        .header(ContentType::JSON)
        .body(json!({ "content": "edited" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);
    assert_eq!(
        s.harness.db.fetch_message(&message.id).await.unwrap().content.as_deref(),
        Some("edited")
    );
}

#[rocket::async_test]
async fn late_first_answer_is_refused() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let (_, accepted) = s.invoke(s.body("n4", json!([]))).await;
    let accepted = accepted.unwrap();
    let token = next_interaction(&mut bot_events).await.token.unwrap();

    tokio::time::sleep(Duration::from_millis(3200)).await;
    let status = s
        .callback(&accepted.id, &token, json!({ "type": "Message", "data": { "content": "late" } }))
        .await;
    assert_eq!(status, Status::Gone);
}

#[rocket::async_test]
async fn invalid_interactions_are_refused() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;

    // Wrong value type, too long, unknown option, duplicate option
    for options in [
        json!([{ "type": "String", "name": "name", "value": 5 }]),
        json!([{ "type": "String", "name": "name", "value": "far too long here" }]),
        json!([{ "type": "String", "name": "nope", "value": "x" }]),
        json!([
            { "type": "String", "name": "name", "value": "a" },
            { "type": "String", "name": "name", "value": "b" }
        ]),
    ] {
        let (status, _) = s.invoke(s.body(&ulid::Ulid::new().to_string(), options)).await;
        assert_eq!(status, Status::BadRequest);
    }

    // A channel from another server is refused like a missing one
    let (_, _, stranger) = s.harness.new_user().await;
    let (_, foreign_channels) = s.harness.new_server(&stranger).await;
    let (status, _) = s
        .invoke(s.body(
            "foreign",
            json!([{ "type": "Channel", "name": "where", "value": foreign_channels[0].id() }]),
        ))
        .await;
    assert_eq!(status, Status::BadRequest);

    // Outdated command revision
    let mut body: serde_json::Value = serde_json::from_str(&s.body("old", json!([]))).unwrap();
    body["command_version"] = json!("01ARZ3NDEKTSV4RRFFQ69G5FAV");
    let (status, _) = s.invoke(body.to_string()).await;
    assert_eq!(status, Status::BadRequest);

    // Another application's command id
    let (other_bot, other_bot_user) = s.harness.new_bot(&stranger).await;
    Member::create(&s.harness.db, &s.server, &other_bot_user, None).await.unwrap();
    let mut body: serde_json::Value = serde_json::from_str(&s.body("cross", json!([]))).unwrap();
    body["application_id"] = json!(other_bot.id);
    revolt_presence::create_session(&other_bot.id, 0).await;
    let (status, _) = s.invoke(body.to_string()).await;
    assert_eq!(status, Status::NotFound);

    // Nothing reached the bot
    assert_silent(&mut bot_events, |e| matches!(e, EventV1::InteractionCreate { .. })).await;
}

#[rocket::async_test]
async fn bot_must_be_installed_and_online() {
    let s = setup().await;

    let (_, _, stranger) = s.harness.new_user().await;
    let (lonely, _) = s.harness.new_bot(&stranger).await;
    revolt_presence::create_session(&lonely.id, 0).await;
    let mut body: serde_json::Value = serde_json::from_str(&s.body("x1", json!([]))).unwrap();
    body["application_id"] = json!(lonely.id);
    let (status, _) = s.invoke(body.to_string()).await;
    assert_eq!(status, Status::Forbidden);

    let (offline, offline_user) = s.harness.new_bot(&stranger).await;
    Member::create(&s.harness.db, &s.server, &offline_user, None).await.unwrap();
    let command = revolt_database::create_application_command(
        &s.harness.db,
        &offline.id,
        None,
        v0::DataApplicationCommand {
            name: "ping".into(),
            description: "Ping".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let body = json!({
        "type": "Command",
        "application_id": offline.id,
        "channel_id": s.channel.id(),
        "command_id": command.id,
        "nonce": "x2",
    });
    let (status, _) = s.invoke(body.to_string()).await;
    assert_eq!(status, Status::ServiceUnavailable);

    // Bots cannot trigger interactions
    let response = s
        .harness
        .client
        .post("/interactions")
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .header(ContentType::JSON)
        .body(s.body("x3", json!([])))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
}

/// Live 2026-11-09: in a ticket thread `/reply` reached a modmail bot and
/// `/close` did not. The modmail commands, registered as the bot does
/// (global, with a left-over server copy of `close`), are used in a thread
/// the way the app sends them: every one reaches the bot, under its name.
#[rocket::async_test]
async fn every_modmail_command_reaches_the_bot_in_a_thread() {
    let s = setup().await;
    let bot_put = |path: String, body: serde_json::Value| {
        s.harness
            .client
            .put(path)
            .header(Header::new("x-bot-token", s.bot.token.clone()))
            .header(ContentType::JSON)
            .body(body.to_string())
    };
    let commands = json!([
        { "name": "reply", "description": "Answer the member", "options": [
            { "type": "String", "name": "message", "description": "Your answer", "required": true, "max_length": 4000 },
            { "type": "Attachment", "name": "file", "description": "A file", "required": false },
            { "type": "Boolean", "name": "anonymous", "description": "Hide your name", "required": false }
        ]},
        { "name": "close", "description": "Close this ticket", "options": [
            { "type": "String", "name": "note", "description": "Short note", "required": false, "max_length": 1000 }
        ]},
        { "name": "tickets", "description": "List open tickets" }
    ]);
    let response = bot_put(format!("/applications/{}/commands", s.bot.id), commands)
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    let response = bot_put(
        format!("/applications/{}/servers/{}/commands", s.bot.id, s.server.id),
        json!([{ "name": "close", "description": "Old server copy" }]),
    )
    .dispatch()
    .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);

    // a ticket thread, as the bot makes it
    let response = s
        .harness
        .client
        .post(format!("/channels/{}/threads", s.channel.id()))
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "name": "ticket-test", "content": "New ticket" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let thread: serde_json::Value = response.into_json().await.unwrap();
    let thread = thread["_id"].as_str().unwrap().to_string();

    // what the app's picker lists for the member
    let response = s
        .harness
        .client
        .get(format!("/servers/{}/commands", s.server.id))
        .header(Header::new("x-session-token", s.user_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let listed: v0::ServerCommands = response.into_json().await.unwrap();
    let mine: Vec<&v0::ApplicationCommand> = listed
        .commands
        .iter()
        .filter(|command| command.application_id == s.bot.id)
        .collect();
    assert_eq!(
        mine.iter().filter(|command| command.name == "close").count(),
        2,
        "the global and the server copy are both listed"
    );

    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut nonce = 0;
    for command in mine {
        // options left out, as the app sends a command without values
        let mut options = json!([]);
        if command.name == "reply" {
            options = json!([{ "type": "String", "name": "message", "value": "hello" }]);
        }
        for options in [Some(options.clone()), None] {
            if options.is_none() && command.name == "reply" {
                continue;
            }
            nonce += 1;
            let mut body = json!({
                "type": "Command",
                "application_id": s.bot.id,
                "channel_id": thread,
                "command_id": command.id,
                "command_version": command.version,
                "nonce": format!("modmail-{nonce}"),
            });
            if let Some(options) = options {
                body["options"] = options;
            }
            let (status, accepted) = s.invoke(body.to_string()).await;
            assert_eq!(status, Status::Ok, "/{} was refused", command.name);

            let interaction = next_interaction(&mut bot_events).await;
            assert_eq!(interaction.id, accepted.unwrap().id);
            assert_eq!(interaction.channel_id, thread);
            assert_eq!(interaction.command.unwrap().name, command.name);
        }
    }
}
