use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Channel, Database, User,
};

use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Fetch Channel
///
/// Fetch channel by its id.
#[openapi(tag = "Channel Information")]
#[get("/<target>")]
pub async fn fetch(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::Channel>> {
    let channel = target.as_channel(db).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    if !permissions.has_channel_permission(ChannelPermission::ViewChannel) {
        // Someone who is not in the channel's server gets what `GET
        // /servers/{id}` gives them: not found. Only members learn that a
        // channel exists they can't see.
        if let Channel::TextChannel { server, .. } = &channel {
            if db.fetch_member(server, &user.id).await.is_err() {
                return Err(create_error!(NotFound));
            }
        }
    }
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;

    Ok(Json(channel.into()))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::Channel;
    use revolt_models::v0;
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn fetch_channel() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

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

        let response = harness
            .client
            .get(format!("/channels/{}", group.id()))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);

        let channel: v0::Channel = response.into_json().await.expect("`Channel`");
        assert_eq!(channel, group.into());
    }

    async fn status_and_type(
        harness: &TestHarness,
        auth: (&'static str, &str),
        path: String,
    ) -> (Status, Option<String>) {
        let response = harness
            .client
            .get(path)
            .header(Header::new(auth.0, auth.1.to_string()))
            .dispatch()
            .await;
        let status = response.status();
        let body: Option<serde_json::Value> = response.into_json().await;
        (
            status,
            body.and_then(|body| body["type"].as_str().map(str::to_owned)),
        )
    }

    /// Gap 14: a bot (or anyone) outside the server gets the server's answer
    /// for its channels too: not found, so nothing is learned either way
    #[rocket::async_test]
    async fn outside_the_server_a_channel_is_not_found_like_its_server() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, session, _) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        let (bot, _) = harness.new_bot(&owner).await;
        let bot_token = bot.token.clone();

        for token in [
            ("x-session-token", session.token.as_str()),
            ("x-bot-token", bot_token.as_str()),
        ] {
            let server_answer =
                status_and_type(&harness, token, format!("/servers/{}", server.id)).await;
            let channel_answer =
                status_and_type(&harness, token, format!("/channels/{}", channels[0].id())).await;
            assert_eq!(server_answer, (Status::NotFound, Some("NotFound".to_string())));
            assert_eq!(channel_answer, server_answer);

            // the same as a channel that does not exist
            let missing =
                status_and_type(&harness, token, "/channels/01NOSUCHCHANNEL000000000".to_string())
                    .await;
            assert_eq!(missing, channel_answer);
        }
    }

    /// A member who can't see a channel still hears why
    #[rocket::async_test]
    async fn a_member_without_access_is_told_the_permission() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, session, member) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&owner).await;
        revolt_database::Member::create(&harness.db, &server, &member, None)
            .await
            .unwrap();
        let hidden = Channel::create_server_channel(
            &harness.db,
            &mut server,
            v0::DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Text,
                name: "staff".to_string(),
                description: None,
                nsfw: Some(false),
                voice: None,
                announcement: None,
                category: None,
                private: Some(true),
            },
            true,
        )
        .await
        .unwrap();

        let answer = status_and_type(
            &harness,
            ("x-session-token", &session.token),
            format!("/channels/{}", hidden.id()),
        )
        .await;
        assert_eq!(answer, (Status::Forbidden, Some("MissingPermission".to_string())));
    }
}
