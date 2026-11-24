//! Desk lookups: every read-only command against the console route it
//! shares a core with. Allowed for a holder of the permission, refused (as
//! the presser, recorded) without it, and the same data as the console.

use revolt_config::FeaturesStaffDesk;
use revolt_database::{Interaction, InteractionSource, StaffRole};
use revolt_models::v0::{ApplicationCommand, InteractionType};
use rocket::http::{ContentType, Header, Status};
use serde_json::{Value, json};

use super::lookup::{self, PAGE_PREFIX, PAGE_SIZE, Show, line, text_of};
use super::registry::{Lang, find};
use super::test_settings;
use super::tests::{Person, Stage, error_type, stage};

/// Every area on for the stage's bot
fn all_areas(s: &Stage) {
    test_settings::register(FeaturesStaffDesk {
        enabled: true,
        bot: s.bot.id.clone(),
        places: vec![s.place.id().to_string()],
        allow_dms: false,
        areas: vec![
            "overview".to_string(),
            "lookup".to_string(),
            "audit".to_string(),
        ],
    });
}

async fn register_offered(s: &Stage) -> ApplicationCommand {
    let response = s
        .harness
        .client
        .get("/staff/desk/commands")
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let tree: Value = response.into_json().await.unwrap();
    revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        None,
        serde_json::from_value(tree).unwrap(),
    )
    .await
    .unwrap()
}

/// `/desk <group> <name> [options]` as `/interactions` stores it
async fn press(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    group: &str,
    name: &str,
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
    interaction.locale = locale.map(str::to_string);
    interaction.command_options = serde_json::from_value(json!([{
        "type": "SubCommandGroup", "name": group,
        "options": [{ "type": "SubCommand", "name": name, "options": options }]
    }]))
    .unwrap();
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    (interaction.id, token)
}

/// A press on a page button `custom_id`, on a private answer of `answer_of`
async fn page_press(s: &Stage, who: &Person, custom_id: &str, answer_of: &str) -> (String, String) {
    let (mut interaction, token) = Interaction::new(
        InteractionType::Component,
        s.bot.id.clone(),
        who.user.id.clone(),
        Some(s.server_id.clone()),
        s.place.id().to_string(),
        ulid::Ulid::new().to_string(),
    );
    interaction.session_id = Some(who.session.id.clone());
    interaction.custom_id = Some(custom_id.to_string());
    interaction.source = Some(InteractionSource::Ephemeral {
        interaction_id: answer_of.to_string(),
        message_id: ulid::Ulid::new().to_string(),
    });
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    (interaction.id, token)
}

/// The console's answer for the same person
async fn console(s: &Stage, who: &Person, token: &str, path: &str) -> (Status, Value) {
    let response = s
        .harness
        .client
        .get(path.to_string())
        .header(Header::new("x-session-token", who.session.token.clone()))
        .header(Header::new("x-staff-session", token.to_string()))
        .dispatch()
        .await;
    let status = response.status();
    (status, response.into_json().await.unwrap_or(Value::Null))
}

