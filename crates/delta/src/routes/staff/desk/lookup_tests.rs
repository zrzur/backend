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
        inbox: String::new(),
        inbox_channels: Default::default(),
        areas: vec![
            "overview".to_string(),
            "lookup".to_string(),
            "audit".to_string(),
        ],
    });
}

async fn register_offered(s: &Stage) -> ApplicationCommand {
    super::tests::register_served(s)
        .await
        .remove("desk")
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
    /// A view: every shown value equal to the console's; every key found
    /// in the console's answer except the `optional` ones
    View(&'static [Show], &'static [&'static str]),
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
    /// The seeded data must show: the console's list is not empty
    non_empty: bool,
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
        Compare::View(shows, optional) => {
            for s in shows.iter() {
                assert!(
                    optional.contains(&s.key) || text_of(console, s.key, Lang::En).is_some(),
                    "{what}: {} not found in {console}",
                    s.key
                );
            }
            let expected: Vec<(String, String)> = shows
                .iter()
                .filter_map(|s| {
                    text_of(console, s.key, Lang::En).map(|v| (s.label.en.to_string(), v))
                })
                .collect();
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
            assert!(
                !case.non_empty || !items.is_empty(),
                "{what}: the seeded data is missing from {console}"
            );
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
                    lines
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

/// The console as the same person
async fn console_send(s: &Stage, who: &Person, token: &str, path: &str, body: Value) -> Value {
    let response = s
        .harness
        .client
        .post(path.to_string())
        .header(Header::new("x-session-token", who.session.token.clone()))
        .header(Header::new("x-staff-session", token.to_string()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = response.status();
    let body: Value = response.into_json().await.unwrap_or(Value::Null);
    assert_eq!(status, Status::Ok, "{path}: {body}");
    body
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

    // something of every kind: a user, a report, a bug report, a ticket, a
    // bot, a user action with an appeal, a notice, a word, a creator, a
    // badge, a design, a Discover request and a bot verification request
    let (_, someone_session, someone) = s.harness.new_user().await;
    let server = s.harness.db.fetch_server(&s.server_id).await.unwrap();
    let (_, author_session, author) = s.harness.new_user().await;
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
    let bug = as_user(
        &s,
        &someone_session.token,
        "/bugs",
        json!({
            "kind": "bug", "title": "Send button stuck", "description": "The send button stays grey", "platform": "web"
        }),
    )
    .await;
    let bug_id = bug["_id"].as_str().unwrap().to_string();
    let ticket = as_user(
        &s,
        &someone_session.token,
        "/support/tickets",
        json!({
            "category": "billing", "subject": "Charged twice", "body": "I was charged twice this month."
        }),
    )
    .await;
    let ticket_id = ticket["_id"].as_str().unwrap().to_string();
    // the console's own actions; notices and creator grants need a step-up
    let response = s
        .harness
        .client
        .post("/staff/session/step-up")
        .header(Header::new("x-session-token", owner.session.token.clone()))
        .header(Header::new("x-staff-session", token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "totp_code": super::tests::code(1) }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let action = console_send(
        &s,
        &owner,
        &token,
        &format!("/staff/users/{}/actions", author.id),
        json!({ "kind": "Warning", "category": "Harassment", "public_reason": "Insults are not allowed" }),
    )
    .await;
    as_user(
        &s,
        &author_session.token,
        &format!(
            "/safety/standing/{}/appeal",
            action["_id"].as_str().unwrap()
        ),
        json!({ "statement": "I was quoting someone else here." }),
    )
    .await;
    console_send(
        &s,
        &owner,
        &token,
        "/staff/notices",
        json!({
            "user": someone.id,
            "title": { "ar": "تحديث لحسابك", "en": "An update on your account", "de": "Neuigkeiten zu deinem Konto" },
            "body": { "ar": "راجعنا طلبك.", "en": "We looked at your request.", "de": "Wir haben deine Anfrage geprüft." },
            "tone": "info"
        }),
    )
    .await;
    console_send(
        &s,
        &owner,
        &token,
        "/staff/words",
        json!({ "pattern": "badword", "category": "Spam", "allow": false, "reason": "Spam wave" }),
    )
    .await;
    console_send(
        &s,
        &owner,
        &token,
        &format!("/staff/creators/{}", someone.id),
        json!({ "action": "grant", "reason": "Makes good effects" }),
    )
    .await;
    console_send(
        &s,
        &owner,
        &token,
        &format!("/staff/users/{}/badges/grant", someone.id),
        json!({ "badge": "community_helper", "reason": "Helps in the community" }),
    )
    .await;
    let design: revolt_database::CommunityItem = serde_json::from_value(json!({
        "_id": ulid::Ulid::new().to_string(),
        "kind": "Effect",
        "name": "Snow",
        "creator_id": someone.id,
        "file": {
            "_id": ulid::Ulid::new().to_string(),
            "tag": "effects",
            "filename": "snow.gif",
            "metadata": { "type": "Image", "width": 576, "height": 1024 },
            "content_type": "image/gif",
            "size": 100
        },
        "status": "PendingReview",
        "revision": 1,
        "rights": { "terms_version": "test", "confirmed_at": 1 }
    }))
    .unwrap();
    s.harness.db.insert_community_item(&design).await.unwrap();
    let (_, listed_owner_session, listed_owner) = s.harness.new_user().await;
    let (listed, _) = s.harness.new_server(&listed_owner).await;
    let response = s
        .harness
        .client
        .put(format!("/servers/{}/discover", listed.id))
        .header(Header::new(
            "x-session-token",
            listed_owner_session.token.clone(),
        ))
        .header(ContentType::JSON)
        .body(r##"{"tags":["Gaming"],"category":"gaming","language":"ar"}"##)
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let (_, bot_owner_session, bot_owner) = s.harness.new_user().await;
    let (verified_bot, _) = s.harness.new_bot(&bot_owner).await;
    // the Verified Bot Developer numbers ask for one legitimate server
    s.harness.legitimate_server_for(&verified_bot.id).await;
    as_user(
        &s,
        &bot_owner_session.token,
        &format!("/bots/{}/verification", verified_bot.id),
        json!({ "statement": "Moderation helper used in 40 Arabic communities.", "website": "https://example.org", "privacy_policy_url": "https://example.org/privacy", "terms_of_service_url": "https://example.org/terms" }),
    )
    .await;

    let bot_id = s.bot.id.clone();
    let user_id = someone.id.clone();
    let server_id = s.server_id.clone();
    let owner_id = owner.user.id.clone();

    use Compare::*;
    let list = |group: &'static str,
                name: &'static str,
                options: Value,
                console: String,
                pointer: &'static str,
                shows: &'static [Show],
                non_empty: bool| Case {
        group,
        name,
        options,
        console,
        compare: List(pointer, shows),
        non_empty,
    };
    let view = |group: &'static str,
                name: &'static str,
                options: Value,
                console: String,
                shows: &'static [Show],
                optional: &'static [&'static str]| Case {
        group,
        name,
        options,
        console,
        compare: View(shows, optional),
        non_empty: false,
    };
    let labels =
        |group: &'static str, name: &'static str, console: &str, shows: &'static [Show]| Case {
            group,
            name,
            options: json!([]),
            console: console.to_string(),
            compare: Labels(shows),
            non_empty: false,
        };
    let user = |id: &str| json!([{ "type": "User", "name": "user", "value": id }]);
    let id = |name: &str, id: &str| json!([{ "type": "String", "name": name, "value": id }]);
    let cases = vec![
        list(
            "user",
            "find",
            json!([{ "type": "String", "name": "query", "value": someone.username }]),
            format!("/staff/users?q={}&limit=50", someone.username),
            "",
            lookup::USER_ROW,
            true,
        ),
        view(
            "user",
            "view",
            user(&user_id),
            format!("/staff/users/{user_id}"),
            lookup::USER_VIEW,
            &["/display_name", "/suspended_until"],
        ),
        list(
            "user",
            "badges",
            user(&user_id),
            format!("/staff/users/{user_id}/badges"),
            "/grants",
            lookup::BADGE_ROW,
            true,
        ),
        list(
            "user",
            "history",
            user(&author.id),
            format!("/staff/safety/users/{}", author.id),
            "/actions",
            lookup::ACTION_ROW,
            true,
        ),
        list(
            "server",
            "find",
            json!([]),
            "/staff/servers?limit=50".to_string(),
            "",
            lookup::SERVER_ROW,
            true,
        ),
        view(
            "server",
            "view",
            id("server", &server_id),
            format!("/staff/servers/{server_id}"),
            lookup::SERVER_VIEW,
            &["/description"],
        ),
        list(
            "bot",
            "find",
            json!([]),
            "/staff/bots?limit=50".to_string(),
            "",
            lookup::BOT_ROW,
            true,
        ),
        view(
            "bot",
            "view",
            id("bot", &bot_id),
            format!("/staff/bots/{bot_id}"),
            lookup::BOT_VIEW,
            &[
                "/interactions_url",
                "/terms_of_service_url",
                "/privacy_policy_url",
                "/verifications",
            ],
        ),
        list(
            "case",
            "list",
            json!([]),
            "/staff/safety/cases?limit=50".to_string(),
            "/cases",
            lookup::CASE_ROW,
            true,
        ),
        view(
            "case",
            "view",
            id("case", &case_id),
            format!("/staff/safety/cases/{case_id}"),
            lookup::CASE_VIEW,
            &["/case/assignee_id", "/actions", "/appeals", "/related"],
        ),
        list(
            "appeal",
            "list",
            json!([]),
            "/staff/safety/appeals?limit=50".to_string(),
            "",
            lookup::APPEAL_ROW,
            true,
        ),
        list(
            "ticket",
            "list",
            json!([{ "type": "String", "name": "status", "value": "Open" }]),
            "/staff/support/tickets?status=Open".to_string(),
            "",
            lookup::TICKET_ROW,
            true,
        ),
        view(
            "ticket",
            "view",
            id("ticket", &ticket_id),
            format!("/staff/support/tickets/{ticket_id}"),
            lookup::TICKET_VIEW,
            &["/assignee", "/closed_at"],
        ),
        list(
            "bug",
            "list",
            json!([]),
            "/staff/bugs".to_string(),
            "",
            lookup::BUG_ROW,
            true,
        ),
        view(
            "bug",
            "view",
            id("bug", &bug_id),
            format!("/staff/bugs/{bug_id}"),
            lookup::BUG_VIEW,
            &["/report/app_version", "/possible_duplicates"],
        ),
        labels("system", "health", "/staff/health", lookup::HEALTH_VIEW),
        labels(
            "system",
            "metrics",
            "/staff/operations/metrics",
            lookup::METRICS_VIEW,
        ),
        list(
            "staff",
            "list",
            json!([]),
            "/staff/members".to_string(),
            "",
            lookup::STAFF_ROW,
            true,
        ),
        list(
            "queue",
            "creators",
            json!([]),
            "/staff/creators".to_string(),
            "",
            lookup::CREATOR_ROW,
            true,
        ),
        list(
            "queue",
            "designs",
            json!([]),
            "/staff/designs/review".to_string(),
            "/pending",
            lookup::DESIGN_ROW,
            true,
        ),
        list(
            "queue",
            "discover",
            json!([]),
            "/staff/discover/requests".to_string(),
            "",
            lookup::DISCOVER_ROW,
            true,
        ),
        list(
            "queue",
            "verifications",
            json!([]),
            "/staff/bot-verifications".to_string(),
            "",
            lookup::VERIFICATION_ROW,
            true,
        ),
        list(
            "words",
            "list",
            json!([]),
            "/staff/words".to_string(),
            "",
            lookup::WORD_ROW,
            true,
        ),
        list(
            "notices",
            "list",
            json!([]),
            "/staff/notices".to_string(),
            "",
            lookup::NOTICE_ROW,
            true,
        ),
        // a test database has no orders
        list(
            "billing",
            "summary",
            json!([]),
            "/staff/billing/summary".to_string(),
            "/orders_by_status",
            lookup::ORDER_ROW,
            false,
        ),
        list(
            "audit",
            "search",
            json!([{ "type": "User", "name": "actor", "value": owner_id }]),
            format!("/staff/audit?actor={owner_id}&limit=50"),
            "",
            lookup::AUDIT_ROW,
            true,
        ),
        labels(
            "audit",
            "verify",
            "/staff/audit/verify",
            lookup::VERIFY_VIEW,
        ),
        labels(
            "security",
            "summary",
            "/staff/security",
            lookup::SECURITY_VIEW,
        ),
    ];

    for case in &cases {
        let what = format!("{} {}", case.group, case.name);
        assert!(
            find("desk", Some(case.group), case.name).is_some(),
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

/// The plugin queue follows the console's plugin switch: while plugins are
/// off it is not in the tree at all and a press on it is refused; while on,
/// it gives the console's data
#[rocket::async_test]
async fn desk_plugin_queue_follows_the_console() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let token = console_token(&s, &owner, 0).await;
    let command = register_offered(&s).await;
    let offered = command
        .options
        .iter()
        .find(|o| o.name == "queue")
        .and_then(|o| o.options.as_ref())
        .is_some_and(|subs| subs.iter().any(|sub| sub.name == "plugins"));

    let pressed = press(&s, &owner, &command, "queue", "plugins", json!([]), None).await;
    let (status, desk) = s.hand("run", &pressed).await;
    let (console_status, from_console) = console(&s, &owner, &token, "/staff/plugins/review").await;
    if console_status == Status::Ok {
        assert!(offered, "plugins are on: the queue is offered");
        assert_eq!(status, Status::Ok, "{desk}");
        let case = Case {
            group: "queue",
            name: "plugins",
            options: json!([]),
            console: String::new(),
            compare: Compare::List("", lookup::PLUGIN_ROW),
            non_empty: false,
        };
        check(&case, &desk, &from_console);
    } else {
        assert_eq!(error_type(&from_console), "PluginsDisabled");
        assert!(!offered, "plugins are off: the queue is not in the tree");
        assert_eq!(error_type(&desk), "InvalidOperation", "{desk}");
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
    // the rows, then the page line (part of the text)
    let lines = page1["lines"].as_array().unwrap();
    assert_eq!(lines.len(), PAGE_SIZE + 1);
    assert_eq!(lines[PAGE_SIZE], json!(format!("1/{pages}")));
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

    // each refusal is recorded as the presser, with its reason
    let denied = |records: Vec<(String, String, String)>| -> Vec<String> {
        records
            .into_iter()
            .filter(|r| r.0 == "staff.desk.page" && r.1 == "Denied")
            .map(|r| r.2)
            .collect()
    };
    assert_eq!(
        denied(s.records(&other).await),
        vec!["desk: not a command of the presser".to_string()]
    );
    assert_eq!(
        denied(s.records(&owner).await),
        vec![
            "desk: not pressed on an answer of that command".to_string(),
            "desk: page out of range".to_string()
        ]
    );

    // a changing command has no pages, not even page 1
    let change = press(
        &s,
        &owner,
        &command,
        "test",
        "change",
        json!([
            { "type": "User", "name": "user", "value": owner.user.id },
            { "type": "String", "name": "note", "value": "n" }
        ]),
        None,
    )
    .await;
    let paged = page_press(
        &s,
        &owner,
        &format!("{PAGE_PREFIX}{}:1", change.0),
        &change.0,
    )
    .await;
    let (_, body) = s.hand("run", &paged).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");
    assert_eq!(
        denied(s.records(&owner).await).last().map(String::as_str),
        Some("desk: a changing command has no pages")
    );
}
