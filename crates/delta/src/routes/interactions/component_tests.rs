//! Components, modals, autocomplete and the member command list.

use std::time::Duration;

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_database::{events::client::EventV1, Bot, Channel, Member, Server, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

struct Setup {
    harness: TestHarness,
    bot: Bot,
    user: User,
    session: String,
    other: User,
    other_session: String,
    server: Server,
    channel: Channel,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, session, user) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let (bot, bot_user) = harness.new_bot(&owner).await;
    for member in [&owner, &user, &other, &bot_user] {
        Member::create(&harness.db, &server, member, None).await.unwrap();
    }
    revolt_presence::create_session(&bot.id, 0).await;
    Setup {
        harness,
        bot,
        user,
        session: session.token,
        other,
        other_session: other_session.token,
        server,
        channel: channels.into_iter().next().unwrap(),
    }
}

impl Setup {
    /// The bot posts a message with components through the normal message route
    async fn bot_message(&self, components: Value) -> v0::Message {
        let response = self
            .harness
            .client
            .post(format!("/channels/{}/messages", self.channel.id()))
            .header(Header::new("x-bot-token", self.bot.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "content": "Pick one", "components": components }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        response.into_json().await.unwrap()
    }

    async fn interact(&self, session: &str, body: Value) -> (Status, Option<v0::InteractionAccepted>) {
        let response = self
            .harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", session.to_string()))
            .header(ContentType::JSON)
            .body(body.to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, if status == Status::Ok { response.into_json().await } else { None })
    }

    fn click(&self, message_id: &str, custom_id: &str, values: Option<Value>) -> Value {
        let mut body = json!({
            "type": "Component",
            "channel_id": self.channel.id(),
            "message_id": message_id,
            "custom_id": custom_id,
            "nonce": ulid::Ulid::new().to_string(),
        });
        if let Some(values) = values {
            body["values"] = values;
        }
        body
    }

    async fn callback(&self, id: &str, token: &str, body: Value) -> Status {
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

async fn next_interaction(events: &mut PubSubTestHelper) -> v0::Interaction {
    match events
        .wait_for_event(|e| matches!(e, EventV1::InteractionCreate { .. }))
        .await
    {
        EventV1::InteractionCreate { interaction } => interaction,
        _ => unreachable!(),
    }
}

async fn assert_silent<F: Fn(&EventV1) -> bool>(events: &mut PubSubTestHelper, predicate: F) {
    assert!(
        tokio::time::timeout(Duration::from_millis(700), events.wait_for_event(predicate))
            .await
            .is_err(),
        "an event leaked"
    );
}

fn buttons() -> Value {
    json!([{ "type": "ActionRow", "components": [
        { "type": "Button", "style": "Primary", "label": "فتح تذكرة", "custom_id": "ticket:open" },
        { "type": "Button", "style": "Danger", "label": "Off", "custom_id": "off", "disabled": true },
        { "type": "Button", "style": "Link", "label": "Docs", "url": "https://zrzur.cc" }
    ]}])
}

#[rocket::async_test]
async fn button_click_reaches_the_bot_and_updates_the_message() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut channel_events = PubSubTestHelper::new(s.channel.id()).await;
    let message = s.bot_message(buttons()).await;
    assert_eq!(message.components.as_ref().unwrap().len(), 1);

    let (status, accepted) = s.interact(&s.session, s.click(&message.id, "ticket:open", None)).await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    assert_eq!(interaction.kind, v0::InteractionType::Component);
    assert_eq!(interaction.message_id.as_deref(), Some(message.id.as_str()));
    let component = interaction.component.unwrap();
    assert_eq!(component.custom_id, "ticket:open");
    assert_eq!(component.component_type, "Button");

    // UpdateMessage removes the buttons from that same message
    let status = s
        .callback(
            &accepted.unwrap().id,
            &interaction.token.unwrap(),
            json!({ "type": "UpdateMessage", "data": { "content": "Opened", "components": [] } }),
        )
        .await;
    assert_eq!(status, Status::NoContent);
    channel_events
        .wait_for_event(|e| matches!(e, EventV1::MessageUpdate { id, .. } if id == &message.id))
        .await;
    let stored = s.harness.db.fetch_message(&message.id).await.unwrap();
    assert_eq!(stored.content.as_deref(), Some("Opened"));
    assert_eq!(stored.components, Some(vec![]));
    // people see that the message changed
    assert!(stored.edited.is_some(), "a component update marks the message edited");

    // The button is gone now
    let (status, _) = s.interact(&s.session, s.click(&message.id, "ticket:open", None)).await;
    assert_eq!(status, Status::BadRequest);
}

/// The DM between the test user and the bot, with a bot message holding buttons
async fn dm_message(setup: &Setup) -> (String, v0::Message) {
    let response = setup
        .harness
        .client
        .get(format!("/users/{}/dm", setup.bot.id))
        .header(Header::new("x-session-token", setup.session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let channel: Value = response.into_json().await.unwrap();
    let channel = channel["_id"].as_str().unwrap().to_string();
    let response = setup
        .harness
        .client
        .post(format!("/channels/{channel}/messages"))
        .header(Header::new("x-bot-token", setup.bot.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "content": "Rate us", "components": buttons() }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    (channel, response.into_json().await.unwrap())
}

#[rocket::async_test]
async fn buttons_work_in_a_dm_with_the_bot() {
    let setup = setup().await;
    let (channel, message) = dm_message(&setup).await;
    let mut events = PubSubTestHelper::new(&format!("{}!", setup.bot.id)).await;

    let body = json!({
        "type": "Component",
        "channel_id": channel,
        "message_id": message.id,
        "custom_id": "ticket:open",
        "nonce": TestHarness::rand_string(),
    });
    let (status, accepted) = setup.interact(&setup.session, body).await;
    assert_eq!(status, Status::Ok);
    assert!(accepted.is_some());

    let interaction = next_interaction(&mut events).await;
    assert_eq!(interaction.server_id, None);
    assert!(interaction.member.is_none());
    assert_eq!(interaction.channel_id, channel);
    let status = setup
        .callback(
            &interaction.id,
            interaction.token.as_ref().unwrap(),
            json!({ "type": "UpdateMessage", "data": { "content": "Thanks", "components": [] } }),
        )
        .await;
    assert!(status.code < 300, "{status}");
}

#[rocket::async_test]
async fn nobody_else_can_press_a_button_in_a_dm() {
    let setup = setup().await;
    let (channel, message) = dm_message(&setup).await;
    let body = json!({
        "type": "Component",
        "channel_id": channel,
        "message_id": message.id,
        "custom_id": "ticket:open",
        "nonce": TestHarness::rand_string(),
    });
    let (status, _) = setup.interact(&setup.other_session, body).await;
    assert_ne!(status, Status::Ok);
}

#[rocket::async_test]
async fn unusable_components_are_refused() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let message = s.bot_message(buttons()).await;

    // Disabled, link buttons, unknown ids, values on a button
    for (custom_id, values, expected) in [
        ("off", None, Status::BadRequest),
        ("nope", None, Status::BadRequest),
        ("ticket:open", Some(json!(["x"])), Status::BadRequest),
    ] {
        let (status, _) = s.interact(&s.session, s.click(&message.id, custom_id, values)).await;
        assert_eq!(status, expected, "{custom_id}");
    }

    // Restricted to one user
    let restricted = s
        .bot_message(json!([{ "type": "ActionRow", "components": [
            { "type": "Button", "style": "Primary", "label": "Mine", "custom_id": "mine",
              "restrict": { "users": [s.user.id] } }
        ]}]))
        .await;
    let (status, _) = s.interact(&s.other_session, s.click(&restricted.id, "mine", None)).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = s.interact(&s.session, s.click(&restricted.id, "mine", None)).await;
    assert_eq!(status, Status::Ok);
    next_interaction(&mut bot_events).await;

    // Users cannot post components themselves
    let response = s
        .harness
        .client
        .post(format!("/channels/{}/messages", s.channel.id()))
        .header(Header::new("x-session-token", s.session.clone()))
        .header(ContentType::JSON)
        .body(json!({ "content": "fake", "components": buttons() }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);

    // Bots cannot post malformed components
    let response = s
        .harness
        .client
        .post(format!("/channels/{}/messages", s.channel.id()))
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "content": "bad", "components": [{ "type": "ActionRow", "components": [
            { "type": "Button", "style": "Link", "label": "x", "url": "javascript:alert(1)" }
        ]}] }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
}

#[rocket::async_test]
async fn select_values_are_checked_again() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let role = s.harness.new_role(&s.server, 1, None).await;
    let message = s
        .bot_message(json!([
            { "type": "ActionRow", "components": [{ "type": "StringSelect", "custom_id": "colour",
                "min_values": 1, "max_values": 2,
                "options": [{ "label": "Red", "value": "red" }, { "label": "Blue", "value": "blue" }] }] },
            { "type": "ActionRow", "components": [{ "type": "RoleSelect", "custom_id": "role" }] }
        ]))
        .await;

    for (custom_id, values) in [
        ("colour", json!(["green"])),
        ("colour", json!(["red", "red"])),
        ("colour", json!([])),
        ("role", json!(["01ARZ3NDEKTSV4RRFFQ69G5FAV"])),
    ] {
        let (status, _) = s.interact(&s.session, s.click(&message.id, custom_id, Some(values.clone()))).await;
        assert_eq!(status, Status::BadRequest, "{custom_id} {values}");
    }

    let (status, _) = s
        .interact(&s.session, s.click(&message.id, "colour", Some(json!(["red", "blue"]))))
        .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(
        next_interaction(&mut bot_events).await.component.unwrap().values,
        vec!["red", "blue"]
    );

    let (status, _) = s
        .interact(&s.session, s.click(&message.id, "role", Some(json!([role.id]))))
        .await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    assert!(interaction
        .component
        .unwrap()
        .resolved
        .unwrap()
        .roles
        .contains_key(&role.id));
}

#[rocket::async_test]
async fn modal_round_trip_is_private_and_single_use() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let mut other_events = PubSubTestHelper::new(&format!("{}!", s.other.id)).await;
    let message = s.bot_message(buttons()).await;

    let (_, accepted) = s.interact(&s.session, s.click(&message.id, "ticket:open", None)).await;
    let accepted = accepted.unwrap();
    let token = next_interaction(&mut bot_events).await.token.unwrap();
    let modal = json!({
        "custom_id": "ticket:form",
        "title": "تذكرة جديدة",
        "components": [{ "type": "ActionRow", "components": [{
            "type": "TextInput", "custom_id": "topic", "style": "Paragraph",
            "label": "الموضوع", "min_length": 5, "max_length": 200
        }]}]
    });
    let status = s
        .callback(&accepted.id, &token, json!({ "type": "Modal", "modal": modal }))
        .await;
    assert_eq!(status, Status::NoContent);
    user_events
        .wait_for_event(|e| matches!(e, EventV1::InteractionModal { id, .. } if id == &accepted.id))
        .await;
    assert_silent(&mut other_events, |_| true).await;

    let submit = |fields: Value| {
        json!({
            "type": "ModalSubmit",
            "channel_id": s.channel.id(),
            "parent_id": accepted.id,
            "custom_id": "ticket:form",
            "fields": fields,
            "nonce": ulid::Ulid::new().to_string(),
        })
    };

    // Too short, unknown field, someone else's form
    let (status, _) = s.interact(&s.session, submit(json!([{ "custom_id": "topic", "value": "hi" }]))).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = s
        .interact(&s.session, submit(json!([{ "custom_id": "topic", "value": "valid topic" }, { "custom_id": "x", "value": "y" }])))
        .await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = s
        .interact(&s.other_session, submit(json!([{ "custom_id": "topic", "value": "valid topic" }])))
        .await;
    assert_eq!(status, Status::NotFound);

    let (status, _) = s
        .interact(&s.session, submit(json!([{ "custom_id": "topic", "value": "My printer is on fire" }])))
        .await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    assert_eq!(interaction.kind, v0::InteractionType::ModalSubmit);
    let data = interaction.modal.unwrap();
    assert_eq!(data.fields[0].value.as_deref(), Some("My printer is on fire"));
    // Opened from a button: the bot may update that message
    assert_eq!(interaction.message_id.as_deref(), Some(message.id.as_str()));

    // Submitted once only
    let (status, _) = s
        .interact(&s.session, submit(json!([{ "custom_id": "topic", "value": "again and again" }])))
        .await;
    assert_eq!(status, Status::Conflict);

    // A form cannot answer a form
    let status = s
        .callback(&interaction.id, &interaction.token.unwrap(), json!({ "type": "Modal", "modal": modal }))
        .await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn components_on_ephemeral_messages_stay_private() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let mut other_events = PubSubTestHelper::new(&format!("{}!", s.other.id)).await;
    let message = s.bot_message(buttons()).await;

    // The first answer is private and carries a button
    let (_, accepted) = s.interact(&s.session, s.click(&message.id, "ticket:open", None)).await;
    let token = next_interaction(&mut bot_events).await.token.unwrap();
    let status = s
        .callback(&accepted.unwrap().id, &token, json!({ "type": "Message", "data": {
            "content": "Sure?", "ephemeral": true,
            "components": [{ "type": "ActionRow", "components": [
                { "type": "Button", "style": "Success", "label": "Yes", "custom_id": "confirm" }
            ]}]
        }}))
        .await;
    assert_eq!(status, Status::NoContent);
    let ephemeral = match user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageCreate { .. }))
        .await
    {
        EventV1::EphemeralMessageCreate { message } => message,
        _ => unreachable!(),
    };

