use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User, AMQP,
};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::State;
use rocket_empty::EmptyResponse;

/// # Acknowledge Message
///
/// Lets the server and all other clients know that we've seen this message id in this channel.
#[openapi(tag = "Messaging")]
#[put("/<target>/ack/<message>")]
pub async fn ack(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    message: Reference<'_>,
) -> Result<EmptyResponse> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;

    channel
        .ack(&user.id, message.id, amqp)
        .await
        .map(|_| EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{events::client::EventV1, Channel};
    use revolt_models::v0::DataCreateGroup;
    use rocket::http::{Header, Status};
    use crate::util::test::PubSubTestHelper;

    #[rocket::async_test]
    async fn success_ack_channel() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let mut pubsub = PubSubTestHelper::new(&format!("{}!", user.id)).await;

        let group = Channel::create_group(
            &harness.db,
            DataCreateGroup {
                ..Default::default()
            },
            user.id.clone(),
        )
        .await
        .expect("`Channel`");

        let message_id = ulid::Ulid::new().to_string();
        let response = harness
            .client
            .put(format!("/channels/{}/ack/{}", group.id(), message_id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NoContent);
        drop(response);

        let event = pubsub
            .wait_for_event(|event| match event {
                EventV1::ChannelAck { id, .. } => id == group.id(),
                _ => false,
            })
            .await;

        match event {
            EventV1::ChannelAck {
                message_id: m_id, ..
            } => assert_eq!(m_id, message_id),
            _ => unreachable!(),
        };
    }

    /// Contract the web app relies on: a forum has no messages of its own,
    /// and the app stores "seen up to the newest post" by acking the forum
    /// with a post's ULID (web `channels/forum/unread.ts`). The ack must take
    /// any ULID there, not only ids of the forum's own messages.
    #[rocket::async_test]
    async fn ack_a_forum_with_the_ulid_of_one_of_its_posts() {
        use revolt_database::Member;
        use rocket::http::ContentType;

        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        let (_, member_session, member) = harness.new_user().await;
        Member::create(&harness.db, &server, &member, None)
            .await
            .unwrap();
        let mut pubsub = PubSubTestHelper::new(&format!("{}!", member.id)).await;

        let response = harness
            .client
            .post(format!("/servers/{}/channels", server.id))
            .header(Header::new("x-session-token", owner_session.token.to_string()))
            .header(ContentType::JSON)
            .body(r#"{ "type": "Forum", "name": "help" }"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let forum: serde_json::Value = response.into_json().await.unwrap();
        let forum = forum["_id"].as_str().unwrap().to_string();

        let response = harness
            .client
            .post(format!("/channels/{forum}/threads"))
            .header(Header::new("x-session-token", owner_session.token.to_string()))
            .header(ContentType::JSON)
            .body(r#"{ "name": "Problem with tickets", "content": "first" }"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let post: serde_json::Value = response.into_json().await.unwrap();
        let post = post["_id"].as_str().unwrap().to_string();

        // the post's id is not a message of the forum
        let response = harness
            .client
            .put(format!("/channels/{forum}/ack/{post}"))
            .header(Header::new("x-session-token", member_session.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NoContent);
        drop(response);

        let event = pubsub
            .wait_for_event(|event| match event {
                EventV1::ChannelAck { id, .. } => id == &forum,
                _ => false,
            })
            .await;
        match event {
            EventV1::ChannelAck { message_id, .. } => assert_eq!(message_id, post),
            _ => unreachable!(),
        };
    }
}
