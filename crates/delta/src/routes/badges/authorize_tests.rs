//! The one badge gate (`revolt_database::mutate_badge`) against the abuse
//! cases of the badge audit (F.1): the same rule matrix on every path.

use revolt_database::{
    appoint_staff, appointment_gaps, approve_badge_request, approve_loaded_badge_request,
    end_staff_appointment, end_staff_badge_with_grant, founder_bit, grant_badge, held_badges,
    mutate_badge, note_founder_config, refuse_badge_request, refuse_loaded_badge_request,
    staff_now, AuditOutcome, BadgeActor, BadgeChange, BadgeMutation, BadgeOutcome, BadgePath,
    BadgeRequestStatus, BadgeSource, PlatformAuditEntry, PlatformAuditQuery, StaffGrant,
    StaffRole, User, ADMIN_ROUTE_RETIRED, SECOND_PERSON_MIN_AGE_MS, STAFF_GRANT_END_ACTION,
};
use revolt_models::v0::UserBadges;
use revolt_result::ErrorType;
use rocket::http::{ContentType, Header, Status};

use crate::util::test::TestHarness;

/// The error's variant name (`ErrorType` has no `PartialEq`)
fn kind(error: ErrorType) -> String {
    format!("{error:?}").split([' ', '{', '(']).next().unwrap_or_default().to_string()
}

const PATHS: [BadgePath; 5] = [
    BadgePath::StaffConsole,
    BadgePath::AdminRoute,
    BadgePath::BadgeCommand,
    BadgePath::Desk,
    BadgePath::Cli,
];

/// Every right there is: an Owner with badge rights and a fresh step-up
fn strongest(user: &User, path: BadgePath) -> BadgeActor {
    BadgeActor {
        id: user.id.clone(),
        path,
        staff: true,
        owner: true,
        step_up: true,
        session_id: None,
    }
}

fn mutation(user: &User, badge: &str, change: BadgeChange) -> BadgeMutation {
    BadgeMutation {
        user_id: user.id.clone(),
        badge: badge.to_string(),
        change,
        reason: "secret-looking reason text".to_string(),
        expires_at: None,
        source_ref: None,
    }
}

async fn try_change(harness: &TestHarness, actor: &BadgeActor, user: &User, badge: &str, change: BadgeChange) -> Result<BadgeOutcome, String> {
    mutate_badge(&harness.db, actor, mutation(user, badge, change))
        .await
        .map_err(|e| kind(e.error_type))
}

