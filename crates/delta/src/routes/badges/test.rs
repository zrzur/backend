use revolt_database::{
    mutate_badge, sync_user_badges, BadgeAction, BadgeActor, BadgeChange, BadgeMutation,
    BadgeOutcome, BadgePath, CreatorStanding, File, Metadata, PartialUser, PlatformPermission,
    PlatformRole, Session, User,
};
use revolt_database::{AuditOutcome, BugKind, BugStatus};
use revolt_result::ErrorType;
use revolt_models::v0::{self, UserBadges};
use rocket::http::{ContentType, Header, Status};
use rocket::local::asynchronous::LocalResponse;

use crate::util::test::TestHarness;

/// The error's variant name (`ErrorType` has no `PartialEq`)
fn kind(error: ErrorType) -> String {
    format!("{error:?}").split([' ', '{', '(']).next().unwrap_or_default().to_string()
}

async fn set_role(
    harness: &TestHarness,
    user: &User,
    permissions: Vec<PlatformPermission>,
    creator: Option<CreatorStanding>,
) {
    harness
        .db
        .save_platform_role(&PlatformRole {
            id: user.id.clone(),
            permissions,
            creator,
            show_public_name: false,
            updated_at: 0,
            updated_by: "test".to_string(),
        })
        .await
        .unwrap();
}

async fn make_privileged(harness: &TestHarness, user: &User) {
    harness.make_privileged(&user.id).await;
}

async fn staff(harness: &TestHarness, permissions: Vec<PlatformPermission>) -> (Session, User) {
    let (_, session, user) = harness.new_user().await;
    set_role(harness, &user, permissions, None).await;
    (session, user)
}

async fn post<'a>(
    harness: &'a TestHarness,
    session: &Session,
    path: &'a str,
    body: serde_json::Value,
) -> LocalResponse<'a> {
    harness
        .client
        .post(path)
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(body.to_string())
        .dispatch()
        .await
}

async fn get<'a>(harness: &'a TestHarness, session: &Session, path: &'a str) -> LocalResponse<'a> {
    harness
        .client
        .get(path)
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await
}

/// Badge rights as the admin console gives them (the gate trusts the path
/// for who is asking; the console builds this from the staff session)
fn console(user: &User, staff: bool, owner: bool) -> BadgeActor {
    BadgeActor {
        id: user.id.clone(),
        path: BadgePath::StaffConsole,
        staff,
        owner,
        step_up: owner,
        session_id: None,
    }
}

/// A new account with `platform.badges.manage` rights in the console
async fn badge_staff(harness: &TestHarness) -> (Session, User, BadgeActor) {
    let (_, session, user) = harness.new_user().await;
    let actor = console(&user, true, false);
    (session, user, actor)
}

async fn change(
    harness: &TestHarness,
    actor: &BadgeActor,
    user: &User,
    badge: &str,
    change: BadgeChange,
    expires_at: Option<i64>,
) -> Result<BadgeOutcome, String> {
    mutate_badge(
        &harness.db,
        actor,
        BadgeMutation {
            user_id: user.id.clone(),
            badge: badge.to_string(),
            change,
            reason: "test change".to_string(),
            expires_at,
            source_ref: None,
        },
    )
    .await
    .map_err(|e| kind(e.error_type))
}

/// Grant through the one gate; `Ok(true)` when applied, `Ok(false)` when it
/// waits for a second person
async fn grant(harness: &TestHarness, actor: &BadgeActor, user: &User, badge: &str) -> Result<bool, String> {
    change(harness, actor, user, badge, BadgeChange::Grant, None)
        .await
        .map(|outcome| matches!(outcome, BadgeOutcome::Applied(_)))
}

async fn badges_of(harness: &TestHarness, user: &User) -> u32 {
    harness
        .db
        .fetch_user(&user.id)
        .await
        .unwrap()
        .badges
        .unwrap_or_default() as u32
}

async fn upload(harness: &TestHarness, user: &User, tag: &str) -> String {
    let id = TestHarness::rand_string();
    harness
        .db
        .insert_attachment(&File {
            id: id.clone(),
            tag: tag.to_string(),
            filename: "shot.png".to_string(),
            hash: None,
            uploaded_at: None,
            uploader_id: Some(user.id.clone()),
            used_for: None,
            deleted: None,
            reported: None,
            purged: None,
            metadata: Metadata::Image {
                width: 128,
                height: 128,
                thumbhash: None,
                animated: None,
            },
            content_type: "image/png".to_string(),
            size: 100,
            message_id: None,
            user_id: None,
            server_id: None,
            object_id: None,
            forwarded_from: None,
        })
        .await
        .expect("attachment");
    id
}

async fn publish_design(harness: &TestHarness, session: &Session, user: &User, name: &str) -> String {
    let file = upload(harness, user, "decorations").await;
    let res = post(
        harness,
        session,
        "/community/decorations",
        json!({
            "name": name,
            "file": file,
            "submit": true,
            "rights": { "confirmed": true, "terms_version": revolt_database::CREATOR_TERMS_VERSION },
        }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    res.into_json::<v0::CommunityItem>().await.unwrap().id
}

async fn beta_allowed(harness: &TestHarness, session: &Session, feature: &str) -> bool {
    get(harness, session, format!("/beta/features/{feature}").leak())
        .await
        .into_json::<v0::BetaAccess>()
        .await
        .unwrap()
        .allowed
}

#[rocket::async_test]
async fn catalogue_lists_every_badge() {    let harness = TestHarness::new().await;
    let res = harness.client.get("/badges/catalogue").dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    let badges = res.into_json::<Vec<v0::BadgeInfo>>().await.unwrap();
    for id in [
        "creator",
        "super_creator",
        "bug_hunter",
        "elite_bug_hunter",
        "beta_tester",
        "beta_veteran",
        "plus",
        "early_supporter",
        "staff",
        "partner",
    ] {
        assert!(badges.iter().any(|b| b.id == id), "{id} missing");
    }
    let bits: Vec<u32> = badges.iter().map(|b| b.bit).collect();
    let unique: std::collections::HashSet<_> = bits.iter().collect();
    assert_eq!(bits.len(), unique.len(), "every badge has its own bit");
}

#[rocket::async_test]
async fn grant_needs_permission() {
    let harness = TestHarness::new().await;
    let (_, _, me) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    let err = grant(&harness, &console(&me, false, false), &target, "community_helper").await.unwrap_err();
    assert!(err == "MissingPlatformPermission", "{err:?}");
    assert_eq!(badges_of(&harness, &target).await, 0);
}

#[rocket::async_test]
async fn grant_and_revoke_with_audit_log() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = badge_staff(&harness).await;
    let (_, _, target) = harness.new_user().await;

    assert_eq!(grant(&harness, &actor, &target, "community_helper").await, Ok(true));
    assert_ne!(badges_of(&harness, &target).await & UserBadges::CommunityHelper as u32, 0);

    // never twice
    assert_eq!(grant(&harness, &actor, &target, "community_helper").await, Err("NoEffect".to_string()));

    let res = change(&harness, &actor, &target, "community_helper", BadgeChange::Revoke, None).await;
    assert!(matches!(res, Ok(BadgeOutcome::Applied(_))));
    assert_eq!(badges_of(&harness, &target).await & UserBadges::CommunityHelper as u32, 0);

    let log = harness.db.fetch_badge_logs(&target.id).await.unwrap();
    assert!(log.iter().any(|e| e.action == BadgeAction::Grant));
    assert!(log
        .iter()
        .any(|e| e.action == BadgeAction::Revoke && e.reason.as_deref() == Some("test change")));

    // revoking again has no effect
    let res = change(&harness, &actor, &target, "community_helper", BadgeChange::Revoke, None).await;
    assert_eq!(res.map(|_| ()), Err("NoEffect".to_string()));
}

#[rocket::async_test]
async fn self_grant_is_denied() {
    let harness = TestHarness::new().await;
    let (_, me, actor) = badge_staff(&harness).await;
    assert_eq!(grant(&harness, &actor, &me, "community_helper").await, Err("NotPrivileged".to_string()));

    // not even as an Owner with a step-up, nor with the privileged flag
    make_privileged(&harness, &me).await;
    let owner = console(&me, true, true);
    assert_eq!(grant(&harness, &owner, &me, "developer").await, Err("NotPrivileged".to_string()));
    assert_eq!(badges_of(&harness, &me).await, 0);
}

#[rocket::async_test]
async fn staff_badge_only_from_an_appointment() {
    let harness = TestHarness::new().await;
    let (_, _, target) = harness.new_user().await;
    let (_, _, admin) = harness.new_user().await;
    make_privileged(&harness, &admin).await;
    // an Owner with a step-up and the privileged flag still can't
    let owner = console(&admin, true, true);
    for badge in ["staff", "platform_moderator"] {
        assert_eq!(grant(&harness, &owner, &target, badge).await, Err("InvalidOperation".to_string()), "{badge}");
    }
    assert_eq!(badges_of(&harness, &target).await, 0);
}

#[rocket::async_test]
async fn bots_cannot_grant_or_receive_badges() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    make_privileged(&harness, &owner).await;
    set_role(&harness, &owner, vec![PlatformPermission::BadgesGrant], None).await;
    let (bot, bot_user) = harness.new_bot(&owner).await;
    let (_, _, target) = harness.new_user().await;

    // a bot acting with its token (even one owned by staff) can't grant
    let res = harness
        .client
        .post("/badges/admin/grant")
        .header(ContentType::JSON)
        .header(Header::new("x-bot-token", bot.token.clone()))
        .body(json!({ "user": target.id, "badge": "community_helper", "reason": "bot api" }).to_string())
        .dispatch()
        .await;
    assert_ne!(res.status(), Status::Ok);
    assert_eq!(badges_of(&harness, &target).await, 0);

    // and a bot never receives one
    let (_, _, actor) = badge_staff(&harness).await;
    assert_eq!(grant(&harness, &actor, &bot_user, "community_helper").await, Err("IsBot".to_string()));
}