    // Someone else cannot use it
    let (status, _) = s.interact(&s.other_session, s.click(&ephemeral.id, "confirm", None)).await;
    assert_eq!(status, Status::NotFound);

    let (status, accepted) = s.interact(&s.session, s.click(&ephemeral.id, "confirm", None)).await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    assert!(interaction.message_ephemeral);
    let status = s
        .callback(
            &accepted.unwrap().id,
            &interaction.token.unwrap(),
            json!({ "type": "UpdateMessage", "data": { "content": "Done", "components": [] } }),
        )
        .await;
    assert_eq!(status, Status::NoContent);
    match user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageUpdate { .. }))
        .await
    {
        EventV1::EphemeralMessageUpdate { message } => {
            assert_eq!(message.id, ephemeral.id);
            assert_eq!(message.content.as_deref(), Some("Done"));
        }
        _ => unreachable!(),
    }
    assert_silent(&mut other_events, |_| true).await;
}

#[rocket::async_test]
async fn components_on_ephemeral_followups_use_that_followup() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let message = s.bot_message(buttons()).await;

    // Public first answer, then a private follow-up with its own button
    let (_, accepted) = s.interact(&s.session, s.click(&message.id, "ticket:open", None)).await;
    let id = accepted.unwrap().id;
    let token = next_interaction(&mut bot_events).await.token.unwrap();
    let status = s
        .callback(&id, &token, json!({ "type": "Message", "data": { "content": "Opened" } }))
        .await;
    assert_eq!(status, Status::NoContent);
    let response = s
        .harness
        .client
        .post(format!("/interactions/{id}/{token}/followups"))
        .header(ContentType::JSON)
        .body(
            json!({
                "content": "Close it?", "ephemeral": true,
                "components": [{ "type": "ActionRow", "components": [
                    { "type": "Button", "style": "Danger", "label": "Close", "custom_id": "ticket:close" }
                ]}]
            })
            .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let followup: v0::FollowupMessage = response.into_json().await.unwrap();
    assert!(followup.ephemeral);
    user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageCreate { message } if message.id == followup.id))
        .await;

    // The click is on the follow-up's button (this used to fail with a 500)
    let (status, accepted) = s.interact(&s.session, s.click(&followup.id, "ticket:close", None)).await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    assert!(interaction.message_ephemeral);
    assert_eq!(interaction.component.unwrap().custom_id, "ticket:close");

    // Updating edits the follow-up, not some other message
    let status = s
        .callback(
            &accepted.unwrap().id,
            &interaction.token.unwrap(),
            json!({ "type": "UpdateMessage", "data": { "content": "Closed", "components": [] } }),
        )
        .await;
    assert_eq!(status, Status::NoContent);
    match user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageUpdate { .. }))
        .await
    {
        EventV1::EphemeralMessageUpdate { message } => {
            assert_eq!(message.id, followup.id);
            assert_eq!(message.content.as_deref(), Some("Closed"));
        }
        _ => unreachable!(),
    }
}

