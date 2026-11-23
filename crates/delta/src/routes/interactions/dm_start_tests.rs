//! Bots in direct messages: a public bot is reachable without a shared
//! server, Start carries the bot link's payload and lets the bot write, and a
//! blocked bot can do neither.

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_database::{events::client::EventV1, Bot, PartialBot, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

struct Setup {
    harness: TestHarness,
    bot: Bot,
    person: User,
    session: String,
}

/// A person and a bot with nothing in common; `public` decides who may reach it
async fn setup(public: bool) -> Setup {
    setup_with(public, true).await
}

/// `enabled`: `features.bot_dms_enabled` (once per test process)
async fn setup_with(public: bool, enabled: bool) -> Setup {
    if enabled {
        revolt_config::overwrite_config(|config| config.features.bot_dms_enabled = true).await;
    }
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, session, person) = harness.new_user().await;
    let (mut bot, _) = harness.new_bot(&owner).await;
    if public {
        bot.update(
            &harness.db,
            PartialBot {
                public: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    }
    revolt_presence::create_session(&bot.id, 0).await;
    Setup {
        harness,
        bot,
        person,
        session: session.token,
    }
}

impl Setup {
    fn person(&self) -> Header<'static> {
        Header::new("x-session-token", self.session.clone())
    }

    fn as_bot(&self) -> Header<'static> {
        Header::new("x-bot-token", self.bot.token.clone())
    }

    async fn open_dm(&self) -> (Status, Value) {
        let response = self
            .harness
            .client
            .get(format!("/users/{}/dm", self.bot.id))
            .header(self.person())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    async fn channel(&self) -> String {
        let (status, channel) = self.open_dm().await;
        assert_eq!(status, Status::Ok, "{channel}");
        channel["_id"].as_str().unwrap().to_string()
    }

    async fn send(&self, channel: &str, header: Header<'static>, content: &str) -> Status {
        self.harness
            .client
            .post(format!("/channels/{channel}/messages"))
            .header(header)
            .header(ContentType::JSON)
            .body(json!({ "content": content }).to_string())
            .dispatch()
            .await
            .status()
    }

    async fn start(&self, channel: &str, nonce: &str, payload: Option<&str>) -> (Status, Value) {
        let response = self
            .harness
            .client
            .post("/interactions")
            .header(self.person())
            .header(ContentType::JSON)
            .body(
                json!({
                    "type": "Start",
                    "application_id": self.bot.id,
                    "channel_id": channel,
                    "payload": payload,
                    "nonce": nonce,
                })
                .to_string(),
            )
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    async fn block(&self, block: bool) {
        let request = if block {
            self.harness.client.put(format!("/users/{}/block", self.bot.id))
        } else {
            self.harness.client.delete(format!("/users/{}/block", self.bot.id))
        };
        let response = request.header(self.person()).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
    }
}

async fn next_interaction(events: &mut PubSubTestHelper) -> v0::Interaction {
    match events
        .wait_for_event(|event| matches!(event, EventV1::InteractionCreate { .. }))
        .await
    {
        EventV1::InteractionCreate { interaction } => interaction,
        _ => unreachable!(),
    }
}

#[rocket::async_test]
async fn a_public_bot_is_reachable_without_a_shared_server() {
    let s = setup(true).await;
    let channel = s.channel().await;
    assert_eq!(s.send(&channel, s.person(), "hello bot").await, Status::Ok);
    // the person wrote first, so the bot may answer
    assert_eq!(s.send(&channel, s.as_bot(), "hello person").await, Status::Ok);
}

#[rocket::async_test]
async fn switched_off_nothing_changes() {
    let s = setup_with(true, false).await;
    // a public bot with nothing in common stays out of reach
    let (status, body) = s.open_dm().await;
    assert_eq!(body["type"], "DirectMessagesRestricted", "{status} {body}");

    // a bot with a server in common: the DM opens, but Start is off
    let (_, _, owner) = s.harness.new_user().await;
    let (bot, bot_user) = s.harness.new_bot(&owner).await;
    let (server, _) = s.harness.new_server(&owner).await;
    for member in [&s.person, &bot_user] {
        revolt_database::Member::create(&s.harness.db, &server, member, None)
            .await
            .unwrap();
    }
    revolt_presence::create_session(&bot.id, 0).await;
    let shared = Setup {
        harness: s.harness,
        bot,
        person: s.person,
        session: s.session,
    };
    let channel = shared.channel().await;
    let (status, body) = shared.start(&channel, "off", None).await;
    assert_ne!(status, Status::Ok);
    assert_eq!(body["type"], "FeatureDisabled", "{body}");
}

#[rocket::async_test]
async fn a_private_bot_stays_out_of_reach() {
    let s = setup(false).await;
    let (status, body) = s.open_dm().await;
    assert_ne!(status, Status::Ok);
    assert_eq!(body["type"], "DirectMessagesRestricted", "{body}");
}

#[rocket::async_test]
async fn start_carries_the_payload_and_lets_the_bot_write() {
    let s = setup(true).await;
    let channel = s.channel().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut channel_events = PubSubTestHelper::new(&channel).await;

    // before Start the bot may not write to someone who never wrote to it
    assert_eq!(s.send(&channel, s.as_bot(), "spam").await, Status::Forbidden);

    let (status, accepted) = s.start(&channel, "start-1", Some("ref-42_a")).await;
    assert_eq!(status, Status::Ok, "{accepted}");
    let interaction = next_interaction(&mut bot_events).await;
    assert_eq!(interaction.kind, v0::InteractionType::Start);
    assert_eq!(interaction.id, accepted["_id"].as_str().unwrap());
    assert_eq!(interaction.server_id, None);
    assert_eq!(interaction.channel_id, channel);
    assert_eq!(interaction.user.id, s.person.id);
    assert_eq!(
        interaction.start,
        Some(v0::InteractionStartData {
            payload: Some("ref-42_a".into())
        })
    );

    // the bot answers in the DM with a button
    let token = interaction.token.unwrap();
    let response = s
        .harness
        .client
        .post(format!("/interactions/{}/{token}/callback", interaction.id))
        .header(ContentType::JSON)
        .body(
            json!({ "type": "Message", "data": { "content": "Welcome", "components": [
                { "type": "ActionRow", "components": [
                    { "type": "Button", "style": "Primary", "label": "Go", "custom_id": "go" }
                ]}
            ]}})
            .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    let message = channel_events.wait_for_message().await;
    assert_eq!(message.author, s.bot.id);
    assert_eq!(
        message.interaction.map(|used| used.kind),
        Some(v0::InteractionType::Start)
    );

    // and may write to the person from now on
    assert_eq!(s.send(&channel, s.as_bot(), "news").await, Status::Ok);

    // Start without a payload: `start` is there, `payload` is not
    let (status, _) = s.start(&channel, "start-2", None).await;
    assert_eq!(status, Status::Ok);
    let interaction = next_interaction(&mut bot_events).await;
    assert_eq!(interaction.start, Some(v0::InteractionStartData { payload: None }));
}

#[rocket::async_test]
async fn start_is_refused_with_a_bad_payload_or_outside_the_dm() {
    let s = setup(true).await;
    let channel = s.channel().await;
    let long = "x".repeat(65);
    for bad in ["has space", "طويل", long.as_str()] {
        let (status, body) = s.start(&channel, &format!("bad-{}", bad.len()), Some(bad)).await;
        assert_eq!(status, Status::BadRequest, "{bad}: {body}");
    }

    // not in a server channel, even with the bot installed there
    let (server, channels) = s.harness.new_server(&s.person).await;
    let bot_user = s.harness.db.fetch_user(&s.bot.id).await.unwrap();
    revolt_database::Member::create(&s.harness.db, &server, &bot_user, None)
        .await
        .unwrap();
    let (status, _) = s.start(channels[0].id(), "server", None).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn a_blocked_bot_cannot_write_or_be_started() {
    let s = setup(true).await;
    let channel = s.channel().await;
    let (status, _) = s.start(&channel, "before", None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(s.send(&channel, s.as_bot(), "hi").await, Status::Ok);

    // a button the bot sent before the person stopped it
    let response = s
        .harness
        .client
        .post(format!("/channels/{channel}/messages"))
        .header(s.as_bot())
        .header(ContentType::JSON)
        .body(
            json!({ "content": "Pick", "components": [{ "type": "ActionRow", "components": [
                { "type": "Button", "style": "Primary", "label": "Go", "custom_id": "go" }
            ]}]})
            .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let message: Value = response.into_json().await.unwrap();

    s.block(true).await;
    assert_eq!(s.send(&channel, s.as_bot(), "still here?").await, Status::Forbidden);
    let (status, body) = s.start(&channel, "while-blocked", None).await;
    assert_eq!(body["type"], "Blocked", "{status} {body}");
    // and clicks on its old buttons reach it no more
    let response = s
        .harness
        .client
        .post("/interactions")
        .header(s.person())
        .header(ContentType::JSON)
        .body(
            json!({ "type": "Component", "channel_id": channel, "message_id": message["_id"],
                "custom_id": "go", "nonce": "click-blocked" })
            .to_string(),
        )
        .dispatch()
        .await;
    let status = response.status();
    let body: Value = response.into_json().await.unwrap_or(Value::Null);
    assert_eq!(body["type"], "Blocked", "{status} {body}");

    // blocking stopped the bot: after an unblock it waits for Start again
    s.block(false).await;
    assert_eq!(s.send(&channel, s.as_bot(), "back").await, Status::Forbidden);
    let (status, _) = s.start(&channel, "after", None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(s.send(&channel, s.as_bot(), "back").await, Status::Ok);
}
