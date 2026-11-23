//! The owner's `/badge` command: the bot hands back the press, the server
//! decides who may give what.

use crate::{
    rocket,
    util::test::{PubSubTestHelper, TestHarness},
};
use revolt_config::FeaturesBadgeCommand;
use revolt_database::{events::client::EventV1, Bot, Channel, Member, Server, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use super::command::{offered_badges, test_settings, DEFAULT_REASON};

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
    server: Server,
    channel: Channel,
    command: v0::ApplicationCommand,
    /// Kind of the `user` option as registered (`User`, or `String` to forge)
    user_kind: String,
    events: PubSubTestHelper,
}

/// The badge choices as the bot registers them: the catalogue, with the server's English names
fn offered_choices() -> Value {
    Value::Array(
        offered_badges()
            .into_iter()
            .map(|b| json!({ "name": crate::util::official::badge_names(b.id)["en"], "value": b.id }))
            .collect(),
    )
}

fn sub(name: &str, choices: &Value, user_kind: &str) -> Value {
    let badge = json!({ "type": "String", "name": "badge", "description": "Badge", "required": true, "choices": choices });
    json!({
        "type": "SubCommand",
        "name": name,
        "description": name,
        "options": [
            { "type": user_kind, "name": "user", "description": "Who", "required": true },
            badge,
            { "type": "String", "name": "reason", "description": "Why", "max_length": 500 }
        ]
    })
}

/// A server with the bot, the owner, another member and the subject; the
/// bot registered `/badge give|remove` as the server offers it, and the
/// command is on for it
async fn stage() -> Stage {
    stage_with(offered_choices(), "User").await
}

/// The same, with the badge choices and the `user` option's kind as given
async fn stage_with(choices: Value, user_kind: &str) -> Stage {
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
            "options": [sub("give", &choices, user_kind), sub("remove", &choices, user_kind)]
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
        server,
        channel: channels.into_iter().next().unwrap(),
        command,
        user_kind: user_kind.to_string(),
        events,
    }
}

