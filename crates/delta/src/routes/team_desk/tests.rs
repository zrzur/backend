//! "Join the Zrzur team": the bot shows the form and the cards and hands
//! back presses; who applied, who decided and who gets a role is settled by
//! the server.

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_config::{FeaturesTeamApplications, TeamRole};
use revolt_database::{
    events::client::EventV1, Bot, Channel, Member, PartialMember, Server, TeamApplication,
    TeamApplicationStatus, TeamGrant, User,
};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, OverrideField};
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
    /// Owner of both servers
    owner: User,
    /// The server people apply in and reviewers work in
    lounge: Server,
    apply: Channel,
    review: Channel,
    /// A second official server, where some roles live
    other: Server,
    /// Roles handed out: plain in the lounge, plain in the other server,
    /// one that can manage roles, and the one behind `administrator`
    support_role: String,
    other_role: String,
    powerful_role: String,
    admin_role: String,
    /// The lounge's "Zrzur Team" role, given with `support`
    team_role: String,
    /// The other server's own staff channel and reviewer role, for `local`
    other_review: Channel,
    other_reviewer_role: String,
    local_role: String,
    reviewer: User,
    reviewer_session: String,
    second_session: String,
    /// A member of the lounge without the reviewer role
    guest: User,
    guest_session: String,
    applicant: User,
    applicant_session: String,
    /// The message in the apply channel with the buttons
    panel: String,
    /// Session token -> user id, to listen for a person's private messages
    users: Vec<(String, String)>,
    /// Id and token of the press the last private question answered
    last_question: Option<(String, String)>,
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

fn team_role(key: &str, server: &Server, role: &str) -> TeamRole {
    TeamRole {
        key: key.to_string(),
        name: format!("Role {key}"),
        description: "What the role does".to_string(),
        server: server.id.clone(),
        role: role.to_string(),
        team_role: String::new(),
        channel_review: String::new(),
        reviewer_role: String::new(),
        adult: true,
        manual: false,
        open: true,
        rules: format!("Rules of {key}"),
        questions: vec![],
        work_channels: vec![],
    }
}

async fn desk() -> Desk {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, reviewer_session, reviewer) = harness.new_user().await;
    let (_, second_session, second) = harness.new_user().await;
    let (_, guest_session, guest) = harness.new_user().await;
    let (_, applicant_session, applicant) = harness.new_user().await;

    let (lounge, channels) = harness.new_server(&owner).await;
    let apply = channels.into_iter().next().unwrap();
    let review = harness.new_channel(&lounge).await;
    let (other, _) = harness.new_server(&owner).await;

    let reviewer_role = harness.new_role(&lounge, 1, None).await;
    let support_role = harness.new_role(&lounge, 5, None).await;
    let admin_role = harness.new_role(&lounge, 2, None).await;
    let powerful_role = harness
        .new_role(
            &lounge,
            3,
            Some(OverrideField {
                a: ChannelPermission::ManageRole as i64 | ChannelPermission::KickMembers as i64,
                d: 0,
            }),
        )
        .await;
    let other_role = harness.new_role(&other, 5, None).await;
    let zrzur_team = harness.new_role(&lounge, 6, None).await;
    let local_role = harness.new_role(&other, 6, None).await;
    let other_reviewer_role = harness.new_role(&other, 1, None).await;
    let other_review = harness.new_channel(&other).await;

    let (bot, bot_user) = harness.new_bot(&owner).await;
    for member in [&reviewer, &second, &guest, &applicant, &bot_user] {
        Member::create(&harness.db, &lounge, member, None).await.unwrap();
    }
    // the bot also shows cards in the other server's staff channel
    Member::create(&harness.db, &other, &bot_user, None).await.unwrap();
    give_role(&harness, &lounge, &reviewer, vec![reviewer_role.id.clone()]).await;
    give_role(&harness, &lounge, &second, vec![reviewer_role.id.clone()]).await;
    revolt_presence::create_session(&bot.id, 0).await;

    let mut closed = team_role("closed", &lounge, &support_role.id);
    closed.open = false;
    let mut teens = team_role("designer", &lounge, &support_role.id);
    teens.adult = false;
    let mut support = team_role("support", &lounge, &support_role.id);
    support.team_role = zrzur_team.id.clone();
    // a role of the other server, reviewed there by that server's own people
    let mut local = team_role("local", &other, &local_role.id);
    local.channel_review = other_review.id().to_string();
    local.reviewer_role = other_reviewer_role.id.clone();
    super::test_settings::register(FeaturesTeamApplications {
        server: lounge.id.clone(),
        channel_apply: apply.id().to_string(),
        channel_review: review.id().to_string(),
        reviewer_role: reviewer_role.id.clone(),
        bot: bot.id.clone(),
        roles: vec![
            support,
            team_role("elsewhere", &other, &other_role.id),
            team_role("powerful", &lounge, &powerful_role.id),
            // a real role is named on purpose: it must still never be given
            team_role("administrator", &lounge, &admin_role.id),
            teens,
            local,
            closed,
            // not set up: no server
            TeamRole {
                key: "unready".to_string(),
                name: "Unready".to_string(),
                ..Default::default()
            },
        ],
        ..Default::default()
    });

    let second_id = second.id.clone();
    let bot_events = PubSubTestHelper::new(&format!("{}!", bot.id)).await;
    let mut desk = Desk {
        harness,
        bot,
        owner,
        lounge,
        apply,
        review,
        other,
        support_role: support_role.id,
        other_role: other_role.id,
        powerful_role: powerful_role.id,
        admin_role: admin_role.id,
        team_role: zrzur_team.id,
        other_review,
        other_reviewer_role: other_reviewer_role.id,
        local_role: local_role.id,
        reviewer,
        reviewer_session: reviewer_session.token,
        second_session: second_session.token,
        guest,
        guest_session: guest_session.token,
        applicant,
        applicant_session: applicant_session.token,
        panel: String::new(),
        users: Vec::new(),
        last_question: None,
        bot_events,
    };
    for (session, user) in [
        (desk.reviewer_session.clone(), desk.reviewer.id.clone()),
        (desk.second_session.clone(), second_id),
        (desk.guest_session.clone(), desk.guest.id.clone()),
        (desk.applicant_session.clone(), desk.applicant.id.clone()),
    ] {
        desk.users.push((session, user));
    }
    let channel = desk.apply.clone();
    desk.panel = desk.message(&channel, &["ta:start", "ta:mine"]).await;
    desk
}

impl Desk {
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

    async fn hand(&self, path: &str, pressed: &(String, String)) -> (Status, Value) {
        self.as_bot(
            &self.bot.token,
            &format!("/team/desk/{path}"),
            Some(json!({ "interaction_id": pressed.0, "token": pressed.1 })),
        )
        .await
    }

    async fn queue(&self) -> Vec<Value> {
        let (status, body) = self.as_bot(&self.bot.token, "/team/desk/queue", None).await;
        assert_eq!(status, Status::Ok);
        body.as_array().cloned().unwrap()
    }

