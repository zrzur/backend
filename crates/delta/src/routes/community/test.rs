use revolt_database::{
    CreatorStanding, File, Metadata, PlatformPermission, PlatformRole, Session, User,
};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};

use crate::util::test::TestHarness;

async fn upload(harness: &TestHarness, user: &User) -> String {
    let id = TestHarness::rand_string();
    harness
        .db
        .insert_attachment(&File {
            id: id.clone(),
            tag: "decorations".to_string(),
            filename: "frame.png".to_string(),
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

async fn creator(harness: &TestHarness) -> (Session, User) {
    let (_, session, user) = harness.new_user().await;
    set_role(
        harness,
        &user,
        vec![
            PlatformPermission::CosmeticsSubmit,
            PlatformPermission::CosmeticsManageOwn,
        ],
        Some(CreatorStanding::Active),
    )
    .await;
    // the rank is 18+
    declare_age(harness, &user, 30).await;
    (session, user)
}

async fn reviewer(harness: &TestHarness) -> (Session, User) {
    let (_, session, user) = harness.new_user().await;
    set_role(
        harness,
        &user,
        vec![PlatformPermission::CosmeticsReview],
        None,
    )
    .await;
    (session, user)
}

async fn create_design(
    harness: &TestHarness,
    session: &Session,
    user: &User,
    name: &str,
    submit: bool,
) -> (Status, Option<v0::CommunityItem>) {
    let file = upload(harness, user).await;
    let res = harness
        .client
        .post("/community/decorations")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "name": name, "file": file, "submit": submit }).to_string())
        .dispatch()
        .await;
    let status = res.status();
    let body = if status == Status::Ok {
        res.into_json::<v0::CommunityItem>().await
    } else {
        None
    };
    (status, body)
}

/// Create and publish in one step, as the Creator Studio does
async fn submit_design(
    harness: &TestHarness,
    session: &Session,
    user: &User,
    name: &str,
) -> (Status, Option<v0::CommunityItem>) {
    create_design(harness, session, user, name, true).await
}

async fn public_list(harness: &TestHarness, session: &Session) -> v0::CommunityPage {
    harness
        .client
        .get("/community/decorations")
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await
        .into_json::<v0::CommunityPage>()
        .await
        .expect("page")
}

async fn act(
    harness: &TestHarness,
    session: &Session,
    id: &str,
    action: &str,
    reason: Option<&str>,
) -> Status {
    harness
        .client
        .post(format!("/community/items/{id}/{action}"))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "reason": reason }).to_string())
        .dispatch()
        .await
        .status()
}

async fn wear(harness: &TestHarness, session: &Session, id: &str) -> Status {
    harness
        .client
        .patch("/users/@me")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "style": { "decoration": format!("c:{id}") } }).to_string())
        .dispatch()
        .await
        .status()
}

fn listed(page: &v0::CommunityPage, id: &str) -> bool {
    page.items.iter().any(|i| i.id == id)
}

#[rocket::async_test]
async fn normal_user_cannot_create() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (status, _) = submit_design(&harness, &session, &user, "Mine").await;
    assert_eq!(status, Status::Forbidden);

    // Plus, feathers or a server role never matter: only platform roles do
    let res = harness
        .client
        .get("/community/mine")
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Forbidden);
}

#[rocket::async_test]
async fn revoked_creator_cannot_create_but_keeps_published() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (rs, _) = reviewer(&harness).await;

    let (status, item) = submit_design(&harness, &session, &user, "Ring").await;
    assert_eq!(status, Status::Ok);
    let item = item.unwrap();
    let (_, draft) = create_design(&harness, &session, &user, "Later", false).await;
    let draft = draft.unwrap();

    set_role(
        &harness,
        &user,
        vec![PlatformPermission::CosmeticsManageOwn],
        Some(CreatorStanding::Revoked),
    )
    .await;
    let (status, _) = submit_design(&harness, &session, &user, "Another").await;
    assert_eq!(status, Status::Forbidden);
    // nor publish a draft made before
    assert_eq!(
        act(&harness, &session, &draft.id, "submit", None).await,
        Status::Forbidden
    );

    let list = public_list(&harness, &rs).await;
    assert!(listed(&list, &item.id));
    assert!(!listed(&list, &draft.id));
}

#[rocket::async_test]
async fn reviewer_cannot_grant_creators() {
    let harness = TestHarness::new().await;
    let (rs, _) = reviewer(&harness).await;
    let (_, _, someone) = harness.new_user().await;
    let res = harness
        .client
        .post("/admin/creators")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", rs.token.clone()))
        .body(json!({ "user": someone.id, "action": "grant" }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Forbidden);

    // and can't hand out permissions either
    let res = harness
        .client
        .post("/admin/permissions")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", rs.token.clone()))
        .body(json!({ "user": someone.id, "permissions": ["cosmetics.submit"] }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Forbidden);
}

#[rocket::async_test]
async fn admin_grants_and_revokes_creators() {
    let harness = TestHarness::new().await;
    let (_, admin_session, admin) = harness.new_user().await;
    set_role(
        &harness,
        &admin,
        vec![PlatformPermission::CosmeticsManageCreators],
        None,
    )
    .await;
    let (_, session, user) = harness.new_user().await;

    let grant = |action: &'static str| {
        harness
            .client
            .post("/admin/creators")
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", admin_session.token.clone()))
            .body(json!({ "user": user.id, "action": action, "reason": "test" }).to_string())
            .dispatch()
    };
    assert_eq!(grant("grant").await.status(), Status::Ok);
    let (status, _) = submit_design(&harness, &session, &user, "Halo").await;
    assert_eq!(status, Status::Ok);

    assert_eq!(grant("revoke").await.status(), Status::Ok);
    let (status, _) = submit_design(&harness, &session, &user, "Halo 2").await;
    assert_eq!(status, Status::Forbidden);

    let history = harness.db.fetch_creator_logs(&user.id).await.unwrap();
    assert_eq!(history.len(), 2);
}

#[rocket::async_test]
async fn creators_publish_directly() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (rs, _) = reviewer(&harness).await;

    // submitting publishes straight away and logs it as the creator's own
    let (status, item) = submit_design(&harness, &session, &user, "Live").await;
    assert_eq!(status, Status::Ok);
    let item = item.unwrap();
    assert!(matches!(item.status, v0::CommunityItemStatus::Published));
    assert!(item.published_at.is_some());
    let log = harness.db.fetch_community_reviews(&item.id).await.unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].action, revolt_database::CommunityReviewAction::Publish);
    assert_eq!(log[0].actor_id, user.id);

    let page = public_list(&harness, &rs).await;
    assert!(listed(&page, &item.id));
    assert!(page.creators.iter().any(|c| c.id == user.id && c.verified));
    assert_eq!(wear(&harness, &rs, &item.id).await, Status::Ok);

    // a draft stays private until the creator publishes it
    let (_, draft) = create_design(&harness, &session, &user, "Draft", false).await;
    let draft = draft.unwrap();
    assert!(matches!(draft.status, v0::CommunityItemStatus::Draft));
    assert!(!listed(&public_list(&harness, &rs).await, &draft.id));
    assert_eq!(wear(&harness, &rs, &draft.id).await, Status::BadRequest);
    let res = harness
        .client
        .get("/community/decorations?query=Draft")
        .header(Header::new("x-session-token", rs.token.clone()))
        .dispatch()
        .await
        .into_json::<v0::CommunityPage>()
        .await
        .unwrap();
    assert!(res.items.is_empty());

    assert_eq!(act(&harness, &session, &draft.id, "submit", None).await, Status::Ok);
    assert!(listed(&public_list(&harness, &rs).await, &draft.id));
    // publishing twice does nothing
    assert_eq!(
        act(&harness, &session, &draft.id, "submit", None).await,
        Status::BadRequest
    );

    // only the creator publishes their own work
    let (other_session, other) = creator(&harness).await;
    let _ = other;
    let (_, mine) = create_design(&harness, &session, &user, "Mine", false).await;
    assert_eq!(
        act(&harness, &other_session, &mine.unwrap().id, "submit", None).await,
        Status::NotFound
    );
}

