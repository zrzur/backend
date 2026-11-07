//! Send a password reset email
//! POST /account/reset_password
use std::time::Duration;

use tokio::time::sleep;
use rocket::serde::json::Json;
use rocket::State;
use rocket_empty::EmptyResponse;
use revolt_result::Result;
use revolt_database::{Database, EmailVerification, util::{email::{normalise_email, validate_email}, captcha::check_captcha, mail_limit::allow_mail, shield::ShieldValidationInput}};
use revolt_models::v0;

/// # Send Password Reset
///
/// Send an email to reset account password.
#[openapi(tag = "Account")]
#[post("/reset_password", data = "<data>")]
pub async fn send_password_reset(
    db: &State<Database>,
    data: Json<v0::DataSendPasswordReset>,
    shield: ShieldValidationInput,
) -> Result<EmptyResponse> {
    let data = data.into_inner();

    // Random jitter from 0-1000ms
    sleep(Duration::from_millis((rand::random::<f32>() * 1000.) as u64)).await;

    // Check Captcha token
    check_captcha(data.captcha.as_deref()).await?;

    // Make sure email is valid and not blocked
    validate_email(&data.email)?;

    // From this point on, do not report failure to the
    // remote client, as this will open us up to user enumeration.

    // Normalise the email
    let email_normalised = normalise_email(data.email);

    // Too many requests for this address or from this source: skip the mail
    // silently, the answer must look the same
    if !allow_mail("reset", &email_normalised, shield.ip.as_deref()).await {
        return Ok(EmptyResponse);
    }

    // Try to find the relevant account
    if let Ok(Some(mut account)) = db
        .fetch_account_by_normalised_email(&email_normalised)
        .await
    {
        if !matches!(account.verification, EmailVerification::Pending { .. }) {
            if let Err(e) = account.start_password_reset(db, false).await {
                revolt_config::capture_error(&e);
            }
        }
    }

    // Never fail this route, (except for db error)
    // You may open the application to email enumeration otherwise.
    Ok(EmptyResponse)
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::Account;
    use revolt_models::v0;
    use rocket::http::{ContentType, Status};

    fn fresh_address() -> String {
        format!("{}@smtp.test", TestHarness::rand_string().to_lowercase())
    }

    /// Mails maildev holds for `address`
    async fn mails_to(address: &str) -> usize {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        reqwest::get("http://localhost:14080/email")
            .await
            .unwrap()
            .json::<Vec<serde_json::Value>>()
            .await
            .unwrap()
            .iter()
            .filter(|mail| mail["envelope"]["to"][0]["address"] == address)
            .count()
    }

    #[rocket::async_test]
    async fn a_second_request_sends_no_second_mail() {
        let harness = TestHarness::new().await;
        let email = fresh_address();
        Account::new(&harness.db, email.clone(), "password".into(), false)
            .await
            .unwrap();

        for _ in 0..2 {
            let res = harness
                .client
                .post("/auth/account/reset_password")
                .header(ContentType::JSON)
                .body(json!({ "email": email }).to_string())
                .dispatch()
                .await;
            // the answer never shows that the mail was skipped
            assert_eq!(res.status(), Status::NoContent);
        }

        assert_eq!(mails_to(&email).await, 1);
    }

    #[rocket::async_test]
    async fn success() {
        let harness = TestHarness::new().await;
        // the cooldown outlives a test run, so every run needs a new address
        let email = fresh_address();

        Account::new(
            &harness.db,
            email.clone(),
            "password".into(),
            false,
        )
        .await
        .unwrap();

        let res = harness.client
            .post("/auth/account/reset_password")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": email,
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::NoContent);

        let (_, code) = harness.assert_email(&email).await;
        let res = harness.client
            .patch("/auth/account/reset_password")
            .header(ContentType::JSON)
            .body(
                json!({
                    "token": code,
                    "password": "valid password"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::NoContent);

        let res = harness.client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": email,
                    "password": "valid password"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        assert!(serde_json::from_str::<v0::Session>(&res.into_string().await.unwrap()).is_ok());
    }
}
