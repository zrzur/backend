//! A locked server is never joined by its id and never listed on Discover,
//! also when it was listed before it was locked.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{PartialServer, Server, Session};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};

struct Setup {
    harness: TestHarness,
    server: Server,
    owner: Session,
}

/// A listed, open server
async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner, owner_user) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner_user).await;
    harness
        .db
        .update_server(
            &server.id,
            &PartialServer {
                discoverable: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    Setup {
        harness,
        server,
        owner,
    }
}

impl Setup {
    /// The state a server could reach before access changes unlisted it
    async fn force_access(&self, access: v0::ServerAccess) {
        self.harness
            .db
            .update_server(
                &self.server.id,
                &PartialServer {
                    access: Some(access),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
    }

    async fn join_by_id(&self) -> (Status, String) {
        let (_, guest, _) = self.harness.new_user().await;
        let response = self
            .harness
            .client
            .post(format!("/invites/{}", self.server.id))
            .header(Header::new("x-session-token", guest.token.clone()))
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn listed(&self) -> bool {
        let listed = self
            .harness
            .db
            .fetch_discoverable_servers(None, 100, 0)
            .await
            .unwrap()
            .iter()
            .any(|server| server.id == self.server.id);
        let page = self
            .harness
            .client
            .get(format!("/discover/servers/{}", self.server.id))
            .dispatch()
            .await
            .status();
        assert_eq!(listed, page == Status::Ok, "list and page disagree");
        listed
    }
}

#[rocket::async_test]
async fn an_open_listed_server_is_joined_by_id() {
    let setup = setup().await;
    assert!(setup.listed().await);
    let (status, body) = setup.join_by_id().await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn locking_a_listed_server_unlists_it_and_stops_joins_by_id() {
    let setup = setup().await;
    let response = setup
        .harness
        .client
        .patch(format!("/servers/{}", setup.server.id))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", setup.owner.token.clone()))
        .body(r#"{"access":"Locked"}"#)
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    // the owner sees it in the answer: no longer listed
    let edited: serde_json::Value = response.into_json().await.unwrap();
    assert_ne!(edited["discoverable"], serde_json::Value::Bool(true));

    assert!(!setup.listed().await);
    let (status, body) = setup.join_by_id().await;
    assert_ne!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn a_locked_server_still_marked_listed_is_hidden_and_closed() {
    let setup = setup().await;
    setup.force_access(v0::ServerAccess::Locked).await;

    assert!(!setup.listed().await);
    let (status, body) = setup.join_by_id().await;
    assert_ne!(status, Status::Ok, "{body}");
    assert!(!body.contains("\"Server\""), "{body}");
}

#[rocket::async_test]
async fn an_approval_server_still_marked_listed_takes_requests_only() {
    let setup = setup().await;
    setup.force_access(v0::ServerAccess::Approval).await;

    assert!(!setup.listed().await);
    let (status, body) = setup.join_by_id().await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body.contains("JoinRequest"), "{body}");
}