#[rocket::async_test]
async fn reviewers_can_still_take_designs_down() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (rs, _) = reviewer(&harness).await;
    let (_, item) = submit_design(&harness, &session, &user, "Loud").await;
    let item = item.unwrap();
    assert_eq!(wear(&harness, &rs, &item.id).await, Status::Ok);

    // a reason is required
    assert_eq!(
        act(&harness, &rs, &item.id, "suspend", None).await,
        Status::BadRequest
    );
    assert_eq!(
        act(&harness, &rs, &item.id, "suspend", Some("not original")).await,
        Status::Ok
    );
    assert!(!listed(&public_list(&harness, &session).await, &item.id));
    assert_eq!(wear(&harness, &session, &item.id).await, Status::BadRequest);

    // the creator can't put it back up; a reviewer can
    assert_eq!(
        act(&harness, &session, &item.id, "submit", None).await,
        Status::BadRequest
    );
    let mine = harness
        .client
        .get("/community/mine")
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await
        .into_json::<Vec<v0::CommunityItem>>()
        .await
        .unwrap();
    assert!(mine
        .iter()
        .any(|i| i.review_note.as_deref() == Some("not original")));
    assert_eq!(act(&harness, &rs, &item.id, "approve", None).await, Status::Ok);
    assert!(listed(&public_list(&harness, &session).await, &item.id));

    // an admin can archive it for good
    let (_, admin_session, admin) = harness.new_user().await;
    set_role(
        &harness,
        &admin,
        vec![PlatformPermission::CosmeticsManageAll],
        None,
    )
    .await;
    assert_eq!(
        act(&harness, &admin_session, &item.id, "archive", None).await,
        Status::Ok
    );
    assert!(!listed(&public_list(&harness, &session).await, &item.id));
}

#[rocket::async_test]
async fn concurrent_decisions_count_once() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (first, _) = reviewer(&harness).await;
    let (second, _) = reviewer(&harness).await;
    let (_, item) = create_design(&harness, &session, &user, "Race", false).await;
    let item = item.unwrap();

    // Two writes of the same version at the same moment: the conditional
    // update lets exactly one through. (Rocket's tracked local client can't
    // dispatch two requests at once, so the race is run against the
    // database the routes use.)
    let read = harness.db.fetch_community_item(&item.id).await.unwrap();
    let mut published = read.clone();
    published.status = revolt_database::CommunityItemStatus::Published;
    published.version = read.version + 1;
    let (a, b) = rocket::tokio::join!(
        harness.db.update_community_item(&published, read.version, false),
        harness.db.update_community_item(&published, read.version, false),
    );
    assert_eq!([a.unwrap(), b.unwrap()].iter().filter(|ok| **ok).count(), 1);

    // through the API: the first suspension wins, a second one is refused
    let (_, item) = submit_design(&harness, &session, &user, "Race 2").await;
    let item = item.unwrap();
    let a = act(&harness, &first, &item.id, "suspend", Some("one")).await;
    let b = act(&harness, &second, &item.id, "suspend", Some("two")).await;
    assert_eq!(a, Status::Ok);
    assert!(b == Status::Conflict || b == Status::BadRequest, "{b}");
    let suspensions = harness
        .db
        .fetch_community_reviews(&item.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.action == revolt_database::CommunityReviewAction::Suspend)
        .count();
    assert_eq!(suspensions, 1);
}

#[rocket::async_test]
async fn edits_go_live_when_published() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (rs, _) = reviewer(&harness).await;
    let (_, item) = submit_design(&harness, &session, &user, "Glow").await;
    let item = item.unwrap();
    assert_eq!(wear(&harness, &rs, &item.id).await, Status::Ok);

    let edit = |body: serde_json::Value| {
        harness
            .client
            .patch(format!("/community/items/{}", item.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(body.to_string())
            .dispatch()
    };

    // saved without publishing: the public version stays as it was
    let new_file = upload(&harness, &user).await;
    let res = edit(json!({ "file": new_file, "name": "Glow v2" })).await;
    assert_eq!(res.status(), Status::Ok);
    let page = public_list(&harness, &rs).await;
    let public = page.items.iter().find(|i| i.id == item.id).unwrap();
    assert_eq!(public.file.id, item.file.id);
    assert_eq!(public.name, "Glow");

    // published: live at once, and people wearing it get the new image
    assert_eq!(act(&harness, &session, &item.id, "submit", None).await, Status::Ok);
    let page = public_list(&harness, &rs).await;
    let public = page.items.iter().find(|i| i.id == item.id).unwrap();
    assert_eq!(public.file.id, new_file);
    assert_eq!(public.name, "Glow v2");
    assert_eq!(public.revision, 2);
    let wearer = harness.db.fetch_user(&rs.user_id).await.unwrap();
    let style = wearer.style.unwrap();
    assert_eq!(style.decoration_file.as_deref(), Some(new_file.as_str()));

    // edit and publish in one step
    let newer = upload(&harness, &user).await;
    assert_eq!(edit(json!({ "file": newer, "submit": true })).await.status(), Status::Ok);
    let page = public_list(&harness, &rs).await;
    let public = page.items.iter().find(|i| i.id == item.id).unwrap();
    assert_eq!(public.file.id, newer);
    assert_eq!(public.revision, 3);
}

#[rocket::async_test]
async fn reviewers_cannot_reinstate_their_own() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    set_role(
        &harness,
        &user,
        vec![
            PlatformPermission::CosmeticsSubmit,
            PlatformPermission::CosmeticsReview,
        ],
        Some(CreatorStanding::Active),
    )
    .await;
    let (rs, _) = reviewer(&harness).await;
    let (_, item) = submit_design(&harness, &session, &user, "Self").await;
    let item = item.unwrap();
    assert_eq!(
        act(&harness, &rs, &item.id, "suspend", Some("too bright")).await,
        Status::Ok
    );
    assert_eq!(
        act(&harness, &session, &item.id, "approve", None).await,
        Status::Forbidden
    );
}

/// The public Designs site: visitors without an account see published
/// designs, a design's page and its creator, never a draft
#[rocket::async_test]
async fn visitors_see_published_designs_only() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, live) = submit_design(&harness, &session, &user, "Live").await;
    let live = live.unwrap();
    let (_, draft) = create_design(&harness, &session, &user, "Draft", false).await;
    let draft = draft.unwrap();

    let page = harness
        .client
        .get("/community/decorations")
        .dispatch()
        .await
        .into_json::<v0::CommunityPage>()
        .await
        .expect("page");
    assert!(listed(&page, &live.id));
    assert!(!listed(&page, &draft.id));
    assert!(page.items.iter().all(|item| !item.favourite));

    let res = harness
        .client
        .get(format!("/community/items/{}", live.id))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    let details = res.into_json::<v0::CommunityItemDetails>().await.unwrap();
    assert!(details.history.is_empty() && details.reporters.is_empty());
    // creator names are off for visitors until the creator agrees
    assert!(details.creator.is_none());
    assert!(details.item.creator_id.is_empty());
    assert!(page.creators.is_empty());
    assert!(page.items.iter().all(|item| item.creator_id.is_empty()));

    let res = harness
        .client
        .get(format!("/community/items/{}", draft.id))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NotFound);

    let res = harness
        .client
        .get(format!("/community/creators/{}", user.id))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NotFound);
    let res = harness
        .client
        .get(format!("/community/decorations?creator={}", user.id))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NotFound);

    // the creator opts in: now the visitor sees the name and profile
    let res = harness
        .client
        .put("/community/creators/@me/public-name")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(r#"{"show_public_name":true}"#)
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NoContent);
    let res = harness
        .client
        .get(format!("/community/items/{}", live.id))
        .dispatch()
        .await;
    let details = res.into_json::<v0::CommunityItemDetails>().await.unwrap();
    assert_eq!(details.creator.map(|c| c.id), Some(user.id.clone()));
    let res = harness
        .client
        .get(format!("/community/creators/{}", user.id))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);

    // a non-creator cannot switch it on
    let (_, other_session, _) = harness.new_user().await;
    let res = harness
        .client
        .put("/community/creators/@me/public-name")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", other_session.token.clone()))
        .body(r#"{"show_public_name":true}"#)
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NotFound);

    // saving a favourite still needs an account
    let res = harness
        .client
        .put(format!("/community/items/{}/favourite", live.id))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Unauthorized);
}

