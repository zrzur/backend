//! The generic desk: the command tree, `/run` and pending actions with
//! `/confirm`. The bot hands back presses; the server decides everything.

use revolt_database::{
    staff_now, DeskPending, Interaction, InteractionSource, InteractionStatus, InteractionTransition,
    StaffRole,
};
use revolt_models::v0::{
    ApplicationCommand, CommandOptionType, EphemeralMessage, InteractionDataOption,
    InteractionOptionValue, InteractionType,
};
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use super::super::StaffContext;
use super::registry::{self, DeskInput};
use super::run::{CONFIRM_LABEL, CONFIRM_PREFIX};
use super::session::STEP_UP_MODAL;
use super::test_settings;
use super::tests::{code, error_type, settings, stage, Kind, Person, Stage};

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
    (status, response.into_json().await.unwrap_or(Value::Null))
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
    let (status, offered) = tree(s, &s.bot.token).await;
    assert_eq!(status, Status::Ok, "{offered}");
    register(s, offered).await
}

/// A `/desk` press on `command` as `/interactions` stores it (options and
/// the version the person saw); `options` is the top-level option list
async fn desk_press(s: &Stage, who: &Person, command: &ApplicationCommand, options: Value) -> (String, String) {
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
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    (interaction.id, token)
}

fn sub(name: &str) -> Value {
    json!([{ "type": "SubCommand", "name": name }])
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
async fn hand_extra(s: &Stage, path: &str, pressed: &(String, String), extra: Value) -> (Status, Value) {
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

/// The confirmation card as the bot renders a DeskView
fn rendered(view: &Value) -> String {
    let mut out = vec![format!("**{}**", view["title"].as_str().unwrap())];
    for line in view["lines"].as_array().unwrap() {
        out.push(line.as_str().unwrap().to_string());
    }
    out.join("\n")
}

/// The pending id in the view's Confirm button
fn pending_id(view: &Value) -> String {
    let buttons = view["buttons"].as_array().unwrap();
    assert_eq!(buttons.len(), 1, "{view}");
    assert_eq!(buttons[0]["label"], json!(CONFIRM_LABEL));
    buttons[0]["action_id"]
        .as_str()
        .unwrap()
        .strip_prefix(CONFIRM_PREFIX)
        .unwrap()
        .to_string()
}

/// The bot answers the `/run` press privately with `content` and one
/// button `custom_id`; gives the message id
async fn answer_card(s: &Stage, run: &(String, String), content: &str, buttons: Value) -> String {
    let message_id = ulid::Ulid::new().to_string();
    let message: EphemeralMessage = serde_json::from_value(json!({
        "_id": message_id,
        "interaction_id": run.0,
        "author": s.bot.id,
        "channel_id": s.place.id(),
        "content": content,
        "components": [{ "type": "ActionRow", "components": buttons }],
    }))
    .unwrap();
    assert!(s
        .harness
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
        .unwrap());
    message_id
}

fn confirm_button(id: &str) -> Value {
    json!([{ "type": "Button", "style": "Danger", "label": CONFIRM_LABEL, "custom_id": format!("{CONFIRM_PREFIX}{id}") }])
}

/// `who` presses `zd:confirm:<id>` on the private message `message_id` of
/// the interaction `card_of`
async fn press_confirm(s: &Stage, who: &Person, id: &str, card_of: &str, message_id: &str) -> (String, String) {
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

/// A signed-in Owner proposes `/desk test <name>` about themselves; gives
/// (the /run press, its view, the card message id)
async fn proposed(s: &Stage, who: &Person, command: &ApplicationCommand, name: &str, note: &str) -> ((String, String), Value, String) {
    let run = desk_press(s, who, command, test_change(name, &who.user.id, note)).await;
    let (status, view) = s.hand("run", &run).await;
    assert_eq!(status, Status::Ok, "{view}");
    let id = pending_id(&view);
    let message = answer_card(s, &run, &rendered(&view), confirm_button(&id)).await;
    (run, view, message)
}

fn denied_summaries(records: &[(String, String, String)]) -> Vec<String> {
    records
        .iter()
        .filter(|r| r.1 == "Denied")
        .map(|r| r.2.clone())
        .collect()
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
    assert_eq!(names, vec!["me", "login", "logout", "stepup", "stats", "test"]);
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
/// the presser, with a Denied record: a relabelled sub-command, an extra
/// one, a press made on an earlier version, a press without a version
#[rocket::async_test]
async fn desk_run_refuses_changed_command() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let (_, offered) = tree(&s, &s.bot.token).await;

    // the bot shows "stats" as something harmless-looking
    let mut relabelled = offered.clone();
    relabelled["options"][4]["description"] = json!("Show the weather");
    let command = register(&s, relabelled).await;
    let pressed = desk_press(&s, &staff, &command, sub("stats")).await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{status} {body}");
    let records = s.records(&staff).await;
    let last = records.last().unwrap();
    assert_eq!(
        (last.0.as_str(), last.1.as_str(), last.2.as_str()),
        ("staff.desk.command", "Denied", "desk: command not as offered")
    );

    // honest again, but the press was made on the relabelled version
    s.harness.db.delete_application_command(&command.id).await.unwrap();
    let honest = register(&s, offered.clone()).await;
    let mut stale = honest.clone();
    stale.version = command.version.clone();
    let pressed = desk_press(&s, &staff, &stale, sub("stats")).await;
    assert_eq!(error_type(&s.hand("run", &pressed).await.1), "InvalidOperation");

    // no version stored at all
    let pressed = desk_press(&s, &staff, &honest, sub("stats")).await;
    let mut stored = s.harness.db.fetch_interaction(&pressed.0).await.unwrap();
    stored.command_version = None;
    let (mut copy, token) = Interaction::new(
        InteractionType::Command,
        s.bot.id.clone(),
        staff.user.id.clone(),
        Some(s.server_id.clone()),
        s.place.id().to_string(),
        ulid::Ulid::new().to_string(),
    );
    copy.session_id = stored.session_id.clone();
    copy.command_id = stored.command_id.clone();
    copy.command_name = stored.command_name.clone();
    copy.command_options = stored.command_options.clone();
    s.harness.db.insert_interaction(&copy).await.unwrap();
    assert_eq!(error_type(&s.hand("run", &(copy.id.clone(), token)).await.1), "InvalidOperation");

    // the honest command works
    let pressed = desk_press(&s, &staff, &honest, sub("stats")).await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
    let denied = denied_summaries(&s.records(&staff).await);
    assert_eq!(denied.len(), 3, "{denied:?}");
}

/// `/desk stats` through `/run` is the console's `stats_core` for a
/// DashboardRead holder; the same command without DashboardRead is refused
/// by the console's own check, recorded on the desk session; `/desk me`
/// answers as a DeskView; session sub-commands are not run here
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
    let fields: Vec<&str> = view["fields"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert!(fields.contains(&"Users") && fields.contains(&"Open reports"), "{view}");
    assert!(view["buttons"].as_array().unwrap().is_empty());

    let pressed = desk_press(&s, &staff, &command, sub("me")).await;
    let (status, me) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{me}");
    let areas = me["fields"].as_array().unwrap().iter().find(|f| f["name"] == json!("Areas")).unwrap();
    assert_eq!(areas["value"], json!("overview"));
    assert!(me["lines"].as_array().unwrap().contains(&json!("/desk stats")), "{me}");

    let pressed = desk_press(&s, &staff, &command, sub("logout")).await;
    assert_eq!(error_type(&s.hand("run", &pressed).await.1), "InvalidOperation");

    // every role holds DashboardRead: the refusal is shown on the presser's
    // own context with the permission taken away
    let user = s.harness.db.fetch_user(&staff.user.id).await.unwrap();
    let session = s.desk_sessions(&staff).await.remove(0);
    let grant = s.harness.db.fetch_staff_grant(&staff.user.id).await.unwrap().unwrap();
    let mut permissions = grant.effective(staff_now());
    permissions.remove(&revolt_database::StaffPermission::DashboardRead);
    let without = StaffContext {
        user,
        session: session.clone(),
        grant,
        permissions,
        now: staff_now(),
        path: "/staff/desk/run".to_string(),
    };
    let entry = registry::find(None, "stats").unwrap();
    let error = registry::dispatch(&s.harness.db, &without, entry, &DeskInput::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&error.error_type, revolt_result::ErrorType::MissingPlatformPermission { permission } if permission == "platform.dashboard.read"),
        "{error:?}"
    );
    let last = s.record_sessions(&staff).await.pop().unwrap();
    assert_eq!((last.0.as_str(), last.1.as_str()), ("staff.access.denied", "Denied"));
    assert_eq!(last.2.as_deref(), Some(session.id.as_str()));
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

    let run = desk_press(&s, &staff, &command, test_change("change", &staff.user.id, "stored note")).await;
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
    assert_eq!(note.value, Some(InteractionOptionValue::String("stored note".to_string())));
    assert!(view["lines"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("stored note")));
    // nothing was executed, nothing recorded as done
    assert!(s.records(&staff).await.iter().all(|r| r.0 != "staff.desk.test.change"));

    // confirmed: the stored note, whatever the bot adds to the body
    step_up(&s, &staff, 0).await;
    let message = answer_card(&s, &run, &rendered(&view), confirm_button(&id)).await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    let (status, done) = hand_extra(&s, "confirm", &pressed, json!({ "note": "forged", "pending_id": "other" })).await;
    assert_eq!(status, Status::Ok, "{done}");
    let note = done["fields"].as_array().unwrap().iter().find(|f| f["name"] == json!("Note")).unwrap();
    assert_eq!(note["value"], json!("stored note"));
}

// ---- pending actions ----

/// Another person's press on the card is refused, as them, and leaves the
/// pending action usable for its owner
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
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "NotPrivileged");
    assert_eq!(denied_summaries(&s.records(&other).await), vec!["desk: not the person who asked".to_string()]);
    assert!(!s.harness.db.fetch_desk_pending(&id).await.unwrap().unwrap().used);
}