    /// The bot posts a message with one plain button per id
    async fn message(&self, channel: &Channel, ids: &[&str]) -> String {
        let buttons: Vec<Value> = ids
            .iter()
            .map(|id| json!({ "type": "Button", "style": "Secondary", "label": "x", "custom_id": id }))
            .collect();
        let response = self
            .harness
            .client
            .post(format!("/channels/{}/messages", channel.id()))
            .header(Header::new("x-bot-token", self.bot.token.clone()))
            .header(ContentType::JSON)
            .body(
                json!({ "content": "Zrzur Team", "components": [{ "type": "ActionRow", "components": buttons }] })
                    .to_string(),
            )
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

    /// The bot answers `parent` with a form and the person sends it
    async fn form(
        &mut self,
        session: &str,
        channel: &Channel,
        parent: &(String, String),
        modal: Value,
        fields: Value,
    ) -> (String, String) {
        let custom_id = modal["custom_id"].as_str().unwrap().to_string();
        let status = self
            .harness
            .client
            .post(format!("/interactions/{}/{}/callback", parent.0, parent.1))
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
                    "parent_id": parent.0,
                    "custom_id": custom_id,
                    "fields": fields,
                    "nonce": ulid::Ulid::new().to_string(),
                }),
            )
            .await;
        assert_eq!(status, Status::Ok, "{accepted}");
        let id = accepted["_id"].as_str().unwrap().to_string();
        (id.clone(), self.token_of(&id).await)
    }

    /// A person presses Apply, gets the form of `role` and sends it; gives
    /// what the server answered to the hand-back
    async fn apply_with(&mut self, session: &str, role: &str, why: &str) -> (Status, Value) {
        let channel = self.apply.clone();
        let panel = self.panel.clone();
        let parent = self.press(session, &channel, &panel, "ta:start").await;
        let text = |id: &str, style: &str| {
            json!({ "type": "Field", "label": id, "component": {
                "type": "TextInput", "custom_id": id, "style": style, "label": id, "required": true
            }})
        };
        let modal = json!({
            "custom_id": format!("ta:apply:{role}"),
            "title": "Join the team",
            "components": [
                text("languages", "Short"),
                { "type": "Field", "label": "Time", "component": {
                    "type": "RadioGroup", "custom_id": "hours", "required": true,
                    "options": [
                        { "label": "Up to 3 hours a week", "value": "under-3" },
                        { "label": "3 to 7 hours a week", "value": "3-7" }
                    ]
                }},
                text("experience", "Paragraph"),
                text("why", "Paragraph"),
            ]
        });
        let fields = json!([
            { "custom_id": "languages", "value": "Arabic, English" },
            { "custom_id": "hours", "value": "3-7" },
            { "custom_id": "experience", "value": "Two years of moderating a large server." },
            { "custom_id": "why", "value": why },
        ]);
        let sent = self.form(session, &channel, &parent, modal, fields).await;
        self.hand("apply", &sent).await
    }

    async fn apply_as(&mut self, session: &str, role: &str) -> (Status, Value) {
        self.apply_with(session, role, "I like helping people find their way.").await
    }

    /// A card in the staff channel with the review buttons of `id`
    async fn card(&self, id: &str) -> String {
        let channel = self.review.clone();
        self.card_in(&channel, id).await
    }

    async fn card_in(&self, channel: &Channel, id: &str) -> String {
        self.message(
            channel,
            &[
                &format!("ta:interview:{id}"),
                &format!("ta:accept:{id}"),
                &format!("ta:reject:{id}"),
                &format!("ta:ask-accept:{id}"),
                &format!("ta:ask-reject:{id}"),
            ],
        )
        .await
    }

    async fn review_press(&mut self, session: &str, card: &str, action: &str, id: &str) -> (Status, Value) {
        let channel = self.review.clone();
        self.review_press_in(&channel, session, card, action, id).await
    }

    fn know(&mut self, session: &str, user: &User) {
        self.users.push((session.to_string(), user.id.clone()));
    }

    /// The words the server wants on the question before an acceptance
    async fn confirm_text(&self, id: &str) -> String {
        self.queue()
            .await
            .iter()
            .find(|entry| entry["id"] == id)
            .and_then(|entry| entry["confirm"].as_str().map(str::to_string))
            .unwrap_or_else(|| "Accept?".to_string())
    }

    /// A reviewer presses Accept on the card, the bot asks in private with
    /// `content` and one button `label`, and the reviewer presses it; gives
    /// what the bot received for that last press
    async fn confirm_press(
        &mut self,
        channel: &Channel,
        session: &str,
        card: &str,
        id: &str,
        content: &str,
        label: &str,
    ) -> (String, String) {
        let user_id = self
            .users
            .iter()
            .find(|(known, _)| known == session)
            .map(|(_, user)| user.clone())
            .expect("a known session");
        let mut user_events = PubSubTestHelper::new(&format!("{user_id}!")).await;
        let channel = channel.clone();
        let asked = self.press(session, &channel, card, &format!("ta:ask-accept:{id}")).await;
        let status = self
            .harness
            .client
            .post(format!("/interactions/{}/{}/callback", asked.0, asked.1))
            .header(ContentType::JSON)
            .body(
                json!({ "type": "Message", "data": {
                    "content": content, "ephemeral": true,
                    "components": [{ "type": "ActionRow", "components": [
                        { "type": "Button", "style": "Success", "label": label, "custom_id": format!("ta:accept:{id}") }
                    ]}]
                }})
                .to_string(),
            )
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::NoContent);
        let question = match user_events
            .wait_for_event(|e| matches!(e, EventV1::EphemeralMessageCreate { .. }))
            .await
        {
            EventV1::EphemeralMessageCreate { message } => message,
            _ => unreachable!(),
        };
        self.last_question = Some((asked.0.clone(), asked.1.clone()));
        self.press(session, &channel, &question.id, &format!("ta:accept:{id}")).await
    }

    async fn review_press_in(&mut self, channel: &Channel, session: &str, card: &str, action: &str, id: &str) -> (Status, Value) {
        if action == "accept" {
            // an acceptance goes through the server's own question
            let content = self.confirm_text(id).await;
            let pressed = self
                .confirm_press(channel, session, card, id, &content, "Yes, accept")
                .await;
            return self.hand("act", &pressed).await;
        }
        let channel = channel.clone();
        let pressed = self.press(session, &channel, card, &format!("ta:{action}:{id}")).await;
        self.hand("act", &pressed).await
    }

    async fn reject(&mut self, session: &str, card: &str, id: &str, reason: &str) -> (Status, Value) {
        let channel = self.review.clone();
        let parent = self.press(session, &channel, card, &format!("ta:ask-reject:{id}")).await;
        let modal = json!({
            "custom_id": format!("ta:reject:{id}"),
            "title": "Reject",
            "components": [{ "type": "ActionRow", "components": [{
                "type": "TextInput", "custom_id": "reason", "style": "Paragraph",
                "label": "Reason", "min_length": 1, "max_length": 512
            }]}]
        });
        let sent = self
            .form(session, &channel, &parent, modal, json!([{ "custom_id": "reason", "value": reason }]))
            .await;
        self.hand("act", &sent).await
    }

    async fn stored(&self, id: &str) -> TeamApplication {
        self.harness.db.fetch_team_application(id).await.unwrap()
    }

    async fn roles_of(&self, server: &Server, user: &User) -> Vec<String> {
        self.harness.db.fetch_member(&server.id, &user.id).await.unwrap().roles
    }

    /// What the official Zrzur account wrote to a person
    async fn told(&self, user: &User) -> Vec<String> {
        let Ok(channel) = self
            .harness
            .db
            .find_direct_message_channel(revolt_database::OfficialAccount::Zrzur.id(), &user.id)
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

    /// (action, actor, outcome) of the audit records about one person
    async fn audit(&self, about: &User) -> Vec<(String, String, String)> {
        self.harness
            .db
            .fetch_platform_audit(revolt_database::PlatformAuditQuery {
                limit: 500,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.target_id.as_deref() == Some(about.id.as_str()))
            .map(|e| (e.action, e.actor_id, format!("{:?}", e.outcome)))
            .collect()
    }
}

fn id_of(outcome: &Value) -> String {
    outcome["application"]["id"].as_str().unwrap().to_string()
}

#[test]
fn the_routes_exist_only_for_the_desk_bot() {
    on_big_stack(|| async {
        let d = desk().await;
        // a person
        let response = d
            .harness
            .client
            .get("/team/desk/queue")
            .header(Header::new("x-session-token", d.reviewer_session.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);
        // another bot
        let (other_bot, _) = d.harness.new_bot(&d.owner).await;
        for path in ["/team/desk/queue", "/team/desk/config", "/team/desk/applications/x"] {
            let (status, _) = d.as_bot(&other_bot.token, path, None).await;
            assert_eq!(status, Status::NotFound, "{path}");
        }
        for path in ["mine", "apply", "withdraw", "act"] {
            let (status, _) = d
                .as_bot(
                    &other_bot.token,
                    &format!("/team/desk/{path}"),
                    Some(json!({ "interaction_id": "x", "token": "y" })),
                )
                .await;
            assert_eq!(status, Status::NotFound, "{path}");
        }

        // the desk's bot: only roles that are open and set up, in order
        let (status, config) = d.as_bot(&d.bot.token, "/team/desk/config", None).await;
        assert_eq!(status, Status::Ok);
        let keys: Vec<&str> = config["roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|role| role["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["support", "elsewhere", "powerful", "administrator", "designer", "local"]);
        assert_eq!(
            config["review_channels"],
            json!([d.review.id(), d.other_review.id()])
        );
        assert!(config.get("compensation").is_none());
        // everything a person reads exists in the app's seven languages
        assert_eq!(config["languages"], json!(["en", "ar", "de", "fr", "es", "ru", "zh-Hans"]));
        let texts = &config["roles"][0]["texts"];
        assert_eq!(texts.as_object().unwrap().len(), 7);
        // the test settings name the role in English; the other languages use the kind's text
        assert_eq!(texts["en"]["name"], "Role support");
        assert_eq!(texts["ar"]["name"], "الدعم");
        assert_eq!(texts["fr"]["questions"][1]["label"], "Pourquoi veux-tu ce rôle ?");
        assert_eq!(texts["zh-Hans"]["questions"][0]["id"], "experience");
        assert_eq!(config["cooldown_days"], 30);
        assert_eq!(config["roles"][3]["manual"], true);
        assert_eq!(config["roles"][0]["manual"], false);
        assert_eq!(config["roles"][0]["questions"][0]["id"], "experience");
        assert_eq!(config["roles"][0]["server_name"], d.lounge.name.as_str());
    });
}

#[test]
fn apply_interview_accept_gives_the_role() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();
        let guest_session = d.guest_session.clone();

        // before applying: may apply, for every open role
        let channel = d.apply.clone();
        let panel = d.panel.clone();
        let pressed = d.press(&applicant_session, &channel, &panel, "ta:start").await;
        let (status, mine) = d.hand("mine", &pressed).await;
        assert_eq!(status, Status::Ok, "{mine}");
        assert_eq!(mine["state"], "none");
        assert_eq!(mine["roles"].as_array().unwrap().len(), 6);

        let (status, outcome) = d.apply_as(&applicant_session, "support").await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        let id = id_of(&outcome);

        // the person got a receipt that says what is kept and that it is volunteer
        let told = d.told(&d.applicant).await;
        assert!(told.iter().any(|m| m.contains("Role support") && m.contains("volunteer")), "{told:?}");

        // the card: who, since when, where, and the answers
        let queue = d.queue().await;
        assert_eq!(queue.len(), 1);
        let entry = &queue[0];
        assert_eq!(entry["id"], id.as_str());
        assert_eq!(entry["status"], "Open");
        assert_eq!(entry["user_id"], d.applicant.id.as_str());
        assert_eq!(entry["adult"], true);
        assert_eq!(entry["servers"], json!([d.lounge.name]));
        assert_eq!(entry["answers"].as_array().unwrap().len(), 4);
        assert_eq!(entry["answers"][1]["value"], "3 to 7 hours a week");
        assert!(entry["account_created_at"].as_i64().unwrap() > 0);
        assert_eq!(entry["channel_review"], d.review.id());
        assert_eq!(entry["restrict_roles"].as_array().unwrap().len(), 1);

        let card = d.card(&id).await;

        // a member without the reviewer role decides nothing
        let (status, _) = d.review_press(&guest_session, &card, "accept", &id).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(d.stored(&id).await.status, TeamApplicationStatus::Open);

        // interview: the person is told who will write, and the conversation exists
        let (status, outcome) = d.review_press(&reviewer_session, &card, "interview", &id).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        assert_eq!(outcome["status"], "Interview");
        assert!(outcome["dm_channel"].is_string());
        let reviewer_name = format!("{}#{}", d.reviewer.username, d.reviewer.discriminator);
        let told = d.told(&d.applicant).await;
        assert!(told.iter().any(|m| m.contains(&reviewer_name) && m.contains("never asks")), "{told:?}");
        assert_eq!(d.queue().await[0]["interviewer_name"], reviewer_name.as_str());
        // a second press on Interview changes nothing
        let (_, outcome) = d.review_press(&reviewer_session, &card, "interview", &id).await;
        assert_eq!(outcome["problem"], "already_interview");

        // accept: the server gives the role; the bot gave nothing
        assert!(d.roles_of(&d.lounge, &d.applicant).await.is_empty());
        let (status, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        assert_eq!(outcome["grant"], "granted");
        // the role they applied for, and the server's Zrzur Team role with it
        assert_eq!(
            d.roles_of(&d.lounge, &d.applicant).await,
            vec![d.support_role.clone(), d.team_role.clone()]
        );
        let stored = d.stored(&id).await;
        assert_eq!(stored.status, TeamApplicationStatus::Accepted);
        assert_eq!(stored.grant, Some(TeamGrant::Granted));
        assert_eq!(stored.acted_by.as_deref(), Some(d.reviewer.id.as_str()));
        assert!(stored.purge_at.is_some());
        let told = d.told(&d.applicant).await;
        assert!(told.iter().any(|m| m.contains("Rules of support")), "{told:?}");

        // decided once: a second decision is refused
        let (_, outcome) = d.reject(&reviewer_session, &card, &id, "Changed my mind").await;
        assert_eq!(outcome["applied"], false);
        assert_eq!(outcome["problem"], "already_decided");
        assert_eq!(d.stored(&id).await.status, TeamApplicationStatus::Accepted);

        // gone from the queue; the state names who decided, not the answers
        assert!(d.queue().await.is_empty());
        let (_, state) = d
            .as_bot(&d.bot.token, &format!("/team/desk/applications/{id}"), None)
            .await;
        assert_eq!(state["status"], "Accepted");
        assert_eq!(state["acted_by_name"], reviewer_name.as_str());
        assert!(state.get("answers").is_none());

        // the reviewer, not the bot, is in the audit log
        let audit = d.audit(&d.applicant).await;
        for action in ["team.application.interview", "team.application.accept"] {
            assert!(
                audit.contains(&(action.to_string(), d.reviewer.id.clone(), "Success".to_string())),
                "{audit:?}"
            );
        }
        assert!(audit.iter().all(|(_, actor, _)| actor != &d.bot.id));

        // platform powers were never touched
        let person = d.harness.db.fetch_user(&d.applicant.id).await.unwrap();
        assert!(!person.privileged);
    });
}

