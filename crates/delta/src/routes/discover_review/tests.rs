//! The Discover review desk: the bot can read the queue and hand back
//! clicks; only a person holding the reviewer role decides anything.

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_config::FeaturesDiscoverReview;
use revolt_database::{
    events::client::EventV1, Bot, Channel, DiscoverRequestStatus, DiscoverRequestType, Invite,
    Member, PartialMember, PartialServer, Server, User,
};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

/// These walks are long: in a debug build their futures do not fit the
/// stack of a test thread, so each runs on a thread with a larger one.
fn on_big_stack<F, Fut>(walk: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || rocket::async_test(walk()))
        .unwrap()
        .join()
        .unwrap();
}

struct Desk {
    harness: TestHarness,
    bot: Bot,
    /// The staff channel of the review server
    channel: Channel,
    /// Another channel of the review server
    lobby: Channel,
    role: String,
    reviewer: User,
    reviewer_session: String,
    second: User,
    second_session: String,
    /// A member of the review server without the role
    guest: User,
    guest_session: String,
    applicant: User,
    applicant_session: String,
    /// The server that asks to be listed
    applied: Server,
    bot_events: PubSubTestHelper,
}

async fn give_role(harness: &TestHarness, server: &Server, user: &User, roles: Vec<String>) {
    let mut member = harness.db.fetch_member(&server.id, &user.id).await.unwrap();
    member
        .update(
            &harness.db,
            PartialMember {
                roles: Some(roles),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

async fn desk() -> Desk {
    let harness = TestHarness::new().await;
    let (_, _, team) = harness.new_user().await;
    let (_, reviewer_session, reviewer) = harness.new_user().await;
    let (_, second_session, second) = harness.new_user().await;
    let (_, guest_session, guest) = harness.new_user().await;
    let (_, applicant_session, applicant) = harness.new_user().await;

    let (server, channels) = harness.new_server(&team).await;
    let channel = channels.into_iter().next().unwrap();
    let lobby = harness.new_channel(&server).await;
    let role = harness.new_role(&server, 1, None).await;
    let (bot, bot_user) = harness.new_bot(&team).await;
    for member in [&reviewer, &second, &guest, &bot_user] {
        Member::create(&harness.db, &server, member, None).await.unwrap();
    }
    give_role(&harness, &server, &reviewer, vec![role.id.clone()]).await;
    give_role(&harness, &server, &second, vec![role.id.clone()]).await;
    revolt_presence::create_session(&bot.id, 0).await;

    super::test_settings::register(FeaturesDiscoverReview {
        server: server.id.clone(),
        channel: channel.id().to_string(),
        channel_bots: String::new(),
        role: role.id.clone(),
        remove_role: String::new(),
        bot: bot.id.clone(),
    });

    let (applied, _) = harness.new_server(&applicant).await;
    let bot_events = PubSubTestHelper::new(&format!("{}!", bot.id)).await;

    let desk = Desk {
        harness,
        bot,
        channel,
        lobby,
        role: role.id,
        reviewer,
        reviewer_session: reviewer_session.token,
        second,
        second_session: second_session.token,
        guest,
        guest_session: guest_session.token,
        applicant,
        applicant_session: applicant_session.token,
        applied,
        bot_events,
    };
    desk.file(&desk.applicant_session, &desk.applied.id).await;
    desk
}

impl Desk {
    /// The owner asks for their server to be listed
    async fn file(&self, session: &str, server: &str) {
        let response = self
            .harness
            .client
            .put(format!("/servers/{server}/discover"))
            .header(Header::new("x-session-token", session.to_string()))
            .header(ContentType::JSON)
            .body(r#"{"tags":["gaming"],"category":"gaming","language":"ar"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
    }

    async fn as_bot(&self, token: &str, path: &str, body: Option<Value>) -> (Status, Value) {
        let request = match &body {
            Some(body) => self
                .harness
                .client
                .post(path.to_string())
                .header(ContentType::JSON)
                .body(body.to_string()),
            None => self.harness.client.get(path.to_string()),
        };
        let response = request
            .header(Header::new("x-bot-token", token.to_string()))
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    async fn queue(&self) -> Vec<Value> {
        let (status, body) = self.as_bot(&self.bot.token, "/discover/review/queue", None).await;
        assert_eq!(status, Status::Ok);
        body.as_array().cloned().unwrap()
    }

    /// The key of the applied server's current filing
    async fn key(&self) -> String {
        self.queue()
            .await
            .iter()
            .find(|entry| entry["id"] == self.applied.id.as_str())
            .map(|entry| entry["key"].as_str().unwrap().to_string())
            .expect("the request is in the queue")
    }

    /// The bot posts a notice with one plain button per action
    async fn notice(&self, channel: &Channel, key: &str) -> String {
        let button = |action: &str| {
            json!({ "type": "Button", "style": "Secondary", "label": action,
                    "custom_id": format!("dr:{action}:{key}") })
        };
        let components = json!([{ "type": "ActionRow", "components": [
            button("review"), button("approve"), button("deny"), button("visit"), button("ask-deny")
        ]}, { "type": "ActionRow", "components": [button("remove")] }]);
        let response = self
            .harness
            .client
            .post(format!("/channels/{}/messages", channel.id()))
            .header(Header::new("x-bot-token", self.bot.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "content": "New Discover request", "components": components }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        response.into_json::<v0::Message>().await.unwrap().id
    }

    async fn interact(&self, session: &str, body: Value) -> (Status, Value) {
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
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    /// A person presses a button; gives what the bot received: id and token
    async fn press(
        &mut self,
        session: &str,
        channel: &Channel,
        message: &str,
        custom_id: &str,
    ) -> (String, String) {
        let (status, accepted) = self
            .interact(
                session,
                json!({
                    "type": "Component",
                    "channel_id": channel.id(),
                    "message_id": message,
                    "custom_id": custom_id,
                    "nonce": ulid::Ulid::new().to_string(),
                }),
            )
            .await;
        assert_eq!(status, Status::Ok, "{accepted}");
        let id = accepted["_id"].as_str().unwrap().to_string();
        (id.clone(), self.token_of(&id).await)
    }

    async fn token_of(&mut self, interaction: &str) -> String {
        match self
            .bot_events
            .wait_for_event(
                |e| matches!(e, EventV1::InteractionCreate { interaction: i } if i.id == interaction),
            )
            .await
        {
            EventV1::InteractionCreate { interaction } => interaction.token.unwrap(),
            _ => unreachable!(),
        }
    }

    /// A reviewer opens the rejection form and sends it with `reason`
    async fn reject(&mut self, session: &str, message: &str, key: &str, reason: &str) -> (String, String) {
        self.form(session, message, key, "deny", reason).await
    }

    /// A person is shown a form of the desk (`deny` or `remove`) and sends it with `reason`
    async fn form(&mut self, session: &str, message: &str, key: &str, action: &str, reason: &str) -> (String, String) {
        let channel = self.channel.clone();
        let (parent, token) = self
            .press(session, &channel, message, &format!("dr:ask-deny:{key}"))
            .await;
        let form = format!("dr:{action}:{key}");
        let modal = json!({
            "custom_id": form,
            "title": "Reject",
            "components": [{ "type": "ActionRow", "components": [{
                "type": "TextInput", "custom_id": "reason", "style": "Paragraph",
                "label": "Reason", "min_length": 3, "max_length": 512
            }]}]
        });
        let status = self
            .harness
            .client
            .post(format!("/interactions/{parent}/{token}/callback"))
            .header(ContentType::JSON)
            .body(json!({ "type": "Modal", "modal": modal }).to_string())
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::NoContent);
        let (status, accepted) = self
            .interact(
                session,
                json!({
                    "type": "ModalSubmit",
                    "channel_id": channel.id(),
                    "parent_id": parent,
                    "custom_id": form,
                    "fields": [{ "custom_id": "reason", "value": reason }],
                    "nonce": ulid::Ulid::new().to_string(),
                }),
            )
            .await;
        assert_eq!(status, Status::Ok, "{accepted}");
        let id = accepted["_id"].as_str().unwrap().to_string();
        (id.clone(), self.token_of(&id).await)
    }

    async fn act(&self, pressed: &(String, String)) -> (Status, Value) {
        self.as_bot(
            &self.bot.token,
            "/discover/review/act",
            Some(json!({ "interaction_id": pressed.0, "token": pressed.1 })),
        )
        .await
    }

    async fn request(&self) -> revolt_database::DiscoverRequest {
        self.harness
            .db
            .fetch_discover_request_by_item_id(DiscoverRequestType::Server, &self.applied.id)
            .await
            .unwrap()
    }

    /// What the applicant was told in their private messages
    async fn told(&self) -> Vec<String> {
        self.told_by(revolt_database::OfficialAccount::Zrzur.id()).await
    }

    /// What one official account wrote to the applicant
    async fn told_by(&self, official: &str) -> Vec<String> {
        let Ok(channel) = self
            .harness
            .db
            .find_direct_message_channel(official, &self.applicant.id)
            .await
        else {
            return vec![];
        };
        self.harness
            .db
            .fetch_messages(revolt_database::MessageQuery {
                limit: Some(20),
                filter: revolt_database::MessageFilter {
                    channel: Some(channel.id().to_string()),
                    ..Default::default()
                },
                time_period: revolt_database::MessageTimePeriod::Absolute {
                    before: None,
                    after: None,
                    sort: None,
                },
            })
            .await
            .unwrap()
            .into_iter()
            .filter_map(|message| message.content)
            .collect()
    }

    /// (action, actor, outcome) of the desk's audit records
    async fn audit(&self) -> Vec<(String, String, String)> {
        self.harness
            .db
            .fetch_platform_audit(revolt_database::PlatformAuditQuery {
                limit: 500,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.target_id.as_deref() == Some(self.applied.id.as_str()))
            .map(|e| (e.action, e.actor_id, format!("{:?}", e.outcome)))
            .collect()
    }
}

#[test]
fn the_routes_exist_only_for_the_desk_bot() {
    on_big_stack(the_routes_exist_only_for_the_desk_bot_walk);
}

async fn the_routes_exist_only_for_the_desk_bot_walk() {
    let d = desk().await;
    let body = json!({ "interaction_id": "x", "token": "y" });

    // no sign-in
    let response = d.harness.client.get("/discover/review/queue").dispatch().await;
    assert_ne!(response.status(), Status::Ok);

    // a person, a reviewer included: these routes are not for people
    for session in [&d.reviewer_session, &d.guest_session] {
        for (path, data) in [
            ("/discover/review/queue".to_string(), None),
            (format!("/discover/review/requests/server/{}", d.applied.id), None),
            ("/discover/review/act".to_string(), Some(body.clone())),
        ] {
            let request = match &data {
                Some(data) => d.harness.client.post(path.clone()).header(ContentType::JSON).body(data.to_string()),
                None => d.harness.client.get(path.clone()),
            };
            let status = request
                .header(Header::new("x-session-token", session.to_string()))
                .dispatch()
                .await
                .status();
            assert_eq!(status, Status::NotFound, "{path}");
        }
    }

    // a privileged person: still not the bot
    let (_, admin_session, admin) = d.harness.new_user().await;
    d.harness.make_privileged(&admin.id).await;
    let status = d
        .harness
        .client
        .get("/discover/review/queue")
        .header(Header::new("x-session-token", admin_session.token.clone()))
        .dispatch()
        .await
        .status();
    assert_eq!(status, Status::NotFound);

    // another bot, even one in the review server
    let (other_bot, other_user) = d.harness.new_bot(&d.guest).await;
    let server = d.harness.db.fetch_server(d.channel.server().unwrap()).await.unwrap();
    Member::create(&d.harness.db, &server, &other_user, None).await.unwrap();
    let (status, _) = d.as_bot(&other_bot.token, "/discover/review/queue", None).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = d.as_bot(&other_bot.token, "/discover/review/act", Some(body)).await;
    assert_eq!(status, Status::NotFound);

    // the desk bot reads the queue
    let queue = d.queue().await;
    let entry = queue.iter().find(|e| e["id"] == d.applied.id.as_str()).unwrap();
    assert_eq!(entry["kind"], "server");
    assert_eq!(entry["status"], "Pending");
    assert_eq!(entry["name"], "Test Server");
    assert_eq!(entry["owner_id"], d.applicant.id.as_str());
    assert_eq!(
        entry["owner_name"],
        format!("{}#{}", d.applicant.username, d.applicant.discriminator)
    );
    assert_eq!(entry["access"], "Open");
    assert_eq!(entry["listable"], true);
    assert_eq!(entry["tags"], json!(["gaming"]));
    assert_eq!(entry["category"], "gaming");
    assert_eq!(entry["language"], "ar");
    let submitted = entry["submitted_at"].as_i64().unwrap();
    assert!(submitted > 0);
    assert_eq!(entry["key"], format!("server:{}:{submitted}", d.applied.id));
    assert!(entry["created_at"].as_i64().unwrap() > 0);
    // no picture, no banner, no marks on a plain server
    assert!(entry.get("icon").is_none());
    assert!(entry.get("banner").is_none());
    assert_eq!(entry["marks"], json!([]));

    // an official, verified server says so
    let mut applied = d.harness.db.fetch_server(&d.applied.id).await.unwrap();
    applied
        .update(
            &d.harness.db,
            PartialServer {
                flags: Some(3),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let queue = d.queue().await;
    let entry = queue.iter().find(|e| e["id"] == d.applied.id.as_str()).unwrap();
    assert_eq!(entry["marks"], json!(["Official", "Verified"]));
}

#[test]
fn with_a_bots_channel_a_server_is_decided_in_the_servers_channel_only() {
    on_big_stack(with_a_bots_channel_walk);
}

async fn with_a_bots_channel_walk() {
    let mut d = desk().await;
    let channel = d.channel.clone();
    let lobby = d.lobby.clone();
    let session = d.reviewer_session.clone();
    // the second staff channel is for bots
    let server = d.channel.server().unwrap().to_string();
    super::test_settings::register(FeaturesDiscoverReview {
        server,
        channel: channel.id().to_string(),
        channel_bots: lobby.id().to_string(),
        role: d.role.clone(),
        remove_role: String::new(),
        bot: d.bot.id.clone(),
    });
    let key = d.key().await;

    // a server's button pressed in the bots' channel does nothing
    let elsewhere = d.notice(&lobby, &key).await;
    let pressed = d.press(&session, &lobby, &elsewhere, &format!("dr:review:{key}")).await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(d.request().await.status, DiscoverRequestStatus::Pending);

    // in the servers' channel it works as before
    let message = d.notice(&channel, &key).await;
    let pressed = d.press(&session, &channel, &message, &format!("dr:review:{key}")).await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok, "{outcome}");
    assert_eq!(outcome["applied"], true);

    // a bot's button pressed in the servers' channel does nothing either
    let bot_key = format!("bot:{}:0", d.bot.id);
    let message = d.notice(&channel, &bot_key).await;
    let pressed = d.press(&session, &channel, &message, &format!("dr:review:{bot_key}")).await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::Forbidden);
}

#[test]
fn a_reviewer_approves_and_it_is_recorded_under_their_name() {
    on_big_stack(a_reviewer_approves_and_it_is_recorded_under_their_name_walk);
}

async fn a_reviewer_approves_and_it_is_recorded_under_their_name_walk() {
    let mut d = desk().await;
    let key = d.key().await;
    let channel = d.channel.clone();
    let message = d.notice(&channel, &key).await;
    let session = d.reviewer_session.clone();

    // under review first: no message to the owner
    let pressed = d.press(&session, &channel, &message, &format!("dr:review:{key}")).await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok, "{outcome}");
    assert_eq!(outcome["applied"], true);
    assert_eq!(outcome["status"], "UnderReview");
    assert_eq!(d.request().await.status, DiscoverRequestStatus::UnderReview);
    assert_eq!(d.request().await.acted_by.as_deref(), Some(d.reviewer.id.as_str()));
    assert!(d.told().await.is_empty());
    let queue = d.queue().await;
    let entry = queue.iter().find(|e| e["id"] == d.applied.id.as_str()).unwrap();
    assert_eq!(entry["status"], "UnderReview");
    assert_eq!(
        entry["acted_by_name"],
        format!("{}#{}", d.reviewer.username, d.reviewer.discriminator)
    );

    // approve
    let pressed = d.press(&session, &channel, &message, &format!("dr:approve:{key}")).await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok, "{outcome}");
    assert_eq!(outcome["applied"], true);
    assert_eq!(outcome["status"], "Approved");
    assert_eq!(outcome["name"], "Test Server");
    assert_eq!(outcome["reviewer"], d.reviewer.id.as_str());

    assert!(d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);
    let request = d.request().await;
    assert_eq!(request.status, DiscoverRequestStatus::Approved(None));
    assert_eq!(request.acted_by.as_deref(), Some(d.reviewer.id.as_str()));
    assert!(request.acted_at.is_some());

    // the person, not the bot, is in the audit log
    let audit = d.audit().await;
    assert!(audit.contains(&(
        "discover.review.approve".to_string(),
        d.reviewer.id.clone(),
        "Success".to_string()
    )));
    assert!(audit.iter().all(|(_, actor, _)| actor != &d.bot.id));

    // the owner is told once
    let told = d.told().await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("Test Server") && told[0].contains("Discover"), "{}", told[0]);

    // the same press again, and a new press: nothing more happens
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(outcome["applied"], false);
    assert_eq!(outcome["problem"], "already_decided");
    let second_session = d.second_session.clone();
    let late = d.press(&second_session, &channel, &message, &format!("dr:approve:{key}")).await;
    let (_, outcome) = d.act(&late).await;
    assert_eq!(outcome["applied"], false);
    assert_eq!(outcome["problem"], "already_decided");
    assert_eq!(
        outcome["acted_by_name"],
        format!("{}#{}", d.reviewer.username, d.reviewer.discriminator)
    );
    let late = d.reject(&second_session, &message, &key, "Too late for this").await;
    let (_, outcome) = d.act(&late).await;
    assert_eq!(outcome["problem"], "already_decided");
    assert!(d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);
    assert_eq!(d.told().await.len(), 1);

    // gone from the queue; its state says who decided, not why
    assert!(d.queue().await.iter().all(|e| e["id"] != d.applied.id.as_str()));
    let (status, state) = d
        .as_bot(&d.bot.token, &format!("/discover/review/requests/server/{}", d.applied.id), None)
        .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(state["status"], "Approved");
    assert_eq!(state["acted_by"], d.reviewer.id.as_str());
}

#[test]
fn a_rejection_needs_the_form_and_its_reason() {
    on_big_stack(a_rejection_needs_the_form_and_its_reason_walk);
}

async fn a_rejection_needs_the_form_and_its_reason_walk() {
    let mut d = desk().await;
    let key = d.key().await;
    let channel = d.channel.clone();
    let message = d.notice(&channel, &key).await;
    let session = d.reviewer_session.clone();

    // a button cannot reject, whatever it is called
    let pressed = d.press(&session, &channel, &message, &format!("dr:deny:{key}")).await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(d.request().await.status, DiscoverRequestStatus::Pending);

    // a form cannot approve
    let channel_id = channel.id().to_string();
    let (parent, token) = d.press(&session, &channel, &message, &format!("dr:ask-deny:{key}")).await;
    let form = format!("dr:approve:{key}");
    let modal = json!({ "custom_id": form, "title": "x", "components": [{ "type": "ActionRow", "components": [{
        "type": "TextInput", "custom_id": "reason", "style": "Short", "label": "Reason" }]}] });
    d.harness
        .client
        .post(format!("/interactions/{parent}/{token}/callback"))
        .header(ContentType::JSON)
        .body(json!({ "type": "Modal", "modal": modal }).to_string())
        .dispatch()
        .await;
    let (status, accepted) = d
        .interact(
            &session,
            json!({ "type": "ModalSubmit", "channel_id": channel_id, "parent_id": parent, "custom_id": form,
                    "fields": [{ "custom_id": "reason", "value": "sneaky" }], "nonce": ulid::Ulid::new().to_string() }),
        )
        .await;
    assert_eq!(status, Status::Ok, "{accepted}");
    let id = accepted["_id"].as_str().unwrap().to_string();
    let token = d.token_of(&id).await;
    let (status, _) = d.act(&(id, token)).await;
    assert_eq!(status, Status::BadRequest);
    assert!(!d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);

    // the form with a reason
    let pressed = d.reject(&session, &message, &key, "  The description is empty.  ").await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok, "{outcome}");
    assert_eq!(outcome["applied"], true);
    assert_eq!(outcome["status"], "Denied");
    assert_eq!(outcome["reason"], "The description is empty.");
    assert_eq!(
        d.request().await.status,
        DiscoverRequestStatus::Denied(Some("The description is empty.".to_string()))
    );
    assert!(!d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);
    let told = d.told().await;
    assert_eq!(told.len(), 1);
    assert!(told[0].contains("The description is empty."), "{}", told[0]);
    assert!(d.audit().await.contains(&(
        "discover.review.deny".to_string(),
        d.reviewer.id.clone(),
        "Success".to_string()
    )));

    // the state shows no reason to the bot
    let (_, state) = d
        .as_bot(&d.bot.token, &format!("/discover/review/requests/server/{}", d.applied.id), None)
        .await;
    assert_eq!(state["status"], "Denied");
    assert!(!state.to_string().contains("description"));

    // the owner files again: a new filing, and the old notice is spent
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    d.file(&d.applicant_session.clone(), &d.applied.id.clone()).await;
    let fresh = d.key().await;
    assert_ne!(fresh, key);
    let request = d.request().await;
    assert_eq!(request.status, DiscoverRequestStatus::Pending);
    assert_eq!(request.acted_by, None);
    let stale = d.press(&session, &channel, &message, &format!("dr:approve:{key}")).await;
    let (_, outcome) = d.act(&stale).await;
    assert_eq!(outcome["applied"], false);
    assert_eq!(outcome["problem"], "already_decided");
    assert!(!d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);

    // the new notice works
    let message = d.notice(&channel, &fresh).await;
    let pressed = d.press(&session, &channel, &message, &format!("dr:approve:{fresh}")).await;
    let (_, outcome) = d.act(&pressed).await;
    assert_eq!(outcome["applied"], true);
    assert!(d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);
}

#[test]
fn only_a_reviewer_in_the_review_channel_decides() {
    on_big_stack(only_a_reviewer_in_the_review_channel_decides_walk);
}

async fn only_a_reviewer_in_the_review_channel_decides_walk() {
    let mut d = desk().await;
    let key = d.key().await;
    let channel = d.channel.clone();
    let lobby = d.lobby.clone();
    let message = d.notice(&channel, &key).await;
    let approve = format!("dr:approve:{key}");
    let untouched = |request: revolt_database::DiscoverRequest| {
        assert_eq!(request.status, DiscoverRequestStatus::Pending);
        assert_eq!(request.acted_by, None);
    };

    // a made-up interaction, and a real one with a wrong token
    let (status, _) = d.act(&(ulid::Ulid::new().to_string(), "a".repeat(64))).await;
    assert_eq!(status, Status::NotFound);
    let guest_session = d.guest_session.clone();
    let pressed = d.press(&guest_session, &channel, &message, &approve).await;
    let (status, _) = d.act(&(pressed.0.clone(), "b".repeat(64))).await;
    assert_eq!(status, Status::NotFound);

    // a member without the role (the button here is not restricted)
    let (status, body) = d.act(&pressed).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    untouched(d.request().await);

    // a reviewer whose role went between the press and the decision
    let second_session = d.second_session.clone();
    let pressed = d.press(&second_session, &channel, &message, &approve).await;
    let server = d.harness.db.fetch_server(channel.server().unwrap()).await.unwrap();
    give_role(&d.harness, &server, &d.second, vec![]).await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::Forbidden);
    untouched(d.request().await);

    // a reviewer who left the server after the press
    give_role(&d.harness, &server, &d.second, vec![d.role.clone()]).await;
    let pressed = d.press(&second_session, &channel, &message, &approve).await;
    d.harness.db.soft_delete_member(&revolt_database::MemberCompositeKey {
        server: server.id.clone(),
        user: d.second.id.clone(),
    })
    .await
    .unwrap();
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::Forbidden);
    untouched(d.request().await);

    // a reviewer, the same buttons, but in another channel
    let session = d.reviewer_session.clone();
    let elsewhere = d.notice(&lobby, &key).await;
    let pressed = d.press(&session, &lobby, &elsewhere, &approve).await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::Forbidden);
    untouched(d.request().await);

    // a button that names no action of the desk
    let pressed = d.press(&session, &channel, &message, &format!("dr:ask-deny:{key}")).await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::BadRequest);
    untouched(d.request().await);

    // nothing of this reached the owner or listed the server
    assert!(d.told().await.is_empty());
    assert!(!d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);
    assert!(d.audit().await.iter().all(|(_, _, outcome)| outcome != "Success"));

    // and the reviewer in the right place still can
    let pressed = d.press(&session, &channel, &message, &approve).await;
    let (_, outcome) = d.act(&pressed).await;
    assert_eq!(outcome["applied"], true);
}

