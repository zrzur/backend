//! The generic desk: the command tree, `/run` and pending actions with
//! `/confirm`. The bot hands back presses; the server decides everything.

use revolt_database::{
    DeskPending, Interaction, InteractionSource, InteractionStatus, InteractionTransition,
    StaffPermission, StaffRole, staff_now,
};
use revolt_models::v0::{
    ApplicationCommand, CommandOptionType, EphemeralMessage, InteractionDataOption,
    InteractionOptionValue, InteractionType,
};
use revolt_result::create_error;
use rocket::http::{ContentType, Header, Status};
use serde_json::{Value, json};

use super::super::StaffContext;
use super::registry::{self, DeskInput, Lang, test_entries, texts};
use super::run::{CONFIRM_PREFIX, execute};
use super::session::STEP_UP_MODAL;
use super::test_settings;
use super::tests::{Kind, Person, Stage, code, error_type, settings, stage};

const CONFIRM: &str = texts::CONFIRM.en;

/// The bot asks for the tree
async fn tree(s: &Stage, token: &str) -> (Status, Value) {
    let response = s
        .harness
        .client
        .get("/staff/desk/commands")
        .header(Header::new("x-bot-token", token.to_string()))
        .dispatch()
        .await;
    let status = response.status();
    let served: Value = response.into_json().await.unwrap_or(Value::Null);
    // the `/desk` command of the served list
    let desk = served
        .as_array()
        .and_then(|all| all.iter().find(|t| t["name"] == json!("desk")).cloned())
        .unwrap_or(served);
    (status, desk)
}

/// The bot registers `tree` as its `/desk`
async fn register(s: &Stage, tree: Value) -> ApplicationCommand {
    revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        None,
        serde_json::from_value(tree).unwrap(),
    )
    .await
    .unwrap()
}

/// The bot registers exactly what the server offers
async fn register_offered(s: &Stage) -> ApplicationCommand {
    super::tests::register_served(s)
        .await
        .remove("desk")
        .unwrap()
}

/// A `/desk` press on `command` as `/interactions` stores it (options, the
/// version the person saw, the client's locale); `options` is the
/// top-level option list
async fn desk_press_in(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    options: Value,
    locale: Option<&str>,
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
    interaction.command_options = serde_json::from_value(options).unwrap();
    interaction.locale = locale.map(str::to_string);
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    (interaction.id, token)
}

async fn desk_press(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    options: Value,
) -> (String, String) {
    desk_press_in(s, who, command, options, None).await
}

fn sub(name: &str) -> Value {
    json!([{ "type": "SubCommand", "name": name }])
}

fn test_sub(name: &str) -> Value {
    json!([{ "type": "SubCommandGroup", "name": "test", "options": [{ "type": "SubCommand", "name": name }] }])
}

/// `/desk test <name> user:<user> note:<note>`
fn test_change(name: &str, user: &str, note: &str) -> Value {
    json!([{ "type": "SubCommandGroup", "name": "test", "options": [{
        "type": "SubCommand", "name": name, "options": [
            { "type": "User", "name": "user", "value": user },
            { "type": "String", "name": "note", "value": note }
        ]
    }]}])
}

/// The press goes to `/staff/desk/<path>` with these extra body fields
/// (which the server must ignore)
async fn hand_extra(
    s: &Stage,
    path: &str,
    pressed: &(String, String),
    extra: Value,
) -> (Status, Value) {
    let mut body = json!({ "interaction_id": pressed.0, "token": pressed.1 });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    let response = s
        .harness
        .client
        .post(format!("/staff/desk/{path}"))
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = response.status();
    (status, response.into_json().await.unwrap_or(Value::Null))
}

/// What the bot shows: the server's text
fn rendered(view: &Value) -> String {
    view["content"].as_str().unwrap().to_string()
}

/// The pending id in the view's Confirm button
fn pending_id(view: &Value) -> String {
    let buttons = view["buttons"].as_array().unwrap();
    assert_eq!(buttons.len(), 1, "{view}");
    buttons[0]["action_id"]
        .as_str()
        .unwrap()
        .strip_prefix(CONFIRM_PREFIX)
        .unwrap()
        .to_string()
}

fn card_message(
    s: &Stage,
    run: &(String, String),
    content: &str,
    buttons: Value,
) -> EphemeralMessage {
    serde_json::from_value(json!({
        "_id": ulid::Ulid::new().to_string(),
        "interaction_id": run.0,
        "author": s.bot.id,
        "channel_id": s.place.id(),
        "content": content,
        "components": [{ "type": "ActionRow", "components": buttons }],
    }))
    .unwrap()
}

/// The bot answers the `/run` press privately with `content` and
/// `buttons`; gives the message id
async fn answer_card(s: &Stage, run: &(String, String), content: &str, buttons: Value) -> String {
    let message = card_message(s, run, content, buttons);
    let id = message.id.clone();
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
    id
}

