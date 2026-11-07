use revolt_database::{
    util::reference::Reference,
    voice::{
        delete_voice_channel, get_user_voice_channel_in_server, remove_user_from_voice_channel,
        UserVoiceChannel, VoiceClient,
    },
    Database, RemovalIntention, User, ValidatedTicket,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::State;

use rocket_empty::EmptyResponse;

/// # Delete / Leave Server
///
/// Deletes a server if owner otherwise leaves.
///
/// Deleting (as owner) requires a validated MFA ticket (`X-MFA-Ticket`).
#[openapi(tag = "Server Information")]
#[delete("/<target>?<options..>")]
pub async fn delete(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    validated_ticket: Option<ValidatedTicket>,
    target: Reference<'_>,
    options: v0::OptionsServerDelete,
) -> Result<EmptyResponse> {
    let server = target.as_server(db).await?;
    let member = db.fetch_member(target.id, &user.id).await?;

    if server.owner == user.id {
        // Zrzur: deleting a server is irreversible, so the owner must
        // re-authenticate; leaving a server does not need a ticket.
        if validated_ticket.is_none() {
            return Err(create_error!(InvalidCredentials));
        }

        for channel_id in &server.channels {
            delete_voice_channel(
                voice_client,
                &UserVoiceChannel {
                    id: channel_id.clone(),
                    server_id: Some(server.id.clone()),
                },
            )
            .await?;
        }

        server.delete(db).await
    } else {
        if let Some(channel_id) = get_user_voice_channel_in_server(&user.id, &server.id).await? {
            remove_user_from_voice_channel(
                voice_client,
                &UserVoiceChannel {
                    id: channel_id,
                    server_id: Some(server.id.clone()),
                },
                &user.id,
            )
            .await?;
        };

        member
            .remove(
                db,
                &server,
                RemovalIntention::Leave,
                options.leave_silently.unwrap_or_default(),
            )
            .await
    }
    .map(|_| EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{MFATicket, Member};
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn owner_delete_without_ticket_is_rejected() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, _) = harness.new_server(&user).await;
        Member::create(&harness.db, &server, &user, None)
            .await
            .expect("`Member`");

        let response = harness
            .client
            .delete(format!("/servers/{}", server.id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Unauthorized);
        assert!(harness.db.fetch_server(&server.id).await.is_ok());
    }

    #[rocket::async_test]
    async fn owner_delete_with_ticket() {
        let harness = TestHarness::new().await;
        let (account, session, user) = harness.new_user().await;
        let (server, _) = harness.new_server(&user).await;
        Member::create(&harness.db, &server, &user, None)
            .await
            .expect("`Member`");

        let ticket = MFATicket::new(account.id.to_string(), true);
        ticket.save(&harness.db).await.unwrap();

        let response = harness
            .client
            .delete(format!("/servers/{}", server.id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .header(Header::new("x-mfa-ticket", ticket.token))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NoContent);
        assert!(harness.db.fetch_server(&server.id).await.is_err());
    }

    #[rocket::async_test]
    async fn member_leave_does_not_need_ticket() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, session, user) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        Member::create(&harness.db, &server, &user, None)
            .await
            .expect("`Member`");

        let response = harness
            .client
            .delete(format!("/servers/{}", server.id))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NoContent);
        assert!(harness.db.fetch_server(&server.id).await.is_ok());
        assert!(harness.db.fetch_member(&server.id, &user.id).await.is_err());
    }
}