#[test]
fn own_requests_unlistable_servers_and_visits() {
    on_big_stack(own_requests_unlistable_servers_and_visits_walk);
}

async fn own_requests_unlistable_servers_and_visits_walk() {
    let mut d = desk().await;
    let channel = d.channel.clone();
    let session = d.reviewer_session.clone();

    // a reviewer's own server
    let (own, _) = d.harness.new_server(&d.reviewer).await;
    d.file(&session, &own.id).await;
    let own_key = d
        .queue()
        .await
        .iter()
        .find(|e| e["id"] == own.id.as_str())
        .map(|e| e["key"].as_str().unwrap().to_string())
        .unwrap();
    let message = d.notice(&channel, &own_key).await;
    let pressed = d.press(&session, &channel, &message, &format!("dr:approve:{own_key}")).await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(outcome["applied"], false);
    assert_eq!(outcome["problem"], "own_item");
    assert!(!d.harness.db.fetch_server(&own.id).await.unwrap().discoverable);
    // another reviewer may
    let second_session = d.second_session.clone();
    let pressed = d.press(&second_session, &channel, &message, &format!("dr:approve:{own_key}")).await;
    let (_, outcome) = d.act(&pressed).await;
    assert_eq!(outcome["applied"], true);

    // a visit: a one-use invite made in the reviewer's name
    let key = d.key().await;
    let message = d.notice(&channel, &key).await;
    let pressed = d.press(&session, &channel, &message, &format!("dr:visit:{key}")).await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok, "{outcome}");
    assert_eq!(outcome["applied"], true);
    let code = outcome["invite"].as_str().unwrap();
    match d.harness.db.fetch_invite(code).await.unwrap() {
        Invite::Server { server, creator, max_uses, expires, .. } => {
            assert_eq!(server, d.applied.id);
            assert_eq!(creator, d.reviewer.id);
            assert_eq!(max_uses, Some(1));
            assert!(expires.is_some());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(d.request().await.status, DiscoverRequestStatus::Pending);
    assert!(d.audit().await.contains(&(
        "discover.review.visit".to_string(),
        d.reviewer.id.clone(),
        "Success".to_string()
    )));
    // the same press handed back again gives no second invite
    let (status, again) = d.act(&pressed).await;
    assert_eq!(status, Status::Gone, "{again}");
    assert!(again["invite"].is_null());
    let visits = d
        .audit()
        .await
        .iter()
        .filter(|(action, _, _)| action == "discover.review.visit")
        .count();
    assert_eq!(visits, 1);

    // a reviewer who is already inside gets no invite: joining again would fail
    Member::create_from(
        &d.harness.db,
        &d.applied,
        &d.reviewer,
        None,
        revolt_database::InsightJoinSource::Invite,
    )
    .await
    .unwrap();
    let pressed = d.press(&session, &channel, &message, &format!("dr:visit:{key}")).await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok, "{outcome}");
    assert_eq!(outcome["applied"], false);
    assert_eq!(outcome["problem"], "already_member");
    assert!(outcome["invite"].is_null());
    assert_eq!(outcome["id"], d.applied.id.as_str());
    let visits = d
        .audit()
        .await
        .iter()
        .filter(|(action, _, _)| action == "discover.review.visit")
        .count();
    assert_eq!(visits, 1);

    // a server that is not open to join cannot be listed
    let mut applied = d.harness.db.fetch_server(&d.applied.id).await.unwrap();
    applied
        .update(
            &d.harness.db,
            PartialServer {
                access: Some(v0::ServerAccess::Approval),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let queue = d.queue().await;
    let entry = queue.iter().find(|e| e["id"] == d.applied.id.as_str()).unwrap();
    assert_eq!(entry["access"], "Approval");
    assert_eq!(entry["listable"], false);
    let pressed = d.press(&session, &channel, &message, &format!("dr:approve:{key}")).await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(outcome["applied"], false);
    assert_eq!(outcome["problem"], "not_listable");
    assert_eq!(d.request().await.status, DiscoverRequestStatus::Pending);
    // and nobody gets a way in past its approval step
    let pressed = d.press(&session, &channel, &message, &format!("dr:visit:{key}")).await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(outcome["applied"], false);
    assert_eq!(outcome["problem"], "not_listable");
    assert!(outcome["invite"].is_null());
    // it can still be rejected with that reason
    let pressed = d.reject(&session, &message, &key, "Open your server to everyone first").await;
    let (_, outcome) = d.act(&pressed).await;
    assert_eq!(outcome["applied"], true);

    // the owner takes a request back: the notice learns it
    let (_, state) = d
        .as_bot(&d.bot.token, &format!("/discover/review/requests/server/{}", ulid::Ulid::new()), None)
        .await;
    assert_eq!(state["status"], "Withdrawn");
}

#[test]
fn two_reviewers_at_once_one_decision() {
    on_big_stack(two_reviewers_at_once_one_decision_walk);
}

async fn two_reviewers_at_once_one_decision_walk() {
    let mut d = desk().await;
    let key = d.key().await;
    let channel = d.channel.clone();
    let message = d.notice(&channel, &key).await;
    let (first, second) = (d.reviewer_session.clone(), d.second_session.clone());

    let approve = d.press(&first, &channel, &message, &format!("dr:approve:{key}")).await;
    let reject = d.reject(&second, &message, &key, "Not ready yet").await;
    let ((_, a), (_, b)) = tokio::join!(d.act(&approve), d.act(&reject));
    let applied = [&a, &b].iter().filter(|o| o["applied"] == true).count();
    assert_eq!(applied, 1, "{a} {b}");
    let loser = if a["applied"] == true { &b } else { &a };
    assert_eq!(loser["problem"], "already_decided");

    // the owner got one message, and the listing matches the winner
    assert_eq!(d.told().await.len(), 1);
    let listed = d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable;
    assert_eq!(listed, a["applied"] == true);
    let request = d.request().await;
    let winner = if a["applied"] == true { &d.reviewer } else { &d.second };
    assert_eq!(request.acted_by.as_deref(), Some(winner.id.as_str()));
}

#[test]
fn only_the_desk_grammar_is_read() {
    use super::parse_pressed;
    assert!(parse_pressed("dr:approve:server:01ABC:1700000000000").is_some());
    assert!(parse_pressed("dr:deny:bot:01ABC:0").is_some());
    for bad in [
        "",
        "dr",
        "dr:approve",
        "dr:approve:server:01ABC",
        "dr:feature:server:01ABC:1",
        "dr:approve:channel:01ABC:1",
        "dr:approve:server::1",
        "dr:approve:server:01ABC:soon",
        "dr:approve:server:01ABC:1:extra",
        "xx:approve:server:01ABC:1",
        "dr:ask-deny:server:01ABC:1",
    ] {
        assert!(parse_pressed(bad).is_none(), "{bad}");
    }
    // a removal is read as one (who may, and that it needs the form, is decided later)
    {
        let pressed = parse_pressed("dr:remove:server:01ABC:1").unwrap();
        assert!(pressed.action == super::Action::Remove);
    }
}

#[test]
fn a_listed_server_is_taken_off_by_the_removal_role_only() {
    on_big_stack(removal_walk);
}

async fn removal_walk() {
    let mut d = desk().await;
    let channel = d.channel.clone();
    let session = d.reviewer_session.clone();
    let second_session = d.second_session.clone();
    let key = d.key().await;
    let message = d.notice(&channel, &key).await;

    // listed first
    let pressed = d.press(&session, &channel, &message, &format!("dr:approve:{key}")).await;
    let (_, outcome) = d.act(&pressed).await;
    assert_eq!(outcome["applied"], true);
    assert!(d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);

    // no removal role named: nobody can, not even a reviewer
    let pressed = d.form(&session, &message, &key, "remove", "Spam in every channel").await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::Forbidden);

    // a removal role, held by the second person only
    let review_server = d.harness.db.fetch_server(d.channel.server().unwrap()).await.unwrap();
    let removers = d.harness.new_role(&review_server, 2, None).await;
    give_role(&d.harness, &review_server, &d.second, vec![d.role.clone(), removers.id.clone()]).await;
    super::test_settings::register(FeaturesDiscoverReview {
        server: review_server.id.clone(),
        channel: channel.id().to_string(),
        channel_bots: String::new(),
        role: d.role.clone(),
        remove_role: removers.id.clone(),
        bot: d.bot.id.clone(),
    });

    // the bot finds the listing by id, by nothing else
    let (status, found) = d
        .as_bot(&d.bot.token, &format!("/discover/review/find?q={}", d.applied.id), None)
        .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(found["status"], "Approved");
    assert_eq!(found["key"], key.as_str());
    let (status, _) = d.as_bot(&d.bot.token, "/discover/review/find?q=nothing-here", None).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = d.as_bot(&d.bot.token, "/discover/review/find?q=..%2F..", None).await;
    assert_eq!(status, Status::NotFound);

    // a reviewer without the removal role is refused, and it stays listed
    let pressed = d.form(&session, &message, &key, "remove", "Spam in every channel").await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::Forbidden);
    assert!(d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);

    // a button cannot remove: only the form, with its reason
    let buttons = d.notice(&channel, &key).await;
    let pressed = d.press(&second_session, &channel, &buttons, &format!("dr:remove:{key}")).await;
    let (status, _) = d.act(&pressed).await;
    assert_eq!(status, Status::BadRequest);
    // (a reason shorter than three characters is refused by the form itself)
    assert!(d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);

    // the holder of the role removes it, with a reason
    // a removal is told by the Safety account
    let safety = revolt_database::OfficialAccount::Safety.id();
    let told_before = d.told_by(safety).await.len();
    let pressed = d.form(&second_session, &message, &key, "remove", "Spam in every channel").await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok, "{outcome}");
    assert_eq!(outcome["applied"], true);
    assert_eq!(outcome["status"], "Removed");
    assert_eq!(outcome["reason"], "Spam in every channel");
    assert!(!d.harness.db.fetch_server(&d.applied.id).await.unwrap().discoverable);
    let request = d.request().await;
    assert_eq!(
        request.status,
        DiscoverRequestStatus::Removed(Some("Spam in every channel".to_string()))
    );
    assert_eq!(request.acted_by.as_deref(), Some(d.second.id.as_str()));
    assert!(d.audit().await.contains(&(
        "discover.review.remove".to_string(),
        d.second.id.clone(),
        "Success".to_string()
    )));
    // the owner is told once, with the reason
    let told = d.told_by(safety).await;
    assert_eq!(told.len(), told_before + 1);
    assert!(told.iter().any(|message| message.contains("Spam in every channel")));

    // a second removal finds nothing listed
    let pressed = d.form(&second_session, &message, &key, "remove", "Spam in every channel").await;
    let (status, outcome) = d.act(&pressed).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(outcome["applied"], false);
    assert_eq!(outcome["problem"], "not_listed");
    assert_eq!(d.told_by(safety).await.len(), told_before + 1);
}