fn button(id: &str, style: &str, label: &str) -> Value {
    json!([{ "type": "Button", "style": style, "label": label, "custom_id": format!("{CONFIRM_PREFIX}{id}") }])
}

fn confirm_button(id: &str) -> Value {
    button(id, "Danger", CONFIRM)
}

/// `who` presses `zd:confirm:<id>` on the private message `message_id` of
/// the interaction `card_of`
async fn press_confirm(
    s: &Stage,
    who: &Person,
    id: &str,
    card_of: &str,
    message_id: &str,
) -> (String, String) {
    let (mut interaction, token) = Interaction::new(
        InteractionType::Component,
        s.bot.id.clone(),
        who.user.id.clone(),
        Some(s.server_id.clone()),
        s.place.id().to_string(),
        ulid::Ulid::new().to_string(),
    );
    interaction.session_id = Some(who.session.id.clone());
    interaction.custom_id = Some(format!("{CONFIRM_PREFIX}{id}"));
    interaction.source = Some(InteractionSource::Ephemeral {
        interaction_id: card_of.to_string(),
        message_id: message_id.to_string(),
    });
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    (interaction.id, token)
}

/// A step-up with the code `offset` steps from now
async fn step_up(s: &Stage, who: &Person, offset: i64) {
    let code = code(offset);
    let pressed = s.press(who, Kind::Modal(STEP_UP_MODAL, &code)).await;
    let (status, body) = s.hand("step-up", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
}

/// A signed-in Owner proposes `/desk test <name>` about themselves; the
/// bot shows the card exactly. Gives (the /run press, its view, the card
/// message id)
async fn proposed(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    name: &str,
    note: &str,
) -> ((String, String), Value, String) {
    let run = desk_press(s, who, command, test_change(name, &who.user.id, note)).await;
    let (status, view) = s.hand("run", &run).await;
    assert_eq!(status, Status::Ok, "{view}");
    let id = pending_id(&view);
    let style = if view["buttons"][0]["style"] == json!("danger") {
        "Danger"
    } else {
        "Primary"
    };
    let message = answer_card(s, &run, &rendered(&view), button(&id, style, CONFIRM)).await;
    (run, view, message)
}

async fn confirm_on(
    s: &Stage,
    who: &Person,
    view: &Value,
    run: &(String, String),
    message: &str,
) -> (Status, Value) {
    let pressed = press_confirm(s, who, &pending_id(view), &run.0, message).await;
    s.hand("confirm", &pressed).await
}

fn denied_summaries(records: &[(String, String, String)]) -> Vec<String> {
    records
        .iter()
        .filter(|r| r.1 == "Denied")
        .map(|r| r.2.clone())
        .collect()
}

async fn used(s: &Stage, id: &str) -> bool {
    s.harness
        .db
        .fetch_desk_pending(id)
        .await
        .unwrap()
        .unwrap()
        .used
}

/// The presser's own context, as the desk builds it
async fn context_of(s: &Stage, who: &Person) -> StaffContext {
    let user = s.harness.db.fetch_user(&who.user.id).await.unwrap();
    let session = s.desk_sessions(who).await.remove(0);
    let grant = s
        .harness
        .db
        .fetch_staff_grant(&who.user.id)
        .await
        .unwrap()
        .unwrap();
    StaffContext {
        permissions: grant.effective(staff_now()),
        user,
        session,
        grant,
        now: staff_now(),
        path: "/staff/desk/confirm".to_string(),
    }
}

// ---- the tree ----

/// The tree holds the always-offered sub-commands, and an area's commands
/// only while the area is on; only the desk's bot gets it
#[rocket::async_test]
async fn desk_commands_only_enabled_areas() {
    let s = stage().await;
    let (status, offered) = tree(&s, &s.bot.token).await;
    assert_eq!(status, Status::Ok, "{offered}");
    assert_eq!(offered["type"], json!("ChatInput"));
    assert_eq!(offered["name"], json!("desk"));
    assert!(offered["description_localizations"]["ar"].is_string());
    assert!(offered["description_localizations"]["de"].is_string());
    let names: Vec<&str> = offered["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["me", "login", "logout", "stepup", "stats", "test"]
    );
    let test = &offered["options"][5];
    assert_eq!(test["type"], json!("SubCommandGroup"));
    assert_eq!(test["options"][0]["name"], json!("change"));
    assert_eq!(test["options"][0]["options"][0]["type"], json!("User"));
    assert_eq!(test["options"][0]["options"][0]["required"], json!(true));

    // the overview area off: only what is always offered
    let mut off = settings(&s.bot, vec![s.place.id().to_string()], false);
    off.areas = vec![];
    test_settings::register(off);
    let (_, offered) = tree(&s, &s.bot.token).await;
    let names: Vec<&str> = offered["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["me", "login", "logout", "stepup"]);

    // a person, or another bot: the route does not exist for them
    let staff = s.person(vec![StaffRole::Owner]).await;
    let response = s
        .harness
        .client
        .get("/staff/desk/commands")
        .header(Header::new("x-session-token", staff.session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NotFound);
    let (_, _, host) = s.harness.new_user().await;
    let (other, _) = s.harness.new_bot(&host).await;
    assert_eq!(tree(&s, &other.token).await.0, Status::NotFound);
}

// ---- /run ----

/// A press on a command not registered exactly as offered is refused as
/// the presser, with a Denied record: a relabelled sub-command, an option
/// of another kind, an optional option made required (or the reverse),
/// added choices, options in another order, a press made on an earlier
/// version, a press without a version
#[rocket::async_test]
async fn desk_run_refuses_changed_command() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let (_, offered) = tree(&s, &s.bot.token).await;

    let change = |edit: &dyn Fn(&mut Value)| {
        let mut changed = offered.clone();
        edit(&mut changed);
        changed
    };
    let variants: Vec<(Value, Value)> = vec![
        // the bot shows "stats" as something harmless-looking
        (
            change(&|t| t["options"][4]["description"] = json!("Show the weather")),
            sub("stats"),
        ),
        // `user` as text: a name typed in, not a user the server resolved
        (
            change(&|t| t["options"][5]["options"][0]["options"][0]["type"] = json!("String")),
            test_change("change", &staff.user.id, "n"),
        ),
        // `note` no longer required
        (
            change(&|t| t["options"][5]["options"][0]["options"][1]["required"] = json!(false)),
            test_change("change", &staff.user.id, "n"),
        ),
        // choices the server never offered
        (
            change(&|t| {
                t["options"][5]["options"][0]["options"][1]["choices"] =
                    json!([{ "name": "Harmless", "value": "n" }])
            }),
            test_change("change", &staff.user.id, "n"),
        ),
        // the options swapped
        (
            change(&|t| {
                let options = t["options"][5]["options"][0]["options"]
                    .as_array_mut()
                    .unwrap();
                options.swap(0, 1);
            }),
            test_change("change", &staff.user.id, "n"),
        ),
    ];
    let mut refused = 0;
    let mut last_command = None;
    for (registered, press) in variants {
        if let Some(command) = last_command.take() {
            let command: ApplicationCommand = command;
            s.harness
                .db
                .delete_application_command(&command.id)
                .await
                .unwrap();
        }
        let command = register(&s, registered).await;
        let pressed = desk_press(&s, &staff, &command, press).await;
        let (status, body) = s.hand("run", &pressed).await;
        assert_eq!(error_type(&body), "InvalidOperation", "{status} {body}");
        refused += 1;
        last_command = Some(command);
    }
    let records = s.records(&staff).await;
    let last = records.last().unwrap();
    assert_eq!(
        (last.0.as_str(), last.1.as_str(), last.2.as_str()),
        (
            "staff.desk.command",
            "Denied",
            "desk: command not as offered"
        )
    );

    // honest again, but the press was made on the last changed version
    let changed = last_command.unwrap();
    s.harness
        .db
        .delete_application_command(&changed.id)
        .await
        .unwrap();
    let honest = register(&s, offered.clone()).await;
    let mut stale = honest.clone();
    stale.version = changed.version.clone();
    let pressed = desk_press(&s, &staff, &stale, sub("stats")).await;
    assert_eq!(
        error_type(&s.hand("run", &pressed).await.1),
        "InvalidOperation"
    );
    refused += 1;

    // no version stored at all
    let (mut copy, token) = Interaction::new(
        InteractionType::Command,
        s.bot.id.clone(),
        staff.user.id.clone(),
        Some(s.server_id.clone()),
        s.place.id().to_string(),
        ulid::Ulid::new().to_string(),
    );
    copy.session_id = Some(staff.session.id.clone());
    copy.command_id = Some(honest.id.clone());
    copy.command_name = Some("desk".to_string());
    copy.command_options = serde_json::from_value(sub("stats")).unwrap();
    s.harness.db.insert_interaction(&copy).await.unwrap();
    assert_eq!(
        error_type(&s.hand("run", &(copy.id.clone(), token)).await.1),
        "InvalidOperation"
    );
    refused += 1;

    // the honest command works
    let pressed = desk_press(&s, &staff, &honest, sub("stats")).await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
    let denied = denied_summaries(&s.records(&staff).await);
    assert_eq!(denied.len(), refused, "{denied:?}");
}

/// `/desk stats` through `/run` is the console's `stats_core` for a
/// DashboardRead holder, in the person's language; `/desk me` answers as a
/// DeskView; session sub-commands are not run here; without the command's
/// permission the console's own check refuses it
#[rocket::async_test]
async fn desk_run_calls_stats_for_dashboard_read() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;

    let pressed = desk_press(&s, &staff, &command, sub("stats")).await;
    let (status, view) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{view}");
    assert_eq!(view["title"], json!("Platform overview"));
    let fields: Vec<&str> = view["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(
        fields.contains(&"Users") && fields.contains(&"Open reports"),
        "{view}"
    );
    assert!(view["buttons"].as_array().unwrap().is_empty());
    assert!(
        view["content"]
            .as_str()
            .unwrap()
            .starts_with("**Platform overview**\nUsers: "),
        "{view}"
    );

    // in Arabic and German, from the stored locale
    let pressed = desk_press_in(&s, &staff, &command, sub("stats"), Some("ar")).await;
    let (_, view) = s.hand("run", &pressed).await;
    assert_eq!(view["title"], json!(texts::OVERVIEW.ar));
    assert_eq!(view["fields"][0]["name"], json!(texts::USERS.ar));
    let pressed = desk_press_in(&s, &staff, &command, sub("stats"), Some("de-AT")).await;
    let (_, view) = s.hand("run", &pressed).await;
    assert_eq!(view["title"], json!(texts::OVERVIEW.de));

    let pressed = desk_press(&s, &staff, &command, sub("me")).await;
    let (status, me) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{me}");
    let areas = me["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("Areas"))
        .unwrap();
    assert_eq!(areas["value"], json!("overview"));
    assert!(
        me["lines"]
            .as_array()
            .unwrap()
            .contains(&json!("/desk stats")),
        "{me}"
    );

    let pressed = desk_press(&s, &staff, &command, sub("logout")).await;
    assert_eq!(
        error_type(&s.hand("run", &pressed).await.1),
        "InvalidOperation"
    );

    // every role holds DashboardRead: the refusal is shown on the presser's
    // own context with the permission taken away
    let mut without = context_of(&s, &staff).await;
    without.permissions.remove(&StaffPermission::DashboardRead);
    let session = without.session.id.clone();
    let entry = registry::find("desk", None, "stats").unwrap();
    let error = registry::dispatch(&s.harness.db, &without, entry, &DeskInput::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&error.error_type, revolt_result::ErrorType::MissingPlatformPermission { permission } if permission == "platform.dashboard.read"),
        "{error:?}"
    );
    let last = s.record_sessions(&staff).await.pop().unwrap();
    assert_eq!(
        (last.0.as_str(), last.1.as_str()),
        ("staff.access.denied", "Denied")
    );
    assert_eq!(last.2.as_deref(), Some(session.as_str()));

    // a changing command never runs through dispatch
    let change = registry::find("desk", Some("test"), "change").unwrap();
    let full = context_of(&s, &staff).await;
    let error = registry::dispatch(&s.harness.db, &full, change, &DeskInput::default())
        .await
        .unwrap_err();
    assert!(matches!(
        error.error_type,
        revolt_result::ErrorType::InvalidOperation
    ));
    assert_eq!(test_entries::runs(""), 0);
}

/// Over HTTP: a Support person (no BillingRead) runs `/desk test peek`;
/// the console's check refuses it as them, on their desk session
#[rocket::async_test]
async fn desk_run_permission_refused_over_http() {
    let s = stage().await;
    let support = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&support, -1).await;
    let command = register_offered(&s).await;

    let pressed = desk_press(&s, &support, &command, test_sub("peek")).await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "MissingPlatformPermission");
    let session = s.desk_sessions(&support).await.remove(0);
    let last = s.record_sessions(&support).await.pop().unwrap();
    assert_eq!(
        (last.0.as_str(), last.1.as_str()),
        ("staff.access.denied", "Denied")
    );
    assert_eq!(last.2.as_deref(), Some(session.id.as_str()));

    // a Billing person holds BillingRead: the same command answers
    let billing = s.person(vec![StaffRole::Billing]).await;
    s.sign_in(&billing, -1).await;
    let pressed = desk_press(&s, &billing, &command, test_sub("peek")).await;
    assert_eq!(s.hand("run", &pressed).await.0, Status::Ok);
}