/// An expired pending action is refused
#[rocket::async_test]
async fn desk_confirm_expired_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    step_up(&s, &staff, 0).await;
    let command = register_offered(&s).await;
    let run = desk_press(&s, &staff, &command, test_change("change", &staff.user.id, "n")).await;
    let (_, view) = s.hand("run", &run).await;
    let fresh = s.harness.db.fetch_desk_pending(&pending_id(&view)).await.unwrap().unwrap();

    // the same action, made eleven minutes ago
    let old = DeskPending {
        id: ulid::Ulid::new().to_string(),
        created_at: iso8601_timestamp::Timestamp::now_utc()
            .checked_add(iso8601_timestamp::Duration::minutes(-11))
            .unwrap(),
        expires_at: iso8601_timestamp::Timestamp::now_utc()
            .checked_add(iso8601_timestamp::Duration::minutes(-1))
            .unwrap(),
        ..fresh
    };
    s.harness.db.insert_desk_pending(&old).await.unwrap();
    let message = answer_card(&s, &run, &rendered(&view), confirm_button(&old.id)).await;
    let pressed = press_confirm(&s, &staff, &old.id, &run.0, &message).await;
    let (status, body) = s.hand("confirm", &pressed).await;
    assert_eq!(status, Status::Gone, "{body}");
    assert_eq!(error_type(&body), "InteractionExpired");
    assert_eq!(denied_summaries(&s.records(&staff).await), vec!["desk: confirmation expired".to_string()]);
}

