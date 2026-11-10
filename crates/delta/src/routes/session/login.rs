//! Login to an account
//! POST /session/login
use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_database::{
    util::{
        captcha::check_captcha,
        email::normalise_email,
        login_limit::{clear_login_failures, count_login_failure, login_gate, LoginGate},
        mfa_limit::{clear_mfa_failures, count_mfa_failure, mfa_is_paused},
    },
    Account, Database, EmailVerification, MFATicket,
};
use revolt_models::v0;
use revolt_result::{create_error, ErrorType, Result};
use rocket::serde::json::Json;
use rocket::State;
use tokio::time::sleep;

/// # Login
///
/// Login to an account.
#[openapi(tag = "Session")]
#[post("/login", data = "<data>")]
pub async fn login(
    db: &State<Database>,
    data: Json<v0::DataLogin>,
) -> Result<Json<v0::ResponseLogin>> {
    // Random jitter from 0-1000ms
    sleep(Duration::from_millis(
        (rand::random::<f32>() * 1000.) as u64,
    ))
    .await;

    let (account, name) = match data.into_inner() {
        v0::DataLogin::Email {
            email,
            password,
            friendly_name,
            captcha,
        } => {
            let email_normalised = normalise_email(email);

            // Many wrong passwords for this address: a captcha (when the
            // server has one) or a wait. Counted per address whether or not
            // an account has it, so the answer tells nothing about accounts.
            let captcha_enabled = !config().await.api.security.captcha.hcaptcha_key.is_empty();
            match login_gate(&email_normalised, captcha_enabled).await {
                LoginGate::Open => {}
                LoginGate::Captcha => check_captcha(captcha.as_deref()).await?,
                LoginGate::Paused(_) => return Err(create_error!(LockedOut)),
            }

            // Lookup the email in database
            let Some(mut account) = db
                .fetch_account_by_normalised_email(&email_normalised)
                .await?
            else {
                // Same work and same answer as a wrong password
                Account::spend_password_check_time(&password);
                count_login_failure(&email_normalised, captcha_enabled).await;
                return Err(create_error!(InvalidCredentials));
            };

            // Verify the password is correct. Nothing else about the
            // account is told before this passes (enumeration).
            if let Err(err) = account.verify_password(&password) {
                count_login_failure(&email_normalised, captcha_enabled).await;
                return Err(err);
            }
            clear_login_failures(&email_normalised).await;

            // Make sure the account has been verified
            if let EmailVerification::Pending { .. } = account.verification {
                return Err(create_error!(UnverifiedAccount));
            }

            // Check whether an MFA step is required
            if account.mfa.is_active() {
                // Create a new ticket
                let mut ticket = MFATicket::new(account.id, false);
                ticket.populate(&account.mfa).await;
                ticket.save(db).await?;

                // Return applicable methods
                return Ok(Json(v0::ResponseLogin::MFA {
                    ticket: ticket.token,
                    allowed_methods: account
                        .mfa
                        .get_methods()
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                }));
            }

            // The per-account lock of earlier releases is not used any more
            // (only real accounts got it, so it told who has one): clear
            // what is left of it
            if account.lockout.is_some() {
                account.lockout = None;
                account.save(db).await?;
            }

            (account, friendly_name)
        }
        v0::DataLogin::MFA {
            mfa_ticket,
            mfa_response,
            friendly_name,
        } => {
            // Resolve the MFA ticket
            let ticket = &mut db.fetch_ticket_by_token(&mfa_ticket).await?;

            // Find the corresponding account
            let mut account = db.fetch_account(&ticket.account_id).await?;

            // Too many wrong answers for this account, here or at
            // PUT /auth/mfa/ticket (five in fifteen minutes): pause,
            // whichever ticket is asking
            if mfa_is_paused(&account.id).await {
                return Err(create_error!(LockedOut));
            }

            // Verify the MFA response
            if let Some(mfa_response) = mfa_response {
                if let Err(err) = account
                    .consume_mfa_response(db, mfa_response, Some(ticket))
                    .await
                {
                    if !matches!(err.error_type, ErrorType::DisallowedMFAMethod) {
                        count_mfa_failure(&account.id).await;
                    }
                    return Err(err);
                }
                clear_mfa_failures(&account.id).await;
            } else if !ticket.authorised {
                return Err(create_error!(InvalidToken));
            }

            // MFA passed, clear lockout information if present
            if account.lockout.is_some() {
                account.lockout = None;
                account.save(db).await?;
            }

            (account, friendly_name)
        }
    };

    // Generate a session name
    let name = name.unwrap_or_else(|| "Unknown".to_string());

    // A Trust & Safety suspension that has run out is lifted here, even if
    // the retention job has not got to it yet
    let mut account = account;
    if account.disabled
        && let Ok(mut user) = db.fetch_user(&account.id).await
        && user.flags.unwrap_or_default() & v0::UserFlags::SuspendedUntil as i32 != 0
        && user
            .suspended_until
            .is_some_and(|until| until <= Timestamp::now_utc())
    {
        user.unsuspend(db).await?;
        account.disabled = false;
    }

    // Prevent disabled accounts from logging in
    if account.disabled {
        return Ok(Json(v0::ResponseLogin::Disabled {
            user_id: account.id,
        }));
    }

    // Create and return a new session
    Ok(Json(v0::ResponseLogin::Success(
        account.create_session(db, name).await?.into(),
    )))
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use iso8601_timestamp::Timestamp;
    use revolt_config::overwrite_config;
    use revolt_database::{
        events::client::EventV1,
        util::{
            email::normalise_email,
            login_limit::{clear_login_failures, LOGIN_STEP_UP_AFTER},
            mfa_limit::clear_mfa_failures,
        },
        Account, EmailVerification, Lockout, MFATicket, Totp,
    };
    use revolt_models::v0;
    use revolt_result::{Error, ErrorType};
    use crate::util::test::PubSubTestHelper;
    use rocket::http::{ContentType, Status};

    #[rocket::async_test]
    async fn success() {
        let harness = TestHarness::new().await;

        let account = Account::new(
            &harness.db,
            "example@validemail.com".into(),
            "password_insecure".into(),
            false,
        )
        .await
        .unwrap();

        let mut pubsub = PubSubTestHelper::new("global").await;

        let res = harness
            .client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "EXAMPLE@validemail.com",
                    "password": "password_insecure"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        assert!(res.into_json::<v0::Session>().await.is_some());

        let event = pubsub.wait_for_event(|event| match event {
            EventV1::CreateSession { session } => session.user_id == account.id,
            _ => false,
        }).await;
        if !matches!(event, EventV1::CreateSession { .. }) {
            panic!("Received incorrect event type. {:?}", event);
        }
    }

    #[rocket::async_test]
    async fn a_sign_in_over_the_limit_closes_the_oldest_with_its_event() {
        let harness = TestHarness::new().await;
        let (account, first, _) = harness.new_user().await;
        let mut pubsub = PubSubTestHelper::new(&format!("{}!", &account.id)).await;

        // the first session has the oldest last_seen
        let mut oldest = first.clone();
        oldest.last_seen = Timestamp::now_utc() + iso8601_timestamp::Duration::days(-2);
        oldest.subscription = Some(revolt_database::WebPushSubscription {
            endpoint: "https://fcm.googleapis.com/fcm/send/a".to_string(),
            p256dh: "key".to_string(),
            auth: "auth".to_string(),
        });
        harness.db.save_session(&oldest).await.unwrap();

        for number in 1..10 {
            account
                .create_session(&harness.db, format!("session{number}"))
                .await
                .unwrap();
        }

        let res = harness
            .client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": account.email,
                    "password": "password_insecure",
                })
                .to_string(),
            )
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);
        let new = res.into_json::<v0::Session>().await.unwrap();

        let closed = first.id.clone();
        pubsub
            .wait_for_event(move |event| {
                matches!(event, EventV1::DeleteSession { session_id, .. } if *session_id == closed)
            })
            .await;

        let left = harness.db.fetch_sessions(&account.id).await.unwrap();
        assert_eq!(left.len(), 10);
        assert!(left.iter().any(|session| session.id == new.id));
        // the push subscription lives on the session and goes with it
        assert!(harness.db.fetch_session(&first.id).await.is_err());
        assert!(harness.db.fetch_session_by_token(&first.token).await.is_err());
    }

    #[rocket::async_test]
    async fn success_totp_mfa() {
        let harness = TestHarness::new().await;
        let (mut account, _, _) = harness.new_user().await;

        let totp = Totp::Enabled {
            secret: "secret".to_string(),
        };

        account.mfa.totp_token = totp.clone();
        account.save(&harness.db).await.unwrap();

        let res = harness
            .client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": &account.email,
                    "password": "password_insecure"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        let response = serde_json::from_str::<v0::ResponseLogin>(&res.into_string().await.unwrap())
            .expect("`ResponseLogin`");

        if let v0::ResponseLogin::MFA {
            ticket,
            allowed_methods,
        } = response
        {
            assert!(allowed_methods.contains(&v0::MFAMethod::Totp));

            let res = harness
                .client
                .post("/auth/session/login")
                .header(ContentType::JSON)
                .body(
                    json!({
                        "mfa_ticket": ticket,
                        "mfa_response": {
                            "totp_code": totp.generate_code().expect("totp code")
                        }
                    })
                    .to_string(),
                )
                .dispatch()
                .await;

            assert_eq!(res.status(), Status::Ok);
            assert!(serde_json::from_str::<v0::Session>(&res.into_string().await.unwrap()).is_ok());
        } else {
            panic!("expected `ResponseLogin::MFA`")
        }
    }

    #[rocket::async_test]
    async fn success_totp_stored_mfa() {
        let harness = TestHarness::new().await;
        let (mut account, _, _) = harness.new_user().await;

        let totp = Totp::Enabled {
            secret: "secret".to_string(),
        };

        account.mfa.totp_token = totp.clone();
        account.save(&harness.db).await.unwrap();

        let mut ticket = MFATicket::new(account.id.to_string(), true);
        ticket.last_totp_code = Some("token from earlier".into());
        ticket.save(&harness.db).await.unwrap();

        let res = harness
            .client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(
                json!({
                    "mfa_ticket": ticket.token,
                    "mfa_response": {
                        "totp_code": "token from earlier"
                    }
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        assert!(serde_json::from_str::<v0::Session>(&res.into_string().await.unwrap()).is_ok());
    }

    #[rocket::async_test]
    async fn fail_totp_invalid_mfa() {
        let harness = TestHarness::new().await;
        let (mut account, _, _) = harness.new_user().await;

        let totp = Totp::Enabled {
            secret: "secret".to_string(),
        };

        account.mfa.totp_token = totp.clone();
        account.save(&harness.db).await.unwrap();

        let res = harness
            .client
            .post("/auth/session/login")
            .json(&json!({
                "email": account.email.clone(),
                "password": "password_insecure"
            }))
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        let response = serde_json::from_str::<v0::ResponseLogin>(&res.into_string().await.unwrap())
            .expect("`ResponseLogin`");

        if let v0::ResponseLogin::MFA {
            ticket,
            allowed_methods,
        } = response
        {
            assert!(allowed_methods.contains(&v0::MFAMethod::Totp));

            let res = harness
                .client
                .post("/auth/session/login")
                .json(&json!({
                    "mfa_ticket": ticket,
                    "mfa_response": {
                        "totp_code": "some random data"
                    }
                }))
                .dispatch()
                .await;

            assert_eq!(res.status(), Status::Unauthorized);
            assert!(matches!(
                res.into_json::<Error>().await.unwrap().error_type,
                ErrorType::InvalidToken,
            ));
        } else {
            panic!("expected `ResponseLogin::MFA`")
        }
    }

    #[rocket::async_test]
    async fn fail_invalid_user() {
        let harness = TestHarness::new().await;

        let res = harness
            .client
            .post("/auth/session/login")
            .json(&json!({
                "email": "example@validemail.com",
                "password": "password_insecure"
            }))
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Unauthorized);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::InvalidCredentials,
        ));
    }

    #[rocket::async_test]
    async fn fail_disabled_account() {
        let harness = TestHarness::new().await;

        let mut account = Account::new(
            &harness.db,
            "example@validemail.com".into(),
            "password_insecure".into(),
            false,
        )
        .await
        .unwrap();

        account.disabled = true;
        account.save(&harness.db).await.unwrap();

        let res = harness
            .client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "example@validemail.com",
                    "password": "password_insecure"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        let response = serde_json::from_str::<v0::ResponseLogin>(&res.into_string().await.unwrap())
            .expect("`ResponseLogin`");

        assert!(matches!(response, v0::ResponseLogin::Disabled { .. }));
    }

    #[rocket::async_test]
    async fn fail_unverified_account() {
        let harness = TestHarness::new().await;

        let mut account = Account::new(
            &harness.db,
            "example@validemail.com".into(),
            "password_insecure".into(),
            false,
        )
        .await
        .unwrap();

        account.verification = EmailVerification::Pending {
            token: "".to_string(),
            expiry: Timestamp::now_utc(),
        };

        account.save(&harness.db).await.unwrap();

        let res = harness
            .client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(
                json!({
                    "email": "example@validemail.com",
                    "password": "password_insecure"
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Forbidden);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::UnverifiedAccount,
        ));
    }

    #[rocket::async_test]
    async fn login_tells_nothing_before_the_password_is_right() {
        let harness = TestHarness::new().await;

        // unknown address
        let unknown = attempt(&harness, "nobody_here@validemail.com", "password1234").await;

        // known address, unverified, and a password from a breach list
        let mut account = Account::new(
            &harness.db,
            "enumeration_probe@validemail.com".into(),
            "password_insecure".into(),
            false,
        )
        .await
        .unwrap();
        account.verification = EmailVerification::Pending {
            token: "token".into(),
            expiry: Timestamp::now_utc() + iso8601_timestamp::Duration::hours(1),
        };
        account.save(&harness.db).await.unwrap();
        let known = attempt(&harness, "enumeration_probe@validemail.com", "password1234").await;

        assert_eq!(unknown.0, Status::Unauthorized);
        assert_eq!(known.0, unknown.0);
        assert!(matches!(unknown.1, Some(ErrorType::InvalidCredentials)));
        assert!(matches!(known.1, Some(ErrorType::InvalidCredentials)));
    }

    async fn attempt(harness: &TestHarness, email: &str, password: &str) -> (Status, Option<ErrorType>) {
        let res = harness
            .client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(json!({ "email": email, "password": password }).to_string())
            .dispatch()
            .await;
        let status = res.status();
        let error = res.into_json::<Error>().await.map(|e| e.error_type);
        (status, error)
    }

    fn unknown_address() -> String {
        format!("nobody_{}@validemail.com", TestHarness::rand_string().to_lowercase())
    }

    /// Ten wrong passwords pause the address for a minute, and an address
    /// without an account answers exactly like one with an account.
    #[rocket::async_test]
    async fn ten_wrong_passwords_pause_known_and_unknown_addresses_alike() {
        let harness = TestHarness::new().await;
        let (account, _, _) = harness.new_user().await;
        let unknown = unknown_address();

        for _ in 0..LOGIN_STEP_UP_AFTER {
            for email in [account.email.as_str(), unknown.as_str()] {
                let (status, error) = attempt(&harness, email, "wrong_password").await;
                assert_eq!(status, Status::Unauthorized);
                assert!(matches!(error, Some(ErrorType::InvalidCredentials)));
            }
        }

        // the right password waits too: nobody else can tell it was right
        let known = attempt(&harness, &account.email, "password_insecure").await;
        let other = attempt(&harness, &unknown, "password_insecure").await;
        assert_eq!(known.0, Status::Forbidden);
        assert!(matches!(known.1, Some(ErrorType::LockedOut)));
        assert_eq!(other.0, known.0);
        assert!(matches!(other.1, Some(ErrorType::LockedOut)));

        clear_login_failures(&normalise_email(account.email.clone())).await;
        clear_login_failures(&normalise_email(unknown)).await;
    }

    #[rocket::async_test]
    async fn a_right_password_forgets_the_wrong_ones() {
        let harness = TestHarness::new().await;
        let (account, _, _) = harness.new_user().await;

        for _ in 0..2 {
            for _ in 0..LOGIN_STEP_UP_AFTER - 1 {
                let (status, _) = attempt(&harness, &account.email, "wrong_password").await;
                assert_eq!(status, Status::Unauthorized);
            }
            let (status, _) = attempt(&harness, &account.email, "password_insecure").await;
            assert_eq!(status, Status::Ok);
        }
    }

    /// With a captcha set up, many wrong passwords never lock the owner out:
    /// the address asks for a captcha instead, for accounts and non-accounts
    /// alike, and a fresh address needs none.
    #[rocket::async_test]
    async fn with_a_captcha_many_wrong_passwords_ask_for_one() {
        overwrite_config(|config| {
            config.api.security.captcha.hcaptcha_key =
                "0x0000000000000000000000000000000000000000".to_string()
        })
        .await;
        let harness = TestHarness::new().await;
        let (account, _, _) = harness.new_user().await;
        let (fresh, _, _) = harness.new_user().await;
        let unknown = unknown_address();

        for _ in 0..LOGIN_STEP_UP_AFTER {
            for email in [account.email.as_str(), unknown.as_str()] {
                let (status, error) = attempt(&harness, email, "wrong_password").await;
                assert_eq!(status, Status::Unauthorized);
                assert!(matches!(error, Some(ErrorType::InvalidCredentials)));
            }
        }

        // from now on the captcha comes first, for wrong and right passwords
        for password in ["wrong_password", "password_insecure"] {
            for email in [account.email.as_str(), unknown.as_str()] {
                let (status, error) = attempt(&harness, email, password).await;
                assert_eq!(status, Status::BadRequest);
                assert!(matches!(error, Some(ErrorType::CaptchaFailed)));
            }
        }

        let (status, _) = attempt(&harness, &fresh.email, "password_insecure").await;
        assert_eq!(status, Status::Ok);

        clear_login_failures(&normalise_email(account.email.clone())).await;
        clear_login_failures(&normalise_email(unknown)).await;
    }

    /// A lock written by an earlier release (only accounts had one, so it
    /// told who has an account) no longer stops the right password
    #[rocket::async_test]
    async fn an_old_account_lock_is_ignored_and_cleared() {
        let harness = TestHarness::new().await;
        let (mut account, _, _) = harness.new_user().await;
        account.lockout = Some(Lockout {
            attempts: 5,
            expiry: Some(Timestamp::now_utc() + iso8601_timestamp::Duration::minutes(10)),
            last_attempt: None,
        });
        account.save(&harness.db).await.unwrap();

        let (status, _) = attempt(&harness, &account.email, "password_insecure").await;
        assert_eq!(status, Status::Ok);
        let after = harness.db.fetch_account(&account.id).await.unwrap();
        assert!(after.lockout.is_none());
    }

    async fn mfa_ticket(harness: &TestHarness, email: &str) -> String {
        let res = harness
            .client
            .post("/auth/session/login")
            .json(&json!({ "email": email, "password": "password_insecure" }))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);
        match serde_json::from_str::<v0::ResponseLogin>(&res.into_string().await.unwrap()).unwrap() {
            v0::ResponseLogin::MFA { ticket, .. } => ticket,
            _ => panic!("expected `ResponseLogin::MFA`"),
        }
    }

    async fn wrong_code(harness: &TestHarness, ticket: &str) -> (Status, Option<ErrorType>) {
        let res = harness
            .client
            .post("/auth/session/login")
            .json(&json!({ "mfa_ticket": ticket, "mfa_response": { "totp_code": "000000" } }))
            .dispatch()
            .await;
        let status = res.status();
        (status, res.into_json::<Error>().await.map(|e| e.error_type))
    }

    /// Wrong two-factor codes at sign-in count with those at
    /// PUT /auth/mfa/ticket: five in fifteen minutes pause the two-factor
    /// step of the account, whatever ticket asks
    #[rocket::async_test]
    async fn five_wrong_codes_at_sign_in_pause_two_factor() {
        let harness = TestHarness::new().await;
        let (mut account, _, _) = harness.new_user().await;
        account.mfa.totp_token = Totp::Enabled {
            secret: "secret".to_string(),
        };
        account.save(&harness.db).await.unwrap();

        // a ticket takes at most three tries, so two tickets
        for tries in [3, 2] {
            let ticket = mfa_ticket(&harness, &account.email).await;
            for _ in 0..tries {
                let (status, error) = wrong_code(&harness, &ticket).await;
                assert_eq!(status, Status::Unauthorized);
                assert!(matches!(error, Some(ErrorType::InvalidToken)));
            }
        }

        let ticket = mfa_ticket(&harness, &account.email).await;
        let (status, error) = wrong_code(&harness, &ticket).await;
        assert_eq!(status, Status::Forbidden);
        assert!(matches!(error, Some(ErrorType::LockedOut)));

        clear_mfa_failures(&account.id).await;
    }
}
