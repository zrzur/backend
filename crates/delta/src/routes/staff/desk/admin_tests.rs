//! Desk administration (`/desk-billing`, `/desk-settings`, `/desk-staff`):
//! every command runs as the console's own core with the console's records
//! on the desk session, needs a fresh code, and keeps the console's rules
//! (refunds within what is left, no Feather for yourself, no Owner role and
//! no change of your own roles, Owner targets only for an Owner).

use revolt_database::StaffRole;
use revolt_models::v0::ApplicationCommand;
use rocket::http::Status;
use serde_json::{Value, json};

use super::moderation_tests::{all_areas, card, confirm, press, recorded, step_up, text, user_opt};
use super::tests::{Person, Stage, error_type, register_served, set_grant, stage};

async fn command(s: &Stage, name: &str) -> ApplicationCommand {
    register_served(s).await.remove(name).unwrap()
}

fn number(name: &str, value: i64) -> Value {
    json!({ "type": "Integer", "name": name, "value": value })
}

/// The card is refused at `/run`; gives the error
async fn refused(
    s: &Stage,
    who: &Person,
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

async fn last(s: &Stage, who: &Person) -> (String, String, String) {
    s.records(who).await.pop().unwrap()
}

/// A paid order through the simulated `test` provider (nothing is sent
/// anywhere): 10.00 EUR, `refunded` cents given back already
async fn paid_order(s: &Stage, user: &str, refunded: u32) -> String {
    let id = format!("{user}-{}", ulid::Ulid::new());
    let order: revolt_database::BillingOrder = serde_json::from_value(json!({
        "_id": id, "user_id": user, "product": { "type": "Plus" }, "kind": "Purchase",
        "amount": 1000, "currency": "eur",
        "status": if refunded > 0 { "PartiallyRefunded" } else { "Paid" },
        "provider": "test", "created_at": 1, "paid_at": 1, "test": true,
        "unit_amount": 1000, "amount_refunded": refunded
    }))
    .unwrap();
    assert!(s.harness.db.insert_order(&order).await.unwrap());
    id
}

#[rocket::async_test]
async fn desk_billing_runs_the_consoles_actions() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = command(&s, "desk-billing").await;
    let (_, _, buyer) = s.harness.new_user().await;
    let order = paid_order(&s, &buyer.id, 400).await;
    let other = paid_order(&s, &buyer.id, 0).await;

    // more than is left: refused at the card
    let error = refused(
        &s,
        &owner,
        &command,
        "billing",
        "refund",
        json!([
            text("order", &order),
            text("reason", "Charged twice"),
            number("amount", 700)
        ]),
    )
    .await;
    assert_eq!(error, "FailedValidation");
    // no Feather for yourself; unknown events
    let error = refused(
        &s,
        &owner,
        &command,
        "billing",
        "grant",
        json!([
            user_opt(&owner.user.id),
            number("days", 7),
            text("reason", "For me")
        ]),
    )
    .await;
    assert_eq!(error, "NotPrivileged");
    let error = refused(
        &s,
        &owner,
        &command,
        "webhook",
        "retry",
        json!([text("event", "evt_unknown")]),
    )
    .await;
    assert_eq!(error, "NotFound");

    let refund = card(
        &s,
        &owner,
        &command,
        "billing",
        "refund",
        json!([
            text("order", &order),
            text("reason", "Charged twice"),
            number("amount", 600)
        ]),
    )
    .await;
    let content = refund.view["content"].as_str().unwrap().to_string();
    for part in ["6.00 EUR", "10.00 EUR", "4.00 EUR", "taken back"] {
        assert!(content.contains(part), "{part}: {content}");
    }
    let deliver = card(
        &s,
        &owner,
        &command,
        "billing",
        "deliver",
        json!([text("order", &other)]),
    )
    .await;
    let reconcile = card(&s, &owner, &command, "billing", "reconcile", json!([])).await;
    let grant = card(
        &s,
        &owner,
        &command,
        "billing",
        "grant",
        json!([
            user_opt(&buyer.id),
            number("days", 7),
            text("reason", "Thanks for the bug reports")
        ]),
    )
    .await;
    assert!(grant.view["content"].as_str().unwrap().contains("7 days"));

    for c in [&refund, &deliver, &reconcile, &grant] {
        assert!(c.danger(), "{}", c.view);
        let (_, body) = confirm(&s, &owner, c).await;
        assert_eq!(error_type(&body), "NotElevated", "{body}");
    }
    step_up(&s, &owner, 0).await;
    let (status, body) = confirm(&s, &owner, &refund).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.billing.refund").await;
    let refunded = s.harness.db.fetch_order(&order).await.unwrap();
    assert_eq!(refunded.amount_refunded, 1000);
    // the others run the console's core: recorded first, whatever the
    // switched-off billing of the test config answers then
    for (c, action) in [
        (&deliver, "staff.billing.deliver"),
        (&reconcile, "staff.billing.reconcile"),
        (&grant, "staff.billing.grant"),
    ] {
        confirm(&s, &owner, c).await;
        recorded(&s, &owner, action).await;
    }
}

