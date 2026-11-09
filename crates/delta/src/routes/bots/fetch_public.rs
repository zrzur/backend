use revolt_database::{util::reference::Reference, Database, User};
use revolt_models::v0::{PublicBot, PublicBotCreator};
use revolt_result::{create_error, Result};

use rocket::serde::json::Json;
use rocket::State;

/// # Fetch Public Bot
///
/// Fetch details of a public (or owned) bot by its id: its profile, the
/// permissions it asks for, whether it is verified, who made it and how
/// many servers it is in.
#[openapi(tag = "Bots")]
#[get("/<target>/invite")]
pub async fn fetch_public_bot(
    db: &State<Database>,
    user: Option<User>,
    target: Reference<'_>,
) -> Result<Json<PublicBot>> {
    let bot = db.fetch_bot(target.id).await?;
    if !bot.public && user.is_none_or(|x| x.id != bot.owner) {
        return Err(create_error!(NotFound));
    }

    let bot_user = db.fetch_user(&bot.id).await?;
    let creator = db
        .fetch_user(&bot.owner)
        .await
        .ok()
        .map(|owner| PublicBotCreator {
            id: owner.id,
            username: owner.username,
            display_name: owner.display_name,
        });
    let server_count = db.fetch_server_count(&bot.id).await.ok().map(|n| n as i64);

    let mut public = bot.into_public_bot(bot_user);
    public.creator = creator;
    public.server_count = server_count;
    Ok(Json(public))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Bot, PartialBot};
    use revolt_models::v0;

    #[rocket::async_test]
    async fn fetch_public() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;

        let (mut bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");

        bot.update(
            &harness.db,
            PartialBot {
                public: Some(true),
                default_permissions: Some(1 << 22),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

        let bot_user = harness.db.fetch_user(&bot.id).await.expect("`User`");
        let response = harness
            .client
            .get(format!("/bots/{}/invite", bot.id))
            .dispatch()
            .await;

        let public_bot: v0::PublicBot = response.into_json().await.expect("`PublicBot`");
        let mut expected = bot.into_public_bot(bot_user);
        expected.creator = Some(v0::PublicBotCreator {
            id: user.id.clone(),
            username: user.username.clone(),
            display_name: user.display_name.clone(),
        });
        expected.server_count = Some(0);
        assert_eq!(public_bot, expected);
        // the add dialog needs these to ask for the right permissions
        assert_eq!(public_bot.default_permissions, Some(1 << 22));
        assert!(!public_bot.verified);
    }
}
