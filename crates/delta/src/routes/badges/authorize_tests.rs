//! The one badge gate (`revolt_database::mutate_badge`) against the abuse
//! cases of the badge audit (F.1): the same rule matrix on every path.

use revolt_database::{
    appoint_staff, appointment_gaps, approve_badge_request, end_staff_appointment, founder_bit,
    grant_badge, held_badges, mutate_badge, note_founder_config, refuse_badge_request, AuditOutcome,
    BadgeActor, BadgeChange, BadgeMutation, BadgeOutcome, BadgePath, BadgeRequestStatus,
    BadgeSource, PlatformAuditEntry, PlatformAuditQuery, StaffGrant, StaffRole, User,
    ADMIN_ROUTE_RETIRED,
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
    };
    assert!(harness.db.save_staff_grant(&grant, None).await.unwrap());
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
        for badge in ["translator", "responsible_disclosure", "supporter", "paw", "reserved_1", "reserved_2"] {
            let result = try_change(&harness, &strongest(&actor, path), &target, badge, BadgeChange::Grant).await;
            assert!(result.is_err(), "{path:?} {badge}");
        }
    }
    assert!(held_badges(&harness.db, &target.id).await.unwrap().is_empty());
    let records = audit(&harness, &target).await;
    assert_eq!(denied(&records, "retired"), 24);
    assert_eq!(denied(&records, "route_retired"), 6);
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