/// `/run` on a changing command changes nothing: it stores a pending action
/// from the stored interaction's options (never the bot's body) and answers
/// with a confirmation card
#[rocket::async_test]
async fn desk_options_only_from_stored_interaction() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;

    let run = desk_press(
        &s,
        &staff,
        &command,
        test_change("change", &staff.user.id, "stored note"),
    )
    .await;
    let (status, view) = hand_extra(
        &s,
        "run",
        &run,
        json!({ "options": [{ "type": "String", "name": "note", "value": "forged" }], "note": "forged", "user": "someone" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{view}");
    assert_eq!(view["title"], json!("Test change"));
    let id = pending_id(&view);
    let pending = s.harness.db.fetch_desk_pending(&id).await.unwrap().unwrap();
    assert_eq!(pending.actor, staff.user.id);
    assert_eq!(pending.action, "test.change");
    assert_eq!(pending.target.as_deref(), Some(staff.user.id.as_str()));
    assert_eq!(pending.card_interaction, run.0);
    assert!(!pending.used);
    let note = pending.options.iter().find(|o| o.name == "note").unwrap();
    assert_eq!(
        note.value,
        Some(InteractionOptionValue::String("stored note".to_string()))
    );
    assert!(
        view["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("stored note"))
    );
    // nothing was executed, nothing recorded as done
    assert!(
        s.records(&staff)
            .await
            .iter()
            .all(|r| r.0 != "staff.desk.test.change")
    );
    assert_eq!(test_entries::runs(&id), 0);

    // confirmed after a fresh step-up: the stored note, whatever the bot adds
    step_up(&s, &staff, 0).await;
    let message = answer_card(&s, &run, &rendered(&view), confirm_button(&id)).await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    let (status, done) = hand_extra(
        &s,
        "confirm",
        &pressed,
        json!({ "note": "forged", "pending_id": "other" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{done}");
    let note = done["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("Note"))
        .unwrap();
    assert_eq!(note["value"], json!("stored note"));
    assert_eq!(test_entries::runs(&id), 1);
}

/// The card and the answer come in the language of the command
#[rocket::async_test]
async fn desk_card_in_the_commands_language() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;

    let run = desk_press_in(
        &s,
        &staff,
        &command,
        test_change("change", &staff.user.id, "*x*"),
        Some("ar"),
    )
    .await;
    let (status, view) = s.hand("run", &run).await;
    assert_eq!(status, Status::Ok, "{view}");
    assert_eq!(view["title"], json!("تغيير تجريبي"));
    assert_eq!(view["buttons"][0]["label"], json!(texts::CONFIRM.ar));
    assert!(
        view["lines"]
            .as_array()
            .unwrap()
            .contains(&json!(texts::WITHIN.ar))
    );
    // the person's text cannot format the card
    assert!(
        view["content"].as_str().unwrap().contains(r"«\*x\*»"),
        "{view}"
    );

    step_up(&s, &staff, 0).await;
    let id = pending_id(&view);
    let message = answer_card(
        &s,
        &run,
        &rendered(&view),
        button(&id, "Danger", texts::CONFIRM.ar),
    )
    .await;
    let (status, done) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(status, Status::Ok, "{done}");
    assert_eq!(done["title"], json!("تم التغيير التجريبي"));
}

// ---- pending actions ----

/// Another person's press on the card is refused like an unknown one (410),
/// recorded as them, and leaves the pending action usable for its owner
#[rocket::async_test]
async fn desk_confirm_by_another_person_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    let other = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    s.sign_in(&other, -1).await;
    let command = register_offered(&s).await;
    let (run, view, message) = proposed(&s, &staff, &command, "change", "n").await;
    let id = pending_id(&view);

    let pressed = press_confirm(&s, &other, &id, &run.0, &message).await;
    let (status, body) = s.hand("confirm", &pressed).await;
    assert_eq!(status, Status::Gone, "{body}");
    assert_eq!(error_type(&body), "InteractionExpired");
    // an id that does not exist answers the same
    let pressed = press_confirm(&s, &other, "01NOPE", &run.0, &message).await;
    let (unknown_status, unknown) = s.hand("confirm", &pressed).await;
    assert_eq!(
        (unknown_status, error_type(&unknown)),
        (status, error_type(&body))
    );
    assert_eq!(
        denied_summaries(&s.records(&other).await),
        vec![
            "desk: not the person who asked".to_string(),
            "desk: no such pending action".to_string()
        ]
    );
    assert!(!used(&s, &id).await);
}

/// A pending action made on an earlier desk session (the person signed in
/// again since) is refused
#[rocket::async_test]
async fn desk_confirm_from_another_desk_session_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let (run, view, message) = proposed(&s, &staff, &command, "change", "n").await;

    s.sign_in(&staff, 0).await;
    let (status, body) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "NotPrivileged");
    assert_eq!(
        denied_summaries(&s.records(&staff).await),
        vec!["desk: asked on another desk session".to_string()]
    );
    assert!(!used(&s, &pending_id(&view)).await);
}

/// An expired pending action is refused
#[rocket::async_test]
async fn desk_confirm_expired_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let run = desk_press(
        &s,
        &staff,
        &command,
        test_change("change", &staff.user.id, "n"),
    )
    .await;
    let (_, view) = s.hand("run", &run).await;
    let fresh = s
        .harness
        .db
        .fetch_desk_pending(&pending_id(&view))
        .await
        .unwrap()
        .unwrap();
    step_up(&s, &staff, 0).await;

    // the same action, made eleven minutes ago
    let old = DeskPending {
        id: ulid::Ulid::new().to_string(),
        created_at: staff_now() - 11 * 60 * 1000,
        expires_at: staff_now() - 60 * 1000,
        ..fresh
    };
    s.harness.db.insert_desk_pending(&old).await.unwrap();
    let card = s
        .harness
        .db
        .fetch_desk_pending(&old.id)
        .await
        .unwrap()
        .unwrap();
    let content = super::view::canonical_render(&card.title, &[], &card.lines);
    let message = answer_card(&s, &run, &content, confirm_button(&old.id)).await;
    let pressed = press_confirm(&s, &staff, &old.id, &run.0, &message).await;
    let (status, body) = s.hand("confirm", &pressed).await;
    assert_eq!(status, Status::Gone, "{body}");
    assert_eq!(error_type(&body), "InteractionExpired");
    assert_eq!(
        denied_summaries(&s.records(&staff).await),
        vec!["desk: confirmation expired".to_string()]
    );
}

/// A pending action runs once; the second press is refused and recorded.
/// The run is recorded as Success on the desk session.
#[rocket::async_test]
async fn desk_confirm_second_press_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let (run, view, message) = proposed(&s, &staff, &command, "change", "once").await;
    step_up(&s, &staff, 0).await;
    let id = pending_id(&view);

    let (status, done) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(status, Status::Ok, "{done}");
    assert_eq!(done["title"], json!("Test change done"));
    assert!(
        done["content"]
            .as_str()
            .unwrap()
            .starts_with("**Test change done**")
    );
    assert!(used(&s, &id).await);
    let session = s.desk_sessions(&staff).await.remove(0);
    let success: Vec<_> = s
        .record_sessions(&staff)
        .await
        .into_iter()
        .filter(|r| r.0 == "staff.desk.test.change" && r.1 == "Success")
        .collect();
    assert_eq!(success.len(), 1, "{success:?}");
    assert_eq!(success[0].2.as_deref(), Some(session.id.as_str()));

    let (status, body) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(status, Status::Gone, "{body}");
    assert_eq!(
        denied_summaries(&s.records(&staff).await),
        vec!["desk: already confirmed".to_string()]
    );
    assert_eq!(test_entries::runs(&id), 1);
}

/// A press that is not on the card exactly as the server made it is
/// refused: changed words, the card wrapped in other text, an extra
/// button, a Primary button on an irreversible action, the card shown on
/// another command's answer, the card as a follow-up
#[rocket::async_test]
async fn desk_confirm_wrong_card_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let mut ids = Vec::new();

    async fn made(
        s: &Stage,
        staff: &Person,
        command: &ApplicationCommand,
    ) -> ((String, String), Value, String) {
        let run = desk_press(
            s,
            staff,
            command,
            test_change("change", &staff.user.id, "n"),
        )
        .await;
        let (_, view) = s.hand("run", &run).await;
        let id = pending_id(&view);
        (run, view, id)
    }
    step_up(&s, &staff, 1).await;

    // the bot rewrote the consequences
    let (run, _, id) = made(&s, &staff, &command).await;
    let message = answer_card(
        &s,
        &run,
        "**Test change**\nNothing happens, just press it.",
        confirm_button(&id),
    )
    .await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    assert_eq!(
        error_type(&s.hand("confirm", &pressed).await.1),
        "InvalidOperation"
    );
    ids.push(id);

    // the exact card, wrapped in the bot's own words
    let (run, view, id) = made(&s, &staff, &command).await;
    let wrapped = format!("Routine check, nothing changes:\n{}", rendered(&view));
    let message = answer_card(&s, &run, &wrapped, confirm_button(&id)).await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    assert_eq!(
        error_type(&s.hand("confirm", &pressed).await.1),
        "InvalidOperation"
    );
    ids.push(id);

    // the right words, a second button beside Confirm
    let (run, view, id) = made(&s, &staff, &command).await;
    let mut buttons = confirm_button(&id);
    buttons.as_array_mut().unwrap().push(
        json!({ "type": "Button", "style": "Primary", "label": "More", "custom_id": "zd:more" }),
    );
    let message = answer_card(&s, &run, &rendered(&view), buttons).await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    assert_eq!(
        error_type(&s.hand("confirm", &pressed).await.1),
        "InvalidOperation"
    );
    ids.push(id);

    // irreversible, but the button is not the danger style
    let (run, view, id) = made(&s, &staff, &command).await;
    let message = answer_card(&s, &run, &rendered(&view), button(&id, "Primary", CONFIRM)).await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    assert_eq!(
        error_type(&s.hand("confirm", &pressed).await.1),
        "InvalidOperation"
    );
    ids.push(id);

    // the right card, but pressed on another command's answer
    let (_, view, id) = made(&s, &staff, &command).await;
    let elsewhere = desk_press(&s, &staff, &command, sub("me")).await;
    let message = answer_card(&s, &elsewhere, &rendered(&view), confirm_button(&id)).await;
    let pressed = press_confirm(&s, &staff, &id, &elsewhere.0, &message).await;
    assert_eq!(
        error_type(&s.hand("confirm", &pressed).await.1),
        "InvalidOperation"
    );
    ids.push(id);

    // the exact card, but as a follow-up of the right interaction
    let (run, view, id) = made(&s, &staff, &command).await;
    answer_card(&s, &run, "One moment", json!([])).await;
    let followup = card_message(&s, &run, &rendered(&view), confirm_button(&id));
    let followup_id = followup.id.clone();
    assert!(
        s.harness
            .db
            .push_interaction_followup(&run.0, Some(followup), None)
            .await
            .unwrap()
    );
    let pressed = press_confirm(&s, &staff, &id, &run.0, &followup_id).await;
    assert_eq!(
        error_type(&s.hand("confirm", &pressed).await.1),
        "InvalidOperation"
    );
    ids.push(id);

    // none of them ran, every refusal recorded
    for id in &ids {
        assert!(!used(&s, id).await);
    }
    assert_eq!(
        denied_summaries(&s.records(&staff).await),
        vec!["desk: not pressed on the card the server made".to_string(); ids.len()]
    );
    for id in &ids {
        assert_eq!(test_entries::runs(id), 0);
    }
}

