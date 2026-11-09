//! Trust & Safety route tests. Run each as its own process
//! (`$BIN --exact routes::safety::test::<name>`): several async tests in
//! one process can hang on shared runtime state.

use revolt_database::{
    Channel, Member, Message, PartialChannel, PlatformPermission, PlatformRole, Server, Session,
    User,
};
use revolt_models::v0::{
    SafetyAction, SafetyAppeal, SafetyCase, SafetyCaseDetails, SafetyCasePage, SafetyReceipt,
    SafetyReceiptStatus, SafetyStanding, SafetyStandingLevel, SafetyStatus,
};
use revolt_permissions::{ChannelPermission, OverrideField};
use rocket::http::{ContentType, Header, Status};
use ulid::Ulid;

use crate::util::test::TestHarness;

async fn role(harness: &TestHarness, user: &User, permissions: Vec<PlatformPermission>) {
    harness
        .db
        .save_platform_role(&PlatformRole {
            id: user.id.clone(),
            permissions,
            creator: None,
            show_public_name: false,
            updated_at: 0,
            updated_by: "test".to_string(),
        })
        .await
        .unwrap();
}

async fn post(
    harness: &TestHarness,
    session: &Session,
    path: &str,
    body: serde_json::Value,
) -> (Status, String) {
    let response = TestHarness::with_session(
        session.clone(),
        harness
            .client
            .post(path.to_string())
            .header(ContentType::JSON)
            .body(body.to_string()),
    )
    .await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

async fn patch(
    harness: &TestHarness,
    session: &Session,
    path: &str,
    body: serde_json::Value,
) -> (Status, String) {
    let response = TestHarness::with_session(
        session.clone(),
        harness
            .client
            .patch(path.to_string())
            .header(ContentType::JSON)
            .body(body.to_string()),
    )
    .await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

async fn get(harness: &TestHarness, session: &Session, path: &str) -> (Status, String) {
    let response =
        TestHarness::with_session(session.clone(), harness.client.get(path.to_string())).await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

/// A server where `author` posted a message and `reporter` is a member
struct Scene {
    server: Server,
    channel: Channel,
    message: Message,
    author: User,
    author_session: Session,
    reporter: User,
    reporter_session: Session,
}

async fn scene(harness: &TestHarness) -> Scene {
    let (_, owner_session, owner) = harness.new_user().await;
    let _ = owner_session;
    let (server, channels) = harness.new_server(&owner).await;
    let (_, author_session, author) = harness.new_user().await;
    let (channel, _, message) = harness.new_message(&author, &server, channels).await;
    let (_, reporter_session, reporter) = harness.new_user().await;
    Member::create(&harness.db, &server, &reporter, None)
        .await
        .expect("member");
    Scene {
        server,
        channel,
        message,
        author,
        author_session,
        reporter,
        reporter_session,
    }
}

fn message_report(id: &str) -> serde_json::Value {
    serde_json::json!({
        "target": { "kind": "Message", "id": id },
        "category": "Harassment",
        "subcategory": "targeted",
        "description": "<script>alert(1)</script> keeps insulting me"
    })
}

async fn file(harness: &TestHarness, s: &Scene) -> SafetyReceipt {
    let (status, body) = post(
        harness,
        &s.reporter_session,
        "/safety/reports",
        message_report(&s.message.id),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[rocket::async_test]
async fn report_message_creates_case_and_dedupes() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;

    let receipt = file(&harness, &s).await;
    assert!(receipt.reference.starts_with("ZR-"));
    assert_eq!(receipt.status, SafetyReceiptStatus::Submitted);
    assert!(!receipt.existing);

    // the same reporter and target: the open report comes back
    let again = file(&harness, &s).await;
    assert_eq!(again.id, receipt.id);
    assert!(again.existing);

    let case = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    assert_eq!(case.target_owner_id.as_deref(), Some(s.author.id.as_str()));
    assert_eq!(case.status, SafetyStatus::Queued);
    // description stored as text, rendered as text by clients
    assert!(case.description.contains("<script>"));
    let evidence = harness.db.fetch_safety_evidence(&receipt.id).await.unwrap();
    assert_eq!(evidence[0].content.as_deref(), Some("Test message"));
    assert!(evidence[0].context.len() <= 6);

    // own reports are listed; the receipt has no reviewer or reporter data
    let (status, body) = get(&harness, &s.reporter_session, "/safety/reports").await;
    assert_eq!(status, Status::Ok);
    assert!(body.contains(&receipt.id));
    assert!(!body.contains(&s.reporter.id));
    assert!(!body.contains(&s.author.id));
}

#[rocket::async_test]
async fn hidden_and_foreign_content_is_not_found() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;

    // a channel the reporter can't see
    let mut hidden = harness.new_channel(&s.server).await;
    hidden
        .update(
            &harness.db,
            PartialChannel {
                default_permissions: Some(OverrideField {
                    a: 0,
                    d: ChannelPermission::ViewChannel as i64,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let secret = Message {
        id: Ulid::new().to_string(),
        channel: hidden.id().to_string(),
        author: s.author.id.clone(),
        content: Some("hidden".into()),
        ..Default::default()
    };
    harness.db.insert_message(&secret).await.unwrap();

    // a DM between two other users
    let (_, _, other) = harness.new_user().await;
    let dm = Channel::DirectMessage {
        id: Ulid::new().to_string(),
        active: true,
        recipients: vec![s.author.id.clone(), other.id.clone()],
        last_message_id: None,
    };
    harness.db.insert_channel(&dm).await.unwrap();
    let private = Message {
        id: Ulid::new().to_string(),
        channel: dm.id().to_string(),
        author: s.author.id.clone(),
        content: Some("private".into()),
        ..Default::default()
    };
    harness.db.insert_message(&private).await.unwrap();

    // a server the reporter isn't in
    let (_, _, stranger) = harness.new_user().await;
    let (foreign, _) = harness.new_server(&stranger).await;

    for (kind, id) in [
        ("Message", secret.id.as_str()),
        ("Message", private.id.as_str()),
        ("Message", "01HZZZZZZZZZZZZZZZZZZZZZZZ"),
        ("Channel", hidden.id()),
        ("Server", foreign.id.as_str()),
    ] {
        let (status, body) = post(
            &harness,
            &s.reporter_session,
            "/safety/reports",
            serde_json::json!({
                "target": { "kind": kind, "id": id },
                "category": "Spam",
            }),
        )
        .await;
        assert_eq!(status, Status::NotFound, "{kind} {id}: {body}");
    }
    // nothing was stored
    assert!(harness
        .db
        .fetch_safety_cases_by_reporter(&s.reporter.id, 10)
        .await
        .unwrap()
        .is_empty());

    // extra evidence must come from the reported message's channel
    let mut body = message_report(&s.message.id);
    body["evidence_message_ids"] = serde_json::json!([private.id]);
    let (status, _) = post(&harness, &s.reporter_session, "/safety/reports", body).await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn invalid_reports_are_rejected() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;

    // yourself
    let (status, _) = post(
        &harness,
        &s.author_session,
        "/safety/reports",
        message_report(&s.message.id),
    )
    .await;
    assert_eq!(status, Status::BadRequest);

    for body in [
        // reason not offered for messages
        serde_json::json!({ "target": { "kind": "Message", "id": s.message.id }, "category": "BotAbuse" }),
        // unknown sub-reason
        serde_json::json!({ "target": { "kind": "Message", "id": s.message.id }, "category": "Spam", "subcategory": "nope" }),
        // "Other" needs a description
        serde_json::json!({ "target": { "kind": "Message", "id": s.message.id }, "category": "Other" }),
        // too much evidence
        serde_json::json!({ "target": { "kind": "Message", "id": s.message.id }, "category": "Spam",
            "evidence_message_ids": ["a","b","c","d","e","f"] }),
    ] {
        let (status, text) = post(&harness, &s.reporter_session, "/safety/reports", body).await;
        assert_eq!(status, Status::BadRequest, "{text}");
    }

    // bots can't report
    let (bot, _) = harness.new_bot(&s.reporter).await;
    let response = harness
        .client
        .post("/safety/reports")
        .header(Header::new("x-bot-token", bot.token.clone()))
        .header(ContentType::JSON)
        .body(message_report(&s.message.id).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
}

#[rocket::async_test]
async fn only_reviewers_reach_the_queue() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let receipt = file(&harness, &s).await;

    // the reporter, the reported user and a server owner/moderator: no access
    let owner_session = harness
        .db
        .fetch_account(&s.server.owner)
        .await
        .unwrap()
        .create_session(&harness.db, String::new())
        .await
        .unwrap();
    for session in [&s.reporter_session, &s.author_session, &owner_session] {
        let (status, _) = get(&harness, session, "/safety/admin/reports").await;
        assert_eq!(status, Status::Forbidden);
        let (status, _) = get(&harness, session, &format!("/safety/admin/reports/{}", receipt.id)).await;
        assert_eq!(status, Status::Forbidden);
        let (status, body) = get(&harness, session, "/safety/admin/access").await;
        assert_eq!(status, Status::Ok);
        assert!(body.contains("\"review\":false"));
    }
    // someone else's receipt looks like no receipt
    let (status, _) = get(&harness, &s.author_session, &format!("/safety/reports/{}", receipt.id)).await;
    assert_eq!(status, Status::NotFound);
    // creator permissions are not Trust & Safety permissions
    role(&harness, &s.author, vec![PlatformPermission::CosmeticsManageAll]).await;
    let (status, _) = get(&harness, &s.author_session, "/safety/admin/reports").await;
    assert_eq!(status, Status::Forbidden);

    let (_, session, reviewer) = harness.new_user().await;
    role(&harness, &reviewer, vec![PlatformPermission::SafetyReview]).await;
    let (status, body) = get(&harness, &session, "/safety/admin/reports?status=Queued&kind=Message").await;
    assert_eq!(status, Status::Ok);
    let page: SafetyCasePage = serde_json::from_str(&body).unwrap();
    assert!(page.cases.iter().any(|c| c.id == receipt.id));
}

#[rocket::async_test]
async fn review_action_standing_and_appeal() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let receipt = file(&harness, &s).await;

    let (_, review_session, reviewer) = harness.new_user().await;
    role(&harness, &reviewer, vec![PlatformPermission::SafetyReview]).await;
    let (_, act_session, actor) = harness.new_user().await;
    role(&harness, &actor, vec![PlatformPermission::SafetyAction]).await;
    let (_, second_session, second) = harness.new_user().await;
    role(&harness, &second, vec![PlatformPermission::SafetyAction]).await;

    // the sender deletes the message; evidence stays for reviewers
    s.message.delete(&harness.db).await.unwrap();

    let (status, body) = get(&harness, &review_session, &format!("/safety/admin/reports/{}", receipt.id)).await;
    assert_eq!(status, Status::Ok, "{body}");
    let details: SafetyCaseDetails = serde_json::from_str(&body).unwrap();
    assert_eq!(details.evidence[0].content.as_deref(), Some("Test message"));
    assert!(details.log.iter().any(|e| e.event == "EvidenceViewed"));

    let path = format!("/safety/admin/reports/{}/status", receipt.id);
    // decisions with consequences can't be set directly
    let (status, _) = post(&harness, &review_session, &path,
        serde_json::json!({ "status": "ActionTaken", "version": details.case.version })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = post(&harness, &review_session, &path,
        serde_json::json!({ "status": "UnderReview", "version": details.case.version })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let case: SafetyCase = serde_json::from_str(&body).unwrap();
    // a stale version conflicts
    let (status, _) = post(&harness, &review_session, &path,
        serde_json::json!({ "status": "Queued", "version": details.case.version })).await;
    assert_eq!(status, Status::Conflict);
    // reviewers without action rights can't decide
    let (status, _) = post(&harness, &review_session, &path,
        serde_json::json!({ "status": "NoViolation", "version": case.version })).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = post(&harness, &review_session, &format!("/safety/admin/reports/{}/actions", receipt.id),
        serde_json::json!({ "kind": "Warning", "public_reason": "Harassment", "version": case.version, "statement": statement() })).await;
    assert_eq!(status, Status::Forbidden);

    let (status, body) = post(&harness, &act_session, &format!("/safety/admin/reports/{}/actions", receipt.id),
        serde_json::json!({ "kind": "Warning", "public_reason": "Harassing another member",
            "internal_note": "INTERNAL-NOTE-XYZ", "version": case.version, "statement": statement() })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let action: SafetyAction = serde_json::from_str(&body).unwrap();
    let case = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    assert_eq!(case.status, SafetyStatus::ActionTaken);

    // the reporter sees an outcome, not who decided
    let (_, body) = get(&harness, &s.reporter_session, &format!("/safety/reports/{}", receipt.id)).await;
    assert!(body.contains("ActionTaken"));
    assert!(!body.contains(&actor.id));

    // the reported user sees the warning, never the reporter or notes
    let (status, body) = get(&harness, &s.author_session, "/safety/standing").await;
    assert_eq!(status, Status::Ok);
    assert!(!body.contains(&s.reporter.id));
    assert!(!body.contains("INTERNAL-NOTE-XYZ"));
    assert!(!body.contains(&actor.id));
    let standing: SafetyStanding = serde_json::from_str(&body).unwrap();
    assert_eq!(standing.level, SafetyStandingLevel::Limited);
    assert!(standing.entries[0].appealable);

    // only the affected user may appeal, once
    let appeal_path = format!("/safety/standing/{}/appeal", action.id);
    let statement = serde_json::json!({ "statement": "This was a misunderstanding between friends." });
    let (status, _) = post(&harness, &s.reporter_session, &appeal_path, statement.clone()).await;
    assert_eq!(status, Status::NotFound);
    let (status, body) = post(&harness, &s.author_session, &appeal_path, statement.clone()).await;
    assert_eq!(status, Status::Ok, "{body}");
    let appeal: SafetyAppeal = serde_json::from_str(&body).unwrap();
    let (status, _) = post(&harness, &s.author_session, &appeal_path, statement).await;
    assert_eq!(status, Status::BadRequest);
    // appealing changes nothing by itself
    let action_now = harness.db.fetch_safety_action(&action.id).await.unwrap();
    assert!(action_now.revoked.is_none());
    assert_eq!(
        harness.db.fetch_safety_case(&receipt.id).await.unwrap().status,
        SafetyStatus::Appealed
    );

    // the reviewer who acted can't judge their own action
    let decide = format!("/safety/admin/appeals/{}", appeal.id);
    let (status, _) = post(&harness, &act_session, &decide,
        serde_json::json!({ "overturn": true, "note": "Overturning my own call" })).await;
    assert_eq!(status, Status::Forbidden);
    let (status, body) = post(&harness, &second_session, &decide,
        serde_json::json!({ "overturn": true, "note": "Context shows it was a joke." })).await;
    assert_eq!(status, Status::Ok, "{body}");

    let action_now = harness.db.fetch_safety_action(&action.id).await.unwrap();
    assert!(action_now.revoked.is_some());
    assert_eq!(
        harness.db.fetch_safety_case(&receipt.id).await.unwrap().status,
        SafetyStatus::AppealResolved
    );
    let (_, body) = get(&harness, &s.author_session, "/safety/standing").await;
    let standing: SafetyStanding = serde_json::from_str(&body).unwrap();
    assert_eq!(standing.level, SafetyStandingLevel::Good);
    // the whole history is kept
    let log = harness.db.fetch_safety_log(&receipt.id).await.unwrap();
    for event in ["Created", "EvidenceViewed", "Status", "Action", "Appeal", "AppealDecision", "ActionRevoked"] {
        assert!(log.iter().any(|e| e.event == event), "missing {event}");
    }
}

#[rocket::async_test]
async fn suspension_and_bot_disable_are_enforced_and_reversible() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let receipt = file(&harness, &s).await;
    let (_, act_session, actor) = harness.new_user().await;
    role(&harness, &actor, vec![PlatformPermission::SafetyAction]).await;

    let case = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    // no duration, no suspension
    let (status, _) = post(&harness, &act_session, &format!("/safety/admin/reports/{}/actions", case.id),
        serde_json::json!({ "kind": "SuspendAccount", "public_reason": "Repeated harassment", "version": case.version, "statement": statement() })).await;
    assert_eq!(status, Status::BadRequest);
    // bans are senior decisions
    let (status, _) = post(&harness, &act_session, &format!("/safety/admin/reports/{}/actions", case.id),
        serde_json::json!({ "kind": "BanAccount", "public_reason": "Repeated harassment", "version": case.version, "statement": statement() })).await;
    assert_eq!(status, Status::Forbidden);

    let (status, body) = post(&harness, &act_session, &format!("/safety/admin/reports/{}/actions", case.id),
        serde_json::json!({ "kind": "SuspendAccount", "public_reason": "Repeated harassment",
            "duration_days": 3, "version": case.version, "statement": statement() })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let action: SafetyAction = serde_json::from_str(&body).unwrap();
    assert!(harness.db.fetch_account(&s.author.id).await.unwrap().disabled);
    // the suspended user's session is gone
    let (status, _) = get(&harness, &s.author_session, "/safety/standing").await;
    assert_eq!(status, Status::Unauthorized);

    let (status, body) = post(&harness, &act_session, &format!("/safety/admin/actions/{}/revoke", action.id),
        serde_json::json!({ "reason": "Wrong account" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(!harness.db.fetch_account(&s.author.id).await.unwrap().disabled);
    assert!(harness.db.fetch_user(&s.author.id).await.unwrap().suspended_until.is_none());

    // a bot: report it, disable it, its token stops working
    let (bot, _) = harness.new_bot(&s.author).await;
    let mut public = bot.clone();
    public
        .update(&harness.db, revolt_database::PartialBot { public: Some(true), ..Default::default() }, vec![])
        .await
        .unwrap();
    let (status, body) = post(&harness, &s.reporter_session, "/safety/reports",
        serde_json::json!({ "target": { "kind": "Bot", "id": bot.id }, "category": "BotAbuse", "subcategory": "spam_dms" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let bot_case: SafetyReceipt = serde_json::from_str(&body).unwrap();
    let version = harness.db.fetch_safety_case(&bot_case.id).await.unwrap().version;
    let (status, body) = post(&harness, &act_session, &format!("/safety/admin/reports/{}/actions", bot_case.id),
        serde_json::json!({ "kind": "DisableBot", "public_reason": "Spam DMs", "version": version, "statement": statement() })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let response = harness
        .client
        .get("/users/@me")
        .header(Header::new("x-bot-token", bot.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Unauthorized);
}

#[rocket::async_test]
async fn legacy_route_and_bulk_triage() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;

    let (status, body) = post(&harness, &s.reporter_session, "/safety/report",
        serde_json::json!({ "content": { "type": "Message", "id": s.message.id, "report_reason": "SpamAbuse" } })).await;
    assert_eq!(status, Status::NoContent, "{body}");
    let cases = harness.db.fetch_safety_cases_by_reporter(&s.reporter.id, 10).await.unwrap();
    assert_eq!(cases.len(), 1);

    // user report with a message as context
    let (status, body) = post(&harness, &s.reporter_session, "/safety/report",
        serde_json::json!({ "content": { "type": "User", "id": s.author.id, "report_reason": "Impersonation",
            "message_id": s.message.id } })).await;
    assert_eq!(status, Status::NoContent, "{body}");

    let (_, session, reviewer) = harness.new_user().await;
    role(&harness, &reviewer, vec![PlatformPermission::SafetyReview]).await;
    let ids: Vec<String> = harness
        .db
        .fetch_safety_cases_by_reporter(&s.reporter.id, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    // review rights can triage but not decide
    let (status, body) = post(&harness, &session, "/safety/admin/reports/bulk",
        serde_json::json!({ "ids": ids, "status": "NoViolation" })).await;
    assert_eq!(status, Status::Ok);
    assert!(body.contains("MissingPlatformPermission"));
    let (status, body) = post(&harness, &session, "/safety/admin/reports/bulk",
        serde_json::json!({ "ids": ids, "status": "UnderReview", "priority": "High", "assign_self": true })).await;
    assert_eq!(status, Status::Ok);
    let result: revolt_models::v0::SafetyBulkResult = serde_json::from_str(&body).unwrap();
    assert_eq!(result.updated.len(), 2);
    // accounts are never touched in bulk
    let (status, _) = post(&harness, &session, "/safety/admin/reports/bulk",
        serde_json::json!({ "ids": ids, "status": "ActionTaken" })).await;
    assert_eq!(status, Status::BadRequest);

    // assigning to a non-reviewer fails
    let case = harness.db.fetch_safety_case(&ids[0]).await.unwrap();
    let (status, _) = patch(&harness, &session, &format!("/safety/admin/reports/{}", case.id),
        serde_json::json!({ "assignee_id": s.author.id, "version": case.version })).await;
    assert_eq!(status, Status::Forbidden);
}

fn statement() -> serde_json::Value {
    serde_json::json!({
        "ground": { "type": "Terms", "rule": "Harassment" },
        "facts": "Insulted another member repeatedly",
    })
}

async fn post_public(harness: &TestHarness, body: serde_json::Value) -> (Status, String) {
    let response = harness
        .client
        .post("/safety/public-reports")
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

fn public_report() -> serde_json::Value {
    serde_json::json!({
        "target_kind": "Message",
        "location": "https://app.zrzur.com/channel/01HZZZZZZZZZZZZZZZZZZZZZZZ/01HYYYYYYYYYYYYYYYYYYYYYYY",
        "description": "This message offers stolen goods for sale",
        "category": "IllegalContent",
        "law": "StGB 259",
        "name": "Sam Example",
        "email": "sam@example.org",
        "good_faith": true,
        "language": "de"
    })
}

#[rocket::async_test]
async fn public_report_needs_no_session_and_enters_the_queue() {
    let harness = TestHarness::new().await;
    let (status, body) = post_public(&harness, public_report()).await;
    assert_eq!(status, Status::Ok, "{body}");
    let receipt: SafetyReceipt = serde_json::from_str(&body).unwrap();
    assert!(receipt.reference.starts_with("ZR-"));
    let case = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    assert_eq!(case.reporter_id, "public");
    assert_eq!(case.status, SafetyStatus::Queued);
    let reporter = case.public_reporter.unwrap();
    assert_eq!(reporter.email.as_deref(), Some("sam@example.org"));
    assert_eq!(reporter.language, "de");
    assert!(case.description.contains("StGB 259"));
}

#[rocket::async_test]
async fn public_report_is_validated() {
    let harness = TestHarness::new().await;
    for change in [
        serde_json::json!({ "good_faith": false }),
        serde_json::json!({ "name": null }),
        serde_json::json!({ "email": null }),
        serde_json::json!({ "email": "not an address" }),
        serde_json::json!({ "description": "short" }),
        serde_json::json!({ "description": "x".repeat(2001) }),
        serde_json::json!({ "category": "Spam" }),
    ] {
        let mut body = public_report();
        for (key, value) in change.as_object().unwrap() {
            body[key] = value.clone();
        }
        let (status, text) = post_public(&harness, body).await;
        assert_eq!(status, Status::BadRequest, "{change}: {text}");
    }
}

#[rocket::async_test]
async fn public_report_of_child_abuse_material_needs_no_name_or_email() {
    let harness = TestHarness::new().await;
    let mut body = public_report();
    body["category"] = "SexualContent".into();
    body["subcategory"] = "minor_safety".into();
    body["name"] = serde_json::Value::Null;
    body["email"] = serde_json::Value::Null;
    let (status, text) = post_public(&harness, body).await;
    assert_eq!(status, Status::Ok, "{text}");
    let receipt: SafetyReceipt = serde_json::from_str(&text).unwrap();
    let case = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    assert_eq!(case.priority, revolt_models::v0::SafetyPriority::Urgent);
    assert!(case.public_reporter.unwrap().name.is_none());
}

#[rocket::async_test]
async fn public_report_is_rate_limited_per_address() {
    let harness = TestHarness::new().await;
    for _ in 0..5 {
        let (status, body) = post_public(&harness, public_report()).await;
        assert_eq!(status, Status::Ok, "{body}");
    }
    let (status, _) = post_public(&harness, public_report()).await;
    assert_eq!(status, Status::TooManyRequests);
}

#[rocket::async_test]
async fn an_action_without_a_ground_is_refused_and_the_statement_is_kept() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let receipt = file(&harness, &s).await;
    let (_, act_session, actor) = harness.new_user().await;
    role(&harness, &actor, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let case = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    let path = format!("/safety/admin/reports/{}/actions", receipt.id);

    // an empty ground
    let (status, _) = post(&harness, &act_session, &path, serde_json::json!({
        "kind": "Warning", "public_reason": "Harassment", "version": case.version,
        "statement": { "ground": { "type": "Law", "law": "StGB", "provision": "" }, "facts": "Said something" }
    })).await;
    assert_eq!(status, Status::BadRequest);

    let (status, body) = post(&harness, &act_session, &path, serde_json::json!({
        "kind": "Warning", "public_reason": "Harassment", "version": case.version,
        "statement": {
            "ground": { "type": "Law", "law": "StGB", "provision": "section 185" },
            "facts": "Insulted another member",
            "territorial_scope": "Germany",
            "automated_detection": true
        }
    })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let action: SafetyAction = serde_json::from_str(&body).unwrap();
    assert!(action.automated_detection && !action.automated_decision);
    assert_eq!(action.territorial_scope, "Germany");
    assert_eq!(action.facts, "Insulted another member");
    assert!(matches!(action.ground, revolt_models::v0::SafetyGround::Law { .. }));
    // the person sees the statement in their standing
    let (status, body) = get(&harness, &s.author_session, "/safety/standing").await;
    assert_eq!(status, Status::Ok, "{body}");
    let standing: SafetyStanding = serde_json::from_str(&body).unwrap();
    assert_eq!(standing.entries[0].facts, "Insulted another member");
    assert_eq!(standing.entries[0].territorial_scope, "Germany");
    assert!(!body.contains("internal_note"));
}

#[rocket::async_test]
async fn an_action_without_a_statement_still_works_and_gets_a_translated_default() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let receipt = file(&harness, &s).await;
    let (_, act_session, actor) = harness.new_user().await;
    role(&harness, &actor, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let case = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    let (status, body) = post(&harness, &act_session, &format!("/safety/admin/reports/{}/actions", receipt.id),
        serde_json::json!({ "kind": "Warning", "public_reason": "Harassing another member", "version": case.version })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let action: SafetyAction = serde_json::from_str(&body).unwrap();
    assert!(matches!(&action.ground, revolt_models::v0::SafetyGround::Terms { rule } if rule == "rule:Harassment"), "{:?}", action.ground);
    assert_eq!(action.basis, revolt_models::v0::SafetyActionBasis::Report);
}

async fn public_case_for(harness: &TestHarness, s: &Scene) -> revolt_models::v0::SafetyCase {
    let mut body = public_report();
    body["location"] = format!("https://app.zrzur.com/channel/{}/{}", s.channel.id(), s.message.id).into();
    let (status, text) = post_public(harness, body).await;
    assert_eq!(status, Status::Ok, "{text}");
    let receipt: SafetyReceipt = serde_json::from_str(&text).unwrap();
    harness.db.fetch_safety_case(&receipt.id).await.unwrap()
}

#[rocket::async_test]
async fn acting_on_a_public_report_keeps_a_copy_and_tells_the_reporter() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let case = public_case_for(&harness, &s).await;
    assert_eq!(case.target.id, s.message.id);
    assert_eq!(case.target_owner_id.as_deref(), Some(s.author.id.as_str()));
    assert!(case.evidence_ids.is_empty(), "nothing is copied for an anonymous caller");

    let (_, act_session, actor) = harness.new_user().await;
    role(&harness, &actor, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let (status, body) = post(&harness, &act_session, &format!("/safety/admin/reports/{}/actions", case.id), serde_json::json!({
        "kind": "RemoveMessage", "public_reason": "Stolen goods", "version": case.version,
        "statement": { "ground": { "type": "Law", "law": "StGB", "provision": "section 259" }, "facts": "Offered stolen goods" }
    })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let action: SafetyAction = serde_json::from_str(&body).unwrap();
    assert_eq!(action.basis, revolt_models::v0::SafetyActionBasis::Notice);
    // the message is gone, the copy is not
    assert!(harness.db.fetch_message(&s.message.id).await.is_err());
    let evidence = harness.db.fetch_safety_evidence(&case.id).await.unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].content.as_deref(), s.message.content.as_deref());
    let after = harness.db.fetch_safety_case(&case.id).await.unwrap();
    assert_eq!(after.evidence_ids.len(), 1);
}

#[rocket::async_test]
async fn a_public_report_of_an_unreadable_link_gets_its_own_target_id() {
    let harness = TestHarness::new().await;
    let mut ids = vec![];
    for _ in 0..2 {
        let mut body = public_report();
        body["location"] = "somebody on the other server".into();
        body["target_kind"] = "User".into();
        let (status, text) = post_public(&harness, body).await;
        assert_eq!(status, Status::Ok, "{text}");
        let receipt: SafetyReceipt = serde_json::from_str(&text).unwrap();
        ids.push(harness.db.fetch_safety_case(&receipt.id).await.unwrap().target.id);
    }
    assert!(ids[0].starts_with("unresolved:") && ids[0] != ids[1]);
}

#[rocket::async_test]
async fn closing_a_public_report_without_action_works_and_asks_for_info_need_a_question() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let case = public_case_for(&harness, &s).await;
    let (_, act_session, actor) = harness.new_user().await;
    role(&harness, &actor, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let path = format!("/safety/admin/reports/{}/status", case.id);
    // a request for information needs the question
    let (status, _) = post(&harness, &act_session, &path,
        serde_json::json!({ "status": "MoreInfoRequired", "version": case.version })).await;
    assert_ne!(status, Status::Ok);
    let (status, body) = post(&harness, &act_session, &path,
        serde_json::json!({ "status": "NoViolation", "version": case.version, "public_note": "Checked: allowed" })).await;
    assert_eq!(status, Status::Ok, "{body}");
}

async fn reviewer(harness: &TestHarness, permissions: Vec<PlatformPermission>) -> Session {
    let (_, session, user) = harness.new_user().await;
    role(harness, &user, permissions).await;
    session
}

async fn public_case_at(harness: &TestHarness, kind: &str, location: String) -> revolt_models::v0::SafetyCase {
    let mut body = public_report();
    body["target_kind"] = kind.into();
    body["location"] = location.into();
    let (status, text) = post_public(harness, body).await;
    assert_eq!(status, Status::Ok, "{text}");
    let receipt: SafetyReceipt = serde_json::from_str(&text).unwrap();
    harness.db.fetch_safety_case(&receipt.id).await.unwrap()
}

fn warn(version: i64) -> serde_json::Value {
    serde_json::json!({
        "kind": "Warning", "public_reason": "Stolen goods", "version": version,
        "statement": { "ground": { "type": "Law", "law": "StGB", "provision": "section 259" }, "facts": "Offered stolen goods" }
    })
}

#[rocket::async_test]
async fn contact_details_go_after_a_year_also_when_an_action_ended_the_case() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let decided = public_case_for(&harness, &s).await;
    let waiting = public_case_for(&harness, &s).await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;

    let (status, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", decided.id), warn(decided.version)).await;
    assert_eq!(status, Status::Ok, "{body}");
    let ended = harness.db.fetch_safety_case(&decided.id).await.unwrap();
    assert!(ended.closed_at.is_none(), "an action does not set closed_at");
    let decided_at = ended.resolution.as_ref().unwrap().decided_at;

    // eleven months later nothing is cleared, thirteen months later the decided case is
    harness.db.clear_public_reporters(decided_at - 1).await.unwrap();
    assert!(harness.db.fetch_safety_case(&decided.id).await.unwrap().public_reporter.unwrap().email.is_some());
    harness.db.clear_public_reporters(decided_at + 1).await.unwrap();
    let cleared = harness.db.fetch_safety_case(&decided.id).await.unwrap().public_reporter.unwrap();
    assert!(cleared.email.is_none() && cleared.name.is_none());
    // a case nobody has decided keeps its reporter, however old
    let open = harness.db.fetch_safety_case(&waiting.id).await.unwrap().public_reporter.unwrap();
    assert!(open.email.is_some());
}

#[rocket::async_test]
async fn a_file_can_be_reported_without_an_account() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let file_id = format!("{}abcdefghijklmnop", ulid::Ulid::new());
    assert_eq!(file_id.len(), 42);
    harness
        .db
        .insert_attachment(&revolt_database::File {
            id: file_id.clone(),
            tag: "attachments".into(),
            filename: "cat.png".into(),
            hash: None,
            uploaded_at: None,
            uploader_id: Some(s.author.id.clone()),
            // as `File::use_attachment` writes it: `message_id` is deprecated and never set
            used_for: Some(revolt_database::FileUsedFor {
                object_type: revolt_database::FileUsedForType::Message,
                id: s.message.id.clone(),
            }),
            deleted: None,
            reported: None,
            purged: None,
            metadata: Default::default(),
            content_type: "image/png".into(),
            size: 3,
            message_id: None,
            user_id: None,
            server_id: None,
            object_id: None,
            forwarded_from: None,
        })
        .await
        .unwrap();
    let case = public_case_at(&harness, "Attachment", format!("https://cdn.zrzur.com/attachments/{file_id}/cat.png")).await;
    assert_eq!(case.target.id, file_id);
    assert_eq!(case.target.message_id.as_deref(), Some(s.message.id.as_str()));
    assert_eq!(case.target_owner_id.as_deref(), Some(s.author.id.as_str()));

    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let (status, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", case.id), serde_json::json!({
        "kind": "RemoveMessage", "public_reason": "Stolen goods", "version": case.version,
        "statement": { "ground": { "type": "Law", "law": "StGB", "provision": "section 259" }, "facts": "Offered stolen goods" }
    })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let action: SafetyAction = serde_json::from_str(&body).unwrap();
    assert_eq!(action.subject_user_id.as_deref(), Some(s.author.id.as_str()), "the author is told");
    assert!(harness.db.fetch_message(&s.message.id).await.is_err());
    let evidence = harness.db.fetch_safety_evidence(&case.id).await.unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].kind, revolt_models::v0::SafetyTargetKind::Attachment);
}

#[rocket::async_test]
async fn a_report_by_custom_link_finds_the_server_and_its_owner() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let code = format!("rabea{}", &s.server.id[20..].to_lowercase());
    harness
        .db
        .insert_invite(&revolt_database::Invite::Server {
            code: code.clone(),
            server: s.server.id.clone(),
            creator: s.server.owner.clone(),
            channel: s.server.channels[0].clone(),
            max_uses: None,
            uses: 0,
            expires: None,
            vanity: true,
        })
        .await
        .unwrap();
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;

    // as the page sends it, and as a person pastes it, query and capitals included
    for link in [
        format!("https://discover.zrzur.com/{code}"),
        format!("discover.zrzur.com/{}/?join=1#top", code.to_uppercase()),
    ] {
        let case = public_case_at(&harness, "DiscoverServer", link).await;
        assert_eq!(case.target.id, s.server.id);
        assert_eq!(case.target_owner_id.as_deref(), Some(s.server.owner.as_str()));
    }

    // a link that names nothing stays unresolved, on its own case
    let unknown = public_case_at(&harness, "DiscoverServer", "https://discover.zrzur.com/no-such-link".to_string()).await;
    assert!(unknown.target.id.starts_with("unresolved:"));
    assert!(unknown.target_owner_id.is_none());

    // the case can be acted on
    let case = public_case_at(&harness, "DiscoverServer", format!("https://discover.zrzur.com/{code}")).await;
    let mut remove = warn(case.version);
    remove["kind"] = "RemoveDiscoverListing".into();
    let (status, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", case.id), remove).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn the_official_account_cannot_be_reported_but_the_public_form_stays_open() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let official = revolt_database::OfficialAccount::Safety.id();
    let (_, session, person) = harness.new_user().await;
    let message = revolt_database::send_official_message(&harness.db, None, revolt_database::OfficialAccount::Safety, &person.id, "Hello".to_string())
        .await
        .unwrap();

    let mut body = message_report(&message.id);
    let (status, text) = post(&harness, &session, "/safety/reports", body.clone()).await;
    assert_eq!(status, Status::Forbidden, "message: {text}");
    body["target"] = serde_json::json!({ "kind": "User", "id": official });
    let (status, text) = post(&harness, &session, "/safety/reports", body).await;
    assert_eq!(status, Status::Forbidden, "user: {text}");

    // other people are reported as ever
    let (status, _) = post(&harness, &s.reporter_session, "/safety/reports", message_report(&s.message.id)).await;
    assert_eq!(status, Status::Ok);

    // anyone may still write to the public form (DSA Art. 16)
    let case = public_case_at(&harness, "User", official.to_string()).await;
    assert_eq!(case.target.id, official);
}

/// A listing with a custom link of its own
async fn linked_server(harness: &TestHarness, s: &Scene, code: &str) {
    harness
        .db
        .insert_invite(&revolt_database::Invite::Server {
            code: code.to_string(),
            server: s.server.id.clone(),
            creator: s.server.owner.clone(),
            channel: s.server.channels[0].clone(),
            max_uses: None,
            uses: 0,
            expires: None,
            vanity: true,
        })
        .await
        .unwrap();
}

#[rocket::async_test]
async fn a_custom_link_that_looks_like_an_id_still_finds_the_server() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let code = format!("linkname{}", s.server.id[8..].to_lowercase());
    assert_eq!(code.len(), 26);
    linked_server(&harness, &s, &code).await;

    for kind in ["DiscoverServer", "Server"] {
        let case = public_case_at(&harness, kind, format!("https://discover.zrzur.com/{code}")).await;
        assert_eq!(case.target.id, s.server.id, "{kind}");
        assert_eq!(case.target_owner_id.as_deref(), Some(s.server.owner.as_str()), "{kind}");
    }
}

async fn link_read_from_the_path_on_our_own_hosts(kind: &str) {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let other = scene(&harness).await;
    let code = format!("rabea{}", &s.server.id[20..].to_lowercase());
    linked_server(&harness, &s, &code).await;

    // an id in the query does not name the server: the path does
    let link = format!("https://discover.zrzur.com/{code}?ref={}", other.server.id);
    let case = public_case_at(&harness, kind, link).await;
    assert_eq!(case.target.id, s.server.id, "query");

    // the bare name, as a person types it
    let case = public_case_at(&harness, kind, code.clone()).await;
    assert_eq!(case.target.id, s.server.id, "bare");

    // someone else's site with the same last word
    let case = public_case_at(&harness, kind, format!("https://evil.example/{code}")).await;
    assert!(case.target.id.starts_with("unresolved:"), "host");
    assert!(case.target_owner_id.is_none(), "host");

    // an id nobody has
    let case = public_case_at(&harness, kind, "01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string()).await;
    assert!(case.target.id.starts_with("unresolved:"), "unknown");
    assert!(case.target_owner_id.is_none(), "unknown");
}

#[rocket::async_test]
async fn a_custom_link_on_a_discover_report_is_read_from_our_own_hosts() {
    link_read_from_the_path_on_our_own_hosts("DiscoverServer").await;
}

#[rocket::async_test]
async fn a_custom_link_on_a_server_report_is_read_from_our_own_hosts() {
    link_read_from_the_path_on_our_own_hosts("Server").await;
}

#[rocket::async_test]
async fn owners_are_found_for_listings_and_bots_and_unknown_ones_refuse_an_action() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (_, _, bot_owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&bot_owner).await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;

    for (kind, id, owner) in [
        ("DiscoverServer", s.server.id.clone(), s.server.owner.clone()),
        ("Bot", bot.id.clone(), bot_owner.id.clone()),
        ("Application", bot.id.clone(), bot_owner.id.clone()),
        ("DiscoverBot", bot.id.clone(), bot_owner.id.clone()),
    ] {
        let case = public_case_at(&harness, kind, format!("https://app.zrzur.com/x/{id}")).await;
        assert_eq!(case.target_owner_id.as_deref(), Some(owner.as_str()), "{kind}");
    }
    // a user who does not exist: nobody can be told, so no warning is issued
    let ghost = public_case_at(&harness, "User", "01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string()).await;
    assert!(ghost.target_owner_id.is_none());
    let (status, _) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", ghost.id), warn(ghost.version)).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn a_legal_hold_on_a_public_report_copies_the_content_and_protects_it() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let case = public_case_for(&harness, &s).await;
    assert!(case.evidence_ids.is_empty());
    let senior = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyManage]).await;
    let (status, body) = patch(&harness, &senior, &format!("/safety/admin/reports/{}", case.id),
        serde_json::json!({ "legal_hold": true, "version": case.version })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let held = harness.db.fetch_safety_case(&case.id).await.unwrap();
    assert!(held.legal_hold);
    let evidence = harness.db.fetch_safety_evidence(&case.id).await.unwrap();
    assert_eq!(evidence.len(), 1, "the hold made a copy");
    assert!(evidence[0].legal_hold && evidence[0].expires_at.is_none());
    // not purged, however late
    harness.db.purge_expired_safety_evidence(i64::MAX / 2).await.unwrap();
    assert_eq!(harness.db.fetch_safety_evidence(&case.id).await.unwrap().len(), 1);
}

#[rocket::async_test]
async fn a_failed_action_still_keeps_the_copy_and_the_owner() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let case = public_case_for(&harness, &s).await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    // suspending needs a duration: the action fails after the preparation
    let (status, _) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", case.id), serde_json::json!({
        "kind": "SuspendAccount", "public_reason": "Stolen goods", "version": case.version,
        "statement": { "ground": { "type": "Terms", "rule": "rule:IllegalContent" }, "facts": "Offered stolen goods" }
    })).await;
    assert_eq!(status, Status::BadRequest);
    let after = harness.db.fetch_safety_case(&case.id).await.unwrap();
    assert_eq!(after.evidence_ids.len(), 1, "the copy is linked to the case");
    // a ban without the senior right does not even take the copy
    let other = public_case_for(&harness, &s).await;
    let (status, _) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", other.id), serde_json::json!({
        "kind": "BanAccount", "public_reason": "Stolen goods", "version": other.version,
        "statement": { "ground": { "type": "Terms", "rule": "rule:IllegalContent" }, "facts": "Offered stolen goods" }
    })).await;
    assert_eq!(status, Status::Forbidden);
    assert!(harness.db.fetch_safety_case(&other.id).await.unwrap().evidence_ids.is_empty());
}

fn reported_file(id: &str, uploader: &str, used_for: Option<revolt_database::FileUsedFor>, message_id: Option<String>) -> revolt_database::File {
    revolt_database::File {
        id: id.to_string(),
        tag: "attachments".into(),
        filename: "cat.png".into(),
        hash: None,
        uploaded_at: None,
        uploader_id: Some(uploader.to_string()),
        used_for,
        deleted: None,
        reported: None,
        purged: None,
        metadata: Default::default(),
        content_type: "image/png".into(),
        size: 3,
        message_id,
        user_id: None,
        server_id: None,
        object_id: None,
        forwarded_from: None,
    }
}

fn file_id() -> String {
    format!("{}abcdefghijklmnop", ulid::Ulid::new())
}

fn file_link(id: &str) -> String {
    format!("https://cdn.zrzur.com/attachments/{id}/cat.png")
}

#[rocket::async_test]
async fn a_file_on_an_old_message_is_found_through_message_id() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let id = file_id();
    harness.db.insert_attachment(&reported_file(&id, &s.author.id, None, Some(s.message.id.clone()))).await.unwrap();
    let case = public_case_at(&harness, "Attachment", file_link(&id)).await;
    assert_eq!(case.target.message_id.as_deref(), Some(s.message.id.as_str()));
    assert_eq!(case.target_owner_id.as_deref(), Some(s.author.id.as_str()));
}

/// `use_attachment` also claims files for bug reports and interaction options:
/// their parent id is no message, exactly like a message that was deleted
#[rocket::async_test]
async fn a_file_without_a_message_is_answered_for_by_its_uploader() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let bug_report = revolt_database::FileUsedFor {
        object_type: revolt_database::FileUsedForType::Message,
        id: ulid::Ulid::new().to_string(),
    };
    let avatar = revolt_database::FileUsedFor {
        object_type: revolt_database::FileUsedForType::UserAvatar,
        id: s.author.id.clone(),
    };
    for used_for in [bug_report, avatar] {
        let id = file_id();
        harness.db.insert_attachment(&reported_file(&id, &s.author.id, Some(used_for), None)).await.unwrap();
        let case = public_case_at(&harness, "Attachment", file_link(&id)).await;
        assert_eq!(case.target_owner_id.as_deref(), Some(s.author.id.as_str()), "the uploader answers");
        assert!(case.target.message_id.is_none(), "a parent that is no message is not named as one");

        // the file itself is copied and protected, and the person can be warned
        let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
        let (status, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", case.id), warn(case.version)).await;
        assert_eq!(status, Status::Ok, "{body}");
        let evidence = harness.db.fetch_safety_evidence(&case.id).await.unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].files[0].id, id);
        assert_eq!(evidence[0].author_id.as_deref(), Some(s.author.id.as_str()));
    }
}

async fn webhook_message(harness: &TestHarness, s: &Scene) -> (revolt_database::Webhook, Message) {
    let webhook = revolt_database::Webhook {
        id: Ulid::new().to_string(),
        name: "Hook".into(),
        creator_id: s.author.id.clone(),
        channel_id: s.channel.id().to_string(),
        ..Default::default()
    };
    harness.db.insert_webhook(&webhook).await.unwrap();
    let mut message = s.message.clone();
    message.id = Ulid::new().to_string();
    message.author = webhook.id.clone();
    message.webhook = Some(revolt_models::v0::MessageWebhook { name: "Hook".into(), avatar: None });
    harness.db.insert_message(&message).await.unwrap();
    (webhook, message)
}

fn message_link(s: &Scene, id: &str) -> String {
    format!("https://app.zrzur.com/channel/{}/{}", s.channel.id(), id)
}

#[rocket::async_test]
async fn the_creator_answers_for_a_webhook_message_and_can_be_acted_on() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (_, message) = webhook_message(&harness, &s).await;
    // reported without an account
    let case = public_case_at(&harness, "Message", message_link(&s, &message.id)).await;
    assert_eq!(case.target_owner_id.as_deref(), Some(s.author.id.as_str()));
    // and in the app
    let (status, body) = post(&harness, &s.reporter_session, "/safety/reports", message_report(&message.id)).await;
    assert_eq!(status, Status::Ok, "{body}");
    let receipt: SafetyReceipt = serde_json::from_str(&body).unwrap();
    let in_app = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    assert_eq!(in_app.target_owner_id.as_deref(), Some(s.author.id.as_str()));

    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let (status, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", case.id), serde_json::json!({
        "kind": "RemoveMessage", "public_reason": "Stolen goods", "version": case.version,
        "statement": { "ground": { "type": "Law", "law": "StGB", "provision": "section 259" }, "facts": "Offered stolen goods" }
    })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let action: SafetyAction = serde_json::from_str(&body).unwrap();
    assert_eq!(action.subject_user_id.as_deref(), Some(s.author.id.as_str()), "the creator is told");
    assert!(harness.db.fetch_message(&message.id).await.is_err());
}

#[rocket::async_test]
async fn a_case_that_names_a_webhook_is_corrected_when_acting() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (webhook, message) = webhook_message(&harness, &s).await;
    let mut case = public_case_at(&harness, "Message", message_link(&s, &message.id)).await;
    // as cases filed before owners were resolved look
    case.target_owner_id = Some(webhook.id.clone());
    assert!(harness.db.update_safety_case(&case, case.version).await.unwrap());
    let case = harness.db.fetch_safety_case(&case.id).await.unwrap();
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let (status, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", case.id), warn(case.version)).await;
    assert_eq!(status, Status::Ok, "{body}");
    let action: SafetyAction = serde_json::from_str(&body).unwrap();
    assert_eq!(action.subject_user_id.as_deref(), Some(s.author.id.as_str()));
}

#[rocket::async_test]
async fn a_deleted_webhook_leaves_no_owner_but_its_message_can_go() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let mut message = s.message.clone();
    message.id = Ulid::new().to_string();
    message.author = Ulid::new().to_string();
    message.webhook = Some(revolt_models::v0::MessageWebhook { name: "Gone".into(), avatar: None });
    harness.db.insert_message(&message).await.unwrap();
    let case = public_case_at(&harness, "Message", message_link(&s, &message.id)).await;
    assert!(case.target_owner_id.is_none());
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let (status, _) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", case.id), warn(case.version)).await;
    assert_eq!(status, Status::BadRequest, "nobody can be warned");
    let case = harness.db.fetch_safety_case(&case.id).await.unwrap();
    let (status, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/actions", case.id), serde_json::json!({
        "kind": "RemoveMessage", "public_reason": "Stolen goods", "version": case.version,
        "statement": { "ground": { "type": "Terms", "rule": "rule:IllegalContent" }, "facts": "Offered stolen goods" }
    })).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn the_owner_answers_for_a_bot_message() {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (_, _, bot_owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&bot_owner).await;
    let mut message = s.message.clone();
    message.id = Ulid::new().to_string();
    message.author = bot.id.clone();
    harness.db.insert_message(&message).await.unwrap();
    let case = public_case_at(&harness, "Message", message_link(&s, &message.id)).await;
    assert_eq!(case.target_owner_id.as_deref(), Some(bot_owner.id.as_str()));
    let (status, body) = post(&harness, &s.reporter_session, "/safety/reports", message_report(&message.id)).await;
    assert_eq!(status, Status::Ok, "{body}");
    let receipt: SafetyReceipt = serde_json::from_str(&body).unwrap();
    let in_app = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    assert_eq!(in_app.target_owner_id.as_deref(), Some(bot_owner.id.as_str()));
}

