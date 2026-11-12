use revolt_database::{Database, PartialUser, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Fetch Privacy Settings
///
/// Who can message you and who can add you to group chats.
#[openapi(tag = "User Information")]
#[get("/@me/privacy")]
pub async fn fetch_privacy(db: &State<Database>, user: User) -> Result<Json<v0::UserPrivacy>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    Ok(Json(user.effective_privacy(db).await))
}

/// # Edit Privacy Settings
///
/// Change who can message you and who can add you to group chats.
/// Fields left out stay the same.
#[openapi(tag = "User Information")]
#[patch("/@me/privacy", data = "<data>")]
pub async fn edit_privacy(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataEditPrivacy>,
) -> Result<Json<v0::UserPrivacy>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let data = data.into_inner();
    // start from what is enforced now, so changing one setting never
    // loosens the other (for example the friends-only default under 18)
    let mut privacy = user.effective_privacy(db).await;
    if let Some(direct_messages) = data.direct_messages {
        privacy.direct_messages = direct_messages;
    }
    if let Some(group_invites) = data.group_invites {
        privacy.group_invites = group_invites;
    }
    // under 18 the server keeps direct messages to friends, so that is what
    // is saved and answered, whatever was asked for
    let mut privacy_user = user.clone();
    privacy_user.privacy = Some(privacy);
    let privacy = privacy_user.effective_privacy(db).await;

    // Written directly: a user update event would go to everyone
    // subscribed to this user, and the settings are private
    db.update_user(
        &user.id,
        &PartialUser {
            privacy: Some(privacy.clone()),
            ..Default::default()
        },
        vec![],
    )
    .await?;

    Ok(Json(privacy))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Channel, RelationshipStatus, User};
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};

    async fn set_privacy(
        harness: &TestHarness,
        token: &str,
        body: serde_json::Value,
    ) -> (Status, Option<v0::UserPrivacy>) {
        let response = harness
            .client
            .patch("/users/@me/privacy")
            .header(Header::new("x-session-token", token.to_string()))
            .header(ContentType::JSON)
            .body(body.to_string())
            .dispatch()
            .await;
        let status = response.status();
        let body = if status == Status::Ok {
            response.into_json::<v0::UserPrivacy>().await
        } else {
            None
        };
        (status, body)
    }

    async fn error_type(response: rocket::local::asynchronous::LocalResponse<'_>) -> String {
        let body: serde_json::Value = response.into_json().await.expect("error body");
        body["type"].as_str().unwrap_or_default().to_string()
    }

    async fn befriend(harness: &TestHarness, a: &mut User, b: &mut User) {
        #[allow(clippy::disallowed_methods)]
        a.apply_relationship(
            &harness.db,
            b,
            RelationshipStatus::Friend,
            RelationshipStatus::Friend,
        )
        .await
        .unwrap();
    }

    async fn block(harness: &TestHarness, a: &mut User, b: &mut User) {
        #[allow(clippy::disallowed_methods)]
        a.apply_relationship(
            &harness.db,
            b,
            RelationshipStatus::Blocked,
            RelationshipStatus::BlockedOther,
        )
        .await
        .unwrap();
    }

    async fn share_group(harness: &TestHarness, a: &User, b: &User) -> Channel {
        let mut group = Channel::create_group(
            &harness.db,
            v0::DataCreateGroup {
                name: TestHarness::rand_string(),
                ..Default::default()
            },
            a.id.to_string(),
        )
        .await
        .unwrap();
        group
            .add_user_to_group(&harness.db, &harness.amqp, b, &a.id)
            .await
            .unwrap();
        group
    }

    async fn open_dm(harness: &TestHarness, token: &str, target: &str) -> (Status, String) {
        let response = harness
            .client
            .get(format!("/users/{target}/dm"))
            .header(Header::new("x-session-token", token.to_string()))
            .dispatch()
            .await;
        let status = response.status();
        let kind = if status == Status::Ok {
            String::new()
        } else {
            error_type(response).await
        };
        (status, kind)
    }

    async fn send(harness: &TestHarness, token: &str, channel: &str) -> (Status, String) {
        let response = harness
            .client
            .post(format!("/channels/{channel}/messages"))
            .header(Header::new("x-session-token", token.to_string()))
            .header(ContentType::JSON)
            .body(json!({ "content": "hello" }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        let kind = if status == Status::Ok {
            String::new()
        } else {
            error_type(response).await
        };
        (status, kind)
    }

    #[rocket::async_test]
    async fn defaults_and_partial_edit() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let response = harness
            .client
            .get("/users/@me/privacy")
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let privacy: v0::UserPrivacy = response.into_json().await.unwrap();
        assert_eq!(privacy, v0::UserPrivacy::default());
        assert_eq!(privacy.direct_messages, v0::DirectMessagePrivacy::Mutual);
        assert_eq!(privacy.group_invites, v0::GroupInvitePrivacy::Friends);

        let (status, privacy) = set_privacy(
            &harness,
            &session.token,
            json!({ "group_invites": "Nobody" }),
        )
        .await;
        assert_eq!(status, Status::Ok);
        let privacy = privacy.unwrap();
        assert_eq!(privacy.direct_messages, v0::DirectMessagePrivacy::Mutual);
        assert_eq!(privacy.group_invites, v0::GroupInvitePrivacy::Nobody);

        let stored = harness.db.fetch_user(&user.id).await.unwrap();
        assert_eq!(stored.privacy(), privacy);
    }

    #[rocket::async_test]
    async fn under_18_default_is_friends_only() {
        let harness = TestHarness::new().await;
        let (mut account, session, teen) = harness.new_user().await;
        let now = revolt_database::iso8601_timestamp::Timestamp::now_utc();
        account.age = Some(revolt_database::AccountAge::declare((
            now.year() - 16,
            u8::from(now.month()),
        )));
        account.save(&harness.db).await.unwrap();

        let response = harness
            .client
            .get("/users/@me/privacy")
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;
        let privacy: v0::UserPrivacy = response.into_json().await.unwrap();
        assert_eq!(privacy.direct_messages, v0::DirectMessagePrivacy::Friends);

        // a shared group is not enough for a stranger
        let (_, session_s, stranger) = harness.new_user().await;
        share_group(&harness, &stranger, &teen).await;
        let (status, kind) = open_dm(&harness, &session_s.token, &teen.id).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(kind, "DirectMessagesRestricted");

        // changing only group adds keeps the friends-only default
        let (_, privacy) = set_privacy(
            &harness,
            &session.token,
            json!({ "group_invites": "Nobody" }),
        )
        .await;
        assert_eq!(
            privacy.unwrap().direct_messages,
            v0::DirectMessagePrivacy::Friends
        );

        // a wider choice is not taken: the server keeps friends only (PR-9)
        for wider in ["Mutual", "Everyone"] {
            let (status, privacy) = set_privacy(
                &harness,
                &session.token,
                json!({ "direct_messages": wider }),
            )
            .await;
            assert_eq!(status, Status::Ok);
            assert_eq!(
                privacy.unwrap().direct_messages,
                v0::DirectMessagePrivacy::Friends,
                "{wider}"
            );
            let (status, kind) = open_dm(&harness, &session_s.token, &teen.id).await;
            assert_eq!(status, Status::Forbidden, "{wider}");
            assert_eq!(kind, "DirectMessagesRestricted");
        }
        let stored = harness.db.fetch_user(&teen.id).await.unwrap();
        assert_eq!(stored.privacy().direct_messages, v0::DirectMessagePrivacy::Friends);
    }

    /// A wider setting saved before (or written around the route) does not
    /// open direct messages to someone aged 16 or 17, not even for a
    /// stranger who shares nothing with them
    #[rocket::async_test]
    async fn under_18_saved_wider_setting_is_not_enforced() {
        let harness = TestHarness::new().await;
        let (mut account, session, teen) = harness.new_user().await;
        let now = revolt_database::iso8601_timestamp::Timestamp::now_utc();
        account.age = Some(revolt_database::AccountAge::declare((
            now.year() - 17,
            u8::from(now.month()),
        )));
        account.save(&harness.db).await.unwrap();
        harness
            .db
            .update_user(
                &teen.id,
                &revolt_database::PartialUser {
                    privacy: Some(v0::UserPrivacy {
                        direct_messages: v0::DirectMessagePrivacy::Everyone,
                        group_invites: v0::GroupInvitePrivacy::Everyone,
                    }),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        let response = harness
            .client
            .get("/users/@me/privacy")
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;
        let privacy: v0::UserPrivacy = response.into_json().await.unwrap();
        assert_eq!(privacy.direct_messages, v0::DirectMessagePrivacy::Friends);
        // the other setting stays theirs
        assert_eq!(privacy.group_invites, v0::GroupInvitePrivacy::Everyone);

        // a stranger sharing nothing
        let (_, session_s, mut stranger) = harness.new_user().await;
        let (status, kind) = open_dm(&harness, &session_s.token, &teen.id).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(kind, "DirectMessagesRestricted");

        // a stranger sharing a group
        share_group(&harness, &stranger, &teen).await;
        let (status, _) = open_dm(&harness, &session_s.token, &teen.id).await;
        assert_eq!(status, Status::Forbidden);

        // a friend can
        let mut teen = harness.db.fetch_user(&teen.id).await.unwrap();
        befriend(&harness, &mut stranger, &mut teen).await;
        let (status, _) = open_dm(&harness, &session_s.token, &teen.id).await;
        assert_eq!(status, Status::Ok);
    }

    #[rocket::async_test]
    async fn rejects_unknown_values() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;

        let (status, _) = set_privacy(
            &harness,
            &session.token,
            json!({ "direct_messages": "Strangers" }),
        )
        .await;
        assert!(status.code >= 400 && status.code < 500, "{status}");
    }

    #[rocket::async_test]
    async fn privacy_is_not_sent_to_others() {
        let harness = TestHarness::new().await;
        let (_, session_a, mut user_a) = harness.new_user().await;
        let (_, session_b, mut user_b) = harness.new_user().await;
        befriend(&harness, &mut user_a, &mut user_b).await;
        set_privacy(
            &harness,
            &session_a.token,
            json!({ "direct_messages": "Friends" }),
        )
        .await;

        let response = harness
            .client
            .get(format!("/users/{}", user_a.id))
            .header(Header::new("x-session-token", session_b.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body = response.into_string().await.unwrap();
        assert!(!body.contains("privacy"), "{body}");
        assert!(!body.contains("direct_messages"), "{body}");
    }

    #[rocket::async_test]
    async fn dm_default_allows_shared_group_not_strangers() {
        let harness = TestHarness::new().await;
        let (_, session_a, user_a) = harness.new_user().await;
        let (_, _, user_b) = harness.new_user().await;

        // No shared server or group yet
        let (status, kind) = open_dm(&harness, &session_a.token, &user_b.id).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(kind, "DirectMessagesRestricted");

        // Sharing a group is enough by default
        share_group(&harness, &user_b, &user_a).await;
        let (status, _) = open_dm(&harness, &session_a.token, &user_b.id).await;
        assert_eq!(status, Status::Ok);
    }

    #[rocket::async_test]
    async fn dm_everyone_and_friends_only() {
        let harness = TestHarness::new().await;
        let (_, session_a, mut user_a) = harness.new_user().await;
        let (_, session_b, mut user_b) = harness.new_user().await;

        // Everyone: a stranger can open the DM and write first
        set_privacy(
            &harness,
            &session_b.token,
            json!({ "direct_messages": "Everyone" }),
        )
        .await;
        let response = harness
            .client
            .get(format!("/users/{}/dm", user_b.id))
            .header(Header::new("x-session-token", session_a.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let channel: v0::Channel = response.into_json().await.unwrap();
        let (status, _) = send(&harness, &session_a.token, channel.id()).await;
        assert_eq!(status, Status::Ok);

        // Friends only: people in a shared group are refused
        let (_, session_c, user_c) = harness.new_user().await;
        share_group(&harness, &user_b, &user_c).await;
        set_privacy(
            &harness,
            &session_b.token,
            json!({ "direct_messages": "Friends" }),
        )
        .await;
        let (status, kind) = open_dm(&harness, &session_c.token, &user_b.id).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(kind, "DirectMessagesRestricted");

        // ... but friends always can
        befriend(&harness, &mut user_a, &mut user_b).await;
        let (status, _) = open_dm(&harness, &session_a.token, &user_b.id).await;
        assert_eq!(status, Status::Ok);
    }

    #[rocket::async_test]
    async fn dm_first_message_refused_and_reply_allowed() {
        let harness = TestHarness::new().await;
        let (_, session_a, user_a) = harness.new_user().await;
        let (_, session_b, user_b) = harness.new_user().await;

        // b only accepts friends, a accepts everyone
        set_privacy(
            &harness,
            &session_b.token,
            json!({ "direct_messages": "Friends" }),
        )
        .await;
        set_privacy(
            &harness,
            &session_a.token,
            json!({ "direct_messages": "Everyone" }),
        )
        .await;

        // A DM channel that already exists does not let a write first
        let channel = Channel::create_dm(&harness.db, &user_a, &user_b)
            .await
            .unwrap();
        let (status, kind) = send(&harness, &session_a.token, channel.id()).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(kind, "DirectMessagesRestricted");

        // b writes first, so a can reply and open the DM again
        let (status, _) = send(&harness, &session_b.token, channel.id()).await;
        assert_eq!(status, Status::Ok);
        let (status, _) = send(&harness, &session_a.token, channel.id()).await;
        assert_eq!(status, Status::Ok);
        let (status, _) = open_dm(&harness, &session_a.token, &user_b.id).await;
        assert_eq!(status, Status::Ok);
    }

    #[rocket::async_test]
    async fn dm_blocked_never() {
        let harness = TestHarness::new().await;
        let (_, session_a, mut user_a) = harness.new_user().await;
        let (_, session_b, mut user_b) = harness.new_user().await;

        set_privacy(
            &harness,
            &session_b.token,
            json!({ "direct_messages": "Everyone" }),
        )
        .await;
        block(&harness, &mut user_b, &mut user_a).await;

        let (status, kind) = open_dm(&harness, &session_a.token, &user_b.id).await;
        assert_eq!(status, Status::Forbidden);
        assert_ne!(kind, "DirectMessagesRestricted");
    }

    async fn add_to_group(harness: &TestHarness, token: &str, group: &str, user: &str) -> (Status, String) {
        let response = harness
            .client
            .put(format!("/channels/{group}/recipients/{user}"))
            .header(Header::new("x-session-token", token.to_string()))
            .dispatch()
            .await;
        let status = response.status();
        let kind = if status == Status::NoContent {
            String::new()
        } else {
            error_type(response).await
        };
        (status, kind)
    }

    async fn own_group(harness: &TestHarness, owner: &User) -> Channel {
        Channel::create_group(
            &harness.db,
            v0::DataCreateGroup {
                name: TestHarness::rand_string(),
                ..Default::default()
            },
            owner.id.to_string(),
        )
        .await
        .unwrap()
    }

    #[rocket::async_test]
    async fn group_add_follows_setting() {
        let harness = TestHarness::new().await;
        let (_, session_a, mut user_a) = harness.new_user().await;
        let (_, session_b, mut user_b) = harness.new_user().await;
        let (_, session_c, user_c) = harness.new_user().await;

        // Default: friends only
        let group = own_group(&harness, &user_a).await;
        let (status, kind) = add_to_group(&harness, &session_a.token, group.id(), &user_b.id).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(kind, "GroupInvitesRestricted");

        befriend(&harness, &mut user_a, &mut user_b).await;
        let (status, _) = add_to_group(&harness, &session_a.token, group.id(), &user_b.id).await;
        assert_eq!(status, Status::NoContent);

        // Everyone: a stranger can add b
        set_privacy(
            &harness,
            &session_b.token,
            json!({ "group_invites": "Everyone" }),
        )
        .await;
        let group_c = own_group(&harness, &user_c).await;
        let (status, _) = add_to_group(&harness, &session_c.token, group_c.id(), &user_b.id).await;
        assert_eq!(status, Status::NoContent);

        // Nobody: not even a friend, and not at group creation either
        set_privacy(
            &harness,
            &session_b.token,
            json!({ "group_invites": "Nobody" }),
        )
        .await;
        let group_2 = own_group(&harness, &user_a).await;
        let (status, kind) = add_to_group(&harness, &session_a.token, group_2.id(), &user_b.id).await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(kind, "GroupInvitesRestricted");

        let response = harness
            .client
            .post("/channels/create")
            .header(Header::new("x-session-token", session_a.token.to_string()))
            .header(ContentType::JSON)
            .body(json!({ "name": "group", "users": [user_b.id] }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Forbidden);
        assert_eq!(error_type(response).await, "GroupInvitesRestricted");

        // b can still make their own group
        let response = harness
            .client
            .post("/channels/create")
            .header(Header::new("x-session-token", session_b.token.to_string()))
            .header(ContentType::JSON)
            .body(json!({ "name": "mine", "users": [user_a.id] }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
    }
}
