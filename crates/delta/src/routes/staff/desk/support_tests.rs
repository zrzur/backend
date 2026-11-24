//! Desk support (`/desk-support`): every command runs as the console's own
//! core with the console's records on the desk session; refused without the
//! permission; a fresh code where the console asks for a step-up.

use revolt_config::{FeaturesTeamApplications, TeamRole};
use revolt_database::{
    PlatformNotice, StaffRole, TeamHistory, TeamMember, TeamMemberStatus, User, staff_now,
};
use revolt_models::v0::ApplicationCommand;
use rocket::http::{ContentType, Header, Status};
use serde_json::{Value, json};

use super::moderation_tests::{
    all_areas, as_user, card, confirm, console_token, press, recorded, step_up, text, user_opt,
};
use super::tests::{Person, Stage, error_type, register_served, stage};

async fn register_support(s: &Stage) -> ApplicationCommand {
    register_served(s).await.remove("desk-support").unwrap()
}

/// The volunteer feature on, looked up by the staff member's id
async fn team_on(s: &Stage, staff: &Person) -> String {
    let mut settings = FeaturesTeamApplications {
        server: s.server_id.clone(),
        channel_apply: "apply".into(),
        channel_review: "review".into(),
        reviewer_role: "reviewer".into(),
        bot: staff.user.id.clone(),
        roles: vec![TeamRole {
            key: "support".into(),
            name: "Support".into(),
            server: s.server_id.clone(),
            ..Default::default()
        }],
        ..Default::default()
    };
    settings.volunteers.enabled = true;
    settings.volunteers.away_max_days = 90;
    settings.volunteers.trial_days = 30;
    crate::routes::team_desk::test_settings::register(settings);
    s.server_id.clone()
}

async fn volunteer(s: &Stage, user: &User, status: TeamMemberStatus) -> TeamMember {
    let member = TeamMember {
        id: ulid::Ulid::new().to_string(),
        user_id: user.id.clone(),
        role: "support".into(),
        server: s.server_id.clone(),
        status,
        joined_at: staff_now() - 1000,
        trial_ends_at: staff_now() + 100_000,
        confirmed_at: (status != TeamMemberStatus::Trial).then(|| staff_now() - 500),
        away_until: None,
        left_at: None,
        low_months: 0,
        notes: None,
        invite: None,
        history: vec![TeamHistory {
            at: 1,
            actor: "system".into(),
            action: "accepted".into(),
        }],
    };
    s.harness.db.insert_team_member(&member).await.unwrap();
    member
}

async fn ticket(s: &Stage, session: &str) -> String {
    let ticket = as_user(
        s,
        session,
        "/support/tickets",
        json!({ "category": "billing", "subject": "Charged twice", "body": "I was charged twice this month." }),
    )
    .await;
    ticket["_id"].as_str().unwrap().to_string()
}

async fn notice_for(s: &Stage, user: &str, by: &str) -> String {
    let lang = |v: &str| {
        ["ar", "en", "de"]
            .into_iter()
            .map(|l| (l.to_string(), v.to_string()))
            .collect()
    };
    let notice = PlatformNotice {
        id: ulid::Ulid::new().to_string(),
        user_id: Some(user.to_string()),
        title: lang("Hello"),
        body: lang("A word from us"),
        link: None,
        tone: "info".into(),
        created_by: by.to_string(),
        created_at: staff_now(),
        expires_at: None,
        withdrawn_at: None,
        emailed: false,
        pushed: false,
    };
    s.harness.db.insert_platform_notice(&notice).await.unwrap();
    notice.id
}

fn notice_options(user: Option<&str>) -> Value {
    let mut options = vec![];
    if let Some(user) = user {
        options.push(user_opt(user));
    }
    for (name, value) in [
        ("title_en", "An update"),
        ("title_ar", "تحديث"),
        ("title_de", "Neuigkeit"),
        ("text_en", "We looked at your request."),
        ("text_ar", "راجعنا طلبك."),
        ("text_de", "Wir haben deine Anfrage geprüft."),
        ("tone", "info"),
    ] {
        options.push(text(name, value));
    }
    Value::Array(options)
}