/// A pending action runs once; the second press is refused and recorded.
/// The run is recorded as Success on the desk session.
#[rocket::async_test]
async fn desk_confirm_second_press_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    step_up(&s, &staff, 0).await;
    let command = register_offered(&s).await;
    let (run, view, message) = proposed(&s, &staff, &command, "change", "once").await;
    let id = pending_id(&view);

    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    let (status, done) = s.hand("confirm", &pressed).await;
    assert_eq!(status, Status::Ok, "{done}");
    assert_eq!(done["title"], json!("Test change done"));
    assert!(s.harness.db.fetch_desk_pending(&id).await.unwrap().unwrap().used);
    let session = s.desk_sessions(&staff).await.remove(0);
    let success: Vec<_> = s
        .record_sessions(&staff)
        .await
        .into_iter()
        .filter(|r| r.0 == "staff.desk.test.change" && r.1 == "Success")
        .collect();
    assert_eq!(success.len(), 1, "{success:?}");
    assert_eq!(success[0].2.as_deref(), Some(session.id.as_str()));

    let again = press_confirm(&s, &staff, &id, &run.0, &message).await;
    let (status, body) = s.hand("confirm", &again).await;
    assert_eq!(status, Status::Gone, "{body}");
    assert_eq!(denied_summaries(&s.records(&staff).await), vec!["desk: already confirmed".to_string()]);
}