async fn change_creator(harness: &TestHarness, user: &User, action: &str, reason: Option<&str>) {
    super::admin::creator_change(
        &harness.db,
        "01ACTOR0000000000000000000",
        v0::DataCreatorAction {
            user: user.id.clone(),
            action: action.to_string(),
            reason: reason.map(str::to_string),
        },
    )
    .await
    .expect("creator change");
}

async fn file_access(
    harness: &TestHarness,
    file_id: &str,
    viewer: Option<&User>,
) -> revolt_database::CommunityFileAccess {
    let file = harness.db.fetch_attachment("decorations", file_id).await.unwrap();
    revolt_database::community_file_access(&harness.db, &file, viewer)
        .await
        .unwrap()
}

async fn get_status(harness: &TestHarness, path: &str, session: Option<&Session>) -> Status {
    let mut request = harness.client.get(path.to_string());
    if let Some(session) = session {
        request = request.header(Header::new("x-session-token", session.token.clone()));
    }
    request.dispatch().await.status()
}

async fn standing_of(harness: &TestHarness, session: &Session) -> v0::CommunityCreatorStanding {
    harness
        .client
        .get("/community/creators/@me/standing")
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await
        .into_json()
        .await
        .expect("standing")
}

#[rocket::async_test]
async fn suspended_creators_designs_are_hidden_and_come_back() {
    use revolt_database::CommunityFileAccess::{Denied, Private, Public};

    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, other, other_user) = harness.new_user().await;
    let (_, item) = submit_design(&harness, &session, &user, "Ring").await;
    let item = item.unwrap();
    let details = format!("/community/items/{}", item.id);
    let profile = format!("/community/creators/{}", user.id);
    assert_eq!(wear(&harness, &other, &item.id).await, Status::Ok);
    assert_eq!(file_access(&harness, &item.file.id, None).await, Public);

    change_creator(&harness, &user, "suspend", Some("stolen art")).await;

    assert!(!listed(&public_list(&harness, &other).await, &item.id));
    assert_eq!(get_status(&harness, &details, Some(&other)).await, Status::NotFound);
    assert_eq!(get_status(&harness, &details, None).await, Status::NotFound);
    assert_eq!(get_status(&harness, &profile, Some(&other)).await, Status::NotFound);
    // the file answers nobody but its creator, and that answer is not stored
    assert_eq!(file_access(&harness, &item.file.id, None).await, Denied);
    assert_eq!(file_access(&harness, &item.file.id, Some(&other_user)).await, Denied);
    assert_eq!(file_access(&harness, &item.file.id, Some(&user)).await, Private);
    // nobody keeps wearing it, or can put it on
    let worn = harness.db.fetch_user(&other_user.id).await.unwrap();
    assert!(
        worn.style.as_ref().is_none_or(|s| s.decoration.is_none()),
        "still worn: {:?}",
        worn.style
    );
    assert_eq!(wear(&harness, &other, &item.id).await, Status::BadRequest);
    // saving it is refused too
    let res = harness
        .client
        .put(format!("/community/items/{}/favourite", item.id))
        .header(Header::new("x-session-token", other.token.clone()))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NotFound);
    // the creator still sees their own
    assert_eq!(get_status(&harness, &details, Some(&session)).await, Status::Ok);
    assert_eq!(get_status(&harness, "/community/mine", Some(&session)).await, Status::Ok);

    change_creator(&harness, &user, "reinstate", Some("cleared")).await;

    assert!(listed(&public_list(&harness, &other).await, &item.id));
    assert_eq!(get_status(&harness, &details, Some(&other)).await, Status::Ok);
    assert_eq!(file_access(&harness, &item.file.id, None).await, Public);
}

#[rocket::async_test]
async fn a_design_suspended_on_its_own_stays_down_after_reinstating() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (rs, _) = reviewer(&harness).await;
    let (_, item) = submit_design(&harness, &session, &user, "Ring").await;
    let item = item.unwrap();
    assert_eq!(act(&harness, &rs, &item.id, "suspend", Some("bad")).await, Status::Ok);

    change_creator(&harness, &user, "suspend", Some("x")).await;
    change_creator(&harness, &user, "reinstate", Some("y")).await;

    assert!(!listed(&public_list(&harness, &rs).await, &item.id));
}

#[rocket::async_test]
async fn suspended_creators_cannot_publish_edit_or_withdraw() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, draft) = create_design(&harness, &session, &user, "Later", false).await;
    let draft = draft.unwrap();
    let (_, live) = submit_design(&harness, &session, &user, "Live").await;
    let live = live.unwrap();
    change_creator(&harness, &user, "suspend", Some("stolen art")).await;

    let (status, _) = submit_design(&harness, &session, &user, "Another").await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(act(&harness, &session, &draft.id, "submit", None).await, Status::Forbidden);
    assert_eq!(act(&harness, &session, &live.id, "archive", None).await, Status::Forbidden);
    let edit = harness
        .client
        .patch(format!("/community/items/{}", draft.id))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "name": "Changed" }).to_string())
        .dispatch()
        .await;
    assert_eq!(edit.status(), Status::Forbidden);
}

#[rocket::async_test]
async fn suspending_needs_a_reason() {
    let harness = TestHarness::new().await;
    let (_, user) = creator(&harness).await;
    for reason in [None, Some("   ".to_string())] {
        let result = super::admin::creator_change(
            &harness.db,
            "01ACTOR0000000000000000000",
            v0::DataCreatorAction {
                user: user.id.clone(),
                action: "suspend".to_string(),
                reason,
            },
        )
        .await;
        assert!(result.is_err());
    }
}

#[rocket::async_test]
async fn suspended_creator_reads_the_reason_and_appeals() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, item) = submit_design(&harness, &session, &user, "Ring").await;
    let item = item.unwrap();

    let before = standing_of(&harness, &session).await;
    assert_eq!(before.standing, "Active");
    assert!(before.reason.is_none() && !before.can_appeal);

    change_creator(&harness, &user, "suspend", Some("stolen art")).await;
    let standing = standing_of(&harness, &session).await;
    assert_eq!(standing.standing, "Suspended");
    assert_eq!(standing.reason.as_deref(), Some("stolen art"));
    assert!(standing.can_appeal);
    let action_id = standing.action_id.expect("action to appeal against");

    let appeal = harness
        .client
        .post(format!("/safety/standing/{action_id}/appeal"))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "statement": "I drew this myself" }).to_string())
        .dispatch()
        .await;
    assert_eq!(appeal.status(), Status::Ok);
    let standing = standing_of(&harness, &session).await;
    assert!(!standing.can_appeal);
    assert_eq!(standing.appeal_status, Some(v0::SafetyAppealStatus::Pending));

    // an overturned appeal puts the creator and their designs back
    let action = harness.db.fetch_safety_action(&action_id).await.unwrap();
    let (_, _, staff) = harness.new_user().await;
    crate::routes::safety::actions::revoke_action(&harness.db, &staff, action, "overturned".into())
        .await
        .unwrap();
    assert_eq!(standing_of(&harness, &session).await.standing, "Active");
    assert_eq!(
        file_access(&harness, &item.file.id, None).await,
        revolt_database::CommunityFileAccess::Public
    );
}

#[rocket::async_test]
async fn reinstating_closes_the_suspension() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    change_creator(&harness, &user, "suspend", Some("stolen art")).await;
    let action_id = standing_of(&harness, &session).await.action_id.unwrap();
    change_creator(&harness, &user, "reinstate", Some("cleared")).await;
    let action = harness.db.fetch_safety_action(&action_id).await.unwrap();
    assert!(action.revoked.is_some());
}