async fn hold(harness: &TestHarness, case: &SafetyCase) {
    let senior = reviewer(harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyManage]).await;
    let (status, body) = patch(harness, &senior, &format!("/safety/admin/reports/{}", case.id),
        serde_json::json!({ "legal_hold": true, "version": case.version })).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn a_legal_hold_copies_servers_listings_bots_and_designs() {
    use revolt_models::v0::SafetyTargetKind;
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (_, _, bot_owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&bot_owner).await;
    let file = reported_file(&file_id(), &s.author.id, None, None);
    let item: revolt_database::CommunityItem = serde_json::from_value(serde_json::json!({
        "_id": Ulid::new().to_string(),
        "kind": "Decoration",
        "name": "Frame",
        "creator_id": s.author.id,
        "file": serde_json::to_value(&file).unwrap(),
        "status": "Published",
        "revision": 1,
    })).unwrap();
    harness.db.insert_community_item(&item).await.unwrap();

    for (kind, target, id, author) in [
        ("Server", SafetyTargetKind::Server, s.server.id.clone(), s.server.owner.clone()),
        ("DiscoverServer", SafetyTargetKind::DiscoverServer, s.server.id.clone(), s.server.owner.clone()),
        ("Bot", SafetyTargetKind::Bot, bot.id.clone(), bot.id.clone()),
        ("CommunityItem", SafetyTargetKind::CommunityItem, item.id.clone(), s.author.id.clone()),
    ] {
        let case = public_case_at(&harness, kind, format!("https://app.zrzur.com/x/{id}")).await;
        hold(&harness, &case).await;
        let evidence = harness.db.fetch_safety_evidence(&case.id).await.unwrap();
        assert_eq!(evidence.len(), 1, "{kind}");
        assert_eq!(evidence[0].kind, target);
        assert_eq!(evidence[0].object_id, id);
        assert_eq!(evidence[0].author_id.as_deref(), Some(author.as_str()), "{kind}");
        assert!(evidence[0].legal_hold && evidence[0].expires_at.is_none(), "{kind}");
    }
}

