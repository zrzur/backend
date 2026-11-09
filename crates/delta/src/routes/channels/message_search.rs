use iso8601_timestamp::Timestamp;
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, Message, MessageFilter, MessageQuery, MessageTimePeriod, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

/// A query for the given search options, before the channel filter
///
/// Checks the options a derive cannot (every id in a list), joins `author`
/// with `authors`, and turns the dates into message id bounds: an id starts
/// with the moment its message was sent.
pub fn search_query(user: &User, options: v0::DataMessageSearch) -> Result<MessageQuery> {
    // Bots may only list pinned messages: searching text would let a bot
    // read messages its intents hide
    if user.bot.is_some() && options.pinned != Some(true) {
        return Err(create_error!(IsBot));
    }

    options
        .validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let is_id = |id: &String| id.len() == 26 && id.chars().all(|c| c.is_ascii_alphanumeric());
    if !options.authors.iter().flatten().all(is_id)
        || !options.channels.iter().flatten().all(is_id)
    {
        return Err(create_error!(InvalidOperation));
    }

    let v0::DataMessageSearch {
        query,
        pinned,
        author,
        authors,
        mentions,
        has,
        before_date,
        after_date,
        channels,
        limit,
        before,
        after,
        sort,
        include_users: _,
    } = options;

    // ids are stored in upper case; a lower-case id would match nothing
    let upper = |id: String| id.to_ascii_uppercase();
    let (author, mentions, before, after) = (
        author.map(upper),
        mentions.map(upper),
        before.map(upper),
        after.map(upper),
    );
    let channels = channels.map(|list| list.into_iter().map(upper).collect::<Vec<_>>());

    let mut authors: Vec<String> = author
        .into_iter()
        .chain(authors.into_iter().flatten().map(upper))
        .collect();
    authors.sort();
    authors.dedup();

    let bound = |time: Timestamp| {
        let ms = time.duration_since(Timestamp::UNIX_EPOCH).whole_milliseconds().max(0) as u64;
        ulid::Ulid::from_parts(ms, 0).to_string()
    };
    // the earlier of the two upper bounds, the later of the two lower ones
    let before = match (before, before_date.map(bound)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let after = match (after, after_date.map(bound)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };

    let mut has = has.unwrap_or_default();
    has.sort_by_key(|kind| format!("{kind:?}"));
    has.dedup();

    Ok(MessageQuery {
        filter: MessageFilter {
            query,
            pinned,
            authors: (!authors.is_empty()).then_some(authors),
            mentions,
            has: (!has.is_empty()).then_some(has),
            channels,
            ..Default::default()
        },
        time_period: MessageTimePeriod::Absolute {
            before,
            after,
            sort: Some(sort),
        },
        limit,
    })
}

/// # Search for Messages
///
/// This route searches for messages within the given parameters: words,
/// authors, a mentioned user, what a message has (`link`, `file`, `image`,
/// `video`, `embed`, `sound`), a time window by id or date, and pinned. Every
/// filter given must match. Bots may only list pinned messages
/// (`pinned: true`): searching text would let a bot read messages its
/// intents hide.
#[openapi(tag = "Messaging")]
#[post("/<target>/search", data = "<options>")]
pub async fn search(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: Json<v0::DataMessageSearch>,
) -> Result<Json<v0::BulkMessageResponse>> {
    let options = options.into_inner();
    let include_users = options.include_users;
    let mut message_query = search_query(&user, options)?;

    let channel = target.as_channel(db).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;

    // Mature channels are for adults only
    revolt_database::util::age::assert_can_view_channel(db, &user, &channel).await?;

    // this route searches one channel: a channel list is for server-wide search
    message_query.filter.channel = Some(channel.id().to_string());
    message_query.filter.channels = None;

    let intents = revolt_database::enabled_intents(db, &user).await;
    let direct_message = matches!(channel, revolt_database::Channel::DirectMessage { .. });
    let mut response =
        Message::fetch_with_users(db, message_query, &user, include_users, channel.server())
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