#[rocket::async_test]
async fn only_the_creator_gets_a_download_link() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, other, _) = harness.new_user().await;
    let (_, item) = submit_design(&harness, &session, &user, "Ring").await;
    let item = item.unwrap();
    change_creator(&harness, &user, "suspend", Some("stolen art")).await;

    let path = format!("/community/items/{}/download", item.id);
    assert_eq!(get_status(&harness, &path, Some(&other)).await, Status::NotFound);
    let res = harness
        .client
        .get(path)
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    let copy: v0::CommunityItemDownload = res.into_json().await.expect("download");
    assert_eq!(copy.filename, "frame.png");
    assert!(copy.url.ends_with(&format!("/decorations/{}/original", item.file.id)), "{}", copy.url);
}

#[rocket::async_test]
async fn the_image_leaves_the_cdn_when_it_leaves_the_public() {
    use revolt_database::CommunityItemStatus::{Published, Suspended};

    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, item) = submit_design(&harness, &session, &user, "Ring").await;
    let before = harness.db.fetch_community_item(&item.unwrap().id).await.unwrap();
    assert_eq!(before.status, Published);

    let mut suspended = before.clone();
    suspended.status = Suspended;
    assert_eq!(super::left_public(&before, &suspended).map(|f| f.id), Some(before.file.id.clone()));

    let mut new_image = before.clone();
    new_image.file.id = "another".to_string();
    assert_eq!(super::left_public(&before, &new_image).map(|f| f.id), Some(before.file.id.clone()));

    let mut renamed = before.clone();
    renamed.name = "Band".to_string();
    assert!(super::left_public(&before, &renamed).is_none());
    assert!(super::left_public(&suspended, &before).is_none());
}

async fn show_name(harness: &TestHarness, session: &Session, show: bool) {
    let res = harness
        .client
        .put("/community/creators/@me/public-name")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "show_public_name": show }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NoContent);
}

#[rocket::async_test]
async fn the_public_never_gets_a_handle() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, other, _) = harness.new_user().await;
    let (_, item) = submit_design(&harness, &session, &user, "Ring").await;
    let item = item.unwrap();

    let raw = harness
        .client
        .get("/community/decorations")
        .header(Header::new("x-session-token", other.token.clone()))
        .dispatch()
        .await
        .into_string()
        .await
        .unwrap();
    assert!(!raw.contains("discriminator") && !raw.contains("\"username\""), "{raw}");
    assert!(!raw.contains(&format!("#{}", user.discriminator)), "{raw}");

    // no display name: the username, without the tag
    let page = public_list(&harness, &other).await;
    let card = page.creators.iter().find(|c| c.id == user.id).unwrap();
    assert_eq!(card.display_name, user.username);
    assert!(!card.open_page);

    let details = harness
        .client
        .get(format!("/community/items/{}", item.id))
        .header(Header::new("x-session-token", other.token.clone()))
        .dispatch()
        .await
        .into_string()
        .await
        .unwrap();
    assert!(!details.contains("discriminator"), "{details}");
}

#[rocket::async_test]
async fn a_display_name_goes_first() {
    let harness = TestHarness::new().await;
    let (session, mut user) = creator(&harness).await;
    user.update(
        &harness.db,
        revolt_database::PartialUser {
            display_name: Some("Layla".to_string()),
            ..Default::default()
        },
        vec![],
    )
    .await
    .unwrap();
    submit_design(&harness, &session, &user, "Ring").await;
    let page = public_list(&harness, &session).await;
    let card = page.creators.iter().find(|c| c.id == user.id).unwrap();
    assert_eq!(card.display_name, "Layla");
}

#[rocket::async_test]
async fn a_creators_page_opens_only_if_they_turn_it_on() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, other, _) = harness.new_user().await;
    submit_design(&harness, &session, &user, "Ring").await;
    let profile = format!("/community/creators/{}", user.id);
    let by_creator = format!("/community/decorations?creator={}", user.id);

    // off: nobody but the creator, whether signed in or not
    assert_eq!(get_status(&harness, &profile, None).await, Status::NotFound);
    assert_eq!(get_status(&harness, &profile, Some(&other)).await, Status::NotFound);
    assert_eq!(get_status(&harness, &by_creator, Some(&other)).await, Status::NotFound);
    assert_eq!(get_status(&harness, &profile, Some(&session)).await, Status::Ok);
    assert_eq!(get_status(&harness, &by_creator, Some(&session)).await, Status::Ok);
    // and searching their username finds nothing that links them to it
    let search = format!("/community/decorations?query={}", user.username);
    let found: v0::CommunityPage = harness
        .client
        .get(search)
        .header(Header::new("x-session-token", other.token.clone()))
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    assert!(found.items.is_empty());

    show_name(&harness, &session, true).await;
    assert_eq!(get_status(&harness, &profile, Some(&other)).await, Status::Ok);
    assert_eq!(get_status(&harness, &by_creator, Some(&other)).await, Status::Ok);
    let page = public_list(&harness, &other).await;
    assert!(page.creators.iter().find(|c| c.id == user.id).unwrap().open_page);
}

#[rocket::async_test]
async fn staff_keep_the_handle() {
    let harness = TestHarness::new().await;
    let (_, user) = creator(&harness).await;
    let record = super::admin::creator_change(
        &harness.db,
        "01ACTOR0000000000000000000",
        v0::DataCreatorAction {
            user: user.id.clone(),
            action: "reinstate".to_string(),
            reason: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(record.user.username, user.username);
    assert_eq!(record.user.discriminator, user.discriminator);
}

const TERMS: &str = revolt_database::CREATOR_TERMS_VERSION;

async fn upload_effect(harness: &TestHarness, user: &User) -> String {
    let id = upload(harness, user).await;
    let mut file = harness.db.fetch_attachment("decorations", &id).await.unwrap();
    let new_id = TestHarness::rand_string();
    file.id = new_id.clone();
    file.tag = "effects".to_string();
    file.filename = "effect.gif".to_string();
    harness.db.insert_attachment(&file).await.unwrap();
    new_id
}

async fn post_effect(
    harness: &TestHarness,
    session: &Session,
    file: &str,
    name: &str,
    rights: serde_json::Value,
    submit: bool,
) -> (Status, Option<v0::CommunityItem>) {
    let res = harness
        .client
        .post("/community/effects")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "name": name, "file": file, "submit": submit, "rights": rights }).to_string())
        .dispatch()
        .await;
    let status = res.status();
    let body = if status == Status::Ok {
        res.into_json::<v0::CommunityItem>().await
    } else {
        None
    };
    (status, body)
}

fn promise() -> serde_json::Value {
    json!({ "confirmed": true, "terms_version": TERMS, "ai_parts": "none" })
}

async fn approved_effect(harness: &TestHarness, session: &Session, user: &User, name: &str) -> v0::CommunityItem {
    let file = upload_effect(harness, user).await;
    let (status, item) = post_effect(harness, session, &file, name, promise(), true).await;
    assert_eq!(status, Status::Ok);
    let item = item.unwrap();
    let (boss, _) = reviewer(harness).await;
    assert_eq!(act(harness, &boss, &item.id, "approve", None).await, Status::Ok);
    item
}

async fn effects_list(harness: &TestHarness) -> v0::CommunityPage {
    harness
        .client
        .get("/community/effects")
        .dispatch()
        .await
        .into_json::<v0::CommunityPage>()
        .await
        .expect("page")
}

async fn wear_effect(harness: &TestHarness, session: &Session, id: &str) -> Status {
    harness
        .client
        .patch("/users/@me")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "style": { "effect": format!("c:{id}") } }).to_string())
        .dispatch()
        .await
        .status()
}

