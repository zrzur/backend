use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Fetch Message
///
/// Retrieves a message by its id.
#[openapi(tag = "Messaging")]
#[get("/<target>/messages/<msg>")]
pub async fn fetch(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
) -> Result<Json<v0::Message>> {
    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let perms = calculate_channel_permissions(&mut query).await;

    perms.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    perms.throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;

    // Mature channels are for adults only
    revolt_database::util::age::assert_can_view_channel(db, &user, &channel).await?;

    let message = msg.as_message(db).await?;
    if message.channel != channel.id() {
        return Err(create_error!(NotFound));
    }

    let mut message = message.into_model(None, None);
    revolt_database::hide_message_content(
        std::slice::from_mut(&mut message),
        &user.id,
        matches!(channel, revolt_database::Channel::DirectMessage { .. }),
        revolt_database::enabled_intents(db, &user).await,
    );
    Ok(Json(message))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Member, PartialBot};
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};

    #[rocket::async_test]
    async fn without_message_content_a_bot_reads_no_text() {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        let channel = &channels[0];
        let (mut bot, bot_user) = harness.new_bot(&owner).await;
        Member::create(&harness.db, &server, &bot_user, None).await.unwrap();

        let send = |content: String| {
            harness
                .client
                .post(format!("/channels/{}/messages", channel.id()))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", session.token.clone()))
                .body(json!({ "content": content }).to_string())
        };
        let plain: v0::Message = send("secret plans".into()).dispatch().await.into_json().await.unwrap();
        let mention: v0::Message = send(format!("hey <@{}>", bot.id)).dispatch().await.into_json().await.unwrap();

        let read = |id: String, token: String| {
            harness
                .client
                .get(format!("/channels/{}/messages/{}", channel.id(), id))
                .header(Header::new("x-bot-token", token))
        };
        let response = read(plain.id.clone(), bot.token.clone()).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let got: v0::Message = response.into_json().await.unwrap();
        assert_eq!(got.content.as_deref(), Some(""));
        assert_eq!(got.author, owner.id);

        // a message that mentions the bot is whole
        let got: v0::Message = read(mention.id.clone(), bot.token.clone()).dispatch().await.into_json().await.unwrap();
        assert!(got.content.unwrap().starts_with("hey"));

        // the history reads the same way
        let page: Vec<v0::Message> = harness
            .client
            .get(format!("/channels/{}/messages", channel.id()))
            .header(Header::new("x-bot-token", bot.token.clone()))
            .dispatch()
            .await
            .into_json()
            .await
            .unwrap();
        let hidden = page.iter().find(|m| m.id == plain.id).unwrap();
        assert_eq!(hidden.content.as_deref(), Some(""));

        // with the intent enabled the text is there
        bot.update(
            &harness.db,
            PartialBot {
                privileged_intents: Some(v0::GatewayIntents::MESSAGE_CONTENT as i64),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
        let got: v0::Message = read(plain.id.clone(), bot.token.clone()).dispatch().await.into_json().await.unwrap();
        assert_eq!(got.content.as_deref(), Some("secret plans"));
    }
}