#[rocket::async_test]
async fn raw_bitfield_edit_is_blocked() {
    let harness = TestHarness::new().await;
    let (_, session, me) = harness.new_user().await;
    make_privileged(&harness, &me).await;
    let res = harness
        .client
        .patch("/users/@me")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "badges": UserBadges::Staff as u32 }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::BadRequest);
    assert_eq!(badges_of(&harness, &me).await, 0);
}

#[rocket::async_test]
async fn plus_creator_and_beta_badges_cannot_be_granted_by_hand() {
    let harness = TestHarness::new().await;
    let (_, _, admin) = harness.new_user().await;
    make_privileged(&harness, &admin).await;
    let owner = console(&admin, true, true);
    let (_, _, target) = harness.new_user().await;
    for badge in ["plus", "creator", "beta_tester", "early_adopter"] {
        assert_eq!(grant(&harness, &owner, &target, badge).await, Err("InvalidOperation".to_string()), "{badge}");
    }
    assert_eq!(badges_of(&harness, &target).await, 0);
}

#[rocket::async_test]
async fn expired_badge_goes_away() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = badge_staff(&harness).await;
    let (_, _, target) = harness.new_user().await;
    let soon = super::now_ms() + 60_000;
    let res = change(&harness, &actor, &target, "community_helper", BadgeChange::Grant, Some(soon)).await;
    assert!(matches!(res, Ok(BadgeOutcome::Applied(_))));
    assert_ne!(badges_of(&harness, &target).await & UserBadges::CommunityHelper as u32, 0);

    // time passes
    let mut grant = harness.db.fetch_badge_grants(&target.id).await.unwrap().pop().unwrap();
    grant.expires_at = Some(super::now_ms() - 1);
    harness.db.save_badge_grant(&grant).await.unwrap();
    sync_user_badges(&harness.db, &target.id).await.unwrap();

    assert_eq!(badges_of(&harness, &target).await & UserBadges::CommunityHelper as u32, 0);
    let log = harness.db.fetch_badge_logs(&target.id).await.unwrap();
    assert!(log.iter().any(|e| e.action == BadgeAction::Expire));
}