#[rocket::async_test]
async fn autocomplete_suggestions_go_to_the_typing_user() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let command = revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        None,
        serde_json::from_value(json!({
            "name": "city", "description": "Pick a city",
            "options": [{ "type": "String", "name": "name", "description": "City", "autocomplete": true, "required": true }]
        }))
        .unwrap(),
    )
    .await
    .unwrap();

    let request = |focused: &str| {
        json!({
            "type": "Autocomplete",
            "application_id": s.bot.id,
            "channel_id": s.channel.id(),
            "command_id": command.id,
            "options": [{ "type": "String", "name": "name", "value": "دم" }],
            "focused": focused,
            "nonce": ulid::Ulid::new().to_string(),
        })
    };
    let send = |body: Value| {
        s.harness
            .client
            .post("/interactions/autocomplete")
            .header(Header::new("x-session-token", s.session.clone()))
            .header(ContentType::JSON)
            .body(body.to_string())
    };

    let response = send(request("missing")).dispatch().await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);

    let response = send(request("name")).dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    let accepted: v0::InteractionAccepted = response.into_json().await.unwrap();
    let interaction = next_interaction(&mut bot_events).await;
    assert_eq!(interaction.kind, v0::InteractionType::Autocomplete);
    assert_eq!(interaction.command.as_ref().unwrap().focused.as_deref(), Some("name"));
    let token = interaction.token.unwrap();

    // A message is not an answer to autocomplete, wrong-typed choices are refused
    assert_eq!(
        s.callback(&accepted.id, &token, json!({ "type": "Message", "data": { "content": "x" } })).await,
        Status::BadRequest
    );
    assert_eq!(
        s.callback(&accepted.id, &token, json!({ "type": "AutocompleteResult", "choices": [{ "name": "x", "value": 5 }] })).await,
        Status::BadRequest
    );
    assert_eq!(
        s.callback(&accepted.id, &token, json!({ "type": "AutocompleteResult", "choices": [{ "name": "دمشق", "value": "damascus" }] })).await,
        Status::NoContent
    );
    match user_events
        .wait_for_event(|e| matches!(e, EventV1::InteractionAutocompleteResult { .. }))
        .await
    {
        EventV1::InteractionAutocompleteResult { choices, .. } => assert_eq!(choices[0].name, "دمشق"),
        _ => unreachable!(),
    }
}

