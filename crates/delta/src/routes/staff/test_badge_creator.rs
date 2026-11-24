//! Badge programs, Phase 5: the Content Creator program and the switch for
//! applying from the app (`[features.badge_applications]`). A child of
//! `test`, so it uses the same staff helpers. `Revolt.test.toml` keeps the
//! owner's Content Creator numbers (100,000 followers, 90 days, 5 works).

use super::*;
use revolt_config::ApiBadgePrograms;
use revolt_database::{
    clean_creator, creator_lines, held_badges, mutate_badge, profile_code, AuditOutcome, BadgeActor,
    BadgeApplication, BadgeChange, BadgeMutation, BadgePath, BadgeProgram, CreatorChannel, CreatorFigures,
    OwnershipChallenge, OwnershipMethod, PlatformAuditQuery, CREATOR_QUALITY,
};
use rocket::http::Method::{Get, Post};

const DAY: i64 = 24 * 60 * 60 * 1000;
const CHANNEL: &str = "https://www.tiktok.com/@maya.teaches";

fn check<'a>(body: &'a Value, list: &str, code: &str) -> &'a Value {
    body[list].as_array().unwrap().iter().find(|c| c["code"] == code).unwrap_or(&Value::Null)
}

fn channel() -> Value {
    json!({
        "platform": "tiktok",
        "url": CHANNEL,
        "category": "education",
        "works": [
            "https://www.tiktok.com/@maya.teaches/video/1",
            "https://www.tiktok.com/@maya.teaches/video/2",
            "https://www.tiktok.com/@maya.teaches/video/3",
            "https://www.tiktok.com/@maya.teaches/video/4",
            "https://www.tiktok.com/@maya.teaches/video/5",
        ],
    })
}

async fn apply(harness: &TestHarness, session: &Session, creator: Value) -> (Status, Value) {
    as_user(
        harness,
        session,
        Post,
        "/badges/applications",
        json!({
            "program": "content_creator",
            "target_id": "@me",
            "statement": "Short Arabic grammar lessons for learners, three new videos every week.",
            "authorized": true,
            "creator": creator,
        }),
    )
    .await
}

