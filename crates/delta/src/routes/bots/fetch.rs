use revolt_database::{util::reference::Reference, Database, User};
use revolt_models::v0::FetchBotResponse;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Fetch Bot
///
/// Fetch details of a bot you own by its id. A bot may fetch itself (with
/// its own id) to read its settings, such as the privileged intents its
/// owner switched on.
#[openapi(tag = "Bots")]
#[get("/<bot_id>")]
pub async fn fetch_bot(
    db: &State<Database>,
    user: User,
    bot_id: Reference<'_>,
) -> Result<Json<FetchBotResponse>> {
    if user.bot.is_some() {
        if bot_id.id != user.id {
            return Err(create_error!(IsBot));
        }
        let bot = db.fetch_bot(&user.id).await?;
        return Ok(Json(FetchBotResponse {
            user: user.into_self(false).await,
            bot: bot.into(),
        }));
    }

    let bot = bot_id.as_bot(db).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }

    Ok(Json(FetchBotResponse {
        user: db.fetch_user(&bot.id).await?.into(db, None).await,
        bot: bot.into(),
    }))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::Bot;
    use revolt_models::v0;
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn fetch_bot() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");

        let response = harness
            .client
            .get(format!("/bots/{}", bot.id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);

        let response: v0::FetchBotResponse = response.into_json().await.expect("`Bot`");
        // the token is shown once, at creation; never when fetching
        let mut expected: v0::Bot = bot.into();
        expected.token = String::new();
        assert_eq!(response.bot, expected);
    }

    /// Gap 17: the owner switches Message Content on in the portal; the bot
    /// reads it back itself, and its next gateway connection gets it
    #[rocket::async_test]
    async fn a_switched_on_intent_is_read_by_the_bot_and_its_next_connection() {
        use rocket::http::ContentType;
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (bot, bot_user) = harness.new_bot(&owner).await;
        let (other, _) = harness.new_bot(&owner).await;
        let content = v0::GatewayIntents::MESSAGE_CONTENT;
        let wanted = v0::GatewayIntents::DEFAULT | content;

        // before: off, and a connection asking for it is refused
        assert!(revolt_database::connection_intents(&harness.db, &bot_user, Some(wanted))
            .await
            .is_err());

        let response = harness
            .client
            .patch(format!("/bots/{}", bot.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.to_string()))
            .body(json!({ "privileged_intents": content }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        drop(response);

        let as_bot = |id: &str| {
            harness
                .client
                .get(format!("/bots/{id}"))
                .header(Header::new("x-bot-token", bot.token.clone()))
        };
        let response = as_bot(&bot.id).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let own: v0::FetchBotResponse = response.into_json().await.expect("`Bot`");
        assert_eq!(own.bot.privileged_intents, content as i64);
        assert!(own.bot.token.is_empty());
        assert_eq!(own.user.id, bot.id);

        // the same as the gateway reads it
        assert_eq!(
            revolt_database::connection_intents(&harness.db, &bot_user, Some(wanted)).await,
            Ok(wanted)
        );

        // another bot's settings stay private, even between bots of one owner
        let response = as_bot(&other.id).dispatch().await;
        assert_eq!(response.status(), Status::BadRequest);
    }
}