#[rocket::async_test]
async fn members_see_installed_commands_only() {
    let s = setup().await;
    let mut server_events = PubSubTestHelper::new(&s.server.id).await;

    // Registering a command tells the servers the bot is in
    let response = s
        .harness
        .client
        .post(format!("/applications/{}/commands", s.bot.id))
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "name": "hello", "description": "Hi" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    server_events
        .wait_for_event(|e| matches!(e, EventV1::ApplicationCommandsUpdate { .. }))
        .await;

    // A bot that is not installed here
    let (_, _, stranger) = s.harness.new_user().await;
    let (elsewhere, _) = s.harness.new_bot(&stranger).await;
    revolt_database::create_application_command(
        &s.harness.db,
        &elsewhere.id,
        None,
        v0::DataApplicationCommand { name: "nope".into(), description: "x".into(), ..Default::default() },
    )
    .await
    .unwrap();

    let response = s
        .harness
        .client
        .get(format!("/servers/{}/commands", s.server.id))
        .header(Header::new("x-session-token", s.session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let list: v0::ServerCommands = response.into_json().await.unwrap();
    assert_eq!(list.commands.len(), 1);
    assert_eq!(list.commands[0].name, "hello");
    assert_eq!(list.applications.len(), 1);
    assert_eq!(list.applications[0].id, s.bot.id);

    let (_, outsider, _) = s.harness.new_user().await;
    let response = s
        .harness
        .client
        .get(format!("/servers/{}/commands", s.server.id))
        .header(Header::new("x-session-token", outsider.token))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NotFound);
}

#[rocket::async_test]
async fn layout_blocks_hold_working_buttons() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let message = s
        .bot_message(json!([{
            "type": "Container", "accent": "#9075FF",
            "components": [
                { "type": "Text", "content": "## Plans" },
                { "type": "Section", "text": ["Game hosting"],
                  "button": { "type": "Button", "style": "Success", "label": "Join", "custom_id": "plan:game" } },
                { "type": "Separator" },
                { "type": "Gallery", "items": [{ "url": "https://zrzur.cc/a.png", "spoiler": true }] },
                { "type": "ActionRow", "components": [
                    { "type": "Button", "style": "Secondary", "label": "Later", "custom_id": "later" }
                ]}
            ]
        }]))
        .await;
    assert_eq!(message.components.as_ref().unwrap().len(), 1);

    // the button next to a section works like any other
    let (status, _) = s.interact(&s.session, s.click(&message.id, "plan:game", None)).await;
    assert_eq!(status, Status::Ok);
    let component = next_interaction(&mut bot_events).await.component.unwrap();
    assert_eq!(component.custom_id, "plan:game");
    assert_eq!(component.component_type, "Button");

    // form fields and files that are not attached are refused in messages
    for components in [
        json!([{ "type": "Field", "label": "x", "component": { "type": "Checkbox", "custom_id": "c" } }]),
        json!([{ "type": "File", "attachment": "01ARZ3NDEKTSV4RRFFQ69G5FAV" }]),
        json!([{ "type": "Container", "components": [{ "type": "Container", "components": [{ "type": "Text", "content": "x" }] }] }]),
    ] {
        let response = s
            .harness
            .client
            .post(format!("/channels/{}/messages", s.channel.id()))
            .header(Header::new("x-bot-token", s.bot.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "components": components }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest, "{components}");
    }
}

