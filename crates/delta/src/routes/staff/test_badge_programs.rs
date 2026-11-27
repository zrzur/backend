//! Badge programs (Phase 4): applications, reviews with reasons, appeals,
//! two people for Partner, ownership checks, the bot legitimacy filter,
//! weekly activity from metadata, retention and the ends of a program. A
//! child of `test`, so it uses the same staff helpers. Numbers come from
//! `Revolt.test.toml` (`[api.badge_programs]`, small ones); the owner's
//! numbers are checked on the pure functions.

use super::*;
use revolt_config::ApiBadgePrograms;
use revolt_database::{
    checked_decision, clean_domain, erase_applications_of, from_epoch_ms, held_badges,
    measure_server_week, mutate_badge, partner_activity, program_upkeep, threshold_line, week_of,
    week_start, ApplicationStatus, AuditOutcome, BadgeActor, BadgeApplication, BadgeChange,
    BadgeEvidence, BadgeMutation, BadgePath, BadgeProgram, Member as ServerMember,
    MemberCompositeKey, Message, PlatformAuditQuery, ServerActivityWeek,
};
use rocket::http::Method::{Get, Post};

use crate::util::ownership_probe::set_test_answer;

const DAY: i64 = 24 * 60 * 60 * 1000;
const HOUR: i64 = 60 * 60 * 1000;

/// A genuine person: an account `days_old` days old (`n` keeps the times
/// apart) with a confirmed e-mail and a declared adult age
async fn genuine(harness: &TestHarness, days_old: i64, n: i64) -> User {
    let made = staff_now() - days_old * DAY - n * 3 * HOUR;
    let id = ulid::Ulid::from_parts(made as u64, rand::random::<u128>() & 0xffff_ffff).to_string();
    let user = User::create(&harness.db, TestHarness::rand_string(), Some(id.clone()), None)
        .await
        .unwrap();
    harness.account_from_user(id).await;
    user
}

async fn join(harness: &TestHarness, server: &str, user: &str, hours_ago: i64) {
    harness
        .db
        .insert_or_merge_member(&ServerMember {
            id: MemberCompositeKey {
                server: server.to_string(),
                user: user.to_string(),
            },
            joined_at: from_epoch_ms(staff_now() - hours_ago * HOUR),
            ..Default::default()
        })
        .await
        .unwrap();
}

/// `count` genuine members, joined days apart
async fn grow(harness: &TestHarness, server: &str, count: i64) {
    for n in 0..count {
        let user = genuine(harness, 30 + n * 2, n).await;
        join(harness, server, &user.id, 48 + n * 24).await;
    }
}

/// Two finished weeks of activity for Partner (test numbers: 2 active, 1
/// contributor, 20 % retention over 2 weeks)
async fn activity(harness: &TestHarness, server: &str) {
    let current = week_of(staff_now());
    for week in [current - 2, current - 1] {
        harness
            .db
            .save_server_activity_week(&ServerActivityWeek {
                id: format!("{server}:{week}"),
                server_id: server.to_string(),
                week,
                active: 5,
                contributors: 3,
                joins: 10,
                retained: Some(5),
                computed_at: staff_now(),
            })
            .await
            .unwrap();
    }
}

fn check<'a>(body: &'a Value, code: &str) -> &'a Value {
    body["checks"].as_array().unwrap().iter().find(|c| c["code"] == code).unwrap_or(&Value::Null)
}

async fn apply(harness: &TestHarness, session: &Session, program: &str, target: &str) -> (Status, Value) {
    as_user(
        harness,
        session,
        Post,
        "/badges/applications",
        json!({
            "program": program,
            "target_id": target,
            "statement": "An Arabic study community with weekly sessions and clear rules.",
            "links": ["https://example.org/rules"],
            "authorized": true,
        }),
    )
    .await
}

async fn audit(harness: &TestHarness, prefix: &str) -> Vec<revolt_database::PlatformAuditEntry> {
    harness
        .db
        .fetch_platform_audit(PlatformAuditQuery {
            action_prefix: Some(prefix.to_string()),
            limit: 200,
            ..Default::default()
        })
        .await
        .unwrap()
}