async fn audit(harness: &TestHarness, target: &User) -> Vec<PlatformAuditEntry> {
    let mut entries: Vec<_> = harness
        .db
        .fetch_platform_audit(PlatformAuditQuery {
            limit: 1000,
            target_id: Some(target.id.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    entries.sort_by_key(|e| e.seq);
    entries
}

fn denied(entries: &[PlatformAuditEntry], code: &str) -> usize {
    entries
        .iter()
        .filter(|e| matches!(e.outcome, AuditOutcome::Denied) && e.summary.as_deref().unwrap_or_default().contains(&format!("denied:{code}")))
        .count()
}

async fn staff_grant(harness: &TestHarness, user: &User, roles: Vec<StaffRole>) {
    let grant = StaffGrant {
        id: user.id.clone(),
        roles,
        disabled: false,
        expires_at: None,
        last_totp_step: 0,
        version: 1,
        updated_at: 0,
        updated_by: "test".to_string(),
        // on the team for longer than a week, made staff by someone else
        active_since: Some(staff_now() - SECOND_PERSON_MIN_AGE_MS - 60_000),
        activated_by: Some("system:cli".to_string()),
    };
    assert!(harness.db.save_staff_grant(&grant, None).await.unwrap());
}

/// A staff grant given `age_ms` ago by `by`
async fn staff_grant_since(harness: &TestHarness, user: &User, roles: Vec<StaffRole>, age_ms: i64, by: &str) {
    let current = harness.db.fetch_staff_grant(&user.id).await.unwrap();
    let grant = StaffGrant {
        id: user.id.clone(),
        roles,
        disabled: false,
        expires_at: None,
        last_totp_step: 0,
        version: current.as_ref().map(|g| g.version + 1).unwrap_or(1),
        updated_at: 0,
        updated_by: by.to_string(),
        active_since: Some(staff_now() - age_ms),
        activated_by: Some(by.to_string()),
    };
    assert!(harness.db.save_staff_grant(&grant, current.map(|g| g.version)).await.unwrap());
}

async fn legacy_grant(harness: &TestHarness, user: &User, badge: &str) {
    grant_badge(&harness.db, &user.id, badge, BadgeSource::Admin, None, "someone", Some("old grant".into()), None)
        .await
        .unwrap();
}

#[rocket::async_test]
async fn founder_is_locked_on_every_path() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    let (_, _, holder) = harness.new_user().await;
    // a founder grant from before the lock: no path takes it away either
    legacy_grant(&harness, &holder, "founder").await;
    for path in PATHS {
        let actor = strongest(&actor, path);
        let given = try_change(&harness, &actor, &target, "founder", BadgeChange::Grant).await;
        assert!(given.is_err(), "{path:?} gave founder");
        let taken = try_change(&harness, &actor, &holder, "founder", BadgeChange::Revoke).await;
        assert!(taken.is_err(), "{path:?} took founder");
    }
    assert!(!held_badges(&harness.db, &target.id).await.unwrap().contains("founder"));
    assert!(held_badges(&harness.db, &holder.id).await.unwrap().contains("founder"));
    // every refusal is written down: four paths say "locked", the admin route "route_retired"
    let records = audit(&harness, &target).await;
    assert_eq!(denied(&records, "locked"), 4);
    assert_eq!(denied(&records, "route_retired"), 1);
    let records = audit(&harness, &holder).await;
    assert_eq!(denied(&records, "locked"), 4);
    assert_eq!(denied(&records, "route_retired"), 1);
}

#[test]
fn founder_follows_the_setting_alone() {
    let founder = UserBadges::Founder as u32;
    // unset: the stored bit stays as it is
    assert_eq!(founder_bit(founder, None, "A"), founder);
    assert_eq!(founder_bit(0, None, "A"), 0);
    // set: only that account, whatever is stored
    assert_eq!(founder_bit(0, Some("A"), "A"), founder);
    assert_eq!(founder_bit(founder | 1, Some("A"), "B"), 1);
}

#[rocket::async_test]
async fn a_changed_founder_setting_is_written_to_the_audit_log_once() {
    let harness = TestHarness::new().await;
    let count = |entries: Vec<PlatformAuditEntry>| entries.into_iter().filter(|e| e.action == "config.founder_id").count();
    let read = || {
        harness.db.fetch_platform_audit(PlatformAuditQuery {
            limit: 100,
            action_prefix: Some("config.founder_id".to_string()),
            ..Default::default()
        })
    };
    // never set: nothing to say
    assert!(!note_founder_config(&harness.db, None).await.unwrap());
    assert!(note_founder_config(&harness.db, Some("01FOUNDER")).await.unwrap());
    // the same value at the next start: nothing new
    assert!(!note_founder_config(&harness.db, Some("01FOUNDER")).await.unwrap());
    assert!(note_founder_config(&harness.db, Some("01OTHER")).await.unwrap());
    assert!(note_founder_config(&harness.db, None).await.unwrap());
    assert_eq!(count(read().await.unwrap()), 3);
    let last = read().await.unwrap().into_iter().next().unwrap();
    assert_eq!(last.summary.as_deref(), Some("founder_id changed"));
}

#[rocket::async_test]
async fn staff_and_platform_moderator_refuse_every_badge_permission() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = harness.new_user().await;
    harness.make_privileged(&actor.id).await;
    let (_, _, target) = harness.new_user().await;
    for path in PATHS {
        for badge in ["staff", "platform_moderator"] {
            for change in [BadgeChange::Grant, BadgeChange::Revoke] {
                let result = try_change(&harness, &strongest(&actor, path), &target, badge, change).await;
                assert!(result.is_err(), "{path:?} {badge} {change:?}");
            }
        }
    }
    assert_eq!(harness.db.fetch_user(&target.id).await.unwrap().badges.unwrap_or_default(), 0);
    let records = audit(&harness, &target).await;
    assert_eq!(denied(&records, "appointment_only"), 16);
    assert_eq!(denied(&records, "route_retired"), 4);
}

#[rocket::async_test]
async fn staff_badge_comes_from_an_appointment_by_two_people() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, _, second) = harness.new_user().await;
    let (_, _, outsider) = harness.new_user().await;
    let (_, _, person) = harness.new_user().await;
    staff_grant(&harness, &owner, vec![StaffRole::Owner]).await;
    staff_grant(&harness, &second, vec![StaffRole::Support]).await;
    let appoint = |first: &User, other: &User, role: &str, training: bool| {
        let (first, other, role) = (first.id.clone(), other.id.clone(), role.to_string());
        let db = harness.db.clone();
        let person = person.id.clone();
        async move { appoint_staff(&db, &first, &other, &person, &role, "joined the team", training).await }
    };

    // one person alone, or a second person without a staff grant: refused
    assert!(appoint(&owner, &owner, "staff", false).await.is_err());
    assert!(appoint(&owner, &outsider, "staff", false).await.is_err());
    // the first person must be an Owner
    assert!(appoint(&second, &owner, "staff", false).await.is_err());
    // never yourself
    let own = appoint_staff(&harness.db, &owner.id, &second.id, &owner.id, "staff", "myself", false).await;
    assert!(own.is_err());
    // platform moderators acknowledge the training first
    assert!(appoint(&owner, &second, "platform_moderator", false).await.is_err());
    assert!(!held_badges(&harness.db, &person.id).await.unwrap().contains("staff"));

    let appointment = appoint(&owner, &second, "staff", false).await.unwrap();
    assert_eq!(appointment.appointed_by, vec![owner.id.clone(), second.id.clone()]);
    assert!(held_badges(&harness.db, &person.id).await.unwrap().contains("staff"));
    assert!(appointment_gaps(&harness.db).await.unwrap().is_empty());
    appoint(&owner, &second, "platform_moderator", true).await.unwrap();
    assert!(held_badges(&harness.db, &person.id).await.unwrap().contains("platform_moderator"));

    // the end takes the badge away (two people again)
    assert!(end_staff_appointment(&harness.db, &owner.id, &owner.id, &person.id, "staff", "left").await.is_err());
    end_staff_appointment(&harness.db, &owner.id, &second.id, &person.id, "staff", "left the team").await.unwrap();
    assert!(!held_badges(&harness.db, &person.id).await.unwrap().contains("staff"));
    let record = harness.db.fetch_staff_appointment(&appointment.id).await.unwrap().unwrap();
    assert!(record.ended_at.is_some());

    let records = audit(&harness, &person).await;
    let actions: Vec<_> = records.iter().map(|e| (e.action.as_str(), format!("{:?}", e.outcome))).collect();
    assert!(actions.contains(&("staff.cli.appointment.appoint", "Success".to_string())), "{actions:?}");
    assert!(actions.contains(&("staff.cli.appointment.appoint", "Denied".to_string())), "{actions:?}");
    assert!(actions.contains(&("staff.cli.appointment.end", "Success".to_string())), "{actions:?}");
}

