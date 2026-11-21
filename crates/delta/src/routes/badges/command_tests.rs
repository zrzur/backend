//! The owner's `/badge` command: the bot hands back the press, the server
//! decides who may give what.

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_config::FeaturesBadgeCommand;
use revolt_database::{events::client::EventV1, Bot, Channel, Member, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use super::command::{test_settings, DEFAULT_REASON};

struct Stage {
    harness: TestHarness,
    bot: Bot,
    /// The account the settings name as owner
    owner: User,
    owner_session: String,
    /// A member of the server who is not the owner
    other: User,
    other_session: String,
    /// Who gets and loses badges
    subject: User,
    channel: Channel,
    command: v0::ApplicationCommand,
    events: PubSubTestHelper,
}

fn sub(name: &str) -> Value {
    let badge = json!({ "type": "String", "name": "badge", "description": "Badge", "required": true });
    json!({
        "type": "SubCommand",
        "name": name,
        "description": name,
        "options": [
            { "type": "User", "name": "user", "description": "Who", "required": true },
            badge,
            { "type": "String", "name": "reason", "description": "Why", "max_length": 500 }
        ]
    })
}

/// A server with the bot, the owner, another member and the subject; the
/// bot registered `/badge give|remove` (without choices, so the tests can
/// send a badge that does not exist) and the command is on for it
async fn stage() -> Stage {
    let harness = TestHarness::new().await;
    let (_, _, host) = harness.new_user().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (_, _, subject) = harness.new_user().await;
    let (server, channels) = harness.new_server(&host).await;
    let (bot, bot_user) = harness.new_bot(&host).await;
    for member in [&host, &owner, &other, &subject, &bot_user] {
        Member::create(&harness.db, &server, member, None).await.unwrap();
    }
    revolt_presence::create_session(&bot.id, 0).await;
    let command = revolt_database::create_application_command(
        &harness.db,
        &bot.id,
        None,
        serde_json::from_value(json!({
            "name": "badge",
            "description": "Give or take a badge",
            "options": [sub("give"), sub("remove")]
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    test_settings::register(FeaturesBadgeCommand {
        owner: owner.id.clone(),
        bot: bot.id.clone(),
    });
    let events = PubSubTestHelper::new(&format!("{}!", bot.id)).await;
    Stage {
        harness,
        bot,
        owner,
        owner_session: owner_session.token,
        other,
        other_session: other_session.token,
        subject,
        channel: channels.into_iter().next().unwrap(),
        command,
        events,
    }
}

impl Stage {
    /// Someone uses `/badge <action>`; gives what the bot received: id and token
    async fn press(&mut self, session: &str, action: &str, user: &str, badge: &str, reason: Option<&str>) -> (String, String) {
        let mut options = vec![
            json!({ "type": "User", "name": "user", "value": user }),
            json!({ "type": "String", "name": "badge", "value": badge }),
        ];
        if let Some(reason) = reason {
            options.push(json!({ "type": "String", "name": "reason", "value": reason }));
        }
        let response = self
            .harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", session.to_string()))
            .header(ContentType::JSON)
            .body(
                json!({
                    "type": "Command",
                    "application_id": self.bot.id,
                    "channel_id": self.channel.id(),
                    "command_id": self.command.id,
                    "command_version": self.command.version,
                    "options": [{ "type": "SubCommand", "name": action, "options": options }],
                    "nonce": ulid::Ulid::new().to_string(),
                })
                .to_string(),
            )
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let accepted: Value = response.into_json().await.unwrap();
        let id = accepted["_id"].as_str().unwrap().to_string();
        let token = match self
            .events
            .wait_for_event(|e| matches!(e, EventV1::InteractionCreate { interaction: i } if i.id == id))
            .await
        {
            EventV1::InteractionCreate { interaction } => interaction.token.unwrap(),
            _ => unreachable!(),
        };
        (id, token)
    }

    async fn call(&self, token: &str, path: &str, body: Option<Value>) -> (Status, Value) {
        let request = match &body {
            Some(body) => self
                .harness
                .client
                .post(path.to_string())
                .header(ContentType::JSON)
                .body(body.to_string()),
            None => self.harness.client.get(path.to_string()),
        };
        let response = request.header(Header::new("x-bot-token", token.to_string())).dispatch().await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    /// The bot hands the press back
    async fn hand(&self, path: &str, pressed: &(String, String)) -> (Status, Value) {
        self.hand_as(&self.bot.token, path, pressed).await
    }

    async fn hand_as(&self, token: &str, path: &str, pressed: &(String, String)) -> (Status, Value) {
        self.call(
            token,
            &format!("/badges/command/{path}"),
            Some(json!({ "interaction_id": pressed.0, "token": pressed.1 })),
        )
        .await
    }

    async fn holds(&self, user: &User, badge: &str) -> bool {
        revolt_database::held_badges(&self.harness.db, &user.id).await.unwrap().contains(badge)
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
            .filter(|e| e.target_id.as_deref() == Some(about.id.as_str()) && e.action.starts_with("badge.command."))
            .map(|e| (e.action, e.actor_id, format!("{:?}", e.outcome)))
            .collect()
    }
}

#[rocket::async_test]
async fn the_owner_gives_a_privileged_badge_and_takes_it_away() {
    let mut s = stage().await;
    let (subject, owner) = (s.subject.clone(), s.owner.clone());
    let session = s.owner_session.clone();

    // the catalogue offers the privileged and the retired badges, never one that follows a record
    let (status, catalogue) = s.call(&s.bot.token, "/badges/command/catalogue", None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(catalogue["enabled"], true);
    let ids: Vec<&str> = catalogue["badges"].as_array().unwrap().iter().map(|b| b["id"].as_str().unwrap()).collect();
    for wanted in ["staff", "founder", "developer", "platform_moderator", "partner", "paw", "reserved_1", "reserved_2"] {
        assert!(ids.contains(&wanted), "{wanted} missing: {ids:?}");
    }
    for never in ["plus", "creator", "beta_tester", "early_adopter", "one_year"] {
        assert!(!ids.contains(&never), "{never} offered");
    }
    assert_eq!(catalogue["badges"][0]["names"]["ar"], "مؤسس");

    let pressed = s.press(&session, "give", &subject.id, "founder", None).await;
    let (status, outcome) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::Ok, "{outcome}");
    assert_eq!(outcome["result"], "done");
    assert_eq!(outcome["action"], "give");
    assert_eq!(outcome["badge"], "founder");
    assert_eq!(outcome["user"]["id"], subject.id.as_str());
    assert!(s.holds(&subject, "founder").await);
    let grant = s
        .harness
        .db
        .fetch_badge_grants(&subject.id)
        .await
        .unwrap()
        .into_iter()
        .find(|g| g.badge == "founder")
        .unwrap();
    assert_eq!(grant.granted_by, owner.id);
    assert_eq!(grant.reason.as_deref(), Some(DEFAULT_REASON));
    assert_eq!(
        s.audit(&subject).await,
        vec![("badge.command.grant".to_string(), owner.id.clone(), "Success".to_string())]
    );

    // a retired one too, with the owner's own reason
    let pressed = s.press(&session, "give", &subject.id, "paw", Some("For the old days")).await;
    let (status, outcome) = s.hand("grant", &pressed).await;
    assert_eq!((status, outcome["result"].as_str()), (Status::Ok, Some("done")), "{outcome}");
    assert!(s.holds(&subject, "paw").await);

    // given twice: they already have it, nothing changes
    let pressed = s.press(&session, "give", &subject.id, "founder", None).await;
    let (status, outcome) = s.hand("grant", &pressed).await;
    assert_eq!((status, outcome["result"].as_str()), (Status::Ok, Some("already")), "{outcome}");

    // taken away
    let pressed = s.press(&session, "remove", &subject.id, "founder", Some("Test is over")).await;
    let (status, outcome) = s.hand("revoke", &pressed).await;
    assert_eq!((status, outcome["result"].as_str()), (Status::Ok, Some("done")), "{outcome}");
    assert_eq!(outcome["action"], "remove");
    assert!(!s.holds(&subject, "founder").await);
    let log = s.audit(&subject).await;
    assert!(log.contains(&("badge.command.revoke".to_string(), owner.id.clone(), "Success".to_string())), "{log:?}");

    // and again: they do not have it
    let pressed = s.press(&session, "remove", &subject.id, "founder", None).await;
    let (status, outcome) = s.hand("revoke", &pressed).await;
    assert_eq!((status, outcome["result"].as_str()), (Status::Ok, Some("not_held")), "{outcome}");

    // a removal handed to the give route is refused, and gives nothing
    let pressed = s.press(&session, "remove", &subject.id, "founder", None).await;
    let (status, _) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::BadRequest);
    assert!(!s.holds(&subject, "founder").await);

    // nobody changes their own badges
    let pressed = s.press(&session, "give", &owner.id, "founder", None).await;
    let (status, outcome) = s.hand("grant", &pressed).await;
    assert_eq!((status, outcome["result"].as_str()), (Status::Ok, Some("self")), "{outcome}");
    assert!(!s.holds(&owner, "founder").await);
}

#[rocket::async_test]
async fn anyone_else_is_refused_and_logged() {
    let mut s = stage().await;
    let (subject, other) = (s.subject.clone(), s.other.clone());
    let session = s.other_session.clone();
    for (action, path) in [("give", "grant"), ("remove", "revoke")] {
        let pressed = s.press(&session, action, &subject.id, "staff", None).await;
        let (status, body) = s.hand(path, &pressed).await;
        assert_eq!(status, Status::Forbidden, "{body}");
        assert_eq!(body["type"], "NotPrivileged");
    }
    assert!(!s.holds(&subject, "staff").await);
    // the bot cannot make it the owner's: the presser is what the server stored
    let mut log = s.audit(&subject).await;
    log.sort();
    assert_eq!(
        log,
        vec![
            ("badge.command.grant".to_string(), other.id.clone(), "Denied".to_string()),
            ("badge.command.revoke".to_string(), other.id.clone(), "Denied".to_string()),
        ]
    );
}

#[rocket::async_test]
async fn a_wrong_bot_a_replayed_press_and_a_wrong_token_are_refused() {
    let mut s = stage().await;
    let subject = s.subject.clone();
    let session = s.owner_session.clone();
    let pressed = s.press(&session, "give", &subject.id, "developer", None).await;

    // another bot, without the command
    let (stranger, _) = s.harness.new_bot(&s.owner).await;
    let (status, _) = s.hand_as(&stranger.token, "grant", &pressed).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = s.call(&stranger.token, "/badges/command/catalogue", None).await;
    assert_eq!(status, Status::NotFound);
    // another bot the command is on for, with a press it never received
    let (second, _) = s.harness.new_bot(&s.owner).await;
    test_settings::register(FeaturesBadgeCommand {
        owner: s.owner.id.clone(),
        bot: second.id.clone(),
    });
    let (status, body) = s.hand_as(&second.token, "grant", &pressed).await;
    assert_eq!(status, Status::NotFound, "{body}");
    assert_eq!(body["type"], "UnknownInteraction");
    // a person, not a bot
    let status = s
        .harness
        .client
        .get("/badges/command/catalogue")
        .header(Header::new("x-session-token", session.clone()))
        .dispatch()
        .await
        .status();
    assert_eq!(status, Status::NotFound);
    // a wrong token
    let (status, _) = s.hand("grant", &(pressed.0.clone(), "not-the-token".to_string())).await;
    assert_eq!(status, Status::NotFound);
    assert!(!s.holds(&subject, "developer").await);

    // the real one works once
    let (status, outcome) = s.hand("grant", &pressed).await;
    assert_eq!((status, outcome["result"].as_str()), (Status::Ok, Some("done")), "{outcome}");
    assert!(s.holds(&subject, "developer").await);
    // replayed: refused, nothing more written
    let (status, body) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::Gone, "{body}");
    assert_eq!(body["type"], "InteractionExpired");
    let pressed_again = s.press(&session, "remove", &subject.id, "developer", None).await;
    let (status, _) = s.hand("revoke", &pressed_again).await;
    assert_eq!(status, Status::Ok);
    let (status, _) = s.hand("revoke", &pressed_again).await;
    assert_eq!(status, Status::Gone);
    assert_eq!(s.audit(&subject).await.len(), 2);
}

#[rocket::async_test]
async fn the_command_is_off_without_settings() {
    let mut s = stage().await;
    let subject = s.subject.clone();
    let session = s.owner_session.clone();
    let pressed = s.press(&session, "give", &subject.id, "staff", None).await;
    // a bot with no settings registered falls back to the shipped config: both ids empty
    let (unset, unset_user) = s.harness.new_bot(&s.owner).await;
    assert!(unset_user.bot.is_some());
    for path in ["grant", "revoke"] {
        let (status, _) = s.hand_as(&unset.token, path, &pressed).await;
        assert_eq!(status, Status::NotFound, "{path}");
    }
    let (status, _) = s.call(&unset.token, "/badges/command/catalogue", None).await;
    assert_eq!(status, Status::NotFound);
    assert!(!revolt_config::config().await.features.badge_command.enabled());
    assert!(!s.holds(&subject, "staff").await);
}

#[rocket::async_test]
async fn an_unknown_badge_is_not_found() {
    let mut s = stage().await;
    let subject = s.subject.clone();
    let session = s.owner_session.clone();
    let pressed = s.press(&session, "give", &subject.id, "no_such_badge", None).await;
    let (status, body) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::NotFound, "{body}");
    assert_eq!(body["type"], "NotFound");
    // a badge that follows a record can't be given by hand, even by the owner
    let pressed = s.press(&session, "give", &subject.id, "plus", None).await;
    let (status, _) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::BadRequest);
    assert!(!s.holds(&subject, "plus").await);
}
