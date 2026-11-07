use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{
    calculate_channel_permissions, calculate_user_permissions, ChannelPermission, PermissionQuery,
    RelationshipStatus, UserPermission,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Open Direct Message
///
/// Open a DM with another user.
///
/// If the target is oneself, a saved messages channel is returned.
///
/// Fails with `DirectMessagesRestricted` when the other user's privacy
/// settings don't let you start a conversation with them.
#[openapi(tag = "Direct Messaging")]
#[get("/<target>/dm")]
pub async fn open_dm(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::Channel>> {
    let target = target.as_user(db).await?;

    // nobody starts a conversation with an official account: only the one
    // it started opens, read-only (the channel permissions stop anyone
    // writing in it)
    if target.official {
        return db
            .find_direct_message_channel(&target.id, &user.id)
            .await
            .map(|channel| Json(channel.into()))
            .map_err(|_| create_error!(OfficialAccount));
    }

    let mut query = DatabasePermissionQuery::new(db, &user).user(&target);
    let permissions = calculate_user_permissions(&mut query).await;

    if !permissions.has_user_permission(UserPermission::SendMessage) {
        let blocked = matches!(
            query.user_relationship().await,
            RelationshipStatus::Blocked | RelationshipStatus::BlockedOther
        );

        if blocked {
            permissions.throw_if_lacking_user_permission(UserPermission::SendMessage)?;
        }

        // An existing conversation they started stays open
        if let Ok(channel) = db.find_direct_message_channel(&user.id, &target.id).await {
            let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
            if calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage)
            {
                return Ok(Json(channel.into()));
            }
        }

        return Err(create_error!(DirectMessagesRestricted));
    }

    // A shared server paused DMs between members who aren't friends; a
    // conversation that already exists can still be opened (sending stays
    // refused while the pause lasts)
    if revolt_database::util::raid::dms_paused_between(db, &user, &target).await?
        && db.find_direct_message_channel(&user.id, &target.id).await.is_err()
    {
        return Err(create_error!(DirectMessagesPaused));
    }

    Channel::create_dm(db, &user, &target)
        .await
        .map(Into::into)
        .map(Json)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::Member;
    use rocket::http::{ContentType, Header, Status};

    /// A person and a bot in the same server
    async fn setup() -> (TestHarness, String, String, String, String) {
        let harness = TestHarness::new().await;
        let (_, person_session, person) = harness.new_user().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        let (bot, bot_user) = harness.new_bot(&owner).await;
        Member::create(&harness.db, &server, &person, None).await.unwrap();
        Member::create(&harness.db, &server, &bot_user, None).await.unwrap();
        (harness, person_session.token, person.id, bot.token, bot.id)
    }

    async fn privacy(harness: &TestHarness, token: &str, direct_messages: &str) {
        let response = harness
            .client
            .patch("/users/@me/privacy")
            .header(Header::new("x-session-token", token.to_string()))
            .header(ContentType::JSON)
            .body(json!({ "direct_messages": direct_messages }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
    }

    async fn bot_opens(harness: &TestHarness, bot_token: &str, person: &str) -> (Status, String) {
        let response = harness
            .client
            .get(format!("/users/{person}/dm"))
            .header(Header::new("x-bot-token", bot_token.to_string()))
            .dispatch()
            .await;
        (response.status(), response.into_string().await.unwrap_or_default())
    }

    #[rocket::async_test]
    async fn nobody_starts_or_befriends_the_official_account() {
        let harness = TestHarness::new().await;
        revolt_database::seed_official_accounts(&harness.db).await;
        let (_, session, _) = harness.new_user().await;
        let official = revolt_database::OfficialAccount::Zrzur.id();
        let token = Header::new("x-session-token", session.token.clone());

        let response = harness
            .client
            .get(format!("/users/{official}/dm"))
            .header(token.clone())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Forbidden);

        let response = harness
            .client
            .post("/users/friend")
            .header(token)
            .header(ContentType::JSON)
            .body(json!({ "username": "Zrzur#0001" }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Forbidden);
    }

    #[rocket::async_test]
    async fn friends_only_privacy_keeps_bots_out() {
        let (harness, person_token, person, bot_token, _) = setup().await;
        privacy(&harness, &person_token, "Friends").await;
        let (status, body) = bot_opens(&harness, &bot_token, &person).await;
        assert_ne!(status, Status::Ok);
        assert!(body.contains("DirectMessagesRestricted"), "{body}");
    }

    #[rocket::async_test]
    async fn mutual_privacy_lets_a_shared_server_bot_in() {
        let (harness, person_token, person, bot_token, _) = setup().await;
        privacy(&harness, &person_token, "Mutual").await;
        let (status, body) = bot_opens(&harness, &bot_token, &person).await;
        assert_eq!(status, Status::Ok, "{body}");
    }

    #[rocket::async_test]
    async fn a_conversation_the_person_started_stays_open() {
        let (harness, person_token, person, bot_token, bot) = setup().await;
        privacy(&harness, &person_token, "Friends").await;
        // the person writes to the bot first
        let channel: serde_json::Value = harness
            .client
            .get(format!("/users/{bot}/dm"))
            .header(Header::new("x-session-token", person_token.clone()))
            .dispatch()
            .await
            .into_json()
            .await
            .expect("a DM with the bot");
        let channel_id = channel["_id"].as_str().unwrap().to_string();
        let response = harness
            .client
            .post(format!("/channels/{channel_id}/messages"))
            .header(Header::new("x-session-token", person_token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "content": "hello bot" }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let (status, body) = bot_opens(&harness, &bot_token, &person).await;
        assert_eq!(status, Status::Ok, "{body}");
    }
}