/// A sent Content Creator application of `who`, with its profile code
async fn sent_application(harness: &TestHarness, who: &Member) -> (String, String) {
    let (status, draft) = apply(harness, &who.session, channel()).await;
    assert_eq!(status, Status::Ok, "{draft}");
    let id = draft["_id"].as_str().unwrap().to_string();
    let (status, issued) = as_user(
        harness,
        &who.session,
        Post,
        &format!("/badges/applications/{id}/ownership"),
        json!({ "method": "profile" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{issued}");
    let code = issued["value"].as_str().unwrap().to_string();
    let (status, sent) = as_user(harness, &who.session, Post, &format!("/badges/applications/{id}/submit"), json!({})).await;
    assert_eq!(status, Status::Ok, "{sent}");
    (id, code)
}

fn figures(followers: i64) -> Value {
    json!({
        "followers": followers,
        "seen_at": staff_now() - DAY,
        "active_days": 120,
        "works": 6,
        "quality": CREATOR_QUALITY,
    })
}

#[test]
fn creator_lines_wait_for_a_person_and_a_fresh_figure() {
    let programs = ApiBadgePrograms::default();
    assert_eq!(programs.creator_followers, 100_000);
    assert_eq!(programs.creator_active_days, 90);
    assert_eq!(programs.creator_works, 5);
    let now = staff_now();

    // before an application: every Content Creator line is a person's
    let before = creator_lines(&programs, None, now);
    assert!(before.iter().all(|line| line.met.is_none()), "{before:?}");
    assert!(before.iter().any(|line| line.code == "followers" && line.needed == Some(100_000)));

    let mut application = BadgeApplication {
        id: "a".into(),
        program: BadgeProgram::ContentCreator,
        applicant_id: "u".into(),
        target_id: "u".into(),
        status: revolt_database::ApplicationStatus::InReview,
        exception: false,
        statement: String::new(),
        links: vec![],
        authorized: true,
        ownership: Some(OwnershipChallenge {
            method: OwnershipMethod::Profile,
            domain: "www.tiktok.com".into(),
            code_hash: "x".into(),
            issued_at: now,
            expires_at: now + DAY,
            verified_at: None,
        }),
        creator: Some(CreatorChannel {
            platform: "tiktok".into(),
            platform_name: None,
            url: CHANNEL.into(),
            category: "comedy".into(),
            works: vec![],
            schedule: Some("Live every Friday at 20:00".into()),
        }),
        creator_figures: None,
        eligibility: vec![],
        created_at: now,
        updated_at: now,
        submitted_at: Some(now),
        reviewer_id: None,
        first_approval_by: None,
        badge_request_id: None,
        decided_at: None,
        decided_by: None,
        reasons: vec![],
        note: None,
        reapply_after: None,
        appeal_until: None,
        appeal: None,
        next_review_at: None,
        recheck_due: false,
        improve_until: None,
        ended_at: None,
        end_reason: None,
        evidence_purge_at: None,
        evidence_purged: false,
        purge_at: None,
    };
    let line = |items: &[revolt_database::EligibilityItem], code: &str| items.iter().find(|i| i.code == code).unwrap().met;
    // a schedule stands in for the works link list; the code waits for a reviewer
    let lines = creator_lines(&programs, Some(&application), now);
    assert_eq!(line(&lines, "channel"), Some(true));
    assert_eq!(line(&lines, "ownership"), None);
    assert_eq!(line(&lines, "followers"), None);

    // the owner's numbers: one under is not enough, on ONE platform
    let mut figures = CreatorFigures {
        followers: 99_999,
        seen_at: now - DAY,
        active_days: 89,
        works: 4,
        schedule: false,
        quality: CREATOR_QUALITY[..6].iter().map(|c| c.to_string()).collect(),
        recorded_by: "r".into(),
        recorded_at: now,
    };
    application.creator_figures = Some(figures.clone());
    let lines = creator_lines(&programs, Some(&application), now);
    for code in ["followers", "active_days", "works", "quality"] {
        assert_eq!(line(&lines, code), Some(false), "{code}");
    }
    figures.followers = 100_000;
    figures.active_days = 90;
    figures.works = 0;
    figures.schedule = true;
    figures.quality = CREATOR_QUALITY.iter().map(|c| c.to_string()).collect();
    application.creator_figures = Some(figures.clone());
    let lines = creator_lines(&programs, Some(&application), now);
    for code in ["followers", "active_days", "works", "quality"] {
        assert_eq!(line(&lines, code), Some(true), "{code}");
    }
    // a figure seen more than 30 days ago has to be looked at again
    figures.seen_at = now - 31 * DAY;
    application.creator_figures = Some(figures);
    assert_eq!(line(&creator_lines(&programs, Some(&application), now), "followers"), None);

    // the channel: known platforms in plain text, https only, a kind of content
    let base = CreatorChannel {
        platform: "YouTube".into(),
        platform_name: None,
        url: "https://www.youtube.com/@maya".into(),
        category: "gaming".into(),
        works: vec![],
        schedule: None,
    };
    assert_eq!(clean_creator(&base).unwrap().platform, "youtube");
    for broken in [
        CreatorChannel { platform: "myspace".into(), ..base.clone() },
        CreatorChannel { platform: "other".into(), ..base.clone() },
        CreatorChannel { url: "http://www.youtube.com/@maya".into(), ..base.clone() },
        CreatorChannel { category: "memes-only".into(), ..base.clone() },
        CreatorChannel { works: vec!["https://a.example/1".into(); 11], ..base.clone() },
    ] {
        assert!(clean_creator(&broken).is_err(), "{broken:?}");
    }
    let other = CreatorChannel { platform: "other".into(), platform_name: Some("Nebula".into()), ..base };
    assert_eq!(clean_creator(&other).unwrap().platform_name.as_deref(), Some("Nebula"));
    // the code is compared the way a person types it
    assert_eq!(profile_code("  ZRZUR-abc  "), "zrzur-abc");
}

#[rocket::async_test]
async fn content_creator_needs_a_reviewer_who_saw_the_channel_and_a_second_person() {
    let harness = TestHarness::new().await;
    // the applicant is on the team too: never their own reviewer
    let maya = member(&harness, vec![StaffRole::SeniorSupport]).await;

    // before applying: nothing measured, a person checks every channel line
    let (status, body) = as_user(&harness, &maya.session, Get, "/badges/programs/content_creator/eligibility?target=@me", json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["can_apply"], true, "{body}");
    assert!(check(&body, "checks", "followers")["met"].is_null());
    assert_eq!(check(&body, "checks", "followers")["needed"], 100_000);
    // nobody applies for someone else
    let (_, _, other) = harness.new_user().await;
    let (status, _) = as_user(
        &harness,
        &maya.session,
        Get,
        &format!("/badges/programs/content_creator/eligibility?target={}", other.id),
        json!({}),
    )
    .await;
    assert_eq!(status, Status::NotFound);

    // the channel is checked as it is written
    let (status, body) = apply(&harness, &maya.session, json!({ "platform": "myspace", "url": CHANNEL, "category": "education" })).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "platform");
    let (status, body) = as_user(&harness, &maya.session, Post, "/badges/applications",
        json!({ "program": "partner", "target_id": "x", "creator": channel() })).await;
    assert_eq!(status, Status::BadRequest, "{body}");

    let (status, draft) = apply(&harness, &maya.session, channel()).await;
    assert_eq!(status, Status::Ok, "{draft}");
    assert_eq!(draft["target_id"], maya.user.id.as_str());
    assert_eq!(draft["creator"]["platform"], "tiktok");
    let id = draft["_id"].as_str().unwrap().to_string();
    // without a code in the channel, it can't be sent
    let (status, body) = as_user(&harness, &maya.session, Post, &format!("/badges/applications/{id}/submit"), json!({})).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "not_eligible:ownership");
    // a DNS check is not for this program; the profile code is
    let (status, _) = as_user(&harness, &maya.session, Post, &format!("/badges/applications/{id}/ownership"),
        json!({ "method": "dns", "domain": "tiktok.com" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, issued) = as_user(&harness, &maya.session, Post, &format!("/badges/applications/{id}/ownership"),
        json!({ "method": "profile" })).await;
    assert_eq!(status, Status::Ok, "{issued}");
    let code = issued["value"].as_str().unwrap().to_string();
    assert!(code.starts_with("zrzur-") && code.len() == 18, "{code}");
    assert_eq!(issued["application"]["ownership"]["method"], "profile");
    assert_eq!(issued["application"]["ownership"]["domain"], "www.tiktok.com");
    assert_eq!(issued["application"]["ownership"]["record_name"], "");
    // the server never looks itself: a reviewer does
    let (status, body) = as_user(&harness, &maya.session, Post, &format!("/badges/applications/{id}/ownership/verify"), json!({})).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "checked_by_reviewer");
    // only the hash is stored
    let stored = harness.db.fetch_badge_application(&id).await.unwrap();
    assert!(!format!("{stored:?}").contains(&code));

    let (status, sent) = as_user(&harness, &maya.session, Post, &format!("/badges/applications/{id}/submit"), json!({})).await;
    assert_eq!(status, Status::Ok, "{sent}");
    assert_eq!(sent["status"], "Submitted");
    assert!(check(&sent, "checks", "ownership")["met"].is_null());

    let a = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let b = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let a_token = sign_in(&harness, &a, 0).await;
    let b_token = sign_in(&harness, &b, 0).await;
    let maya_token = sign_in(&harness, &maya, 0).await;
    let detail = format!("/staff/badges/applications/{id}");
    let decide = format!("{detail}/decide");
    let seen = format!("{detail}/creator/ownership");
    let record = format!("{detail}/creator/figures");

    // in the queue, with the channel; the applicant can't review it
    let (status, queue) = get(&harness, &a, &a_token, "/staff/badges/applications?program=content_creator").await;
    assert_eq!(status, Status::Ok, "{queue}");
    assert!(queue.as_array().unwrap().iter().any(|r| r["id"] == id.as_str()));
    let (status, _) = send(&harness, &maya, &maya_token, Post, &seen, json!({ "code": code })).await;
    assert_eq!(status, Status::Forbidden);
    let (status, body) = get(&harness, &a, &a_token, &detail).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["creator"]["url"], CHANNEL);
    assert_eq!(body["quality_criteria"].as_array().unwrap().len(), 7);
    assert!(check(&body, "live_checks", "followers")["met"].is_null());

    // nothing is approved before a person settled every line
    step_up(&harness, &a, &a_token).await;
    let (status, body) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::BadRequest);
    assert!(body["error"].as_str().unwrap().contains("ownership"), "{body}");

    // the code the reviewer saw on the channel: a wrong one is refused
    let (status, body) = send(&harness, &a, &a_token, Post, &seen, json!({ "code": "zrzur-notthecode" })).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "challenge_not_found");
    let typed = format!("  {}  ", code.to_uppercase());
    let (status, body) = send(&harness, &a, &a_token, Post, &seen, json!({ "code": typed })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["ownership"]["verified_at"].is_i64());

    // the figure is the reviewer's, with its day: one under is not enough
    let (status, body) = send(&harness, &a, &a_token, Post, &record, figures(99_999)).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(check(&body, "live_checks", "followers")["met"], false);
    let (status, body) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "not_eligible:followers");
    // nonsense is refused: a future day, an unknown criterion
    let mut future = figures(100_000);
    future["seen_at"] = json!(staff_now() + 3 * DAY);
    let (status, _) = send(&harness, &a, &a_token, Post, &record, future).await;
    assert_eq!(status, Status::BadRequest);
    let mut unknown = figures(100_000);
    unknown["quality"] = json!(["popularity"]);
    let (status, _) = send(&harness, &a, &a_token, Post, &record, unknown).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &a, &a_token, Post, &record, figures(100_000)).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["creator_figures"]["followers"], 100_000);
    assert_eq!(check(&body, "live_checks", "quality")["met"], true);

    // first approval: the badge waits for a second person
    let (status, body) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "InReview");
    assert_eq!(body["first_approval_by"], a.user.id.as_str());
    assert!(body["badge_request_id"].is_string());
    assert!(!held_badges(&harness.db, &maya.user.id).await.unwrap().contains("content_creator"));
    let (status, _) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Forbidden);
    step_up(&harness, &b, &b_token).await;
    let (status, body) = send(&harness, &b, &b_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Approved");
    assert!(held_badges(&harness.db, &maya.user.id).await.unwrap().contains("content_creator"));
    let bits = harness.db.fetch_user(&maya.user.id).await.unwrap().badges.unwrap_or_default() as u32;
    assert_ne!(bits & revolt_models::v0::UserBadges::ContentCreator as u32, 0);

    // the applicant sees the decision, the channel, never who decided
    let (_, mine) = as_user(&harness, &maya.session, Get, "/badges/applications/@me", json!({})).await;
    assert_eq!(mine[0]["status"], "Approved");
    assert_eq!(mine[0]["creator"]["url"], CHANNEL);
    let text = mine.to_string();
    assert!(!text.contains(&a.user.id) && !text.contains(&b.user.id), "{text}");
    // the reviewer's record stays with the team (the checklist sent shows
    // only that a person checks the figure)
    assert!(!text.contains("creator_figures") && !text.contains("recorded_by"), "{text}");
    // another application while it stands: refused
    let (status, body) = apply(&harness, &maya.session, channel()).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "already_approved");

    // the log has ids and outcomes: never the channel or the code
    let records = harness
        .db
        .fetch_platform_audit(PlatformAuditQuery {
            action_prefix: Some("staff.badge.".to_string()),
            limit: 200,
            ..Default::default()
        })
        .await
        .unwrap();
    for action in [
        "staff.badge.application.ownership",
        "staff.badge.application.figures",
        "staff.badge.application.approve_first",
        "staff.badge.application.approve",
        "staff.badge.grant.request",
    ] {
        assert!(records.iter().any(|e| e.action == action && matches!(e.outcome, AuditOutcome::Success)), "{action}");
    }
    assert!(records.iter().any(|e| e.action == "staff.badge.application.ownership" && matches!(e.outcome, AuditOutcome::Denied)));
    let logged = format!("{records:?}");
    assert!(!logged.contains("tiktok") && !logged.contains(&code) && !logged.contains("notthecode"), "{logged}");

    // no path gives the badge without an application
    let rights = BadgeActor {
        id: a.user.id.clone(),
        path: BadgePath::StaffConsole,
        staff: true,
        owner: true,
        step_up: true,
        session_id: None,
    };
    let refused = mutate_badge(&harness.db, &rights, BadgeMutation {
        user_id: other.id.clone(),
        badge: "content_creator".into(),
        change: BadgeChange::Grant,
        reason: "famous streamer".into(),
        expires_at: None,
        source_ref: None,
    })
    .await
    .unwrap_err();
    assert!(format!("{refused:?}").contains("content creator application"), "{refused:?}");
    let system = BadgeMutation {
        user_id: other.id.clone(),
        badge: "content_creator".into(),
        change: BadgeChange::Grant,
        reason: "followers".into(),
        expires_at: None,
        source_ref: Some("application:x".into()),
    };
    assert!(mutate_badge(&harness.db, &BadgeActor::system(), system).await.is_err());

    // the team takes it away in the badge console: the application ends
    let (status, body) = send(&harness, &b, &b_token, Post, &format!("/staff/users/{}/badges/revoke", maya.user.id),
        json!({ "badge": "content_creator", "reason": "Signs of bought followers found" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(!held_badges(&harness.db, &maya.user.id).await.unwrap().contains("content_creator"));
    let ended = harness.db.fetch_badge_application(&id).await.unwrap();
    assert_eq!(ended.status, revolt_database::ApplicationStatus::Ended);
    assert_eq!(ended.end_reason.as_deref(), Some("badge_removed"));
}

#[rocket::async_test]
async fn content_creator_refusals_wait_unless_only_information_was_missing() {
    let harness = TestHarness::new().await;
    let sam = member(&harness, vec![]).await;
    let lee = member(&harness, vec![]).await;
    let a = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let a_token = sign_in(&harness, &a, 0).await;
    step_up(&harness, &a, &a_token).await;

    // only information missing: apply again at once
    let (id, _) = sent_application(&harness, &sam).await;
    let (status, body) = send(&harness, &a, &a_token, Post, &format!("/staff/badges/applications/{id}/decide"),
        json!({ "decision": "refuse", "reasons": ["more_information"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["reapply_after"].as_i64().unwrap() <= staff_now());
    let (status, body) = apply(&harness, &sam.session, channel()).await;
    assert_eq!(status, Status::Ok, "{body}");

    // a quality reason: the 90-day wait and a 30-day appeal window
    let (id, _) = sent_application(&harness, &lee).await;
    let (status, body) = send(&harness, &a, &a_token, Post, &format!("/staff/badges/applications/{id}/decide"),
        json!({ "decision": "refuse", "reasons": ["originality", "rights"], "note": "Mostly reuploads of other channels" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["reapply_after"].as_i64().unwrap() > staff_now() + 89 * DAY);
    assert!(body["appeal_until"].as_i64().unwrap() > staff_now() + 29 * DAY);
    let (status, body) = apply(&harness, &lee.session, channel()).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "reapply_wait");
    // a genre is never a reason: no such code exists
    let (status, _) = send(&harness, &a, &a_token, Post, &format!("/staff/badges/applications/{id}/decide"),
        json!({ "decision": "refuse", "reasons": ["comedy"] })).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn applying_is_off_by_default_and_review_keeps_working() {
    revolt_config::overwrite_config(|config| config.features.badge_applications.enabled = false).await;
    let harness = TestHarness::new().await;
    let maya = member(&harness, vec![]).await;

    // the default in Revolt.toml is off, and `GET /` says so
    let (status, root) = as_user(&harness, &maya.session, Get, "/", json!({})).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(root["features"]["badge_applications"], false);
    // every applicant route answers FeatureDisabled
    for (method, path) in [
        (Get, "/badges/programs/targets".to_string()),
        (Get, "/badges/programs/content_creator/eligibility?target=@me".to_string()),
        (Get, "/badges/applications/@me".to_string()),
        (Post, "/badges/applications".to_string()),
        (Post, "/badges/applications/01HZZZZZZZZZZZZZZZZZZZZZZZ/submit".to_string()),
        (Post, "/badges/applications/01HZZZZZZZZZZZZZZZZZZZZZZZ/appeal".to_string()),
    ] {
        let (status, body) = as_user(&harness, &maya.session, method, &path, json!({ "program": "partner", "target_id": "x", "text": "x" })).await;
        assert_eq!(status, Status::BadRequest, "{path}: {body}");
        assert_eq!(body["type"], "FeatureDisabled", "{path}: {body}");
    }

    // an application made earlier is still reviewed in the admin console
    let now = staff_now();
    harness.back_partner(&maya.user.id).await;
    let made = harness.db.fetch_badge_applications_by_applicant(&maya.user.id).await.unwrap();
    let mut waiting = made[0].clone();
    let before = waiting.updated_at;
    waiting.status = revolt_database::ApplicationStatus::Submitted;
    waiting.updated_at = now + 1;
    assert!(harness.db.save_badge_application(&waiting, before).await.unwrap());
    let a = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let a_token = sign_in(&harness, &a, 0).await;
    let (status, queue) = get(&harness, &a, &a_token, "/staff/badges/applications").await;
    assert_eq!(status, Status::Ok, "{queue}");
    assert!(queue.as_array().unwrap().iter().any(|r| r["id"] == waiting.id.as_str()), "{queue}");
    let (status, body) = send(&harness, &a, &a_token, Post, &format!("/staff/badges/applications/{}/take", waiting.id), json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "InReview");
}

#[rocket::async_test]
async fn applying_is_reported_on_when_switched_on() {
    let harness = TestHarness::new().await;
    let maya = member(&harness, vec![]).await;
    let (status, root) = as_user(&harness, &maya.session, Get, "/", json!({})).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(root["features"]["badge_applications"], true);
    let (status, _) = as_user(&harness, &maya.session, Get, "/badges/applications/@me", json!({})).await;
    assert_eq!(status, Status::Ok);
}