#[rocket::async_test]
async fn form_fields_are_checked_and_resolved() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let message = s.bot_message(buttons()).await;

    let (_, accepted) = s.interact(&s.session, s.click(&message.id, "ticket:open", None)).await;
    let accepted = accepted.unwrap();
    let token = next_interaction(&mut bot_events).await.token.unwrap();
    let options = json!([{ "label": "A", "value": "a" }, { "label": "B", "value": "b" }]);
    let modal = json!({
        "custom_id": "survey",
        "title": "استبيان",
        "components": [
            { "type": "Text", "content": "Answers go to this bot only." },
            { "type": "Field", "label": "Class", "component": { "type": "RadioGroup", "custom_id": "class", "options": options } },
            { "type": "Field", "label": "Days", "description": "Pick any", "component":
                { "type": "CheckboxGroup", "custom_id": "days", "options": options, "required": false } },
            { "type": "Field", "label": "Agree", "component": { "type": "Checkbox", "custom_id": "agree" } },
            { "type": "Field", "label": "Who", "component": { "type": "UserSelect", "custom_id": "who" } }
        ]
    });
    let status = s
        .callback(&accepted.id, &token, json!({ "type": "Modal", "modal": modal }))
        .await;
    assert_eq!(status, Status::NoContent);

    let submit = |fields: Value| {
        json!({
            "type": "ModalSubmit",
            "channel_id": s.channel.id(),
            "parent_id": accepted.id,
            "custom_id": "survey",
            "fields": fields,
            "nonce": ulid::Ulid::new().to_string(),
        })
    };
    let user = s.user.id.clone();

    // a choice that is not offered, a missing required radio, a stranger's id
    for fields in [
        json!([{ "custom_id": "class", "value": "z" }, { "custom_id": "who", "values": [user] }]),
        json!([{ "custom_id": "who", "values": [user] }]),
        json!([{ "custom_id": "class", "value": "a" }, { "custom_id": "who", "values": ["01ARZ3NDEKTSV4RRFFQ69G5FAV"] }]),
    ] {
        let (status, _) = s.interact(&s.session, submit(fields.clone())).await;
        assert_eq!(status, Status::BadRequest, "{fields}");
    }

    let (status, _) = s
        .interact(
            &s.session,
            submit(json!([
                { "custom_id": "class", "value": "b" },
                { "custom_id": "days", "values": ["a", "b"] },
                { "custom_id": "agree", "checked": true },
                { "custom_id": "who", "values": [user] }
            ])),
        )
        .await;
    assert_eq!(status, Status::Ok);
    let data = next_interaction(&mut bot_events).await.modal.unwrap();
    let field = |id: &str| data.fields.iter().find(|f| f.custom_id == id).unwrap().clone();
    assert_eq!(field("class").value.as_deref(), Some("b"));
    assert_eq!(field("days").values, Some(vec!["a".to_string(), "b".to_string()]));
    assert_eq!(field("agree").checked, Some(true));
    assert!(data.resolved.unwrap().users.contains_key(&s.user.id));
}

