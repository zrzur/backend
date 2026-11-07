//! Rules screening: who joins after it is switched on accepts the rules
//! before talking; members from before are not affected.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{Member, Server, Session};
use rocket::http::{ContentType, Header, Status};

struct Setup {
    harness: TestHarness,
    server: Server,
    owner: Session,
    channel: String,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner, owner_user) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner_user).await;
    Setup {
        channel: channels[0].id().to_string(),
        harness,
        server,
        owner,
    }
}

impl Setup {
    async fn edit(&self, body: serde_json::Value) -> Status {
        self.harness
            .client
            .patch(format!("/servers/{}", self.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", self.owner.token.clone()))
            .body(body.to_string())
            .dispatch()
            .await
            .status()
    }

    async fn join(&self) -> Session {
        let (_, session, user) = self.harness.new_user().await;
        let server = self.harness.db.fetch_server(&self.server.id).await.unwrap();
        Member::create(&self.harness.db, &server, &user, None)
            .await
            .unwrap();
        session
    }

    async fn send(&self, session: &Session) -> (Status, String) {
        let response = self
            .harness
            .client
            .post(format!("/channels/{}/messages", self.channel))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(json!({ "content": "hello" }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn accept(&self, session: &Session) -> Status {
        self.harness
            .client
            .post(format!("/servers/{}/rules/accept", self.server.id))
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
            .await
            .status()
    }
}

async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
}

#[rocket::async_test]
async fn screening_needs_a_rules_channel() {
    let setup = setup().await;
    assert_eq!(setup.edit(json!({ "rules_screening": true })).await, Status::BadRequest);
    assert_eq!(
        setup
            .edit(json!({ "rules_channel": setup.channel, "rules_screening": true }))
            .await,
        Status::Ok
    );
}

#[rocket::async_test]
async fn new_members_accept_the_rules_before_talking() {
    let setup = setup().await;
    let before = setup.join().await;
    tick().await;
    assert_eq!(
        setup
            .edit(json!({ "rules_channel": setup.channel, "rules_screening": true }))
            .await,
        Status::Ok
    );
    tick().await;
    let newcomer = setup.join().await;

    // members from before are not affected
    let (status, body) = setup.send(&before).await;
    assert_eq!(status, Status::Ok, "{body}");

    let (status, body) = setup.send(&newcomer).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("RulesNotAccepted"), "{body}");

    assert_eq!(setup.accept(&newcomer).await, Status::Ok);
    let (status, body) = setup.send(&newcomer).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn switching_screening_off_lets_everyone_talk() {
    let setup = setup().await;
    setup
        .edit(json!({ "rules_channel": setup.channel, "rules_screening": true }))
        .await;
    tick().await;
    let newcomer = setup.join().await;
    assert_eq!(setup.send(&newcomer).await.0, Status::Forbidden);

    assert_eq!(setup.edit(json!({ "rules_screening": false })).await, Status::Ok);
    let (status, body) = setup.send(&newcomer).await;
    assert_eq!(status, Status::Ok, "{body}");
}