/// An irreversible action needs a step-up made after its card: one made
/// before is not enough (`NotElevated`, nothing used), and the same card
/// works after a new step-up
#[rocket::async_test]
async fn desk_confirm_step_up_then_works() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;

    // no step-up at all
    let (run, view, message) = proposed(&s, &staff, &command, "change", "after step-up").await;
    let id = pending_id(&view);
    assert_eq!(view["buttons"][0]["style"], json!("danger"));
    let (status, body) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "NotElevated");
    assert!(!used(&s, &id).await);

    // a step-up made before the card
    step_up(&s, &staff, 0).await;
    let (run2, view2, message2) = proposed(&s, &staff, &command, "change", "too early").await;
    let (_, body) = confirm_on(&s, &staff, &view2, &run2, &message2).await;
    assert_eq!(error_type(&body), "NotElevated");
    assert!(!used(&s, &pending_id(&view2)).await);

    // the first card, now after that step-up: it works
    let (status, body) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(used(&s, &id).await);
    assert_eq!(test_entries::runs(&id), 1);
    assert_eq!(test_entries::runs(&pending_id(&view2)), 0);
}

/// A reversible action whose core asks for a step-up itself: the pending
/// action is given back unused, and works after the step-up
#[rocket::async_test]
async fn desk_confirm_core_step_up_keeps_pending() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let (run, view, message) = proposed(&s, &staff, &command, "soft", "soft").await;
    let id = pending_id(&view);
    assert_eq!(view["buttons"][0]["style"], json!("primary"));

    let (_, body) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(error_type(&body), "NotElevated");
    assert!(!used(&s, &id).await);

    step_up(&s, &staff, 0).await;
    let (status, body) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(status, Status::Ok, "{body}");
}

