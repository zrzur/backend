//! Desk moderation: every change goes through a confirmation card and runs
//! as the console's own core, writing the console's records on the desk
//! session; refused without the permission; irreversible cards need a
//! fresh step-up.

use revolt_config::FeaturesStaffDesk;
use revolt_database::{
    Interaction, InteractionSource, InteractionStatus, InteractionTransition, Member, StaffRole,
    User,
};
use revolt_models::v0::{ApplicationCommand, EphemeralMessage, InteractionType};
use rocket::http::{ContentType, Header, Status};
use serde_json::{Value, json};

use super::run::CONFIRM_PREFIX;
use super::session::STEP_UP_MODAL;
use super::test_settings;
use super::tests::{Kind, Person, Stage, code, error_type, stage};

pub(super) fn all_areas(s: &Stage) {
    test_settings::register(FeaturesStaffDesk {
        enabled: true,
        bot: s.bot.id.clone(),
        places: vec![s.place.id().to_string()],
        allow_dms: false,
        areas: [
            "overview",
            "lookup",
            "audit",
            "moderation",
            "support",
            "review",
            "servers",
            "billing",
            "settings",
            "staff",
        ]
        .map(str::to_string)
        .to_vec(),
    });
}

async fn register_offered(s: &Stage) -> ApplicationCommand {
    // moderation is its own command
    super::tests::register_served(s)
        .await
        .remove("desk-mod")
        .unwrap()
}

pub(super) async fn press(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    group: &str,
    name: &str,
    options: Value,
) -> (String, String) {
    let (mut interaction, token) = Interaction::new(
        InteractionType::Command,
        s.bot.id.clone(),
        who.user.id.clone(),
        Some(s.server_id.clone()),
        s.place.id().to_string(),
        ulid::Ulid::new().to_string(),
    );
    interaction.session_id = Some(who.session.id.clone());
    interaction.command_id = Some(command.id.clone());
    interaction.command_name = Some(command.name.clone());
    interaction.command_version = Some(command.version.clone());
    interaction.command_options = serde_json::from_value(json!([{
        "type": "SubCommandGroup", "name": group,
        "options": [{ "type": "SubCommand", "name": name, "options": options }]
    }]))
    .unwrap();
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    (interaction.id, token)
}

/// A card the bot showed exactly as served, ready to confirm
pub(super) struct Card {
    pub(super) run: String,
    pub(super) message: String,
    pub(super) view: Value,
}

impl Card {
    pub(super) fn id(&self) -> String {
        self.view["buttons"][0]["action_id"]
            .as_str()
            .unwrap()
            .strip_prefix(CONFIRM_PREFIX)
            .unwrap()
            .to_string()
    }

    pub(super) fn danger(&self) -> bool {
        self.view["buttons"][0]["style"] == json!("danger")
    }
}