/// A press that is not on the card the server made is refused: changed
/// words, an extra button, the card shown on another command's answer
#[rocket::async_test]
async fn desk_confirm_wrong_card_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    step_up(&s, &staff, 0).await;
    let command = register_offered(&s).await;

    // the bot rewrote the consequences
    let run = desk_press(&s, &staff, &command, test_change("change", &staff.user.id, "n")).await;
    let (_, view) = s.hand("run", &run).await;
    let id = pending_id(&view);
    let message = answer_card(&s, &run, "**Test change**\nNothing happens, just press it.", confirm_button(&id)).await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    let (_, body) = s.hand("confirm", &pressed).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");

    // the right words, a second button beside Confirm
    let run = desk_press(&s, &staff, &command, test_change("change", &staff.user.id, "n")).await;
    let (_, view) = s.hand("run", &run).await;
    let id2 = pending_id(&view);
    let mut buttons = confirm_button(&id2);
    buttons.as_array_mut().unwrap().push(json!({ "type": "Button", "style": "Primary", "label": "More", "custom_id": "zd:more" }));
    let message = answer_card(&s, &run, &rendered(&view), buttons).await;
    let pressed = press_confirm(&s, &staff, &id2, &run.0, &message).await;
    assert_eq!(error_type(&s.hand("confirm", &pressed).await.1), "InvalidOperation");

    // the right card, but pressed on another command's answer
    let run3 = desk_press(&s, &staff, &command, test_change("change", &staff.user.id, "n")).await;
    let (_, view3) = s.hand("run", &run3).await;
    let id3 = pending_id(&view3);
    let elsewhere = desk_press(&s, &staff, &command, sub("me")).await;
    let message = answer_card(&s, &elsewhere, &rendered(&view3), confirm_button(&id3)).await;
    let pressed = press_confirm(&s, &staff, &id3, &elsewhere.0, &message).await;
    assert_eq!(error_type(&s.hand("confirm", &pressed).await.1), "InvalidOperation");

    // none of them ran, all three refusals recorded
    for id in [&id, &id2, &id3] {
        assert!(!s.harness.db.fetch_desk_pending(id).await.unwrap().unwrap().used);
    }
    assert_eq!(
        denied_summaries(&s.records(&staff).await),
        vec!["desk: not pressed on the card the server made".to_string(); 3]
    );
}

/// An irreversible action needs a step-up: `NotElevated` first, and the
/// same card works after the step-up
#[rocket::async_test]
async fn desk_confirm_step_up_then_works() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&staff, -1).await;
    let command = register_offered(&s).await;
    let (run, view, message) = proposed(&s, &staff, &command, "change", "after step-up").await;
    let id = pending_id(&view);
    assert_eq!(view["buttons"][0]["style"], json!("danger"));

    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    let (status, body) = s.hand("confirm", &pressed).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "NotElevated");
    assert!(!s.harness.db.fetch_desk_pending(&id).await.unwrap().unwrap().used);

    step_up(&s, &staff, 0).await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    let (status, body) = s.hand("confirm", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(s.harness.db.fetch_desk_pending(&id).await.unwrap().unwrap().used);
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

    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    assert_eq!(error_type(&s.hand("confirm", &pressed).await.1), "NotElevated");
    assert!(!s.harness.db.fetch_desk_pending(&id).await.unwrap().unwrap().used);

    step_up(&s, &staff, 0).await;
    let pressed = press_confirm(&s, &staff, &id, &run.0, &message).await;
    let (status, body) = s.hand("confirm", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
}

/// The real `/interactions` route: `/desk` of the desk bot needs the
/// version the person saw, and keeps the options and the version
#[rocket::async_test]
async fn desk_interaction_keeps_options_and_version() {
    use revolt_database::Member;

    let s = stage().await;
    let staff = s.person(vec![StaffRole::Owner]).await;
    let server = s.harness.db.fetch_server(&s.server_id).await.unwrap();
    let bot_user = s.harness.db.fetch_user(&s.bot.id).await.unwrap();
    for member in [&staff.user, &bot_user] {
        Member::create(&s.harness.db, &server, member, None).await.unwrap();
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
    assert_eq!(stored.command_version.as_deref(), Some(command.version.as_str()));
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