async fn console_token(s: &Stage, who: &Person, offset: i64) -> String {
    let response = s
        .harness
        .client
        .post("/staff/session")
        .header(Header::new("x-session-token", who.session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "totp_code": super::tests::code(offset) }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let body: Value = response.into_json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

/// As a person through the public API
async fn as_user(s: &Stage, session: &str, path: &str, body: Value) -> Value {
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

/// How the desk answer is compared with the console's
enum Compare {
    /// A view: every shown value equal to the console's
    View(&'static [Show]),
    /// A list at this pointer of the console's answer (empty: the answer)
    List(&'static str, &'static [Show]),
    /// Numbers that move between two calls (counters, latencies): the same
    /// field names only
    Labels(&'static [Show]),
}

struct Case {
    group: &'static str,
    name: &'static str,
    options: Value,
    console: String,
    compare: Compare,
}

fn field_names(view: &Value) -> Vec<String> {
    view["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect()
}

fn check(case: &Case, desk: &Value, console: &Value) {
    let what = format!("{} {}", case.group, case.name);
    match &case.compare {
        Compare::View(shows) => {
            let expected: Vec<(String, String)> = shows
                .iter()
                .filter_map(|s| {
                    text_of(console, s.key, Lang::En).map(|v| (s.label.en.to_string(), v))
                })
                .collect();
            assert!(
                expected.len() >= 2,
                "{what}: too few values found in {console}"
            );
            let got: Vec<(String, String)> = desk["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    (
                        f["name"].as_str().unwrap().to_string(),
                        f["value"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            assert_eq!(got, expected, "{what}");
        }
        Compare::List(pointer, shows) => {
            let items = if pointer.is_empty() {
                console.clone()
            } else {
                console.pointer(pointer).cloned().unwrap_or(Value::Null)
            };
            let items = items.as_array().cloned().unwrap_or_default();
            let lines: Vec<String> = desk["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap().to_string())
                .collect();
            if items.is_empty() {
                assert!(
                    lines.contains(&"Nothing found.".to_string()),
                    "{what}: {desk}"
                );
            } else {
                let expected: Vec<String> = items
                    .iter()
                    .take(PAGE_SIZE)
                    .map(|i| line(i, shows, Lang::En))
                    .collect();
                assert!(
                    lines.ends_with(&expected)
                        || lines
                            .windows(expected.len())
                            .any(|w| w == expected.as_slice()),
                    "{what}: {lines:?} vs {expected:?}"
                );
                assert!(
                    expected.iter().all(|l| !l.is_empty()),
                    "{what}: empty line from {items:?}"
                );
            }
        }
        Compare::Labels(shows) => {
            let names = field_names(desk);
            let expected: Vec<String> = shows
                .iter()
                .filter(|s| text_of(console, s.key, Lang::En).is_some())
                .map(|s| s.label.en.to_string())
                .collect();
            assert!(!expected.is_empty(), "{what}: nothing found in {console}");
            assert_eq!(names, expected, "{what}");
        }
    }
}

/// Every lookup: an Owner gets the console's data; an Analyst (no lookup
/// permission) is refused as themselves, with a Denied record
#[rocket::async_test]
async fn desk_lookups_match_the_console() {
    use revolt_database::Member;

    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let analyst = s.person(vec![StaffRole::Analyst]).await;
    s.sign_in(&owner, -1).await;
    s.sign_in(&analyst, -1).await;
    let token = console_token(&s, &owner, 0).await;
    let command = register_offered(&s).await;

    // something to find: a user, a report, a bug report, a ticket, a bot
    let (_, someone_session, someone) = s.harness.new_user().await;
    let server = s.harness.db.fetch_server(&s.server_id).await.unwrap();
    let (_, _, author) = s.harness.new_user().await;
    let (_, _, message) = s
        .harness
        .new_message(&author, &server, vec![s.place.clone()])
        .await;
    Member::create(&s.harness.db, &server, &someone, None)
        .await
        .unwrap();
    let report = as_user(
        &s,
        &someone_session.token,
        "/safety/reports",
        json!({
            "target": { "kind": "Message", "id": message.id },
            "category": "Harassment", "subcategory": "targeted", "description": "keeps insulting me"
        }),
    )
    .await;
    let case_id = report["_id"].as_str().unwrap().to_string();
    let bug = as_user(&s, &someone_session.token, "/bugs", json!({
        "kind": "bug", "title": "Send button stuck", "description": "The send button stays grey", "platform": "web"
    }))
    .await;
    let bug_id = bug["_id"].as_str().unwrap().to_string();
    let ticket = as_user(&s, &someone_session.token, "/support/tickets", json!({
        "category": "billing", "subject": "Charged twice", "body": "I was charged twice this month."
    }))
    .await;
    let ticket_id = ticket["_id"].as_str().unwrap().to_string();
    let bot_id = s.bot.id.clone();
    let user_id = someone.id.clone();
    let server_id = s.server_id.clone();
    let owner_id = owner.user.id.clone();

    use Compare::*;
    let cases = vec![
        Case {
            group: "user",
            name: "find",
            options: json!([{ "type": "String", "name": "query", "value": someone.username }]),
            console: format!("/staff/users?q={}&limit=50", someone.username),
            compare: List("", lookup::USER_ROW),
        },
        Case {
            group: "user",
            name: "view",
            options: json!([{ "type": "User", "name": "user", "value": user_id }]),
            console: format!("/staff/users/{user_id}"),
            compare: View(lookup::USER_VIEW),
        },
        Case {
            group: "user",
            name: "badges",
            options: json!([{ "type": "User", "name": "user", "value": user_id }]),
            console: format!("/staff/users/{user_id}/badges"),
            compare: List("/grants", lookup::BADGE_ROW),
        },
        Case {
            group: "user",
            name: "history",
            options: json!([{ "type": "User", "name": "user", "value": author.id }]),
            console: format!("/staff/safety/users/{}", author.id),
            compare: List("/actions", lookup::ACTION_ROW),
        },
        Case {
            group: "server",
            name: "find",
            options: json!([]),
            console: "/staff/servers?limit=50".to_string(),
            compare: List("", lookup::SERVER_ROW),
        },
        Case {
            group: "server",
            name: "view",
            options: json!([{ "type": "String", "name": "server", "value": server_id }]),
            console: format!("/staff/servers/{server_id}"),
            compare: View(lookup::SERVER_VIEW),
        },
        Case {
            group: "bot",
            name: "find",
            options: json!([]),
            console: "/staff/bots?limit=50".to_string(),
            compare: List("", lookup::BOT_ROW),
        },
        Case {
            group: "bot",
            name: "view",
            options: json!([{ "type": "String", "name": "bot", "value": bot_id }]),
            console: format!("/staff/bots/{bot_id}"),
            compare: View(lookup::BOT_VIEW),
        },
        Case {
            group: "case",
            name: "list",
            options: json!([]),
            console: "/staff/safety/cases?limit=50".to_string(),
            compare: List("/cases", lookup::CASE_ROW),
        },
        Case {
            group: "case",
            name: "view",
            options: json!([{ "type": "String", "name": "case", "value": case_id }]),
            console: format!("/staff/safety/cases/{case_id}"),
            compare: View(lookup::CASE_VIEW),
        },
        Case {
            group: "appeal",
            name: "list",
            options: json!([]),
            console: "/staff/safety/appeals?limit=50".to_string(),
            compare: List("", lookup::APPEAL_ROW),
        },
        Case {
            group: "ticket",
            name: "list",
            options: json!([{ "type": "String", "name": "status", "value": "Open" }]),
            console: "/staff/support/tickets?status=Open".to_string(),
            compare: List("", lookup::TICKET_ROW),
        },
        Case {
            group: "ticket",
            name: "view",
            options: json!([{ "type": "String", "name": "ticket", "value": ticket_id }]),
            console: format!("/staff/support/tickets/{ticket_id}"),
            compare: View(lookup::TICKET_VIEW),
        },
        Case {
            group: "bug",
            name: "list",
            options: json!([]),
            console: "/staff/bugs".to_string(),
            compare: List("", lookup::BUG_ROW),
        },
        Case {
            group: "bug",
            name: "view",
            options: json!([{ "type": "String", "name": "bug", "value": bug_id }]),
            console: format!("/staff/bugs/{bug_id}"),
            compare: View(lookup::BUG_VIEW),
        },
        Case {
            group: "system",
            name: "health",
            options: json!([]),
            console: "/staff/health".to_string(),
            compare: Labels(lookup::HEALTH_VIEW),
        },
        Case {
            group: "system",
            name: "metrics",
            options: json!([]),
            console: "/staff/operations/metrics".to_string(),
            compare: Labels(lookup::METRICS_VIEW),
        },
        Case {
            group: "staff",
            name: "list",
            options: json!([]),
            console: "/staff/members".to_string(),
            compare: List("", lookup::STAFF_ROW),
        },
        Case {
            group: "queue",
            name: "creators",
            options: json!([]),
            console: "/staff/creators".to_string(),
            compare: List("", lookup::CREATOR_ROW),
        },
        Case {
            group: "queue",
            name: "designs",
            options: json!([]),
            console: "/staff/designs/review".to_string(),
            compare: List("/pending", lookup::DESIGN_ROW),
        },
        Case {
            group: "queue",
            name: "discover",
            options: json!([]),
            console: "/staff/discover/requests".to_string(),
            compare: List("", lookup::DISCOVER_ROW),
        },
        Case {
            group: "queue",
            name: "verifications",
            options: json!([]),
            console: "/staff/bot-verifications".to_string(),
            compare: List("", lookup::VERIFICATION_ROW),
        },
        Case {
            group: "words",
            name: "list",
            options: json!([]),
            console: "/staff/words".to_string(),
            compare: List("", lookup::WORD_ROW),
        },
        Case {
            group: "notices",
            name: "list",
            options: json!([]),
            console: "/staff/notices".to_string(),
            compare: List("", lookup::NOTICE_ROW),
        },
        Case {
            group: "billing",
            name: "summary",
            options: json!([]),
            console: "/staff/billing/summary".to_string(),
            compare: List("/orders_by_status", lookup::ORDER_ROW),
        },
        Case {
            group: "audit",
            name: "search",
            options: json!([{ "type": "User", "name": "actor", "value": owner_id }]),
            console: format!("/staff/audit?actor={owner_id}&limit=50"),
            compare: List("", lookup::AUDIT_ROW),
        },
        Case {
            group: "audit",
            name: "verify",
            options: json!([]),
            console: "/staff/audit/verify".to_string(),
            compare: Labels(lookup::VERIFY_VIEW),
        },
        Case {
            group: "security",
            name: "summary",
            options: json!([]),
            console: "/staff/security".to_string(),
            compare: Labels(lookup::SECURITY_VIEW),
        },
    ];

    for case in &cases {
        let what = format!("{} {}", case.group, case.name);
        assert!(
            find(Some(case.group), case.name).is_some(),
            "{what} is not registered"
        );

        // allowed, and the console's data
        let pressed = press(
            &s,
            &owner,
            &command,
            case.group,
            case.name,
            case.options.clone(),
            None,
        )
        .await;
        let (status, desk) = s.hand("run", &pressed).await;
        assert_eq!(status, Status::Ok, "{what}: {desk}");
        assert!(
            desk["content"].as_str().is_some_and(|c| !c.is_empty()),
            "{what}"
        );
        let (console_status, from_console) = console(&s, &owner, &token, &case.console).await;
        assert_eq!(console_status, Status::Ok, "{what}: {from_console}");
        check(case, &desk, &from_console);

        // refused without the permission, as the presser, recorded
        let before = s.records(&analyst).await.len();
        let pressed = press(
            &s,
            &analyst,
            &command,
            case.group,
            case.name,
            case.options.clone(),
            None,
        )
        .await;
        let (status, body) = s.hand("run", &pressed).await;
        assert_eq!(status, Status::Forbidden, "{what}: {body}");
        assert_eq!(error_type(&body), "MissingPlatformPermission", "{what}");
        let records = s.records(&analyst).await;
        assert_eq!(records.len(), before + 1, "{what}");
        assert_eq!(
            (records[before].0.as_str(), records[before].1.as_str()),
            ("staff.access.denied", "Denied"),
            "{what}"
        );
    }

    // every registered lookup is covered (the plugin queue has its own test)
    for entry in
        super::registry::entries().filter(|e| matches!(e.area, Some("lookup") | Some("audit")))
    {
        let covered = cases
            .iter()
            .any(|c| Some(c.group) == entry.group && c.name == entry.name)
            || (entry.group == Some("queue") && entry.name == "plugins");
        assert!(covered, "{} has no test", entry.key());
    }
}

/// The plugin queue: the console's own plugin switch, then the console's data
#[rocket::async_test]
async fn desk_plugin_queue_follows_the_console() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let token = console_token(&s, &owner, 0).await;
    let command = register_offered(&s).await;

    let pressed = press(&s, &owner, &command, "queue", "plugins", json!([]), None).await;
    let (status, desk) = s.hand("run", &pressed).await;
    let (console_status, from_console) = console(&s, &owner, &token, "/staff/plugins/review").await;
    assert_eq!(status, console_status, "{desk} / {from_console}");
    if status == Status::Ok {
        let case = Case {
            group: "queue",
            name: "plugins",
            options: json!([]),
            console: String::new(),
            compare: Compare::List("", lookup::PLUGIN_ROW),
        };
        check(&case, &desk, &from_console);
    } else {
        assert_eq!(error_type(&desk), error_type(&from_console));
    }
}

/// Personal data only as the console shows it: an email only with
/// UsersPiiRead, and that look is recorded
#[rocket::async_test]
async fn desk_user_view_email_only_with_pii_read() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let support = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&owner, -1).await;
    s.sign_in(&support, -1).await;
    let command = register_offered(&s).await;
    let (_, _, someone) = s.harness.new_user().await;
    let options = json!([{ "type": "User", "name": "user", "value": someone.id }]);

    let pressed = press(
        &s,
        &support,
        &command,
        "user",
        "view",
        options.clone(),
        None,
    )
    .await;
    let (status, view) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{view}");
    assert!(!field_names(&view).contains(&"Email".to_string()), "{view}");
    assert!(
        s.records(&support)
            .await
            .iter()
            .all(|r| r.0 != "staff.user.view_pii")
    );

    let pressed = press(&s, &owner, &command, "user", "view", options, None).await;
    let (status, view) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{view}");
    assert!(field_names(&view).contains(&"Email".to_string()), "{view}");
    assert!(
        s.records(&owner)
            .await
            .iter()
            .any(|r| r.0 == "staff.user.view_pii" && r.1 == "Success")
    );
}

/// Opening a case is recorded, like in the console
#[rocket::async_test]
async fn desk_case_view_is_recorded() {
    use revolt_database::Member;

    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_offered(&s).await;
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
    let id = report["_id"].as_str().unwrap();

    let pressed = press(
        &s,
        &owner,
        &command,
        "case",
        "view",
        json!([{ "type": "String", "name": "case", "value": id }]),
        None,
    )
    .await;
    let (status, view) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{view}");
    let session = s.desk_sessions(&owner).await.remove(0);
    let viewed: Vec<_> = s
        .record_sessions(&owner)
        .await
        .into_iter()
        .filter(|r| r.0 == "staff.safety.case.view" && r.1 == "Success")
        .collect();
    assert_eq!(viewed.len(), 1, "{viewed:?}");
    assert_eq!(viewed[0].2.as_deref(), Some(session.id.as_str()));
}

/// Lists page: page 1 with Next, the Next press (handed back to `/run`)
/// gives page 2 of the same command; a page press made up for another
/// person's command, or not on an answer of it, is refused; in Arabic the
/// words are Arabic
#[rocket::async_test]
async fn desk_lists_page() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let other = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    s.sign_in(&other, -1).await;
    let command = register_offered(&s).await;
    for _ in 0..(PAGE_SIZE + 3) {
        s.harness.new_user().await;
    }

    let first = press(&s, &owner, &command, "user", "find", json!([]), Some("ar")).await;
    let (status, page1) = s.hand("run", &first).await;
    assert_eq!(status, Status::Ok, "{page1}");
    assert_eq!(page1["title"], json!("المستخدمون"));
    assert_eq!(page1["page"], json!(1));
    let pages = page1["pages"].as_u64().unwrap();
    assert!(pages >= 2, "{page1}");
    assert_eq!(page1["lines"].as_array().unwrap().len(), PAGE_SIZE);
    let next = page1["buttons"].as_array().unwrap().last().unwrap()["action_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(next, format!("{PAGE_PREFIX}{}:2", first.0));

    // the Next press on the first answer
    let pressed = page_press(&s, &owner, &next, &first.0).await;
    let (status, page2) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{page2}");
    assert_eq!(page2["page"], json!(2));
    assert_eq!(page2["title"], json!("المستخدمون"));
    assert_ne!(page2["lines"], page1["lines"]);
    // a press on that second answer works too
    let back = page2["buttons"].as_array().unwrap()[0]["action_id"]
        .as_str()
        .unwrap()
        .to_string();
    let again = page_press(&s, &owner, &back, &pressed.0).await;
    let (status, back_page) = s.hand("run", &again).await;
    assert_eq!(status, Status::Ok, "{back_page}");
    assert_eq!(back_page["lines"], page1["lines"]);

    // another person cannot page someone else's command
    let forged = page_press(&s, &other, &next, &first.0).await;
    let (_, body) = s.hand("run", &forged).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");
    // nor a press that is not on an answer of that command
    let elsewhere = press(&s, &owner, &command, "user", "find", json!([]), None).await;
    let stray = page_press(&s, &owner, &next, &elsewhere.0).await;
    let (_, body) = s.hand("run", &stray).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");
    // a page number out of range is refused
    let wild = page_press(&s, &owner, &format!("{PAGE_PREFIX}{}:0", first.0), &first.0).await;
    let (_, body) = s.hand("run", &wild).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");
}