#[rocket::async_test]
async fn staff_holders_from_before_appointments_keep_the_badge_and_are_listed() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, _, second) = harness.new_user().await;
    let (_, _, old) = harness.new_user().await;
    staff_grant(&harness, &owner, vec![StaffRole::Owner]).await;
    staff_grant(&harness, &second, vec![StaffRole::Auditor]).await;
    legacy_grant(&harness, &old, "staff").await;
    // kept and shown
    assert!(held_badges(&harness.db, &old.id).await.unwrap().contains("staff"));
    let bits = harness.db.fetch_user(&old.id).await.unwrap().badges.unwrap_or_default() as u32;
    assert_ne!(bits & UserBadges::Staff as u32, 0);
    // listed until the owner confirms them
    let gaps = appointment_gaps(&harness.db).await.unwrap();
    assert!(gaps.contains(&("staff".to_string(), old.id.clone())));
    // confirming keeps the grant and closes the gap
    appoint_staff(&harness.db, &owner.id, &second.id, &old.id, "staff", "confirmed by the owner", false).await.unwrap();
    assert!(held_badges(&harness.db, &old.id).await.unwrap().contains("staff"));
    assert!(!appointment_gaps(&harness.db).await.unwrap().iter().any(|(_, user)| user == &old.id));
}

#[rocket::async_test]
async fn retired_badges_are_never_given_again() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    for path in PATHS {
        for badge in [
            "translator",
            "responsible_disclosure",
            "supporter",
            "paw",
            "reserved_1",
            "reserved_2",
            "saha_champion",
        ] {
            let result = try_change(&harness, &strongest(&actor, path), &target, badge, BadgeChange::Grant).await;
            assert!(result.is_err(), "{path:?} {badge}");
        }
    }
    assert!(held_badges(&harness.db, &target.id).await.unwrap().is_empty());
    let records = audit(&harness, &target).await;
    assert_eq!(denied(&records, "retired"), 28);
    assert_eq!(denied(&records, "route_retired"), 7);
}

#[rocket::async_test]
async fn saha_champion_is_historical_a_holder_keeps_it_and_history_stays() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = harness.new_user().await;
    let (_, _, second) = harness.new_user().await;
    let (_, _, holder) = harness.new_user().await;
    let definition = revolt_database::badge_definition("saha_champion").unwrap();
    assert_eq!(definition.rule, revolt_database::BadgeGrantRule::Historical);
    assert!(definition.retired);
    assert_eq!(definition.bit, UserBadges::SahaChampion as u32);
    let paw = revolt_database::badge_definition("paw").unwrap();
    assert_eq!(paw.rule, revolt_database::BadgeGrantRule::Historical);

    // someone who held it before keeps it, and it still shows
    grant_badge(&harness.db, &holder.id, "saha_champion", BadgeSource::Admin, None, &actor.id, Some("tournament".into()), None)
        .await
        .unwrap();
    let bits = harness.db.fetch_user(&holder.id).await.unwrap().get_badges().await;
    assert_ne!(bits & UserBadges::SahaChampion as u32, 0);

    // taking it away is an Owner change with a step-up and a second person
    let staff_only = BadgeActor { owner: false, ..strongest(&actor, BadgePath::StaffConsole) };
    assert_eq!(
        try_change(&harness, &staff_only, &holder, "saha_champion", BadgeChange::Revoke).await.map(|_| ()),
        Err("NotPrivileged".to_string())
    );
    let pending = try_change(&harness, &strongest(&actor, BadgePath::StaffConsole), &holder, "saha_champion", BadgeChange::Revoke)
        .await
        .unwrap();
    let BadgeOutcome::Pending(request) = pending else {
        panic!("a second person must approve");
    };
    approve_badge_request(&harness.db, &strongest(&second, BadgePath::StaffConsole), &request.id).await.unwrap();
    assert!(!held_badges(&harness.db, &holder.id).await.unwrap().contains("saha_champion"));
    // the grant record and the badge log stay as history
    let grants = harness.db.fetch_badge_grants(&holder.id).await.unwrap();
    assert!(grants.iter().any(|g| g.badge == "saha_champion" && g.revoked_at.is_some()));
    let log = harness.db.fetch_badge_logs(&holder.id).await.unwrap();
    assert_eq!(log.iter().filter(|e| e.badge == "saha_champion").count(), 2);
}