#[rocket::async_test]
async fn an_effect_waits_for_a_reviewer_then_anyone_can_wear_it() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (boss, _) = reviewer(&harness).await;
    let (_, wearer_session, wearer) = harness.new_user().await;

    let file = upload_effect(&harness, &user).await;
    let (status, item) = post_effect(&harness, &session, &file, "Snow", promise(), true).await;
    assert_eq!(status, Status::Ok);
    let item = item.unwrap();
    assert_eq!(item.status, v0::CommunityItemStatus::PendingReview);
    assert!(item.published_at.is_none());
    assert_eq!(item.rights.as_ref().unwrap().terms_version, TERMS);
    assert!(item.rights.as_ref().unwrap().confirmed_at > 0);
    assert!(!listed(&effects_list(&harness).await, &item.id));
    // not published yet: nobody can put it on
    assert_ne!(wear_effect(&harness, &wearer_session, &item.id).await, Status::Ok);

    assert_eq!(act(&harness, &boss, &item.id, "approve", None).await, Status::Ok);
    let page = effects_list(&harness).await;
    assert!(listed(&page, &item.id));
    // what the creator promised stays between them and the reviewers
    assert!(page.items[0].rights.is_none());

    assert_eq!(wear_effect(&harness, &wearer_session, &item.id).await, Status::Ok);
    let worn = harness.db.fetch_user(&wearer.id).await.unwrap();
    let style = worn.style.unwrap();
    assert_eq!(style.effect, Some(format!("c:{}", item.id)));
    assert_eq!(style.effect_file, Some(file));
    assert_eq!(harness.db.fetch_community_item(&item.id).await.unwrap().popularity, 1);
}

#[rocket::async_test]
async fn an_effect_needs_the_creators_promise() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;

    let file = upload_effect(&harness, &user).await;
    for rights in [
        serde_json::Value::Null,
        json!({ "confirmed": false, "terms_version": TERMS }),
        json!({ "confirmed": true, "terms_version": "1999-01-01" }),
    ] {
        let (status, _) = post_effect(&harness, &session, &file, "Snow", rights, true).await;
        assert_ne!(status, Status::Ok);
    }
    // a draft can wait for it, but can't be sent without it
    let (status, item) =
        post_effect(&harness, &session, &file, "Snow", serde_json::Value::Null, false).await;
    assert_eq!(status, Status::Ok);
    let item = item.unwrap();
    let res = harness
        .client
        .post(format!("/community/items/{}/submit", item.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_ne!(res.status(), Status::Ok);
}

#[rocket::async_test]
async fn a_new_file_needs_a_new_promise() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (boss, _) = reviewer(&harness).await;
    let file = upload_effect(&harness, &user).await;
    let (_, item) = post_effect(&harness, &session, &file, "Snow", promise(), true).await;
    let item = item.unwrap();
    act(&harness, &boss, &item.id, "approve", None).await;

    let new_file = upload_effect(&harness, &user).await;
    let refused = harness
        .client
        .patch(format!("/community/items/{}", item.id))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "file": new_file, "submit": true }).to_string())
        .dispatch()
        .await;
    assert_ne!(refused.status(), Status::Ok);
    let sent = harness
        .client
        .patch(format!("/community/items/{}", item.id))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "file": new_file, "submit": true, "rights": promise() }).to_string())
        .dispatch()
        .await;
    assert_eq!(sent.status(), Status::Ok);
    // the public keeps the old file until a reviewer approves the new one
    let stored = harness.db.fetch_community_item(&item.id).await.unwrap();
    assert_eq!(stored.file.id, file);
    assert_eq!(stored.status, revolt_database::CommunityItemStatus::Published);
    assert_eq!(stored.pending.unwrap().status, revolt_database::CommunityItemStatus::PendingReview);

    assert_eq!(act(&harness, &boss, &item.id, "approve", None).await, Status::Ok);
    let stored = harness.db.fetch_community_item(&item.id).await.unwrap();
    assert_eq!(stored.file.id, new_file);
}

#[rocket::async_test]
async fn an_effect_only_takes_a_file_from_the_effects_tag() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let decoration_file = upload(&harness, &user).await;
    let (status, _) = post_effect(&harness, &session, &decoration_file, "Snow", promise(), true).await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn an_effect_cannot_use_blocked_names() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    for name in ["Zrzur snow", "زرزور", "Official rain", "Staff glow"] {
        let file = upload_effect(&harness, &user).await;
        let (status, _) = post_effect(&harness, &session, &file, name, promise(), true).await;
        assert_ne!(status, Status::Ok, "{name}");
    }
    let file = upload_effect(&harness, &user).await;
    let res = harness
        .client
        .post("/community/effects")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "name": "Snow", "file": file, "tags": ["discord"] }).to_string())
        .dispatch()
        .await;
    assert_ne!(res.status(), Status::Ok);
}

#[rocket::async_test]
async fn taking_an_effect_down_tells_the_creator_who_can_appeal() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (boss, _) = reviewer(&harness).await;
    let (_, wearer_session, wearer) = harness.new_user().await;
    let item = approved_effect(&harness, &session, &user, "Snow").await;
    assert_eq!(wear_effect(&harness, &wearer_session, &item.id).await, Status::Ok);

    assert_eq!(act(&harness, &boss, &item.id, "suspend", Some("Breaks the rules")).await, Status::Ok);

    let actions = harness.db.fetch_safety_actions_for_user(&user.id).await.unwrap();
    assert_eq!(actions.len(), 1);
    let action = &actions[0];
    assert_eq!(action.kind, v0::SafetyActionKind::SuspendCommunityItem);
    assert!(action.appealable);
    assert!(!action.automated_decision);
    assert_eq!(action.public_reason, "Breaks the rules");
    assert!(action.case_id.is_some());

    let worn = harness.db.fetch_user(&wearer.id).await.unwrap();
    assert!(worn.style.and_then(|style| style.effect).is_none());
    assert!(!listed(&effects_list(&harness).await, &item.id));
}

#[rocket::async_test]
async fn five_reports_hide_an_effect_for_now_and_say_it_was_automatic() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let item = approved_effect(&harness, &session, &user, "Snow").await;

    for _ in 0..revolt_database::COMMUNITY_REPORTS_TO_HIDE {
        let (_, reporter, _) = harness.new_user().await;
        let res = harness
            .client
            .post(format!("/community/items/{}/report", item.id))
            .header(Header::new("x-session-token", reporter.token.clone()))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::NoContent);
    }

    let actions = harness.db.fetch_safety_actions_for_user(&user.id).await.unwrap();
    assert_eq!(actions.len(), 1);
    assert!(actions[0].automated_decision && actions[0].automated_detection);
    let case = harness.db.fetch_safety_case(actions[0].case_id.as_ref().unwrap()).await.unwrap();
    assert!(case.status.is_open(), "a person still has to look");
    assert_eq!(case.priority, v0::SafetyPriority::Urgent);
}

#[rocket::async_test]
async fn decorations_still_go_live_at_once_and_ask_for_no_promise() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (status, item) = submit_design(&harness, &session, &user, "Official ring").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(item.unwrap().status, v0::CommunityItemStatus::Published);
}