/// `/desk-support <name>`: a sub-command without a group
async fn press_top(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    name: &str,
    options: Value,
) -> (String, String) {
    let (mut interaction, token) = revolt_database::Interaction::new(
        revolt_models::v0::InteractionType::Command,
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
    interaction.command_options =
        serde_json::from_value(json!([{ "type": "SubCommand", "name": name, "options": options }]))
            .unwrap();
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    (interaction.id, token)
}

fn member_opt(id: &str) -> Value {
    text("member", id)
}

/// Every support command, confirmed by an Owner: the console's record on
/// the desk session, the change visible as in the console; the commands
/// the console guards with a step-up wait for a fresh code
#[rocket::async_test]
async fn desk_support_runs_the_consoles_actions() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    team_on(&s, &owner).await;
    let command = register_support(&s).await;
    let console = console_token(&s, &owner, 0).await;
    let (_, user_session, user) = s.harness.new_user().await;

    // ---- tickets ----
    let t1 = ticket(&s, &user_session.token).await;
    let reply = card(
        &s,
        &owner,
        &command,
        "ticket",
        "reply",
        json!([
            text("ticket", &t1),
            text("text", "We refunded the *second* charge.")
        ]),
    )
    .await;
    assert!(!reply.danger());
    assert!(
        reply.view["content"]
            .as_str()
            .unwrap()
            .contains(r"the \*second\* charge"),
        "{}",
        reply.view
    );
    let (status, body) = confirm(&s, &owner, &reply).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.support.reply").await;
    assert!(
        body["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["value"] == json!("Answered")),
        "{body}"
    );
    // the same answer through the console gives the same status
    let t2 = ticket(&s, &user_session.token).await;
    let response = s
        .harness
        .client
        .post(format!("/staff/support/tickets/{t2}/messages"))
        .header(Header::new("x-session-token", owner.session.token.clone()))
        .header(Header::new("x-staff-session", console.clone()))
        .header(ContentType::JSON)
        .body(json!({ "body": "We refunded the second charge." }).to_string())
        .dispatch()
        .await;
    let from_console: Value = response.into_json().await.unwrap();
    assert_eq!(from_console["status"], json!("Answered"));
    for (name, options, record) in [
        (
            "note",
            json!([text("ticket", &t1), text("text", "Refund went through")]),
            "staff.support.note",
        ),
        (
            "assign",
            json!([text("ticket", &t1)]),
            "staff.support.change",
        ),
        (
            "close",
            json!([text("ticket", &t1)]),
            "staff.support.change",
        ),
        (
            "reopen",
            json!([text("ticket", &t1)]),
            "staff.support.change",
        ),
    ] {
        let c = card(&s, &owner, &command, "ticket", name, options).await;
        let (status, body) = confirm(&s, &owner, &c).await;
        assert_eq!(status, Status::Ok, "ticket {name}: {body}");
        recorded(&s, &owner, record).await;
    }
    let stored = s.harness.db.fetch_support_ticket(&t1).await.unwrap();
    assert_eq!(stored.assigned_to.as_deref(), Some(owner.user.id.as_str()));
    assert_eq!(stored.messages.len(), 3);

    // ---- change requests ----
    let c = card(
        &s,
        &owner,
        &command,
        "change-request",
        "create",
        json!([
            user_opt(&user.id),
            text("reason", "Your name copies a brand"),
            text("display_name", "Sam")
        ]),
    )
    .await;
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.user.change_request").await;
    let request = body["fields"]
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
        "change-request",
        "cancel",
        json!([text("request", &request)]),
    )
    .await;
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.user.change_request.cancel").await;

    // ---- a bug review gives Bug Hunter as in the console ----
    let bug = as_user(&s, &user_session.token, "/bugs", json!({ "kind": "bug", "title": "Send button stuck", "description": "The send button stays grey", "platform": "web" })).await;
    let c = card(
        &s,
        &owner,
        &command,
        "bug",
        "review",
        json!([
            text("bug", bug["_id"].as_str().unwrap()),
            text("action", "confirm"),
            text("severity", "medium")
        ]),
    )
    .await;
    assert!(!c.danger(), "a bug report is not a security report");
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.bug.review").await;
    let report = s
        .harness
        .db
        .fetch_bug_report(bug["_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&report.status).unwrap(),
        json!("Confirmed")
    );

    // ---- the volunteer team ----
    let (_, _, trial_user) = s.harness.new_user().await;
    let trial = volunteer(&s, &trial_user, TeamMemberStatus::Trial).await;
    for (name, options, record) in [
        (
            "confirm",
            json!([member_opt(&trial.id)]),
            "team.member.confirm",
        ),
        (
            "counting",
            json!([
                member_opt(&trial.id),
                json!({ "type": "Boolean", "name": "on", "value": false })
            ]),
            "team.member.counting",
        ),
        (
            "away",
            json!([
                member_opt(&trial.id),
                json!({ "type": "Integer", "name": "days", "value": 5 })
            ]),
            "team.member.away",
        ),
        (
            "note",
            json!([
                member_opt(&trial.id),
                text("text", "Helpful in the Arabic channels")
            ]),
            "team.member.note",
        ),
    ] {
        let c = card(&s, &owner, &command, "team", name, options).await;
        let (status, body) = confirm(&s, &owner, &c).await;
        assert_eq!(status, Status::Ok, "team {name}: {body}");
        recorded(&s, &owner, record).await;
    }
    let stored = s.harness.db.fetch_team_member(&trial.id).await.unwrap();
    assert_eq!(stored.status, TeamMemberStatus::Away);
    assert_eq!(
        stored.notes.as_deref(),
        Some("Helpful in the Arabic channels")
    );
    let pressed = press(
        &s,
        &owner,
        &command,
        "team",
        "view",
        json!([member_opt(&trial.id)]),
    )
    .await;
    let (status, view) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{view}");
    let pressed = press(&s, &owner, &command, "team", "list", json!([])).await;
    let (status, list) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{list}");
    assert!(
        list["content"].as_str().unwrap().contains(&trial.id),
        "{list}"
    );

    // ---- step-up actions: cards first, one fresh code, then confirm ----
    let (_, _, leaving_user) = s.harness.new_user().await;
    let leaving = volunteer(&s, &leaving_user, TeamMemberStatus::Active).await;
    let old_notice = notice_for(&s, &user.id, &owner.user.id).await;
    let send = card(
        &s,
        &owner,
        &command,
        "notice",
        "send",
        notice_options(Some(&user.id)),
    )
    .await;
    let everyone = card(
        &s,
        &owner,
        &command,
        "notice",
        "everyone",
        notice_options(None),
    )
    .await;
    let withdraw = card(
        &s,
        &owner,
        &command,
        "notice",
        "withdraw",
        json!([
            text("notice", &old_notice),
            text("reason", "Sent by mistake")
        ]),
    )
    .await;
    let remove = card(
        &s,
        &owner,
        &command,
        "team",
        "remove",
        json!([
            member_opt(&leaving.id),
            text("reason", "No activity for three months now")
        ]),
    )
    .await;
    for c in [&send, &everyone, &withdraw, &remove] {
        assert!(c.danger(), "{}", c.view);
        let (_, body) = confirm(&s, &owner, c).await;
        assert_eq!(error_type(&body), "NotElevated", "{body}");
    }
    // the identity needs the console's step-up too
    let pressed = press(
        &s,
        &owner,
        &command,
        "team",
        "identity",
        json!([member_opt(&trial.id)]),
    )
    .await;
    let (_, body) = s.hand("run", &pressed).await;
    assert_eq!(error_type(&body), "NotElevated", "{body}");

    step_up(&s, &owner, 1).await;
    for (c, record) in [
        (&send, "staff.notice.send"),
        (&everyone, "staff.notice.send"),
        (&withdraw, "staff.notice.withdraw"),
        (&remove, "team.member.remove"),
    ] {
        let (status, body) = confirm(&s, &owner, c).await;
        assert_eq!(status, Status::Ok, "{record}: {body}");
        recorded(&s, &owner, record).await;
    }
    assert!(
        s.harness
            .db
            .fetch_platform_notice(&old_notice)
            .await
            .unwrap()
            .withdrawn_at
            .is_some()
    );
    assert!(
        s.harness
            .db
            .fetch_team_member(&leaving.id)
            .await
            .unwrap()
            .left_at
            .is_some()
    );
    let pressed = press(
        &s,
        &owner,
        &command,
        "team",
        "identity",
        json!([member_opt(&trial.id)]),
    )
    .await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "team.identity.view").await;
}