#[test]
fn reject_needs_a_reason_and_starts_the_cooldown() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();

        let (_, outcome) = d.apply_as(&applicant_session, "support").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;

        // the reject button alone carries no reason: refused
        let (status, _) = d.review_press(&reviewer_session, &card, "reject", &id).await;
        assert_eq!(status, Status::BadRequest);
        // a reason too short
        let (status, _) = d.reject(&reviewer_session, &card, &id, "no").await;
        assert_ne!(status, Status::Ok);
        assert_eq!(d.stored(&id).await.status, TeamApplicationStatus::Open);

        let reason = "We need someone who is around in the evenings.";
        let (status, outcome) = d.reject(&reviewer_session, &card, &id, reason).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        assert_eq!(outcome["status"], "Rejected");
        let stored = d.stored(&id).await;
        assert_eq!(stored.status, TeamApplicationStatus::Rejected);
        assert_eq!(stored.reason.as_deref(), Some(reason));
        // kept for the cooldown, then deleted
        let kept_for = stored.purge_at.unwrap() - stored.acted_at.unwrap();
        assert_eq!(kept_for, 30 * 24 * 60 * 60 * 1000);
        assert!(d.roles_of(&d.lounge, &d.applicant).await.is_empty());

        // told politely, with the reason and when to come back
        let told = d.told(&d.applicant).await;
        assert!(told.iter().any(|m| m.contains(reason) && m.contains("30 days")), "{told:?}");
        assert!(d
            .audit(&d.applicant)
            .await
            .contains(&("team.application.reject".to_string(), d.reviewer.id.clone(), "Success".to_string())));

        // the cooldown: the person is told to wait, and a new form is refused
        let channel = d.apply.clone();
        let panel = d.panel.clone();
        let pressed = d.press(&applicant_session, &channel, &panel, "ta:mine").await;
        let (_, mine) = d.hand("mine", &pressed).await;
        assert_eq!(mine["state"], "wait");
        assert!(mine["wait_until"].as_i64().unwrap() > stored.acted_at.unwrap());
        let (status, outcome) = d.apply_as(&applicant_session, "designer").await;
        assert_eq!(status, Status::Ok);
        assert_eq!(outcome["applied"], false);
        assert_eq!(outcome["problem"], "wait");
        assert_eq!(d.harness.db.fetch_team_applications_of(&d.applicant.id).await.unwrap().len(), 1);

        // once its time is up the record is deleted, and the person may apply again
        let mut old = stored.clone();
        old.acted_at = Some(1);
        old.purge_at = Some(2);
        assert!(d
            .harness
            .db
            .replace_team_application_if(&old, TeamApplicationStatus::Rejected)
            .await
            .unwrap());
        let (_, state) = d
            .as_bot(&d.bot.token, &format!("/team/desk/applications/{id}"), None)
            .await;
        assert_eq!(state["status"], "Gone");
        assert!(d.harness.db.fetch_team_application(&id).await.is_err());
        let (_, outcome) = d.apply_as(&applicant_session, "designer").await;
        assert_eq!(outcome["applied"], true, "{outcome}");
    });
}

