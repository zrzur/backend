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
    // Deprecated: use POST /bots/<id>/token/reset. Like it, a new token
    // needs the owner to sign in again (X-MFA-Ticket).
    if data.remove.contains(&v0::FieldsBot::Token) && validated_ticket.is_none() {
        return Err(create_error!(InvalidCredentials));
    }
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let mut bot = bot_id.as_bot(db).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }

    let mut user = db.fetch_user(&bot.id).await?;
    if let Some(name) = data.name {
        // a system bot keeps the name it was given on the server
        if user.official_bot {
            return Err(create_error!(OfficialAccount));
        }
        user.update_username(db, name).await?;
    }

    if data.public.is_none()
        && data.analytics.is_none()
        && data.interactions_url.is_none()
        && data.remove.is_empty()
        && data.default_permissions.is_none()
        && data.privileged_intents.is_none()
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
        ..
    } = data;

    // only the three privileged intents are switched on or off here
    if privileged_intents.is_some_and(|bits| bits < 0 || (bits as u64) & !v0::GatewayIntents::PRIVILEGED != 0) {
        return Err(create_error!(FailedValidation {
            error: "privileged_intents: only MESSAGE_CONTENT, SERVER_MEMBERS and PRESENCE".to_string()
        }));
    }

    let partial = PartialBot {
        public,
        analytics,
        interactions_url,
        default_permissions,
        privileged_intents,
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
}