#[rocket::async_test]
async fn a_legal_hold_with_nothing_to_copy_is_recorded() {
    let harness = TestHarness::new().await;
    let case = public_case_at(&harness, "Server", "01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string()).await;
    hold(&harness, &case).await;
    let after = harness.db.fetch_safety_case(&case.id).await.unwrap();
    assert!(after.legal_hold, "the hold stands");
    assert!(harness.db.fetch_safety_evidence(&case.id).await.unwrap().is_empty());
    let log = harness.db.fetch_safety_log(&case.id).await.unwrap();
    assert!(log.iter().any(|entry| entry.event == "LegalHoldWithoutCopy"), "{log:?}");
}

/// What maildev received for an address, oldest first, as (subject, text)
async fn mails_to(address: &str, at_least: usize) -> Vec<(String, String)> {
    let mut found = vec![];
    for _ in 0..40 {
        let inbox: Vec<serde_json::Value> = reqwest::get("http://localhost:14080/email")
            .await
            .expect("maildev answers on 14080")
            .json()
            .await
            .unwrap();
        found = inbox
            .iter()
            .filter(|mail| mail["envelope"]["to"][0]["address"].as_str() == Some(address))
            .map(|mail| {
                (
                    mail["subject"].as_str().unwrap_or_default().to_string(),
                    mail["text"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        if found.len() >= at_least {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    found
}

/// A public report from an address of its own, in this language
async fn mailed_case(harness: &TestHarness, s: &Scene, language: &str) -> (SafetyCase, String) {
    let address = format!("{}@example.org", Ulid::new().to_string().to_lowercase());
    let mut body = public_report();
    body["location"] = message_link(s, &s.message.id).into();
    body["language"] = language.into();
    body["email"] = address.clone().into();
    let (status, text) = post_public(harness, body).await;
    assert_eq!(status, Status::Ok, "{text}");
    let receipt: SafetyReceipt = serde_json::from_str(&text).unwrap();
    (harness.db.fetch_safety_case(&receipt.id).await.unwrap(), address)
}

/// The receipt came first; then this notice, whole, in the reporter's language
async fn assert_notice(address: &str, language: &str, reference: &str, outcome: crate::util::official::ReportOutcome<'_>, note: Option<&str>) {
    let mails = mails_to(address, 2).await;
    assert_eq!(mails.len(), 2, "receipt and one notice: {mails:?}");
    let (subject, body) = crate::util::official::reporter_notice(language, reference, &outcome, note, true);
    let notice = mails.iter().find(|mail| mail.0 == subject).unwrap_or_else(|| panic!("no {subject:?} in {mails:?}"));
    assert!(mails.iter().any(|mail| mail.0 != subject && mail.1.contains(reference)), "the receipt names the reference");
    let text = notice.1.replace("\r\n", "\n");
    for part in body.split("\n\n") {
        assert!(text.contains(part), "missing {part:?} in {text:?}");
    }
}

/// Closing and asking for more start from a case under review; returns its version
async fn under_review(harness: &TestHarness, actor: &Session, case: &SafetyCase) -> i64 {
    let (code, body) = post(harness, actor, &format!("/safety/admin/reports/{}/status", case.id),
        serde_json::json!({ "status": "UnderReview", "version": case.version })).await;
    assert_eq!(code, Status::Ok, "{body}");
    serde_json::from_str::<SafetyCase>(&body).unwrap().version
}

async fn status_mail(status: &str, language: &str, note: Option<&str>, outcome: crate::util::official::ReportOutcome<'_>) {
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (case, address) = mailed_case(&harness, &s, language).await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let version = under_review(&harness, &actor, &case).await;
    let (code, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/status", case.id),
        serde_json::json!({ "status": status, "version": version, "public_note": note })).await;
    assert_eq!(code, Status::Ok, "{body}");
    assert_notice(&address, language, &case.reference, outcome, note).await;
}

#[rocket::async_test]
async fn mail_when_no_violation_was_found() {
    use crate::util::official::ReportOutcome;
    status_mail("NoViolation", "en", Some("Checked: allowed"), ReportOutcome::NoViolation).await;
}

#[rocket::async_test]
async fn mail_when_a_report_is_closed() {
    use crate::util::official::ReportOutcome;
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (case, address) = mailed_case(&harness, &s, "ar").await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let path = format!("/safety/admin/reports/{}/status", case.id);
    let version = under_review(&harness, &actor, &case).await;
    // a case closes undecided only after a question nobody answered
    let (code, body) = post(&harness, &actor, &path,
        serde_json::json!({ "status": "MoreInfoRequired", "version": version, "public_note": "أي قناة؟" })).await;
    assert_eq!(code, Status::Ok, "{body}");
    let asked: SafetyCase = serde_json::from_str(&body).unwrap();
    let (code, body) = post(&harness, &actor, &path,
        serde_json::json!({ "status": "Closed", "version": asked.version })).await;
    assert_eq!(code, Status::Ok, "{body}");
    let mails = mails_to(&address, 3).await;
    assert_eq!(mails.len(), 3, "receipt, question and closing: {mails:?}");
    let (_, body) = crate::util::official::reporter_notice("ar", &case.reference, &ReportOutcome::Closed, None, true);
    let line = body.split("\n\n").nth(1).unwrap();
    assert!(mails.iter().any(|mail| mail.1.contains(line)), "{mails:?}");
}

#[rocket::async_test]
async fn mail_when_more_information_is_needed() {
    use crate::util::official::ReportOutcome;
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (case, address) = mailed_case(&harness, &s, "de").await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let version = under_review(&harness, &actor, &case).await;
    let (code, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/status", case.id),
        serde_json::json!({ "status": "MoreInfoRequired", "version": version, "public_note": "Welcher Kanal?" })).await;
    assert_eq!(code, Status::Ok, "{body}");
    assert_notice(&address, "de", &case.reference, ReportOutcome::Info("Welcher Kanal?"), None).await;
}

#[rocket::async_test]
async fn mail_when_a_report_is_a_duplicate() {
    use crate::util::official::ReportOutcome;
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let first = public_case_for(&harness, &s).await;
    let (case, address) = mailed_case(&harness, &s, "en").await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let (code, body) = patch(&harness, &actor, &format!("/safety/admin/reports/{}", case.id),
        serde_json::json!({ "duplicate_of": first.id, "version": case.version })).await;
    assert_eq!(code, Status::Ok, "{body}");
    let marked: SafetyCase = serde_json::from_str(&body).unwrap();
    let (code, body) = post(&harness, &actor, &format!("/safety/admin/reports/{}/status", case.id),
        serde_json::json!({ "status": "Duplicate", "version": marked.version })).await;
    assert_eq!(code, Status::Ok, "{body}");
    assert_notice(&address, "en", &case.reference, ReportOutcome::Duplicate, None).await;
}

/// A warning on a mailed case; returns the action
async fn acted_on(harness: &TestHarness, actor: &Session, case: &SafetyCase) -> SafetyAction {
    let (code, body) = post(harness, actor, &format!("/safety/admin/reports/{}/actions", case.id), warn(case.version)).await;
    assert_eq!(code, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[rocket::async_test]
async fn mail_when_an_action_was_taken() {
    use crate::util::official::ReportOutcome;
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (case, address) = mailed_case(&harness, &s, "ar").await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let action = acted_on(&harness, &actor, &case).await;
    assert_notice(&address, "ar", &case.reference, ReportOutcome::Action(&action), None).await;
}

#[rocket::async_test]
async fn mail_when_an_action_is_revoked_without_an_appeal() {
    use crate::util::official::ReportOutcome;
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (case, address) = mailed_case(&harness, &s, "de").await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let action = acted_on(&harness, &actor, &case).await;
    let (code, body) = post(&harness, &actor, &format!("/safety/admin/actions/{}/revoke", action.id),
        serde_json::json!({ "reason": "We made a mistake" })).await;
    assert_eq!(code, Status::Ok, "{body}");
    let mails = mails_to(&address, 3).await;
    assert_eq!(mails.len(), 3, "receipt, decision and reversal: {mails:?}");
    let (_, body) = crate::util::official::reporter_notice("de", &case.reference, &ReportOutcome::Revoked, None, true);
    let line = body.split("\n\n").nth(1).unwrap();
    assert!(mails.iter().any(|mail| mail.1.contains(line)), "{mails:?}");
}

#[rocket::async_test]
async fn mail_when_an_appeal_overturns_the_action() {
    use crate::util::official::ReportOutcome;
    let harness = TestHarness::new().await;
    let s = scene(&harness).await;
    let (case, address) = mailed_case(&harness, &s, "en").await;
    let actor = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let action = acted_on(&harness, &actor, &case).await;
    let (code, body) = post(&harness, &s.author_session, &format!("/safety/standing/{}/appeal", action.id),
        serde_json::json!({ "statement": "It was a joke between friends" })).await;
    assert_eq!(code, Status::Ok, "{body}");
    let appeal: SafetyAppeal = serde_json::from_str(&body).unwrap();
    let second = reviewer(&harness, vec![PlatformPermission::SafetyReview, PlatformPermission::SafetyAction]).await;
    let (code, body) = post(&harness, &second, &format!("/safety/admin/appeals/{}", appeal.id),
        serde_json::json!({ "overturn": true, "note": "Context shows it was a joke." })).await;
    assert_eq!(code, Status::Ok, "{body}");
    let mails = mails_to(&address, 3).await;
    assert_eq!(mails.len(), 3, "receipt, decision and reversal: {mails:?}");
    let (_, body) = crate::util::official::reporter_notice("en", &case.reference, &ReportOutcome::Overturned, None, true);
    let line = body.split("\n\n").nth(1).unwrap();
    assert!(mails.iter().any(|mail| mail.1.contains(line)), "{mails:?}");
}

#[rocket::async_test]
async fn the_public_form_takes_an_effect_and_flashing_is_urgent() {
    let harness = TestHarness::new().await;
    let (_, _, creator) = harness.new_user().await;
    let file = reported_file(&file_id(), &creator.id, None, None);
    let item: revolt_database::CommunityItem = serde_json::from_value(serde_json::json!({
        "_id": Ulid::new().to_string(),
        "kind": "Effect",
        "name": "Sparks",
        "creator_id": creator.id,
        "file": serde_json::to_value(&file).unwrap(),
        "status": "Published",
        "revision": 1,
    }))
    .unwrap();
    harness.db.insert_community_item(&item).await.unwrap();

    let mut body = public_report();
    body["target_kind"] = "CommunityItem".into();
    body["location"] = format!("https://designs.zrzur.com/{}", item.id).into();
    body["category"] = "Other".into();
    body["subcategory"] = "flashing".into();
    body["description"] = "It flashes hard over the whole card".into();
    let (status, answer) = post_public(&harness, body).await;
    assert_eq!(status, Status::Ok, "{answer}");

    let receipt: SafetyReceipt = serde_json::from_str(&answer).unwrap();
    let case = harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    assert_eq!(case.target.id, item.id);
    assert_eq!(case.target_owner_id.as_deref(), Some(creator.id.as_str()));
    assert_eq!(case.priority, revolt_models::v0::SafetyPriority::Urgent);
}