#[rocket::async_test]
async fn desk_settings_run_the_consoles_actions() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = command(&s, "desk-settings").await;
    crate::routes::badges::beta::save_feature(
        &s.harness.db,
        &owner.user.id,
        revolt_models::v0::DataBetaFeature {
            id: "voice-rooms".to_string(),
            name: "Voice rooms".to_string(),
            description: None,
            enabled: false,
            all_testers: true,
        },
    )
    .await
    .unwrap();
    let (_, _, tester) = s.harness.new_user().await;

    let error = refused(
        &s,
        &owner,
        &command,
        "beta",
        "feature",
        json!([text("feature", "voice-rooms"), text("state", "off")]),
    )
    .await;
    assert_eq!(error, "NoEffect");
    let policy = |url: &str| {
        json!([
            text("message_ar", "حدّثنا سياسة الخصوصية."),
            text("message_en", "We updated the privacy policy."),
            text(
                "message_de",
                "Wir haben die Datenschutzerklärung aktualisiert."
            ),
            text("url", url),
            number("days", 14)
        ])
    };
    let error = refused(
        &s,
        &owner,
        &command,
        "policy",
        "publish",
        policy("http://example.org"),
    )
    .await;
    assert_eq!(error, "FailedValidation");

    let feature = card(
        &s,
        &owner,
        &command,
        "beta",
        "feature",
        json!([text("feature", "voice-rooms"), text("state", "on")]),
    )
    .await;
    assert!(
        feature.view["content"]
            .as_str()
            .unwrap()
            .contains("every tester")
    );
    let member = card(
        &s,
        &owner,
        &command,
        "beta",
        "member",
        json!([
            user_opt(&tester.id),
            text("action", "invite"),
            text("features", "voice-rooms")
        ]),
    )
    .await;
    let publish = card(
        &s,
        &owner,
        &command,
        "policy",
        "publish",
        policy("https://zrzur.com/legal/privacy"),
    )
    .await;
    let content = publish.view["content"].as_str().unwrap().to_string();
    assert!(
        content.contains("zrzur.com/legal/privacy") && content.contains("الخصوصية"),
        "{content}"
    );
    for c in [&feature, &member, &publish] {
        let (_, body) = confirm(&s, &owner, c).await;
        assert_eq!(error_type(&body), "NotElevated", "{body}");
    }
    step_up(&s, &owner, 0).await;
    for (c, action) in [
        (&feature, "staff.beta.feature"),
        (&member, "staff.beta.member"),
        (&publish, "staff.policy.publish"),
    ] {
        let (status, body) = confirm(&s, &owner, c).await;
        assert_eq!(status, Status::Ok, "{action}: {body}");
        recorded(&s, &owner, action).await;
    }
    let features = crate::routes::badges::beta::features_list(&s.harness.db)
        .await
        .unwrap();
    assert!(features.iter().any(|f| f.id == "voice-rooms" && f.enabled));

    let published = s.harness.db.fetch_policy_changes().await.unwrap();
    let id = published[0].id.clone();
    let withdraw = card(
        &s,
        &owner,
        &command,
        "policy",
        "withdraw",
        json!([text("policy", &id), text("reason", "Sent too early")]),
    )
    .await;
    step_up(&s, &owner, 1).await;
    let (status, body) = confirm(&s, &owner, &withdraw).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &owner, "staff.policy.withdraw").await;
    assert!(
        s.harness
            .db
            .fetch_policy_changes()
            .await
            .unwrap()
            .iter()
            .all(|c| c.id != id)
    );
}

