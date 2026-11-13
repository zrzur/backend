use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, MessageFlagsValue, PartialMessage, User,
};
use revolt_models::v0::{Embed, MessageFlags};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::Result;
use rocket::State;
use rocket_empty::EmptyResponse;

/// # Remove link previews
///
/// Removes the link previews from a message, for everyone, and keeps them
/// from coming back when the message is edited. Embeds a bot or webhook sent
/// on purpose stay. The author can do this, and so can anyone who may manage
/// messages in the channel.
#[openapi(tag = "Messaging")]
#[delete("/<target>/messages/<msg>/embeds")]
pub async fn message_embeds_remove(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
) -> Result<EmptyResponse> {
    let channel = target.as_channel(db).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;

    let mut message = msg.as_message_in_channel(db, channel.id()).await?;
    if message.author != user.id {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;
        revolt_database::assert_not_official_message(db, &message).await?;
    }

    let embeds: Vec<Embed> = message
        .embeds
        .iter()
        .flatten()
        .filter(|embed| matches!(embed, Embed::Text(_)))
        .cloned()
        .collect();

    let mut flags = MessageFlagsValue(message.flags.unwrap_or_default());
    flags.set(MessageFlags::SuppressEmbeds, true);

    message
        .update(
            db,
            PartialMessage {
                embeds: Some(embeds),
                flags: Some(flags.0),
                ..Default::default()
            },
            vec![],
        )
        .await?;

    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{util::reference::Reference, AppendMessage, Member, Message, Server};
    use revolt_models::v0::{self, Embed};
    use rocket::http::{Header, Status};
    use serde_json::json;

    fn preview() -> Embed {
        serde_json::from_value(json!({ "type": "Website", "title": "A linked page" })).unwrap()
    }

    fn card() -> Embed {
        serde_json::from_value(json!({ "type": "Text", "title": "A card a bot sent" })).unwrap()
    }

    async fn remove(harness: &TestHarness, token: &str, channel: &str, id: &str) -> Status {
        harness
            .client
            .delete(format!("/channels/{channel}/messages/{id}/embeds"))
            .header(Header::new("x-session-token", token.to_string()))
            .dispatch()
            .await
            .status()
    }

    async fn fetch(harness: &TestHarness, id: &str) -> Message {
        Reference::from_unchecked(id)
            .as_message(&harness.db)
            .await
            .expect("message")
    }

    #[rocket::async_test]
    async fn author_and_moderators_remove_previews_and_they_stay_removed() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (server, channels) = Server::create(
            &harness.db,
            v0::DataCreateServer {
                name: "Previews".to_string(),
                ..Default::default()
            },
            &owner,
            true,
        )
        .await
        .expect("server");

        let (_, author_session, author) = harness.new_user().await;
        let (channel, _, message) = harness.new_message(&author, &server, channels.clone()).await;
        let (_, other_session, other) = harness.new_user().await;
        Member::create(&harness.db, &server, &other, Some(channels))
            .await
            .expect("member");

        harness
            .db
            .append_message(&message.id, &AppendMessage { embeds: Some(vec![preview(), card()]) })
            .await
            .expect("seed embeds");

        // another member may not remove them
        assert_eq!(
            remove(&harness, &other_session.token, channel.id(), &message.id).await,
            Status::Forbidden
        );
        assert_eq!(fetch(&harness, &message.id).await.embeds.unwrap().len(), 2);

        // the author can: the preview goes, the bot's card stays, the flag is set
        assert_eq!(
            remove(&harness, &author_session.token, channel.id(), &message.id).await,
            Status::NoContent
        );
        let after = fetch(&harness, &message.id).await;
        assert!(matches!(after.embeds.as_deref(), Some([Embed::Text(_)])));
        assert!(after.has_suppressed_embeds());

        // previews that arrive later (the link check was still running) are dropped
        Message::append(
            &harness.db,
            message.id.clone(),
            channel.id().to_string(),
            AppendMessage { embeds: Some(vec![preview()]) },
        )
        .await
        .expect("append");
        assert_eq!(fetch(&harness, &message.id).await.embeds.unwrap().len(), 1);

        // the server owner manages messages, so may do it too; doing it again is fine
        assert_eq!(
            remove(&harness, &owner_session.token, channel.id(), &message.id).await,
            Status::NoContent
        );
        assert!(fetch(&harness, &message.id).await.has_suppressed_embeds());
        assert_ne!(owner.id, author.id);
    }
}