#[test]
fn reading_is_for_role_holders_and_the_log_follows() {
    on_big_stack(reading_walk);
}

async fn reading_walk() {
    let mut d = desk().await;
    let channel = d.channel.clone();
    let lobby = d.lobby.clone();
    let session = d.reviewer_session.clone();
    let guest_session = d.guest_session.clone();
    let key = d.key().await;
    let message = d.notice(&channel, &key).await;

    // a message of the bot outside the staff channel, with one button
    let response = d
        .harness
        .client
        .post(format!("/channels/{}/messages", lobby.id()))
        .header(Header::new("x-bot-token", d.bot.token.clone()))
        .header(ContentType::JSON)
        .body(
            json!({ "content": "Desk", "components": [{ "type": "ActionRow", "components": [
                { "type": "Button", "style": "Secondary", "label": "Listed", "custom_id": "dk:listed:all:0" }
            ]}]})
            .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let panel = response.into_json::<v0::Message>().await.unwrap().id;

    let ask = |pressed: &(String, String), what: &str, q: Option<&str>| {
        json!({ "interaction_id": pressed.0, "token": pressed.1, "what": what, "q": q })
    };

    // a member without a role reads nothing
    let pressed = d.press(&guest_session, &lobby, &panel, "dk:listed:all:0").await;
    for what in ["queue", "listed", "lookup", "stats"] {
        let (status, _) = d
            .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, what, Some(&d.applied.id))))
            .await;
        assert_eq!(status, Status::Forbidden, "{what}");
    }

    // a reviewer reads, in any channel of the review server
    let pressed = d.press(&session, &lobby, &panel, "dk:listed:all:0").await;
    // the token has to be the one the bot received
    let (status, _) = d
        .as_bot(
            &d.bot.token,
            "/discover/review/ask",
            Some(json!({ "interaction_id": pressed.0, "token": "not-it", "what": "queue" })),
        )
        .await;
    assert_ne!(status, Status::Ok);
    let (status, answer) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "queue", None)))
        .await;
    assert_eq!(status, Status::Ok, "{answer}");
    assert_eq!(answer["may_remove"], false);
    assert_eq!(answer["total"], 1);
    assert_eq!(answer["entries"][0]["id"], d.applied.id.as_str());
    let (status, _) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "everything", None)))
        .await;
    assert_eq!(status, Status::BadRequest);

    // any server can be looked up: one with a request, one without, none
    let (status, answer) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "lookup", Some(&d.applied.id))))
        .await;
    assert_eq!(status, Status::Ok, "{answer}");
    assert_eq!(answer["entry"]["status"], "Pending");
    assert_eq!(answer["entry"]["owner_id"], d.applicant.id.as_str());
    assert_eq!(answer["records"].as_array().unwrap().len(), 0);
    let (other, _) = d.harness.new_server(&d.applicant).await;
    let (status, answer) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "lookup", Some(&other.id))))
        .await;
    assert_eq!(status, Status::Ok, "{answer}");
    assert_eq!(answer["entry"]["status"], "None");
    assert_eq!(answer["entry"]["name"], other.name.as_str());
    for q in ["nothing-here", "../..", ""] {
        let (status, _) = d
            .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "lookup", Some(q))))
            .await;
        assert_eq!(status, Status::NotFound, "{q}");
    }

    // nothing is listed yet
    let (_, answer) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "listed", None)))
        .await;
    assert_eq!(answer["total"], 0);
    assert_eq!(answer["pages"], 1);

    // where the log stands now; nothing after it yet
    let (status, log) = d.as_bot(&d.bot.token, "/discover/review/log", None).await;
    assert_eq!(status, Status::Ok, "{log}");
    assert_eq!(log["records"].as_array().unwrap().len(), 0);
    let start = log["upto"].as_i64().unwrap();
    let (_, log) = d
        .as_bot(&d.bot.token, &format!("/discover/review/log?after={start}"), None)
        .await;
    assert_eq!(log["records"].as_array().unwrap().len(), 0);
    assert_eq!(log["upto"], start);

    // the reviewer approves: the log tells it once, under their name
    let approve = d.press(&session, &channel, &message, &format!("dr:approve:{key}")).await;
    let (_, outcome) = d.act(&approve).await;
    assert_eq!(outcome["applied"], true);
    let (status, log) = d
        .as_bot(&d.bot.token, &format!("/discover/review/log?after={start}"), None)
        .await;
    assert_eq!(status, Status::Ok, "{log}");
    let records = log["records"].as_array().unwrap();
    assert_eq!(records.len(), 1, "{log}");
    assert_eq!(records[0]["action"], "approve");
    assert_eq!(records[0]["actor_id"], d.reviewer.id.as_str());
    assert_eq!(records[0]["id"], d.applied.id.as_str());
    assert_eq!(records[0]["name"], d.applied.name.as_str());
    let upto = log["upto"].as_i64().unwrap();
    assert!(upto > start);
    let (_, log) = d
        .as_bot(&d.bot.token, &format!("/discover/review/log?after={upto}"), None)
        .await;
    assert_eq!(log["records"].as_array().unwrap().len(), 0);

    // it is listed now: in the list, in the counts and in its own record
    let (_, answer) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "listed", None)))
        .await;
    assert_eq!(answer["total"], 1);
    assert_eq!(answer["entries"][0]["id"], d.applied.id.as_str());
    assert_eq!(answer["entries"][0]["key"], key.as_str());
    let (_, answer) = d
        .as_bot(
            &d.bot.token,
            "/discover/review/ask",
            Some(json!({ "interaction_id": pressed.0, "token": pressed.1, "what": "listed", "kind": "bot" })),
        )
        .await;
    assert_eq!(answer["total"], 0);
    let (_, answer) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "stats", None)))
        .await;
    assert_eq!(answer["listed_servers"], 1);
    assert_eq!(answer["pending"], 0);
    assert_eq!(answer["counts"][0]["approved"], 1);
    let (_, answer) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&pressed, "lookup", Some(&d.applied.id))))
        .await;
    assert_eq!(answer["entry"]["status"], "Approved");
    assert_eq!(answer["records"][0]["action"], "approve");
    assert_eq!(answer["records"][0]["actor_id"], d.reviewer.id.as_str());

    // a submitted form is not a way to read
    let form = d.form(&session, &message, &key, "remove", "Spam in every channel").await;
    let (status, _) = d
        .as_bot(&d.bot.token, "/discover/review/ask", Some(ask(&form, "queue", None)))
        .await;
    assert_eq!(status, Status::BadRequest);
}
