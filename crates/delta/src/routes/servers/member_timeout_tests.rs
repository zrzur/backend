//! Timeouts: rank decides who may time out whom, and a timeout ends in the
//! future, at most `max_timeout_days` (28) from now.

use crate::{rocket, util::test::TestHarness};
use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{Member, PartialMember, Server, Session, User};
use revolt_permissions::{ChannelPermission, OverrideField};
use rocket::http::{ContentType, Header, Status};

struct Setup {
    harness: TestHarness,
    server: Server,
    owner: Session,
    /// Rank 0, Timeout Members
    senior: (Session, User),
    /// Rank 1, Timeout Members
    moderator: (Session, User),
    /// No roles
    member: (Session, User),
}

async fn member_with_role(harness: &TestHarness, server: &Server, role: Option<&str>) -> (Session, User) {
    let (_, session, user) = harness.new_user().await;
    let (mut member, _) = Member::create(&harness.db, server, &user, None).await.unwrap();
    if let Some(role) = role {
        member
            .update(
                &harness.db,
                PartialMember {
                    roles: Some(vec![role.to_string()]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
    }
    (session, user)
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner, owner_user) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner_user).await;
    let timeout = Some(OverrideField {
        a: ChannelPermission::TimeoutMembers as i64,
        d: 0,
    });
    // Role::create ranks a role after the server's current roles, so fetch
    // the server again between the two: senior gets rank 0, moderator 1.
    let senior_role = harness.new_role(&server, 0, timeout).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    let moderator_role = harness.new_role(&server, 1, timeout).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();

    let senior = member_with_role(&harness, &server, Some(&senior_role.id)).await;
    let moderator = member_with_role(&harness, &server, Some(&moderator_role.id)).await;
    let member = member_with_role(&harness, &server, None).await;

    Setup {
        harness,
        server,
        owner,
        senior,
        moderator,
        member,
    }
}

impl Setup {
    async fn edit(&self, as_: &Session, target: &User, body: serde_json::Value) -> (Status, String) {
        let response = self
            .harness
            .client
            .patch(format!("/servers/{}/members/{}", self.server.id, target.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", as_.token.to_string()))
            .body(body.to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn timeout_of(&self, user: &User) -> Option<Timestamp> {
        self.harness
            .db
            .fetch_member(&self.server.id, &user.id)
            .await
            .unwrap()
            .timeout
    }
}

fn days_from_now(days: i64) -> serde_json::Value {
    json!({ "timeout": Timestamp::now_utc() + Duration::days(days) })
}

#[rocket::async_test]
async fn owner_times_out_a_moderator() {
    let setup = setup().await;
    let (status, body) = setup
        .edit(&setup.owner, &setup.moderator.1, days_from_now(1))
        .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(setup.timeout_of(&setup.moderator.1).await.is_some());
}

#[rocket::async_test]
async fn higher_moderator_times_out_a_lower_one() {
    let setup = setup().await;
    let (status, body) = setup
        .edit(&setup.senior.0, &setup.moderator.1, days_from_now(1))
        .await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn moderator_cannot_time_out_a_higher_moderator() {
    let setup = setup().await;
    let (status, body) = setup
        .edit(&setup.moderator.0, &setup.senior.1, days_from_now(1))
        .await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("NotElevated"), "{body}");
    assert!(setup.timeout_of(&setup.senior.1).await.is_none());
}

#[rocket::async_test]
async fn timeout_longer_than_28_days_is_refused() {
    let setup = setup().await;
    let (status, body) = setup
        .edit(&setup.owner, &setup.member.1, days_from_now(29))
        .await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert!(body.contains("InvalidProperty"), "{body}");

    let (status, body) = setup
        .edit(&setup.owner, &setup.member.1, days_from_now(27))
        .await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn timeout_in_the_past_is_refused() {
    let setup = setup().await;
    let (status, body) = setup
        .edit(&setup.moderator.0, &setup.member.1, days_from_now(-1))
        .await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert!(setup.timeout_of(&setup.member.1).await.is_none());
}

#[rocket::async_test]
async fn nobody_times_out_the_owner_or_themselves() {
    let setup = setup().await;
    let owner_user = setup
        .harness
        .db
        .fetch_user(&setup.server.owner)
        .await
        .unwrap();
    // the test server has no member entry for its owner; a real one does
    Member::create(&setup.harness.db, &setup.server, &owner_user, None)
        .await
        .unwrap();
    let (status, _) = setup
        .edit(&setup.senior.0, &owner_user, days_from_now(1))
        .await;
    assert_eq!(status, Status::BadRequest);

    let (status, body) = setup
        .edit(&setup.moderator.0, &setup.moderator.1, days_from_now(1))
        .await;
    assert!(body.contains("CannotTimeoutYourself"), "{status} {body}");
}

#[rocket::async_test]
async fn removing_a_timeout_needs_rank() {
    let setup = setup().await;
    let (status, body) = setup
        .edit(&setup.owner, &setup.senior.1, days_from_now(1))
        .await;
    assert_eq!(status, Status::Ok, "{body}");

    let (status, body) = setup
        .edit(&setup.moderator.0, &setup.senior.1, json!({ "remove": ["Timeout"] }))
        .await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(setup.timeout_of(&setup.senior.1).await.is_some());
}