#[test]
fn one_open_application_and_the_person_can_withdraw() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let guest_session = d.guest_session.clone();
        let reviewer_session = d.reviewer_session.clone();

        let (_, outcome) = d.apply_as(&applicant_session, "support").await;
        let id = id_of(&outcome);

        // a second application while one is open
        let (status, outcome) = d.apply_as(&applicant_session, "designer").await;
        assert_eq!(status, Status::Ok);
        assert_eq!(outcome["problem"], "already_open");
        assert_eq!(outcome["application"]["id"], id.as_str());
        assert_eq!(d.queue().await.len(), 1);

        let channel = d.apply.clone();
        let panel = d.panel.clone();
        let pressed = d.press(&applicant_session, &channel, &panel, "ta:mine").await;
        let (_, mine) = d.hand("mine", &pressed).await;
        assert_eq!(mine["state"], "open");
        assert_eq!(mine["application"]["role"], "support");

        // nobody else can withdraw it, and the answer does not say it exists
        let withdraw = d.message(&channel, &[&format!("ta:withdraw:{id}")]).await;
        let pressed = d.press(&guest_session, &channel, &withdraw, &format!("ta:withdraw:{id}")).await;
        let (status, _) = d.hand("withdraw", &pressed).await;
        assert_eq!(status, Status::NotFound);
        assert_eq!(d.stored(&id).await.status, TeamApplicationStatus::Open);

        // the person can: the answers are erased at once
        let pressed = d.press(&applicant_session, &channel, &withdraw, &format!("ta:withdraw:{id}")).await;
        let (status, outcome) = d.hand("withdraw", &pressed).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        let stored = d.stored(&id).await;
        assert_eq!(stored.status, TeamApplicationStatus::Withdrawn);
        assert!(stored.answers.is_empty());
        assert!(stored.purge_at.is_some());
        assert!(d.queue().await.is_empty());
        let told = d.told(&d.applicant).await;
        assert!(told.iter().any(|m| m.contains("erased")), "{told:?}");

        // a reviewer who still sees the old card decides nothing
        let card = d.card(&id).await;
        let (_, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(outcome["applied"], false);
        assert_eq!(outcome["problem"], "withdrawn");
        assert!(d.roles_of(&d.lounge, &d.applicant).await.is_empty());

        // withdrawing twice
        let pressed = d.press(&applicant_session, &channel, &withdraw, &format!("ta:withdraw:{id}")).await;
        let (_, outcome) = d.hand("withdraw", &pressed).await;
        assert_eq!(outcome["problem"], "not_open");
    });
}

#[test]
fn administrator_and_powerful_roles_are_never_given() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let guest_session = d.guest_session.clone();
        let reviewer_session = d.reviewer_session.clone();

        // administrator: accepted, recorded, the owner told, nothing given,
        // although the settings name a real role for it
        let (_, outcome) = d.apply_as(&applicant_session, "administrator").await;
        let id = id_of(&outcome);
        assert_eq!(d.queue().await[0]["manual"], true);
        let card = d.card(&id).await;
        let (status, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        assert_eq!(outcome["grant"], "manual");
        assert!(d.roles_of(&d.lounge, &d.applicant).await.is_empty());
        assert_eq!(d.stored(&id).await.grant, Some(TeamGrant::Manual));
        let person = d.harness.db.fetch_user(&d.applicant.id).await.unwrap();
        assert!(!person.privileged);
        assert!(person.badges.unwrap_or_default() == d.applicant.badges.unwrap_or_default());
        let told_owner = d.told(&d.owner).await;
        assert!(told_owner.iter().any(|m| m.contains("by hand")), "{told_owner:?}");
        let told = d.told(&d.applicant).await;
        assert!(told.iter().any(|m| m.contains("owner of Zrzur")), "{told:?}");
        // looking again later gives nothing either
        let (status, settled) = d.as_bot(&d.bot.token, "/team/desk/settle", Some(json!({}))).await;
        assert_eq!(status, Status::Ok);
        assert_eq!(settled, json!([]));
        assert!(d.roles_of(&d.lounge, &d.applicant).await.is_empty());

        // a role that can manage roles is refused even when the settings
        // do not call it manual
        let (_, outcome) = d.apply_as(&guest_session, "powerful").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;
        let (_, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(outcome["applied"], true);
        assert_eq!(outcome["grant"], "manual");
        assert!(d.roles_of(&d.lounge, &d.guest).await.is_empty());
        let _ = (&d.powerful_role, &d.admin_role);
    });
}

#[test]
fn a_role_in_another_server_waits_for_the_join() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();

        let (_, outcome) = d.apply_as(&applicant_session, "elsewhere").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;
        let (_, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(outcome["applied"], true);
        assert_eq!(outcome["grant"], "pending_join");
        assert_eq!(outcome["server_name"], d.other.name.as_str());
        let told = d.told(&d.applicant).await;
        assert!(told.iter().any(|m| m.contains("Join")), "{told:?}");

        // not a member yet: nothing to give
        let (_, settled) = d.as_bot(&d.bot.token, "/team/desk/settle", Some(json!({}))).await;
        assert_eq!(settled, json!([]));

        Member::create(&d.harness.db, &d.other, &d.applicant, None).await.unwrap();
        let (status, settled) = d.as_bot(&d.bot.token, "/team/desk/settle", Some(json!({}))).await;
        assert_eq!(status, Status::Ok);
        assert_eq!(settled.as_array().unwrap().len(), 1);
        assert_eq!(settled[0]["id"], id.as_str());
        assert_eq!(d.roles_of(&d.other, &d.applicant).await, vec![d.other_role.clone()]);
        assert_eq!(d.stored(&id).await.grant, Some(TeamGrant::Granted));
        // once
        let (_, settled) = d.as_bot(&d.bot.token, "/team/desk/settle", Some(json!({}))).await;
        assert_eq!(settled, json!([]));
    });
}

#[test]
fn age_own_application_and_forged_presses() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let reviewer_session = d.reviewer_session.clone();
        let second_session = d.second_session.clone();
        let applicant_session = d.applicant_session.clone();

        // an account whose stated age is 17: adult roles are closed to it
        let (mut young_account, young_session, young) = d.harness.new_user().await;
        let year: i32 = iso8601_timestamp::Timestamp::now_utc().format().to_string()[0..4]
            .parse()
            .unwrap();
        young_account.age.as_mut().unwrap().birth_month = format!("{}-01", year - 17);
        d.harness.db.save_account(&young_account).await.unwrap();
        Member::create(&d.harness.db, &d.lounge, &young, None).await.unwrap();
        let channel = d.apply.clone();
        let panel = d.panel.clone();
        let pressed = d.press(&young_session.token, &channel, &panel, "ta:start").await;
        let (_, mine) = d.hand("mine", &pressed).await;
        assert_eq!(mine["roles"], json!(["designer"]));
        assert_eq!(mine["adult_only"].as_array().unwrap().len(), 5);
        let (status, outcome) = d.apply_as(&young_session.token, "support").await;
        assert_eq!(status, Status::Ok);
        assert_eq!(outcome["problem"], "adult_only");
        assert!(d.harness.db.fetch_team_applications_of(&young.id).await.unwrap().is_empty());
        // the role open to them works, and only "not 18+" is kept
        let (_, outcome) = d.apply_as(&young_session.token, "designer").await;
        assert_eq!(outcome["applied"], true, "{outcome}");
        assert!(!d.stored(&id_of(&outcome)).await.adult);

        // closed and unknown roles
        for role in ["closed", "unready", "nothing"] {
            let (_, outcome) = d.apply_as(&applicant_session, role).await;
            assert_eq!(outcome["problem"], "role_closed", "{role}");
        }
        // an answer too short is refused before anything is stored
        let (status, _) = d.apply_with(&applicant_session, "support", "short").await;
        assert_ne!(status, Status::Ok);
        assert!(d.harness.db.fetch_team_applications_of(&d.applicant.id).await.unwrap().is_empty());

        // a reviewer cannot decide their own application; a colleague can
        let (_, outcome) = d.apply_as(&reviewer_session, "support").await;
        let own = id_of(&outcome);
        let card = d.card(&own).await;
        let (_, outcome) = d.review_press(&reviewer_session, &card, "accept", &own).await;
        assert_eq!(outcome["applied"], false);
        assert_eq!(outcome["problem"], "own_application");

        // a review button pressed outside the staff channel counts for nothing
        let outside = d.message(&channel, &[&format!("ta:accept:{own}")]).await;
        let pressed = d.press(&second_session, &channel, &outside, &format!("ta:accept:{own}")).await;
        let (status, _) = d.hand("act", &pressed).await;
        assert_eq!(status, Status::Forbidden);

        // the bot alone decides nothing: no press, no act
        let (status, _) = d
            .as_bot(
                &d.bot.token,
                "/team/desk/act",
                Some(json!({ "interaction_id": own, "token": "made-up" })),
            )
            .await;
        assert_eq!(status, Status::NotFound);
        // a genuine press with a wrong token
        let review = d.review.clone();
        let content = d.confirm_text(&own).await;
        let pressed = d
            .confirm_press(&review, &second_session, &card, &own, &content, "Yes, accept")
            .await;
        let (status, _) = d.hand("act", &(pressed.0.clone(), "wrong".to_string())).await;
        assert_eq!(status, Status::NotFound);
        assert_eq!(d.stored(&own).await.status, TeamApplicationStatus::Open);
        // a reviewer who lost the role since the card was drawn
        give_role(&d.harness, &d.lounge, &d.reviewer.clone(), vec![]).await;
        let other = d.apply_as(&applicant_session, "support").await.1;
        let other_id = id_of(&other);
        let other_card = d.card(&other_id).await;
        let (status, _) = d.review_press(&reviewer_session, &other_card, "accept", &other_id).await;
        assert_eq!(status, Status::Forbidden);
        // the colleague's genuine press works
        let (status, outcome) = d.hand("act", &pressed).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        // the same press handed back again decides nothing new
        let (_, outcome) = d.hand("act", &pressed).await;
        assert_eq!(outcome["applied"], false);
    });
}

