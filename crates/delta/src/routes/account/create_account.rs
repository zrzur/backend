//! Create a new account
//! POST /account/create
use std::time::Duration;

use tokio::time::sleep;
use revolt_config::config;
use revolt_database::{
    util::{
        captcha::check_captcha,
        email::validate_email,
        age::{band_for_birth, parse_birth_month},
        password::assert_safe,
        shield::{validate_shield, ShieldValidationInput},
    },
    Account, AccountAge, Database, TermsAcceptance,
};
use revolt_models::v0::{self, AgeBand};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use rocket_empty::EmptyResponse;

/// # Create Account
///
/// Create a new account.
#[openapi(tag = "Account")]
#[post("/create", data = "<data>")]
pub async fn create_account(
    db: &State<Database>,
    data: Json<v0::DataCreateAccount>,
    mut shield: ShieldValidationInput,
    client: crate::routes::safety::public_report::ClientAddress,
) -> Result<EmptyResponse> {
    let data = data.into_inner();

    // Random jitter from 0-1000ms
    sleep(Duration::from_millis((rand::random::<f32>() * 1000.) as u64)).await;

    // Check Captcha token
    check_captcha(data.captcha.as_deref()).await?;

    // Even with a solved captcha, a few new accounts a day per source address
    let per_day = config().await.api.security.captcha.signups_per_source_per_day;
    if !revolt_database::util::signup_limit::allow_signup(&client.0, per_day).await {
        // retry_after is in milliseconds: the window resets within a day
        return Err(create_error!(RateLimited {
            retry_after: 60 * 60 * 1000,
            global: false
        }));
    }

    // Check the minimum age; nothing is stored for a refused sign-up.
    // With `require_birth_month` off, an age is still checked when given.
    let birth = match data.birth_month.as_deref() {
        Some(value) => Some(parse_birth_month(value)?),
        None if config().await.features.age_safety.require_birth_month => {
            return Err(create_error!(IncorrectData {
                with: "birth_month".to_string()
            }))
        }
        None => None,
    };
    if birth.is_some_and(|birth| band_for_birth(birth) == AgeBand::UnderMinimum) {
        return Err(create_error!(AgeRequirementNotMet));
    }

    // Validate the request
    shield.email = Some(data.email.to_string());
    validate_shield(shield).await?;

    // Make sure email is valid and not blocked
    validate_email(&data.email)?;

    // Ensure password is safe to use
    assert_safe(&data.password).await?;

    // If required, fetch valid invite
    let invite = if config().await.api.registration.invite_only {
        if let Some(invite) = data.invite {
            // Claim atomically before creating the account, so one invite makes one account
            Some(db.claim_account_invite(&invite).await?)
        } else {
            return Err(create_error!(MissingInvite));
        }
    } else {
        None
    };

    // Create account. Signing up accepts the Terms of Service the sign-up
    // page links to; which version and when is kept on the account (T-6).
    let account = match Account::new_with_age_and_terms(
        db,
        data.email,
        data.password,
        true,
        birth.map(AccountAge::declare),
        Some(TermsAcceptance::current()),
    )
    .await
    {
        Ok(account) => account,
        Err(err) => {
            if let Some(invite) = &invite {
                db.release_account_invite(&invite.id).await.ok();
            }
            return Err(err);
        }
    };

    // Record who used the invite
    if let Some(mut invite) = invite {
        invite.claimed_by = Some(account.id);
        invite.used = true;

        db.save_account_invite(&invite).await?;
    }

    Ok(EmptyResponse)
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use revolt_config::overwrite_config;
    use revolt_database::{events::client::EventV1, AccountInvite};
    use revolt_result::{Error, ErrorType};
    use rocket::http::{ContentType, Status};
    use crate::util::test::PubSubTestHelper;

    #[rocket::async_test]
    async fn success() {
        let harness = TestHarness::new().await;
        let mut pubsub = PubSubTestHelper::new("global").await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "success@validemail.com",
                    "password": "valid password",
                    "birth_month": "1990-01"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::NoContent);
        drop(res);

        pubsub
            .wait_for_event(|event| match event {
                EventV1::CreateAccount { account } => account.email == "success@validemail.com",
                _ => false,
            }).await;
    }

    /// Sign-up records the terms version and the time (T-6); a second
    /// sign-up with the same address changes nothing on the account
    #[rocket::async_test]
    async fn sign_up_records_the_accepted_terms() {
        let harness = TestHarness::new().await;
        let email = format!("{}@validemail.com", TestHarness::rand_string().to_lowercase());
        let before = revolt_database::iso8601_timestamp::Timestamp::now_utc();
        let sign_up = || {
            harness
                .client
                .post("/auth/account/create")
                .header(ContentType::JSON)
                .body(
                    json!({
                        "email": email,
                        "password": "valid password",
                        "birth_month": "1990-01"
                    })
                    .to_string(),
                )
                .dispatch()
        };
        assert_eq!(sign_up().await.status(), Status::NoContent);

        let normalised = revolt_database::util::email::normalise_email(email.clone());
        let account = harness
            .db
            .fetch_account_by_normalised_email(&normalised)
            .await
            .unwrap()
            .expect("account");
        let terms = account.terms.clone().expect("terms accepted at sign-up");
        assert_eq!(terms.version, revolt_database::TERMS_VERSION);
        assert!(terms.accepted_at >= before);

        assert_eq!(sign_up().await.status(), Status::NoContent);
        let again = harness
            .db
            .fetch_account_by_normalised_email(&normalised)
            .await
            .unwrap()
            .expect("account");
        assert_eq!(again.terms, Some(terms));
    }

    #[rocket::async_test]
    async fn fail_invalid_email() {
        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "invalid",
                    "password": "valid password",
                    "birth_month": "1990-01"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::BadRequest);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::IncorrectData { .. },
        ));
    }

    #[rocket::async_test]
    async fn fail_invalid_password() {
        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "fail_invalid_password@validemail.com",
                    "password": "password1234",
                    "birth_month": "1990-01"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::BadRequest);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::CompromisedPassword,
        ));
    }

    #[rocket::async_test]
    async fn fail_invalid_invite() {
        overwrite_config(|config| config.api.registration.invite_only = true).await;

        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "fail_invalid_invite@validemail.com",
                    "password": "valid password",
                    "birth_month": "1990-01",
                    "invite": "invalid"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::BadRequest);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::InvalidInvite,
        ));
    }

    #[rocket::async_test]
    async fn success_valid_invite() {
        overwrite_config(|config| config.api.registration.invite_only = true).await;

        let harness = TestHarness::new().await;

        let invite = AccountInvite {
            id: "invite".to_string(),
            used: false,
            claimed_by: None,
        };

        invite.save(&harness.db).await.unwrap();

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "success_valid_invite@validemail.com",
                    "password": "valid password",
                    "birth_month": "1990-01",
                    "invite": "invite"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::NoContent);

        let invite = harness
            .db
            .fetch_account_invite("invite")
            .await
            .expect("`Invite`");

        assert!(invite.used);
    }

    #[rocket::async_test]
    async fn fail_invite_reused() {
        overwrite_config(|config| config.api.registration.invite_only = true).await;

        let harness = TestHarness::new().await;

        AccountInvite {
            id: "single_use_invite".to_string(),
            used: false,
            claimed_by: None,
        }
        .save(&harness.db)
        .await
        .unwrap();

        let mut statuses = vec![];
        for email in ["reuse_a@validemail.com", "reuse_b@validemail.com"] {
            let res = harness
                .client
                .post("/auth/account/create")
                .header(ContentType::JSON)
                .body(
                    json!({
                        "email": email,
                        "password": "valid password",
                        "birth_month": "1990-01",
                        "invite": "single_use_invite"
                    })
                    .to_string(),
                )
                .dispatch()
                .await;
            statuses.push(res.status());
        }

        assert_eq!(statuses, vec![Status::NoContent, Status::BadRequest]);
        assert!(harness
            .db
            .fetch_account_by_normalised_email("reuse_b@validemail.com")
            .await
            .unwrap()
            .is_none());
    }

    #[rocket::async_test]
    async fn fail_missing_captcha() {
        overwrite_config(|config| {
            config.api.security.captcha.turnstile_secret =
                "1x0000000000000000000000000000000AA".to_string()
        })
        .await;

        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "fail_missing_captcha@validemail.com",
                    "password": "valid password",
                    "birth_month": "1990-01",
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::BadRequest);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::CaptchaFailed,
        ));
    }

    #[rocket::async_test]
    async fn fail_captcha_invalid() {
        // Cloudflare's published test secret that rejects every token
        overwrite_config(|config| {
            config.api.security.captcha.turnstile_secret =
                "2x0000000000000000000000000000000AA".to_string()
        })
        .await;

        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "fail_captcha_invalid@validemail.com",
                    "password": "valid password",
                    "birth_month": "1990-01",
                    "captcha": "00000000-aaaa-bbbb-cccc-000000000000"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::BadRequest);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::CaptchaFailed,
        ));
    }

    #[rocket::async_test]
    async fn success_captcha_valid() {
        overwrite_config(|config| {
            config.api.security.captcha.turnstile_secret =
                "1x0000000000000000000000000000000AA".to_string()
        })
        .await;

        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "success_captcha_valid@validemail.com",
                    "password": "valid password",
                    "birth_month": "1990-01",
                    "captcha": "XXXX.DUMMY.TOKEN.XXXX"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::NoContent);
    }

    #[rocket::async_test]
    async fn success_smtp_sent() {
        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "success_smtp_sent@smtp.test",
                    "password": "valid password",
                    "birth_month": "1990-01",
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::NoContent);

        let (_, code) = harness.assert_email("success_smtp_sent@smtp.test").await;
        let res = harness
            .client
            .post(format!("/auth/account/verify/{code}"))
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
    }

    #[rocket::async_test]
    async fn success_stores_birth_month() {
        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "success_stores_birth_month@validemail.com",
                    "password": "valid password",
                    "birth_month": "1990-01",
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::NoContent);

        let account = harness
            .db
            .fetch_account_by_normalised_email("success_stores_birth_month@validemail.com")
            .await
            .unwrap()
            .expect("`Account`");

        let age = account.age.as_ref().expect("age");
        assert_eq!(age.birth_month, "1990-01");
        assert_eq!(account.age_band(), revolt_models::v0::AgeBand::AdultDeclared);
    }

    /// The month and year of birth are asked at the first entry instead
    #[rocket::async_test]
    async fn success_without_birth_month() {
        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "success_without_birth_month@validemail.com",
                    "password": "valid password",
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::NoContent);
        let account = harness
            .db
            .fetch_account_by_normalised_email("success_without_birth_month@validemail.com")
            .await
            .unwrap()
            .expect("account");
        assert!(account.age.is_none());
    }

    #[rocket::async_test]
    async fn fail_under_minimum_age() {
        let harness = TestHarness::new().await;
        let now = iso8601_timestamp::Timestamp::now_utc();
        let birth_month = format!("{:04}-{:02}", now.year() - 15, u8::from(now.month()));

        let res = harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "fail_under_minimum_age@validemail.com",
                    "password": "valid password",
                    "birth_month": birth_month,
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Forbidden);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::AgeRequirementNotMet,
        ));

        // Nothing is stored for a refused sign-up
        assert!(harness
            .db
            .fetch_account_by_normalised_email("fail_under_minimum_age@validemail.com")
            .await
            .unwrap()
            .is_none());
    }

    /// `YYYY-MM` for `months` months ago
    fn months_ago(months: i32) -> String {
        let now = iso8601_timestamp::Timestamp::now_utc();
        let total = now.year() * 12 + u8::from(now.month()) as i32 - 1 - months;
        format!("{:04}-{:02}", total / 12, total % 12 + 1)
    }

    async fn sign_up(harness: &TestHarness, email: &str, birth_month: &str) -> Status {
        harness
            .client
            .post("/auth/account/create")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": email,
                    "password": "valid password",
                    "birth_month": birth_month,
                })
                .to_string(),
            )
            .dispatch()
            .await
            .status()
    }

    #[rocket::async_test]
    async fn minimum_age_boundary() {
        let harness = TestHarness::new().await;

        // Born in this month 16 years ago: the birthday may not have passed,
        // so the account is refused (the day is never asked)
        assert_eq!(
            sign_up(&harness, "boundary_15@validemail.com", &months_ago(16 * 12)).await,
            Status::Forbidden
        );

        // One month older: certainly 16
        assert_eq!(
            sign_up(&harness, "boundary_16@validemail.com", &months_ago(16 * 12 + 1)).await,
            Status::NoContent
        );
        let account = harness
            .db
            .fetch_account_by_normalised_email("boundary_16@validemail.com")
            .await
            .unwrap()
            .expect("`Account`");
        assert_eq!(account.age_band(), revolt_models::v0::AgeBand::Teen);
    }

    #[rocket::async_test]
    async fn fail_future_or_invalid_birth_month() {
        let harness = TestHarness::new().await;

        for (index, value) in [months_ago(-1), "1990-13".into(), "1990-1".into(), "1800-01".into()]
            .iter()
            .enumerate()
        {
            assert_eq!(
                sign_up(&harness, &format!("invalid_birth_{index}@validemail.com"), value).await,
                Status::BadRequest,
                "{value}"
            );
        }
    }
}
