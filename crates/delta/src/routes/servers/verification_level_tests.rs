//! Verification level: what a member without a role needs before talking.

use crate::{rocket, util::test::TestHarness};
use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{
    Account, EmailVerification, Member, MemberCompositeKey, PartialMember, Server, Session, User,
};
use rocket::http::{ContentType, Header, Status};

struct Setup {
    harness: TestHarness,
    server: Server,
    channel: String,
    owner: Session,
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
    async fn set_level(&self, level: &str) {
        let response = self
            .harness
            .client
            .patch(format!("/servers/{}", self.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", self.owner.token.clone()))
            .body(json!({ "verification_level": level }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
    }

    /// A member whose account was made `account_minutes` ago
    async fn member(&self, account_minutes: i64) -> (Account, Session, User) {
        let created = Timestamp::now_utc() - Duration::minutes(account_minutes);
        let ms = created.duration_since(Timestamp::UNIX_EPOCH).whole_milliseconds() as u64;
        let id = ulid::Ulid::from_parts(ms, rand::random()).to_string();
        let user = User {
            id: id.clone(),
            username: format!("member{}", &id[20..]),
            discriminator: "0001".to_string(),
            ..Default::default()
        };
        self.harness.db.insert_user(&user).await.unwrap();
        let (account, session) = self.harness.account_from_user(id).await;
        Member::create(&self.harness.db, &self.server, &user, None)
            .await
            .unwrap();
        (account, session, user)
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
}

#[rocket::async_test]
async fn no_level_lets_new_members_talk() {
    let setup = setup().await;
    let (_, session, _) = setup.member(0).await;
    let (status, body) = setup.send(&session).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn low_needs_a_verified_email() {
    let setup = setup().await;
    setup.set_level("Low").await;
    let (mut account, session, _) = setup.member(60).await;
    account.verification = EmailVerification::Pending {
        token: "token".to_string(),
        expiry: Timestamp::now_utc() + Duration::days(1),
    };
    setup.harness.db.save_account(&account).await.unwrap();

    let (status, body) = setup.send(&session).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("VerificationRequired") && body.contains("email"), "{body}");

    account.verification = EmailVerification::Verified;
    setup.harness.db.save_account(&account).await.unwrap();
    let (status, body) = setup.send(&session).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn medium_needs_an_account_older_than_5_minutes() {
    let setup = setup().await;
    setup.set_level("Medium").await;

    let (_, fresh, _) = setup.member(1).await;
    let (status, body) = setup.send(&fresh).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("account_age"), "{body}");
    let error: serde_json::Value = serde_json::from_str(&body).unwrap();
    let wait = error["wait_seconds"].as_u64().unwrap();
    assert!((180..=240).contains(&wait), "{body}");

    let (_, older, _) = setup.member(6).await;
    let (status, body) = setup.send(&older).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn high_needs_10_minutes_in_the_server_unless_the_member_has_a_role() {
    let setup = setup().await;
    setup.set_level("High").await;
    let (_, session, user) = setup.member(60).await;

    let (status, body) = setup.send(&session).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("member_age"), "{body}");

    // reacting and joining voice follow the same rule
    let response = setup
        .harness
        .client
        .put(format!(
            "/channels/{}/messages/{}/reactions/%F0%9F%91%8D",
            setup.channel,
            ulid::Ulid::new()
        ))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);

    // a member with any role is exempt
    let role = setup.harness.new_role(&setup.server, 1, None).await;
    setup
        .harness
        .db
        .update_member(
            &MemberCompositeKey {
                server: setup.server.id.clone(),
                user: user.id.clone(),
            },
            &PartialMember {
                roles: Some(vec![role.id]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (status, body) = setup.send(&session).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn only_managers_change_the_level() {
    let setup = setup().await;
    let (_, session, _) = setup.member(60).await;
    let response = setup
        .harness
        .client
        .patch(format!("/servers/{}", setup.server.id))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!({ "verification_level": "None" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);

    setup.set_level("High").await;
    let server = setup.harness.db.fetch_server(&setup.server.id).await.unwrap();
    assert_eq!(
        server.verification_level,
        Some(revolt_models::v0::ServerVerificationLevel::High)
    );
}