/// Without the permission, nothing: every support command is refused at
/// once, as the presser, with the console's Denied record
#[rocket::async_test]
async fn desk_support_refused_without_permission() {
    let s = stage().await;
    all_areas(&s);
    let analyst = s.person(vec![StaffRole::Analyst]).await;
    s.sign_in(&analyst, -1).await;
    let command = register_support(&s).await;
    let entries: Vec<_> = super::registry::entries()
        .filter(|e| e.area == Some("support"))
        .collect();
    assert_eq!(entries.len(), 20);
    for entry in entries {
        let before = s.records(&analyst).await.len();
        let pressed = match entry.group {
            Some(group) => press(&s, &analyst, &command, group, entry.name, json!([])).await,
            None => press_top(&s, &analyst, &command, entry.name, json!([])).await,
        };
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
}

/// A notice to everyone is the settings team's; a security report's review
/// needs a fresh code; a security request without an email server is
/// refused like the console refuses it
#[rocket::async_test]
async fn desk_support_follows_the_consoles_guards() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let support = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&owner, -1).await;
    s.sign_in(&support, -1).await;
    let command = register_support(&s).await;
    let (_, user_session, user) = s.harness.new_user().await;

    // Support may not write to everyone
    let pressed = press(
        &s,
        &support,
        &command,
        "notice",
        "everyone",
        notice_options(None),
    )
    .await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_eq!(
        (status, error_type(&body)),
        (Status::Forbidden, "MissingPlatformPermission"),
        "{body}"
    );

    // a security report: Danger and a fresh code
    let secret = as_user(&s, &user_session.token, "/bugs", json!({ "kind": "security", "title": "Token in logs", "description": "The session token shows in the logs", "platform": "web" })).await;
    let c = card(
        &s,
        &owner,
        &command,
        "bug",
        "review",
        json!([
            text("bug", secret["_id"].as_str().unwrap()),
            text("action", "triage")
        ]),
    )
    .await;
    assert!(c.danger());
    let (_, body) = confirm(&s, &owner, &c).await;
    assert_eq!(error_type(&body), "NotElevated", "{body}");

    // a security request: Danger and a fresh code, as the console asks
    let pressed = press_top(
        &s,
        &owner,
        &command,
        "security-request",
        json!([
            user_opt(&user.id),
            text("kind", "two_factor"),
            text("reason", "Lost the authenticator app")
        ]),
    )
    .await;
    let (status, view) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{view}");
    assert_eq!(view["buttons"][0]["style"], json!("danger"));
    assert!(
        view["lines"]
            .as_array()
            .unwrap()
            .contains(&json!(super::registry::texts::FRESH_CODE.en)),
        "{view}"
    );
}

