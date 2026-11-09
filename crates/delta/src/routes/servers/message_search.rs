use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, Message, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::routes::channels::message_search::search_query;

/// # Search for Messages in a Server
///
/// Searches the server's channels that you may read: you need to view the
/// channel and read its history, and mature channels are for declared adults
/// only. Other channels are left out silently, never reported. Takes the same
/// filters as a channel search, plus `channels` to search only some of them.
/// Bots may only list pinned messages (`pinned: true`).
#[openapi(tag = "Messaging")]
#[post("/<target>/messages/search", data = "<options>")]
pub async fn search_server(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: Json<v0::DataMessageSearch>,
) -> Result<Json<v0::BulkMessageResponse>> {
    let options = options.into_inner();
    let include_users = options.include_users;
    let mut message_query = search_query(&user, options)?;

    let server = target.as_server(db).await?;
    // not a member: the server is not there for you
    db.fetch_member(&server.id, &user.id)
        .await
        .map_err(|_| create_error!(NotFound))?;

    // the channels asked for, or all of them, and only those of this server
    let wanted: Vec<String> = match message_query.filter.channels.take() {
        Some(asked) => server
            .channels
            .iter()
            .filter(|id| asked.contains(id))
            .cloned()
            .collect(),
        None => server.channels.clone(),
    };

    let mature = revolt_database::util::age::may_open_mature(db, &user).await?;
    let mut readable = Vec::with_capacity(wanted.len());
    if mature || !server.nsfw {
        for channel in db.fetch_channels(&wanted).await? {
            if !mature && channel.is_nsfw() {
                continue;
            }
            let mut query = DatabasePermissionQuery::new(db, &user)
                .server(&server)
                .channel(&channel);
            let permissions = calculate_channel_permissions(&mut query).await;
            if permissions.has_channel_permission(ChannelPermission::ViewChannel)
                && permissions.has_channel_permission(ChannelPermission::ReadMessageHistory)
            {
                readable.push(channel.id().to_string());
            }
        }
    }

    if readable.is_empty() {
        return Ok(Json(if include_users == Some(true) {
            v0::BulkMessageResponse::MessagesAndUsers {
                messages: vec![],
                users: vec![],
                members: Some(vec![]),
            }
        } else {
            v0::BulkMessageResponse::JustMessages(vec![])
        }));
    }
    message_query.filter.channel = None;
    message_query.filter.channels = Some(readable);

    let intents = revolt_database::enabled_intents(db, &user).await;
    let mut response =
        Message::fetch_with_users(db, message_query, &user, include_users, Some(&server.id))
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
    revolt_database::hide_message_content(messages, &user.id, false, intents);
    Ok(Json(response))
}