#[rocket::async_test]
async fn nobody_changes_their_own_badges_on_any_path() {
    let harness = TestHarness::new().await;
    let (_, _, me) = harness.new_user().await;
    for path in PATHS {
        for badge in ["community_helper", "developer"] {
            let result = try_change(&harness, &strongest(&me, path), &me, badge, BadgeChange::Grant).await;
            assert!(result.is_err(), "{path:?} {badge}");
        }
    }
    assert!(held_badges(&harness.db, &me.id).await.unwrap().is_empty());
    let records = audit(&harness, &me).await;
    assert_eq!(denied(&records, "self"), 8);
}

#[rocket::async_test]
async fn the_same_rules_on_every_path_that_may_change_badges() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = harness.new_user().await;
    for path in [BadgePath::StaffConsole, BadgePath::BadgeCommand, BadgePath::Cli, BadgePath::Desk] {
        let (_, _, target) = harness.new_user().await;
        // no permission
        let none = BadgeActor { staff: false, owner: false, step_up: false, ..strongest(&actor, path) };
        let result = try_change(&harness, &none, &target, "community_helper", BadgeChange::Grant).await;
        assert!(result.as_ref().map(|_| ()) == Err(&"MissingPlatformPermission".to_string()), "{path:?} {result:?}");
        // badge rights, but a privileged badge needs an Owner
        let staff = BadgeActor { staff: true, owner: false, step_up: true, ..strongest(&actor, path) };
        assert_eq!(try_change(&harness, &staff, &target, "developer", BadgeChange::Grant).await.map(|_| ()), Err("NotPrivileged".to_string()));
        // an Owner without a fresh step-up
        let owner = BadgeActor { step_up: false, ..strongest(&actor, path) };
        assert_eq!(try_change(&harness, &owner, &target, "developer", BadgeChange::Grant).await.map(|_| ()), Err("NotElevated".to_string()));
        // the record must back it
        let result = try_change(&harness, &staff, &target, "super_creator", BadgeChange::Grant).await;
        assert!(result.as_ref().map(|_| ()) == Err(&"FailedValidation".to_string()), "{path:?} {result:?}");
        // allowed
        assert!(matches!(try_change(&harness, &staff, &target, "community_helper", BadgeChange::Grant).await, Ok(BadgeOutcome::Applied(_))));
        assert!(matches!(try_change(&harness, &strongest(&actor, path), &target, "developer", BadgeChange::Grant).await, Ok(BadgeOutcome::Applied(_))));

        let records = audit(&harness, &target).await;
        for code in ["no_permission", "owner_only", "step_up_required", "ineligible"] {
            assert_eq!(denied(&records, code), 1, "{path:?} {code}");
        }
        // never the reason text in the summary, only ids and codes
        assert!(records.iter().all(|e| !e.summary.as_deref().unwrap_or_default().contains("secret-looking")));
        assert!(records.iter().filter(|e| matches!(e.outcome, AuditOutcome::Success)).count() >= 2);
    }
}

#[rocket::async_test]
async fn sensitive_changes_need_a_second_person() {
    let harness = TestHarness::new().await;
    let (_, _, asker) = harness.new_user().await;
    let (_, _, second) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    let asker_rights = strongest(&asker, BadgePath::StaffConsole);
    let second_rights = strongest(&second, BadgePath::StaffConsole);

    // giving Partner waits
    let request = match try_change(&harness, &asker_rights, &target, "partner", BadgeChange::Grant).await {
        Ok(BadgeOutcome::Pending(request)) => request,
        other => panic!("{other:?}"),
    };
    assert!(!held_badges(&harness.db, &target.id).await.unwrap().contains("partner"));
    // asking again shows the same request
    match try_change(&harness, &asker_rights, &target, "partner", BadgeChange::Grant).await {
        Ok(BadgeOutcome::Pending(again)) => assert_eq!(again.id, request.id),
        other => panic!("{other:?}"),
    }
    // the asker can't approve it, nor the person concerned
    let own = approve_badge_request(&harness.db, &asker_rights, &request.id).await;
    assert_eq!(own.map(|_| ()).map_err(|e| kind(e.error_type)), Err("NotPrivileged".to_string()));
    let theirs = approve_badge_request(&harness.db, &strongest(&target, BadgePath::StaffConsole), &request.id).await;
    assert_eq!(theirs.map(|_| ()).map_err(|e| kind(e.error_type)), Err("NotPrivileged".to_string()));
    // nor someone without the right
    let weak = BadgeActor { staff: false, owner: false, ..second_rights.clone() };
    assert!(approve_badge_request(&harness.db, &weak, &request.id).await.is_err());
    assert_eq!(harness.db.fetch_badge_request(&request.id).await.unwrap().status, BadgeRequestStatus::Pending);
    // a second person with the right
    approve_badge_request(&harness.db, &second_rights, &request.id).await.unwrap();
    assert!(held_badges(&harness.db, &target.id).await.unwrap().contains("partner"));
    let decided = harness.db.fetch_badge_request(&request.id).await.unwrap();
    assert_eq!(decided.status, BadgeRequestStatus::Approved);
    assert_eq!(decided.decided_by.as_deref(), Some(second.id.as_str()));
    // only once
    assert!(approve_badge_request(&harness.db, &second_rights, &request.id).await.is_err());

    // taking away a privileged badge waits too; it can be refused
    legacy_grant(&harness, &target, "developer").await;
    let request = match try_change(&harness, &asker_rights, &target, "developer", BadgeChange::Revoke).await {
        Ok(BadgeOutcome::Pending(request)) => request,
        other => panic!("{other:?}"),
    };
    assert!(refuse_badge_request(&harness.db, &strongest(&target, BadgePath::StaffConsole), &request.id).await.is_err());
    refuse_badge_request(&harness.db, &second_rights, &request.id).await.unwrap();
    assert!(approve_badge_request(&harness.db, &second_rights, &request.id).await.is_err());
    assert!(held_badges(&harness.db, &target.id).await.unwrap().contains("developer"));

    // Elite Bug Hunter (after its record checks) waits as well
    let records = audit(&harness, &target).await;
    assert!(denied(&records, "same_person") >= 2);
    assert!(denied(&records, "not_pending") >= 2);
    assert!(records.iter().any(|e| e.action == "staff.badge.grant.request"));
    assert!(records.iter().any(|e| e.action == "staff.badge.revoke.request"));
    assert!(records
        .iter()
        .any(|e| e.action == "staff.badge.grant" && e.summary.as_deref().unwrap_or_default().contains(&format!("approves {}", decided.id))));
}

