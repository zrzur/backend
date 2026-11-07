use std::collections::HashSet;

use revolt_database::{
    util::permissions::DatabasePermissionQuery, Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// How far back the mentions inbox reaches
const MENTIONS_WINDOW_DAYS: u64 = 30;

/// # Fetch Mentions
///
/// Messages from the last 30 days that mention you: by name, by one of your
/// roles, or with @everyone / @here, in channels you can see. Newest first.
/// For people only: the whole messages and their authors would pass a bot's
/// intents.
#[openapi(tag = "User Information")]
#[get("/@me/mentions?<limit>")]
pub async fn fetch_mentions(
    db: &State<Database>,
    user: User,
    limit: Option<i64>,
) -> Result<Json<v0::BulkMessageResponse>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let limit = limit.unwrap_or(50).clamp(1, 100);

    // every channel this user can see: conversations, and the channels of
    // their servers that they may view
    let mut channels: Vec<Channel> = db.find_direct_messages(&user.id).await?;
    let memberships = db.fetch_all_memberships(&user.id).await?;
    let role_ids: Vec<String> = memberships
        .iter()
        .flat_map(|member| member.roles.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let server_ids: Vec<String> = memberships.iter().map(|m| m.id.server.clone()).collect();
    let servers = db.fetch_servers(&server_ids).await?;
    let server_channel_ids: Vec<String> = servers
        .iter()
        .flat_map(|server| server.channels.iter().cloned())
        .collect();
    channels.extend(db.fetch_channels(&server_channel_ids).await?);

    // mature channels and channels of mature Servers only for declared adults
    let mature = revolt_database::util::age::may_open_mature(db, &user).await?;
    let mature_servers: Vec<&str> = servers
        .iter()
        .filter(|server| server.nsfw)
        .map(|server| server.id.as_str())
        .collect();

    let mut visible = Vec::with_capacity(channels.len());
    for channel in &channels {
        if !mature
            && (channel.is_nsfw()
                || channel
                    .server()
                    .is_some_and(|server| mature_servers.contains(&server)))
        {
            continue;
        }

        let mut query = DatabasePermissionQuery::new(db, &user).channel(channel);
        if calculate_channel_permissions(&mut query)
            .await
            .has_channel_permission(ChannelPermission::ViewChannel)
        {
            visible.push(channel.id().to_string());
        }
    }

    if visible.is_empty() {
        return Ok(Json(v0::BulkMessageResponse::JustMessages(vec![])));
    }

    // message ids are ULIDs: everything at or after this id is newer than the window
    let since = ulid::Ulid::from_parts(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|now| now.as_millis() as u64)
            .unwrap_or_default()
            .saturating_sub(MENTIONS_WINDOW_DAYS * 24 * 60 * 60 * 1000),
        0,
    )
    .to_string();

    let messages: Vec<v0::Message> = db
        .fetch_mentions(&user.id, &role_ids, &visible, &since, limit)
        .await?
        .into_iter()
        .map(|message| message.into_model(None, None))
        .collect();

    let author_ids: Vec<String> = messages
        .iter()
        .map(|message| message.author.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let users = User::fetch_many_ids_as_mutuals(db, &user, &author_ids).await?;

    Ok(Json(v0::BulkMessageResponse::MessagesAndUsers {
        messages,
        users,
        members: None,
    }))
}

#[cfg(test)]
mod test {
    use revolt_database::{Member, Message, PartialChannel};
    use revolt_models::v0;
    use revolt_permissions::{ChannelPermission, OverrideField};
    use rocket::http::{Header, Status};

    use crate::util::test::TestHarness;

    /// A bot has no inbox: it would get @everyone messages and their authors'
    /// status past its intents
    #[rocket::async_test]
    async fn bots_get_no_mentions_inbox() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (bot, _) = harness.new_bot(&owner).await;
        let response = harness
            .client
            .get("/users/@me/mentions")
            .header(Header::new("x-bot-token", bot.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
        assert!(response.into_string().await.unwrap().contains("IsBot"));
    }

    /// You see messages that mention you, never your own, and nothing from a
    /// channel you can't view
    #[rocket::async_test]
    async fn mentions_inbox() {
        let harness = TestHarness::new().await;
        let (_, _, author) = harness.new_user().await;
        let (_, session, me) = harness.new_user().await;
        let (server, channels) = harness.new_server(&author).await;
        Member::create(&harness.db, &server, &author, Some(channels.clone())).await.unwrap();
        Member::create(&harness.db, &server, &me, Some(channels.clone())).await.unwrap();
        let open = channels[0].clone();

        // a second channel that only the author can see
        let mut hidden = harness.new_channel(&server).await;
        hidden
            .update(
                &harness.db,
                PartialChannel {
                    default_permissions: Some(OverrideField {
                        a: 0,
                        d: ChannelPermission::ViewChannel as i64,
                    }),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        let insert = |id: &str, channel: &str, author: &str, mentions: Vec<String>| Message {
            id: id.to_string(),
            channel: channel.to_string(),
            author: author.to_string(),
            content: Some("hello".to_string()),
            mentions: Some(mentions),
            ..Default::default()
        };
        let now = ulid::Ulid::new().to_string();
        let a = format!("{}A", &now[..25]);
        let b = format!("{}B", &now[..25]);
        let c = format!("{}C", &now[..25]);
        harness.db.insert_message(&insert(&a, open.id(), &author.id, vec![me.id.clone()])).await.unwrap();
        harness.db.insert_message(&insert(&b, hidden.id(), &author.id, vec![me.id.clone()])).await.unwrap();
        harness.db.insert_message(&insert(&c, open.id(), &me.id, vec![me.id.clone()])).await.unwrap();

        let response = harness
            .client
            .get("/users/@me/mentions")
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: v0::BulkMessageResponse = response.into_json().await.unwrap();
        let ids: Vec<String> = match body {
            v0::BulkMessageResponse::MessagesAndUsers { messages, .. } => {
                messages.into_iter().map(|m| m.id).collect()
            }
            v0::BulkMessageResponse::JustMessages(messages) => {
                messages.into_iter().map(|m| m.id).collect()
            }
        };
        assert_eq!(ids, vec![a]);
    }
}
