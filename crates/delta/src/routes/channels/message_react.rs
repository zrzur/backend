use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User,
};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::Result;
use rocket::State;
use rocket_empty::EmptyResponse;

/// # Add Reaction to Message
///
/// React to a given message.
#[openapi(tag = "Interactions")]
#[put("/<target>/messages/<msg>/reactions/<emoji>")]
pub async fn react_message(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
    emoji: Reference<'_>,
) -> Result<EmptyResponse> {
    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::React)?;

    // Fetch relevant message
    let message = msg.as_message_in_channel(db, channel.id()).await?;

    // Add the reaction
    message
        .add_reaction(db, &user, &mut query, emoji.id)
        .await
        .map(|_| EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        util::idempotency::IdempotencyKey, Member, Message, Server,
    };
    use revolt_models::v0;
    use rocket::http::{Header, Status};

    async fn seed_channel_message(
        harness: &TestHarness,
        user: &revolt_database::User,
    ) -> (String, String) {
        let (server, channels) = Server::create(
            &harness.db,
            v0::DataCreateServer {
                name: "React Test".to_string(),
                ..Default::default()
            },
            user,
            true,
        )
        .await
        .expect("server");

        let (member, channels) = Member::create(&harness.db, &server, user, Some(channels))
            .await
            .expect("member");
        let channel = &channels[0];

        let message = Message::create_from_api(
            &harness.db,
            None,
            channel.clone(),
            v0::DataMessageSend {
                content: Some("react target".to_string()),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(user)).await),
            Some(user.clone().into(&harness.db, Some(user)).await),
            Some(member.into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("react-0".to_string()),
            false,
            false,
        )
        .await
        .expect("message");

        (channel.id().to_string(), message.id)
    }

    #[rocket::async_test]
    async fn member_can_react_with_unicode_emoji() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (channel_id, message_id) = seed_channel_message(&harness, &user).await;

        let response = harness
            .client
            .put(format!(
                "/channels/{channel_id}/messages/{message_id}/reactions/\u{1F44D}"
            ))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NoContent);
    }

    #[rocket::async_test]
    async fn outsider_cannot_react() {
        let harness = TestHarness::new().await;
        let (_, _session_owner, owner) = harness.new_user().await;
        let (_, session_outsider, _) = harness.new_user().await;
        let (channel_id, message_id) = seed_channel_message(&harness, &owner).await;

        let response = harness
            .client
            .put(format!(
                "/channels/{channel_id}/messages/{message_id}/reactions/\u{1F44D}"
            ))
            .header(Header::new(
                "x-session-token",
                session_outsider.token.to_string(),
            ))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Forbidden);
    }

    #[rocket::async_test]
    async fn unsupported_emoji_is_rejected() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (channel_id, message_id) = seed_channel_message(&harness, &user).await;

        let response = harness
            .client
            .put(format!(
                "/channels/{channel_id}/messages/{message_id}/reactions/not-real-emoji"
            ))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_ne!(response.status(), Status::NoContent);
    }
}
