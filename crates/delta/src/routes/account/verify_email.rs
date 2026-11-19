//! Verify an account
//! POST /verify/<code>
use rocket::{serde::json::Json, State};
use revolt_database::{Database, EmailVerification, MFATicket, util::{email::normalise_email, password::{assert_safe, hash_password}}};
use revolt_result::{create_error, Result};
use revolt_models::v0;

/// # Verification Link Information
///
/// Whether confirming with this link needs a password to be chosen (an
/// account made from an invite link has none yet). Uses nothing up.
#[openapi(tag = "Account")]
#[get("/verify/<code>")]
pub async fn verify_email_info(
    db: &State<Database>,
    code: String,
) -> Result<Json<v0::VerifyEmailInfo>> {
    let account = db.fetch_account_with_email_verification(&code).await?;
    Ok(Json(v0::VerifyEmailInfo {
        needs_password: account
            .provisional
            .as_ref()
            .is_some_and(|provisional| provisional.password_unset),
    }))
}

/// # Verify Email
///
/// Verify an email address. An account made from an invite link sends the
/// password it wants with it.
#[openapi(tag = "Account")]
#[post("/verify/<code>", data = "<data>")]
pub async fn verify_email(
    db: &State<Database>,
    code: String,
    data: Option<Json<v0::DataVerifyEmail>>,
) -> Result<Json<v0::ResponseVerify>> {
    // Find the account
    let mut account = db
        .fetch_account_with_email_verification(&code)
        .await?;

    // An account made from an invite link chooses its password here; the
    // link stays good until it does
    if account
        .provisional
        .as_ref()
        .is_some_and(|provisional| provisional.password_unset)
    {
        let Some(password) = data.and_then(|data| data.into_inner().password) else {
            return Err(create_error!(IncorrectData {
                with: "password".to_string()
            }));
        };
        assert_safe(&password).await?;
        account.password = hash_password(password)?;
    }
    account.provisional = None;

    // Update account email
    let response = if let EmailVerification::Moving { new_email, .. } = &account.verification {
        account.email = new_email.clone();
        account.email_normalised = normalise_email(new_email.clone());
        v0::ResponseVerify::NoTicket
    } else {
        let mut ticket = MFATicket::new(account.id.to_string(), false);
        ticket.authorised = true;
        ticket.save(db).await?;
        v0::ResponseVerify::WithTicket { ticket: ticket.into() }
    };

    // Mark as verified
    account.verification = EmailVerification::Verified;

    // Save to database
    account.save(db).await?;
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use iso8601_timestamp::{Timestamp, Duration};
    use revolt_database::EmailVerification;
    use crate::{rocket, util::test::TestHarness};
    use rocket::http::{ContentType, Status};
    use revolt_models::v0;
    use revolt_result::{Error, ErrorType};
    
    #[rocket::async_test]
    async fn success() {
        let harness = TestHarness::new().await;
        let (mut account, _, _) = harness.new_user().await;

        account.verification = EmailVerification::Pending {
            token: "token".into(),
            expiry: Timestamp::now_utc() + Duration::seconds(100),
        };

        account.save(&harness.db).await.unwrap();

        let res = harness.client.post("/auth/account/verify/token").dispatch().await;

        assert_eq!(res.status(), Status::Ok);

        // Make sure it was used and can't be used again
        assert!(harness.db
            .fetch_account_with_email_verification("token")
            .await
            .is_err());

        // Check that we can login using the received MFA ticket
        let response = res.into_json::<v0::ResponseVerify>().await
            .expect("`ResponseVerify`");

        if let v0::ResponseVerify::WithTicket { ticket } = response {
            let res = harness.client
                .post("/auth/session/login")
                .header(ContentType::JSON)
                .body(json!({ "mfa_ticket": ticket.token }).to_string())
                .dispatch()
                .await;

            assert_eq!(res.status(), Status::Ok);
            assert!(res.into_json::<v0::Session>().await.is_some());
        } else {
            panic!("Expected `ResponseVerify::WithTicket`");
        }
    }

    #[rocket::async_test]
    async fn fail_invalid_token() {
        let harness = TestHarness::new().await;

        let res = harness.client.post("/auth/account/verify/token").dispatch().await;

        assert_eq!(res.status(), Status::Unauthorized);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::InvalidToken,
        ));
    }
}
