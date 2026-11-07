//! People and masquerade: a new server does not let members post under
//! another name, and nobody may use the name of another member.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{Member, PartialMember, PartialUser, Server, Session, User};
use revolt_permissions::{ChannelPermission, OverrideField};
use rocket::http::{ContentType, Header, Status};

struct Setup {
    harness: TestHarness,
    server: Server,
    channel: String,
    session: Session,
    user: User,
    /// Another member, display name "Night Owl"
    other: User,
}

async fn join(harness: &TestHarness, server: &Server) -> (Session, User) {
    let (_, session, user) = harness.new_user().await;
    Member::create(&harness.db, server, &user, None).await.unwrap();
    (session, user)
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let (session, user) = join(&harness, &server).await;
    let (_, mut other) = join(&harness, &server).await;
    other
        .update(
            &harness.db,
            PartialUser {
                display_name: Some("Night Owl".to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    Setup {
        channel: channels[0].id().to_string(),
        harness,
        server,
        session,
        user,
        other,
    }
}

impl Setup {
    async fn grant_masquerade(&self) {
        let role = self
            .harness
            .new_role(
                &self.server,
                1,
                Some(OverrideField {
                    a: ChannelPermission::Masquerade as i64,
                    d: 0,
                }),
            )
            .await;
        self.harness
            .db
            .update_member(
                &revolt_database::MemberCompositeKey {
                    server: self.server.id.clone(),
                    user: self.user.id.clone(),
                },
                &PartialMember {
                    roles: Some(vec![role.id]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
    }

    async fn send_as(&self, name: &str) -> (Status, String) {
        let response = self
            .harness
            .client
            .post(format!("/channels/{}/messages", self.channel))
            .header(Header::new("x-session-token", self.session.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "content": "hello", "masquerade": { "name": name } }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }
}

#[rocket::async_test]
async fn a_new_server_does_not_let_members_masquerade() {
    let setup = setup().await;
    let (status, body) = setup.send_as("Admin").await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("Masquerade"), "{body}");
}

#[rocket::async_test]
async fn a_member_with_the_grant_may_use_a_free_name() {
    let setup = setup().await;
    setup.grant_masquerade().await;
    let (status, body) = setup.send_as("Narrator").await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn nobody_masquerades_as_another_member() {
    let setup = setup().await;
    setup.grant_masquerade().await;
    for name in ["Night Owl", "night owl", " NIGHT\u{200b}OWL ", setup.other.username.as_str()] {
        let (status, body) = setup.send_as(name).await;
        assert_eq!(status, Status::BadRequest, "{name}: {body}");
        assert!(body.contains("MasqueradeNameTaken"), "{body}");
    }

    // their own name is fine
    let (status, body) = setup.send_as(&setup.user.username.clone()).await;
    assert_eq!(status, Status::Ok, "{body}");
}