impl Stage {
    /// Someone uses `/badge <action>`; gives what the bot received: id and token
    async fn press(&mut self, session: &str, action: &str, user: &str, badge: &str, reason: Option<&str>) -> (String, String) {
        let mut options = vec![
            json!({ "type": self.user_kind, "name": "user", "value": user }),
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
        self.audit_full(about).await.into_iter().map(|(a, b, c, _)| (a, b, c)).collect()
    }

    /// The same with each record's summary
    async fn audit_full(&self, about: &User) -> Vec<(String, String, String, String)> {
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
            .map(|e| (e.action, e.actor_id, format!("{:?}", e.outcome), e.summary.unwrap_or_default()))
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

    // given twice: they already have it, nothing changes, and the press writes one record
    let pressed = s.press(&session, "give", &subject.id, "founder", None).await;
    let (status, outcome) = s.hand("grant", &pressed).await;
    assert_eq!((status, outcome["result"].as_str()), (Status::Ok, Some("already")), "{outcome}");
    let log = s.audit_full(&subject).await;
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(log.iter().all(|(_, _, outcome, _)| outcome == "Success"), "{log:?}");
    assert!(log.iter().any(|(_, _, _, summary)| summary == "founder (no change: already)"), "{log:?}");

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
    // five presses, five records, none failed
    let log = s.audit(&subject).await;
    assert_eq!(log.len(), 5, "{log:?}");
    assert!(!log.iter().any(|(_, _, outcome)| outcome == "Failed"), "{log:?}");

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
async fn a_relabelled_choice_is_refused() {
    // the bot shows another badge's name but sends `founder` (a registered
    // command may not repeat a choice name, so the label is new)
    let mut choices = offered_choices();
    let founder = choices.as_array_mut().unwrap().iter_mut().find(|c| c["value"] == "founder").unwrap();
    founder["name"] = json!("Partner badge");
    let mut s = stage_with(choices, "User").await;
    let (subject, owner) = (s.subject.clone(), s.owner.clone());
    let session = s.owner_session.clone();
    let pressed = s.press(&session, "give", &subject.id, "founder", None).await;
    let (status, body) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert_eq!(body["type"], "InvalidOperation");
    assert!(!s.holds(&subject, "founder").await);
    assert_eq!(
        s.audit_full(&subject).await,
        vec![(
            "badge.command.grant".to_string(),
            owner.id.clone(),
            "Denied".to_string(),
            "founder (command not as offered)".to_string()
        )]
    );

    // a translated label that says something else is refused too
    let mut choices = offered_choices();
    choices[0]["name_localizations"] = json!({ "ar": "شريك" });
    let mut s = stage_with(choices, "User").await;
    let (subject, session) = (s.subject.clone(), s.owner_session.clone());
    let first = offered_badges()[0].id;
    let pressed = s.press(&session, "give", &subject.id, first, None).await;
    let (status, _) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::BadRequest);
    assert!(!s.holds(&subject, first).await);
}

#[rocket::async_test]
async fn a_user_asked_for_as_text_is_refused() {
    let mut s = stage_with(offered_choices(), "String").await;
    let (subject, owner) = (s.subject.clone(), s.owner.clone());
    let session = s.owner_session.clone();
    let pressed = s.press(&session, "give", &subject.id, "staff", None).await;
    let (status, body) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert!(!s.holds(&subject, "staff").await);
    // the user came as text, so the record names nobody
    let log: Vec<_> = s
        .harness
        .db
        .fetch_platform_audit(revolt_database::PlatformAuditQuery {
            limit: 500,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.actor_id == owner.id && e.action == "badge.command.grant")
        .map(|e| (format!("{:?}", e.outcome), e.target_id, e.summary))
        .collect();
    assert_eq!(log, vec![("Denied".to_string(), None, Some("staff (command not as offered)".to_string()))]);
}

#[rocket::async_test]
async fn an_unknown_badge_is_logged_as_unknown() {
    // a bot that adds a badge the server does not have
    let mut choices = offered_choices();
    choices.as_array_mut().unwrap().push(json!({ "name": "Gold", "value": "no_such_badge" }));
    let mut s = stage_with(choices, "User").await;
    let (subject, other, owner) = (s.subject.clone(), s.other.clone(), s.owner.clone());
    let (other_session, owner_session) = (s.other_session.clone(), s.owner_session.clone());
    let pressed = s.press(&other_session, "give", &subject.id, "no_such_badge", None).await;
    let (status, _) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::Forbidden);
    let pressed = s.press(&owner_session, "give", &subject.id, "no_such_badge", None).await;
    let (status, _) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::BadRequest);
    let mut log = s.audit_full(&subject).await;
    log.sort();
    let mut wanted = vec![
        ("badge.command.grant".to_string(), other.id.clone(), "Denied".to_string(), "unknown (owner only)".to_string()),
        ("badge.command.grant".to_string(), owner.id.clone(), "Denied".to_string(), "unknown (command not as offered)".to_string()),
    ];
    wanted.sort();
    assert_eq!(log, wanted);
}

#[rocket::async_test]
async fn only_the_badge_bots_badge_command_keeps_its_options() {
    let mut s = stage().await;
    let (subject, session) = (s.subject.clone(), s.owner_session.clone());
    let pressed = s.press(&session, "give", &subject.id, "partner", None).await;
    let stored = s.harness.db.fetch_interaction(&pressed.0).await.unwrap();
    assert_eq!(stored.command_options.len(), 1);
    assert_eq!(stored.command_options[0].name, "give");

    // another bot with its own `/badge`: nothing of what the person chose is kept
    let (other_bot, other_user) = s.harness.new_bot(&s.owner).await;
    Member::create(&s.harness.db, &s.server, &other_user, None).await.unwrap();
    revolt_presence::create_session(&other_bot.id, 0).await;
    let command = revolt_database::create_application_command(
        &s.harness.db,
        &other_bot.id,
        None,
        serde_json::from_value(json!({
            "name": "badge",
            "description": "Something else",
            "options": [{ "type": "String", "name": "note", "description": "Note" }]
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let status_and_id = {
        let response = s
            .harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", session.clone()))
            .header(ContentType::JSON)
            .body(
                json!({
                    "type": "Command",
                    "application_id": other_bot.id,
                    "channel_id": s.channel.id(),
                    "command_id": command.id,
                    "command_version": command.version,
                    "options": [{ "type": "String", "name": "note", "value": "private words" }],
                    "nonce": ulid::Ulid::new().to_string(),
                })
                .to_string(),
            )
            .dispatch()
            .await;
        let status = response.status();
        let accepted: Value = response.into_json().await.unwrap_or(Value::Null);
        (status, accepted["_id"].as_str().unwrap_or_default().to_string())
    };
    assert_eq!(status_and_id.0, Status::Ok);
    let stored = s.harness.db.fetch_interaction(&status_and_id.1).await.unwrap();
    assert!(stored.command_options.is_empty());
}

impl Stage {
    /// The bot replaces its commands through the registration route, as the library does
    async fn sync(&self, commands: Value) -> (Status, Value) {
        let response = self
            .harness
            .client
            .put(format!("/applications/{}/commands", self.bot.id))
            .header(Header::new("x-bot-token", self.bot.token.clone()))
            .header(ContentType::JSON)
            .body(commands.to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }
}

#[rocket::async_test]
async fn a_command_changed_between_the_press_and_the_grant_is_refused() {
    // the bot shows a relabelled command and takes the owner's press ...
    let mut choices = offered_choices();
    let founder = choices.as_array_mut().unwrap().iter_mut().find(|c| c["value"] == "founder").unwrap();
    founder["name"] = json!("Partner badge");
    let mut s = stage_with(choices, "User").await;
    let (subject, owner) = (s.subject.clone(), s.owner.clone());
    let session = s.owner_session.clone();
    let pressed = s.press(&session, "give", &subject.id, "founder", None).await;
    let stored = s.harness.db.fetch_interaction(&pressed.0).await.unwrap();
    assert_eq!(stored.command_version.as_deref(), Some(s.command.version.as_str()));

    // ... then puts the honest command back before handing the press over
    let honest = offered_choices();
    let (status, synced) = s
        .sync(json!([{
            "name": "badge",
            "description": "Give or take a badge",
            "options": [sub("give", &honest, "User"), sub("remove", &honest, "User")]
        }]))
        .await;
    assert_eq!(status, Status::Ok, "{synced}");
    assert_eq!(synced[0]["_id"], s.command.id.as_str());
    assert_ne!(synced[0]["version"], s.command.version.as_str());

    let (status, body) = s.hand("grant", &pressed).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert!(!s.holds(&subject, "founder").await);
    assert_eq!(
        s.audit_full(&subject).await,
        vec![(
            "badge.command.grant".to_string(),
            owner.id.clone(),
            "Denied".to_string(),
            "founder (command not as offered)".to_string()
        )]
    );

    // a press on the honest command works
    let command: v0::ApplicationCommand = serde_json::from_value(synced[0].clone()).unwrap();
    s.command = command;
    let pressed = s.press(&session, "give", &subject.id, "founder", None).await;
    let (status, outcome) = s.hand("grant", &pressed).await;
    assert_eq!((status, outcome["result"].as_str()), (Status::Ok, Some("done")), "{outcome}");
}

#[rocket::async_test]
async fn the_command_registers_with_manage_server_as_the_bot_sends_it() {
    let s = stage().await;
    let honest = offered_choices();
    let (status, synced) = s
        .sync(json!([{
            "type": "ChatInput",
            "name": "badge",
            "description": "Give or take a Zrzur badge (owner only)",
            "default_member_permissions": 2,
            "options": [sub("give", &honest, "User"), sub("remove", &honest, "User")]
        }]))
        .await;
    assert_eq!(status, Status::Ok, "{synced}");
    assert_eq!(synced[0]["default_member_permissions"], 2);
}

#[rocket::async_test]
async fn a_badge_press_must_say_which_version_it_saw() {
    let s = stage().await;
    let send = |application: String, command: String, options: Value| {
        s.harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", s.owner_session.clone()))
            .header(ContentType::JSON)
            .body(
                json!({
                    "type": "Command",
                    "application_id": application,
                    "channel_id": s.channel.id(),
                    "command_id": command,
                    "options": options,
                    "nonce": ulid::Ulid::new().to_string(),
                })
                .to_string(),
            )
    };
    // `/badge` of the badge bot without `command_version`: refused like a changed command
    let options = json!([{ "type": "SubCommand", "name": "give", "options": [
        { "type": "User", "name": "user", "value": s.subject.id },
        { "type": "String", "name": "badge", "value": "partner" }
    ]}]);
    let status = send(s.bot.id.clone(), s.command.id.clone(), options).dispatch().await.status();
    assert_eq!(status, Status::BadRequest);

    // another bot's command without it still works
    let (other_bot, other_user) = s.harness.new_bot(&s.owner).await;
    Member::create(&s.harness.db, &s.server, &other_user, None).await.unwrap();
    revolt_presence::create_session(&other_bot.id, 0).await;
    let command = revolt_database::create_application_command(
        &s.harness.db,
        &other_bot.id,
        None,
        serde_json::from_value(json!({ "name": "badge", "description": "Something else" })).unwrap(),
    )
    .await
    .unwrap();
    let status = send(other_bot.id.clone(), command.id.clone(), json!([])).dispatch().await.status();
    assert_eq!(status, Status::Ok);
}