#[test]
fn applications_go_with_their_time_and_with_the_account() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let guest_session = d.guest_session.clone();
        let (_, outcome) = d.apply_as(&applicant_session, "support").await;
        let id = id_of(&outcome);
        let (_, outcome) = d.apply_as(&guest_session, "support").await;
        let guest_id = id_of(&outcome);

        // an open application has no end date
        assert_eq!(d.harness.db.delete_team_applications_due(i64::MAX).await.unwrap(), 0);

        // the export holds the person's own application, without reviewer ids
        let documents = d
            .harness
            .db
            .export_documents(&d.applicant.id, revolt_database::ExportSource::TeamApplications)
            .await
            .unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0]["_id"], id.as_str());

        // deleting the account deletes its applications, and only its own
        let mut person = d.harness.db.fetch_user(&d.applicant.id).await.unwrap();
        person.mark_deleted(&d.harness.db).await.unwrap();
        assert!(d.harness.db.fetch_team_application(&id).await.is_err());
        assert!(d.harness.db.fetch_team_application(&guest_id).await.is_ok());
        assert_eq!(d.queue().await.len(), 1);
    });
}

#[test]
fn a_server_reviews_its_own_applications() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();
        let guest_session = d.guest_session.clone();
        let other_review = d.other_review.clone();

        // someone of the other server's own team: holds its reviewer role
        // there and nothing in the lounge
        let (_, local_session, local) = d.harness.new_user().await;
        Member::create(&d.harness.db, &d.other, &local, None).await.unwrap();
        give_role(&d.harness, &d.other, &local, vec![d.other_reviewer_role.clone()]).await;
        d.know(&local_session.token, &local);
        // the common reviewer and a plain member can see that channel too
        for person in [d.reviewer.clone(), d.guest.clone(), d.applicant.clone()] {
            Member::create(&d.harness.db, &d.other, &person, None).await.unwrap();
        }

        let (_, outcome) = d.apply_as(&applicant_session, "local").await;
        assert_eq!(outcome["applied"], true, "{outcome}");
        let id = id_of(&outcome);
        // the card belongs in that server's staff channel; its buttons carry
        // no role limit there, the server alone decides
        let entry = d.queue().await.into_iter().find(|e| e["id"] == id.as_str()).unwrap();
        assert_eq!(entry["channel_review"], other_review.id());
        assert_eq!(entry["restrict_roles"], json!([]));

        // decided in the common channel: refused, even for a common reviewer
        let wrong = d.card(&id).await;
        let (status, _) = d.review_press(&reviewer_session, &wrong, "accept", &id).await;
        assert_eq!(status, Status::Forbidden);

        let card = d.card_in(&other_review, &id).await;
        // a member of that server without its reviewer role
        let (status, _) = d.review_press_in(&other_review, &guest_session, &card, "accept", &id).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(d.stored(&id).await.status, TeamApplicationStatus::Open);

        // that server's reviewer cannot touch an application of another role
        let (_, outcome) = d.apply_as(&guest_session, "support").await;
        let support_id = id_of(&outcome);
        let support_card = d.card(&support_id).await;
        let lounge_review = d.review.clone();
        Member::create(&d.harness.db, &d.lounge, &local, None).await.unwrap();
        let (status, _) = d
            .review_press_in(&lounge_review, &local_session.token, &support_card, "accept", &support_id)
            .await;
        assert_eq!(status, Status::Forbidden);
        let stray = d.card_in(&other_review, &support_id).await;
        let (status, _) = d
            .review_press_in(&other_review, &local_session.token, &stray, "accept", &support_id)
            .await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(d.stored(&support_id).await.status, TeamApplicationStatus::Open);

        // that server's reviewer decides its own application, under their name
        let (status, outcome) = d
            .review_press_in(&other_review, &local_session.token, &card, "interview", &id)
            .await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        assert!(d
            .audit(&d.applicant)
            .await
            .contains(&("team.application.interview".to_string(), local.id.clone(), "Success".to_string())));

        // a common reviewer reviews everything, there too
        let (status, outcome) = d.review_press_in(&other_review, &reviewer_session, &card, "accept", &id).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["grant"], "granted");
        assert_eq!(d.roles_of(&d.other, &d.applicant).await, vec![d.local_role.clone()]);
    });
}

#[test]
fn a_team_role_that_can_manage_the_server_is_left_out() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();
        // the settings name the powerful role as the "Zrzur Team" role
        let mut settings = super::test_settings::get(&d.bot.id).unwrap();
        settings.roles[0].team_role = d.powerful_role.clone();
        super::test_settings::register(settings);

        let (_, outcome) = d.apply_as(&applicant_session, "support").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;
        let (_, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(outcome["grant"], "granted");
        // the role applied for is given; the powerful one is not
        assert_eq!(d.roles_of(&d.lounge, &d.applicant).await, vec![d.support_role.clone()]);
    });
}

#[test]
fn a_person_reads_everything_in_their_language_and_no_age_is_kept() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let reviewer_session = d.reviewer_session.clone();
        // the default: a role asks for no age. A 17-year-old who uses the app in French
        let mut settings = super::test_settings::get(&d.bot.id).unwrap();
        for role in &mut settings.roles {
            role.adult = false;
        }
        super::test_settings::register(settings);
        let (mut account, session, mut young) = d.harness.new_user().await;
        let year: i32 = iso8601_timestamp::Timestamp::now_utc().format().to_string()[0..4]
            .parse()
            .unwrap();
        account.age.as_mut().unwrap().birth_month = format!("{}-01", year - 17);
        d.harness.db.save_account(&account).await.unwrap();
        young
            .update(
                &d.harness.db,
                revolt_database::PartialUser {
                    locale: Some("fr-FR".into()),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        Member::create(&d.harness.db, &d.lounge, &young, None).await.unwrap();

        let channel = d.apply.clone();
        let panel = d.panel.clone();
        let pressed = d.press(&session.token, &channel, &panel, "ta:start").await;
        let (_, mine) = d.hand("mine", &pressed).await;
        assert_eq!(mine["roles"].as_array().unwrap().len(), 6);
        assert_eq!(mine["adult_only"], json!([]));

        let (_, outcome) = d.apply_as(&session.token, "support").await;
        assert_eq!(outcome["applied"], true, "{outcome}");
        let id = id_of(&outcome);
        // no age fact on the record or on the card
        assert!(!d.stored(&id).await.adult);
        assert_eq!(d.queue().await[0]["adult"], false);
        // the receipt is French and says it is a volunteer project, not a job
        let told = d.told(&young).await;
        assert!(
            told.iter().any(|m| m.contains("Ta candidature est arrivée")
                && m.contains("projet bénévole")
                && m.contains("Ce n'est ni un travail ni un emploi")
                && m.contains("« Support »")),
            "{told:?}"
        );
        // the reviewers still read English
        assert_eq!(d.queue().await[0]["role_name"], "Role support");
        assert_eq!(d.queue().await[0]["answers"][2]["label"], "Your experience");

        let card = d.card(&id).await;
        let (_, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(outcome["grant"], "granted");
        let told = d.told(&young).await;
        // the welcome is French too
        assert!(told.iter().any(|m| m.contains("Bienvenue dans l'équipe Zrzur") && m.contains("membre bénévole")), "{told:?}");
    });
}