#[rocket::async_test]
async fn a_suspended_creator_cannot_send_or_change_an_effect() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let file = upload_effect(&harness, &user).await;
    let (_, draft) = post_effect(&harness, &session, &file, "Snow", promise(), false).await;
    let draft = draft.unwrap();

    change_creator(&harness, &user, "suspend", Some("Rules")).await;

    let other = upload_effect(&harness, &user).await;
    let (status, _) = post_effect(&harness, &session, &other, "Rain", promise(), true).await;
    assert_ne!(status, Status::Ok);
    let res = harness
        .client
        .post(format!("/community/items/{}/submit", draft.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_ne!(res.status(), Status::Ok);
    // they still see their own work
    let res = harness
        .client
        .get("/community/mine")
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
}

async fn profile_of(harness: &TestHarness, session: &Session, id: &str) -> v0::UserProfile {
    harness
        .client
        .get(format!("/users/{id}/profile"))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await
        .into_json::<v0::UserProfile>()
        .await
        .expect("profile")
}

async fn creations_of(harness: &TestHarness, session: &Session, id: &str) -> (Status, String) {
    let res = harness
        .client
        .get(format!("/community/creators/{id}/designs"))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    (res.status(), res.into_string().await.unwrap_or_default())
}

async fn friends(harness: &TestHarness, user: &User, other: &User) {
    let mut a = harness.db.fetch_user(&user.id).await.unwrap();
    let mut b = harness.db.fetch_user(&other.id).await.unwrap();
    #[allow(clippy::disallowed_methods)]
    a.apply_relationship(
        &harness.db,
        &mut b,
        revolt_database::RelationshipStatus::Friend,
        revolt_database::RelationshipStatus::Friend,
    )
    .await
    .unwrap();
}

#[rocket::async_test]
async fn a_closed_page_keeps_their_creations_and_its_name_closed() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, viewer_session, viewer) = harness.new_user().await;
    friends(&harness, &user, &viewer).await;
    let (_, item) = submit_design(&harness, &session, &user, "Ring").await;
    let item = item.unwrap();

    // the page stays closed, as it does for anyone who never opened it
    let page = harness
        .client
        .get(format!("/community/creators/{}", user.id))
        .header(Header::new("x-session-token", viewer_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(page.status(), Status::NotFound);

    assert!(profile_of(&harness, &viewer_session, &user.id).await.creator);
    assert!(profile_of(&harness, &session, &user.id).await.creator);
    let (status, body) = creations_of(&harness, &viewer_session, &user.id).await;
    assert_eq!(status, Status::Ok);
    let items: Vec<v0::CommunityItem> = serde_json::from_str(&body).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, item.id);
    // nothing but the designs: no name, no avatar, no open-page flag
    for word in ["display_name", "username", "avatar", "open_page"] {
        assert!(!body.contains(word), "{word} in {body}");
    }
}

#[rocket::async_test]
async fn only_an_active_creator_counts_on_a_profile() {
    let harness = TestHarness::new().await;
    let (_session, user) = creator(&harness).await;
    let (_, viewer_session, viewer) = harness.new_user().await;
    friends(&harness, &user, &viewer).await;

    change_creator(&harness, &user, "suspend", Some("Rules")).await;
    assert!(!profile_of(&harness, &viewer_session, &user.id).await.creator);
    assert_eq!(creations_of(&harness, &viewer_session, &user.id).await.0, Status::NotFound);

    change_creator(&harness, &user, "reinstate", None).await;
    assert!(profile_of(&harness, &viewer_session, &user.id).await.creator);

    change_creator(&harness, &user, "revoke", None).await;
    assert!(!profile_of(&harness, &viewer_session, &user.id).await.creator);
    assert_eq!(creations_of(&harness, &viewer_session, &user.id).await.0, Status::NotFound);

    let (_, plain_session, plain) = harness.new_user().await;
    assert!(!profile_of(&harness, &plain_session, &plain.id).await.creator);
}

#[rocket::async_test]
async fn a_minors_creations_stop_where_their_links_stop() {
    let harness = TestHarness::new().await;
    let (_session, user) = creator(&harness).await;
    let (_, friend_session, friend) = harness.new_user().await;
    let (_, stranger_session, _stranger) = harness.new_user().await;
    friends(&harness, &user, &friend).await;

    // no stated age counts as a minor, the safe side
    assert!(profile_of(&harness, &friend_session, &user.id).await.creator);
    let seen = harness
        .client
        .get(format!("/users/{}/profile", user.id))
        .header(Header::new("x-session-token", stranger_session.token.clone()))
        .dispatch()
        .await;
    if seen.status() == Status::Ok {
        let profile = seen.into_json::<v0::UserProfile>().await.unwrap();
        assert!(!profile.creator);
    }
    assert_ne!(creations_of(&harness, &stranger_session, &user.id).await.0, Status::Ok);
}

/// Someone of this age, by the month and year of birth they declared
async fn declare_age(harness: &TestHarness, user: &User, age: i32) {
    let now = iso8601_timestamp::Timestamp::now_utc();
    let mut account = harness.db.fetch_account(&user.id).await.unwrap();
    account.age = Some(revolt_database::AccountAge::declare((
        now.year() - age - 1,
        u8::from(now.month()),
    )));
    account.save(&harness.db).await.unwrap();
}

fn says_adults_only(error: revolt_result::Error) {
    match error.error_type {
        revolt_result::ErrorType::FailedValidation { fields, .. } => {
            assert_eq!(fields[0].rule, "adult");
        }
        other => panic!("refused with {other:?}"),
    }
}

async fn manager(harness: &TestHarness) -> (Session, User) {
    let (_, session, user) = harness.new_user().await;
    set_role(harness, &user, vec![PlatformPermission::CosmeticsManageAll], None).await;
    (session, user)
}

#[rocket::async_test]
async fn only_a_declared_adult_can_be_given_the_rank() {
    let harness = TestHarness::new().await;
    let (_, _, teen) = harness.new_user().await;
    declare_age(&harness, &teen, 17).await;
    let (_, _, unknown) = harness.new_user_without_age().await;
    let (admin, _) = manager(&harness).await;

    for user in [&teen, &unknown] {
        for action in ["grant", "reinstate"] {
            let error = super::admin::creator_change(
                &harness.db,
                "01ACTOR0000000000000000000",
                v0::DataCreatorAction {
                    user: user.id.clone(),
                    action: action.to_string(),
                    reason: None,
                },
            )
            .await
            .expect_err("a minor is refused");
            says_adults_only(error);
        }
        // the permission route is a way to the same rank
        let res = harness
            .client
            .post("/admin/permissions")
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", admin.token.clone()))
            .body(json!({ "user": user.id, "permissions": ["cosmetics.submit"] }).to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::BadRequest);
        assert!(harness.db.fetch_platform_role(&user.id).await.unwrap().is_none());
    }

    // eighteen is enough
    let (_, _, adult) = harness.new_user().await;
    declare_age(&harness, &adult, 18).await;
    change_creator(&harness, &adult, "grant", None).await;
}

#[rocket::async_test]
async fn a_minor_who_has_the_rank_cannot_publish() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (status, live) = submit_design(&harness, &session, &user, "Older design").await;
    assert_eq!(status, Status::Ok);
    let live = live.unwrap();
    let (_, draft) = create_design(&harness, &session, &user, "Draft", false).await;
    let draft = draft.unwrap();
    declare_age(&harness, &user, 17).await;

    assert_eq!(
        create_design(&harness, &session, &user, "New design", true).await.0,
        Status::Forbidden
    );
    let submit = harness
        .client
        .post(format!("/community/items/{}/submit", draft.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(submit.status(), Status::Forbidden);
    let edit = harness
        .client
        .patch(format!("/community/items/{}", live.id))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "name": "Changed" }).to_string())
        .dispatch()
        .await;
    assert_eq!(edit.status(), Status::Forbidden);

    // uploads ask the same question, and the app is told
    let fresh = harness.db.fetch_user(&user.id).await.unwrap();
    let perms = revolt_database::platform_permissions(&harness.db, &fresh).await.unwrap();
    assert!(!perms.has(PlatformPermission::CosmeticsSubmit));
    assert!(perms.has(PlatformPermission::CosmeticsManageOwn));
    let told = harness
        .client
        .get("/community/permissions")
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await
        .into_json::<v0::CommunityPermissions>()
        .await
        .unwrap();
    assert!(!told.permissions.iter().any(|p| p == "cosmetics.submit"));
}

#[rocket::async_test]
async fn no_stated_age_cannot_publish_either() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user_without_age().await;
    set_role(
        &harness,
        &user,
        vec![PlatformPermission::CosmeticsSubmit, PlatformPermission::CosmeticsManageOwn],
        Some(CreatorStanding::Active),
    )
    .await;
    let perms = revolt_database::platform_permissions(&harness.db, &user).await.unwrap();
    assert!(!perms.has(PlatformPermission::CosmeticsSubmit));
    // the session itself is asked for an age first: the rule above still holds
    assert_ne!(create_design(&harness, &session, &user, "Design", true).await.0, Status::Ok);
}