#[test]
fn the_owners_numbers_only_unlock_applying() {
    let real = ApiBadgePrograms::default();
    // Partner and Verified Server members, bot servers and age: one under is not enough
    for (value, needed, met) in [
        (4_999, real.partner_members as i64, false),
        (5_000, real.partner_members as i64, true),
        (9_999, real.verified_server_members as i64, false),
        (10_000, real.verified_server_members as i64, true),
        (24, real.bot_servers as i64, false),
        (25, real.bot_servers as i64, true),
        (29, real.bot_age_days, false),
        (30, real.bot_age_days, true),
        (179, real.partner_age_days, false),
        (180, real.verified_server_age_days, true),
    ] {
        assert_eq!(threshold_line("x", value, needed).met, Some(met), "{value} of {needed}");
    }

    // weekly averages over 8 finished weeks, retention over the joins
    let now = staff_now();
    let current = week_of(now);
    let rows = |active: u32, contributors: u32, joins: u32, retained: u32| -> Vec<ServerActivityWeek> {
        (current - 8..current)
            .map(|week| ServerActivityWeek {
                id: format!("s:{week}"),
                server_id: "s".into(),
                week,
                active,
                contributors,
                joins,
                retained: Some(retained),
                computed_at: now,
            })
            .collect()
    };
    let at = partner_activity(&rows(500, 200, 10, 2), &real, now);
    assert_eq!((at.weeks, at.avg_active, at.avg_contributors, at.retention_percent), (8, 500, 200, Some(20)));
    let below = partner_activity(&rows(499, 199, 10, 1), &real, now);
    assert_eq!((below.avg_active, below.avg_contributors, below.retention_percent), (499, 199, Some(10)));
    // fewer than 50 joins in the window: the reviewer decides retention
    assert_eq!(partner_activity(&rows(500, 200, 6, 6), &real, now).retention_percent, None);
    // a missing week counts as nothing, and the current week never counts
    let mut seven = rows(560, 230, 10, 2);
    seven.remove(0);
    let partial = partner_activity(&seven, &real, now);
    assert_eq!((partial.weeks, partial.avg_active), (7, 490));
    assert_eq!(week_start(week_of(now)) % DAY, 0);
    // weeks start on Monday (1970-01-05 was one)
    assert_eq!(week_start(0), 4 * DAY);

    // domains for ownership checks: host names only
    assert_eq!(clean_domain(" Example.ORG. ").as_deref(), Some("example.org"));
    for bad in ["localhost", "10.0.0.1", "https://example.org", "exa mple.org", "-a.org", "a..org", "example.org/x"] {
        assert!(clean_domain(bad).is_none(), "{bad}");
    }
    // reasons come from the program's list; `other` needs a note
    assert!(checked_decision(BadgeProgram::Partner, &["criteria_met".into()], None).is_ok());
    assert!(checked_decision(BadgeProgram::Partner, &["malicious".into()], None).is_err());
    assert!(checked_decision(BadgeProgram::Partner, &["other".into()], None).is_err());
    assert!(checked_decision(BadgeProgram::Partner, &["other".into()], Some("Rules page is empty")).is_ok());
    assert!(checked_decision(BadgeProgram::Partner, &[], None).is_err());
}

