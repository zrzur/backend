//! Administrator (owner, 2026-11-09): sees and manages every channel whatever
//! its overrides say; only the owner or another administrator can give it;
//! role ranks still apply; a timeout doesn't hold it, so it is refused.

use crate::{rocket, util::test::TestHarness};
use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{Member, PartialMember, Server, Session, User};
use revolt_permissions::{ChannelPermission, OverrideField};
use rocket::http::{ContentType, Header, Status};

const ADMIN: i64 = ChannelPermission::Administrator as i64;
const VIEW: i64 = ChannelPermission::ViewChannel as i64;
const MANAGE: i64 = ChannelPermission::ManagePermissions as i64 | ChannelPermission::ManageRole as i64;

struct Setup {
    harness: TestHarness,
    server: Server,
    owner: Session,
    /// rank 0: Administrator
    admin: (Session, User),
    admin_role: String,
    /// rank 1: Manage permissions and roles, not Administrator
    manager: (Session, User),
    /// rank 2: nothing
    plain_role: String,
    member: (Session, User),
    /// denied to @everyone
    private: String,
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
    let admin_role = harness.new_role(&server, 0, Some(OverrideField { a: ADMIN, d: 0 })).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    let manager_role = harness.new_role(&server, 1, Some(OverrideField { a: MANAGE, d: 0 })).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    let plain_role = harness.new_role(&server, 2, None).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    let channel = harness.new_channel(&server).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();

    let admin = member_with_role(&harness, &server, Some(&admin_role.id)).await;
    let manager = member_with_role(&harness, &server, Some(&manager_role.id)).await;
    let member = member_with_role(&harness, &server, None).await;

    let setup = Setup {
        harness,
        server,
        owner,
        admin,
        admin_role: admin_role.id,
        manager,
        plain_role: plain_role.id,
        member,
        private: channel.id().to_string(),
    };
    let (status, body) = setup
        .send(
            "PUT",
            &setup.owner,
            &format!("/channels/{}/permissions/default", setup.private),
            Some(json!({ "permissions": { "allow": 0, "deny": VIEW } })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{body}");
    setup
}

impl Setup {
    async fn send(&self, method: &str, as_: &Session, path: &str, body: Option<serde_json::Value>) -> (Status, String) {
        let client = &self.harness.client;
        let request = match method {
            "GET" => client.get(path.to_string()),
            "PUT" => client.put(path.to_string()),
            "PATCH" => client.patch(path.to_string()),
            _ => unreachable!(),
        }
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", as_.token.to_string()));
        let request = match body {
            Some(body) => request.body(body.to_string()),
            None => request,
        };
        let response = request.dispatch().await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }
}

#[rocket::async_test]
async fn administrator_opens_a_channel_denied_to_everyone() {
    let setup = setup().await;
    let path = format!("/channels/{}", setup.private);
    let (status, body) = setup.send("GET", &setup.admin.0, &path, None).await;
    assert_eq!(status, Status::Ok, "{body}");

    let (status, _) = setup.send("GET", &setup.member.0, &path, None).await;
    assert_ne!(status, Status::Ok);
    let (status, _) = setup.send("GET", &setup.manager.0, &path, None).await;
    assert_ne!(status, Status::Ok, "manage rights alone don't open it");
}

#[rocket::async_test]
async fn administrator_ignores_a_deny_for_its_own_role() {
    let setup = setup().await;
    let (status, body) = setup
        .send(
            "PUT",
            &setup.owner,
            &format!("/channels/{}/permissions/{}", setup.private, setup.admin_role),
            Some(json!({ "permissions": { "allow": 0, "deny": VIEW } })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{body}");
    let (status, body) = setup
        .send("GET", &setup.admin.0, &format!("/channels/{}", setup.private), None)
        .await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn only_an_administrator_or_the_owner_gives_it() {
    let setup = setup().await;
    let path = format!("/servers/{}/permissions/{}", setup.server.id, setup.plain_role);
    let grant = json!({ "permissions": { "allow": ADMIN, "deny": 0 } });

    let (status, body) = setup.send("PUT", &setup.manager.0, &path, Some(grant.clone())).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("CannotGiveMissingPermissions"), "{body}");

    let (status, body) = setup.send("PUT", &setup.admin.0, &path, Some(grant.clone())).await;
    assert_eq!(status, Status::Ok, "{body}");

    let (status, body) = setup.send("PUT", &setup.owner, &path, Some(grant)).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn ranks_still_hold_for_an_administrator() {
    let setup = setup().await;
    // a role at the administrator's own rank is out of its reach
    let (status, body) = setup
        .send(
            "PATCH",
            &setup.admin.0,
            &format!("/servers/{}/roles/{}", setup.server.id, setup.admin_role),
            Some(json!({ "name": "renamed" })),
        )
        .await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("NotElevated"), "{body}");

    // a member below it is not
    let (status, body) = setup
        .send(
            "PATCH",
            &setup.admin.0,
            &format!("/servers/{}/members/{}", setup.server.id, setup.member.1.id),
            Some(json!({ "roles": [setup.plain_role] })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn an_administrator_cannot_be_timed_out() {
    let setup = setup().await;
    let until = Timestamp::now_utc() + Duration::days(1);
    let (status, body) = setup
        .send(
            "PATCH",
            &setup.owner,
            &format!("/servers/{}/members/{}", setup.server.id, setup.admin.1.id),
            Some(json!({ "timeout": until })),
        )
        .await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert!(body.contains("InvalidOperation"), "{body}");

    let (status, body) = setup
        .send(
            "PATCH",
            &setup.owner,
            &format!("/servers/{}/members/{}", setup.server.id, setup.member.1.id),
            Some(json!({ "timeout": until })),
        )
        .await;
    assert_eq!(status, Status::Ok, "{body}");
}