/// `/desk <group> <name>`: the card, shown by the bot as served
pub(super) async fn card(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    group: &str,
    name: &str,
    options: Value,
) -> Card {
    let run = press(s, who, command, group, name, options).await;
    let (status, view) = s.hand("run", &run).await;
    assert_eq!(status, Status::Ok, "{group} {name}: {view}");
    let id = view["buttons"][0]["action_id"]
        .as_str()
        .unwrap()
        .to_string();
    let style = if view["buttons"][0]["style"] == json!("danger") {
        "Danger"
    } else {
        "Primary"
    };
    let message: EphemeralMessage = serde_json::from_value(json!({
        "_id": ulid::Ulid::new().to_string(),
        "interaction_id": run.0,
        "author": s.bot.id,
        "channel_id": s.place.id(),
        "content": view["content"],
        "components": [{ "type": "ActionRow", "components": [{
            "type": "Button", "style": style, "label": view["buttons"][0]["label"], "custom_id": id
        }]}],
    }))
    .unwrap();
    let message_id = message.id.clone();
    assert!(
        s.harness
            .db
            .transition_interaction(
                &run.0,
                &[InteractionStatus::Pending],
                InteractionStatus::Responded,
                InteractionTransition {
                    ephemeral: Some(true),
                    ephemeral_message: Some(message),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    );
    Card {
        run: run.0,
        message: message_id,
        view,
    }
}

pub(super) async fn confirm(s: &Stage, who: &Person, card: &Card) -> (Status, Value) {
    let (mut interaction, token) = Interaction::new(
        InteractionType::Component,
        s.bot.id.clone(),
        who.user.id.clone(),
        Some(s.server_id.clone()),
        s.place.id().to_string(),
        ulid::Ulid::new().to_string(),
    );
    interaction.session_id = Some(who.session.id.clone());
    interaction.custom_id = Some(format!("{CONFIRM_PREFIX}{}", card.id()));
    interaction.source = Some(InteractionSource::Ephemeral {
        interaction_id: card.run.clone(),
        message_id: card.message.clone(),
    });
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    s.hand("confirm", &(interaction.id, token)).await
}

pub(super) async fn step_up(s: &Stage, who: &Person, offset: i64) {
    let code = code(offset);
    let pressed = s.press(who, Kind::Modal(STEP_UP_MODAL, &code)).await;
    let (status, body) = s.hand("step-up", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
}

pub(super) async fn as_user(s: &Stage, session: &str, path: &str, body: Value) -> Value {
    let response = s
        .harness
        .client
        .post(path.to_string())
        .header(Header::new("x-session-token", session.to_string()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok, "{path}");
    response.into_json().await.unwrap()
}

/// A report about a fresh message of a fresh author; gives (case id, author)
async fn reported(s: &Stage) -> (String, User) {
    let (case, author, _) = reported_message(s).await;
    (case, author)
}

/// The same, with the reported message's id
async fn reported_message(s: &Stage) -> (String, User, String) {
    let server = s.harness.db.fetch_server(&s.server_id).await.unwrap();
    let (_, reporter_session, reporter) = s.harness.new_user().await;
    let (_, _, author) = s.harness.new_user().await;
    let (_, _, message) = s
        .harness
        .new_message(&author, &server, vec![s.place.clone()])
        .await;
    Member::create(&s.harness.db, &server, &reporter, None)
        .await
        .unwrap();
    let report = as_user(
        &s,
        &reporter_session.token,
        "/safety/reports",
        json!({
            "target": { "kind": "Message", "id": message.id },
            "category": "Harassment", "subcategory": "targeted", "description": "keeps insulting me"
        }),
    )
    .await;
    (
        report["_id"].as_str().unwrap().to_string(),
        author,
        message.id,
    )
}

/// The console's record, Success, on the presser's desk session
pub(super) async fn recorded(s: &Stage, who: &Person, action: &str) {
    let session = s.desk_sessions(who).await.remove(0);
    let found =
        s.record_sessions(who).await.into_iter().any(|r| {
            r.0 == action && r.1 == "Success" && r.2.as_deref() == Some(session.id.as_str())
        });
    assert!(
        found,
        "no Success {action} on the desk session: {:?}",
        s.records(who).await
    );
}

pub(super) fn user_opt(id: &str) -> Value {
    json!({ "type": "User", "name": "user", "value": id })
}

pub(super) fn text(name: &str, value: &str) -> Value {
    json!({ "type": "String", "name": name, "value": value })
}

/// Every moderation command: the card, the confirmation, the console's own
/// record on the desk session, and the change visible in the console
#[rocket::async_test]
async fn desk_moderation_runs_the_consoles_actions() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_offered(&s).await;
    let console = console_token(&s, &owner, 0).await;

    // ---- reversible: card, then confirm, one after the other ----
    let (_, warned_session, warned) = s.harness.new_user().await;
    let (_, _, warned_again) = s.harness.new_user().await;
    let c = card(
        &s,
        &owner,
        &command,
        "user",
        "warn",
        json!([
            user_opt(&warned.id),
            text("category", "Spam"),
            text("reason", "Please stop sending spam")
        ]),
    )
    .await;
    assert!(!c.danger());
    assert!(
        c.view["content"].as_str().unwrap().contains(&warned.id),
        "{}",
        c.view
    );
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.user.action").await;
    let action_id = body["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("ID"))
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .to_string();

    // the same warning through the console gives the same kind of action
    let response = s
        .harness
        .client
        .post(format!("/staff/users/{}/actions", warned_again.id))
        .header(Header::new("x-session-token", owner.session.token.clone()))
        .header(Header::new("x-staff-session", console.clone()))
        .header(ContentType::JSON)
        .body(json!({ "kind": "Warning", "category": "Spam", "public_reason": "Please stop sending spam" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let from_console: Value = response.into_json().await.unwrap();
    let kind = body["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("Kind"))
        .unwrap()["value"]
        .clone();
    assert_eq!(kind, from_console["kind"]);

    // the person appeals; the decision comes with the irreversible cards
    let appeal = as_user(
        &s,
        &warned_session.token,
        &format!("/safety/standing/{action_id}/appeal"),
        json!({ "statement": "I did not send any spam at all." }),
    )
    .await;
    let appeal_id = appeal["_id"].as_str().unwrap().to_string();

    // a report worked on from the desk
    let (case_id, _) = reported(&s).await;
    for (name, options, record) in [
        (
            "status",
            json!([text("case", &case_id), text("status", "UnderReview")]),
            "staff.safety.case.status",
        ),
        (
            "note",
            json!([
                text("case", &case_id),
                text("text", "Looked at the history.")
            ]),
            "staff.safety.case.note",
        ),
        (
            "assign",
            json!([text("case", &case_id), user_opt(&owner.user.id)]),
            "staff.safety.case.edit",
        ),
        (
            "priority",
            json!([text("case", &case_id), text("priority", "High")]),
            "staff.safety.case.edit",
        ),
    ] {
        let c = card(&s, &owner, &command, "case", name, options).await;
        let (status, body) = confirm(&s, &owner, &c).await;
        assert_eq!(status, Status::Ok, "case {name}: {body}");
        recorded(&s, &owner, record).await;
    }
    let response = s
        .harness
        .client
        .get(format!("/staff/safety/cases/{case_id}"))
        .header(Header::new("x-session-token", owner.session.token.clone()))
        .header(Header::new("x-staff-session", console.clone()))
        .dispatch()
        .await;
    let details: Value = response.into_json().await.unwrap();
    assert_eq!(details["case"]["priority"], json!("High"), "{details}");
    assert_eq!(
        details["case"]["assignee_id"],
        json!(owner.user.id),
        "{details}"
    );

    // a card made before another change of the same case is refused by the
    // console's version check, and nothing changes
    let stale = card(
        &s,
        &owner,
        &command,
        "case",
        "priority",
        json!([text("case", &case_id), text("priority", "Low")]),
    )
    .await;
    let c = card(
        &s,
        &owner,
        &command,
        "case",
        "priority",
        json!([text("case", &case_id), text("priority", "Urgent")]),
    )
    .await;
    assert_eq!(confirm(&s, &owner, &c).await.0, Status::Ok);
    let (status, _) = confirm(&s, &owner, &stale).await;
    assert_ne!(status, Status::Ok);

    // an action on a report, then revoked
    let (case2, _) = reported(&s).await;
    let c = card(
        &s,
        &owner,
        &command,
        "case",
        "action",
        json!([
            text("case", &case2),
            text("kind", "Warning"),
            text("reason", "Insults are not allowed here")
        ]),
    )
    .await;
    assert!(!c.danger(), "a warning can be undone");
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.safety.action.take").await;
    let taken = body["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("ID"))
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .to_string();
    let c = card(
        &s,
        &owner,
        &command,
        "action",
        "revoke",
        json!([text("action", &taken), text("reason", "Taken by mistake")]),
    )
    .await;
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.safety.action.revoke").await;

    // ---- irreversible: every card first, one step-up after them ----
    let (_, _, to_suspend) = s.harness.new_user().await;
    let (_, _, to_ban) = s.harness.new_user().await;
    let (_, _, signed_out) = s.harness.new_user().await;
    let (_, _, bot_owner) = s.harness.new_user().await;
    let (bot, _) = s.harness.new_bot(&bot_owner).await;
    let suspend = card(
        &s,
        &owner,
        &command,
        "user",
        "suspend",
        json!([
            user_opt(&to_suspend.id),
            text("category", "Spam"),
            json!({ "type": "Integer", "name": "days", "value": 3 }),
            text("reason", "Repeated spam after a warning")
        ]),
    )
    .await;
    let ban = card(
        &s,
        &owner,
        &command,
        "user",
        "ban",
        json!([
            user_opt(&to_ban.id),
            text("category", "ScamFraud"),
            text("reason", "Scamming other members")
        ]),
    )
    .await;
    let sessions = card(
        &s,
        &owner,
        &command,
        "user",
        "sessions-end",
        json!([
            user_opt(&signed_out.id),
            text("reason", "The account was taken over")
        ]),
    )
    .await;
    let disable = card(
        &s,
        &owner,
        &command,
        "bot",
        "disable",
        json!([
            text("bot", &bot.id),
            text("category", "BotAbuse"),
            text("reason", "The bot floods channels")
        ]),
    )
    .await;
    let decide = card(
        &s,
        &owner,
        &command,
        "appeal",
        "decide",
        json!([
            text("appeal", &appeal_id),
            text("decision", "overturn"),
            text("note", "We checked again and agree.")
        ]),
    )
    .await;
    let hold = card(
        &s,
        &owner,
        &command,
        "case",
        "hold",
        json!([
            text("case", &case2),
            json!({ "type": "Boolean", "name": "on", "value": true })
        ]),
    )
    .await;
    for c in [&suspend, &ban, &sessions, &disable, &decide, &hold] {
        assert!(c.danger(), "{}", c.view);
        let (status, body) = confirm(&s, &owner, c).await;
        assert_eq!(
            (status, error_type(&body)),
            (Status::Forbidden, "NotElevated"),
            "{body}"
        );
    }
    step_up(&s, &owner, 1).await;
    for (c, record) in [
        (&suspend, "staff.user.action"),
        (&ban, "staff.user.action"),
        (&sessions, "staff.user.revoke_sessions"),
        (&disable, "staff.bot.disable"),
        (&hold, "staff.safety.case.edit"),
        (&decide, "staff.safety.appeal.decide"),
    ] {
        let (status, body) = confirm(&s, &owner, c).await;
        assert_eq!(status, Status::Ok, "{record}: {body}");
        recorded(&s, &owner, record).await;
    }
    let banned = s.harness.db.fetch_user(&to_ban.id).await.unwrap();
    assert_ne!(
        banned.flags.unwrap_or_default() & revolt_models::v0::UserFlags::Banned as i32,
        0,
        "the ban happened"
    );
    assert!(
        s.harness
            .db
            .fetch_sessions(&signed_out.id)
            .await
            .unwrap()
            .is_empty()
    );
}

pub(super) async fn console_token(s: &Stage, who: &Person, offset: i64) -> String {
    let response = s
        .harness
        .client
        .post("/staff/session")
        .header(Header::new("x-session-token", who.session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "totp_code": code(offset) }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let body: Value = response.into_json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

/// Without the permission, no card is made: refused as the presser, with
/// the console's Denied record
#[rocket::async_test]
async fn desk_moderation_refused_without_permission() {
    let s = stage().await;
    all_areas(&s);
    let analyst = s.person(vec![StaffRole::Analyst]).await;
    s.sign_in(&analyst, -1).await;
    let command = register_offered(&s).await;
    let entries: Vec<_> = super::registry::entries()
        .filter(|e| e.area == Some("moderation"))
        .collect();
    assert_eq!(entries.len(), 13);
    for entry in entries {
        let before = s.records(&analyst).await.len();
        let pressed = press(
            &s,
            &analyst,
            &command,
            entry.group.unwrap(),
            entry.name,
            json!([user_opt(&analyst.user.id)]),
        )
        .await;
        let (status, body) = s.hand("run", &pressed).await;
        assert_eq!(status, Status::Forbidden, "{}: {body}", entry.key());
        assert_eq!(
            error_type(&body),
            "MissingPlatformPermission",
            "{}",
            entry.key()
        );
        let records = s.records(&analyst).await;
        assert_eq!(records.len(), before + 1, "{}", entry.key());
        assert_eq!(
            (records[before].0.as_str(), records[before].1.as_str()),
            ("staff.access.denied", "Denied")
        );
    }
    assert!(
        s.harness
            .db
            .fetch_safety_actions_for_user(&analyst.user.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Suspending needs a step-up made after its card; a warning in the same
/// command family does not; an account action on a report is Danger
#[rocket::async_test]
async fn desk_moderation_irreversible_needs_fresh_step_up() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_offered(&s).await;
    let (_, _, someone) = s.harness.new_user().await;

    // a step-up made before the card does not count
    step_up(&s, &owner, 0).await;
    let c = card(
        &s,
        &owner,
        &command,
        "user",
        "suspend",
        json!([
            user_opt(&someone.id),
            text("category", "Spam"),
            json!({ "type": "Integer", "name": "days", "value": 1 }),
            text("reason", "Spam again and again")
        ]),
    )
    .await;
    assert!(c.danger());
    let (_, body) = confirm(&s, &owner, &c).await;
    assert_eq!(error_type(&body), "NotElevated", "{body}");
    assert!(
        s.harness
            .db
            .fetch_safety_actions_for_user(&someone.id)
            .await
            .unwrap()
            .is_empty()
    );
    let pending = s
        .harness
        .db
        .fetch_desk_pending(&c.id())
        .await
        .unwrap()
        .unwrap();
    assert!(!pending.used && pending.irreversible);

    // a ban on a report is Danger, a warning on the same report is not
    let (case_id, _) = reported(&s).await;
    let ban = card(
        &s,
        &owner,
        &command,
        "case",
        "action",
        json!([
            text("case", &case_id),
            text("kind", "BanAccount"),
            text("reason", "Scam links everywhere")
        ]),
    )
    .await;
    assert!(ban.danger());
    assert!(
        ban.view["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap() == super::registry::texts::FRESH_CODE.en)
    );
    let warn = card(
        &s,
        &owner,
        &command,
        "case",
        "action",
        json!([
            text("case", &case_id),
            text("kind", "Warning"),
            text("reason", "Mind your words")
        ]),
    )
    .await;
    assert!(!warn.danger());

    // after a fresh step-up the suspension runs
    step_up(&s, &owner, 1).await;
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(
        s.harness
            .db
            .fetch_safety_actions_for_user(&someone.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// Sets who a case names as the target's owner, as an old or public
/// report may
async fn name_owner(s: &Stage, case_id: &str, owner: Option<String>) {
    let mut case = s.harness.db.fetch_safety_case(case_id).await.unwrap();
    let version = case.version;
    case.target_owner_id = owner;
    case.version = version + 1;
    assert!(
        s.harness
            .db
            .update_safety_case(&case, version)
            .await
            .unwrap()
    );
}

async fn run_card(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    group: &str,
    name: &str,
    options: Value,
) -> (Status, Value) {
    let pressed = press(s, who, command, group, name, options).await;
    s.hand("run", &pressed).await
}

/// The case action card names the account the action will really concern,
/// resolved as the action resolves it: a report naming nobody, or naming
/// the bot that posted the message, shows the message's author; when the
/// account changes between the card and the press, the press is refused
#[rocket::async_test]
async fn desk_case_action_names_the_real_subject() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_offered(&s).await;
    let warn = |case: &str| {
        json!([
            text("case", case),
            text("kind", "Warning"),
            text("reason", "Mind your words")
        ])
    };

    // a report naming nobody yet
    let (nobody_case, author, message_id) = reported_message(&s).await;
    name_owner(&s, &nobody_case, None).await;
    let c = card(&s, &owner, &command, "case", "action", warn(&nobody_case)).await;
    assert!(
        c.view["content"].as_str().unwrap().contains(&author.id),
        "{}",
        c.view
    );

    // a report naming the bot that posted the message
    let (bot_case, bot_author) = reported(&s).await;
    name_owner(&s, &bot_case, Some(s.bot.id.clone())).await;
    let c2 = card(&s, &owner, &command, "case", "action", warn(&bot_case)).await;
    let content = c2.view["content"].as_str().unwrap();
    assert!(
        content.contains(&bot_author.id) && !content.contains(&s.bot.id),
        "{content}"
    );

    // the message goes away: the action would now concern nobody, so the
    // card no longer says what would happen
    s.harness.db.delete_message(&message_id).await.unwrap();
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(
        (status, error_type(&body)),
        (Status::BadRequest, "InvalidOperation"),
        "{body}"
    );
    let records = s.records(&owner).await;
    assert!(
        records
            .iter()
            .any(|r| r.1 == "Denied" && r.2 == "desk: the report now concerns another account"),
        "{records:?}"
    );
    assert!(
        s.harness
            .db
            .fetch_safety_actions_for_user(&author.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The cards refuse at once what the console would refuse anyway, ask for
/// days on a suspension, never cut the text, and say only what is true:
/// a removed message cannot be undone; disabling a bot needs a fresh code
/// either way; a ban lasts until revoked; an appeal card names the appealed
/// action; choices show their own names
#[rocket::async_test]
async fn desk_moderation_cards_are_honest() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let other_staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&owner, -1).await;
    let command = register_offered(&s).await;
    let warn = |user: &str| {
        json!([
            user_opt(user),
            text("category", "Spam"),
            text("reason", "Please stop that")
        ])
    };

    // yourself, a bot, another staff member
    let (_, body) = run_card(&s, &owner, &command, "user", "warn", warn(&owner.user.id)).await;
    assert_eq!(error_type(&body), "CannotReportYourself", "{body}");
    let (_, body) = run_card(&s, &owner, &command, "user", "warn", warn(&s.bot.id)).await;
    assert_eq!(error_type(&body), "IsBot", "{body}");
    let (_, body) = run_card(
        &s,
        &owner,
        &command,
        "user",
        "warn",
        warn(&other_staff.user.id),
    )
    .await;
    assert_eq!(error_type(&body), "NotPrivileged", "{body}");
    let (_, body) = run_card(
        &s,
        &owner,
        &command,
        "user",
        "sessions-end",
        json!([user_opt(&s.bot.id), text("reason", "test it")]),
    )
    .await;
    assert_eq!(error_type(&body), "IsBot", "{body}");

    // a text that would be cut is refused, not cut
    let (_, _, someone) = s.harness.new_user().await;
    let long = "*".repeat(500);
    let (status, body) = run_card(
        &s,
        &owner,
        &command,
        "user",
        "warn",
        json!([
            user_opt(&someone.id),
            text("category", "Spam"),
            text("reason", &long),
            text("note", &long)
        ]),
    )
    .await;
    assert_eq!(
        (status, error_type(&body)),
        (Status::BadRequest, "FailedValidation"),
        "{body}"
    );

    // a suspension on a report says for how long
    let (case_id, _) = reported(&s).await;
    let (_, body) = run_card(
        &s,
        &owner,
        &command,
        "case",
        "action",
        json!([
            text("case", &case_id),
            text("kind", "SuspendAccount"),
            text("reason", "Spam again")
        ]),
    )
    .await;
    assert_eq!(error_type(&body), "FailedValidation", "{body}");

    let lines_of = |c: &Card| -> Vec<String> {
        c.view["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect()
    };
    let permanent = super::registry::texts::PERMANENT.en.to_string();
    let fresh = super::registry::texts::FRESH_CODE.en.to_string();

    // removing a message: Danger, a fresh code, and it cannot be undone
    let c = card(
        &s,
        &owner,
        &command,
        "case",
        "action",
        json!([
            text("case", &case_id),
            text("kind", "RemoveMessage"),
            text("reason", "Not allowed here")
        ]),
    )
    .await;
    assert!(c.danger());
    assert!(
        lines_of(&c).contains(&permanent) && lines_of(&c).contains(&fresh),
        "{:?}",
        lines_of(&c)
    );
    let (_, body) = confirm(&s, &owner, &c).await;
    assert_eq!(error_type(&body), "NotElevated", "{body}");

    // disabling a bot: Danger and a fresh code, from a report or directly,
    // and it can be undone
    let c = card(
        &s,
        &owner,
        &command,
        "case",
        "action",
        json!([
            text("case", &case_id),
            text("kind", "DisableBot"),
            text("reason", "Bot floods")
        ]),
    )
    .await;
    assert!(c.danger());
    assert!(
        !lines_of(&c).contains(&permanent) && lines_of(&c).contains(&fresh),
        "{:?}",
        lines_of(&c)
    );
    let (_, _, bot_owner) = s.harness.new_user().await;
    let (bot, _) = s.harness.new_bot(&bot_owner).await;
    let c = card(
        &s,
        &owner,
        &command,
        "bot",
        "disable",
        json!([
            text("bot", &bot.id),
            text("category", "BotAbuse"),
            text("reason", "The bot floods channels")
        ]),
    )
    .await;
    assert!(c.danger());
    assert!(
        !lines_of(&c).contains(&permanent) && lines_of(&c).contains(&fresh),
        "{:?}",
        lines_of(&c)
    );
    let (_, body) = confirm(&s, &owner, &c).await;
    assert_eq!(error_type(&body), "NotElevated", "{body}");

    // the ban lasts until it is revoked
    let c = card(
        &s,
        &owner,
        &command,
        "user",
        "ban",
        json!([
            user_opt(&someone.id),
            text("category", "Spam"),
            text("reason", "Spam everywhere")
        ]),
    )
    .await;
    assert!(
        c.view["content"]
            .as_str()
            .unwrap()
            .contains("until the ban is revoked"),
        "{}",
        c.view
    );

    // choices by their own names; the appeal card names what is appealed
    let (_, warned_session, warned) = s.harness.new_user().await;
    let c = card(
        &s,
        &owner,
        &command,
        "user",
        "warn",
        json!([
            user_opt(&warned.id),
            text("category", "HateOrThreats"),
            text("reason", "Threats are not allowed")
        ]),
    )
    .await;
    assert!(
        c.view["content"]
            .as_str()
            .unwrap()
            .contains("Hate or threats"),
        "{}",
        c.view
    );
    let (status, done) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{done}");
    let action_id = done["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("ID"))
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .to_string();
    let appeal = as_user(
        &s,
        &warned_session.token,
        &format!("/safety/standing/{action_id}/appeal"),
        json!({ "statement": "I never threatened anyone at all." }),
    )
    .await;
    let c = card(
        &s,
        &owner,
        &command,
        "appeal",
        "decide",
        json!([
            text("appeal", appeal["_id"].as_str().unwrap()),
            text("decision", "uphold"),
            text("note", "The messages are clear.")
        ]),
    )
    .await;
    let content = c.view["content"].as_str().unwrap();
    assert!(
        content.contains(&action_id) && content.contains("Warning") && content.contains(&warned.id),
        "{content}"
    );
}