#[test]
fn an_acceptance_counts_only_from_the_servers_own_question() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();
        let review = d.review.clone();
        let (_, outcome) = d.apply_as(&applicant_session, "support").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;
        let wanted = d.confirm_text(&id).await;
        // the server words the question: who, which role, where
        let applicant_name = format!("{}#{}", d.applicant.username, d.applicant.discriminator);
        assert!(wanted.starts_with(&format!("Accept {applicant_name} for Role support in ")), "{wanted}");

        // a button on the card that carries an acceptance, whatever it says
        let pressed = d.press(&reviewer_session, &review, &card, &format!("ta:accept:{id}")).await;
        let (status, _) = d.hand("act", &pressed).await;
        assert_eq!(status, Status::BadRequest);

        // a private question with other words on it
        let pressed = d
            .confirm_press(&review, &reviewer_session, &card, &id, "Open an interview with this person?", "Yes, accept")
            .await;
        let (status, _) = d.hand("act", &pressed).await;
        assert_eq!(status, Status::BadRequest);
        // the question names someone else
        let other = wanted.replace(&applicant_name, "Omar#0001");
        let pressed = d
            .confirm_press(&review, &reviewer_session, &card, &id, &other, "Yes, accept")
            .await;
        let (status, _) = d.hand("act", &pressed).await;
        assert_eq!(status, Status::BadRequest);
        // the right words under a button that says something else
        let pressed = d
            .confirm_press(&review, &reviewer_session, &card, &id, &wanted, "Interview")
            .await;
        let (status, _) = d.hand("act", &pressed).await;
        assert_eq!(status, Status::BadRequest);

        // other words shown, then changed to the right ones before the hand-back
        let pressed = d
            .confirm_press(&review, &reviewer_session, &card, &id, "Thank this person?", "Yes, accept")
            .await;
        let question = d.last_question.clone().unwrap();
        let response = d
            .harness
            .client
            .patch(format!("/interactions/{}/{}/original", question.0, question.1))
            .header(ContentType::JSON)
            .body(json!({ "content": wanted }).to_string())
            .dispatch()
            .await;
        assert!(response.status().code < 300, "{:?}", response.status());
        drop(response);
        let (status, _) = d.hand("act", &pressed).await;
        assert_eq!(status, Status::BadRequest);

        // nothing happened so far
        assert_eq!(d.stored(&id).await.status, TeamApplicationStatus::Open);
        assert!(d.roles_of(&d.lounge, &d.applicant).await.is_empty());
        assert!(d.audit(&d.applicant).await.is_empty());

        // the server's own question, unchanged: accepted
        let (status, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        // outside the bot, the reviewer reads whom they accepted
        let told = d.told(&d.reviewer.clone()).await;
        assert!(told.iter().any(|m| m.contains("You accepted") && m.contains(&d.applicant.username)), "{told:?}");

        // "Apply" and "My application" count only in the apply channel
        let stray = d.message(&review, &["ta:start"]).await;
        let pressed = d.press(&applicant_session, &review, &stray, "ta:start").await;
        let (status, _) = d.hand("mine", &pressed).await;
        assert_eq!(status, Status::Forbidden);
    });
}

#[test]
fn a_role_that_can_manage_anything_is_never_handed_out() {
    on_big_stack(|| async {
        let d = desk().await;
        let db = &d.harness.db;
        let lounge = db.fetch_server(&d.lounge.id).await.unwrap();
        // the plain roles of the desk are fine
        assert!(!super::powerful(db, &lounge, &d.support_role).await);
        assert!(!super::powerful(db, &lounge, &d.team_role).await);
        // a role that does not exist is not waved through
        assert!(super::powerful(db, &lounge, "01NOSUCHROLE00000000000000").await);

        for permission in super::POWERFUL {
            let role = d
                .harness
                .new_role(&d.lounge, 9, Some(OverrideField { a: *permission as i64, d: 0 }))
                .await;
            let lounge = db.fetch_server(&d.lounge.id).await.unwrap();
            assert!(super::powerful(db, &lounge, &role.id).await, "{permission:?}");
        }
        // what a moderator needs is allowed
        let moderator = d
            .harness
            .new_role(
                &d.lounge,
                9,
                Some(OverrideField {
                    a: ChannelPermission::KickMembers as i64
                        | ChannelPermission::BanMembers as i64
                        | ChannelPermission::TimeoutMembers as i64
                        | ChannelPermission::ManageMessages as i64,
                    d: 0,
                }),
            )
            .await;
        let lounge = db.fetch_server(&d.lounge.id).await.unwrap();
        assert!(!super::powerful(db, &lounge, &moderator.id).await);

        // a plain role that one channel lets manage its webhooks
        let mut channel = d.apply.clone();
        channel
            .set_role_permission(
                db,
                &moderator.id,
                OverrideField { a: ChannelPermission::ManageWebhooks as i64, d: 0 },
            )
            .await
            .unwrap();
        let lounge = db.fetch_server(&d.lounge.id).await.unwrap();
        assert!(super::powerful(db, &lounge, &moderator.id).await);
    });
}

#[test]
fn the_reason_stays_out_of_the_log_and_cannot_carry_a_link() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();
        let (_, outcome) = d.apply_as(&applicant_session, "support").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;
        let reason = "[Re-verify your account](https://evil.example) @everyone {terms}";
        let (status, outcome) = d.reject(&reviewer_session, &card, &id, reason).await;
        assert_eq!(status, Status::Ok, "{outcome}");

        // the person reads the words, not a link, a mention or a second copy of the terms
        let told = d.told(&d.applicant).await;
        let message = told.iter().find(|m| m.contains("Re-verify")).expect("the rejection");
        assert!(!message.contains("](https://"), "{message}");
        assert!(!message.contains("https://evil"), "{message}");
        assert!(!message.contains("@everyone"), "{message}");
        assert!(!message.contains("volunteer project"), "{message}");

        // the log says that there was a reason, not what it was
        let entries: Vec<_> = d
            .harness
            .db
            .fetch_platform_audit(revolt_database::PlatformAuditQuery {
                limit: 500,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.target_id.as_deref() == Some(d.applicant.id.as_str()))
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].reason.is_none());
        let summary = entries[0].summary.clone().unwrap_or_default();
        assert!(summary.contains("reason of 64 characters"), "{summary}");
        assert!(!format!("{:?}", entries[0]).contains("evil.example"));
    });
}

#[test]
fn a_banned_account_is_not_taken_into_the_team() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let applicant_session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();
        let (_, outcome) = d.apply_as(&applicant_session, "support").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;
        let mut person = d.harness.db.fetch_user(&d.applicant.id).await.unwrap();
        person
            .update(
                &d.harness.db,
                revolt_database::PartialUser {
                    flags: Some(v0::UserFlags::Banned as i32),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        let (status, outcome) = d.review_press(&reviewer_session, &card, "accept", &id).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], false);
        assert_eq!(outcome["problem"], "not_eligible");
        assert_eq!(d.stored(&id).await.status, TeamApplicationStatus::Open);
        assert!(d.roles_of(&d.lounge, &d.applicant).await.is_empty());
    });
}

// ---- "About you" ----

impl Desk {
    /// Switch the volunteer care on, with the rules at `version`
    fn volunteers_on(&self, version: &str) {
        let mut settings = super::test_settings::get(&self.bot.id).unwrap();
        settings.volunteers.enabled = true;
        settings.volunteers.rules_version = version.to_string();
        super::test_settings::register(settings);
    }

    /// A person presses Apply, gets "About you" and sends it; gives what the
    /// server answered to the hand-back
    async fn about_with(&mut self, session: &str, role: &str, fields: Value) -> (Status, Value) {
        let channel = self.apply.clone();
        let panel = self.panel.clone();
        let parent = self.press(session, &channel, &panel, "ta:start").await;
        let text = |id: &str| {
            json!({ "type": "Field", "label": id, "component": {
                "type": "TextInput", "custom_id": id, "style": "Short", "label": id, "required": true
            }})
        };
        let modal = json!({
            "custom_id": format!("ta:about:{role}"),
            "title": "About you",
            "components": [
                text("name"),
                text("age"),
                text("country"),
                { "type": "Field", "label": "Rules", "component": {
                    "type": "CheckboxGroup", "custom_id": "rules", "required": false,
                    "options": [{ "label": "I accept the volunteer rules", "value": "accept" }]
                }},
            ]
        });
        let sent = self.form(session, &channel, &parent, modal, fields).await;
        self.hand("about", &sent).await
    }

    async fn about_as(&mut self, session: &str, age: &str) -> (Status, Value) {
        self.about_with(session, "support", about_fields(age, &["accept"])).await
    }

    async fn identity_of(&self, user: &User) -> Option<revolt_database::TeamIdentity> {
        self.harness.db.fetch_team_identity(&user.id).await.unwrap()
    }

    /// The person takes the application `id` back
    async fn withdraw_as(&mut self, session: &str, id: &str) -> (Status, Value) {
        let channel = self.apply.clone();
        let withdraw = self.message(&channel, &[&format!("ta:withdraw:{id}")]).await;
        let pressed = self.press(session, &channel, &withdraw, &format!("ta:withdraw:{id}")).await;
        self.hand("withdraw", &pressed).await
    }
}

fn about_fields(age: &str, rules: &[&str]) -> Value {
    json!([
        { "custom_id": "name", "value": "Lina Haddad" },
        { "custom_id": "age", "value": age },
        { "custom_id": "country", "value": "Austria" },
        { "custom_id": "rules", "values": rules },
    ])
}

#[test]
fn about_saves_identity_and_rules_version() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("3");
        let session = d.applicant_session.clone();
        let (status, outcome) = d.about_as(&session, " 24 ").await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["saved"], true);
        let identity = d.identity_of(&d.applicant).await.unwrap();
        assert_eq!(identity.id, d.applicant.id);
        assert_eq!(identity.name, "Lina Haddad");
        assert_eq!(identity.age, 24);
        assert_eq!(identity.country, "Austria");
        assert_eq!(identity.rules_version, "3");
        assert!(identity.rules_at > 0);
        assert_eq!(identity.purge_at, None);
        // a closed role takes nobody's answers
        let (_, outcome) = d.about_with(&session, "closed", about_fields("24", &["accept"])).await;
        assert_eq!(outcome["saved"], false);
        assert_eq!(outcome["problem"], "role_closed");
    });
}

