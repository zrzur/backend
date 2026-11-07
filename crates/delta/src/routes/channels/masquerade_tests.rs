//! Bots and masquerade: a bot may only show another name when the server
//! gave it that right on purpose, and never the name of someone in the server.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{Bot, Member, PartialMember, PartialServer, PartialUser, Server, User};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, OverrideField, DEFAULT_PERMISSION_SERVER};
use rocket::http::{ContentType, Header, Status};

struct Setup {
    harness: TestHarness,
    server: Server,
    channel: String,
    bot: Bot,
    bot_user: User,
}

/// A server whose default permissions include Masquerade, with a bot in it
async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    harness
        .db
        .update_server(
            &server.id,
            &PartialServer {
                default_permissions: Some(
                    (*DEFAULT_PERMISSION_SERVER | ChannelPermission::Masquerade as u64) as i64,
                ),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (bot, bot_user) = harness.new_bot(&owner).await;
    Member::create(&harness.db, &server, &bot_user, None).await.unwrap();
    Setup {
        channel: channels[0].id().to_string(),
        harness,
        server,
        bot,
        bot_user,
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
                    user: self.bot_user.id.clone(),
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

    async fn send(&self, header: Header<'static>, name: &str) -> (Status, String) {
        let response = self
            .harness
            .client
            .post(format!("/channels/{}/messages", self.channel))
            .header(header)
            .header(ContentType::JSON)
            .body(
                json!({
                    "content": "hello",
                    "masquerade": { "name": name }
                })
                .to_string(),
            )
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn bot_sends(&self, name: &str) -> (Status, String) {
        self.send(Header::new("x-bot-token", self.bot.token.clone()), name)
            .await
    }
}

#[rocket::async_test]
async fn a_bot_does_not_get_masquerade_from_the_server_defaults() {
    let setup = setup().await;
    let (status, body) = setup.bot_sends("Weather").await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("Masquerade"), "{body}");

    // people keep what the defaults give them
    let (_, session, person) = setup.harness.new_user().await;
    Member::create(&setup.harness.db, &setup.server, &person, None)
        .await
        .unwrap();
    let (status, body) = setup
        .send(Header::new("x-session-token", session.token.clone()), "Weather")
        .await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn a_bot_with_an_explicit_grant_can_masquerade() {
    let setup = setup().await;
    setup.grant_masquerade().await;
    let (status, body) = setup.bot_sends("Weather").await;
    assert_eq!(status, Status::Ok, "{body}");
    let message: v0::Message = serde_json::from_str(&body).unwrap();
    // the message stays the bot's: clients show the BOT badge from the author
    assert_eq!(message.author, setup.bot_user.id);
    assert_eq!(message.masquerade.unwrap().name.as_deref(), Some("Weather"));
    assert!(setup
        .harness
        .db
        .fetch_user(&message.author)
        .await
        .unwrap()
        .bot
        .is_some());
}

#[rocket::async_test]
async fn a_bot_cannot_take_the_name_of_someone_in_the_server() {
    let setup = setup().await;
    setup.grant_masquerade().await;
    let (_, _, person) = setup.harness.new_user().await;
    Member::create(&setup.harness.db, &setup.server, &person, None)
        .await
        .unwrap();
    setup
        .harness
        .db
        .update_user(
            &person.id,
            &PartialUser {
                display_name: Some("Lina Haddad".to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    setup
        .harness
        .db
        .update_member(
            &revolt_database::MemberCompositeKey {
                server: setup.server.id.clone(),
                user: person.id.clone(),
            },
            &PartialMember {
                nickname: Some("Night Owl".to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    for taken in [
        person.username.as_str(),
        &person.username.to_uppercase(),
        "lina haddad",
        "  LinaHaddad ",
        "night\u{200b}owl",
        "NIGHT OWL",
    ] {
        let (status, body) = setup.bot_sends(taken).await;
        assert_eq!(status, Status::BadRequest, "{taken}: {body}");
        assert!(body.contains("MasqueradeNameTaken"), "{taken}: {body}");
    }

    // the bot's own name and names nobody here uses are fine
    let (status, body) = setup.bot_sends(&setup.bot_user.username.clone()).await;
    assert_eq!(status, Status::Ok, "{body}");
    let (status, body) = setup.bot_sends("Lina's helper").await;
    assert_eq!(status, Status::Ok, "{body}");
}