/// The area switched off between the card and the press: refused, nothing
/// used
#[rocket::async_test]
async fn desk_confirm_area_switched_off_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let (run, view, message) = proposed(&s, &staff, &command, "change", "n").await;
    step_up(&s, &staff, 0).await;

    let mut off = settings(&s.bot, vec![s.place.id().to_string()], false);
    off.areas = vec![];
    test_settings::register(off);
    let (_, body) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");
    assert_eq!(
        denied_summaries(&s.records(&staff).await),
        vec!["desk: action no longer offered".to_string()]
    );
    assert!(!used(&s, &pending_id(&view)).await);
}

/// The action fails after its Success record: a Failed record follows and
/// the pending action stays used
#[rocket::async_test]
async fn desk_confirm_failed_action_recorded() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let (run, view, message) = proposed(&s, &staff, &command, "change", "fail").await;
    step_up(&s, &staff, 0).await;

    let (_, body) = confirm_on(&s, &staff, &view, &run, &message).await;
    assert_eq!(error_type(&body), "NotFound", "{body}");
    assert!(used(&s, &pending_id(&view)).await);
    let records: Vec<_> = s
        .records(&staff)
        .await
        .into_iter()
        .filter(|r| r.0 == "staff.desk.test.change")
        .map(|r| (r.1, r.2))
        .collect();
    assert_eq!(
        records,
        vec![
            (
                "Success".to_string(),
                "desk: confirmed test.change".to_string()
            ),
            ("Failed".to_string(), "NotFound".to_string())
        ]
    );
}