/// Every kind of select, with what a forged click could try: people and
/// roles from elsewhere, hidden or wrong channels, too many or too few picks
#[rocket::async_test]
async fn every_select_kind_checks_what_was_picked() {
    use revolt_database::{PartialChannel, VoiceInformation};
    use revolt_permissions::{ChannelPermission, OverrideField};

    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let role = s.harness.new_role(&s.server, 1, None).await;
    let (_, _, stranger) = s.harness.new_user().await;
    let (elsewhere, elsewhere_channels) = s.harness.new_server(&stranger).await;
    let foreign_role = s.harness.new_role(&elsewhere, 1, None).await;
    let mut voice = s.harness.new_channel(&s.server).await;
    voice
        .update(
            &s.harness.db,
            PartialChannel { voice: Some(VoiceInformation::default()), ..Default::default() },
            vec![],
        )
        .await
        .unwrap();
    let mut hidden = s.harness.new_channel(&s.server).await;
    hidden
        .update(
            &s.harness.db,
            PartialChannel {
                default_permissions: Some(OverrideField { a: 0, d: ChannelPermission::ViewChannel as i64 }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let options: Vec<Value> = (0..25)
        .map(|i| json!({ "label": format!("خريطة {i}"), "value": format!("m{i}") }))
        .collect();
    let message = s
        .bot_message(json!([
            { "type": "ActionRow", "components": [{ "type": "StringSelect", "custom_id": "maps",
                "min_values": 0, "max_values": 25, "options": options }] },
            { "type": "ActionRow", "components": [{ "type": "UserSelect", "custom_id": "who", "max_values": 2 }] },
            { "type": "ActionRow", "components": [{ "type": "MentionableSelect", "custom_id": "ping", "max_values": 2 }] },
            { "type": "ActionRow", "components": [{ "type": "ChannelSelect", "custom_id": "room",
                "channel_types": ["Voice"] }] },
            { "type": "ActionRow", "components": [{ "type": "ChannelSelect", "custom_id": "any" }] }
        ]))
        .await;

    let all: Vec<String> = (0..25).map(|i| format!("m{i}")).collect();
    let mut too_many = all.clone();
    too_many.push("m0".into());
    let refused = [
        ("maps", json!(too_many)),
        ("maps", json!(["M0"])),
        ("who", json!([stranger.id])),
        ("who", json!([role.id])),
        ("who", json!([s.user.id, s.user.id])),
        ("who", json!([s.user.id, s.other.id, s.bot.id])),
        ("ping", json!([foreign_role.id])),
        ("ping", json!([s.channel.id()])),
        ("room", json!([s.channel.id()])),
        ("room", json!([s.user.id])),
        ("any", json!([elsewhere_channels[0].id()])),
        ("any", json!([hidden.id()])),
        ("any", json!(["not-an-id"])),
        ("any", json!([])),
    ];
    for (custom_id, values) in refused {
        let (status, _) = s.interact(&s.session, s.click(&message.id, custom_id, Some(values.clone()))).await;
        assert_eq!(status, Status::BadRequest, "{custom_id} {values}");
    }
    assert_silent(&mut bot_events, |e| matches!(e, EventV1::InteractionCreate { .. })).await;

    // nothing chosen is allowed when min_values is 0; all 25 at once too
    for values in [json!([]), json!(all)] {
        let (status, _) = s.interact(&s.session, s.click(&message.id, "maps", Some(values.clone()))).await;
        assert_eq!(status, Status::Ok, "maps {values}");
        let picked = next_interaction(&mut bot_events).await.component.unwrap().values;
        assert_eq!(json!(picked), values);
    }

    let (status, _) = s
        .interact(&s.session, s.click(&message.id, "who", Some(json!([s.user.id, s.other.id]))))
        .await;
    assert_eq!(status, Status::Ok);
    let resolved = next_interaction(&mut bot_events).await.component.unwrap().resolved.unwrap();
    assert!(resolved.users.contains_key(&s.other.id) && resolved.members.contains_key(&s.other.id));

    let (status, _) = s
        .interact(&s.session, s.click(&message.id, "ping", Some(json!([role.id, s.other.id]))))
        .await;
    assert_eq!(status, Status::Ok);
    let resolved = next_interaction(&mut bot_events).await.component.unwrap().resolved.unwrap();
    assert!(resolved.roles.contains_key(&role.id) && resolved.users.contains_key(&s.other.id));

    let (status, _) = s.interact(&s.session, s.click(&message.id, "room", Some(json!([voice.id()])))).await;
    assert_eq!(status, Status::Ok);
    let resolved = next_interaction(&mut bot_events).await.component.unwrap().resolved.unwrap();
    assert!(resolved.channels.contains_key(voice.id()));
}

#[rocket::async_test]
async fn disabled_and_restricted_selects_are_refused() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let options = json!([{ "label": "A", "value": "a" }, { "label": "B", "value": "b" }]);
    let message = s
        .bot_message(json!([
            { "type": "ActionRow", "components": [{ "type": "StringSelect", "custom_id": "off",
                "disabled": true, "options": options }] },
            { "type": "ActionRow", "components": [{ "type": "StringSelect", "custom_id": "mine",
                "restrict": { "users": [s.user.id] }, "options": options }] }
        ]))
        .await;

    let (status, _) = s.interact(&s.session, s.click(&message.id, "off", Some(json!(["a"])))).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = s.interact(&s.other_session, s.click(&message.id, "mine", Some(json!(["a"])))).await;
    assert_eq!(status, Status::Forbidden);
    assert_silent(&mut bot_events, |e| matches!(e, EventV1::InteractionCreate { .. })).await;

    let (status, _) = s.interact(&s.session, s.click(&message.id, "mine", Some(json!(["b"])))).await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    assert_eq!(interaction.component.unwrap().values, vec!["b"]);

    // menus a bot may not post: line breaks, the same default twice
    for components in [
        json!([{ "type": "ActionRow", "components": [{ "type": "StringSelect", "custom_id": "x",
            "options": [{ "label": "two\nlines", "value": "a" }] }] }]),
        json!([{ "type": "ActionRow", "components": [{ "type": "UserSelect", "custom_id": "x", "max_values": 2,
            "default_values": [s.user.id, s.user.id] }] }]),
    ] {
        let response = s
            .harness
            .client
            .post(format!("/channels/{}/messages", s.channel.id()))
            .header(Header::new("x-bot-token", s.bot.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "components": components }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest, "{components}");
    }
}

/// The clicker's app shows "thinking" only for a deferred answer; a deferred
/// update just ends the button's wait
#[rocket::async_test]
async fn deferred_update_and_deferred_answer_are_told_apart() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let message = s.bot_message(buttons()).await;

    for (callback, expected) in [
        ("DeferredUpdate", v0::InteractionState::DeferredUpdate),
        ("DeferredMessage", v0::InteractionState::Deferred),
    ] {
        let (_, accepted) = s.interact(&s.session, s.click(&message.id, "ticket:open", None)).await;
        let accepted = accepted.unwrap();
        let token = next_interaction(&mut bot_events).await.token.unwrap();
        let status = s.callback(&accepted.id, &token, json!({ "type": callback })).await;
        assert_eq!(status, Status::NoContent, "{callback}");
        match user_events
            .wait_for_event(|e| matches!(e, EventV1::InteractionStateUpdate { id, .. } if id == &accepted.id))
            .await
        {
            EventV1::InteractionStateUpdate { state, .. } => assert_eq!(state, expected, "{callback}"),
            _ => unreachable!(),
        }
    }
}

#[rocket::async_test]
async fn updates_keep_blocks_that_point_at_the_message_files() {
    use revolt_database::{File, Metadata};

    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let image = ulid::Ulid::new().to_string();
    s.harness
        .db
        .insert_attachment(&File {
            id: image.clone(),
            tag: "attachments".to_string(),
            filename: "a.png".to_string(),
            hash: None,
            uploaded_at: None,
            uploader_id: Some(s.bot.id.clone()),
            used_for: None,
            deleted: None,
            reported: None,
            purged: None,
            metadata: Metadata::Image {
                width: 8,
                height: 8,
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
    let blocks = |label: &str| {
        json!([
            { "type": "Gallery", "items": [{ "attachment": image }] },
            { "type": "ActionRow", "components": [{ "type": "Button", "style": "Primary", "label": label, "custom_id": "lab:press" }] }
        ])
    };
    let response = s
        .harness
        .client
        .post(format!("/channels/{}/messages", s.channel.id()))
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "attachments": [image], "components": blocks("Press") }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let message: v0::Message = response.into_json().await.unwrap();

    let (status, accepted) = s.interact(&s.session, s.click(&message.id, "lab:press", None)).await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    let response = s
        .harness
        .client
        .post(format!(
            "/interactions/{}/{}/callback",
            accepted.unwrap().id,
            interaction.token.unwrap()
        ))
        .header(ContentType::JSON)
        .body(
            json!({ "type": "UpdateMessage", "data": { "components": blocks("Pressed") } })
                .to_string(),
        )
        .dispatch()
        .await;
    let status = response.status();
    let body = response.into_string().await.unwrap_or_default();
    assert_eq!(status, Status::NoContent, "{body}");

    // a new message still cannot point at files it does not carry
    let (status, accepted) = s.interact(&s.session, s.click(&message.id, "lab:press", None)).await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    let status = s
        .callback(
            &accepted.unwrap().id,
            &interaction.token.unwrap(),
            json!({ "type": "Message", "data": { "components": blocks("New") } }),
        )
        .await;
    assert_eq!(status, Status::BadRequest);
}