#[rocket::async_test]
async fn legacy_bits_are_kept_and_recorded() {
    let harness = TestHarness::new().await;
    let (_, session, me) = harness.new_user().await;
    harness
        .db
        .update_user(
            &me.id,
            &PartialUser {
                badges: Some(UserBadges::Founder as i32),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let res = get(&harness, &session, "/badges/@me").await;
    assert_eq!(res.status(), Status::Ok);
    let mine = res.into_json::<v0::MyBadges>().await.unwrap();
    assert!(mine.badges.iter().any(|b| b.badge == "founder" && b.source == "legacy"));
    assert_eq!(badges_of(&harness, &me).await, UserBadges::Founder as u32);
}

#[rocket::async_test]
async fn owner_can_hide_and_show_a_badge() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = badge_staff(&harness).await;
    let (_, target_session, target) = harness.new_user().await;
    assert_eq!(grant(&harness, &actor, &target, "community_helper").await, Ok(true));

    let hide = |hidden: bool| {
        harness
            .client
            .patch("/badges/@me/community_helper")
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", target_session.token.clone()))
            .body(json!({ "hidden": hidden }).to_string())
            .dispatch()
    };
    assert_eq!(hide(true).await.status(), Status::NoContent);
    assert_eq!(badges_of(&harness, &target).await & UserBadges::CommunityHelper as u32, 0);
    // still held
    assert!(revolt_database::held_badges(&harness.db, &target.id).await.unwrap().contains("community_helper"));
    assert_eq!(hide(false).await.status(), Status::NoContent);
    assert_ne!(badges_of(&harness, &target).await & UserBadges::CommunityHelper as u32, 0);

    // hiding something you don't hold does nothing
    let res = harness
        .client
        .patch("/badges/@me/staff")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", target_session.token.clone()))
        .body(json!({ "hidden": false }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NotFound);
    assert_eq!(badges_of(&harness, &target).await & UserBadges::Staff as u32, 0);
}

#[rocket::async_test]
async fn creator_badge_is_earned_once_and_stays() {
    let harness = TestHarness::new().await;
    let (_, session, creator) = harness.new_user().await;
    set_role(
        &harness,
        &creator,
        vec![PlatformPermission::CosmeticsSubmit, PlatformPermission::CosmeticsManageOwn],
        Some(CreatorStanding::Active),
    )
    .await;
    // permission to create designs is not the badge
    assert_eq!(badges_of(&harness, &creator).await & UserBadges::Creator as u32, 0);

    let item = publish_design(&harness, &session, &creator, "Leaf frame").await;
    assert_ne!(badges_of(&harness, &creator).await & UserBadges::Creator as u32, 0);
    // the system's grant went through the gate and was written down
    let records = system_records(&harness, &creator).await;
    assert!(records.iter().any(|e| e.action == "badge.system.grant"
        && matches!(e.outcome, AuditOutcome::Success)
        && e.summary.as_deref().unwrap_or_default().contains("creator via system")));

    // archiving the only design keeps it (permanent)
    let res = post(&harness, &session, format!("/community/items/{item}/archive").leak(), json!({})).await;
    assert_eq!(res.status(), Status::Ok);
    assert_ne!(badges_of(&harness, &creator).await & UserBadges::Creator as u32, 0);

    // so does losing the creator record
    let (manager_session, _) = staff(&harness, vec![PlatformPermission::CosmeticsManageCreators]).await;
    let res = post(
        &harness,
        &manager_session,
        "/admin/creators",
        json!({ "user": creator.id, "action": "revoke", "reason": "test" }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert_ne!(badges_of(&harness, &creator).await & UserBadges::Creator as u32, 0);
}

#[rocket::async_test]
async fn an_earned_achievement_is_taken_away_only_for_fraud_by_two_people() {
    let harness = TestHarness::new().await;
    let (_, session, creator) = harness.new_user().await;
    set_role(
        &harness,
        &creator,
        vec![PlatformPermission::CosmeticsSubmit, PlatformPermission::CosmeticsManageOwn],
        Some(CreatorStanding::Active),
    )
    .await;
    publish_design(&harness, &session, &creator, "Copied frame").await;
    assert!(revolt_database::held_badges(&harness.db, &creator.id).await.unwrap().contains("creator"));

    // nobody gives it by hand
    let (_, _, first) = badge_staff(&harness).await;
    assert_eq!(grant(&harness, &first, &creator, "creator").await, Err("InvalidOperation".to_string()));
    // without badge rights nobody takes it
    let (_, _, nobody) = harness.new_user().await;
    let weak = console(&nobody, false, false);
    assert_eq!(
        change(&harness, &weak, &creator, "creator", BadgeChange::Revoke, None).await.map(|_| ()),
        Err("MissingPlatformPermission".to_string())
    );
    // the system never takes it back
    let system = BadgeActor::system();
    assert_eq!(
        change(&harness, &system, &creator, "creator", BadgeChange::Revoke, None).await.map(|_| ()),
        Err("InvalidOperation".to_string())
    );
    // staff ask, a second person approves
    let pending = change(&harness, &first, &creator, "creator", BadgeChange::Revoke, None).await.unwrap();
    let BadgeOutcome::Pending(request) = pending else { panic!("a second person is needed") };
    assert!(revolt_database::held_badges(&harness.db, &creator.id).await.unwrap().contains("creator"));
    let (_, second_user) = staff(&harness, vec![]).await;
    let second = console(&second_user, true, false);
    revolt_database::approve_badge_request(&harness.db, &second, &request.id).await.unwrap();
    assert!(!revolt_database::held_badges(&harness.db, &creator.id).await.unwrap().contains("creator"));

    // a new design never brings it back: staff took it away
    publish_design(&harness, &session, &creator, "Another frame").await;
    revolt_database::sync_creator_badge(&harness.db, &creator.id).await.unwrap();
    assert!(!revolt_database::held_badges(&harness.db, &creator.id).await.unwrap().contains("creator"));
    assert_eq!(badges_of(&harness, &creator).await & UserBadges::Creator as u32, 0);
    // and the gate refuses it too, written down
    let forced = change(&harness, &system, &creator, "creator", BadgeChange::Grant, None).await;
    assert_eq!(forced.map(|_| ()), Err("InvalidOperation".to_string()));
    let records = system_records(&harness, &creator).await;
    assert!(records.iter().any(|e| matches!(e.outcome, AuditOutcome::Denied)
        && e.summary.as_deref().unwrap_or_default().contains("denied:staff_revoked")));
}

/// Platform audit records about a user
async fn system_records(harness: &TestHarness, user: &User) -> Vec<revolt_database::PlatformAuditEntry> {
    harness
        .db
        .fetch_platform_audit(revolt_database::PlatformAuditQuery {
            target_id: Some(user.id.clone()),
            limit: 100,
            ..Default::default()
        })
        .await
        .unwrap()
}

/// `count` more approved, published designs like `item`
async fn copy_design(harness: &TestHarness, item: &str, count: usize) {
    let original = harness.db.fetch_community_item(item).await.unwrap();
    for _ in 0..count {
        let mut copy = original.clone();
        copy.id = ulid::Ulid::new().to_string();
        harness.db.insert_community_item(&copy).await.unwrap();
    }
}

/// The creator joined the program `days` ago (creator log)
async fn joined_program(harness: &TestHarness, user: &User, days: i64) {
    harness
        .db
        .insert_creator_log(&revolt_database::CreatorPermissionLog {
            id: ulid::Ulid::new().to_string(),
            user_id: user.id.clone(),
            actor_id: "test".to_string(),
            action: revolt_database::CreatorAction::Grant,
            permissions: vec![],
            reason: None,
            created_at: super::now_ms() - days * 24 * 60 * 60 * 1000 - 60_000,
        })
        .await
        .unwrap();
}

#[rocket::async_test]
async fn super_creator_needs_25_designs_180_days_and_staff() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = badge_staff(&harness).await;
    let (_, creator_session, creator) = harness.new_user().await;
    set_role(
        &harness,
        &creator,
        vec![PlatformPermission::CosmeticsSubmit, PlatformPermission::CosmeticsManageOwn],
        Some(CreatorStanding::Active),
    )
    .await;
    let first = publish_design(&harness, &creator_session, &creator, "One").await;
    joined_program(&harness, &creator, 179).await;

    // 3 designs were enough before: no longer
    copy_design(&harness, &first, 2).await;
    let refused = grant(&harness, &actor, &creator, "super_creator").await.unwrap_err();
    assert!(refused == "FailedValidation", "{refused:?}");
    // 24 designs: one short
    copy_design(&harness, &first, 21).await;
    assert_eq!(grant(&harness, &actor, &creator, "super_creator").await, Err("FailedValidation".to_string()));
    // 25 designs but 179 days in the program
    copy_design(&harness, &first, 1).await;
    let why = revolt_database::badge_eligibility(&harness.db, &creator.id, "super_creator").await.unwrap();
    assert!(why.as_deref().unwrap_or_default().contains("180 days"), "{why:?}");
    assert_eq!(grant(&harness, &actor, &creator, "super_creator").await, Err("FailedValidation".to_string()));
    // 180 days
    joined_program(&harness, &creator, 180).await;
    assert_eq!(revolt_database::badge_eligibility(&harness.db, &creator.id, "super_creator").await.unwrap(), None);
    assert_eq!(grant(&harness, &actor, &creator, "super_creator").await, Ok(true));
    let bits = badges_of(&harness, &creator).await;
    assert_ne!(bits & UserBadges::SuperCreator as u32, 0);
    assert_ne!(bits & UserBadges::Creator as u32, 0);
    // Super Creator grants no platform powers
    let perms = revolt_database::platform_permissions(&harness.db, &harness.db.fetch_user(&creator.id).await.unwrap())
        .await
        .unwrap();
    assert!(!perms.has(PlatformPermission::CosmeticsReview));
    assert!(!perms.has(PlatformPermission::BadgesGrant));

    // the My Badges page shows the new rule
    let mine = get(&harness, &creator_session, "/badges/@me").await.into_json::<v0::MyBadges>().await.unwrap();
    assert_eq!(mine.creator.super_creator_designs, 25);
    assert_eq!(mine.creator.super_creator_days, 180);
    assert!(mine.creator.program_days.is_some_and(|days| days >= 180));
}

#[rocket::async_test]
async fn a_super_creator_from_before_keeps_it_and_losing_designs_keeps_it() {
    let harness = TestHarness::new().await;
    let (_, creator_session, creator) = harness.new_user().await;
    set_role(
        &harness,
        &creator,
        vec![PlatformPermission::CosmeticsSubmit, PlatformPermission::CosmeticsManageOwn],
        Some(CreatorStanding::Active),
    )
    .await;
    let item = publish_design(&harness, &creator_session, &creator, "Old").await;
    // held from the 3-design rule
    revolt_database::grant_badge(
        &harness.db,
        &creator.id,
        "super_creator",
        revolt_database::BadgeSource::Admin,
        None,
        "old-staff",
        Some("old rule".into()),
        None,
    )
    .await
    .unwrap();
    let res = post(&harness, &creator_session, format!("/community/items/{item}/archive").leak(), json!({})).await;
    assert_eq!(res.status(), Status::Ok);
    revolt_database::sync_creator_badge(&harness.db, &creator.id).await.unwrap();
    let held = revolt_database::held_badges(&harness.db, &creator.id).await.unwrap();
    assert!(held.contains("super_creator") && held.contains("creator"));
}

#[rocket::async_test]
async fn bot_creation_does_not_need_creator() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let res = post(&harness, &session, "/bots/create", json!({ "name": "helperbot" })).await;
    assert_eq!(res.status(), Status::Ok);
    // and it gives no badge
    assert_eq!(badges_of(&harness, &user).await, 0);
}

#[rocket::async_test]
async fn beta_access_comes_from_membership() {
    let harness = TestHarness::new().await;
    let (admin_session, _) = staff(&harness, vec![PlatformPermission::BetaManage]).await;
    let (_, session, tester) = harness.new_user().await;
    for (id, all) in [("new-composer", false), ("other-thing", false)] {
        let res = post(
            &harness,
            &admin_session,
            "/beta/admin/features",
            json!({ "id": id, "name": id, "enabled": true, "all_testers": all }),
        )
        .await;
        assert_eq!(res.status(), Status::Ok);
    }
    assert!(!beta_allowed(&harness, &session, "new-composer").await);

    // closed program: asking only puts you on the list
    assert_eq!(post(&harness, &session, "/beta/@me/request", json!({})).await.status(), Status::Ok);
    assert!(!beta_allowed(&harness, &session, "new-composer").await);
    assert_eq!(badges_of(&harness, &tester).await & UserBadges::BetaTester as u32, 0);

    // a normal user can't approve anyone
    let (_, other_session, _) = harness.new_user().await;
    let res = post(
        &harness,
        &other_session,
        "/beta/admin/members",
        json!({ "user": tester.id, "action": "approve", "features": ["new-composer"] }),
    )
    .await;
    assert_eq!(res.status(), Status::Forbidden);

    let res = post(
        &harness,
        &admin_session,
        "/beta/admin/members",
        json!({ "user": tester.id, "action": "approve", "features": ["new-composer"] }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert!(beta_allowed(&harness, &session, "new-composer").await);
    assert!(!beta_allowed(&harness, &session, "other-thing").await, "only features given to the tester");
    assert_ne!(badges_of(&harness, &tester).await & UserBadges::BetaTester as u32, 0);

    // switching the flag off closes it for everyone
    post(
        &harness,
        &admin_session,
        "/beta/admin/features",
        json!({ "id": "new-composer", "name": "new-composer", "enabled": false, "all_testers": false }),
    )
    .await;
    assert!(!beta_allowed(&harness, &session, "new-composer").await);
    post(
        &harness,
        &admin_session,
        "/beta/admin/features",
        json!({ "id": "new-composer", "name": "new-composer", "enabled": true, "all_testers": false }),
    )
    .await;

    // completing needs a contribution
    let res = post(
        &harness,
        &admin_session,
        "/beta/admin/members",
        json!({ "user": tester.id, "action": "complete" }),
    )
    .await;
    assert_eq!(res.status(), Status::BadRequest);
    let res = post(
        &harness,
        &session,
        "/bugs",
        json!({ "kind": "feedback", "title": "Composer feedback", "description": "The new composer works well", "platform": "web", "feature": "new-composer" }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    let res = post(
        &harness,
        &admin_session,
        "/beta/admin/members",
        json!({ "user": tester.id, "action": "complete" }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);

    // completing ends the beta access; Beta Veteran is no longer automatic
    let bits = badges_of(&harness, &tester).await;
    assert_eq!(bits & UserBadges::BetaVeteran as u32, 0);
    assert_eq!(bits & UserBadges::BetaTester as u32, 0);
    assert!(!beta_allowed(&harness, &session, "new-composer").await);

    // the team approves it after the cycle, when the record backs it: one
    // unread feedback report is not enough
    let (_, _, actor) = badge_staff(&harness).await;
    assert_eq!(grant(&harness, &actor, &tester, "beta_veteran").await, Err("FailedValidation".to_string()));
    let why = revolt_database::badge_eligibility(&harness.db, &tester.id, "beta_veteran").await.unwrap();
    assert!(why.as_deref().unwrap_or_default().contains("has 0 and 0"), "{why:?}");
}

/// A report by `user` of `kind` and `status`, sent `at`
async fn report_at(harness: &TestHarness, user: &User, kind: BugKind, status: BugStatus, at: i64) {
    harness
        .db
        .insert_bug_report(&revolt_database::BugReport {
            id: ulid::Ulid::new().to_string(),
            author_id: user.id.clone(),
            kind,
            title: "Report".to_string(),
            description: "What happened".to_string(),
            steps: None,
            expected: None,
            actual: None,
            platform: "web".to_string(),
            device: None,
            app_version: None,
            feature: None,
            attachments: vec![],
            status,
            severity: None,
            duplicate_of: None,
            reviewed_by: None,
            reviewer_note: None,
            keywords: vec![],
            history: vec![],
            created_at: at,
            updated_at: at,
        })
        .await
        .unwrap();
}

/// A finished beta cycle from `start` to `end`
async fn completed_member(harness: &TestHarness, user: &User, start: i64, end: i64) {
    harness
        .db
        .save_beta_member(&revolt_database::BetaMember {
            id: user.id.clone(),
            status: revolt_database::BetaStatus::Completed,
            features: vec![],
            approved_by: Some("test".to_string()),
            created_at: start,
            joined_at: Some(start),
            ended_at: Some(end),
            note: None,
            contributions: 3,
        })
        .await
        .unwrap();
}

#[rocket::async_test]
async fn beta_veteran_needs_three_reviewed_feedback_reports_or_one_confirmed_bug() {
    let harness = TestHarness::new().await;
    let (_, _, actor) = badge_staff(&harness).await;
    let now = super::now_ms();
    let (start, end) = (now - 30 * 24 * 60 * 60 * 1000, now - 24 * 60 * 60 * 1000);

    // two reviewed feedback reports, one unread, one duplicate, one outside the cycle
    let (_, _, a) = harness.new_user().await;
    completed_member(&harness, &a, start, end).await;
    report_at(&harness, &a, BugKind::Feedback, BugStatus::Triaged, start + 1).await;
    report_at(&harness, &a, BugKind::Feedback, BugStatus::Confirmed, start + 2).await;
    report_at(&harness, &a, BugKind::Feedback, BugStatus::Open, start + 3).await;
    report_at(&harness, &a, BugKind::Feedback, BugStatus::Duplicate, start + 4).await;
    report_at(&harness, &a, BugKind::Feedback, BugStatus::Fixed, end + 1000).await;
    report_at(&harness, &a, BugKind::Bug, BugStatus::NotABug, start + 5).await;
    assert_eq!(grant(&harness, &actor, &a, "beta_veteran").await, Err("FailedValidation".to_string()));
    // the third reviewed one
    report_at(&harness, &a, BugKind::Feedback, BugStatus::Fixed, end - 1000).await;
    assert_eq!(grant(&harness, &actor, &a, "beta_veteran").await, Ok(true));
    assert_ne!(badges_of(&harness, &a).await & UserBadges::BetaVeteran as u32, 0);

    // one confirmed bug report is enough
    let (_, _, b) = harness.new_user().await;
    completed_member(&harness, &b, start, end).await;
    report_at(&harness, &b, BugKind::Bug, BugStatus::Confirmed, start + 10).await;
    assert_eq!(grant(&harness, &actor, &b, "beta_veteran").await, Ok(true));

    // still in the beta: not yet
    let (_, _, c) = harness.new_user().await;
    harness
        .db
        .save_beta_member(&revolt_database::BetaMember {
            id: c.id.clone(),
            status: revolt_database::BetaStatus::Active,
            features: vec![],
            approved_by: None,
            created_at: start,
            joined_at: Some(start),
            ended_at: None,
            note: None,
            contributions: 5,
        })
        .await
        .unwrap();
    report_at(&harness, &c, BugKind::Bug, BugStatus::Confirmed, start + 10).await;
    assert_eq!(grant(&harness, &actor, &c, "beta_veteran").await, Err("FailedValidation".to_string()));

    // without badge rights: refused
    let (_, _, nobody) = harness.new_user().await;
    assert_eq!(
        grant(&harness, &console(&nobody, false, false), &b, "beta_veteran").await,
        Err("MissingPlatformPermission".to_string())
    );
}

#[rocket::async_test]
async fn a_beta_veteran_from_the_old_rule_keeps_it() {
    let harness = TestHarness::new().await;
    let (_, _, tester) = harness.new_user().await;
    let now = super::now_ms();
    completed_member(&harness, &tester, now - 10_000, now - 5_000).await;
    revolt_database::grant_badge(
        &harness.db,
        &tester.id,
        "beta_veteran",
        revolt_database::BadgeSource::System,
        Some("beta".into()),
        revolt_database::BADGE_SYSTEM_ACTOR,
        None,
        None,
    )
    .await
    .unwrap();
    // leaving or syncing again never takes it away
    let member = harness.db.fetch_beta_member(&tester.id).await.unwrap().unwrap();
    revolt_database::sync_beta_badges(&harness.db, &member, revolt_database::BADGE_SYSTEM_ACTOR).await.unwrap();
    assert!(revolt_database::held_badges(&harness.db, &tester.id).await.unwrap().contains("beta_veteran"));
}

#[rocket::async_test]
async fn removed_tester_loses_access_and_badge() {
    let harness = TestHarness::new().await;
    let (admin_session, admin) = staff(&harness, vec![PlatformPermission::BetaManage]).await;
    let (_, session, tester) = harness.new_user().await;
    post(
        &harness,
        &admin_session,
        "/beta/admin/features",
        json!({ "id": "voice-lab", "name": "Voice lab", "enabled": true, "all_testers": true }),
    )
    .await;
    // staff can't enrol themselves
    let res = post(
        &harness,
        &admin_session,
        "/beta/admin/members",
        json!({ "user": admin.id, "action": "invite" }),
    )
    .await;
    assert_eq!(res.status(), Status::Forbidden);

    let res = post(
        &harness,
        &admin_session,
        "/beta/admin/members",
        json!({ "user": tester.id, "action": "invite" }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(post(&harness, &session, "/beta/@me/accept", json!({})).await.status(), Status::Ok);
    let allowed = get(&harness, &session, "/beta/features/voice-lab")
        .await
        .into_json::<v0::BetaAccess>()
        .await
        .unwrap()
        .allowed;
    assert!(allowed);
    assert_ne!(badges_of(&harness, &tester).await & UserBadges::BetaTester as u32, 0);

    let res = post(
        &harness,
        &admin_session,
        "/beta/admin/members",
        json!({ "user": tester.id, "action": "remove" }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    let allowed = get(&harness, &session, "/beta/features/voice-lab")
        .await
        .into_json::<v0::BetaAccess>()
        .await
        .unwrap()
        .allowed;
    assert!(!allowed);
    assert_eq!(badges_of(&harness, &tester).await & UserBadges::BetaTester as u32, 0);
}

#[rocket::async_test]
async fn bug_hunter_needs_a_verified_report() {
    let harness = TestHarness::new().await;
    let (_, session, reporter) = harness.new_user().await;
    let shot = upload(&harness, &reporter, "attachments").await;
    let res = post(
        &harness,
        &session,
        "/bugs",
        json!({
            "kind": "bug",
            "title": "Voice channel list flickers",
            "description": "The member list in voice flickers after joining",
            "steps": "Join a voice channel",
            "platform": "web",
            "attachments": [shot],
        }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    let report = res.into_json::<v0::BugReport>().await.unwrap();
    assert_eq!(report.attachments.len(), 1);
    // sending it is not enough
    assert_eq!(badges_of(&harness, &reporter).await & UserBadges::BugHunter as u32, 0);

    // a second report with the same title shows up as a possible duplicate
    let res = post(
        &harness,
        &session,
        "/bugs",
        json!({ "kind": "bug", "title": "Voice channel list flickers again", "description": "Same flicker in the voice list", "platform": "desktop-windows" }),
    )
    .await;
    let second = res.into_json::<v0::BugReport>().await.unwrap();

    // the reporter can't review their own report, even as a reviewer
    set_role(&harness, &reporter, vec![PlatformPermission::BugsReview], None).await;
    let res = post(
        &harness,
        &session,
        format!("/bugs/admin/{}/review", report.id).leak(),
        json!({ "action": "confirm", "updated_at": report.updated_at }),
    )
    .await;
    assert_eq!(res.status(), Status::Forbidden);
    set_role(&harness, &reporter, vec![], None).await;

    let (reviewer_session, _) = staff(&harness, vec![PlatformPermission::BugsReview]).await;
    let res = get(&harness, &reviewer_session, format!("/bugs/{}", second.id).leak()).await;
    let review = res.into_json::<v0::BugReportReview>().await.unwrap();
    assert!(review.possible_duplicates.contains(&report.id));

    let res = post(
        &harness,
        &reviewer_session,
        format!("/bugs/admin/{}/review", report.id).leak(),
        json!({ "action": "confirm", "severity": "medium", "updated_at": report.updated_at }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert_ne!(badges_of(&harness, &reporter).await & UserBadges::BugHunter as u32, 0);

    // a stale decision is refused
    let res = post(
        &harness,
        &reviewer_session,
        format!("/bugs/admin/{}/review", report.id).leak(),
        json!({ "action": "fix", "updated_at": report.updated_at }),
    )
    .await;
    assert_eq!(res.status(), Status::Conflict);

    // Elite is never automatic and needs more than one report
    let (_, _, actor) = badge_staff(&harness).await;
    let refused = grant(&harness, &actor, &reporter, "elite_bug_hunter").await.unwrap_err();
    assert!(refused == "FailedValidation", "{refused:?}");
    assert_eq!(badges_of(&harness, &reporter).await & UserBadges::EliteBugHunter as u32, 0);
}

#[rocket::async_test]
async fn security_reports_stay_private() {
    let harness = TestHarness::new().await;
    let (_, session, reporter) = harness.new_user().await;
    let res = post(
        &harness,
        &session,
        "/bugs",
        json!({ "kind": "security", "title": "Token visible in logs", "description": "Session token printed in a debug log", "platform": "web" }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    let report = res.into_json::<v0::BugReport>().await.unwrap();
    let path: &'static str = format!("/bugs/{}", report.id).leak();

    // the reporter sees it, nobody else does
    assert_eq!(get(&harness, &session, path).await.status(), Status::Ok);
    let (_, other_session, _) = harness.new_user().await;
    assert_eq!(get(&harness, &other_session, path).await.status(), Status::NotFound);
    let (bug_reviewer, _) = staff(&harness, vec![PlatformPermission::BugsReview]).await;
    assert_eq!(get(&harness, &bug_reviewer, path).await.status(), Status::NotFound);
    let queue = get(&harness, &bug_reviewer, "/bugs/admin/queue")
        .await
        .into_json::<Vec<v0::BugReportReview>>()
        .await
        .unwrap();
    assert!(queue.iter().all(|r| r.report.id != report.id));
    assert_eq!(
        get(&harness, &bug_reviewer, "/bugs/admin/queue?security=true").await.status(),
        Status::Forbidden
    );

    let (security_reviewer, _) = staff(&harness, vec![PlatformPermission::BugsSecurity]).await;
    assert_eq!(get(&harness, &security_reviewer, path).await.status(), Status::Ok);
    let res = post(
        &harness,
        &security_reviewer,
        format!("/bugs/admin/{}/review", report.id).leak(),
        json!({ "action": "confirm", "severity": "critical", "updated_at": report.updated_at }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert_ne!(badges_of(&harness, &reporter).await & UserBadges::BugHunter as u32, 0);

    // the public badge list says nothing about the report
    let public = get(&harness, &other_session, format!("/badges/users/{}", reporter.id).leak())
        .await
        .into_string()
        .await
        .unwrap();
    assert!(public.contains("bug_hunter"));
    assert!(!public.contains(&report.id) && !public.contains("Token"));
}

#[rocket::async_test]
async fn platform_permissions_are_guarded() {
    let harness = TestHarness::new().await;
    let (session, me) = staff(&harness, vec![PlatformPermission::CosmeticsManageAll]).await;
    let (_, _, target) = harness.new_user().await;

    // a design admin can give design permissions...
    let res = post(
        &harness,
        &session,
        "/admin/permissions",
        json!({ "user": target.id, "permissions": ["cosmetics.review"] }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    // ...but not badge powers, and never to themselves
    let res = post(
        &harness,
        &session,
        "/admin/permissions",
        json!({ "user": target.id, "permissions": ["cosmetics.review", "badges.grant"] }),
    )
    .await;
    assert_eq!(res.status(), Status::Forbidden);
    let res = post(
        &harness,
        &session,
        "/admin/permissions",
        json!({ "user": me.id, "permissions": ["cosmetics.manage_all", "cosmetics.review"] }),
    )
    .await;
    assert_eq!(res.status(), Status::Forbidden);
}

mod automatic_badges {
    use revolt_database::{
        account_age_badges, check_server_community, community_badge_pass, decide_community_check,
        from_epoch_ms, grant_badge, held_badges, resync_verified_badges, retire_stored_time_badges,
        sync_verified_bot_developer, Account, AuditOutcome, BadgeActor, BadgePath, BadgeSource,
        CommunityCheckOutcome, CommunityCheckStatus, CommunityThresholds, EmailVerification, Member, MemberCompositeKey,
        PartialBot, PartialServer, PartialUser, PlatformAuditQuery, Session, User,
        TIME_BADGE_RETIRED_REASON, BADGE_SYSTEM_ACTOR,
    };
    use revolt_models::v0::{self, BotFlags, UserBadges, UserFlags};
    use rocket::http::{ContentType, Header, Status};

    use crate::util::test::TestHarness;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    const YEAR_MS: i64 = 365 * DAY_MS;

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// Small thresholds for the tests: 5 genuine members for Community
    /// Builder, 8 for Big Community (the rules are the same)
    const SMALL: CommunityThresholds = CommunityThresholds { builder: 5, big: 8 };

    /// An account made `days_old` days ago (`n` keeps the times apart), its
    /// e-mail confirmed or not
    async fn person(harness: &TestHarness, days_old: i64, n: i64, confirmed: bool) -> User {
        let made = now_ms() - days_old * DAY_MS - n * 3 * 60 * 60 * 1000;
        let id = ulid::Ulid::from_parts(made as u64, rand::random::<u128>() & 0xffff_ffff).to_string();
        let user = User::create(&harness.db, TestHarness::rand_string(), Some(id.clone()), None)
            .await
            .unwrap();
        let email = format!("{}@zrzur.cc", TestHarness::rand_string());
        harness
            .db
            .save_account(&Account {
                id,
                email: email.clone(),
                email_normalised: email,
                password: String::new(),
                disabled: false,
                verification: if confirmed {
                    EmailVerification::Verified
                } else {
                    EmailVerification::Pending {
                        token: TestHarness::rand_string(),
                        expiry: iso8601_timestamp::Timestamp::now_utc(),
                    }
                },
                password_reset: None,
                deletion: None,
                lockout: None,
                mfa: Default::default(),
                age: None,
                terms: None,
                provisional: None,
            })
            .await
            .unwrap();
        user
    }

    /// `user` joined `server` `hours_ago` hours ago
    async fn joined(harness: &TestHarness, server: &str, user: &User, hours_ago: i64) {
        harness
            .db
            .insert_or_merge_member(&Member {
                id: MemberCompositeKey {
                    server: server.to_string(),
                    user: user.id.clone(),
                },
                joined_at: from_epoch_ms(now_ms() - hours_ago * 60 * 60 * 1000),
                ..Default::default()
            })
            .await
            .unwrap();
    }

    /// `count` genuine members, joined days apart, accounts made days apart
    async fn genuine_members(harness: &TestHarness, server: &str, count: i64, from: i64) {
        for n in from..from + count {
            let user = person(harness, 30 + n * 2, n, true).await;
            joined(harness, server, &user, 48 + n * 24).await;
        }
    }

    async fn check_of(harness: &TestHarness, server: &str) -> revolt_database::CommunityBadgeCheck {
        harness.db.fetch_community_check(server).await.unwrap().unwrap()
    }

    #[rocket::async_test]
    async fn only_genuine_human_members_count() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        genuine_members(&harness, &server.id, 4, 0).await;

        // none of these count
        let young = person(&harness, 3, 0, true).await; // account under 7 days
        joined(&harness, &server.id, &young, 48).await;
        let unconfirmed = person(&harness, 40, 1, false).await; // e-mail not confirmed
        joined(&harness, &server.id, &unconfirmed, 48).await;
        let just_joined = person(&harness, 40, 2, true).await; // member under 24 hours
        joined(&harness, &server.id, &just_joined, 2).await;
        let banned = person(&harness, 40, 3, true).await;
        harness
            .db
            .update_user(&banned.id, &PartialUser { flags: Some(UserFlags::Banned as i32), ..Default::default() }, vec![])
            .await
            .unwrap();
        joined(&harness, &server.id, &banned, 48).await;
        let deleted = person(&harness, 40, 4, true).await;
        harness
            .db
            .update_user(&deleted.id, &PartialUser { flags: Some(UserFlags::Deleted as i32), ..Default::default() }, vec![])
            .await
            .unwrap();
        joined(&harness, &server.id, &deleted, 48).await;
        let (bot, _) = harness.new_bot(&owner).await;
        let bot_user = harness.db.fetch_user(&bot.id).await.unwrap();
        joined(&harness, &server.id, &bot_user, 48).await;

        let outcome = check_server_community(&harness.db, &server.id, SMALL, now_ms()).await.unwrap();
        assert_eq!(outcome, CommunityCheckOutcome::Counted);
        let check = check_of(&harness, &server.id).await;
        assert_eq!(check.counted, 4, "{check:?}");
        assert!(check.members >= 10, "{check:?}");
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().is_empty());

        // the fifth genuine member: Community Builder, written down
        genuine_members(&harness, &server.id, 1, 10).await;
        let outcome = check_server_community(&harness.db, &server.id, SMALL, now_ms()).await.unwrap();
        assert_eq!(outcome, CommunityCheckOutcome::Granted(vec!["community_builder"]));
        let held = held_badges(&harness.db, &owner.id).await.unwrap();
        assert!(held.contains("community_builder") && !held.contains("big_community"));
        let records = harness
            .db
            .fetch_platform_audit(PlatformAuditQuery { target_id: Some(owner.id.clone()), limit: 50, ..Default::default() })
            .await
            .unwrap();
        assert!(records.iter().any(|e| e.action == "badge.system.grant"
            && matches!(e.outcome, AuditOutcome::Success)
            && e.summary.as_deref().unwrap_or_default().contains("community_builder via system")));
        assert_eq!(check_of(&harness, &server.id).await.builder_to.as_deref(), Some(owner.id.as_str()));

        // counting again gives nothing twice
        assert_eq!(
            check_server_community(&harness.db, &server.id, SMALL, now_ms()).await.unwrap(),
            CommunityCheckOutcome::Counted
        );

        // eight: Big Community too, from the crond pass
        genuine_members(&harness, &server.id, 3, 20).await;
        let report = community_badge_pass(&harness.db, SMALL, now_ms()).await.unwrap();
        assert_eq!(report.failures, 0, "{report:?}");
        let held = held_badges(&harness.db, &owner.id).await.unwrap();
        assert!(held.contains("community_builder") && held.contains("big_community"));
        let bits = harness.db.fetch_user(&owner.id).await.unwrap().badges.unwrap_or_default();
        assert_ne!(bits & UserBadges::BigCommunity as i32, 0);
        assert_eq!(
            check_server_community(&harness.db, &server.id, SMALL, now_ms()).await.unwrap(),
            CommunityCheckOutcome::Done
        );

        // the server shrinks: both stay
        for member in harness.db.fetch_all_members(&server.id).await.unwrap() {
            if member.id.user != owner.id {
                harness.db.force_delete_member(&member.id).await.unwrap();
            }
        }
        community_badge_pass(&harness.db, SMALL, now_ms()).await.unwrap();
        let held = held_badges(&harness.db, &owner.id).await.unwrap();
        assert!(held.contains("community_builder") && held.contains("big_community"));
    }

    #[rocket::async_test]
    async fn a_burst_of_joins_or_signups_waits_for_staff() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        // five accounts that joined within the same hour
        for n in 0..5 {
            let user = person(&harness, 30 + n * 2, n, true).await;
            joined(&harness, &server.id, &user, 48).await;
        }
        let outcome = check_server_community(&harness.db, &server.id, SMALL, now_ms()).await.unwrap();
        assert_eq!(outcome, CommunityCheckOutcome::Held);
        let check = check_of(&harness, &server.id).await;
        assert_eq!(check.status, CommunityCheckStatus::Held);
        assert_eq!(check.join_burst, 5);
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().is_empty());
        // one hold record, not one per pass
        check_server_community(&harness.db, &server.id, SMALL, now_ms()).await.unwrap();
        let holds = harness
            .db
            .fetch_platform_audit(PlatformAuditQuery {
                target_id: Some(server.id.clone()),
                action_prefix: Some("badge.system.community.hold".to_string()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(holds.len(), 1);

        // accounts all made in the same hour: held as well
        let (_, _, owner_b) = harness.new_user().await;
        let (server_b, _) = harness.new_server(&owner_b).await;
        for n in 0..5 {
            let user = person(&harness, 30, 0, true).await;
            joined(&harness, &server_b.id, &user, 48 + n * 24).await;
        }
        assert_eq!(
            check_server_community(&harness.db, &server_b.id, SMALL, now_ms()).await.unwrap(),
            CommunityCheckOutcome::Held
        );
        assert!(check_of(&harness, &server_b.id).await.signup_burst >= 5);

        // staff without badge rights can't release; the owner never can
        let (_, _, nobody) = harness.new_user().await;
        let weak = BadgeActor { staff: false, ..staff_actor(&nobody) };
        let error = decide_community_check(&harness.db, &weak, &server.id, true, "looked at it", SMALL, now_ms())
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("MissingPlatformPermission"));
        let own = decide_community_check(&harness.db, &staff_actor(&owner), &server.id, true, "mine", SMALL, now_ms())
            .await
            .unwrap_err();
        assert!(format!("{own:?}").contains("NotPrivileged"));
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().is_empty());

        // a staff member looks and releases it: given at once, written down
        let (_, _, reviewer) = harness.new_user().await;
        let after = decide_community_check(
            &harness.db,
            &staff_actor(&reviewer),
            &server.id,
            true,
            "launch announcement, real people",
            SMALL,
            now_ms(),
        )
        .await
        .unwrap();
        assert_eq!(after.status, CommunityCheckStatus::Released);
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().contains("community_builder"));
        let decisions = harness
            .db
            .fetch_platform_audit(PlatformAuditQuery {
                target_id: Some(server.id.clone()),
                action_prefix: Some("staff.badge.community.".to_string()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(decisions.iter().any(|e| matches!(e.outcome, AuditOutcome::Success) && e.action.ends_with("release")));
        assert_eq!(decisions.iter().filter(|e| matches!(e.outcome, AuditOutcome::Denied)).count(), 2);

        // the other one is refused: no badge from it, pass after pass
        decide_community_check(&harness.db, &staff_actor(&reviewer), &server_b.id, false, "bought accounts", SMALL, now_ms())
            .await
            .unwrap();
        assert_eq!(
            check_server_community(&harness.db, &server_b.id, SMALL, now_ms()).await.unwrap(),
            CommunityCheckOutcome::Done
        );
        assert!(held_badges(&harness.db, &owner_b.id).await.unwrap().is_empty());
    }

    fn staff_actor(user: &User) -> BadgeActor {
        BadgeActor {
            id: user.id.clone(),
            path: BadgePath::StaffConsole,
            staff: true,
            owner: false,
            step_up: false,
            session_id: None,
        }
    }

    #[rocket::async_test]
    async fn a_server_gives_each_badge_once_and_old_holders_keep_theirs() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        genuine_members(&harness, &server.id, 5, 0).await;
        check_server_community(&harness.db, &server.id, SMALL, now_ms()).await.unwrap();
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().contains("community_builder"));

        // handed to someone else: the new owner gets nothing from this server
        let (_, _, next) = harness.new_user().await;
        harness
            .db
            .update_server(&server.id, &PartialServer { owner: Some(next.id.clone()), ..Default::default() }, vec![])
            .await
            .unwrap();
        check_server_community(&harness.db, &server.id, SMALL, now_ms()).await.unwrap();
        assert!(held_badges(&harness.db, &next.id).await.unwrap().is_empty());

        // someone who got it under the old rule (any member counted) keeps it
        let (_, _, old_holder) = harness.new_user().await;
        let (small, _) = harness.new_server(&old_holder).await;
        revolt_database::grant_badge(
            &harness.db,
            &old_holder.id,
            "community_builder",
            revolt_database::BadgeSource::System,
            Some(format!("server:{}", small.id)),
            revolt_database::BADGE_SYSTEM_ACTOR,
            None,
            None,
        )
        .await
        .unwrap();
        community_badge_pass(&harness.db, SMALL, now_ms()).await.unwrap();
        check_server_community(&harness.db, &small.id, SMALL, now_ms()).await.unwrap();
        assert!(held_badges(&harness.db, &old_holder.id).await.unwrap().contains("community_builder"));

        // a banned owner gets nothing until the ban ends
        let (_, _, banned_owner) = harness.new_user().await;
        let (third, _) = harness.new_server(&banned_owner).await;
        genuine_members(&harness, &third.id, 5, 40).await;
        harness
            .db
            .update_user(&banned_owner.id, &PartialUser { flags: Some(UserFlags::Banned as i32), ..Default::default() }, vec![])
            .await
            .unwrap();
        assert_eq!(
            check_server_community(&harness.db, &third.id, SMALL, now_ms()).await.unwrap(),
            CommunityCheckOutcome::OwnerSkipped
        );
        assert!(held_badges(&harness.db, &banned_owner.id).await.unwrap().is_empty());
    }

    #[rocket::async_test]
    async fn joining_a_server_never_gives_a_community_badge() {
        // the count is crond's: a join only adds a member
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        for _ in 0..3 {
            let (_, _, user) = harness.new_user().await;
            Member::create(&harness.db, &server, &user, None).await.unwrap();
        }
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().is_empty());
        assert!(harness.db.fetch_community_check(&server.id).await.unwrap().is_none());
    }

    #[rocket::async_test]
    async fn a_verified_server_gives_its_owner_no_badge_and_holders_keep_theirs() {
        // Verified Server Owner was cancelled (2026-11-24): the server's mark
        // no longer gives or takes the owner's badge, the safety net included
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        let set = |flags: i32| {
            let db = harness.db.clone();
            let id = server.id.clone();
            async move {
                db.update_server(
                    &id,
                    &PartialServer {
                        flags: Some(flags),
                        ..Default::default()
                    },
                    vec![],
                )
                .await
                .unwrap();
            }
        };

        set(2).await;
        resync_verified_badges(&harness.db).await.unwrap();
        assert!(!held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));

        grant_badge(&harness.db, &owner.id, "verified_server_owner", BadgeSource::System, Some("server".into()), BADGE_SYSTEM_ACTOR, None, None)
            .await
            .unwrap();
        set(0).await;
        resync_verified_badges(&harness.db).await.unwrap();
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));
    }

    #[rocket::async_test]
    async fn the_developer_badge_follows_a_verified_bot_that_is_not_suspended() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (bot, _) = harness.new_bot(&owner).await;
        for (flags, earned) in [
            (BotFlags::Verified as i32, true),
            (BotFlags::Verified as i32 | BotFlags::Suspended as i32, false),
            (BotFlags::Verified as i32, true),
            (0, false),
        ] {
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
            sync_verified_bot_developer(&harness.db, &owner.id).await.unwrap();
            let held = held_badges(&harness.db, &owner.id).await.unwrap();
            assert_eq!(held.contains("verified_bot_developer"), earned, "flags {flags}");
        }
    }

    /// An account made at `ms` (UTC milliseconds)
    fn made_at(ms: u64) -> User {
        User {
            id: ulid::Ulid::from_parts(ms, 7).to_string(),
            username: "someone".to_string(),
            ..Default::default()
        }
    }

    const CUTOFF: u64 = 1_803_859_200_000; // 2027-03-01T00:00:00Z

    #[test]
    fn early_adopter_ends_exactly_at_the_cutoff_in_utc() {
        let now = CUTOFF as i64 + 1;
        let early = UserBadges::EarlyAdopter as u32;
        assert_eq!(account_age_badges(&made_at(CUTOFF - 1), Some(CUTOFF), now) & early, early);
        assert_eq!(account_age_badges(&made_at(CUTOFF), Some(CUTOFF), now) & early, 0);
        assert_eq!(account_age_badges(&made_at(CUTOFF + 1), Some(CUTOFF), now) & early, 0);
        // unset: nobody has it
        assert_eq!(account_age_badges(&made_at(CUTOFF - 1), None, now) & early, 0);
    }

    #[test]
    fn bots_official_system_and_removed_accounts_get_no_time_badges() {
        let now = CUTOFF as i64 + 5 * YEAR_MS;
        let old = || made_at(CUTOFF - 2 * YEAR_MS as u64);
        assert_ne!(account_age_badges(&old(), Some(CUTOFF), now), 0);

        let mut bot = old();
        bot.bot = Some(revolt_database::BotInformation {
            owner: "someone".to_string(),
            verified: false,
        });
        let mut official = old();
        official.official = true;
        // the official accounts were made with a zero time
        let system = User {
            id: "0000000000000000000000ZRZR".to_string(),
            ..Default::default()
        };
        let mut deleted = old();
        deleted.flags = Some(UserFlags::Deleted as i32);
        let mut banned = old();
        banned.flags = Some(UserFlags::Banned as i32);
        for user in [bot, official, system, deleted, banned] {
            assert_eq!(account_age_badges(&user, Some(CUTOFF), now), 0, "{}", user.id);
        }
    }

    #[test]
    fn one_and_three_years_follow_full_calendar_years() {
        let made = CUTOFF; // 2027-03-01, before 29 February 2028
        let one = UserBadges::OneYear as u32;
        let three = UserBadges::ThreeYears as u32;
        let year_one = 1_835_481_600_000; // 2028-03-01T00:00:00Z
        let year_three = 1_898_553_600_000; // 2030-03-01T00:00:00Z
        let badges = |now: i64| account_age_badges(&made_at(made), None, now);
        // 365 days later is still 29 February 2028: not a full year
        assert_eq!(badges(made as i64 + 365 * DAY_MS) & one, 0);
        assert_eq!(badges(year_one - 1) & one, 0);
        assert_eq!(badges(year_one), one);
        assert_eq!(badges(year_three - 1), one);
        assert_eq!(badges(year_three), one | three);
    }

    #[rocket::async_test]
    async fn time_badges_are_computed_and_follow_member_since() {
        // before the harness: the first config read is kept
        revolt_config::overwrite_config(|config| {
            config.api.users.early_adopter_cutoff = Some(CUTOFF);
        })
        .await;
        let harness = TestHarness::new().await;
        let (_, own_session, user) = harness.new_user().await;
        let early = UserBadges::EarlyAdopter as u32;

        // a new account, made before the cutoff
        let fetched = harness.db.fetch_user(&user.id).await.unwrap();
        assert_eq!(fetched.get_badges().await & early, early);
        let shown = |session: &Session| {
            let path = format!("/users/{}", user.id);
            let token = session.token.clone();
            let client = &harness.client;
            async move {
                let res = client
                    .get(path)
                    .header(Header::new("x-session-token", token))
                    .dispatch()
                    .await;
                assert_eq!(res.status(), Status::Ok);
                res.into_json::<v0::User>().await.unwrap().badges
            }
        };
        assert_eq!(shown(&own_session).await & early, early);

        // hiding "Member since" hides them for everyone, in the API too
        let res = harness
            .client
            .patch("/users/@me")
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", own_session.token.clone()))
            .body(serde_json::json!({ "profile": { "hidden_sections": ["member_since"] } }).to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);
        let own = res.into_json::<v0::User>().await.unwrap();
        assert_eq!(own.badges & early, 0);
        assert_eq!(shown(&own_session).await & early, 0);
        let fetched = harness.db.fetch_user(&user.id).await.unwrap();
        assert!(fetched.hides_member_since());
        assert_eq!(fetched.get_badges().await & early, 0);

        // a stored bit is never shown on its own
        harness
            .db
            .update_user(
                &user.id,
                &PartialUser {
                    badges: Some((UserBadges::OneYear as u32 | UserBadges::ThreeYears as u32) as i32),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        let fetched = harness.db.fetch_user(&user.id).await.unwrap();
        assert_eq!(fetched.get_badges().await & revolt_database::ACCOUNT_AGE_BADGES, 0);
    }

    #[rocket::async_test]
    async fn stored_time_grants_end_once_with_a_record() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        // what the old worker gave (an official account had them too)
        let official = revolt_database::official_account(
            &harness.db,
            revolt_database::OfficialAccount::Zrzur,
        )
        .await
        .unwrap();
        for (id, badge) in [
            (&user.id, "one_year"),
            (&official.id, "one_year"),
            (&official.id, "three_years"),
        ] {
            harness
                .db
                .insert_badge_grant(&revolt_database::BadgeGrant {
                    id: format!("{id}:{badge}"),
                    user_id: id.clone(),
                    badge: badge.to_string(),
                    source: revolt_database::BadgeSource::System,
                    source_ref: Some("time".to_string()),
                    granted_by: revolt_database::BADGE_SYSTEM_ACTOR.to_string(),
                    granted_at: now_ms(),
                    reason: None,
                    expires_at: None,
                    revoked_at: None,
                    revoked_by: None,
                    revoke_reason: None,
                    hidden: false,
                })
                .await
                .unwrap();
        }
        harness
            .db
            .update_user(
                &official.id,
                &PartialUser {
                    badges: Some((UserBadges::OneYear as u32 | UserBadges::ThreeYears as u32) as i32),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        assert_eq!(retire_stored_time_badges(&harness.db).await.unwrap(), 3);
        // twice changes nothing
        assert_eq!(retire_stored_time_badges(&harness.db).await.unwrap(), 0);

        for id in [&user.id, &official.id] {
            assert!(held_badges(&harness.db, id).await.unwrap().is_empty());
            // the records stay as history, with the reason
            let grants = harness.db.fetch_badge_grants(id).await.unwrap();
            assert!(!grants.is_empty());
            assert!(grants.iter().all(|g| g.revoke_reason.as_deref() == Some(TIME_BADGE_RETIRED_REASON)));
            let log = harness.db.fetch_badge_logs(id).await.unwrap();
            assert!(log.iter().any(|e| matches!(e.action, revolt_database::BadgeAction::Revoke)));
        }
        let stored = harness.db.fetch_user(&official.id).await.unwrap();
        assert_eq!(stored.badges.unwrap_or_default() as u32 & revolt_database::ACCOUNT_AGE_BADGES, 0);
        assert_eq!(stored.get_badges().await & revolt_database::ACCOUNT_AGE_BADGES, 0);

        // one platform audit record per ended grant, none the second time
        let audit = harness
            .db
            .fetch_platform_audit(revolt_database::PlatformAuditQuery {
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        let ended: Vec<_> = audit.iter().filter(|e| e.action == "badge.system.revoke").collect();
        assert_eq!(ended.len(), 3);
        assert!(ended.iter().any(|e| e.summary.as_deref() == Some("three_years stored grant ended (official or system account)")));
    }
}