#[rocket::async_test]
async fn the_admin_routes_are_retired_with_a_clear_answer() {
    let harness = TestHarness::new().await;
    let (_, session, admin) = harness.new_user().await;
    harness.make_privileged(&admin.id).await;
    let (_, _, target) = harness.new_user().await;
    for (path, badge) in [("/badges/admin/grant", "community_helper"), ("/badges/admin/grant", "founder"), ("/badges/admin/revoke", "staff")] {
        let res = harness
            .client
            .post(path)
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(json!({ "user": target.id, "badge": badge, "reason": "old tool" }).to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::BadRequest, "{path} {badge}");
        let body: serde_json::Value = res.into_json().await.unwrap();
        assert_eq!(body["error"], ADMIN_ROUTE_RETIRED, "{body}");
    }
    assert!(held_badges(&harness.db, &target.id).await.unwrap().is_empty());
    let records = audit(&harness, &target).await;
    assert_eq!(denied(&records, "route_retired"), 3);
    assert!(records.iter().filter(|e| e.action.starts_with("badge.admin.")).all(|e| e.actor_id == admin.id));
}

// ---- security review of 2026-11-24 (F2-F7, F12) ----

/// F2: the retired routes write nothing for a plain account (the audit log
/// can't be filled by anyone signed in); a staff caller's try is written
/// without the typed reason, and with the target only if it is a real id.
#[rocket::async_test]
async fn the_retired_routes_write_only_staff_tries_without_their_text() {
    let harness = TestHarness::new().await;
    let (_, plain_session, plain) = harness.new_user().await;
    let (_, staff_session, staff) = harness.new_user().await;
    harness.make_privileged(&staff.id).await;
    let (_, _, target) = harness.new_user().await;
    let long = "x".repeat(500);
    let send = |token: String, user: String| {
        let harness = &harness;
        let long = long.clone();
        async move {
            let res = harness
                .client
                .post("/badges/admin/grant")
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", token))
                .body(json!({ "user": user, "badge": "community_helper", "reason": long }).to_string())
                .dispatch()
                .await;
            let status = res.status();
            let body: serde_json::Value = res.into_json().await.unwrap();
            (status, body)
        }
    };
    let by = |actor: &User| {
        let db = harness.db.clone();
        let actor = actor.id.clone();
        async move {
            db.fetch_platform_audit(PlatformAuditQuery { limit: 1000, actor_id: Some(actor), ..Default::default() })
                .await
                .unwrap()
                .into_iter()
                .filter(|e| e.action.starts_with("badge.admin."))
                .collect::<Vec<_>>()
        }
    };

    // a plain account: the same answer, nothing written
    for user in [target.id.clone(), "not-an-id-but-a-long-text".to_string()] {
        let (status, body) = send(plain_session.token.clone(), user).await;
        assert_eq!(status, Status::BadRequest);
        assert_eq!(body["error"], ADMIN_ROUTE_RETIRED, "{body}");
    }
    assert!(by(&plain).await.is_empty());

    // a staff caller: written, never the reason, a target only if it is an id
    let (status, _) = send(staff_session.token.clone(), target.id.clone()).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(staff_session.token.clone(), "not-an-id-but-a-long-text".to_string()).await;
    assert_eq!(status, Status::BadRequest);
    let records = by(&staff).await;
    assert_eq!(records.len(), 2, "{records:?}");
    assert!(records.iter().all(|e| matches!(e.outcome, AuditOutcome::Denied) && e.reason.is_none()));
    assert!(records.iter().any(|e| e.target_id.as_deref() == Some(target.id.as_str())));
    assert!(records.iter().any(|e| e.target_id.is_none()));
    assert!(held_badges(&harness.db, &target.id).await.unwrap().is_empty());
}