#[rocket::async_test]
async fn desk_staff_runs_the_consoles_actions_and_rules() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let second = s.person(vec![StaffRole::Owner]).await;
    let support = s.person(vec![StaffRole::Support]).await;
    let billing = s.person(vec![StaffRole::Billing]).await;
    let security = s.person(vec![StaffRole::Security]).await;
    s.sign_in(&owner, -1).await;
    s.sign_in(&security, -1).await;
    let command = command(&s, "desk-staff").await;
    let (_, _, newcomer) = s.harness.new_user().await;

    // the Owner role is never given through the desk
    let error = refused(
        &s,
        &owner,
        &command,
        "staff",
        "roles",
        json!([
            user_opt(&newcomer.id),
            text("roles", "owner"),
            text("reason", "Co-founder")
        ]),
    )
    .await;
    assert_eq!(error, "NotPrivileged");
    let (action, outcome, summary) = last(&s, &owner).await;
    assert_eq!(
        (action.as_str(), outcome.as_str()),
        ("staff.grant.update", "Denied")
    );
    assert!(summary.contains("OwnerRole"), "{summary}");
    // nor your own roles changed
    let error = refused(
        &s,
        &owner,
        &command,
        "staff",
        "roles",
        json!([
            user_opt(&owner.user.id),
            text("roles", "support"),
            text("reason", "Step down")
        ]),
    )
    .await;
    assert_eq!(error, "NotPrivileged");
    assert!(last(&s, &owner).await.2.contains("SelfChange"));
    let error = refused(
        &s,
        &owner,
        &command,
        "staff",
        "roles",
        json!([
            user_opt(&newcomer.id),
            text("roles", "wizard"),
            text("reason", "New role")
        ]),
    )
    .await;
    assert_eq!(error, "FailedValidation");
    // never an Owner disabled; never yourself locked out
    let error = refused(
        &s,
        &owner,
        &command,
        "staff",
        "disable",
        json!([user_opt(&second.user.id), text("reason", "Left the team")]),
    )
    .await;
    assert_eq!(error, "NotPrivileged");
    assert_eq!(last(&s, &owner).await.0, "staff.grant.disable");
    let error = refused(
        &s,
        &owner,
        &command,
        "security",
        "lock",
        json!([user_opt(&owner.user.id), text("reason", "Lost phone")]),
    )
    .await;
    assert_eq!(error, "InvalidOperation");
    // only an Owner locks an Owner out
    let error = refused(
        &s,
        &security,
        &command,
        "security",
        "lock",
        json!([user_opt(&owner.user.id), text("reason", "Looks stolen")]),
    )
    .await;
    assert_eq!(error, "NotPrivileged");
    let (action, outcome, summary) = last(&s, &security).await;
    assert_eq!(
        (action.as_str(), outcome.as_str(), summary.as_str()),
        ("staff.security.lock", "Denied", "owner target")
    );

    let roles = card(
        &s,
        &owner,
        &command,
        "staff",
        "roles",
        json!([
            user_opt(&newcomer.id),
            text("roles", "support, billing"),
            text("reason", "Joined the team")
        ]),
    )
    .await;
    let content = roles.view["content"].as_str().unwrap().to_string();
    assert!(
        content.contains("support") && content.contains("billing"),
        "{content}"
    );
    // a grant that changes after the card: the console's version check
    let stale = card(
        &s,
        &owner,
        &command,
        "staff",
        "roles",
        json!([
            user_opt(&support.user.id),
            text("roles", "support, billing"),
            text("reason", "More work")
        ]),
    )
    .await;
    set_grant(
        &s.harness,
        &support.user.id,
        vec![StaffRole::SeniorSupport],
        false,
    )
    .await;
    let disable = card(
        &s,
        &owner,
        &command,
        "staff",
        "disable",
        json!([user_opt(&billing.user.id), text("reason", "Left the team")]),
    )
    .await;
    let sessions = card(
        &s,
        &owner,
        &command,
        "staff",
        "sessions-end",
        json!([user_opt(&support.user.id), text("reason", "Lost laptop")]),
    )
    .await;
    let lock = card(
        &s,
        &owner,
        &command,
        "security",
        "lock",
        json!([user_opt(&billing.user.id), text("reason", "Looks stolen")]),
    )
    .await;
    for c in [&roles, &stale, &disable, &sessions, &lock] {
        assert!(c.danger(), "{}", c.view);
        let (_, body) = confirm(&s, &owner, c).await;
        assert_eq!(error_type(&body), "NotElevated", "{body}");
    }
    step_up(&s, &owner, 0).await;
    for (c, action) in [
        (&roles, "staff.grant.update"),
        (&disable, "staff.grant.disable"),
        (&sessions, "staff.sessions.revoke"),
        (&lock, "staff.security.lock"),
    ] {
        let (status, body) = confirm(&s, &owner, c).await;
        assert_eq!(status, Status::Ok, "{action}: {body}");
        recorded(&s, &owner, action).await;
    }
    let (_, body) = confirm(&s, &owner, &stale).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");
    let db = &s.harness.db;
    let granted = db.fetch_staff_grant(&newcomer.id).await.unwrap().unwrap();
    assert_eq!(granted.roles, vec![StaffRole::Support, StaffRole::Billing]);
    assert!(
        db.fetch_staff_grant(&billing.user.id)
            .await
            .unwrap()
            .unwrap()
            .disabled
    );
    assert_eq!(
        db.fetch_staff_grant(&support.user.id)
            .await
            .unwrap()
            .unwrap()
            .roles,
        vec![StaffRole::SeniorSupport]
    );
}

/// Without the permission, no card: refused as the presser, recorded
#[rocket::async_test]
async fn desk_admin_refused_without_permission() {
    let s = stage().await;
    all_areas(&s);
    let analyst = s.person(vec![StaffRole::Analyst]).await;
    s.sign_in(&analyst, -1).await;
    let commands = register_served(&s).await;
    let entries: Vec<_> = super::registry::entries()
        .filter(|e| matches!(e.area, Some("billing" | "settings" | "staff")))
        .collect();
    assert_eq!(entries.len(), 13);
    for entry in entries {
        assert!(entry.irreversible && entry.changes, "{}", entry.key());
        let command = &commands[entry.command()];
        let before = s.records(&analyst).await.len();
        let pressed = press(
            &s,
            &analyst,
            command,
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