/// The cards apply the console's rules before showing anything: a security
/// report stays hidden from Support; nobody answers their own ticket; no
/// notice, change or security request about a bot or yourself; away within
/// the configured limit; a notice without an end says so; a ticket card is
/// refused when the ticket changed since
#[rocket::async_test]
async fn desk_support_cards_follow_the_console() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let support = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&owner, -1).await;
    s.sign_in(&support, -1).await;
    team_on(&s, &owner).await;
    let command = register_support(&s).await;
    let console = console_token(&s, &owner, 0).await;
    let (_, user_session, user) = s.harness.new_user().await;

    // a security report: Support (no SecurityRead) gets "not found"
    let secret = as_user(&s, &user_session.token, "/bugs", json!({ "kind": "security", "title": "Token in logs", "description": "The session token shows in the logs", "platform": "web" })).await;
    let pressed = press(
        &s,
        &support,
        &command,
        "bug",
        "review",
        json!([
            text("bug", secret["_id"].as_str().unwrap()),
            text("action", "triage")
        ]),
    )
    .await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_eq!(
        (status, error_type(&body)),
        (Status::NotFound, "NotFound"),
        "{body}"
    );
    assert!(
        s.records(&support)
            .await
            .iter()
            .any(|r| r.1 == "Denied" && r.2 == "desk: a report this person may not see"),
        "{:?}",
        s.records(&support).await
    );

    // nobody answers their own ticket
    let own = ticket(&s, &owner.session.token).await;
    let pressed = press(
        &s,
        &owner,
        &command,
        "ticket",
        "reply",
        json!([text("ticket", &own), text("text", "Answering myself")]),
    )
    .await;
    assert_eq!(
        error_type(&s.hand("run", &pressed).await.1),
        "NotPrivileged"
    );

    // about a bot, or about yourself
    let pressed = press(
        &s,
        &owner,
        &command,
        "notice",
        "send",
        notice_options(Some(&s.bot.id)),
    )
    .await;
    assert_eq!(error_type(&s.hand("run", &pressed).await.1), "IsBot");
    let pressed = press(
        &s,
        &owner,
        &command,
        "change-request",
        "create",
        json!([
            user_opt(&s.bot.id),
            text("reason", "Rename it"),
            text("display_name", "Sam")
        ]),
    )
    .await;
    assert_eq!(error_type(&s.hand("run", &pressed).await.1), "IsBot");
    let pressed = press_top(
        &s,
        &owner,
        &command,
        "security-request",
        json!([
            user_opt(&owner.user.id),
            text("kind", "two_factor"),
            text("reason", "Lost my phone")
        ]),
    )
    .await;
    assert_eq!(
        error_type(&s.hand("run", &pressed).await.1),
        "CannotReportYourself"
    );

    // away: no longer than the volunteers' own limit (90 here)
    let (_, _, volunteer_user) = s.harness.new_user().await;
    let member = volunteer(&s, &volunteer_user, TeamMemberStatus::Active).await;
    let pressed = press(
        &s,
        &owner,
        &command,
        "team",
        "away",
        json!([
            member_opt(&member.id),
            json!({ "type": "Integer", "name": "days", "value": 91 })
        ]),
    )
    .await;
    assert_eq!(
        error_type(&s.hand("run", &pressed).await.1),
        "FailedValidation"
    );

    // a notice without an end says so
    let c = card(
        &s,
        &owner,
        &command,
        "notice",
        "send",
        notice_options(Some(&user.id)),
    )
    .await;
    assert!(
        c.view["lines"]
            .as_array()
            .unwrap()
            .contains(&json!("No end date.")),
        "{}",
        c.view
    );

    // the ticket changes between the card and the press: refused
    let t = ticket(&s, &user_session.token).await;
    let c = card(
        &s,
        &owner,
        &command,
        "ticket",
        "close",
        json!([text("ticket", &t)]),
    )
    .await;
    let response = s
        .harness
        .client
        .post(format!("/staff/support/tickets/{t}/messages"))
        .header(Header::new("x-session-token", owner.session.token.clone()))
        .header(Header::new("x-staff-session", console.clone()))
        .header(ContentType::JSON)
        .body(json!({ "body": "Looking into it" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(
        (status, error_type(&body)),
        (Status::BadRequest, "InvalidOperation"),
        "{body}"
    );
    assert!(
        s.records(&owner)
            .await
            .iter()
            .any(|r| r.1 == "Denied" && r.2 == "desk: the ticket changed since the card")
    );
    assert_ne!(
        serde_json::to_value(s.harness.db.fetch_support_ticket(&t).await.unwrap().status).unwrap(),
        json!("Closed")
    );
}
