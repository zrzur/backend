use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, Message, MessageFilter, MessageQuery, MessageTimePeriod, User,
};
use revolt_models::v0::{self, MessageSort};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

/// # Fetch Messages
///
/// Fetch multiple messages.
#[openapi(tag = "Messaging")]
#[get("/<target>/messages?<options..>")]
pub async fn query(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: v0::OptionsQueryMessages,
) -> Result<Json<v0::BulkMessageResponse>> {
    options.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    if let Some(MessageSort::Relevance) = options.sort {
        return Err(create_error!(InvalidOperation));
    }

    let channel = target.as_channel(db).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;

    // Mature channels are for adults only
    revolt_database::util::age::assert_can_view_channel(db, &user, &channel).await?;

    let v0::OptionsQueryMessages {
        limit,
        before,
        after,
        sort,
        nearby,
        include_users,
    } = options;

    let intents = revolt_database::enabled_intents(db, &user).await;
    let direct_message = matches!(channel, revolt_database::Channel::DirectMessage { .. });
    let mut response = Message::fetch_with_users(
        db,
        MessageQuery {
            filter: MessageFilter {
                channel: Some(channel.id().to_string()),
                ..Default::default()
            },
            time_period: if let Some(nearby) = nearby {
                MessageTimePeriod::Relative { nearby }
            } else {
                MessageTimePeriod::Absolute {
                    before,
                    after,
                    sort,
                }
            },
            limit,
        },
        &user,
        include_users,
        channel.server(),
    )
    .await?;
    let messages = match &mut response {
        v0::BulkMessageResponse::JustMessages(messages) => messages,
        v0::BulkMessageResponse::MessagesAndUsers {
            messages, users, ..
        } => {
            revolt_database::hide_presence(db, &user, users).await;
            messages
        }
    };
    revolt_database::hide_message_content(messages, &user.id, direct_message, intents);
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{AccountAge, Server};
    use revolt_models::v0;
    use revolt_result::{Error, ErrorType};
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn mature_server_needs_adult() {
        let harness = TestHarness::new().await;
        let (mut account, session, user) = harness.new_user().await;

        let (_, channels) = Server::create(
            &harness.db,
            v0::DataCreateServer {
                name: "Mature".to_string(),
                nsfw: Some(true),
                ..Default::default()
            },
            &user,
            true,
        )
        .await
        .unwrap();
        let path = format!("/channels/{}/messages", channels[0].id());

        // A declared teen (an account without an age reaches nothing at all)
        let now = iso8601_timestamp::Timestamp::now_utc();
        account.age = Some(AccountAge::declare((now.year() - 17, u8::from(now.month()))));
        account.save(&harness.db).await.unwrap();

        let res = harness
            .client
            .get(path.clone())
            .header(Header::new("X-Session-Token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Forbidden);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::AgeRestricted,
        ));

        // Declared adult
        account.age = Some(AccountAge::declare((1990, 1)));
        account.save(&harness.db).await.unwrap();

        let res = harness
            .client
            .get(path)
            .header(Header::new("X-Session-Token", session.token))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);
    }
}