#[test]
fn about_under_16_stores_nothing() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.applicant_session.clone();
        for age in ["15", "12", "0"] {
            let (status, outcome) = d.about_as(&session, age).await;
            assert_eq!(status, Status::Ok, "{outcome}");
            assert_eq!(outcome["saved"], false, "{age}");
            assert_eq!(outcome["problem"], "too_young", "{age}");
        }
        assert!(d.identity_of(&d.applicant).await.is_none());
    });
}

#[test]
fn about_accepts_arabic_digits() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.applicant_session.clone();
        let (status, outcome) = d.about_as(&session, "\u{0661}\u{0667}").await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["saved"], true, "{outcome}");
        assert_eq!(d.identity_of(&d.applicant).await.unwrap().age, 17);
    });
}

#[test]
fn about_refuses_words_in_age() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.applicant_session.clone();
        for age in ["17 years", "seventeen", "1 7", "100", "-20"] {
            let (status, outcome) = d.about_as(&session, age).await;
            assert_eq!(status, Status::Ok, "{outcome}");
            assert_eq!(outcome["saved"], false, "{age}");
            assert_eq!(outcome["problem"], "bad_age", "{age}");
        }
        assert!(d.identity_of(&d.applicant).await.is_none());
    });
}

#[test]
fn about_needs_the_rules_tick() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.applicant_session.clone();
        for rules in [vec![], vec!["something-else"]] {
            let (status, outcome) = d.about_with(&session, "support", about_fields("30", &rules)).await;
            assert_eq!(status, Status::Ok, "{outcome}");
            assert_eq!(outcome["saved"], false);
            assert_eq!(outcome["problem"], "rules_not_accepted");
        }
        assert!(d.identity_of(&d.applicant).await.is_none());
    });
}

#[test]
fn apply_needs_identity_when_enabled() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.applicant_session.clone();
        let (status, outcome) = d.apply_as(&session, "support").await;
        assert_eq!(status, Status::Ok);
        assert_eq!(outcome["applied"], false);
        assert_eq!(outcome["problem"], "identity_missing");
        assert!(d.harness.db.fetch_team_applications_of(&d.applicant.id).await.unwrap().is_empty());

        d.about_as(&session, "24").await;
        // the rules changed since: "About you" is asked again
        d.volunteers_on("2");
        let (_, outcome) = d.apply_as(&session, "support").await;
        assert_eq!(outcome["problem"], "identity_missing");
        d.about_as(&session, "24").await;
        let (_, outcome) = d.apply_as(&session, "support").await;
        assert_eq!(outcome["applied"], true, "{outcome}");
    });
}

#[test]
fn apply_works_without_identity_when_disabled() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let session = d.applicant_session.clone();
        let (_, outcome) = d.apply_as(&session, "support").await;
        assert_eq!(outcome["applied"], true, "{outcome}");
        assert!(d.identity_of(&d.applicant).await.is_none());
        let entry = d.queue().await.remove(0);
        assert_eq!(entry["has_identity"], false);
        assert_eq!(entry["age_note"], false);
        // and "About you" does not exist while it is off
        let (status, _) = d.about_as(&session, "24").await;
        assert_ne!(status, Status::Ok);
        assert!(d.identity_of(&d.applicant).await.is_none());
    });
}

#[test]
fn age_note_when_band_disagrees() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        // an account that states it is 17 ...
        let (mut account, young_session, young) = d.harness.new_user().await;
        let year: i32 = iso8601_timestamp::Timestamp::now_utc().format().to_string()[0..4]
            .parse()
            .unwrap();
        account.age.as_mut().unwrap().birth_month = format!("{}-01", year - 17);
        d.harness.db.save_account(&account).await.unwrap();
        Member::create(&d.harness.db, &d.lounge, &young, None).await.unwrap();
        // ... but says 25 in "About you"
        let (_, outcome) = d.about_with(&young_session.token, "designer", about_fields("25", &["accept"])).await;
        assert_eq!(outcome["saved"], true, "{outcome}");
        let (_, outcome) = d.apply_as(&young_session.token, "designer").await;
        assert_eq!(outcome["applied"], true, "{outcome}");
        let queue = d.queue().await;
        let entry = queue.iter().find(|e| e["user_id"] == young.id.as_str()).unwrap();
        assert_eq!(entry["has_identity"], true);
        assert_eq!(entry["age_note"], true);

        // an adult account that agrees has no note
        let session = d.applicant_session.clone();
        d.about_as(&session, "25").await;
        d.apply_as(&session, "support").await;
        let queue = d.queue().await;
        let entry = queue.iter().find(|e| e["user_id"] == d.applicant.id.as_str()).unwrap();
        assert_eq!(entry["has_identity"], true);
        assert_eq!(entry["age_note"], false);
        // the card never carries the values
        assert!(!entry.to_string().contains("Lina Haddad"));
    });
}

#[test]
fn withdraw_erases_identity_without_membership() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.applicant_session.clone();
        d.about_as(&session, "24").await;
        let (_, outcome) = d.apply_as(&session, "support").await;
        let (status, outcome) = d.withdraw_as(&session, &id_of(&outcome)).await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true);
        assert!(d.identity_of(&d.applicant).await.is_none());

        // a current volunteer in another role keeps theirs
        let (_, other_session, other) = d.harness.new_user().await;
        Member::create(&d.harness.db, &d.lounge, &other, None).await.unwrap();
        d.about_as(&other_session.token, "40").await;
        d.harness
            .db
            .insert_team_member(&revolt_database::TeamMember {
                id: ulid::Ulid::new().to_string(),
                user_id: other.id.clone(),
                role: "designer".to_string(),
                server: d.lounge.id.clone(),
                status: revolt_database::TeamMemberStatus::Active,
                joined_at: 1,
                trial_ends_at: 2,
                confirmed_at: None,
                away_until: None,
                left_at: None,
                low_months: 0,
                notes: None,
                history: vec![],
            })
            .await
            .unwrap();
        let (_, outcome) = d.apply_as(&other_session.token, "support").await;
        let (_, outcome) = d.withdraw_as(&other_session.token, &id_of(&outcome)).await;
        assert_eq!(outcome["applied"], true, "{outcome}");
        assert!(d.identity_of(&other).await.is_some());
    });
}

#[test]
fn reject_sets_identity_purge_with_the_application() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();
        d.about_as(&session, "24").await;
        let (_, outcome) = d.apply_as(&session, "support").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;
        let (status, outcome) = d.reject(&reviewer_session, &card, &id, "Not a fit right now.").await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], true, "{outcome}");
        let purge_at = d.stored(&id).await.purge_at;
        assert!(purge_at.is_some());
        let identity = d.identity_of(&d.applicant).await.unwrap();
        assert_eq!(identity.purge_at, purge_at);
        // "About you" again brings the person back: no deletion time
        d.about_as(&session, "24").await;
        assert_eq!(d.identity_of(&d.applicant).await.unwrap().purge_at, None);
    });
}

#[test]
fn a_stale_identity_does_not_pass_apply() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.applicant_session.clone();
        let reviewer_session = d.reviewer_session.clone();
        d.about_as(&session, "24").await;
        let (_, outcome) = d.apply_as(&session, "support").await;
        let id = id_of(&outcome);
        let card = d.card(&id).await;
        d.reject(&reviewer_session, &card, &id, "Not a fit right now.").await;
        // time is up for both, but the clean-up task has not run
        let mut identity = d.identity_of(&d.applicant).await.unwrap();
        identity.purge_at = Some(1);
        d.harness.db.upsert_team_identity(&identity).await.unwrap();
        let mut application = d.stored(&id).await;
        application.purge_at = Some(1);
        assert!(d
            .harness
            .db
            .replace_team_application_if(&application, TeamApplicationStatus::Rejected)
            .await
            .unwrap());
        let (status, outcome) = d.apply_as(&session, "support").await;
        assert_eq!(status, Status::Ok, "{outcome}");
        assert_eq!(outcome["applied"], false);
        assert_eq!(outcome["problem"], "identity_missing");
        assert!(d.identity_of(&d.applicant).await.is_none());
    });
}

// ---- "Show identity" ----

impl Desk {
    /// A reviewer presses "Show identity" on the card of `id`
    async fn identity_press(&mut self, session: &str, id: &str) -> (Status, Value) {
        let channel = self.review.clone();
        let card = self.message(&channel, &[&format!("ta:identity:{id}")]).await;
        let pressed = self.press(session, &channel, &card, &format!("ta:identity:{id}")).await;
        self.hand("identity", &pressed).await
    }

