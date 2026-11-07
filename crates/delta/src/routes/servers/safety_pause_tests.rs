//! Security actions: pause invites, pause DMs, and the raid alert.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{
    Invite, Member, PartialServer, RelationshipStatus, Server, Session, SystemMessageChannels,
    User,
};
use rocket::http::{ContentType, Header, Status};

struct Setup {
    harness: TestHarness,
    server: Server,
    owner: Session,
    channel: revolt_database::Channel,
    owner_user: User,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner, owner_user) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner_user).await;
    Setup {
        harness,
        server,
        owner,
        channel: channels[0].clone(),
        owner_user,
    }
}

impl Setup {
    async fn pause(&self, session: &Session, body: serde_json::Value) -> Status {
        self.harness
            .client
            .put(format!("/servers/{}/safety/pause", self.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(body.to_string())
            .dispatch()
            .await
            .status()
    }

    async fn join(&self, code: &str) -> (Status, String) {
        let (_, guest, _) = self.harness.new_user().await;
        let response = self
            .harness
            .client
            .post(format!("/invites/{code}"))
            .header(Header::new("x-session-token", guest.token.clone()))
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn member(&self) -> (Session, User) {
        let (_, session, user) = self.harness.new_user().await;
        Member::create(&self.harness.db, &self.server, &user, None)
            .await
            .unwrap();
        (session, user)
    }
}

#[rocket::async_test]
async fn paused_invites_refuse_every_join_until_resumed() {
    let setup = setup().await;
    let invite = Invite::create_channel_invite(&setup.harness.db, &setup.owner_user, &setup.channel, None, None)
        .await
        .unwrap();

    assert_eq!(setup.pause(&setup.owner, json!({ "invites_hours": 6 })).await, Status::Ok);
    let (status, body) = setup.join(invite.code()).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("InvitesPaused"), "{body}");

    assert_eq!(setup.pause(&setup.owner, json!({ "invites_hours": 0 })).await, Status::Ok);
    let (status, body) = setup.join(invite.code()).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn only_managers_pause_and_only_for_allowed_hours() {
    let setup = setup().await;
    let (member, _) = setup.member().await;
    assert_eq!(setup.pause(&member, json!({ "invites_hours": 1 })).await, Status::Forbidden);
    assert_eq!(setup.pause(&setup.owner, json!({ "invites_hours": 48 })).await, Status::BadRequest);
    assert_eq!(setup.pause(&setup.owner, json!({})).await, Status::BadRequest);
}

#[rocket::async_test]
async fn paused_dms_stop_strangers_not_friends() {
    let setup = setup().await;
    let (a_session, mut a) = setup.member().await;
    let (_, mut b) = setup.member().await;
    assert_eq!(setup.pause(&setup.owner, json!({ "dms_hours": 1 })).await, Status::Ok);

    let open = |session: &Session, user: &User| {
        setup
            .harness
            .client
            .get(format!("/users/{}/dm", user.id))
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
    };
    let response = open(&a_session, &b).await;
    assert_eq!(response.status(), Status::Forbidden);
    assert!(response.into_string().await.unwrap().contains("DirectMessagesPaused"));

    #[allow(clippy::disallowed_methods)]
    a.apply_relationship(
        &setup.harness.db,
        &mut b,
        RelationshipStatus::Friend,
        RelationshipStatus::Friend,
    )
    .await
    .unwrap();
    let response = open(&a_session, &b).await;
    assert_eq!(response.status(), Status::Ok);
}

#[rocket::async_test]
async fn many_joins_post_one_raid_alert() {
    let setup = setup().await;
    let system = setup.channel.id().to_string();
    setup
        .harness
        .db
        .update_server(
            &setup.server.id,
            &PartialServer {
                system_messages: Some(SystemMessageChannels {
                    user_joined: Some(system.clone()),
                    user_left: None,
                    user_kicked: None,
                    user_banned: None,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let server = setup.harness.db.fetch_server(&setup.server.id).await.unwrap();

    // the default limit is 10 joins a minute
    for _ in 0..13 {
        let (_, _, user) = setup.harness.new_user().await;
        Member::create(&setup.harness.db, &server, &user, None)
            .await
            .unwrap();
    }

    let messages = setup
        .harness
        .db
        .fetch_messages(revolt_database::MessageQuery {
            filter: revolt_database::MessageFilter {
                channel: Some(system),
                ..Default::default()
            },
            limit: Some(100),
            time_period: revolt_database::MessageTimePeriod::Absolute {
                before: None,
                after: None,
                sort: None,
            },
        })
        .await
        .unwrap();
    let alerts = messages
        .iter()
        .filter(|message| {
            matches!(
                message.system,
                Some(revolt_database::SystemMessage::RaidAlert { joins: 11, .. })
            )
        })
        .count();
    assert_eq!(alerts, 1);
}