/// A creator with one design in each state that matters: live, worn and
/// saved with a new image still waiting, sent to a reviewer, and a draft
struct Erasable {
    user: User,
    wearer: User,
    items: Vec<String>,
    files: Vec<String>,
}

async fn erasable(harness: &TestHarness) -> Erasable {
    let (session, user) = creator(harness).await;
    let (_, wearer_session, wearer) = harness.new_user().await;

    let live = submit_design(harness, &session, &user, "Live").await.1.unwrap();
    let old_file = live.file.id.clone();
    let new_file = upload(harness, &user).await;
    let res = harness
        .client
        .patch(format!("/community/items/{}", live.id))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "file": new_file }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(wear(harness, &wearer_session, &live.id).await, Status::Ok);
    let res = harness
        .client
        .put(format!("/community/items/{}/favourite", live.id))
        .header(Header::new("x-session-token", wearer_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NoContent);

    let effect = upload_effect(harness, &user).await;
    let (status, pending) = post_effect(harness, &session, &effect, "Waiting", promise(), true).await;
    assert_eq!(status, Status::Ok);
    let pending = pending.unwrap();
    assert_eq!(pending.status, v0::CommunityItemStatus::PendingReview);
    let draft = create_design(harness, &session, &user, "Draft", false).await.1.unwrap();

    Erasable {
        user,
        wearer,
        files: vec![old_file, new_file, effect, draft.file.id.clone()],
        items: vec![live.id, pending.id, draft.id],
    }
}

async fn assert_erased(harness: &TestHarness, what: &Erasable) {
    for id in &what.items {
        assert!(harness.db.fetch_community_item(id).await.is_err(), "{id} stayed");
        assert!(harness.db.fetch_community_reviews(id).await.unwrap().is_empty());
    }
    let released = harness.db.fetch_deleted_attachments().await.unwrap();
    for id in &what.files {
        assert!(released.iter().any(|file| &file.id == id), "{id} not released");
    }
    assert!(harness
        .db
        .fetch_community_items(revolt_database::CommunityItemQuery {
            creator_id: Some(&what.user.id),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap()
        .is_empty());

    let worn = harness.db.fetch_user(&what.wearer.id).await.unwrap();
    assert!(worn
        .style
        .is_none_or(|style| style.decoration.is_none() && style.decoration_file.is_none()));
    assert!(harness.db.fetch_community_favourites(&what.wearer.id).await.unwrap().is_empty());
}

#[rocket::async_test]
async fn erasing_an_account_removes_its_designs_and_their_files() {
    let harness = TestHarness::new().await;
    let what = erasable(&harness).await;

    // what the crond task does for an account that is due
    let mut account = harness.db.fetch_account(&what.user.id).await.unwrap();
    let mut user = harness.db.fetch_user(&what.user.id).await.unwrap();
    user.delete(&harness.db).await.unwrap();
    account.mark_deleted(&harness.db).await.unwrap();
    assert_erased(&harness, &what).await;
    let role = harness.db.fetch_platform_role(&what.user.id).await.unwrap().unwrap();
    assert!(role.creator.is_none() && role.permissions.is_empty() && !role.show_public_name);

    // crond tries again after a failure further on: nothing goes wrong
    account.mark_deleted(&harness.db).await.unwrap();
    assert_erased(&harness, &what).await;
}

#[rocket::async_test]
async fn a_full_erasure_removes_the_designs_too() {
    let harness = TestHarness::new().await;
    let what = erasable(&harness).await;
    let mut account = harness.db.fetch_account(&what.user.id).await.unwrap();
    account.erase_completely(&harness.db).await.unwrap();
    assert_erased(&harness, &what).await;
}

#[rocket::async_test]
async fn a_takedown_of_an_erased_design_can_still_be_undone() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (boss, boss_user) = reviewer(&harness).await;
    let item = approved_effect(&harness, &session, &user, "Snow").await;
    assert_eq!(act(&harness, &boss, &item.id, "suspend", Some("Rules")).await, Status::Ok);
    let action = harness.db.fetch_safety_actions_for_user(&user.id).await.unwrap().remove(0);
    let mut account = harness.db.fetch_account(&user.id).await.unwrap();
    account.mark_deleted(&harness.db).await.unwrap();

    crate::routes::safety::actions::revoke_action(&harness.db, &boss_user, action, "Mistake".to_string())
        .await
        .expect("nothing left to put back");
}

async fn post_kind(harness: &TestHarness, session: &Session, user: &User, kind: &str) -> v0::CommunityItem {
    let file = upload(harness, user).await;
    let res = harness
        .client
        .post(format!("/community/{kind}"))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "name": "Frame", "file": file, "submit": true }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    res.into_json::<v0::CommunityItem>().await.unwrap()
}

#[rocket::async_test]
async fn taking_any_design_down_comes_with_a_statement_and_an_appeal() {
    let harness = TestHarness::new().await;
    for kind in ["decorations", "nameplates"] {
        let (session, user) = creator(&harness).await;
        let (boss, _) = reviewer(&harness).await;
        let item = post_kind(&harness, &session, &user, kind).await;

        assert_eq!(act(&harness, &boss, &item.id, "suspend", Some("Breaks the rules")).await, Status::Ok);

        let actions = harness.db.fetch_safety_actions_for_user(&user.id).await.unwrap();
        assert_eq!(actions.len(), 1, "{kind}");
        let action = &actions[0];
        assert_eq!(action.kind, v0::SafetyActionKind::SuspendCommunityItem);
        assert!(action.appealable && !action.automated_decision);
        assert_eq!(action.public_reason, "Breaks the rules");
        let case = harness.db.fetch_safety_case(action.case_id.as_ref().unwrap()).await.unwrap();
        assert_eq!(case.status, v0::SafetyStatus::ActionTaken);
    }
}

#[test]
fn wearers_are_told_what_left_their_profile() {
    use crate::routes::safety::actions::wearer_notice;
    use revolt_database::CommunityItemKind::{Decoration, Effect, Nameplate};
    for language in ["en", "ar", "de"] {
        let titles: std::collections::HashSet<_> = [Decoration, Nameplate, Effect]
            .iter()
            .map(|kind| wearer_notice(kind).0[language].clone())
            .collect();
        assert_eq!(titles.len(), 3, "{language}");
    }
    assert!(wearer_notice(&Nameplate).1["en"].contains("nameplate"));
    assert!(wearer_notice(&Decoration).1["de"].contains("Avatar-Rahmen"));
    assert!(wearer_notice(&Effect).1["ar"].contains("المؤثر"));
}

#[rocket::async_test]
async fn five_reports_on_a_decoration_stay_an_open_case() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let item = post_kind(&harness, &session, &user, "decorations").await;
    for _ in 0..revolt_database::COMMUNITY_REPORTS_TO_HIDE {
        let (_, reporter, _) = harness.new_user().await;
        harness
            .client
            .post(format!("/community/items/{}/report", item.id))
            .header(Header::new("x-session-token", reporter.token.clone()))
            .dispatch()
            .await;
    }
    let actions = harness.db.fetch_safety_actions_for_user(&user.id).await.unwrap();
    assert_eq!(actions.len(), 1);
    assert!(actions[0].automated_decision && actions[0].automated_detection);
    let case = harness.db.fetch_safety_case(actions[0].case_id.as_ref().unwrap()).await.unwrap();
    assert!(case.status.is_open(), "a person still has to look");
}

async fn edit_design(harness: &TestHarness, session: &Session, id: &str, body: serde_json::Value) -> Status {
    harness
        .client
        .patch(format!("/community/items/{id}"))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(body.to_string())
        .dispatch()
        .await
        .status()
}

#[rocket::async_test]
async fn changing_a_waiting_effect_takes_it_out_of_review() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (boss, _) = reviewer(&harness).await;
    let file = upload_effect(&harness, &user).await;
    let (_, item) = post_effect(&harness, &session, &file, "Snow", promise(), true).await;
    let id = item.unwrap().id;

    assert_eq!(edit_design(&harness, &session, &id, json!({ "name": "Other" })).await, Status::Ok);
    let item = harness.db.fetch_community_item(&id).await.unwrap();
    assert_eq!(item.status, revolt_database::CommunityItemStatus::Draft);
    assert_ne!(act(&harness, &boss, &id, "approve", None).await, Status::Ok);

    // sending it again puts the new content in front of a reviewer
    assert_eq!(act(&harness, &session, &id, "submit", None).await, Status::Ok);
    assert_eq!(act(&harness, &boss, &id, "approve", None).await, Status::Ok);
}

