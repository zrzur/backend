use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::{util::reference::Reference, Database, User};
use revolt_database::AMQP;
use revolt_database::voice::{sync_voice_permissions, VoiceClient};
use revolt_models::v0;
use revolt_permissions::{
    calculate_channel_permissions, ChannelPermission,
};
use revolt_result::{create_error, Result};
use rocket::State;

use rocket::serde::json::Json;
use rocket_empty::EmptyResponse;

use crate::util::audit_log_reason::AuditLogReason;
use revolt_database::Channel;

/// # Invite Bot
///
/// Invite a bot to a server or group by its id.`
#[openapi(tag = "Bots")]
#[post("/<target>/invite?<permissions>", data = "<dest>")]
pub async fn invite_bot(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    permissions: Option<i64>,
    dest: Json<v0::InviteBotDestination>,
) -> Result<EmptyResponse> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let bot = target.as_bot(db).await?;
    if !bot.public && bot.owner != user.id {
        return Err(create_error!(BotIsPrivate));
    }

    let bot_user = db.fetch_user(&bot.id).await?;

    match dest.into_inner() {
        v0::InviteBotDestination::Server { server } => {
            let server = db.fetch_server(&server).await?;

            // Same rules as an OAuth2 install, and recorded the same way
            crate::routes::applications::install::install_bot(
                db,
                &user,
                &bot,
                &server,
                permissions,
                vec!["bot.install".to_string()],
                "invite",
                reason,
            )
            .await?;

            // a bot added to a server counts as a join for a top-row bot
            revolt_database::record_promotion_event(
                db,
                &bot_user.id,
                &user.id,
                revolt_database::PromotionStat::Joins,
            )
            .await
            .ok();

            Ok(EmptyResponse)
        }
        v0::InviteBotDestination::Group { group } => {
            let mut channel = db.fetch_channel(&group).await?;

            let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
            calculate_channel_permissions(&mut query)
                .await
                .throw_if_lacking_channel_permission(ChannelPermission::InviteOthers)?;

            // A bot reads everything in the group, so only the owner chooses
            // one; everyone sees who added it (system message)
            match &channel {
                Channel::Group { owner, .. } if owner == &user.id => {}
                Channel::Group { .. } => return Err(create_error!(NotOwner)),
                _ => return Err(create_error!(InvalidOperation)),
            }

            channel
                .add_user_to_group(db, amqp, &bot_user, &user.id)
                .await
                .map(|_| EmptyResponse)
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{events::client::EventV1, Bot, Channel, Server};
    use revolt_models::v0::{self, DataCreateServer};
    use rocket::http::{ContentType, Header, Status};
    use crate::util::test::PubSubTestHelper;

    #[rocket::async_test]
    async fn invite_bot_to_group() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");

        let group = Channel::create_group(
            &harness.db,
            v0::DataCreateGroup {
                name: TestHarness::rand_string(),
                ..Default::default()
            },
            user.id.to_string(),
        )
        .await
        .unwrap();
        let mut pubsub = PubSubTestHelper::new(group.id()).await;

        let response = harness
            .client
            .post(format!("/bots/{}/invite", bot.id))
            .header(ContentType::JSON)
            .body(
                json!(v0::InviteBotDestination::Group {
                    group: group.id().to_string()
                })
                .to_string(),
            )
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NoContent);
        drop(response);

        let event = pubsub
            .wait_for_event(|event| match event {
                EventV1::ChannelGroupJoin { id, .. } => id == group.id(),
                _ => false,
            })
            .await;

        match event {
            EventV1::ChannelGroupJoin { user, .. } => {
                assert_eq!(bot.id, user);
            }
            _ => unreachable!(),
        }
    }

    #[rocket::async_test]
    async fn invite_bot_to_server() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");

        let (server, _) = Server::create(
            &harness.db,
            DataCreateServer {
                name: TestHarness::rand_string(),
                ..Default::default()
            },
            &user,
            false,
        )
        .await
        .unwrap();
        let mut pubsub = PubSubTestHelper::new(&server.id).await;

        let response = harness
            .client
            .post(format!("/bots/{}/invite", bot.id))
            .header(ContentType::JSON)
            .body(
                json!(v0::InviteBotDestination::Server {
                    server: server.id.to_string()
                })
                .to_string(),
            )
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NoContent);
        drop(response);

        let event = pubsub
            .wait_for_event(|event| match event {
                EventV1::ServerMemberJoin { id, .. } => id == &server.id,
                _ => false,
            })
            .await;

        match event {
            EventV1::ServerMemberJoin { member, .. } => {
                assert_eq!(bot.id, member.id.user);
            }
            _ => unreachable!(),
        }
    }
}
