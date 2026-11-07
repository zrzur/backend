use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, Message, MessageFilter, MessageQuery, MessageTimePeriod, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

/// # Search for Messages
///
/// This route searches for messages within the given parameters. Bots may
/// only list pinned messages (`pinned: true`): searching text would let a
/// bot read messages its intents hide.
#[openapi(tag = "Messaging")]
#[post("/<target>/search", data = "<options>")]
pub async fn search(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: Json<v0::DataMessageSearch>,
) -> Result<Json<v0::BulkMessageResponse>> {
    if user.bot.is_some() && options.pinned != Some(true) {
        return Err(create_error!(IsBot));
    }

    let options = options.into_inner();
    options.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    if options.query.is_some() && options.pinned.is_some() {
        return Err(create_error!(InvalidOperation));
    }

    let channel = target.as_channel(db).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;

    // Mature channels are for adults only
    revolt_database::util::age::assert_can_view_channel(db, &user, &channel).await?;

    let v0::DataMessageSearch {
        query,
        pinned,
        author,
        limit,
        before,
        after,
        sort,
        include_users,
    } = options;

    let intents = revolt_database::enabled_intents(db, &user).await;
    let direct_message = matches!(channel, revolt_database::Channel::DirectMessage { .. });
    let mut response = Message::fetch_with_users(
        db,
        MessageQuery {
            filter: MessageFilter {
                channel: Some(channel.id().to_string()),
                query,
                pinned,
                author,
                ..Default::default()
            },
            time_period: MessageTimePeriod::Absolute {
                before,
                after,
                sort: Some(sort),
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
