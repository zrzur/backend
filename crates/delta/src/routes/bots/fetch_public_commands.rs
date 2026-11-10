use revolt_database::{Database, User};
use revolt_models::v0::ApplicationCommand;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::public_commands;

/// # Fetch Public Bot Commands
///
/// The global commands a bot registered, for its page in Discover and its
/// profile, and for the command list in a DM with it. Works for public
/// bots, for the owner, and for anyone who has a DM with the bot. Commands
/// limited to one server are never listed here. The list is cached for a
/// minute; registering or changing a command through the API shows at once.
#[openapi(tag = "Bots")]
#[get("/<target>/commands")]
pub async fn fetch_public_bot_commands(
    db: &State<Database>,
    user: Option<User>,
    target: &str,
) -> Result<Json<Vec<ApplicationCommand>>> {
    let listing = public_commands::listing(db, target).await?;
    let allowed = listing.public
        || match &user {
            Some(user) => {
                user.id == listing.owner
                    || db
                        .find_direct_message_channel(&user.id, target)
                        .await
                        .is_ok()
            }
            None => false,
        };
    if !allowed {
        return Err(create_error!(NotFound));
    }

    Ok(Json(listing.commands.as_ref().clone()))
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
        // written straight to the database, past the edit route that drops
        // the cached listing
        crate::util::public_commands::forget(&bot.id);

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

    async fn names(harness: &TestHarness, bot: &str) -> Vec<String> {
        let response = harness
            .client
            .get(format!("/bots/{bot}/commands"))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let commands: Vec<v0::ApplicationCommand> = response.into_json().await.unwrap();
        commands.into_iter().map(|c| c.name).collect()
    }

    async fn public_bot(harness: &TestHarness) -> (revolt_database::Bot, revolt_database::Session) {
        let (_, owner_session, owner) = harness.new_user().await;
        let (mut bot, _) = harness.new_bot(&owner).await;
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
        (bot, owner_session)
    }

    /// A popular bot's page is read from memory: a command written straight
    /// to the database (no route, so nothing is told) does not show until the
    /// cached list is dropped
    #[rocket::async_test]
    async fn repeat_views_are_served_from_the_cache() {
        let harness = TestHarness::new().await;
        let (bot, _) = public_bot(&harness).await;
        assert!(names(&harness, &bot.id).await.is_empty());

        revolt_database::create_application_command(
            &harness.db,
            &bot.id,
            None,
            v0::DataApplicationCommand {
                name: "later".into(),
                description: "Added behind the cache".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(names(&harness, &bot.id).await.is_empty());

        crate::util::public_commands::forget(&bot.id);
        assert_eq!(names(&harness, &bot.id).await, vec!["later"]);
    }

    /// Registering a command through the API drops the cached list at once
    #[rocket::async_test]
    async fn registering_a_command_refreshes_the_list() {
        let harness = TestHarness::new().await;
        let (bot, owner_session) = public_bot(&harness).await;
        assert!(names(&harness, &bot.id).await.is_empty());

        let response = harness
            .client
            .post(format!("/applications/{}/commands", bot.id))
            .header(Header::new("x-session-token", owner_session.token.clone()))
            .header(Header::new("content-type", "application/json"))
            .body(r#"{"name":"ping","description":"Answers pong"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        assert_eq!(names(&harness, &bot.id).await, vec!["ping"]);
    }

    /// A bot made private stops listing its commands for strangers at once
    #[rocket::async_test]
    async fn making_a_bot_private_drops_the_cached_list() {
        let harness = TestHarness::new().await;
        let (bot, owner_session) = public_bot(&harness).await;
        names(&harness, &bot.id).await;

        let response = harness
            .client
            .patch(format!("/bots/{}", bot.id))
            .header(Header::new("x-session-token", owner_session.token.clone()))
            .header(Header::new("content-type", "application/json"))
            .body(r#"{"public":false}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);

        let response = harness
            .client
            .get(format!("/bots/{}/commands", bot.id))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);
    }

    /// Reading a bot's page has its own, larger bucket: browsing Discover and
    /// opening profiles must not use up the bucket of bot management
    #[rocket::async_test]
    async fn public_bot_reads_have_their_own_bucket() {
        let harness = TestHarness::new().await;
        let (bot, _) = public_bot(&harness).await;
        let mut buckets = vec![];
        for path in ["commands", "invite"] {
            let response = harness
                .client
                .get(format!("/bots/{}/{path}", bot.id))
                .dispatch()
                .await;
            assert_eq!(response.status(), Status::Ok, "{path}");
            assert_eq!(
                response.headers().get_one("x-ratelimit-limit"),
                Some("40"),
                "{path}"
            );
            buckets.push(response.headers().get_one("x-ratelimit-bucket").map(str::to_string));
        }
        assert!(buckets[0].is_some());
        assert_eq!(buckets[0], buckets[1]);
    }
}
