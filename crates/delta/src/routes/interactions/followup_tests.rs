//! Follow-ups, edits and deletions with the interaction token.

use std::time::Duration;

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_database::{events::client::EventV1, Bot, Channel, Member, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

struct Setup {
    harness: TestHarness,
    bot: Bot,
    user: User,
    session: String,
    other: User,
    channel: Channel,
    command: v0::ApplicationCommand,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, session, user) = harness.new_user().await;
    let (_, _, other) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let (bot, bot_user) = harness.new_bot(&owner).await;
    for member in [&owner, &user, &other, &bot_user] {
        Member::create(&harness.db, &server, member, None).await.unwrap();
    }
    revolt_presence::create_session(&bot.id, 0).await;
    let command = revolt_database::create_application_command(
        &harness.db,
        &bot.id,
        None,
        v0::DataApplicationCommand {
            name: "work".into(),
            description: "Do work".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Setup {
        harness,
        bot,
        user,
        session: session.token,
        other,
        channel: channels.into_iter().next().unwrap(),
        command,
    }
}

impl Setup {
    /// Use the command and return (interaction id, token)
    async fn invoke(&self, bot_events: &mut PubSubTestHelper) -> (String, String) {
        let response = self
            .harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", self.session.clone()))
            .header(ContentType::JSON)
            .body(
                json!({
                    "type": "Command",
                    "application_id": self.bot.id,
                    "channel_id": self.channel.id(),
                    "command_id": self.command.id,
                    "nonce": ulid::Ulid::new().to_string(),
                })
                .to_string(),
            )
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        drop(response);
        match bot_events
            .wait_for_event(|e| matches!(e, EventV1::InteractionCreate { .. }))
            .await
        {
            EventV1::InteractionCreate { interaction } => (interaction.id, interaction.token.unwrap()),
            _ => unreachable!(),
        }
    }

    async fn send(&self, method: &str, path: String, body: Option<Value>) -> (Status, Option<Value>) {
        let client = &self.harness.client;
        let request = match method {
            "POST" => client.post(path),
            "PATCH" => client.patch(path),
            "DELETE" => client.delete(path),
            _ => unreachable!(),
        };
        let request = match body {
            Some(body) => request.header(ContentType::JSON).body(body.to_string()),
            None => request,
        };
        let response = request.dispatch().await;
        let status = response.status();
        let body = if status == Status::Ok { response.into_json().await } else { None };
        (status, body)
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

/// A bot answering in a thread goes by the same rules as any other way of
/// writing: a thread closed to the bot takes no public answer from it, a
/// private one still works
#[rocket::async_test]
async fn a_bot_cant_answer_in_a_closed_thread() {
    use revolt_permissions::ChannelPermission;
    let mut s = setup().await;
    let parent = s.channel.clone();
    let owner_session = s.session.clone();
    let response = s
        .harness
        .client
        .post(format!("/channels/{}/threads", parent.id()))
        .header(Header::new("x-session-token", owner_session))
        .header(ContentType::JSON)
        .body(json!({ "name": "Work", "content": "first" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let thread: Value = response.into_json().await.unwrap();
    s.channel = s
        .harness
        .db
        .fetch_channel(thread["_id"].as_str().unwrap())
        .await
        .unwrap();

    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let callback = |id: &str, token: &str| format!("/interactions/{id}/{token}/callback");
    let (id, token) = s.invoke(&mut bot_events).await;
    let answer = json!({ "type": "Message", "data": { "content": "done" } });
    let (status, _) = s.send("POST", callback(&id, &token), Some(answer.clone())).await;
    assert_eq!(status, Status::NoContent);

    // close the thread's parent to the bot; the person who runs the command
    // can still write there, or they would be refused before the bot hears
    let mut parent = parent;
    parent
        .update(
            &s.harness.db,
            revolt_database::PartialChannel {
                member_permissions: Some(std::collections::HashMap::from([(
                    s.bot.id.clone(),
                    revolt_permissions::OverrideField {
                        a: 0,
                        d: ChannelPermission::SendMessageInThreads as i64,
                    },
                )])),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let (id, token) = s.invoke(&mut bot_events).await;
    let (status, _) = s.send("POST", callback(&id, &token), Some(answer)).await;
    assert_eq!(status, Status::Forbidden);

    let (id, token) = s.invoke(&mut bot_events).await;
    let private = json!({ "type": "Message", "data": { "content": "only you", "ephemeral": true } });
    let (status, _) = s.send("POST", callback(&id, &token), Some(private)).await;
    assert_eq!(status, Status::NoContent);
}

#[rocket::async_test]
async fn followups_are_bounded_and_editable() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let mut other_events = PubSubTestHelper::new(&format!("{}!", s.other.id)).await;
    let (id, token) = s.invoke(&mut bot_events).await;

    // Nothing before the first answer
    let (status, _) = s
        .send("POST", format!("/interactions/{id}/{token}/followups"), Some(json!({ "content": "early" })))
        .await;
    assert_eq!(status, Status::BadRequest);

    let (status, _) = s
        .send("POST", format!("/interactions/{id}/{token}/callback"), Some(json!({ "type": "DeferredMessage", "data": { "ephemeral": true } })))
        .await;
    assert_eq!(status, Status::NoContent);

    // A private follow-up reaches only the user
    let (status, body) = s
        .send("POST", format!("/interactions/{id}/{token}/followups"), Some(json!({ "content": "step 1", "ephemeral": true })))
        .await;
    assert_eq!(status, Status::Ok);
    let private_id = body.unwrap()["_id"].as_str().unwrap().to_string();
    user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageCreate { message } if message.id == private_id))
        .await;
    assert_silent(&mut other_events, |_| true).await;

    // Edit, then delete it
    let (status, _) = s
        .send("PATCH", format!("/interactions/{id}/{token}/messages/{private_id}"), Some(json!({ "content": "step 1 done" })))
        .await;
    assert_eq!(status, Status::NoContent);
    user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageUpdate { message } if message.content.as_deref() == Some("step 1 done")))
        .await;
    let (status, _) = s
        .send("DELETE", format!("/interactions/{id}/{token}/messages/{private_id}"), None)
        .await;
    assert_eq!(status, Status::NoContent);
    user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageDelete { id, .. } if id == &private_id))
        .await;

    // A public follow-up is a normal message the token may delete
    let (status, body) = s
        .send("POST", format!("/interactions/{id}/{token}/followups"), Some(json!({ "content": "public step" })))
        .await;
    assert_eq!(status, Status::Ok);
    let public_id = body.unwrap()["_id"].as_str().unwrap().to_string();
    assert!(s.harness.db.fetch_message(&public_id).await.is_ok());
    let (status, _) = s
        .send("DELETE", format!("/interactions/{id}/{token}/messages/{public_id}"), None)
        .await;
    assert_eq!(status, Status::NoContent);
    assert!(s.harness.db.fetch_message(&public_id).await.is_err());

    // At most five follow-ups (the deleted private one no longer counts)
    for n in 0..4 {
        let (status, _) = s
            .send("POST", format!("/interactions/{id}/{token}/followups"), Some(json!({ "content": format!("n{n}"), "ephemeral": true })))
            .await;
        assert_eq!(status, Status::Ok, "follow-up {n}");
    }
    let (status, _) = s
        .send("POST", format!("/interactions/{id}/{token}/followups"), Some(json!({ "content": "too many", "ephemeral": true })))
        .await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn tokens_only_reach_their_own_messages() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let (first, first_token) = s.invoke(&mut bot_events).await;
    let (second, second_token) = s.invoke(&mut bot_events).await;

    for (id, token) in [(&first, &first_token), (&second, &second_token)] {
        let (status, _) = s
            .send("POST", format!("/interactions/{id}/{token}/callback"), Some(json!({ "type": "Message", "data": { "content": "public" } })))
            .await;
        assert_eq!(status, Status::NoContent);
    }
    let first_message = s
        .harness
        .db
        .fetch_interaction(&first)
        .await
        .unwrap()
        .original_message_id
        .unwrap();

    // The second interaction's token cannot touch the first one's message
    let (status, _) = s
        .send("PATCH", format!("/interactions/{second}/{second_token}/messages/{first_message}"), Some(json!({ "content": "hijack" })))
        .await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = s
        .send("DELETE", format!("/interactions/{second}/{second_token}/messages/{first_message}"), None)
        .await;
    assert_eq!(status, Status::NotFound);
    // Nor can a token of one interaction be used with another's id
    let (status, _) = s
        .send("DELETE", format!("/interactions/{first}/{second_token}/original"), None)
        .await;
    assert_eq!(status, Status::NotFound);

    let (status, _) = s
        .send("DELETE", format!("/interactions/{first}/{first_token}/original"), None)
        .await;
    assert_eq!(status, Status::NoContent);
    assert!(s.harness.db.fetch_message(&first_message).await.is_err());
}

#[rocket::async_test]
async fn ephemeral_messages_cannot_be_fetched_as_channel_messages() {
    let s = setup().await;
    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let (id, token) = s.invoke(&mut bot_events).await;
    s.send("POST", format!("/interactions/{id}/{token}/callback"), Some(json!({ "type": "Message", "data": { "content": "secret", "ephemeral": true } })))
        .await;
    let message = match user_events
        .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageCreate { .. }))
        .await
    {
        EventV1::EphemeralMessageCreate { message } => message,
        _ => unreachable!(),
    };

    let response = s
        .harness
        .client
        .get(format!("/channels/{}/messages/{}", s.channel.id(), message.id))
        .header(Header::new("x-session-token", s.session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NotFound);
}
