use revolt_database::{
    sync_user_badges, BadgeAction, CreatorStanding, File, Metadata, PartialUser,
    PlatformPermission, PlatformRole, Session, User,
};
use revolt_models::v0::{self, UserBadges};
use rocket::http::{ContentType, Header, Status};
use rocket::local::asynchronous::LocalResponse;

use crate::util::test::TestHarness;

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
    harness
        .db
        .update_user(
            &user.id,
            &PartialUser {
                privileged: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
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

async fn grant(
    harness: &TestHarness,
    session: &Session,
    user: &User,
    badge: &str,
) -> Status {
    post(
        harness,
        session,
        "/badges/admin/grant",
        json!({ "user": user.id, "badge": badge, "reason": "test grant" }),
    )
    .await
    .status()
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
        json!({ "name": name, "file": file, "submit": true }),
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
    let (_, session, _) = harness.new_user().await;
    let (_, _, target) = harness.new_user().await;
    assert_eq!(grant(&harness, &session, &target, "partner").await, Status::Forbidden);
    assert_eq!(badges_of(&harness, &target).await, 0);
}

#[rocket::async_test]
async fn grant_and_revoke_with_audit_log() {
    let harness = TestHarness::new().await;
    let (session, _) = staff(&harness, vec![PlatformPermission::BadgesGrant]).await;
    let (_, _, target) = harness.new_user().await;

    assert_eq!(grant(&harness, &session, &target, "partner").await, Status::Ok);
    assert_ne!(badges_of(&harness, &target).await & UserBadges::Partner as u32, 0);

    // never twice
    assert_eq!(grant(&harness, &session, &target, "partner").await, Status::BadRequest);

    let res = post(
        &harness,
        &session,
        "/badges/admin/revoke",
        json!({ "user": target.id, "badge": "partner", "reason": "partnership ended" }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(badges_of(&harness, &target).await & UserBadges::Partner as u32, 0);

    let log = harness.db.fetch_badge_logs(&target.id).await.unwrap();
    assert!(log.iter().any(|e| e.action == BadgeAction::Grant));
    assert!(log
        .iter()
        .any(|e| e.action == BadgeAction::Revoke && e.reason.as_deref() == Some("partnership ended")));

    // revoking again has no effect
    let res = post(
        &harness,
        &session,
        "/badges/admin/revoke",
        json!({ "user": target.id, "badge": "partner", "reason": "again" }),
    )
    .await;
    assert_eq!(res.status(), Status::BadRequest);
}

#[rocket::async_test]
async fn self_grant_is_denied() {
    let harness = TestHarness::new().await;
    let (session, me) = staff(&harness, vec![PlatformPermission::BadgesGrant]).await;
    assert_eq!(grant(&harness, &session, &me, "partner").await, Status::Forbidden);

    // not even with a privileged account
    make_privileged(&harness, &me).await;
    assert_eq!(grant(&harness, &session, &me, "staff").await, Status::Forbidden);
    assert_eq!(badges_of(&harness, &me).await, 0);
}

#[rocket::async_test]
async fn staff_badge_needs_privileged_account() {
    let harness = TestHarness::new().await;
    let (session, _) = staff(&harness, vec![PlatformPermission::BadgesGrant]).await;
    let (_, _, target) = harness.new_user().await;
    assert_eq!(grant(&harness, &session, &target, "staff").await, Status::Forbidden);

    let (_, admin_session, admin) = harness.new_user().await;
    make_privileged(&harness, &admin).await;
    assert_eq!(grant(&harness, &admin_session, &target, "staff").await, Status::Ok);
    assert_ne!(badges_of(&harness, &target).await & UserBadges::Staff as u32, 0);
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
        .body(json!({ "user": target.id, "badge": "staff", "reason": "bot api" }).to_string())
        .dispatch()
        .await;
    assert_ne!(res.status(), Status::Ok);
    assert_eq!(badges_of(&harness, &target).await, 0);

    // and a bot never receives one
    let (_, admin_session, admin) = harness.new_user().await;
    make_privileged(&harness, &admin).await;
    assert_eq!(grant(&harness, &admin_session, &bot_user, "partner").await, Status::BadRequest);
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
    let (_, session, admin) = harness.new_user().await;
    make_privileged(&harness, &admin).await;
    let (_, _, target) = harness.new_user().await;
    for badge in ["plus", "creator", "beta_tester", "beta_veteran", "early_adopter"] {
        assert_eq!(grant(&harness, &session, &target, badge).await, Status::BadRequest, "{badge}");
    }
    assert_eq!(badges_of(&harness, &target).await, 0);
}

#[rocket::async_test]
async fn expired_badge_goes_away() {
    let harness = TestHarness::new().await;
    let (session, _) = staff(&harness, vec![PlatformPermission::BadgesGrant]).await;
    let (_, _, target) = harness.new_user().await;
    let soon = super::now_ms() + 60_000;
    let res = post(
        &harness,
        &session,
        "/badges/admin/grant",
        json!({ "user": target.id, "badge": "partner", "reason": "event", "expires_at": soon }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert_ne!(badges_of(&harness, &target).await & UserBadges::Partner as u32, 0);

    // time passes
    let mut grant = harness.db.fetch_badge_grants(&target.id).await.unwrap().pop().unwrap();
    grant.expires_at = Some(super::now_ms() - 1);
    harness.db.save_badge_grant(&grant).await.unwrap();
    sync_user_badges(&harness.db, &target.id).await.unwrap();

    assert_eq!(badges_of(&harness, &target).await & UserBadges::Partner as u32, 0);
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
    let (session, _) = staff(&harness, vec![PlatformPermission::BadgesGrant]).await;
    let (_, target_session, target) = harness.new_user().await;
    assert_eq!(grant(&harness, &session, &target, "partner").await, Status::Ok);

    let hide = |hidden: bool| {
        harness
            .client
            .patch("/badges/@me/partner")
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", target_session.token.clone()))
            .body(json!({ "hidden": hidden }).to_string())
            .dispatch()
    };
    assert_eq!(hide(true).await.status(), Status::NoContent);
    assert_eq!(badges_of(&harness, &target).await & UserBadges::Partner as u32, 0);
    // still held
    assert!(revolt_database::held_badges(&harness.db, &target.id).await.unwrap().contains("partner"));
    assert_eq!(hide(false).await.status(), Status::NoContent);
    assert_ne!(badges_of(&harness, &target).await & UserBadges::Partner as u32, 0);

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
async fn creator_badge_follows_published_designs() {
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

    // archiving the only design takes it away
    let res = post(&harness, &session, format!("/community/items/{item}/archive").leak(), json!({})).await;
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(badges_of(&harness, &creator).await & UserBadges::Creator as u32, 0);

    // revoking the creator record takes it away too
    publish_design(&harness, &session, &creator, "Second frame").await;
    assert_ne!(badges_of(&harness, &creator).await & UserBadges::Creator as u32, 0);
    let (manager_session, _) = staff(&harness, vec![PlatformPermission::CosmeticsManageCreators]).await;
    let res = post(
        &harness,
        &manager_session,
        "/admin/creators",
        json!({ "user": creator.id, "action": "revoke", "reason": "test" }),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(badges_of(&harness, &creator).await & UserBadges::Creator as u32, 0);
}

#[rocket::async_test]
async fn super_creator_needs_designs_and_staff() {
    let harness = TestHarness::new().await;
    let (session, _) = staff(&harness, vec![PlatformPermission::BadgesGrant]).await;
    let (_, creator_session, creator) = harness.new_user().await;
    set_role(
        &harness,
        &creator,
        vec![PlatformPermission::CosmeticsSubmit, PlatformPermission::CosmeticsManageOwn],
        Some(CreatorStanding::Active),
    )
    .await;
    publish_design(&harness, &creator_session, &creator, "One").await;
    assert_eq!(grant(&harness, &session, &creator, "super_creator").await, Status::BadRequest);
    publish_design(&harness, &creator_session, &creator, "Two").await;
    publish_design(&harness, &creator_session, &creator, "Three").await;
    assert_eq!(grant(&harness, &session, &creator, "super_creator").await, Status::Ok);
    let bits = badges_of(&harness, &creator).await;
    assert_ne!(bits & UserBadges::SuperCreator as u32, 0);
    assert_ne!(bits & UserBadges::Creator as u32, 0);
    // Super Creator grants no platform powers
    let perms = revolt_database::platform_permissions(&harness.db, &harness.db.fetch_user(&creator.id).await.unwrap())
        .await
        .unwrap();
    assert!(!perms.has(PlatformPermission::CosmeticsReview));
    assert!(!perms.has(PlatformPermission::BadgesGrant));
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

    // Beta Veteran is recognition only: no more beta access
    let bits = badges_of(&harness, &tester).await;
    assert_ne!(bits & UserBadges::BetaVeteran as u32, 0);
    assert_eq!(bits & UserBadges::BetaTester as u32, 0);
    assert!(!beta_allowed(&harness, &session, "new-composer").await);
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
    let (grant_session, _) = staff(&harness, vec![PlatformPermission::BadgesGrant]).await;
    assert_eq!(grant(&harness, &grant_session, &reporter, "elite_bug_hunter").await, Status::BadRequest);
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
        grant_time_badge, held_badges, sync_server_size_badges, sync_verified_bot_developer,
        sync_verified_server_owner, PartialBot, PartialServer,
    };
    use revolt_models::v0::{BotFlags, UserBadges};

    use crate::util::test::TestHarness;

    const YEAR_MS: i64 = 365 * 24 * 60 * 60 * 1000;

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    #[rocket::async_test]
    async fn a_server_that_grows_earns_badges_and_keeps_them() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;

        sync_server_size_badges(&harness.db, &server, 99).await.unwrap();
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().is_empty());

        sync_server_size_badges(&harness.db, &server, 100).await.unwrap();
        let held = held_badges(&harness.db, &owner.id).await.unwrap();
        assert!(held.contains("community_builder") && !held.contains("big_community"));

        sync_server_size_badges(&harness.db, &server, 1000).await.unwrap();
        // running it again is fine, and so is the server shrinking
        sync_server_size_badges(&harness.db, &server, 1000).await.unwrap();
        sync_server_size_badges(&harness.db, &server, 3).await.unwrap();
        let held = held_badges(&harness.db, &owner.id).await.unwrap();
        assert!(held.contains("community_builder") && held.contains("big_community"));
        let bits = harness.db.fetch_user(&owner.id).await.unwrap().badges.unwrap_or_default();
        assert_ne!(bits & UserBadges::BigCommunity as i32, 0);
    }

    #[rocket::async_test]
    async fn the_owner_badge_follows_the_verified_server() {
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

        sync_verified_server_owner(&harness.db, &owner.id).await.unwrap();
        assert!(!held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));

        set(2).await;
        sync_verified_server_owner(&harness.db, &owner.id).await.unwrap();
        assert!(held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));

        set(0).await;
        sync_verified_server_owner(&harness.db, &owner.id).await.unwrap();
        assert!(!held_badges(&harness.db, &owner.id).await.unwrap().contains("verified_server_owner"));
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

    #[rocket::async_test]
    async fn time_badges_go_to_accounts_old_enough_and_only_once() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, bot_user) = harness.new_bot(&owner).await;
        let bit = UserBadges::OneYear as u32;

        // a new account has a while to go
        grant_time_badge(&harness.db, "one_year", bit, 1, now_ms(), None).await.unwrap();
        assert!(!held_badges(&harness.db, &user.id).await.unwrap().contains("one_year"));

        // a year later it has, and a second walk finds nothing to do
        let later = now_ms() + YEAR_MS + 60_000;
        let next = grant_time_badge(&harness.db, "one_year", bit, 1, later, None).await.unwrap();
        assert!(next.is_none());
        assert!(held_badges(&harness.db, &user.id).await.unwrap().contains("one_year"));
        assert!(held_badges(&harness.db, &bot_user.id).await.unwrap().is_empty());
        grant_time_badge(&harness.db, "one_year", bit, 1, later, None).await.unwrap();
    }
}
