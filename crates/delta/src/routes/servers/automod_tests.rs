//! AutoMod: rules set by managers, checked when a message is sent or edited.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{Member, MemberCompositeKey, PartialMember, Server, Session, User};
use rocket::http::{ContentType, Header, Status};
use serde_json::Value;

struct Setup {
    harness: TestHarness,
    server: Server,
    owner: Session,
    general: String,
    other: String,
    alerts: String,
    member: Session,
    member_user: User,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner, owner_user) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner_user).await;
    let other = harness.new_channel(&server).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    let alerts = harness.new_channel(&server).await;
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    let (_, member, member_user) = harness.new_user().await;
    Member::create(&harness.db, &server, &member_user, None)
        .await
        .unwrap();
    Setup {
        general: channels[0].id().to_string(),
        other: other.id().to_string(),
        alerts: alerts.id().to_string(),
        harness,
        server,
        owner,
        member,
        member_user,
    }
}

impl Setup {
    async fn set_rules(&self, session: &Session, rules: Value) -> (Status, String) {
        let response = self
            .harness
            .client
            .put(format!("/servers/{}/automod", self.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(json!({ "rules": rules }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn rule(&self, trigger: Value, actions: Value) {
        let (status, body) = self
            .set_rules(
                &self.owner,
                json!([{ "id": "r1", "name": "No spam", "trigger": trigger, "actions": actions }]),
            )
            .await;
        assert_eq!(status, Status::Ok, "{body}");
    }

    async fn send(&self, channel: &str, content: &str) -> (Status, String) {
        let response = self
            .harness
            .client
            .post(format!("/channels/{channel}/messages"))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", self.member.token.clone()))
            .body(json!({ "content": content }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn blocked(&self, channel: &str, content: &str) -> bool {
        let (status, body) = self.send(channel, content).await;
        if status == Status::Ok {
            return false;
        }
        assert_eq!(status, Status::Forbidden, "{body}");
        assert!(body.contains("AutoModBlocked") && body.contains("No spam"), "{body}");
        true
    }
}

#[rocket::async_test]
async fn only_managers_see_and_set_rules() {
    let setup = setup().await;
    let rules = json!([{ "id": "r1", "name": "No spam",
        "trigger": { "type": "MentionSpam", "max": 3 }, "actions": { "block": true } }]);
    let (status, _) = setup.set_rules(&setup.member, rules.clone()).await;
    assert_eq!(status, Status::Forbidden);
    let (status, body) = setup.set_rules(&setup.owner, rules).await;
    assert_eq!(status, Status::Ok, "{body}");

    // members never receive the rules with the server
    let server = setup
        .harness
        .client
        .get(format!("/servers/{}", setup.server.id))
        .header(Header::new("x-session-token", setup.member.token.clone()))
        .dispatch()
        .await
        .into_string()
        .await
        .unwrap();
    assert!(!server.contains("automod"), "{server}");

    // a rule without any action, or with an unknown channel, is refused
    let (status, _) = setup
        .set_rules(&setup.owner, json!([{ "id": "r", "name": "x",
            "trigger": { "type": "MentionSpam", "max": 3 }, "actions": {} }]))
        .await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = setup
        .set_rules(&setup.owner, json!([{ "id": "r", "name": "x",
            "trigger": { "type": "MentionSpam", "max": 3 },
            "actions": { "alert_channel": "01NOTACHANNEL0000000000000" } }]))
        .await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn keywords_match_arabic_variants_and_respect_the_allow_list() {
    let setup = setup().await;
    setup
        .rule(
            json!({ "type": "Keywords", "words": ["سيئة", "spam*"], "allow": ["spamalot"] }),
            json!({ "block": true }),
        )
        .await;
    assert!(setup.blocked(&setup.general, "هذه كلمة سيّئه جداً").await);
    assert!(setup.blocked(&setup.general, "total SPAMMERS here").await);
    assert!(!setup.blocked(&setup.general, "watching spamalot tonight").await);
    assert!(!setup.blocked(&setup.general, "a nice day").await);
}

#[rocket::async_test]
async fn mention_spam_is_blocked() {
    let setup = setup().await;
    setup
        .rule(json!({ "type": "MentionSpam", "max": 2 }), json!({ "block": true }))
        .await;
    assert!(!setup.blocked(&setup.general, "<@01A> <@01A> <@01B>").await);
    assert!(setup.blocked(&setup.general, "<@01A> <@01B> <@01C>").await);
}

#[rocket::async_test]
async fn new_members_cannot_post_links_and_other_domains_are_refused() {
    let setup = setup().await;
    setup
        .rule(
            json!({ "type": "LinkSpam", "new_member_minutes": 10 }),
            json!({ "block": true }),
        )
        .await;
    assert!(setup.blocked(&setup.general, "join https://spam.example now").await);
    assert!(!setup.blocked(&setup.general, "no link here").await);

    setup
        .rule(
            json!({ "type": "LinkSpam", "allowed_domains": ["zrzur.com"] }),
            json!({ "block": true }),
        )
        .await;
    assert!(!setup.blocked(&setup.general, "see https://docs.zrzur.com/x").await);
    assert!(setup.blocked(&setup.general, "see www.other.io").await);
}

#[rocket::async_test]
async fn the_same_text_in_many_channels_is_blocked() {
    let setup = setup().await;
    setup
        .rule(
            json!({ "type": "RepeatedMessages", "channels": 2, "seconds": 30 }),
            json!({ "block": true }),
        )
        .await;
    let text = format!("buy now {}", ulid::Ulid::new());
    assert!(!setup.blocked(&setup.general, &text).await);
    assert!(setup.blocked(&setup.other, &text).await);
}

#[rocket::async_test]
async fn exempt_roles_are_not_checked() {
    let setup = setup().await;
    let role = setup.harness.new_role(&setup.server, 1, None).await;
    let rules = json!([{ "id": "r1", "name": "No spam",
        "trigger": { "type": "Keywords", "words": ["spam"] },
        "actions": { "block": true }, "exempt_roles": [role.id] }]);
    let (status, body) = setup.set_rules(&setup.owner, rules).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(setup.blocked(&setup.general, "spam").await);

    setup
        .harness
        .db
        .update_member(
            &MemberCompositeKey {
                server: setup.server.id.clone(),
                user: setup.member_user.id.clone(),
            },
            &PartialMember {
                roles: Some(vec![role.id.clone()]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    assert!(!setup.blocked(&setup.general, "spam").await);
}

#[rocket::async_test]
async fn alert_and_timeout_actions_and_the_audit_log() {
    let setup = setup().await;
    setup
        .rule(
            json!({ "type": "Keywords", "words": ["spam"] }),
            json!({ "alert_channel": setup.alerts, "timeout_minutes": 5 }),
        )
        .await;
    // not blocked: only alert and timeout
    let (status, body) = setup.send(&setup.general, "spam").await;
    assert_eq!(status, Status::Ok, "{body}");

    let member = setup
        .harness
        .db
        .fetch_member(&setup.server.id, &setup.member_user.id)
        .await
        .unwrap();
    assert!(member.timeout.is_some());

    let alerts = setup
        .harness
        .client
        .get(format!("/channels/{}/messages", setup.alerts))
        .header(Header::new("x-session-token", setup.owner.token.clone()))
        .dispatch()
        .await
        .into_string()
        .await
        .unwrap();
    assert!(alerts.contains("automod_alert"), "{alerts}");

    let audit = setup
        .harness
        .client
        .get(format!("/servers/{}/audit_logs", setup.server.id))
        .header(Header::new("x-session-token", setup.owner.token.clone()))
        .dispatch()
        .await
        .into_string()
        .await
        .unwrap();
    assert!(audit.contains("AutoModAction"), "{audit}");
}

#[rocket::async_test]
async fn edits_are_checked_too() {
    let setup = setup().await;
    let (status, body) = setup.send(&setup.general, "hello").await;
    assert_eq!(status, Status::Ok, "{body}");
    let message: Value = serde_json::from_str(&body).unwrap();
    setup
        .rule(json!({ "type": "Keywords", "words": ["spam"] }), json!({ "block": true }))
        .await;

    let response = setup
        .harness
        .client
        .patch(format!(
            "/channels/{}/messages/{}",
            setup.general,
            message["_id"].as_str().unwrap()
        ))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", setup.member.token.clone()))
        .body(json!({ "content": "spam" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);
}
