//! Verification for large bots: unverified bots stop at 100 servers. The
//! request and the staff decision are tested in `routes::staff`.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{Bot, Member, MemberCompositeKey, PartialBot, Session};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use ulid::Ulid;

/// Put the bot into `count` servers without creating them
async fn fill_servers(harness: &TestHarness, bot: &str, count: usize) {
    for _ in 0..count {
        harness
            .db
            .insert_or_merge_member(&Member {
                id: MemberCompositeKey {
                    server: Ulid::new().to_string(),
                    user: bot.to_string(),
                },
                ..Default::default()
            })
            .await
            .unwrap();
    }
}

async fn set_flags(harness: &TestHarness, bot: &Bot, flags: i32) {
    harness
        .db
        .update_bot(
            &bot.id,
            &PartialBot {
                flags: Some(flags),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

async fn invite(harness: &TestHarness, session: &Session, bot: &Bot, server: &str) -> (Status, String) {
    let response = harness
        .client
        .post(format!("/bots/{}/invite", bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "server": server }).to_string())
        .dispatch()
        .await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

#[rocket::async_test]
async fn the_101st_install_of_an_unverified_bot_is_refused() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    fill_servers(&harness, &bot.id, 99).await;

    // the 100th server is fine
    let (server, _) = harness.new_server(&owner).await;
    let (status, body) = invite(&harness, &session, &bot, &server.id).await;
    assert_eq!(status, Status::NoContent, "{body}");

    // the 101st is not
    let (server, _) = harness.new_server(&owner).await;
    let (status, body) = invite(&harness, &session, &bot, &server.id).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("BotNeedsVerification"), "{body}");
    assert!(harness.db.fetch_member(&server.id, &bot.id).await.is_err());
}

#[rocket::async_test]
async fn a_verified_bot_is_not_limited() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    set_flags(&harness, &bot, v0::BotFlags::Verified as i32).await;
    fill_servers(&harness, &bot.id, 100).await;

    let (server, _) = harness.new_server(&owner).await;
    let (status, body) = invite(&harness, &session, &bot, &server.id).await;
    assert_eq!(status, Status::NoContent, "{body}");
}

async fn apply(harness: &TestHarness, session: &Session, bot: &Bot, body: serde_json::Value) -> Status {
    harness
        .client
        .post(format!("/bots/{}/verification", bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await
        .status()
}

const STATEMENT: &str = "Moderation helper used in 40 Arabic communities.";

#[rocket::async_test]
async fn the_weekly_bot_limit_says_when_it_resets() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let per_week = revolt_config::config()
        .await
        .features
        .limits
        .global
        .bot_creations_per_week;
    assert_eq!(per_week, 7, "the documented limit");
    for _ in 0..per_week {
        harness.new_bot(&owner).await;
    }
    let before = chrono_free_now();
    let error = Bot::create(&harness.db, TestHarness::rand_string(), &owner, None)
        .await
        .expect_err("over the limit");
    let revolt_result::ErrorType::TooManyBotCreations { max, resets_at } = error.error_type else {
        panic!("wrong error: {:?}", error.error_type);
    };
    assert_eq!(max, 7);
    const WEEK: i64 = 7 * 24 * 60 * 60 * 1000;
    let resets_at = resets_at.expect("the date");
    assert!(resets_at > before + WEEK - 300_000 && resets_at <= before + WEEK, "{resets_at}");
}

fn chrono_free_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// The owner saw `statement: Validation error: length [...]`
#[rocket::async_test]
async fn a_refused_form_names_the_field_and_no_validator_text() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;

    for (statement, privacy, field, rule, min) in [
        ("too short", "https://example.org/p", "statement", "length", Some(20)),
        // long enough for the rule, too short once the spaces are gone
        ("   short once trimmed             ", "https://example.org/p", "statement", "length", Some(20)),
        (STATEMENT, "http://example.org/p", "privacy_policy_url", "https", None),
    ] {
        let response = harness
            .client
            .post(format!("/bots/{}/verification", bot.id))
            .header(Header::new("x-session-token", session.token.clone()))
            .header(ContentType::JSON)
            .body(
                json!({
                    "statement": statement,
                    "privacy_policy_url": privacy,
                    "terms_of_service_url": "https://example.org/t",
                })
                .to_string(),
            )
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
        let text = response.into_string().await.expect("body");
        for raw in ["Validation error", "\"params\"", "\"value\"", "[{\"code", statement] {
            if raw != STATEMENT {
                assert!(!text.contains(raw), "{raw} in {text}");
            }
        }
        let body: serde_json::Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(body["type"], "FailedValidation");
        assert_eq!(body["fields"][0]["field"], field);
        assert_eq!(body["fields"][0]["rule"], rule);
        assert_eq!(body["fields"][0]["min"].as_i64(), min);
    }
}

#[rocket::async_test]
async fn verification_needs_both_links_over_https() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;

    let good = "https://example.org/policy";
    for (privacy, terms) in [
        ("", good),
        (good, ""),
        ("http://example.org/policy", good),
        (good, "http://example.org/terms"),
        ("https://", good),
        ("ftp://example.org", good),
        ("javascript:alert(1)", good),
        ("https://user:secret@example.org", good),
        ("https://example.org/a b", good),
    ] {
        let status = apply(
            &harness,
            &session,
            &bot,
            json!({ "statement": STATEMENT, "privacy_policy_url": privacy, "terms_of_service_url": terms }),
        )
        .await;
        assert_eq!(status, Status::BadRequest, "{privacy} / {terms}");
    }
    // no links at all, as an old client sends
    let status = apply(&harness, &session, &bot, json!({ "statement": STATEMENT })).await;
    assert_eq!(status, Status::BadRequest);
    assert!(harness.db.fetch_bot_verifications_for_bot(&bot.id).await.unwrap().is_empty());
}

#[rocket::async_test]
async fn the_links_go_on_the_request_and_the_bot() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;

    let status = apply(
        &harness,
        &session,
        &bot,
        json!({
            "statement": STATEMENT,
            "privacy_policy_url": " https://example.org/privacy ",
            "terms_of_service_url": "https://example.org/terms",
        }),
    )
    .await;
    assert_eq!(status, Status::Ok);

    let request = harness.db.fetch_bot_verifications_for_bot(&bot.id).await.unwrap().remove(0);
    assert_eq!(request.privacy_policy_url.as_deref(), Some("https://example.org/privacy"));
    assert_eq!(request.terms_of_service_url.as_deref(), Some("https://example.org/terms"));
    let saved = harness.db.fetch_bot(&bot.id).await.unwrap();
    assert_eq!(saved.privacy_policy_url, "https://example.org/privacy");
    assert_eq!(saved.terms_of_service_url, "https://example.org/terms");
}
