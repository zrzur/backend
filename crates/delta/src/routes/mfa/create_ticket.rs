//! Create a new MFA ticket or validate an existing one.
//! PUT /mfa/ticket
use revolt_result::{ErrorType, Result, create_error};
use revolt_database::{
    util::mfa_limit::{clear_mfa_failures, count_mfa_failure, mfa_is_paused},
    Account, Database, MFATicket, UnvalidatedTicket,
};
use revolt_models::v0;
use rocket::serde::json::Json;
use rocket::State;


/// # Create MFA ticket
///
/// Create a new MFA ticket or validate an existing one.
#[openapi(tag = "MFA")]
#[put("/ticket", data = "<data>")]
pub async fn create_ticket(
    db: &State<Database>,
    account: Option<Account>,
    existing_ticket: Option<UnvalidatedTicket>,
    data: Json<v0::MFAResponse>,
) -> Result<Json<v0::MFATicket>> {
    // Find the relevant account
    let mut account = match (account, existing_ticket) {
        (Some(_), Some(_)) => return Err(create_error!(OperationFailed)),
        (Some(account), _) => account,
        (_, Some(ticket)) => {
            db.delete_ticket(&ticket.id).await?;
            db.fetch_account(&ticket.account_id).await?
        }
        _ => return Err(create_error!(InvalidToken)),
    };

    // Too many wrong answers: pause, whichever session or ticket is asking
    if mfa_is_paused(&account.id).await {
        return Err(create_error!(LockedOut));
    }

    // Validate the MFA response
    match account
        .consume_mfa_response(db, data.into_inner(), None)
        .await
    {
        Ok(()) => clear_mfa_failures(&account.id).await,
        Err(error) => {
            if !matches!(error.error_type, ErrorType::DisallowedMFAMethod) {
                count_mfa_failure(&account.id).await;
            }
            return Err(error);
        }
    }

    // Create a new ticket for this account
    let ticket = MFATicket::new(account.id, true);
    ticket.save(db).await?;
    Ok(Json(ticket.into()))
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::Totp;
    use rocket::http::{Header, Status};
    use revolt_models::v0;
    use revolt_result::{Error, ErrorType};

    #[rocket::async_test]
    async fn success() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;

        let res = harness.client
            .put("/auth/mfa/ticket")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(
                json!({
                    "password": "password_insecure"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        assert!(res.into_json::<v0::MFATicket>().await.unwrap().validated);
    }

    #[rocket::async_test]
    async fn success_totp() {
        let harness = TestHarness::new().await;
        let (mut account, session, _) = harness.new_user().await;

        account.mfa.totp_token = Totp::Enabled {
            secret: "secret".to_string(),
        };
        account.save(&harness.db).await.unwrap();

        let res = harness.client
            .put("/auth/mfa/ticket")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(
                json!({
                    "totp_code": Totp::Enabled {
                        secret: "secret".to_string(),
                    }.generate_code().unwrap()
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        assert!(res.into_json::<v0::MFATicket>().await.is_some());
    }

    #[rocket::async_test]
    async fn failure_totp() {
        let harness = TestHarness::new().await;
        let (mut account, session, _) = harness.new_user().await;

        account.mfa.totp_token = Totp::Enabled {
            secret: "secret".to_string(),
        };
        account.save(&harness.db).await.unwrap();

        let res = harness.client
            .put("/auth/mfa/ticket")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(
                json!({
                    "totp_code": "000000"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Unauthorized);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::InvalidToken,
        ));
    }

    #[rocket::async_test]
    async fn a_time_code_works_once() {
        let harness = TestHarness::new().await;
        let (mut account, session, _) = harness.new_user().await;

        let totp = Totp::Enabled {
            secret: "secret".to_string(),
        };
        let code = totp.generate_code().unwrap();
        account.mfa.totp_token = totp;
        account.save(&harness.db).await.unwrap();

        let answer = json!({ "totp_code": code }).to_string();
        let first = harness.client
            .put("/auth/mfa/ticket")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(answer.clone())
            .dispatch()
            .await;
        assert_eq!(first.status(), Status::Ok);

        let second = harness.client
            .put("/auth/mfa/ticket")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(answer)
            .dispatch()
            .await;
        assert_eq!(second.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn guessing_pauses_even_the_right_answer() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;

        for _ in 0..revolt_database::util::mfa_limit::MFA_FAILURE_LIMIT {
            let res = harness.client
                .put("/auth/mfa/ticket")
                .header(Header::new("X-Session-Token", session.token.clone()))
                .body(json!({ "password": "wrong" }).to_string())
                .dispatch()
                .await;
            assert_eq!(res.status(), Status::Unauthorized);
        }

        let res = harness.client
            .put("/auth/mfa/ticket")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(json!({ "password": "password_insecure" }).to_string())
            .dispatch()
            .await;
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::LockedOut,
        ));
    }

    #[rocket::async_test]
    async fn failure_no_totp() {
        let harness = TestHarness::new().await;
        let (mut account, session, _) = harness.new_user().await;

        account.mfa.totp_token = Totp::Enabled {
            secret: "secret".to_string(),
        };
        account.save(&harness.db).await.unwrap();

        let res = harness.client
            .put("/auth/mfa/ticket")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(
                json!({
                    "password": "this is the wrong mfa method"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::BadRequest);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::DisallowedMFAMethod,
        ));
    }
}