#[rocket::async_test]
async fn changing_a_waiting_new_version_takes_it_out_of_review() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (boss, _) = reviewer(&harness).await;
    let item = approved_effect(&harness, &session, &user, "Snow").await;

    let sent = json!({ "name": "Frost", "submit": true });
    assert_eq!(edit_design(&harness, &session, &item.id, sent).await, Status::Ok);
    let later = json!({ "name": "Unseen" });
    assert_eq!(edit_design(&harness, &session, &item.id, later).await, Status::Ok);

    let stored = harness.db.fetch_community_item(&item.id).await.unwrap();
    assert_eq!(stored.pending.unwrap().status, revolt_database::CommunityItemStatus::Draft);
    assert_ne!(act(&harness, &boss, &item.id, "approve", None).await, Status::Ok);
    let stored = harness.db.fetch_community_item(&item.id).await.unwrap();
    assert_eq!(stored.name, "Snow");
}

#[rocket::async_test]
async fn an_effect_without_the_promise_is_not_approved() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (boss, _) = reviewer(&harness).await;
    let file = upload_effect(&harness, &user).await;
    let (_, item) = post_effect(&harness, &session, &file, "Snow", promise(), true).await;
    let id = item.unwrap().id;

    let mut stored = harness.db.fetch_community_item(&id).await.unwrap();
    let version = stored.version;
    stored.rights = None;
    stored.version += 1;
    assert!(harness.db.update_community_item(&stored, version, false).await.unwrap());

    assert_eq!(act(&harness, &boss, &id, "approve", None).await, Status::BadRequest);
    let stored = harness.db.fetch_community_item(&id).await.unwrap();
    assert_eq!(stored.status, revolt_database::CommunityItemStatus::PendingReview);
}

#[rocket::async_test]
async fn hiding_the_creations_section_hides_them_and_the_rank() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, viewer_session, viewer) = harness.new_user().await;
    friends(&harness, &user, &viewer).await;
    assert_eq!(submit_design(&harness, &session, &user, "Ring").await.0, Status::Ok);
    assert!(profile_of(&harness, &viewer_session, &user.id).await.creator);

    let res = harness
        .client
        .patch("/users/@me")
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "profile": { "hidden_sections": ["creations"] } }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);

    assert!(!profile_of(&harness, &viewer_session, &user.id).await.creator);
    assert_eq!(creations_of(&harness, &viewer_session, &user.id).await.0, Status::NotFound);
    // the owner still has both
    assert!(profile_of(&harness, &session, &user.id).await.creator);
    assert_eq!(creations_of(&harness, &session, &user.id).await.0, Status::Ok);
}

#[rocket::async_test]
async fn a_holder_under_18_is_not_shown_as_a_creator() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (_, viewer_session, _) = harness.new_user().await;
    declare_age(&harness, &user, 30).await;
    let (_, item) = submit_design(&harness, &session, &user, "Ring").await;
    let id = item.unwrap().id;
    let mut role = harness.db.fetch_platform_role(&user.id).await.unwrap().unwrap();
    role.show_public_name = true;
    harness.db.save_platform_role(&role).await.unwrap();

    let page = |token: String| {
        let client = &harness.client;
        let path = format!("/community/creators/{}", user.id);
        async move {
            client
                .get(path)
                .header(Header::new("x-session-token", token))
                .dispatch()
                .await
                .status()
        }
    };
    assert_eq!(page(viewer_session.token.clone()).await, Status::Ok);
    let shown = public_list(&harness, &viewer_session).await;
    let card = shown.creators.iter().find(|c| c.id == user.id).unwrap();
    assert!(card.verified && card.open_page);

    declare_age(&harness, &user, 17).await;
    assert_eq!(page(viewer_session.token.clone()).await, Status::NotFound);
    let shown = public_list(&harness, &viewer_session).await;
    assert!(listed(&shown, &id));
    let card = shown.creators.iter().find(|c| c.id == user.id).unwrap();
    assert!(!card.verified && !card.open_page);
}

#[rocket::async_test]
async fn the_promise_is_kept_when_given_by_an_edit_and_dropped_with_the_file() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (boss, _) = reviewer(&harness).await;
    let file = upload_effect(&harness, &user).await;
    let (status, item) = post_effect(&harness, &session, &file, "Snow", json!(null), false).await;
    assert_eq!(status, Status::Ok);
    let id = item.unwrap().id;

    let promised = json!({ "rights": promise() });
    assert_eq!(edit_design(&harness, &session, &id, promised).await, Status::Ok);
    let stored = harness.db.fetch_community_item(&id).await.unwrap();
    assert!(stored.rights.is_some());

    // the old promise was about the old file
    let new_file = upload_effect(&harness, &user).await;
    let changed = json!({ "file": new_file });
    assert_eq!(edit_design(&harness, &session, &id, changed).await, Status::Ok);
    let stored = harness.db.fetch_community_item(&id).await.unwrap();
    assert!(stored.rights.is_none());

    // promised again and sent, a reviewer can approve it
    let sent = json!({ "rights": promise(), "submit": true });
    assert_eq!(edit_design(&harness, &session, &id, sent).await, Status::Ok);
    assert_eq!(act(&harness, &boss, &id, "approve", None).await, Status::Ok);
}

async fn delete(harness: &TestHarness, session: &Session, id: &str) -> Status {
    harness
        .client
        .delete(format!("/community/items/{id}"))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await
        .status()
}

/// A creator can delete a design they withdrew, and its file goes with it
/// (Creator Terms, CT-2). A published or taken-down design is withdrawn or
/// decided first; nobody else can delete it.
#[rocket::async_test]
async fn a_creator_deletes_a_withdrawn_design() {
    let harness = TestHarness::new().await;
    let (session, user) = creator(&harness).await;
    let (other_session, _) = creator(&harness).await;
    let (_, live) = submit_design(&harness, &session, &user, "Ring").await;
    let live = live.unwrap();

    // still published: withdraw first
    assert_eq!(delete(&harness, &session, &live.id).await, Status::BadRequest);
    assert_eq!(act(&harness, &session, &live.id, "archive", None).await, Status::Ok);
    // not someone else's to delete
    assert_eq!(delete(&harness, &other_session, &live.id).await, Status::NotFound);

    assert_eq!(delete(&harness, &session, &live.id).await, Status::NoContent);
    assert!(harness.db.fetch_community_item(&live.id).await.is_err());
    let file = harness.db.fetch_attachment("decorations", &live.file.id).await;
    assert!(file.map_or(true, |file| file.deleted == Some(true)), "the file goes too");
    assert_eq!(delete(&harness, &session, &live.id).await, Status::NotFound);

    // a draft that was never public can go straight away
    let (_, draft) = create_design(&harness, &session, &user, "Sketch", false).await;
    assert_eq!(delete(&harness, &session, &draft.unwrap().id).await, Status::NoContent);

    // a design a reviewer took down stays until it is decided
    let (rs, _) = reviewer(&harness).await;
    let (_, taken) = submit_design(&harness, &session, &user, "Taken").await;
    let taken = taken.unwrap();
    assert_eq!(act(&harness, &rs, &taken.id, "suspend", Some("reported")).await, Status::Ok);
    assert_eq!(delete(&harness, &session, &taken.id).await, Status::BadRequest);

    // nothing changes while the creator is suspended
    let (_, sketch) = create_design(&harness, &session, &user, "Later", false).await;
    change_creator(&harness, &user, "suspend", Some("stolen art")).await;
    assert_eq!(delete(&harness, &session, &sketch.unwrap().id).await, Status::Forbidden);
}