    /// An application with an identity of `name`, ready to be looked at
    async fn applied_as(&mut self, name: &str) -> String {
        self.volunteers_on("1");
        let session = self.applicant_session.clone();
        let fields = json!([
            { "custom_id": "name", "value": name },
            { "custom_id": "age", "value": "24" },
            { "custom_id": "country", "value": "Austria" },
            { "custom_id": "rules", "values": ["accept"] },
        ]);
        let (_, outcome) = self.about_with(&session, "support", fields).await;
        assert_eq!(outcome["saved"], true, "{outcome}");
        let (_, outcome) = self.apply_as(&session, "support").await;
        id_of(&outcome)
    }
}

#[test]
fn reviewer_sees_identity_and_view_is_logged() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let id = d.applied_as("Lina Haddad").await;
        let session = d.reviewer_session.clone();
        let (status, view) = d.identity_press(&session, &id).await;
        assert_eq!(status, Status::Ok, "{view}");
        assert_eq!(view["name"], "Lina Haddad");
        assert_eq!(view["age"], 24);
        assert_eq!(view["country"], "Austria");
        assert_eq!(view["age_note"], false);
        assert_eq!(view["missing"], false);
        let views: Vec<_> = d
            .audit(&d.applicant)
            .await
            .into_iter()
            .filter(|(action, _, _)| action == "team.identity.view")
            .collect();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].1, d.reviewer.id);
        assert_eq!(views[0].2, "Success");
        // the record names nobody's details
        let log = format!("{:?}", d.harness.db.fetch_platform_audit(revolt_database::PlatformAuditQuery {
            limit: 500,
            ..Default::default()
        }).await.unwrap());
        for secret in ["Lina Haddad", "Austria"] {
            assert!(!log.contains(secret), "{secret} in the log");
        }
    });
}

#[test]
fn former_reviewer_is_refused_and_logged() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let id = d.applied_as("Lina Haddad").await;
        let session = d.reviewer_session.clone();
        // the card was drawn while they were a reviewer; the role is gone now
        let channel = d.review.clone();
        let card = d.message(&channel, &[&format!("ta:identity:{id}")]).await;
        give_role(&d.harness, &d.lounge, &d.reviewer.clone(), vec![]).await;
        let pressed = d.press(&session, &channel, &card, &format!("ta:identity:{id}")).await;
        let (status, body) = d.hand("identity", &pressed).await;
        assert_eq!(status, Status::Forbidden);
        assert!(!body.to_string().contains("Lina"));
        let refused: Vec<_> = d
            .audit(&d.applicant)
            .await
            .into_iter()
            .filter(|(action, _, _)| action == "team.identity.view")
            .collect();
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].1, d.reviewer.id);
        assert_eq!(refused[0].2, "Failed");
    });
}

#[test]
fn identity_is_off_while_the_switch_is_off() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let session = d.reviewer_session.clone();
        let (_, outcome) = d.apply_as(&d.applicant_session.clone(), "support").await;
        let id = id_of(&outcome);
        let (status, _) = d.identity_press(&session, &id).await;
        assert_eq!(status, Status::BadRequest);
        assert!(d
            .audit(&d.applicant)
            .await
            .iter()
            .all(|(action, _, _)| action != "team.identity.view"));
    });
}

#[test]
fn identity_press_is_single_use() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let id = d.applied_as("Lina Haddad").await;
        let session = d.reviewer_session.clone();
        let channel = d.review.clone();
        let card = d.message(&channel, &[&format!("ta:identity:{id}")]).await;
        let pressed = d.press(&session, &channel, &card, &format!("ta:identity:{id}")).await;
        let (status, view) = d.hand("identity", &pressed).await;
        assert_eq!(status, Status::Ok, "{view}");
        assert_eq!(view["name"], "Lina Haddad");
        let (status, body) = d.hand("identity", &pressed).await;
        assert_ne!(status, Status::Ok);
        assert!(!body.to_string().contains("Lina"));
        let views: Vec<_> = d
            .audit(&d.applicant)
            .await
            .into_iter()
            .filter(|(action, _, _)| action == "team.identity.view")
            .collect();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].2, "Success");
    });
}

#[test]
fn identity_missing_when_never_filled_or_unknown_application() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let session = d.reviewer_session.clone();
        let (_, outcome) = d.apply_as(&d.applicant_session.clone(), "support").await;
        let id = id_of(&outcome);
        d.volunteers_on("1");
        // the person never filled "About you"
        let (status, view) = d.identity_press(&session, &id).await;
        assert_eq!(status, Status::Ok, "{view}");
        assert_eq!(view["missing"], true);
        assert_eq!(view["name"], "");
        assert_eq!(view["age"], 0);
        let seen: Vec<_> = d
            .audit(&d.applicant)
            .await
            .into_iter()
            .filter(|(action, _, _)| action == "team.identity.view")
            .collect();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].2, "Success");
        // an application that does not exist
        let (status, view) = d.identity_press(&session, "01ZZZZZZZZZZZZZZZZZZZZZZZZ").await;
        assert_eq!(status, Status::Ok, "{view}");
        assert_eq!(view["missing"], true);
    });
}

#[test]
fn identity_of_own_application_is_refused_and_logged() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let session = d.reviewer_session.clone();
        let (_, outcome) = d.about_with(&session, "support", about_fields("30", &["accept"])).await;
        assert_eq!(outcome["saved"], true, "{outcome}");
        let (_, outcome) = d.apply_as(&session, "support").await;
        let id = id_of(&outcome);
        let (status, body) = d.identity_press(&session, &id).await;
        assert_eq!(status, Status::Forbidden);
        assert!(!body.to_string().contains("Lina"));
        let refused: Vec<_> = d
            .audit(&d.reviewer)
            .await
            .into_iter()
            .filter(|(action, _, _)| action == "team.identity.view")
            .collect();
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].2, "Failed");
    });
}

#[test]
fn identity_pressed_outside_a_review_channel_is_refused_and_logged() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let id = d.applied_as("Lina Haddad").await;
        let session = d.reviewer_session.clone();
        let channel = d.apply.clone();
        let card = d.message(&channel, &[&format!("ta:identity:{id}")]).await;
        let pressed = d.press(&session, &channel, &card, &format!("ta:identity:{id}")).await;
        let (status, body) = d.hand("identity", &pressed).await;
        assert_eq!(status, Status::Forbidden);
        assert!(!body.to_string().contains("Lina"));
        let refused: Vec<_> = d
            .audit(&d.applicant)
            .await
            .into_iter()
            .filter(|(action, _, _)| action == "team.identity.view")
            .collect();
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].2, "Failed");
    });
}

#[test]
fn identity_carries_the_age_note() {
    on_big_stack(|| async {
        let mut d = desk().await;
        d.volunteers_on("1");
        let (mut account, young_session, young) = d.harness.new_user().await;
        let year: i32 = iso8601_timestamp::Timestamp::now_utc().format().to_string()[0..4]
            .parse()
            .unwrap();
        account.age.as_mut().unwrap().birth_month = format!("{}-01", year - 17);
        d.harness.db.save_account(&account).await.unwrap();
        Member::create(&d.harness.db, &d.lounge, &young, None).await.unwrap();
        let (_, outcome) = d.about_with(&young_session.token, "designer", about_fields("25", &["accept"])).await;
        assert_eq!(outcome["saved"], true, "{outcome}");
        let (_, outcome) = d.apply_as(&young_session.token, "designer").await;
        let id = id_of(&outcome);
        let session = d.reviewer_session.clone();
        let (status, view) = d.identity_press(&session, &id).await;
        assert_eq!(status, Status::Ok, "{view}");
        assert_eq!(view["age"], 25);
        assert_eq!(view["age_note"], true);
        assert_eq!(view["missing"], false);
    });
}

#[test]
fn identity_never_in_queue_or_card_entry() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let id = d.applied_as("Lina Haddad").await;
        let queue = d.queue().await;
        let entry = queue.iter().find(|e| e["id"] == id.as_str()).unwrap();
        assert_eq!(entry["has_identity"], true);
        let text = Value::Array(queue.clone()).to_string();
        for secret in ["Lina", "Haddad", "Austria"] {
            assert!(!text.contains(secret), "{secret} in the queue");
        }
        let (_, state) = d
            .as_bot(&d.bot.token, &format!("/team/desk/applications/{id}"), None)
            .await;
        assert!(!state.to_string().contains("Haddad"));
    });
}

#[test]
fn identity_escaped() {
    on_big_stack(|| async {
        let mut d = desk().await;
        let id = d.applied_as("@everyone [x](http://a)").await;
        let session = d.reviewer_session.clone();
        let (status, view) = d.identity_press(&session, &id).await;
        assert_eq!(status, Status::Ok, "{view}");
        let name = view["name"].as_str().unwrap();
        assert!(name.contains("@\u{200b}everyone"), "mention alive: {name}");
        assert!(name.contains("\\[x\\]"), "link brackets alive: {name}");
        assert!(!name.contains("://"), "link alive: {name}");
        assert_eq!(name, crate::util::team_texts::plain("@everyone [x](http://a)"));
    });
}
