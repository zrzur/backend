use revolt_database::{Database, User};
use revolt_models::v0::ApplicationCommand;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Fetch Public Bot Commands
///
/// The global commands a bot registered, for its page in Discover and its
/// profile, and for the command list in a DM with it. Works for public
/// bots, for the owner, and for anyone who has a DM with the bot. Commands
/// limited to one server are never listed here.
#[openapi(tag = "Bots")]
#[get("/<target>/commands")]
pub async fn fetch_public_bot_commands(
    db: &State<Database>,
    user: Option<User>,
    target: &str,
) -> Result<Json<Vec<ApplicationCommand>>> {
    let bot = db.fetch_bot(target).await.map_err(|_| create_error!(NotFound))?;
    let allowed = bot.public
        || match &user {
            Some(user) => {
                user.id == bot.owner
                    || db
                        .find_direct_message_channel(&user.id, &bot.id)
                        .await
                        .is_ok()
            }
            None => false,
        };
    if !allowed {
        return Err(create_error!(NotFound));
    }

    let mut commands = db.fetch_application_commands(&bot.id, None).await?;
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(commands))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::PartialBot;
    use revolt_models::v0;
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn lists_global_commands_of_a_public_bot() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, session, _) = harness.new_user().await;
        let (mut bot, _) = harness.new_bot(&owner).await;
        let (server, _) = harness.new_server(&owner).await;

        for (name, scope) in [("zeta", None), ("alpha", None), ("local", Some(server.id.as_str()))] {
            revolt_database::create_application_command(
                &harness.db,
                &bot.id,
                scope,
                v0::DataApplicationCommand {
                    name: name.into(),
                    description: "Does things".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }

        // private: only the owner sees them
        let response = harness
            .client
            .get(format!("/bots/{}/commands", bot.id))
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);
        let response = harness
            .client
            .get(format!("/bots/{}/commands", bot.id))
            .header(Header::new("x-session-token", owner_session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);

        bot.update(
            &harness.db,
            PartialBot {
                public: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

        // public: anyone, signed in or not; global commands only, by name
        let response = harness
            .client
            .get(format!("/bots/{}/commands", bot.id))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let commands: Vec<v0::ApplicationCommand> = response.into_json().await.unwrap();
        let names: Vec<_> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);

        let response = harness
            .client
            .get("/bots/01ARZ3NDEKTSV4RRFFQ69G5FAV/commands")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);
    }
}
