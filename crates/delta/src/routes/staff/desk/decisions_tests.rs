//! Desk reviews (`/desk-review`): every decision runs as the console's own
//! core with the console's records on the desk session; refused without the
//! permission; a fresh code where the console steps up, and for removals
//! and every change of the official mark.

use revolt_database::StaffRole;
use revolt_models::v0::ApplicationCommand;
use rocket::http::{ContentType, Header, Status};
use serde_json::{Value, json};

use super::moderation_tests::{
    all_areas, as_user, card, confirm, press, recorded, step_up, text, user_opt,
};
use super::tests::{Stage, error_type, register_served, stage};

async fn register_review(s: &Stage) -> ApplicationCommand {
    register_served(s).await.remove("desk-review").unwrap()
}

/// A design waiting for review, by `creator`
async fn design(s: &Stage, creator: &str) -> String {
    let id = ulid::Ulid::new().to_string();
    let item: revolt_database::CommunityItem = serde_json::from_value(json!({
        "_id": id,
        "kind": "Effect",
        "name": "Snow",
        "creator_id": creator,
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
    s.harness.db.insert_community_item(&item).await.unwrap();
    id
}

fn field(view: &Value, name: &str) -> String {
    view["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!(name))
        .map(|f| f["value"].as_str().unwrap().to_string())
        .unwrap_or_default()
}

/// Every review command, confirmed: the console's record on the desk
/// session and the change made; the guarded ones wait for a fresh code
#[rocket::async_test]
async fn desk_reviews_run_the_consoles_actions() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let second = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    s.sign_in(&second, -1).await;
    let command = register_review(&s).await;

    // something to decide on
    let (_, listed_session, listed_owner) = s.harness.new_user().await;
    let (listed, _) = s.harness.new_server(&listed_owner).await;
    let response = s
        .harness
        .client
        .put(format!("/servers/{}/discover", listed.id))
        .header(Header::new("x-session-token", listed_session.token.clone()))
        .header(ContentType::JSON)
        .body(r##"{"tags":["Gaming"],"category":"gaming","language":"ar"}"##)
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let (_, bot_owner_session, bot_owner) = s.harness.new_user().await;
    let (bot, _) = s.harness.new_bot(&bot_owner).await;
    // the Verified Bot Developer numbers ask for one legitimate server
    s.harness.legitimate_server_for(&bot.id).await;
    let applied = as_user(
        &s,
        &bot_owner_session.token,
        &format!("/bots/{}/verification", bot.id),
        json!({ "statement": "Moderation helper used in 40 Arabic communities.", "website": "https://example.org", "privacy_policy_url": "https://example.org/privacy", "terms_of_service_url": "https://example.org/terms" }),
    )
    .await;
    let request_id = applied["request"]["_id"].as_str().unwrap().to_string();
    let (_, _, someone) = s.harness.new_user().await;
    let design_id = design(&s, &someone.id).await;

    // ---- without a fresh code ----
    for (group, name, options, record) in [
        (
            "discover",
            "decide",
            json!([
                text("kind", "server"),
                text("id", &listed.id),
                text("decision", "Approve")
            ]),
            "staff.discover.decide",
        ),
        (
            "verification",
            "decide",
            json!([text("request", &request_id), text("decision", "approve")]),
            "staff.bot.verification",
        ),
        (
            "creator",
            "change",
            json!([
                user_opt(&someone.id),
                text("action", "grant"),
                text("reason", "Makes good effects")
            ]),
            "staff.creator.change",
        ),
        (
            "words",
            "add",
            json!([
                text("pattern", "badword"),
                text("category", "Spam"),
                text("reason", "Spam wave")
            ]),
            "platform.words.add",
        ),
        (
            "badge",
            "give",
            json!([
                user_opt(&someone.id),
                text("badge", "community_helper"),
                text("reason", "Official partner")
            ]),
            "staff.desk.badge.grant",
        ),
    ] {
        let c = card(&s, &owner, &command, group, name, options).await;
        assert!(!c.danger(), "{group} {name}: {}", c.view);
        let (status, body) = confirm(&s, &owner, &c).await;
        assert_eq!(status, Status::Ok, "{group} {name}: {body}");
        recorded(&s, &owner, record).await;
    }
    let discovered = s
        .harness
        .db
        .fetch_discover_request_by_item_id(revolt_database::DiscoverRequestType::Server, &listed.id)
        .await
        .unwrap();
    // the status names `Approved` (it carries the decision's details)
    assert!(
        serde_json::to_value(&discovered.status)
            .unwrap()
            .to_string()
            .contains("Approved"),
        "{:?}",
        discovered.status
    );
    // the word list, checked from the desk
    let pressed = press(
        &s,
        &owner,
        &command,
        "words",
        "check",
        json!([text("text", "this is badword spam")]),
    )
    .await;
    let (status, checked) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{checked}");
    assert_eq!(field(&checked, "The list says"), "Spam", "{checked}");
    let word = s
        .harness
        .db
        .fetch_platform_words()
        .await
        .unwrap()
        .into_iter()
        .find(|w| w.pattern == "badword")
        .unwrap();
    let word_card = card(
        &s,
        &owner,
        &command,
        "words",
        "remove",
        json!([text("word", &word.id), text("reason", "Too broad")]),
    )
    .await;
    let unverify = card(
        &s,
        &owner,
        &command,
        "bot",
        "unverify",
        json!([text("bot", &bot.id), text("reason", "Verified by mistake")]),
    )
    .await;
    let badge_remove = card(
        &s,
        &owner,
        &command,
        "badge",
        "remove",
        json!([
            user_opt(&someone.id),
            text("badge", "community_helper"),
            text("reason", "Partnership ended")
        ]),
    )
    .await;

    // nobody decides on their own design
    let own = design(&s, &owner.user.id).await;
    let pressed = press(
        &s,
        &owner,
        &command,
        "design",
        "review",
        json!([text("design", &own), text("action", "approve")]),
    )
    .await;
    assert_eq!(
        error_type(&s.hand("run", &pressed).await.1),
        "NotPrivileged"
    );

    // ---- with a fresh code: cards first, then the code ----
    let design_card = card(
        &s,
        &owner,
        &command,
        "design",
        "review",
        json!([text("design", &design_id), text("action", "approve")]),
    )
    .await;
    let remove = card(
        &s,
        &owner,
        &command,
        "discover",
        "decide",
        json!([
            text("kind", "server"),
            text("id", &listed.id),
            text("decision", "Remove"),
            text("reason", "Not a gaming server")
        ]),
    )
    .await;
    let revoke = card(
        &s,
        &owner,
        &command,
        "creator",
        "change",
        json!([
            user_opt(&someone.id),
            text("action", "revoke"),
            text("reason", "Copied other people's work")
        ]),
    )
    .await;
    let staff_badge = card(
        &s,
        &owner,
        &command,
        "badge",
        "give",
        json!([
            user_opt(&someone.id),
            text("badge", "developer"),
            text("reason", "Joined the team")
        ]),
    )
    .await;
    let propose = card(
        &s,
        &owner,
        &command,
        "official",
        "propose",
        json!([
            user_opt(&someone.id),
            text("action", "give"),
            text("reason", "A partner account")
        ]),
    )
    .await;
    for c in [
        &design_card,
        &remove,
        &revoke,
        &staff_badge,
        &propose,
        &word_card,
        &unverify,
        &badge_remove,
    ] {
        assert!(c.danger(), "{}", c.view);
        let (_, body) = confirm(&s, &owner, c).await;
        assert_eq!(error_type(&body), "NotElevated", "{body}");
    }
    step_up(&s, &owner, 0).await;
    for (c, record) in [
        (&design_card, "staff.design.review"),
        (&remove, "staff.discover.decide"),
        (&revoke, "staff.creator.change"),
        (&staff_badge, "staff.desk.badge.grant"),
        (&propose, "staff.official.propose"),
        (&word_card, "platform.words.remove"),
        (&unverify, "staff.bot.unverify"),
        (&badge_remove, "staff.desk.badge.revoke"),
    ] {
        let (status, body) = confirm(&s, &owner, c).await;
        assert_eq!(status, Status::Ok, "{record}: {body}");
        recorded(&s, &owner, record).await;
    }

    // the two-person rule: not your own proposal; a second Owner confirms
    let requests = s.harness.db.fetch_official_mark_requests().await.unwrap();
    let request = requests.iter().find(|r| r.user_id == someone.id).unwrap();
    let pressed = press(
        &s,
        &owner,
        &command,
        "official",
        "confirm",
        json!([text("request", &request.id)]),
    )
    .await;
    assert_eq!(
        error_type(&s.hand("run", &pressed).await.1),
        "NotPrivileged"
    );
    let c = card(
        &s,
        &second,
        &command,
        "official",
        "confirm",
        json!([text("request", &request.id)]),
    )
    .await;
    assert!(c.danger());
    step_up(&s, &second, 0).await;
    let (status, body) = confirm(&s, &second, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &second, "staff.official.confirm").await;
    assert!(s.harness.db.fetch_user(&someone.id).await.unwrap().official);
}

/// Without the permission, no card: refused as the presser, recorded
#[rocket::async_test]
async fn desk_reviews_refused_without_permission() {
    let s = stage().await;
    all_areas(&s);
    let analyst = s.person(vec![StaffRole::Analyst]).await;
    s.sign_in(&analyst, -1).await;
    let command = register_review(&s).await;
    let plugins = super::registry::plugins_on().await;
    let entries: Vec<_> = super::registry::entries()
        .filter(|e| e.area == Some("review") && (plugins || !e.plugins))
        .collect();
    assert!(entries.len() >= 13, "{}", entries.len());
    for entry in entries {
        let before = s.records(&analyst).await.len();
        let pressed = press(
            &s,
            &analyst,
            &command,
            entry.group.unwrap(),
            entry.name,
            json!([]),
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
}

/// The plugin review follows the plugin switch: absent from the tree while
/// plugins are off
#[rocket::async_test]
async fn desk_plugin_review_follows_the_switch() {
    let s = stage().await;
    all_areas(&s);
    let command = register_review(&s).await;
    let offered = command.options.iter().any(|o| o.name == "plugin");
    assert_eq!(offered, super::registry::plugins_on().await);
}

// ---- review fixes ----

fn flag(name: &str, value: bool) -> Value {
    json!({ "type": "Boolean", "name": name, "value": value })
}

/// The card is refused at `/run`; gives the error
async fn refused_card(
    s: &Stage,
    who: &super::tests::Person,
    command: &ApplicationCommand,
    group: &str,
    name: &str,
    options: Value,
) -> String {
    let pressed = press(s, who, command, group, name, options).await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_ne!(status, Status::Ok, "{group} {name}: {body}");
    error_type(&body).to_string()
}

/// Ask for a Discover listing of `server` as its owner
async fn list_on_discover(s: &Stage, session: &str, server: &str) {
    let response = s
        .harness
        .client
        .put(format!("/servers/{server}/discover"))
        .header(Header::new("x-session-token", session.to_string()))
        .header(ContentType::JSON)
        .body(r##"{"tags":["Gaming"],"category":"gaming","language":"ar"}"##)
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
}

/// A plugin card names the release, its plugin, publisher and risk; an
/// unknown id is refused at the card (plugins are on in the test config)
#[rocket::async_test]
async fn desk_plugin_card_reads_the_target() {
    let s = stage().await;
    assert!(super::registry::plugins_on().await);
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_review(&s).await;
    let (_, _, publisher) = s.harness.new_user().await;
    let plugin_id = format!(
        "dev.example.p{}",
        ulid::Ulid::new().to_string().to_lowercase()
    );
    let release_id = ulid::Ulid::new().to_string();
    let plugin: revolt_database::Plugin = serde_json::from_value(json!({
        "_id": plugin_id, "publisher_id": publisher.id, "name": "Focus Timer",
        "summary": "A timer", "category": "productivity", "status": "Draft",
        "created_at": 1, "updated_at": 1
    }))
    .unwrap();
    s.harness.db.insert_plugin(&plugin).await.unwrap();
    let release: revolt_database::PluginRelease = serde_json::from_value(json!({
        "_id": release_id, "plugin_id": plugin_id, "version": "1.2.0", "manifest": "{}",
        "package_sha256": "00", "size": 2, "permissions": [], "risk": "High",
        "entrypoint": "declarative", "platforms": ["web"], "min_app_version": "1.0.0",
        "status": "PendingReview", "submitted_by": publisher.id, "submitted_at": 1
    }))
    .unwrap();
    s.harness.db.insert_plugin_release(&release).await.unwrap();

    let unknown = refused_card(
        &s,
        &owner,
        &command,
        "plugin",
        "review",
        json!([text("id", "01UNKNOWN"), text("action", "approve")]),
    )
    .await;
    assert_eq!(unknown, "NotFound");
    let c = card(
        &s,
        &owner,
        &command,
        "plugin",
        "review",
        json!([
            text("id", &release_id),
            text("action", "reject"),
            text("reason", "Unsafe code")
        ]),
    )
    .await;
    let content = c.view["content"].as_str().unwrap().to_string();
    for part in [
        "Focus Timer",
        "1.2.0",
        publisher.username.as_str(),
        "risk high",
    ] {
        assert!(content.contains(part), "{part}: {content}");
    }
    step_up(&s, &owner, 0).await;
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.plugin.review").await;
}

/// Plugins switched off after a card was made: the confirmation runs the
/// core, and the core itself refuses. The test config can be overwritten
/// only once per process, so the switch is off from the start and the
/// confirmation's core is called as the desk calls it.
#[rocket::async_test]
async fn desk_plugin_core_checks_the_switch_itself() {
    revolt_config::overwrite_config(|config| {
        config.features.plugins_enabled = false;
    })
    .await;
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    step_up(&s, &owner, 0).await;
    let staff = super::framework_tests::context_of(&s, &owner).await;
    let result = super::super::review::plugin_decide_core(
        &s.harness.db,
        &staff,
        "01UNKNOWN",
        super::super::review::DataStaffPluginDecision {
            action: "reject".to_string(),
            reason: Some("Unsafe code".to_string()),
            verify_publisher: false,
            acknowledge_high_risk: false,
        },
    )
    .await;
    let error = result.err().expect("refused while plugins are off");
    assert_eq!(format!("{:?}", error.error_type), "PluginsDisabled");
}

/// Verification: a decided request and a denial without a note are
/// refused at the card; the card names the privileged intents and their
/// approval needs a fresh code
#[rocket::async_test]
async fn desk_verification_card_checks_like_the_console() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_review(&s).await;
    let (_, bot_owner_session, bot_owner) = s.harness.new_user().await;
    let (bot, _) = s.harness.new_bot(&bot_owner).await;
    // the Verified Bot Developer numbers ask for one legitimate server
    s.harness.legitimate_server_for(&bot.id).await;
    let applied = as_user(
        &s,
        &bot_owner_session.token,
        &format!("/bots/{}/verification", bot.id),
        json!({ "statement": "Moderation helper used in 40 Arabic communities.", "website": "https://example.org", "privacy_policy_url": "https://example.org/privacy", "terms_of_service_url": "https://example.org/terms", "intents": 1024 }),
    )
    .await;
    let request = applied["request"]["_id"].as_str().unwrap().to_string();

    let denied = refused_card(
        &s,
        &owner,
        &command,
        "verification",
        "decide",
        json!([text("request", &request), text("decision", "deny")]),
    )
    .await;
    assert_eq!(denied, "FailedValidation");
    let c = card(
        &s,
        &owner,
        &command,
        "verification",
        "decide",
        json!([text("request", &request), text("decision", "approve")]),
    )
    .await;
    assert!(
        c.view["content"]
            .as_str()
            .unwrap()
            .contains("message content"),
        "{}",
        c.view
    );
    assert!(c.danger());
    let (_, body) = confirm(&s, &owner, &c).await;
    assert_eq!(error_type(&body), "NotElevated", "{body}");
    step_up(&s, &owner, 0).await;
    let (status, body) = confirm(&s, &owner, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    // decided: no second card
    let again = refused_card(
        &s,
        &owner,
        &command,
        "verification",
        "decide",
        json!([text("request", &request), text("decision", "approve")]),
    )
    .await;
    assert_eq!(again, "InvalidOperation");
}

/// Zrzur Desk's own rule: no decision about yourself (creator, badges,
/// your own Discover listing), at the card and again at the confirmation
#[rocket::async_test]
async fn desk_refuses_decisions_about_yourself() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_review(&s).await;
    let me = owner.user.id.clone();
    let (own, _) = s.harness.new_server(&owner.user).await;
    list_on_discover(&s, &owner.session.token, &own.id).await;
    for (group, name, options) in [
        (
            "creator",
            "change",
            json!([
                user_opt(&me),
                text("action", "grant"),
                text("reason", "Makes effects")
            ]),
        ),
        (
            "badge",
            "give",
            json!([
                user_opt(&me),
                text("badge", "community_helper"),
                text("reason", "Partner")
            ]),
        ),
        (
            "discover",
            "decide",
            json!([
                text("kind", "server"),
                text("id", &own.id),
                text("decision", "Approve")
            ]),
        ),
    ] {
        let error = refused_card(&s, &owner, &command, group, name, options).await;
        assert_eq!(error, "NotPrivileged", "{group} {name}");
    }

    // a listing that became yours after the card
    let (_, session, someone) = s.harness.new_user().await;
    let (theirs, _) = s.harness.new_server(&someone).await;
    list_on_discover(&s, &session.token, &theirs.id).await;
    let c = card(
        &s,
        &owner,
        &command,
        "discover",
        "decide",
        json!([
            text("kind", "server"),
            text("id", &theirs.id),
            text("decision", "Approve")
        ]),
    )
    .await;
    let mut server = s.harness.db.fetch_server(&theirs.id).await.unwrap();
    server
        .update(
            &s.harness.db,
            revolt_database::PartialServer {
                owner: Some(me.clone()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (_, body) = confirm(&s, &owner, &c).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");
    let (action, outcome, _) = s.records(&owner).await.pop().unwrap();
    assert_eq!(
        (action.as_str(), outcome.as_str()),
        ("staff.desk.discover.decide", "Denied")
    );
}

/// Refusals at the card write the console's own Denied records; the
/// cores' cheap checks answer at the card
#[rocket::async_test]
async fn desk_card_refusals_match_the_console() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let senior = s.person(vec![StaffRole::SeniorSupport]).await;
    s.sign_in(&owner, -1).await;
    s.sign_in(&senior, -1).await;
    let command = register_review(&s).await;
    let (_, _, someone) = s.harness.new_user().await;

    // a privileged badge by someone without the Owner role
    let error = refused_card(
        &s,
        &senior,
        &command,
        "badge",
        "give",
        json!([
            user_opt(&someone.id),
            text("badge", "developer"),
            text("reason", "Joined the team")
        ]),
    )
    .await;
    assert_eq!(error, "NotPrivileged");
    // beta's single badge gate, on the desk's path
    let (action, outcome, summary) = s.records(&senior).await.pop().unwrap();
    assert_eq!(
        (action.as_str(), outcome.as_str()),
        ("staff.desk.badge.grant", "Denied")
    );
    assert_eq!(summary, "developer denied:owner_only via desk");

    // your own proposal: refused and recorded like the console
    let c = card(
        &s,
        &owner,
        &command,
        "official",
        "propose",
        json!([
            user_opt(&someone.id),
            text("action", "give"),
            text("reason", "A partner account")
        ]),
    )
    .await;
    step_up(&s, &owner, 0).await;
    assert_eq!(confirm(&s, &owner, &c).await.0, Status::Ok);
    let requests = s.harness.db.fetch_official_mark_requests().await.unwrap();
    let request = requests.iter().find(|r| r.user_id == someone.id).unwrap();
    let error = refused_card(
        &s,
        &owner,
        &command,
        "official",
        "confirm",
        json!([text("request", &request.id)]),
    )
    .await;
    assert_eq!(error, "NotPrivileged");
    let (action, outcome, _) = s.records(&owner).await.pop().unwrap();
    assert_eq!(
        (action.as_str(), outcome.as_str()),
        ("staff.official.confirm", "Denied")
    );

    // the propose checks: already open, no effect, a bot
    let (_, _, plain) = s.harness.new_user().await;
    let (_, bot_user) = s.harness.new_bot(&plain).await;
    for (user, action, expected) in [
        (&someone.id, "give", "InvalidOperation"),
        (&plain.id, "remove", "NoEffect"),
        (&bot_user.id, "give", "IsBot"),
    ] {
        let error = refused_card(
            &s,
            &owner,
            &command,
            "official",
            "propose",
            json!([
                user_opt(user),
                text("action", action),
                text("reason", "A partner account")
            ]),
        )
        .await;
        assert_eq!(error, expected, "{action}");
    }

    // Discover: no such request; a denial needs a reason
    let (_, session, lister) = s.harness.new_user().await;
    let (listed, _) = s.harness.new_server(&lister).await;
    let error = refused_card(
        &s,
        &owner,
        &command,
        "discover",
        "decide",
        json!([
            text("kind", "server"),
            text("id", &listed.id),
            text("decision", "Approve")
        ]),
    )
    .await;
    assert_eq!(error, "NotFound");
    list_on_discover(&s, &session.token, &listed.id).await;
    let error = refused_card(
        &s,
        &owner,
        &command,
        "discover",
        "decide",
        json!([
            text("kind", "server"),
            text("id", &listed.id),
            text("decision", "Deny"),
            text("reason", "no")
        ]),
    )
    .await;
    assert_eq!(error, "FailedValidation");

    // "featured" only with an approval; the design's kind in words
    let deny = card(
        &s,
        &owner,
        &command,
        "discover",
        "decide",
        json!([
            text("kind", "server"),
            text("id", &listed.id),
            text("decision", "Deny"),
            flag("featured", true),
            text("reason", "Not a gaming server")
        ]),
    )
    .await;
    assert!(
        !deny.view["content"].as_str().unwrap().contains("Featured"),
        "{}",
        deny.view
    );
    let approve = card(
        &s,
        &owner,
        &command,
        "discover",
        "decide",
        json!([
            text("kind", "server"),
            text("id", &listed.id),
            text("decision", "Approve"),
            flag("featured", true)
        ]),
    )
    .await;
    assert!(
        approve.view["content"]
            .as_str()
            .unwrap()
            .contains("Featured: yes"),
        "{}",
        approve.view
    );
    let design_id = design(&s, &someone.id).await;
    let c = card(
        &s,
        &owner,
        &command,
        "design",
        "review",
        json!([text("design", &design_id), text("action", "approve")]),
    )
    .await;
    assert!(
        c.view["content"]
            .as_str()
            .unwrap()
            .contains("profile effect"),
        "{}",
        c.view
    );
}