/// The first record cannot be written: the action does not run and the
/// claim is given back
#[rocket::async_test]
async fn desk_confirm_audit_failure_changes_nothing() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let (_, view, _) = proposed(&s, &staff, &command, "change", "n").await;
    let id = pending_id(&view);
    let pending = s.harness.db.fetch_desk_pending(&id).await.unwrap().unwrap();
    assert!(
        s.harness
            .db
            .claim_desk_pending(&id, staff_now())
            .await
            .unwrap()
    );

    let staff_context = context_of(&s, &staff).await;
    let entry = registry::find_key("test.change").unwrap();
    let input = DeskInput {
        options: pending.options.clone(),
        areas: vec!["overview".to_string()],
        lang: Lang::En,
        pending: Some(id.clone()),
        page: 1,
        origin: String::new(),
    };
    let record = staff_context.record("staff.desk.test.change", None);
    let result = execute(
        &s.harness.db,
        &staff_context,
        entry,
        &pending,
        &input,
        record,
        |_, change| async move {
            // the write fails before the change is started
            drop(change);
            Err(create_error!(InternalError))
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(test_entries::runs(&id), 0);
    assert!(!used(&s, &id).await, "the claim must be given back");
}

/// The real `/interactions` route: `/desk` of the desk bot needs the
/// version the person saw, and keeps the options, the version and the
/// client's locale
#[rocket::async_test]
async fn desk_interaction_keeps_options_and_version() {
    use revolt_database::Member;

    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    let server = s.harness.db.fetch_server(&s.server_id).await.unwrap();
    let bot_user = s.harness.db.fetch_user(&s.bot.id).await.unwrap();
    for member in [&staff.user, &bot_user] {
        Member::create(&s.harness.db, &server, member, None)
            .await
            .unwrap();
    }
    revolt_presence::create_session(&s.bot.id, 0).await;
    let command = register_offered(&s).await;
    let send = |version: Option<String>| {
        let mut body = json!({
            "type": "Command",
            "application_id": s.bot.id,
            "channel_id": s.place.id(),
            "command_id": command.id,
            "options": test_change("change", &staff.user.id, "kept"),
            "locale": "ar",
            "nonce": ulid::Ulid::new().to_string(),
        });
        if let Some(version) = version {
            body["command_version"] = json!(version);
        }
        s.harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", staff.session.token.clone()))
            .header(ContentType::JSON)
            .body(body.to_string())
    };

    let response = send(None).dispatch().await;
    assert_eq!(response.status(), Status::BadRequest);

    let response = send(Some(command.version.clone())).dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    let accepted: Value = response.into_json().await.unwrap();
    let stored = s
        .harness
        .db
        .fetch_interaction(accepted["_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        stored.command_version.as_deref(),
        Some(command.version.as_str())
    );
    assert_eq!(stored.locale.as_deref(), Some("ar"));
    let expected: Vec<InteractionDataOption> = vec![InteractionDataOption {
        name: "test".to_string(),
        kind: CommandOptionType::SubCommandGroup,
        value: None,
        options: Some(vec![InteractionDataOption {
            name: "change".to_string(),
            kind: CommandOptionType::SubCommand,
            value: None,
            options: Some(vec![
                InteractionDataOption {
                    name: "user".to_string(),
                    kind: CommandOptionType::User,
                    value: Some(InteractionOptionValue::String(staff.user.id.clone())),
                    options: None,
                },
                InteractionDataOption {
                    name: "note".to_string(),
                    kind: CommandOptionType::String,
                    value: Some(InteractionOptionValue::String("kept".to_string())),
                    options: None,
                },
            ]),
        }]),
    }];
    assert_eq!(stored.command_options, expected);
}

/// The desk bot answers a desk command only in private: a channel answer,
/// or deferring one, is refused before anything is posted; a private
/// answer works
#[rocket::async_test]
async fn desk_answers_are_private() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    let command = register_offered(&s).await;
    let callback = |pressed: &(String, String), body: Value| {
        s.harness
            .client
            .post(format!(
                "/interactions/{}/{}/callback",
                pressed.0, pressed.1
            ))
            .header(ContentType::JSON)
            .body(body.to_string())
    };

    let pressed = desk_press(&s, &staff, &command, sub("me")).await;
    let response = callback(
        &pressed,
        json!({ "type": "Message", "data": { "content": "for everyone", "ephemeral": false } }),
    )
    .dispatch()
    .await;
    assert_eq!(response.status(), Status::BadRequest);
    let response = callback(&pressed, json!({ "type": "DeferredMessage" }))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
    // nothing was claimed: the private answer still goes through
    let response = callback(
        &pressed,
        json!({ "type": "Message", "data": { "content": "only you", "ephemeral": true } }),
    )
    .dispatch()
    .await;
    assert_eq!(response.status(), Status::NoContent);
}
