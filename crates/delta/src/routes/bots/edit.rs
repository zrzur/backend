use revolt_database::{util::reference::Reference, Database, PartialBot, User, ValidatedTicket};
use revolt_models::v0::{self, DataEditBot};
use revolt_result::{create_error, Result};
use rocket::State;

use rocket::serde::json::Json;
use validator::Validate;

/// # Edit Bot
///
/// Edit bot details by its id.
#[openapi(tag = "Bots")]
#[patch("/<bot_id>", data = "<data>")]
pub async fn edit_bot(
    db: &State<Database>,
    user: User,
    validated_ticket: Option<ValidatedTicket>,
    bot_id: Reference<'_>,
    data: Json<DataEditBot>,
) -> Result<Json<v0::BotWithUserResponse>> {
    let data = data.into_inner();

    // who may touch this bot comes first: anyone else learns nothing more
    // than "not found", whatever else is wrong with the request
    let mut bot = bot_id.as_bot(db).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }

    // Deprecated: use POST /bots/<id>/token/reset. Like it, a new token
    // needs the owner to sign in again (X-MFA-Ticket).
    if data.remove.contains(&v0::FieldsBot::Token) && validated_ticket.is_none() {
        return Err(create_error!(InvalidCredentials));
    }

    // Everything is checked before anything changes, so a refused request
    // leaves the bot as it was (the name was changed before, then a 400)
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // only the three privileged intents are switched on or off here
    if data.privileged_intents.is_some_and(|bits| bits < 0 || (bits as u64) & !v0::GatewayIntents::PRIVILEGED != 0) {
        return Err(create_error!(FailedValidation {
            error: "privileged_intents: only MESSAGE_CONTENT, SERVER_MEMBERS and PRESENCE".to_string()
        }));
    }

    // permission bits the server knows, nothing past them
    if data.default_permissions.is_some_and(|bits| {
        bits < 0 || (bits as u64) & !(revolt_permissions::ChannelPermission::GrantAllSafe as u64) != 0
    }) {
        return Err(create_error!(FailedValidation {
            error: "default_permissions: unknown permission bits".to_string()
        }));
    }

    // a web address only: no javascript:, data: or other schemes reach a
    // client. An empty string is refused too: to clear the address, send
    // `"remove": ["InteractionsURL"]`.
    if let Some(url) = &data.interactions_url {
        let valid = url::Url::parse(url).is_ok_and(|parsed| {
            matches!(parsed.scheme(), "https" | "http")
                && parsed.host_str().is_some_and(|host| !host.is_empty())
                && parsed.username().is_empty()
                && parsed.password().is_none()
        });
        if !valid || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(create_error!(FailedValidation {
                error: "interactions_url must be an http or https address".to_string()
            }));
        }
    }

    let mut user = db.fetch_user(&bot.id).await?;
    if data.name.is_some() && revolt_database::is_official_bot_flag(&bot) {
        // a system bot keeps the name it was given on the server
        return Err(create_error!(OfficialAccount));
    }
    if let Some(name) = data.name {
        let previous = user.username.clone();
        // the username and the display name change one after the other: the
        // servers the bot is in hear about the new name once, at the end
        let mut renamed = user.update_username_held(db, v0::bot_username(&name)).await?;
        match v0::bot_display_name(&name) {
            // a name with spaces is shown as typed
            Some(display_name) => {
                renamed |= user
                    .update_held(
                        db,
                        revolt_database::PartialUser {
                            display_name: Some(display_name),
                            ..Default::default()
                        },
                        vec![],
                    )
                    .await?
            }
            // a name without one: the display name made from the old name
            // goes, so the new name shows; one set some other way stays
            None if user
                .display_name
                .as_deref()
                .is_some_and(|shown| shown.replace(' ', "_") == previous) =>
            {
                renamed |= user
                    .update_held(
                        db,
                        Default::default(),
                        vec![revolt_database::FieldsUser::DisplayName],
                    )
                    .await?
            }
            None => {}
        }
        if renamed {
            user.announce_appearance(db);
        }
    }

    if data.public.is_none()
        && data.analytics.is_none()
        && data.interactions_url.is_none()
        && data.remove.is_empty()
        && data.default_permissions.is_none()
        && data.privileged_intents.is_none()
        && data.show_creator.is_none()
    {
        return Ok(Json(v0::BotWithUserResponse {
            bot: bot.into(),
            user: user.into_self(false).await,
        }));
    }

    let DataEditBot {
        public,
        analytics,
        interactions_url,
        remove,
        default_permissions,
        privileged_intents,
        show_creator,
        ..
    } = data;

    let partial = PartialBot {
        public,
        analytics,
        interactions_url,
        default_permissions,
        privileged_intents,
        show_creator,
        ..Default::default()
    };

    bot.update(
        db,
        partial,
        remove
            .into_iter()
            .map(|v| v.into())
            .collect(),
    )
    .await?;
    // who may read its commands follows the public flag
    crate::util::public_commands::forget(&bot.id);

    Ok(Json(v0::BotWithUserResponse {
        bot: bot.into(),
        user: user.into_self(false).await,
    }))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Bot, MFATicket};
    use revolt_models::v0::{self, FieldsBot};
    use rocket::http::{ContentType, Header, Status};

    #[rocket::async_test]
    async fn edit_bot() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");
        // a new token needs the owner to sign in again
        let ticket = MFATicket::new(user.id.to_string(), true);
        ticket.save(&harness.db).await.unwrap();

        let response = harness
            .client
            .patch(format!("/bots/{}", bot.id))
            .header(Header::new("x-mfa-ticket", ticket.token.clone()))
            .header(ContentType::JSON)
            .body(
                json!(v0::DataEditBot {
                    public: Some(true),
                    remove: vec![FieldsBot::Token],
                    ..Default::default()
                })
                .to_string(),
            )
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);

        let updated_bot: v0::Bot = response.into_json().await.expect("`Bot`");
        assert!(!bot.public);
        assert!(updated_bot.public);
    }

    #[rocket::async_test]
    async fn privileged_intents_are_switched_on_by_the_owner() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");
        // new bots start with none
        assert_eq!(bot.privileged_intents, 0);

        let patch = |bits: i64| {
            harness
                .client
                .patch(format!("/bots/{}", bot.id))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", session.token.to_string()))
                .body(json!({ "privileged_intents": bits }).to_string())
        };
        let content = v0::GatewayIntents::MESSAGE_CONTENT as i64;
        let response = patch(content).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let updated: v0::Bot = response.into_json().await.expect("`Bot`");
        assert_eq!(updated.privileged_intents, content);

        // only the three privileged intents can be set here
        let response = patch(v0::GatewayIntents::SERVERS as i64).dispatch().await;
        assert_eq!(response.status(), Status::BadRequest);
        let response = patch(-1).dispatch().await;
        assert_eq!(response.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn interactions_url_takes_web_addresses_only() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");

        let patch = |url: &str| {
            harness
                .client
                .patch(format!("/bots/{}", bot.id))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", session.token.to_string()))
                .body(json!({ "interactions_url": url }).to_string())
        };
        for bad in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "https://user:pass@example.com/",
            "https:// example.com",
            "//example.com/x",
        ] {
            let response = patch(bad).dispatch().await;
            assert_eq!(response.status(), Status::BadRequest, "{bad}");
        }
        let stored = harness.db.fetch_bot(&bot.id).await.unwrap();
        assert!(stored.interactions_url.is_empty());

        let response = patch("https://example.com/interactions").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
    }
}
