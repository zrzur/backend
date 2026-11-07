use revolt_database::{util::reference::Reference, Database, FieldsBot, PartialBot, User, ValidatedTicket};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Reset Bot Token
///
/// Makes a new token for the bot; the old one stops working at once and
/// the bot is signed out everywhere. The new token is in this response
/// only: Zrzur keeps just a hash and can't show it again.
///
/// Requires a validated MFA ticket (`X-MFA-Ticket`) created by the bot owner
/// with their password, authenticator code or recovery code.
#[openapi(tag = "Bots")]
#[post("/<bot_id>/token/reset")]
pub async fn reset_token(
    db: &State<Database>,
    user: User,
    validated_ticket: Option<ValidatedTicket>,
    bot_id: Reference<'_>,
) -> Result<Json<v0::Bot>> {
    let mut bot = bot_id.as_bot(db).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }
    // the guard only yields a ticket that is validated, unexpired and this
    // account's, and it uses the ticket up
    if validated_ticket.is_none() {
        return Err(create_error!(InvalidCredentials));
    }

    bot.update(db, PartialBot::default(), vec![FieldsBot::Token]).await?;
    Ok(Json(bot.into()))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{hash_token, Bot, MFATicket};
    use revolt_models::v0;
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn the_database_never_holds_a_plain_token() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .unwrap();
        assert!(bot.token.starts_with(&format!("zrzur_bot_{}.", bot.id)));
        let stored = harness.db.fetch_bot(&bot.id).await.unwrap();
        assert_eq!(stored.token, "");
        assert_eq!(stored.token_hash, hash_token(&bot.token));
        let raw = serde_json::to_value(&stored).unwrap();
        assert!(raw.get("token").is_none(), "{raw}");
        // and the token still signs in
        assert_eq!(harness.db.fetch_bot_by_token(&bot.token).await.unwrap().id, bot.id);
    }

    #[rocket::async_test]
    async fn fetching_a_bot_never_shows_its_token() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .unwrap();
        let body = harness
            .client
            .get(format!("/bots/{}", bot.id))
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
            .await
            .into_string()
            .await
            .unwrap();
        assert!(!body.contains(&bot.token), "{body}");
        let mine = harness
            .client
            .get("/bots/@me")
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
            .await
            .into_string()
            .await
            .unwrap();
        assert!(!mine.contains(&bot.token));
    }

    #[rocket::async_test]
    async fn a_reset_needs_a_ticket_and_replaces_the_token() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .unwrap();
        let reset = |ticket: Option<String>| {
            let mut request = harness
                .client
                .post(format!("/bots/{}/token/reset", bot.id))
                .header(Header::new("x-session-token", session.token.clone()));
            if let Some(ticket) = ticket {
                request = request.header(Header::new("x-mfa-ticket", ticket));
            }
            request
        };
        assert_eq!(reset(None).dispatch().await.status(), Status::Unauthorized);

        let ticket = MFATicket::new(user.id.to_string(), true);
        ticket.save(&harness.db).await.unwrap();
        let response = reset(Some(ticket.token.clone())).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let updated: v0::Bot = response.into_json().await.unwrap();
        assert!(updated.token.starts_with("zrzur_bot_"));
        assert_ne!(updated.token, bot.token);

        // the old token no longer works, the new one does
        assert!(harness.db.fetch_bot_by_token(&bot.token).await.is_err());
        assert!(harness.db.fetch_bot_by_token(&updated.token).await.is_ok());
        // a used ticket can't reset again
        assert_ne!(reset(Some(ticket.token)).dispatch().await.status(), Status::Ok);
    }

    #[rocket::async_test]
    async fn the_old_edit_reset_needs_a_ticket_too() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .unwrap();
        let response = harness
            .client
            .patch(format!("/bots/{}", bot.id))
            .header(Header::new("x-session-token", session.token.clone()))
            .header(rocket::http::ContentType::JSON)
            .body(json!({ "remove": ["Token"] }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
        assert!(harness.db.fetch_bot_by_token(&bot.token).await.is_ok());
    }

    #[rocket::async_test]
    async fn a_token_from_before_hashing_keeps_working() {
        // migration 67 stores the hash of the token a bot already had
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .unwrap();
        let legacy = "qWk3vLx9Tz0aB7mN2pR5sU8yD1fH4jK6cE9gI0oL3qW7tY2uA5zX8vB1nM4kJ6hG";
        harness
            .db
            .update_bot(
                &bot.id,
                &revolt_database::PartialBot {
                    token_hash: Some(hash_token(legacy)),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(harness.db.fetch_bot_by_token(legacy).await.unwrap().id, bot.id);
    }
}