/// F3: a reason of the wrong length never skips the Denied record, and its
/// text is never written
#[rocket::async_test]
async fn a_bad_reason_is_refused_after_the_rights_and_written_without_its_text() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    let rights = strongest(&actor, BadgePath::StaffConsole);
    for reason in ["no", &"y".repeat(501)] {
        // refused by the rules: Denied with that code
        let request = BadgeMutation { reason: reason.to_string(), ..mutation(&target, "founder", BadgeChange::Grant) };
        assert!(mutate_badge(&harness.db, &rights, request).await.is_err());
        // allowed by the rules: Denied bad_reason
        let request = BadgeMutation { reason: reason.to_string(), ..mutation(&target, "community_helper", BadgeChange::Grant) };
        let result = mutate_badge(&harness.db, &rights, request).await.map(|_| ()).map_err(|e| kind(e.error_type));
        assert_eq!(result, Err("FailedValidation".to_string()));
    }
    let records = audit(&harness, &target).await;
    assert_eq!(denied(&records, "locked"), 2);
    assert_eq!(denied(&records, "bad_reason"), 2);
    assert!(records.iter().all(|e| e.reason.is_none()), "{records:?}");
    assert!(held_badges(&harness.db, &target.id).await.unwrap().is_empty());
}

/// F4: someone without the right never marks a request approved, so a
/// real approver at the same moment is never turned away
#[rocket::async_test]
async fn an_approver_without_rights_never_holds_the_request() {
    let harness = TestHarness::new().await;
    let (_, _, asker) = harness.new_user().await;
    let (_, _, weak) = harness.new_user().await;
    let (_, _, real) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    let request = match try_change(&harness, &strongest(&asker, BadgePath::StaffConsole), &target, "partner", BadgeChange::Grant).await {
        Ok(BadgeOutcome::Pending(request)) => request,
        other => panic!("{other:?}"),
    };
    let weak_rights = BadgeActor { staff: false, owner: false, ..strongest(&weak, BadgePath::StaffConsole) };
    let real_rights = strongest(&real, BadgePath::StaffConsole);
    let (weak_result, real_result) = rocket::tokio::join!(
        approve_badge_request(&harness.db, &weak_rights, &request.id),
        approve_badge_request(&harness.db, &real_rights, &request.id),
    );
    assert!(weak_result.is_err());
    real_result.unwrap();
    let decided = harness.db.fetch_badge_request(&request.id).await.unwrap();
    assert_eq!(decided.status, BadgeRequestStatus::Approved);
    assert_eq!(decided.decided_by.as_deref(), Some(real.id.as_str()));
    assert!(held_badges(&harness.db, &target.id).await.unwrap().contains("partner"));
    // the weak try was refused at the approval itself, before any mark
    let records = audit(&harness, &target).await;
    assert!(records.iter().any(|e| e.action == "staff.badge.request.approve"
        && e.actor_id == weak.id
        && e.summary.as_deref().unwrap_or_default().contains("denied:no_permission")));
    assert!(!records.iter().any(|e| e.actor_id == real.id && matches!(e.outcome, AuditOutcome::Denied)));

    // a stale copy (decided meanwhile) is refused without a change
    let stale = approve_loaded_badge_request(&harness.db, &real_rights, request.clone()).await;
    assert!(stale.is_err());
}

/// F5: a refusal is written as Success only once it really happened
#[rocket::async_test]
async fn a_refusal_is_written_after_the_decision() {
    let harness = TestHarness::new().await;
    let (_, _, asker) = harness.new_user().await;
    let (_, _, second) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    let request = match try_change(&harness, &strongest(&asker, BadgePath::StaffConsole), &target, "partner", BadgeChange::Grant).await {
        Ok(BadgeOutcome::Pending(request)) => request,
        other => panic!("{other:?}"),
    };
    // approved by someone else in the meantime; the refusal still holds the old copy
    approve_badge_request(&harness.db, &strongest(&second, BadgePath::StaffConsole), &request.id).await.unwrap();
    let refused = refuse_loaded_badge_request(&harness.db, &strongest(&asker, BadgePath::StaffConsole), request.clone()).await;
    assert!(refused.is_err());
    assert_eq!(harness.db.fetch_badge_request(&request.id).await.unwrap().status, BadgeRequestStatus::Approved);
    let records = audit(&harness, &target).await;
    let refusals: Vec<_> = records.iter().filter(|e| e.action == "staff.badge.request.refuse").collect();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(matches!(refusals[0].outcome, AuditOutcome::Failed));

    // a real refusal: one Success record, after the change
    let request = match try_change(&harness, &strongest(&asker, BadgePath::StaffConsole), &target, "elite_bug_hunter", BadgeChange::Grant).await {
        Ok(BadgeOutcome::Pending(request)) => Some(request),
        _ => None,
    };
    if let Some(request) = request {
        refuse_badge_request(&harness.db, &strongest(&second, BadgePath::StaffConsole), &request.id).await.unwrap();
        let records = audit(&harness, &target).await;
        assert!(records
            .iter()
            .any(|e| e.action == "staff.badge.request.refuse" && matches!(e.outcome, AuditOutcome::Success)));
    }
}

