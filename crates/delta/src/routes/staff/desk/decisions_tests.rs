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
            "bot",
            "unverify",
            json!([text("bot", &bot.id), text("reason", "Verified by mistake")]),
            "staff.bot.unverify",
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
                text("badge", "partner"),
                text("reason", "Official partner")
            ]),
            "staff.badge.grant",
        ),
        (
            "badge",
            "remove",
            json!([
                user_opt(&someone.id),
                text("badge", "partner"),
                text("reason", "Partnership ended")
            ]),
            "staff.badge.revoke",
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
    let c = card(
        &s,
        &owner,
        &command,
        "words",
        "remove",
        json!([text("word", &word.id), text("reason", "Too broad")]),
    )
    .await;
    assert_eq!(confirm(&s, &owner, &c).await.0, Status::Ok);
    recorded(&s, &owner, "platform.words.remove").await;

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
            text("badge", "staff"),
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
    for c in [&design_card, &remove, &revoke, &staff_badge, &propose] {
        assert!(c.danger(), "{}", c.view);
        let (_, body) = confirm(&s, &owner, c).await;
        assert_eq!(error_type(&body), "NotElevated", "{body}");
    }
    step_up(&s, &owner, 0).await;
    for (c, record) in [
        (&design_card, "staff.design.review"),
        (&remove, "staff.discover.decide"),
        (&revoke, "staff.creator.change"),
        (&staff_badge, "staff.badge.grant"),
        (&propose, "staff.official.propose"),
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
