//! Join requests for servers whose access is `Approval`.
//!
//! A request is created by `POST /invites/<code>` (see invite_join.rs). Here
//! people with Manage Join Requests list, approve and reject them, and the
//! applicant reads or cancels their own request. Every check is on the
//! server; nothing depends on what the client shows.

use futures::future::join_all;
use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::util::reference::Reference;
use revolt_database::{Database, JoinRequestStatus, Member, User};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, ErrorType, Result};
use rocket::serde::json::Json;
use rocket::State;
use rocket_empty::EmptyResponse;

async fn require_reviewer(db: &Database, user: &User, server: &revolt_database::Server) -> Result<()> {
    let mut query = DatabasePermissionQuery::new(db, user).server(server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageJoinRequests)
}

/// # Fetch Join Requests
///
/// Pending requests to join this server, oldest first.
#[openapi(tag = "Server Members")]
#[get("/<target>/join_requests")]
pub async fn list(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::JoinRequestList>> {
    let server = target.as_server(db).await?;
    require_reviewer(db, &user, &server).await?;

    let requests = db
        .fetch_join_requests(&server.id, JoinRequestStatus::Pending)
        .await?;
    let ids: Vec<String> = requests.iter().map(|r| r.id.user.clone()).collect();
    let users = join_all(
        db.fetch_users(&ids)
            .await?
            .into_iter()
            .map(|u| u.into_listed()),
    )
    .await;

    Ok(Json(v0::JoinRequestList {
        requests: requests.into_iter().map(Into::into).collect(),
        users: users.into_iter().map(Into::into).collect(),
    }))
}

/// # Approve Join Request
///
/// Make the applicant a member. Fails if the request is no longer pending
/// (already decided by someone else) or the person can't join (banned,
/// server limit, age).
#[openapi(tag = "Server Members")]
#[post("/<target>/join_requests/<applicant>/approve")]
pub async fn approve(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    applicant: Reference<'_>,
) -> Result<EmptyResponse> {
    let server = target.as_server(db).await?;
    require_reviewer(db, &user, &server).await?;

    let request = db.fetch_join_request(&server.id, applicant.id).await?;
    if request.status != JoinRequestStatus::Pending {
        return Err(create_error!(InvalidOperation));
    }

    let person = db.fetch_user(applicant.id).await?;
    person.can_acquire_server(db).await?;
    revolt_database::util::age::assert_can_join_server(db, &person, &server).await?;

    // claim the request first: two reviewers at once can't both approve
    if !db
        .decide_join_request(&request.id, JoinRequestStatus::Approved, &user.id)
        .await?
    {
        return Err(create_error!(InvalidOperation));
    }

    match Member::create(db, &server, &person, None).await {
        Ok(_) => Ok(EmptyResponse),
        Err(error) if matches!(error.error_type, ErrorType::AlreadyInServer) => Ok(EmptyResponse),
        Err(error) => {
            // could not add them (e.g. banned meanwhile): put it back
            db.save_join_request(&request).await.ok();
            Err(error)
        }
    }
}

/// # Reject Join Request
#[openapi(tag = "Server Members")]
#[post("/<target>/join_requests/<applicant>/reject")]
pub async fn reject(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    applicant: Reference<'_>,
) -> Result<EmptyResponse> {
    let server = target.as_server(db).await?;
    require_reviewer(db, &user, &server).await?;

    let request = db.fetch_join_request(&server.id, applicant.id).await?;
    if !db
        .decide_join_request(&request.id, JoinRequestStatus::Rejected, &user.id)
        .await?
    {
        return Err(create_error!(InvalidOperation));
    }
    Ok(EmptyResponse)
}

/// # Fetch Own Join Request
///
/// Where your request to join this server stands.
#[openapi(tag = "Server Members")]
#[get("/<target>/join_requests/@me")]
pub async fn fetch_own(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::ServerJoinRequest>> {
    let request = db.fetch_join_request(target.id, &user.id).await?;
    Ok(Json(request.into()))
}

/// # Cancel Own Join Request
#[openapi(tag = "Server Members")]
#[delete("/<target>/join_requests/@me")]
pub async fn cancel_own(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<EmptyResponse> {
    let request = db.fetch_join_request(target.id, &user.id).await?;
    if request.status != JoinRequestStatus::Pending {
        return Err(create_error!(InvalidOperation));
    }
    db.delete_join_request(&request.id).await?;
    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        Invite, JoinRequestStatus, Member, PartialServer, Server, ServerBan, User,
    };
    use revolt_models::v0;
    use rocket::http::{ContentType, Status};

    /// a server that needs approval, with a plain invite to its first channel
    async fn approval_server(harness: &TestHarness, owner: &User) -> (Server, String) {
        let (mut server, channels) = harness.new_server(owner).await;
        server
            .update(
                &harness.db,
                PartialServer {
                    access: Some(v0::ServerAccess::Approval),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("access");
        let code = TestHarness::rand_string()[..8].to_string();
        harness
            .db
            .insert_invite(&Invite::Server {
                code: code.clone(),
                server: server.id.clone(),
                creator: owner.id.clone(),
                channel: channels[0].id().to_string(),
                max_uses: None,
                uses: 0,
                expires: None,
                vanity: false,
            })
            .await
            .expect("invite");
        (server, code)
    }

    #[rocket::async_test]
    async fn invite_to_approval_server_creates_a_request_not_a_member() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, session, applicant) = harness.new_user().await;
        let (server, code) = approval_server(&harness, &owner).await;

        let response =
            TestHarness::with_session(session.clone(), harness.client.post(format!("/invites/{code}")))
                .await;
        assert_eq!(response.status(), Status::Ok);
        let body: v0::InviteJoinResponse = response.into_json().await.expect("response");
        assert!(matches!(body, v0::InviteJoinResponse::JoinRequest { .. }));
        assert!(harness.db.fetch_member(&server.id, &applicant.id).await.is_err());

        // asking again keeps the one pending request
        let again =
            TestHarness::with_session(session, harness.client.post(format!("/invites/{code}"))).await;
        assert_eq!(again.status(), Status::Ok);
        let pending = harness
            .db
            .fetch_join_requests(&server.id, JoinRequestStatus::Pending)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);

        // a pending applicant still can't read the server
        let fetch = TestHarness::with_session(
            harness.account_from_user(applicant.id.clone()).await.1,
            harness.client.get(format!("/servers/{}", server.id)),
        )
        .await;
        assert_eq!(fetch.status(), Status::NotFound);
    }

    #[rocket::async_test]
    async fn only_reviewers_approve_and_approval_adds_the_member_once() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, applicant_session, applicant) = harness.new_user().await;
        let (_, member_session, member) = harness.new_user().await;
        let (server, code) = approval_server(&harness, &owner).await;
        Member::create(&harness.db, &server, &member, None).await.unwrap();
        TestHarness::with_session(applicant_session, harness.client.post(format!("/invites/{code}")))
            .await;

        // an ordinary member (forged call) is refused
        let forged = TestHarness::with_session(
            member_session,
            harness.client.post(format!(
                "/servers/{}/join_requests/{}/approve",
                server.id, applicant.id
            )),
        )
        .await;
        assert_eq!(forged.status(), Status::Forbidden);
        assert!(harness.db.fetch_member(&server.id, &applicant.id).await.is_err());

        let approve = TestHarness::with_session(
            owner_session.clone(),
            harness.client.post(format!(
                "/servers/{}/join_requests/{}/approve",
                server.id, applicant.id
            )),
        )
        .await;
        assert_eq!(approve.status(), Status::NoContent);
        assert!(harness.db.fetch_member(&server.id, &applicant.id).await.is_ok());

        // deciding twice is refused, not a second membership
        let twice = TestHarness::with_session(
            owner_session,
            harness.client.post(format!(
                "/servers/{}/join_requests/{}/approve",
                server.id, applicant.id
            )),
        )
        .await;
        assert_eq!(twice.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn rejected_applicant_is_not_added_and_must_wait() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, applicant_session, applicant) = harness.new_user().await;
        let (server, code) = approval_server(&harness, &owner).await;
        TestHarness::with_session(applicant_session.clone(), harness.client.post(format!("/invites/{code}")))
            .await;

        let reject = TestHarness::with_session(
            owner_session,
            harness.client.post(format!(
                "/servers/{}/join_requests/{}/reject",
                server.id, applicant.id
            )),
        )
        .await;
        assert_eq!(reject.status(), Status::NoContent);
        assert!(harness.db.fetch_member(&server.id, &applicant.id).await.is_err());

        let again =
            TestHarness::with_session(applicant_session, harness.client.post(format!("/invites/{code}")))
                .await;
        assert_eq!(again.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn banned_user_cannot_request() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, session, banned) = harness.new_user().await;
        let (server, code) = approval_server(&harness, &owner).await;
        ServerBan::create(&harness.db, &server, &banned.id, None).await.unwrap();

        let response =
            TestHarness::with_session(session, harness.client.post(format!("/invites/{code}"))).await;
        assert_eq!(response.status(), Status::Forbidden);
        assert!(harness
            .db
            .fetch_join_request(&server.id, &banned.id)
            .await
            .is_err());
    }

    #[rocket::async_test]
    async fn closing_a_server_unlists_it_and_keeps_it_off_discover() {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&owner).await;
        server
            .update(
                &harness.db,
                PartialServer {
                    discoverable: Some(true),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        let response = TestHarness::with_session(
            session,
            harness
                .client
                .patch(format!("/servers/{}", server.id))
                .header(ContentType::JSON)
                .body(r#"{"access":"Approval"}"#),
        )
        .await;
        assert_eq!(response.status(), Status::Ok);
        let stored = harness.db.fetch_server(&server.id).await.unwrap();
        assert_eq!(stored.access, Some(v0::ServerAccess::Approval));
        assert!(!stored.discoverable);
    }
}