/// F7: every refused appointment change is written down with its code
#[rocket::async_test]
async fn refused_appointment_changes_are_written_down() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, _, second) = harness.new_user().await;
    let (_, _, person) = harness.new_user().await;
    staff_grant(&harness, &owner, vec![StaffRole::Owner]).await;
    staff_grant(&harness, &second, vec![StaffRole::Support]).await;
    let unknown = ulid::Ulid::new().to_string();

    assert!(appoint_staff(&harness.db, &owner.id, &second.id, &unknown, "staff", "joined", false).await.is_err());
    appoint_staff(&harness.db, &owner.id, &second.id, &person.id, "staff", "joined", false).await.unwrap();
    assert!(appoint_staff(&harness.db, &owner.id, &second.id, &person.id, "staff", "again", false).await.is_err());
    assert!(end_staff_appointment(&harness.db, &owner.id, &second.id, &person.id, "platform_moderator", "never was").await.is_err());

    let codes = |entries: Vec<PlatformAuditEntry>| {
        entries
            .into_iter()
            .filter(|e| matches!(e.outcome, AuditOutcome::Denied) && e.action.starts_with("staff.cli.appointment."))
            .map(|e| e.summary.unwrap_or_default())
            .collect::<Vec<_>>()
    };
    let theirs = codes(audit(&harness, &person).await);
    assert!(theirs.iter().any(|s| s.contains("staff denied:already")), "{theirs:?}");
    assert!(theirs.iter().any(|s| s.contains("platform_moderator denied:not_held")), "{theirs:?}");
    let unknown_records = harness
        .db
        .fetch_platform_audit(PlatformAuditQuery { limit: 100, target_id: Some(unknown.clone()), ..Default::default() })
        .await
        .unwrap();
    assert!(codes(unknown_records).iter().any(|s| s.contains("staff denied:unknown_user")));
}

/// F12: the second person has been staff for more than 7 days, and was not
/// made staff by the first person
#[rocket::async_test]
async fn the_second_person_is_not_new_and_not_made_by_the_first() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, _, fresh) = harness.new_user().await;
    let (_, _, made) = harness.new_user().await;
    let (_, _, unknown_age) = harness.new_user().await;
    let (_, _, good) = harness.new_user().await;
    let (_, _, person) = harness.new_user().await;
    staff_grant(&harness, &owner, vec![StaffRole::Owner]).await;
    let week = SECOND_PERSON_MIN_AGE_MS;
    staff_grant_since(&harness, &fresh, vec![StaffRole::Support], week - 60_000, "system:cli").await;
    staff_grant_since(&harness, &made, vec![StaffRole::Support], week * 2, &owner.id).await;
    // a grant from before these dates were kept
    let grant = StaffGrant {
        id: unknown_age.id.clone(),
        roles: vec![StaffRole::Support],
        disabled: false,
        expires_at: None,
        last_totp_step: 0,
        version: 1,
        updated_at: 0,
        updated_by: "test".to_string(),
        active_since: None,
        activated_by: None,
    };
    assert!(harness.db.save_staff_grant(&grant, None).await.unwrap());
    staff_grant_since(&harness, &good, vec![StaffRole::Analyst], week + 60_000, "system:cli").await;

    for second in [&fresh, &made, &unknown_age] {
        let result = appoint_staff(&harness.db, &owner.id, &second.id, &person.id, "staff", "joined", false).await;
        assert_eq!(result.map(|_| ()).map_err(|e| kind(e.error_type)), Err("NotPrivileged".to_string()));
    }
    assert!(!held_badges(&harness.db, &person.id).await.unwrap().contains("staff"));
    appoint_staff(&harness.db, &owner.id, &good.id, &person.id, "staff", "joined", false).await.unwrap();

    let records = audit(&harness, &person).await;
    let summaries: Vec<_> = records.iter().filter_map(|e| e.summary.clone()).collect();
    assert_eq!(summaries.iter().filter(|s| s.contains("denied:second_too_new")).count(), 2, "{summaries:?}");
    assert_eq!(summaries.iter().filter(|s| s.contains("denied:second_made_by_first")).count(), 1, "{summaries:?}");
}

/// F12: a grant keeps the time its access started; it starts again only
/// after access ended
#[test]
fn a_grant_keeps_when_its_access_started() {
    let base = StaffGrant {
        id: "A".into(),
        roles: vec![StaffRole::Support],
        disabled: false,
        expires_at: None,
        last_totp_step: 0,
        version: 1,
        updated_at: 0,
        updated_by: "x".into(),
        active_since: None,
        activated_by: None,
    };
    let mut first = base.clone();
    first.carry_activation(None, 100, "owner");
    assert_eq!((first.active_since, first.activated_by.as_deref()), (Some(100), Some("owner")));
    // a role change keeps it
    let mut changed = StaffGrant { roles: vec![StaffRole::SeniorSupport], ..base.clone() };
    changed.carry_activation(Some(&first), 500, "other");
    assert_eq!((changed.active_since, changed.activated_by.as_deref()), (Some(100), Some("owner")));
    // no roles: cleared; access again: starts again
    let mut ended = StaffGrant { roles: vec![], ..base.clone() };
    ended.carry_activation(Some(&changed), 600, "other");
    assert_eq!(ended.active_since, None);
    let mut again = base.clone();
    again.carry_activation(Some(&ended), 700, "other");
    assert_eq!((again.active_since, again.activated_by.as_deref()), (Some(700), Some("other")));
    // switched off before: starts again
    let off = StaffGrant { disabled: true, ..first.clone() };
    let mut back = base.clone();
    back.carry_activation(Some(&off), 800, "third");
    assert_eq!(back.active_since, Some(800));
}

