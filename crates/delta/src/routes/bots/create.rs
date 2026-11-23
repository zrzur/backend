use revolt_database::{Bot, Database, User};
use revolt_models::v0;
use revolt_result::Result;
use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// # Create Bot
///
/// Create a new bot.
#[openapi(tag = "Bots")]
#[post("/create", data = "<info>")]
pub async fn create_bot(
    db: &State<Database>,
    user: User,
    info: Json<v0::DataCreateBot>,
) -> Result<Json<v0::BotWithUserResponse>> {
    // not before the e-mail of an account made from an invite link is confirmed
    revolt_database::Account::assert_confirmed(db, &user.id).await?;

    let info = info.into_inner();
    info.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // "Liro Bot": the username is Liro_Bot, people see the name as typed;
    // both are written with the bot's user, in one write
    let (bot, user) = Bot::create_with_display_name(
        db,
        v0::bot_username(&info.name),
        v0::bot_display_name(&info.name),
        &user,
        None,
    )
    .await?;
    Ok(Json(v0::BotWithUserResponse {
        bot: bot.into(),
        user: user.into_self(false).await,
    }))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};

    #[rocket::async_test]
    async fn create_bot() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;

        let response = harness
            .client
            .post("/bots/create")
            .header(Header::new("x-session-token", session.token.to_string()))
            .header(ContentType::JSON)
            .body(
                json!(v0::DataCreateBot {
                    name: TestHarness::rand_string(),
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);

        let bot: v0::Bot = response.into_json().await.expect("`Bot`");
        assert!(harness.db.fetch_bot(&bot.id).await.is_ok());
    }
}