#[rocket::async_test]
async fn partner_needs_the_numbers_an_application_and_two_people() {
    let harness = TestHarness::new().await;
    // the server's owner is on the team too: never their own reviewer
    let owner = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let (server, _) = harness.new_server(&owner.user).await;
    grow(&harness, &server.id, 5).await;
    let eligibility = format!("/badges/programs/partner/eligibility?target={}", server.id);

    // no activity measured yet: applying stays closed
    let (status, body) = as_user(&harness, &owner.session, Get, &eligibility, json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["can_apply"], false);
    assert_eq!(check(&body, "activity_weeks")["met"], false);
    assert_eq!(check(&body, "members")["value"], 5);
    activity(&harness, &server.id).await;
    let (_, body) = as_user(&harness, &owner.session, Get, &eligibility, json!({})).await;
    assert_eq!(body["can_apply"], true, "{body}");
    assert_eq!(check(&body, "retention")["met"], true);
    // only the owner sees a server's checklist
    let (_, stranger, outsider) = harness.new_user().await;
    let (status, _) = as_user(&harness, &stranger, Get, &eligibility, json!({})).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = apply(&harness, &stranger, "partner", &server.id).await;
    assert_eq!(status, Status::NotFound);

    // draft, a private note, send
    let (status, draft) = apply(&harness, &owner.session, "partner", &server.id).await;
    assert_eq!(status, Status::Ok, "{draft}");
    assert_eq!(draft["status"], "Draft");
    let id = draft["_id"].as_str().unwrap().to_string();
    let (status, body) = as_user(&harness, &owner.session, Post, &format!("/badges/applications/{id}/evidence"),
        json!({ "kind": "note", "text": "Moderation log is public in #mod-log" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["evidence"].as_array().unwrap().len(), 1);
    let (status, body) = apply(&harness, &owner.session, "partner", &server.id).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "open_application");
    let (status, sent) = as_user(&harness, &owner.session, Post, &format!("/badges/applications/{id}/submit"), json!({})).await;
    assert_eq!(status, Status::Ok, "{sent}");
    assert_eq!(sent["status"], "Submitted");
    assert!(sent["checks"].as_array().unwrap().len() >= 8);

    let a = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let b = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let a_token = sign_in(&harness, &a, 0).await;
    let b_token = sign_in(&harness, &b, 0).await;
    let owner_token = sign_in(&harness, &owner, 0).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let detail = format!("/staff/badges/applications/{id}");
    let decide = format!("{detail}/decide");

    // the queue: Support may not look; the owner sees it marked as theirs
    let (status, _) = get(&harness, &support, &support_token, "/staff/badges/applications").await;
    assert_eq!(status, Status::Forbidden);
    let (status, queue) = get(&harness, &a, &a_token, "/staff/badges/applications").await;
    assert_eq!(status, Status::Ok, "{queue}");
    assert!(queue.as_array().unwrap().iter().any(|r| r["id"] == id.as_str() && r["conflict"] == false));
    let (_, queue) = get(&harness, &owner, &owner_token, "/staff/badges/applications?program=partner").await;
    assert!(queue.as_array().unwrap().iter().any(|r| r["id"] == id.as_str() && r["conflict"] == true));
    // the owner can't open or decide their own application
    let (status, _) = get(&harness, &owner, &owner_token, &detail).await;
    assert_eq!(status, Status::Forbidden);
    step_up(&harness, &owner, &owner_token).await;
    let (status, _) = send(&harness, &owner, &owner_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Forbidden);

    // a reviewer reads it (evidence included, the opening written down)
    let (status, body) = get(&harness, &a, &a_token, &detail).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["evidence"][0]["text"], "Moderation log is public in #mod-log");
    assert!(body["live_checks"].as_array().unwrap().iter().any(|c| c["code"] == "members" && c["met"] == true));
    // a decision needs a step-up
    let (status, _) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_ne!(status, Status::Ok);
    step_up(&harness, &a, &a_token).await;
    let (status, body) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "InReview");
    assert_eq!(body["first_approval_by"], a.user.id.as_str());
    assert!(!held_badges(&harness.db, &owner.user.id).await.unwrap().contains("partner"));
    // the same person can't be the second
    let (status, _) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Forbidden);
    step_up(&harness, &b, &b_token).await;
    let (status, body) = send(&harness, &b, &b_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"], "note": "Welcome" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Approved");
    assert!(body["next_review_at"].as_i64().unwrap() > staff_now() + 170 * DAY);
    assert!(held_badges(&harness.db, &owner.user.id).await.unwrap().contains("partner"));
    let flags = harness.db.fetch_server(&server.id).await.unwrap().flags.unwrap_or_default();
    assert_ne!(flags & revolt_database::SERVER_FLAG_PARTNER, 0);

    // the applicant sees the decision and its reasons, never who decided
    let (_, mine) = as_user(&harness, &owner.session, Get, "/badges/applications/@me", json!({})).await;
    assert_eq!(mine[0]["status"], "Approved");
    assert_eq!(mine[0]["reasons"][0], "criteria_met");
    let text = mine.to_string();
    assert!(!text.contains(&a.user.id) && !text.contains(&b.user.id), "{text}");

    // written down: both approvals, the badge through the gate, the view
    let records = audit(&harness, "staff.badge.").await;
    for action in ["staff.badge.application.view", "staff.badge.application.approve_first", "staff.badge.application.approve", "staff.badge.grant.request"] {
        assert!(records.iter().any(|e| e.action == action && matches!(e.outcome, AuditOutcome::Success)), "{action}");
    }
    assert!(records.iter().any(|e| e.action == "staff.badge.application.decide" && matches!(e.outcome, AuditOutcome::Denied)));
    // never the statement or the evidence
    assert!(!format!("{records:?}").contains("Moderation log"));

    // no console, command or line gives Partner without an application
    let rights = BadgeActor {
        id: a.user.id.clone(),
        path: BadgePath::StaffConsole,
        staff: true,
        owner: true,
        step_up: true,
        session_id: None,
    };
    let refused = mutate_badge(&harness.db, &rights, BadgeMutation {
        user_id: outsider.id.clone(),
        badge: "partner".into(),
        change: BadgeChange::Grant,
        reason: "big community".into(),
        expires_at: None,
        source_ref: None,
    })
    .await
    .unwrap_err();
    assert!(format!("{refused:?}").contains("partner application"), "{refused:?}");
    // the system gives Partner never, and takes it only naming the record
    let mut system = BadgeMutation {
        user_id: owner.user.id.clone(),
        badge: "partner".into(),
        change: BadgeChange::Revoke,
        reason: "partnership ended".into(),
        expires_at: None,
        source_ref: None,
    };
    assert!(mutate_badge(&harness.db, &BadgeActor::system(), system.clone()).await.is_err());
    system.change = BadgeChange::Grant;
    system.user_id = outsider.id.clone();
    system.source_ref = Some("application:x".into());
    assert!(mutate_badge(&harness.db, &BadgeActor::system(), system).await.is_err());

    // a recheck is flagged by crond and decided by a person
    let mut approved = harness.db.fetch_badge_application(&id).await.unwrap();
    let before = approved.updated_at;
    approved.next_review_at = Some(staff_now() - 1);
    approved.updated_at += 1;
    assert!(harness.db.save_badge_application(&approved, before).await.unwrap());
    let report = program_upkeep(&harness.db, &revolt_config::config().await.api.badge_programs, staff_now()).await.unwrap();
    assert_eq!(report.rechecks, 1);
    let (_, queue) = get(&harness, &a, &a_token, "/staff/badges/applications").await;
    assert!(queue.as_array().unwrap().iter().any(|r| r["id"] == id.as_str() && r["recheck_due"] == true));
    assert!(held_badges(&harness.db, &owner.user.id).await.unwrap().contains("partner"));
    let (status, body) = send(&harness, &a, &a_token, Post, &format!("{detail}/recheck"), json!({ "decision": "warn", "reasons": ["moderation"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["improve_until"].as_i64().unwrap() > staff_now() + 29 * DAY);
    let (status, body) = send(&harness, &b, &b_token, Post, &format!("{detail}/recheck"), json!({ "decision": "end", "reasons": ["metrics_not_met"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Ended");
    assert!(!held_badges(&harness.db, &owner.user.id).await.unwrap().contains("partner"));
    let flags = harness.db.fetch_server(&server.id).await.unwrap().flags.unwrap_or_default();
    assert_eq!(flags & revolt_database::SERVER_FLAG_PARTNER, 0);
}

#[rocket::async_test]
async fn a_refusal_has_reasons_one_appeal_and_a_wait() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    grow(&harness, &server.id, 5).await;
    let (status, draft) = apply(&harness, &session, "verified_server", &server.id).await;
    assert_eq!(status, Status::Ok, "{draft}");
    let id = draft["_id"].as_str().unwrap().to_string();
    let app = format!("/badges/applications/{id}");

    // the ownership check comes first
    let (status, body) = as_user(&harness, &session, Post, &format!("{app}/submit"), json!({})).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "not_eligible:ownership");
    let (status, body) = as_user(&harness, &session, Post, &format!("{app}/ownership"), json!({ "method": "dns", "domain": "localhost" })).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    let (status, issued) = as_user(&harness, &session, Post, &format!("{app}/ownership"), json!({ "method": "dns", "domain": "Study-Circle.ORG" })).await;
    assert_eq!(status, Status::Ok, "{issued}");
    let value = issued["value"].as_str().unwrap().to_string();
    assert!(value.starts_with("zrzur-verification="));
    assert_eq!(issued["application"]["ownership"]["record_name"], "_zrzur-verification.study-circle.org");
    // the code is kept hashed only
    let stored = harness.db.fetch_badge_application(&id).await.unwrap();
    assert!(!format!("{stored:?}").contains(&value[19..]));
    // nothing published yet, then the record
    let (status, body) = as_user(&harness, &session, Post, &format!("{app}/ownership/verify"), json!({})).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "challenge_not_found");
    set_test_answer("dns:_zrzur-verification.study-circle.org", vec!["v=spf1 -all".into(), value.clone()]);
    let (status, body) = as_user(&harness, &session, Post, &format!("{app}/ownership/verify"), json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["ownership"]["verified_at"].is_i64());
    let (status, _) = as_user(&harness, &session, Post, &format!("{app}/submit"), json!({})).await;
    assert_eq!(status, Status::Ok);

    // Verified Server needs servers.restrict as well as badges.manage
    let a = member(&harness, vec![StaffRole::TrustSafety, StaffRole::SeniorSupport]).await;
    let b = member(&harness, vec![StaffRole::TrustSafety, StaffRole::SeniorSupport]).await;
    let senior = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let a_token = sign_in(&harness, &a, 0).await;
    let b_token = sign_in(&harness, &b, 0).await;
    let senior_token = sign_in(&harness, &senior, 0).await;
    let decide = format!("/staff/badges/applications/{id}/decide");
    step_up(&harness, &senior, &senior_token).await;
    let (status, _) = send(&harness, &senior, &senior_token, Post, &decide, json!({ "decision": "refuse", "reasons": ["not_official"] })).await;
    assert_eq!(status, Status::Forbidden);
    step_up(&harness, &a, &a_token).await;
    // reasons from the list; `other` needs words
    let (status, _) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "refuse", "reasons": ["other"] })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &a, &a_token, Post, &decide, json!({ "decision": "refuse", "reasons": ["inflated_installs"] })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &a, &a_token, Post, &decide,
        json!({ "decision": "refuse", "reasons": ["not_official", "branding"], "note": "The website does not link to this server." })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Refused");
    let now = staff_now();
    assert!((body["appeal_until"].as_i64().unwrap() - (now + 30 * DAY)).abs() < HOUR);
    assert!((body["reapply_after"].as_i64().unwrap() - (now + 90 * DAY)).abs() < HOUR);

    // one appeal, with words
    let (status, _) = as_user(&harness, &session, Post, &format!("{app}/appeal"), json!({ "text": "Wrong" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = as_user(&harness, &session, Post, &format!("{app}/appeal"),
        json!({ "text": "Our website now links to the server from its footer." })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["appeal"]["status"], "Open");
    let (status, _) = as_user(&harness, &session, Post, &format!("{app}/appeal"),
        json!({ "text": "Our website now links to the server from its footer." })).await;
    assert_eq!(status, Status::BadRequest);
    // decided by someone other than the reviewer who refused
    let appeal = format!("/staff/badges/applications/{id}/appeal");
    let (status, _) = send(&harness, &a, &a_token, Post, &appeal, json!({ "uphold": false, "reasons": ["not_official"] })).await;
    assert_eq!(status, Status::Forbidden);
    step_up(&harness, &b, &b_token).await;
    let (status, body) = send(&harness, &b, &b_token, Post, &appeal, json!({ "uphold": false, "reasons": ["not_official"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["appeal"]["status"], "Rejected");

    // a new application waits; the checklist says until when
    let (status, body) = apply(&harness, &session, "verified_server", &server.id).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], "reapply_wait");
    let (_, body) = as_user(&harness, &session, Get, &format!("/badges/programs/verified_server/eligibility?target={}", server.id), json!({})).await;
    assert_eq!(body["blocked"], "reapply_wait");
    assert!(body["blocked_until"].as_i64().unwrap() > now + 89 * DAY);

    let records = audit(&harness, "staff.badge.application.").await;
    for action in ["staff.badge.application.refuse", "staff.badge.application.appeal_rejected"] {
        assert!(records.iter().any(|e| e.action == action && e.summary.as_deref().unwrap_or_default().contains("not_official")), "{action}");
    }
    assert!(records.iter().any(|e| e.action == "staff.badge.application.appeal" && matches!(e.outcome, AuditOutcome::Denied)));
    let own = audit(&harness, "badge.application.ownership").await;
    assert_eq!(own.iter().filter(|e| matches!(e.outcome, AuditOutcome::Denied)).count(), 1);
    assert!(!format!("{own:?}").contains(&value[19..]));
}

#[rocket::async_test]
async fn a_verified_server_comes_from_an_application_and_ends_with_its_owner() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    grow(&harness, &server.id, 5).await;
    let boss = member(&harness, vec![StaffRole::Owner]).await;
    let boss_token = sign_in(&harness, &boss, 0).await;
    step_up(&harness, &boss, &boss_token).await;

    // the console's old shortcut is closed
    let (status, body) = send(&harness, &boss, &boss_token, Post, &format!("/staff/servers/{}/actions", server.id),
        json!({ "action": "verify", "reason": "Official game studio" })).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["error"], crate::routes::staff::servers::APPLICATION_ONLY);

    // the website file proves control
    let (_, draft) = apply(&harness, &session, "verified_server", &server.id).await;
    let id = draft["_id"].as_str().unwrap().to_string();
    let (_, issued) = as_user(&harness, &session, Post, &format!("/badges/applications/{id}/ownership"),
        json!({ "method": "website", "domain": "studio.example.net" })).await;
    assert_eq!(issued["application"]["ownership"]["file_url"], "https://studio.example.net/.well-known/zrzur-verification.txt");
    set_test_answer("web:studio.example.net", vec![issued["value"].as_str().unwrap().to_string()]);
    let (status, _) = as_user(&harness, &session, Post, &format!("/badges/applications/{id}/ownership/verify"), json!({})).await;
    assert_eq!(status, Status::Ok);
    let (status, _) = as_user(&harness, &session, Post, &format!("/badges/applications/{id}/submit"), json!({})).await;
    assert_eq!(status, Status::Ok);
    let (status, body) = send(&harness, &boss, &boss_token, Post, &format!("/staff/badges/applications/{id}/decide"),
        json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Approved");
    let flags = harness.db.fetch_server(&server.id).await.unwrap().flags.unwrap_or_default();
    assert_ne!(flags & revolt_database::SERVER_FLAG_VERIFIED, 0);
    // the server gets its mark; the owner gets no badge (Verified Server
    // Owner was cancelled on 2026-11-24) and nothing tried to give one
    assert!(!held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));
    let records = audit(&harness, "badge.system.").await;
    assert!(!records.iter().any(|e| e.summary.as_deref().unwrap_or_default().contains("verified_server_owner")));

    // someone who held the badge before keeps it when the server changes hands
    revolt_database::grant_badge(&harness.db, &owner.id, "verified_server_owner", revolt_database::BadgeSource::System,
        Some("server".into()), revolt_database::BADGE_SYSTEM_ACTOR, Some("the record backs it".into()), None).await.unwrap();

    // handed over: the mark and the program end, the old owner keeps the badge
    let heir = genuine(&harness, 40, 9).await;
    join(&harness, &server.id, &heir.id, 30).await;
    let (status, body) = send(&harness, &boss, &boss_token, Post, &format!("/staff/servers/{}/actions", server.id),
        json!({ "action": "transfer", "reason": "Studio sold the community", "to": heir.id })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let flags = harness.db.fetch_server(&server.id).await.unwrap().flags.unwrap_or_default();
    assert_eq!(flags & revolt_database::SERVER_FLAG_VERIFIED, 0);
    let ended = harness.db.fetch_badge_application(&id).await.unwrap();
    assert_eq!(ended.status, ApplicationStatus::Ended);
    assert_eq!(ended.end_reason.as_deref(), Some("ownership_transferred"));
    assert!(held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));
    assert!(!held_badges(&harness.db, &heir.id).await.unwrap().contains("verified_server_owner"));

    // a holder keeps it when their verified server is deleted, too
    let (other, _) = harness.new_server(&owner).await;
    harness.db.update_server(&other.id, &revolt_database::PartialServer {
        flags: Some(revolt_database::SERVER_FLAG_VERIFIED), ..Default::default() }, vec![]).await.unwrap();
    let other = harness.db.fetch_server(&other.id).await.unwrap();
    other.delete(&harness.db).await.unwrap();
    assert!(held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));
    let records = audit(&harness, "badge.system.").await;
    assert!(!records.iter().any(|e| e.summary.as_deref().unwrap_or_default().contains("verified_server_owner")));
}

/// The official-entity exception (a staff Owner first, a second person
/// after): the server gets its Verified mark, its owner no badge.
#[rocket::async_test]
async fn the_official_entity_exception_gives_the_mark_and_no_owner_badge() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    // below the test numbers (4 members): only the exception can carry it
    let (server, _) = harness.new_server(&owner).await;
    let boss = member(&harness, vec![StaffRole::Owner]).await;
    let boss_token = sign_in(&harness, &boss, 0).await;
    step_up(&harness, &boss, &boss_token).await;
    let second = member(&harness, vec![StaffRole::Owner]).await;
    let second_token = sign_in(&harness, &second, 0).await;
    step_up(&harness, &second, &second_token).await;

    let (status, draft) = as_user(&harness, &session, Post, "/badges/applications", json!({
        "program": "verified_server",
        "target_id": server.id,
        "statement": "The official community of a public university library.",
        "links": ["https://library.example.net/zrzur"],
        "authorized": true,
        "exception": true,
    })).await;
    assert_eq!(status, Status::Ok, "{draft}");
    let id = draft["_id"].as_str().unwrap().to_string();
    let (_, issued) = as_user(&harness, &session, Post, &format!("/badges/applications/{id}/ownership"),
        json!({ "method": "website", "domain": "library.example.net" })).await;
    set_test_answer("web:library.example.net", vec![issued["value"].as_str().unwrap().to_string()]);
    let (status, _) = as_user(&harness, &session, Post, &format!("/badges/applications/{id}/ownership/verify"), json!({})).await;
    assert_eq!(status, Status::Ok);
    let (status, body) = as_user(&harness, &session, Post, &format!("/badges/applications/{id}/submit"), json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");

    let decide = format!("/staff/badges/applications/{id}/decide");
    let (status, body) = send(&harness, &boss, &boss_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_ne!(body["status"], "Approved", "an exception needs a second person");
    let (status, body) = send(&harness, &second, &second_token, Post, &decide, json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Approved");
    let flags = harness.db.fetch_server(&server.id).await.unwrap().flags.unwrap_or_default();
    assert_ne!(flags & revolt_database::SERVER_FLAG_VERIFIED, 0);
    assert!(!held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));
    let bits = harness.db.fetch_user(&owner.id).await.unwrap().get_badges().await;
    assert_eq!(bits & revolt_models::v0::UserBadges::VerifiedServerOwner as u32, 0);
}

#[rocket::async_test]
async fn a_bot_asks_at_any_size_and_its_badge_ends_with_it() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    let legacy = json!({
        "statement": "Moderation helper used in Arabic study communities.",
        "privacy_policy_url": "https://example.org/privacy",
        "terms_of_service_url": "https://example.org/terms",
    });

    // in no server yet: asking is open all the same, and the line waits for
    // a person instead of blocking (owner, 2026-11-27)
    let (_, body) = as_user(&harness, &session, Get, &format!("/badges/programs/verified_bot/eligibility?target={}", bot.id), json!({})).await;
    assert!(check(&body, "legit_servers")["met"].is_null(), "{body}");
    assert_eq!(body["can_apply"], true, "{body}");
    let (status, body) = as_user(&harness, &session, Post, &format!("/bots/{}/verification", bot.id), legacy.clone()).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["request"]["status"], "Pending");
    // the owner's own server never counts
    let (own, _) = harness.new_server(&owner).await;
    grow(&harness, &own.id, 5).await;
    join(&harness, &own.id, &bot.id, 30).await;
    let (_, body) = as_user(&harness, &session, Get, &format!("/badges/programs/verified_bot/eligibility?target={}", bot.id), json!({})).await;
    assert_eq!(check(&body, "legit_servers")["value"], 0, "{body}");
    // nor a server of a fresh account
    let fresh = harness.new_user().await.2;
    let (fresh_server, _) = harness.new_server(&fresh).await;
    grow(&harness, &fresh_server.id, 5).await;
    join(&harness, &fresh_server.id, &bot.id, 30).await;
    let (_, body) = as_user(&harness, &session, Get, &format!("/badges/programs/verified_bot/eligibility?target={}", bot.id), json!({})).await;
    assert_eq!(check(&body, "legit_servers")["value"], 0, "{body}");
    // a server of a genuine person with genuine members does
    let stranger = genuine(&harness, 60, 1).await;
    let (theirs, _) = harness.new_server(&stranger).await;
    grow(&harness, &theirs.id, 5).await;
    join(&harness, &theirs.id, &bot.id, 30).await;
    let (_, body) = as_user(&harness, &session, Get, &format!("/badges/programs/verified_bot/eligibility?target={}", bot.id), json!({})).await;
    assert_eq!(check(&body, "legit_servers")["met"], true, "{body}");
    assert_eq!(body["can_apply"], true);

    let (status, draft) = apply(&harness, &session, "verified_bot", &bot.id).await;
    assert_eq!(status, Status::Ok, "{draft}");
    let id = draft["_id"].as_str().unwrap().to_string();
    let (status, _) = as_user(&harness, &session, Post, &format!("/badges/applications/{id}/submit"), json!({})).await;
    assert_eq!(status, Status::Ok);
    let ts = member(&harness, vec![StaffRole::TrustSafety, StaffRole::SeniorSupport]).await;
    let token = sign_in(&harness, &ts, 0).await;
    step_up(&harness, &ts, &token).await;
    let (status, body) = send(&harness, &ts, &token, Post, &format!("/staff/badges/applications/{id}/decide"),
        json!({ "decision": "approve", "reasons": ["criteria_met"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(harness.db.fetch_bot(&bot.id).await.unwrap().is_verified());
    assert!(held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_bot_developer"));

    // deleted: the application ends and the badge goes, through the gate
    let bot = harness.db.fetch_bot(&bot.id).await.unwrap();
    bot.delete(&harness.db).await.unwrap();
    let ended = harness.db.fetch_badge_application(&id).await.unwrap();
    assert_eq!(ended.status, ApplicationStatus::Ended);
    assert_eq!(ended.end_reason.as_deref(), Some("bot_deleted"));
    assert!(!held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_bot_developer"));
    let records = audit(&harness, "badge.system.revoke").await;
    assert!(records.iter().any(|e| e.summary.as_deref().unwrap_or_default().contains("verified_bot_developer via system")));
}

#[rocket::async_test]
async fn weekly_activity_is_counted_from_ids_only() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let channel = channels[0].id().to_string();
    let now = staff_now();
    let week = week_of(now) - 1;
    let inside = |offset: i64| ulid::Ulid::from_parts((week_start(week) + offset) as u64, rand::random::<u128>() & 0xffff).to_string();
    let writer = genuine(&harness, 30, 0).await;
    let other = genuine(&harness, 30, 1).await;
    let reader = genuine(&harness, 30, 2).await;
    let (bot, _) = harness.new_bot(&owner).await;
    for (author, webhook) in [(&writer.id, false), (&writer.id, false), (&other.id, false), (&bot.id, false), (&other.id, true)] {
        harness
            .db
            .insert_message(&Message {
                id: inside(DAY),
                channel: channel.clone(),
                author: author.clone(),
                webhook: webhook.then(|| revolt_models::v0::MessageWebhook { name: "hook".into(), avatar: None }),
                content: Some("private words".into()),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    // a message of the week before does not count
    harness.db.insert_message(&Message { id: inside(-DAY), channel: channel.clone(), author: reader.id.clone(), ..Default::default() }).await.unwrap();
    // the reader read up to a message of the week
    let read = inside(2 * DAY);
    harness.db.acknowledge_message(&channel, &reader.id, &read).await.unwrap();
    // one of the week's joiners is still there
    join(&harness, &server.id, &reader.id, (now - week_start(week) - 3 * DAY) / HOUR).await;

    let row = measure_server_week(&harness.db, &server, week, 1, now).await.unwrap();
    assert_eq!((row.contributors, row.active), (2, 3), "{row:?}");
    assert_eq!(row.joins, 1);
    // counts only: nothing of the message or of who wrote it
    let stored = format!("{row:?}");
    assert!(!stored.contains("private words") && !stored.contains(&writer.id), "{stored}");
}

#[rocket::async_test]
async fn application_data_is_deleted_on_time_and_with_the_account() {
    let harness = TestHarness::new().await;
    let programs = revolt_config::config().await.api.badge_programs;
    let now = staff_now();
    let base = BadgeApplication {
        id: String::new(),
        program: BadgeProgram::Partner,
        applicant_id: "01HZZZZZZZZZZZZZZZZZZZZZZZ".into(),
        target_id: "01HZZZZZZZZZZZZZZZZZZZZZZY".into(),
        status: ApplicationStatus::Refused,
        exception: false,
        statement: "statement".into(),
        links: vec![],
        authorized: true,
        ownership: None,
        creator: None,
        creator_figures: None,
        eligibility: vec![],
        created_at: now - 400 * DAY,
        updated_at: now - 400 * DAY,
        submitted_at: None,
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
    let make = |id: &str, f: &dyn Fn(&mut BadgeApplication)| {
        let mut a = base.clone();
        a.id = id.to_string();
        f(&mut a);
        a
    };
    let evidence_due = make("A1", &|a| a.evidence_purge_at = Some(now - 1));
    let record_due = make("A2", &|a| a.purge_at = Some(now - 1));
    let old_draft = make("A3", &|a| a.status = ApplicationStatus::Draft);
    let kept = make("A4", &|a| {
        a.evidence_purge_at = Some(now + DAY);
        a.purge_at = Some(now + 200 * DAY);
    });
    for application in [&evidence_due, &record_due, &old_draft, &kept] {
        harness.db.insert_badge_application(application).await.unwrap();
        harness
            .db
            .insert_badge_evidence(&BadgeEvidence {
                id: format!("{}-e", application.id),
                application_id: application.id.clone(),
                applicant_id: application.applicant_id.clone(),
                kind: "note".into(),
                text: "evidence".into(),
                created_at: now,
            })
            .await
            .unwrap();
    }
    let report = program_upkeep(&harness.db, &programs, now).await.unwrap();
    assert_eq!((report.evidence, report.records), (1, 2), "{report:?}");
    let a1 = harness.db.fetch_badge_application("A1").await.unwrap();
    assert!(a1.evidence_purged && harness.db.fetch_badge_evidence("A1").await.unwrap().is_empty());
    assert!(harness.db.fetch_badge_application("A2").await.is_err());
    assert!(harness.db.fetch_badge_application("A3").await.is_err());
    assert_eq!(harness.db.fetch_badge_evidence("A4").await.unwrap().len(), 1);
    // twice: nothing more
    assert_eq!(program_upkeep(&harness.db, &programs, now).await.unwrap(), Default::default());
    // the account goes: so does everything it applied with
    assert_eq!(erase_applications_of(&harness.db, &base.applicant_id).await.unwrap(), 2);
    assert!(harness.db.fetch_badge_evidence("A4").await.unwrap().is_empty());
}