/// F12: a removed staff grant takes the Staff badge with it, through the
/// end of the appointment
#[rocket::async_test]
async fn a_removed_staff_grant_ends_the_staff_badge() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (_, _, second) = harness.new_user().await;
    let (_, _, person) = harness.new_user().await;
    let (_, _, legacy) = harness.new_user().await;
    let (_, _, nobody) = harness.new_user().await;
    staff_grant(&harness, &owner, vec![StaffRole::Owner]).await;
    staff_grant(&harness, &second, vec![StaffRole::Support]).await;
    let appointment = appoint_staff(&harness.db, &owner.id, &second.id, &person.id, "staff", "joined", false).await.unwrap();
    legacy_grant(&harness, &legacy, "staff").await;

    assert!(end_staff_badge_with_grant(&harness.db, &owner.id, &person.id, "left the team").await.unwrap());
    assert!(end_staff_badge_with_grant(&harness.db, &owner.id, &legacy.id, "left the team").await.unwrap());
    assert!(!end_staff_badge_with_grant(&harness.db, &owner.id, &nobody.id, "never staff").await.unwrap());
    assert!(!held_badges(&harness.db, &person.id).await.unwrap().contains("staff"));
    assert!(!held_badges(&harness.db, &legacy.id).await.unwrap().contains("staff"));
    let record = harness.db.fetch_staff_appointment(&appointment.id).await.unwrap().unwrap();
    assert!(record.ended_at.is_some());
    assert!(audit(&harness, &person).await.iter().any(|e| e.action == STAFF_GRANT_END_ACTION));
    assert!(audit(&harness, &nobody).await.is_empty());
}

/// F12: the open request and the appointment gaps come from queries, not
/// from the newest 200 records
#[rocket::async_test]
async fn open_requests_and_gaps_are_found_beyond_the_newest_200() {
    let harness = TestHarness::new().await;
    let (_, _, asker) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    let rights = strongest(&asker, BadgePath::StaffConsole);
    let first = match try_change(&harness, &rights, &target, "partner", BadgeChange::Grant).await {
        Ok(BadgeOutcome::Pending(request)) => request,
        other => panic!("{other:?}"),
    };
    // 205 newer requests of other people
    for _ in 0..205 {
        let mut other = first.clone();
        other.id = ulid::Ulid::new().to_string();
        other.user_id = ulid::Ulid::new().to_string();
        harness.db.insert_badge_request(&other).await.unwrap();
    }
    match try_change(&harness, &rights, &target, "partner", BadgeChange::Grant).await {
        Ok(BadgeOutcome::Pending(again)) => assert_eq!(again.id, first.id),
        other => panic!("{other:?}"),
    }
    // 205 legacy Staff holders: every one is listed
    let mut holders = vec![];
    for _ in 0..205 {
        let id = ulid::Ulid::new().to_string();
        let grant = revolt_database::BadgeGrant {
            id: format!("{id}:staff"),
            user_id: id.clone(),
            badge: "staff".to_string(),
            source: BadgeSource::Legacy,
            source_ref: None,
            granted_by: "legacy".to_string(),
            granted_at: staff_now(),
            reason: None,
            expires_at: None,
            revoked_at: None,
            revoked_by: None,
            revoke_reason: None,
            hidden: false,
        };
        assert!(harness.db.insert_badge_grant(&grant).await.unwrap());
        holders.push(id);
    }
    let gaps = appointment_gaps(&harness.db).await.unwrap();
    assert!(holders.iter().all(|id| gaps.contains(&("staff".to_string(), id.clone()))), "{} gaps", gaps.len());
}

/// F10: the public badge list says what the badge bits say about Founder:
/// once `founder_id` is set only that account has it, even without a
/// grant row, and it can't be hidden
#[test]
fn the_founder_list_follows_the_setting_like_the_bits() {
    use super::public_badges;
    let founder = ulid::Ulid::new().to_string();
    let other = ulid::Ulid::new().to_string();
    let row = |user: &str, badge: &str, hidden: bool| revolt_database::BadgeGrant {
        id: format!("{user}:{badge}"),
        user_id: user.to_string(),
        badge: badge.to_string(),
        source: BadgeSource::Legacy,
        source_ref: None,
        granted_by: "legacy".to_string(),
        granted_at: 5,
        reason: None,
        expires_at: None,
        revoked_at: None,
        revoked_by: None,
        revoke_reason: None,
        hidden,
    };
    let names = |list: Vec<revolt_models::v0::PublicBadge>| list.into_iter().map(|b| b.badge).collect::<Vec<_>>();
    // set: the founder without a row is listed; someone else's row is history
    assert_eq!(names(public_badges(vec![], Some(&founder), &founder, 10)), vec!["founder"]);
    assert!(names(public_badges(vec![row(&other, "founder", false)], Some(&founder), &other, 10)).is_empty());
    // set: a hidden founder row is shown anyway, once
    assert_eq!(names(public_badges(vec![row(&founder, "founder", true)], Some(&founder), &founder, 10)), vec!["founder"]);
    // unset: rows as stored, hidden ones not shown
    assert_eq!(names(public_badges(vec![row(&other, "founder", false)], None, &other, 10)), vec!["founder"]);
    assert!(names(public_badges(vec![row(&other, "founder", true), row(&other, "partner", true)], None, &other, 10)).is_empty());
}
