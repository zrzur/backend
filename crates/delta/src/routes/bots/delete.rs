use revolt_database::{
    util::reference::Reference,
    voice::{remove_user_from_voice_channels, VoiceClient},
    Database, User, ValidatedTicket,
};
use revolt_result::{create_error, Result};
use rocket::State;
use rocket_empty::EmptyResponse;

/// # Delete Bot
///
/// Delete a bot by its id.
///
/// Requires a validated MFA ticket (`X-MFA-Ticket`) created by the bot owner
/// with their password, authenticator code or recovery code.
#[openapi(tag = "Bots")]
#[delete("/<bot_id>")]
pub async fn delete_bot(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    validated_ticket: Option<ValidatedTicket>,
    bot_id: Reference<'_>,
) -> Result<EmptyResponse> {
    let bot = bot_id.as_bot(db).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }

    // Zrzur: deleting a bot is irreversible, so the owner must re-authenticate.
    // The guard only yields a ticket that is validated, unexpired and owned by
    // this session's account, and it consumes the ticket.
    if validated_ticket.is_none() {
        return Err(create_error!(InvalidCredentials));
    }

    bot.delete(db).await?;

    remove_user_from_voice_channels(voice_client, &bot.id).await?;

    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{events::client::EventV1, Bot, MFATicket};
    use rocket::http::{Header, Status};
    use crate::util::test::PubSubTestHelper;

    #[rocket::async_test]
    async fn delete_bot() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");
        let mut pubsub = PubSubTestHelper::new(&bot.id).await;

        let ticket = MFATicket::new(user.id.to_string(), true);
        ticket.save(&harness.db).await.unwrap();

        let response = harness
            .client
            .delete(format!("/bots/{}", bot.id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .header(Header::new("x-mfa-ticket", ticket.token))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NoContent);
        assert!(harness.db.fetch_bot(&bot.id).await.is_err());
        drop(response);

        let event = pubsub
            .wait_for_event(|event| match event {
                EventV1::UserUpdate { id, .. } => id == &bot.id,
                _ => false,
            })
            .await;

        match event {
            EventV1::UserUpdate { data, .. } => {
                assert_eq!(data.flags, Some(2));
            }
            _ => unreachable!(),
        }
    }

    #[rocket::async_test]
    async fn delete_bot_without_ticket_is_rejected() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");

        let response = harness
            .client
            .delete(format!("/bots/{}", bot.id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Unauthorized);
        assert!(harness.db.fetch_bot(&bot.id).await.is_ok());
    }

    #[rocket::async_test]
    async fn delete_bot_with_unvalidated_ticket_is_rejected() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");

        // ticket whose password / code was never verified
        let ticket = MFATicket::new(user.id.to_string(), false);
        ticket.save(&harness.db).await.unwrap();

        let response = harness
            .client
            .delete(format!("/bots/{}", bot.id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .header(Header::new("x-mfa-ticket", ticket.token))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Unauthorized);
        assert!(harness.db.fetch_bot(&bot.id).await.is_ok());
    }

    #[rocket::async_test]
    async fn delete_bot_with_other_accounts_ticket_is_rejected() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (other, _, _) = harness.new_user().await;

        let (bot, _) = Bot::create(&harness.db, TestHarness::rand_string(), &user, None)
            .await
            .expect("`Bot`");

        let ticket = MFATicket::new(other.id.to_string(), true);
        ticket.save(&harness.db).await.unwrap();

        let response = harness
            .client
            .delete(format!("/bots/{}", bot.id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .header(Header::new("x-mfa-ticket", ticket.token))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Unauthorized);
        assert!(harness.db.fetch_bot(&bot.id).await.is_ok());
    }
}
