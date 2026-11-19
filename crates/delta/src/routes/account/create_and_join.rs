//! Create an account from an invite link and enter the server
//! POST /account/join
use std::time::Duration;

use iso8601_timestamp::Timestamp;
use nanoid::nanoid;
use revolt_config::config;
use revolt_database::{
    util::{
        age::{band_for_birth, parse_birth_month},
        captcha::check_captcha,
        email::{normalise_email, validate_email},
        reference::Reference,
        shield::{validate_shield, ShieldValidationInput},
    },
    Account, AccountAge, Database, EmailVerification, PartialUser, ProvisionalAccount,
    TermsAcceptance, User, AMQP,
};
use revolt_models::v0::{self, AgeBand, RE_DISPLAY_NAME};
use revolt_result::{create_error, ErrorType, Result};
use rocket::serde::json::Json;
use rocket::State;
use tokio::time::sleep;

use crate::routes::invites::invite_join::join_invite;

/// # Create Account And Join
///
/// The short path from an invite link (`features.join_by_link_enabled`): a
/// name, an e-mail address and a month and year of birth make an account that
/// is signed in and joined to the invite at once.
///
/// The account has no password: the confirmation mail's link asks for one.
/// Until the e-mail is confirmed it cannot open direct messages, join or
/// create another server, create invites or bots, or send files, and crond
/// removes it after `features.join_confirm_hours`.
///
/// Every sign-up check applies (captcha, sign-ups per source, minimum age,
/// blocked addresses) and joining goes through the checks of
/// `POST /invites/{code}`. A join the server refuses (ban, paused invites)
/// still leaves an account: `join_error` says why.
///
/// For an address that already has an account the answer is `CheckEmail` and
/// the usual mail goes out; nothing is created or joined.
#[openapi(tag = "Account")]
#[post("/join", data = "<data>")]
pub async fn create_and_join(
    db: &State<Database>,
    amqp: &State<AMQP>,
    data: Json<v0::DataCreateAndJoin>,
    mut shield: ShieldValidationInput,
    client: crate::routes::safety::public_report::ClientAddress,
) -> Result<Json<v0::ResponseCreateAndJoin>> {
    let config = config().await;
    // without mail there is no way to set the password afterwards
    if !config.features.join_by_link_enabled || config.api.smtp.host.is_empty() {
        return Err(create_error!(FeatureDisabled {
            feature: "join_by_link".to_string()
        }));
    }

    let data = data.into_inner();

    // Random jitter from 0-1000ms
    sleep(Duration::from_millis((rand::random::<f32>() * 1000.) as u64)).await;

    // Check Captcha token
    check_captcha(data.captcha.as_deref()).await?;

    // Even with a solved captcha, a few new accounts a day per source address
    let per_day = config.api.security.captcha.signups_per_source_per_day;
    if !revolt_database::util::signup_limit::allow_signup(&client.0, per_day).await {
        return Err(create_error!(RateLimited {
            retry_after: 60 * 60 * 1000,
            global: false
        }));
    }

    // The minimum age, before anything is stored
    let birth = parse_birth_month(&data.birth_month)?;
    if band_for_birth(birth) == AgeBand::UnderMinimum {
        return Err(create_error!(AgeRequirementNotMet));
    }

    // Validate the request
    shield.email = Some(data.email.to_string());
    validate_shield(shield).await?;

    // Make sure email is valid and not blocked
    validate_email(&data.email)?;

    let display_name = data.display_name.trim().to_string();
    let length = display_name.chars().count();
    if !(2..=32).contains(&length) || !RE_DISPLAY_NAME.is_match(&display_name) {
        return Err(create_error!(IncorrectData {
            with: "display_name".to_string()
        }));
    }

    // A dead link makes no account
    let invite = Reference::from_unchecked(&data.invite).as_invite(db).await?;
    revolt_database::check_vanity_open(db, &invite).await?;

    // The password nobody knows: the confirmation link asks for a real one
    let placeholder = nanoid!(64);

    // An address that has an account: the same mail as a normal sign-up, and
    // nothing about the account is told here
    if db
        .fetch_account_by_normalised_email(&normalise_email(data.email.clone()))
        .await?
        .is_some()
    {
        Account::new_with_age_and_terms(db, data.email, placeholder, true, None, None).await?;
        return Ok(Json(v0::ResponseCreateAndJoin::CheckEmail));
    }

    let mut account = Account::new_with_age_and_terms(
        db,
        data.email,
        placeholder,
        true,
        Some(AccountAge::declare(birth)),
        Some(TermsAcceptance::current()),
    )
    .await?;

    // two sign-ups for one address at the same moment: the other one won
    if !matches!(account.verification, EmailVerification::Pending { .. })
        || account.provisional.is_some()
    {
        return Ok(Json(v0::ResponseCreateAndJoin::CheckEmail));
    }

    account.provisional = Some(ProvisionalAccount {
        since: Timestamp::now_utc(),
        password_unset: true,
    });
    account.save(db).await?;

    let user = match create_user(db, &account.id, &display_name).await {
        Ok(user) => user,
        Err(error) => {
            // no half-made account stays behind
            db.remove_account(&account.id).await.ok();
            return Err(error);
        }
    };

    let session = account.create_session(db, "Invite link".to_string()).await?;

    let (joined, join_error) = match join_invite(db, amqp, &user, &data.invite).await {
        Ok(joined) => (Some(joined), None),
        Err(error) => (None, Some(error_name(&error.error_type))),
    };

    Ok(Json(v0::ResponseCreateAndJoin::Entered {
        session: session.into(),
        joined,
        join_error,
        confirm_by: account
            .confirm_by()
            .await
            .unwrap_or_else(Timestamp::now_utc),
    }))
}

/// The user of a new account: the name as typed is the display name, and the
/// username is made from it with four digits, so it is free and can be
/// changed later in Settings
async fn create_user(db: &Database, account_id: &str, display_name: &str) -> Result<User> {
    let base = username_base(display_name);
    let mut last = create_error!(InvalidUsername);

    for attempt in 0..6 {
        // a name the username rules refuse falls back to a plain word
        let base = if attempt < 3 { base.as_str() } else { "member" };
        let username = format!("{base}_{:04}", rand::random::<u16>() % 10_000);
        match User::create(
            db,
            username,
            account_id.to_string(),
            PartialUser {
                display_name: Some(display_name.to_string()),
                ..Default::default()
            },
        )
        .await
        {
            Ok(user) => return Ok(user),
            Err(error)
                if matches!(
                    error.error_type,
                    ErrorType::UsernameTaken | ErrorType::InvalidUsername
                ) =>
            {
                last = error
            }
            Err(error) => return Err(error),
        }
    }

    Err(last)
}

/// What is left of a display name once only letters, digits, `_`, `.` and `-`
/// are kept (spaces become `_`), at most 24 characters; "member" when that
/// is too short
fn username_base(display_name: &str) -> String {
    let base: String = display_name
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
        .take(24)
        .collect();
    let base = base.trim_matches(|c| matches!(c, '_' | '.' | '-')).to_string();

    if base.chars().count() < 2 {
        "member".to_string()
    } else {
        base
    }
}

/// The name of an error type, as clients read it in `type`
fn error_name(error: &ErrorType) -> String {
    let name: &'static str = error.into();
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::username_base;
    use crate::{rocket, util::test::TestHarness};
    use iso8601_timestamp::Timestamp;
    use revolt_config::overwrite_config;
    use revolt_database::{Channel, EmailVerification, PartialServer, Server, User};
    use revolt_models::v0::{self, DataCreateInvite, Invite, ResponseCreateAndJoin};
    use rocket::http::{ContentType, Header, Status};
    use rocket::local::asynchronous::LocalResponse;
    use serde_json::{json, Value};

    async fn enabled() -> TestHarness {
        overwrite_config(|config| config.features.join_by_link_enabled = true).await;
        TestHarness::new().await
    }

    /// A server of a new owner and an invite to its first channel
    async fn server_with_invite(harness: &TestHarness) -> (User, Server, Vec<Channel>, String) {
        let (_, session, owner) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        let response = TestHarness::with_session(
            session,
            harness
                .client
                .post(format!("/channels/{}/invites", channels[0].id()))
                .json(&DataCreateInvite {
                    max_uses: None,
                    expires: None,
                    never_expires: false,
                }),
        )
        .await;
        assert_eq!(response.status(), Status::Ok);
        let code = match response.into_json::<Invite>().await.expect("`Invite`") {
            Invite::Server { code, .. } => code,
            _ => unreachable!(),
        };
        (owner, server, channels, code)
    }

    fn address() -> String {
        format!("{}@validemail.com", TestHarness::rand_string().to_lowercase())
    }

    async fn sign_up<'a>(harness: &'a TestHarness, body: Value) -> LocalResponse<'a> {
        harness
            .client
            .post("/auth/account/join")
            .header(ContentType::JSON)
            .body(body.to_string())
            .dispatch()
            .await
    }

    fn body(email: &str, invite: &str) -> Value {
        json!({ "email": email, "display_name": "Lina Haddad", "birth_month": "1998-03", "invite": invite })
    }

    /// Sign up and return the session token and user id
    async fn entered(harness: &TestHarness, email: &str, invite: &str) -> (String, String, ResponseCreateAndJoin) {
        let response = sign_up(harness, body(email, invite)).await;
        assert_eq!(response.status(), Status::Ok);
        let answer: ResponseCreateAndJoin = response.into_json().await.expect("`ResponseCreateAndJoin`");
        match &answer {
            ResponseCreateAndJoin::Entered { session, .. } => {
                (session.token.clone(), session.user_id.clone(), answer)
            }
            _ => panic!("not entered"),
        }
    }

    fn token(value: &str) -> Header<'static> {
        Header::new("x-session-token", value.to_string())
    }

    #[rocket::async_test]
    async fn off_by_default() {
        let harness = TestHarness::new().await;
        let (_, _, _, code) = server_with_invite(&harness).await;
        let response = sign_up(&harness, body(&address(), &code)).await;
        assert_eq!(response.status(), Status::BadRequest);
        let error: Value = response.into_json().await.unwrap();
        assert_eq!(error["type"], "FeatureDisabled");
    }

    #[rocket::async_test]
    async fn one_request_makes_a_member_who_can_talk() {
        let harness = enabled().await;
        let (_, server, channels, code) = server_with_invite(&harness).await;
        let email = address();

        let (session, user_id, answer) = entered(&harness, &email, &code).await;
        match answer {
            ResponseCreateAndJoin::Entered { joined, join_error, confirm_by, .. } => {
                assert!(join_error.is_none());
                assert!(matches!(joined, Some(v0::InviteJoinResponse::Server { .. })));
                assert!(confirm_by > Timestamp::now_utc());
            }
            _ => unreachable!(),
        }

        // a member, with the name as typed and a username made from it
        harness.db.fetch_member(&server.id, &user_id).await.expect("`Member`");
        let user = harness.db.fetch_user(&user_id).await.expect("`User`");
        assert_eq!(user.display_name.as_deref(), Some("Lina Haddad"));
        assert!(user.username.starts_with("Lina_Haddad_"));

        // the account: age and terms on record, e-mail not confirmed yet
        let account = harness.db.fetch_account(&user_id).await.expect("`Account`");
        assert!(account.age.is_some());
        assert!(account.terms.is_some());
        assert!(matches!(account.verification, EmailVerification::Pending { .. }));
        assert!(account.provisional.is_some_and(|p| p.password_unset));

        // the first message
        let response = harness
            .client
            .post(format!("/channels/{}/messages", channels[0].id()))
            .header(token(&session))
            .json(&json!({ "content": "hi everyone" }))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);

        // the account says when it must be confirmed
        let response = harness.client.get("/auth/account").header(token(&session)).dispatch().await;
        let info: v0::AccountInfo = response.into_json().await.unwrap();
        assert!(info.confirm_by.is_some());
    }

    #[rocket::async_test]
    async fn under_sixteen_stores_nothing() {
        let harness = enabled().await;
        let (_, _, _, code) = server_with_invite(&harness).await;
        let email = address();
        let year = Timestamp::now_utc().year() - 14;

        let response = sign_up(
            &harness,
            json!({ "email": email, "display_name": "Lina", "birth_month": format!("{year}-01"), "invite": code }),
        )
        .await;
        assert_eq!(response.status(), Status::Forbidden);
        assert!(harness
            .db
            .fetch_account_by_normalised_email(&email)
            .await
            .unwrap()
            .is_none());
    }

    #[rocket::async_test]
    async fn a_dead_link_makes_no_account() {
        let harness = enabled().await;
        let email = address();
        let response = sign_up(&harness, body(&email, "nosuchinvite")).await;
        assert_eq!(response.status(), Status::NotFound);
        assert!(harness
            .db
            .fetch_account_by_normalised_email(&email)
            .await
            .unwrap()
            .is_none());
    }

    #[rocket::async_test]
    async fn a_bad_name_is_refused() {
        let harness = enabled().await;
        let (_, _, _, code) = server_with_invite(&harness).await;
        for name in ["", " L ", "line\nbreak", "a name that is far too long to be a display name"] {
            let response = sign_up(
                &harness,
                json!({ "email": address(), "display_name": name, "birth_month": "1998-03", "invite": code }),
            )
            .await;
            assert_eq!(response.status(), Status::BadRequest, "{name:?}");
        }
    }

    #[rocket::async_test]
    async fn an_address_with_an_account_gets_mail_and_nothing_else() {
        let harness = enabled().await;
        let (_, server, _, code) = server_with_invite(&harness).await;
        let (account, _, user) = harness.new_user().await;

        let response = sign_up(&harness, body(&account.email, &code)).await;
        assert_eq!(response.status(), Status::Ok);
        let answer: Value = response.into_json().await.unwrap();
        assert_eq!(answer, json!({ "result": "CheckEmail" }));
        assert!(harness.db.fetch_member(&server.id, &user.id).await.is_err());
    }

    #[rocket::async_test]
    async fn an_approval_server_gets_a_request_not_a_member() {
        let harness = enabled().await;
        let (_, server, _, code) = server_with_invite(&harness).await;
        harness
            .db
            .update_server(
                &server.id,
                &PartialServer { access: Some(v0::ServerAccess::Approval), ..Default::default() },
                vec![],
            )
            .await
            .unwrap();

        let (_, user_id, answer) = entered(&harness, &address(), &code).await;
        assert!(matches!(
            answer,
            ResponseCreateAndJoin::Entered { joined: Some(v0::InviteJoinResponse::JoinRequest { .. }), .. }
        ));
        assert!(harness.db.fetch_member(&server.id, &user_id).await.is_err());
    }

    #[rocket::async_test]
    async fn paused_invites_refuse_the_join_and_say_so() {
        let harness = enabled().await;
        let (_, server, _, code) = server_with_invite(&harness).await;
        harness
            .db
            .update_server(
                &server.id,
                &PartialServer {
                    invites_paused_until: Some(
                        Timestamp::now_utc() + iso8601_timestamp::Duration::hours(1),
                    ),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        let (_, user_id, answer) = entered(&harness, &address(), &code).await;
        match answer {
            ResponseCreateAndJoin::Entered { joined, join_error, .. } => {
                assert!(joined.is_none());
                assert_eq!(join_error.as_deref(), Some("InvitesPaused"));
            }
            _ => unreachable!(),
        }
        assert!(harness.db.fetch_member(&server.id, &user_id).await.is_err());
    }

    #[rocket::async_test]
    async fn limits_hold_until_the_email_is_confirmed() {
        let harness = enabled().await;
        let (owner, _, channels, code) = server_with_invite(&harness).await;
        let (_, _, _, other_code) = server_with_invite(&harness).await;
        let (session, _, _) = entered(&harness, &address(), &code).await;

        let refused = |response: LocalResponse<'_>| {
            assert_eq!(response.status(), Status::Forbidden);
        };

        // a second server
        refused(harness.client.post(format!("/invites/{other_code}")).header(token(&session)).dispatch().await);
        // a server of their own
        refused(
            harness.client.post("/servers/create").header(token(&session))
                .json(&json!({ "name": "Mine" })).dispatch().await,
        );
        // a direct message
        refused(harness.client.get(format!("/users/{}/dm", owner.id)).header(token(&session)).dispatch().await);
        // a friend request
        refused(
            harness.client.post("/users/friend").header(token(&session))
                .json(&json!({ "username": owner.username })).dispatch().await,
        );
        // an invite
        refused(
            harness.client.post(format!("/channels/{}/invites", channels[0].id()))
                .header(token(&session)).json(&json!({})).dispatch().await,
        );
        // a bot
        refused(
            harness.client.post("/bots/create").header(token(&session))
                .json(&json!({ "name": "helper" })).dispatch().await,
        );
        // a group
        refused(
            harness.client.post("/channels/create").header(token(&session))
                .json(&json!({ "name": "group" })).dispatch().await,
        );
    }

    #[rocket::async_test]
    async fn confirming_sets_the_password_and_lifts_the_limits() {
        let harness = enabled().await;
        let (_, _, _, code) = server_with_invite(&harness).await;
        let (_, _, _, other_code) = server_with_invite(&harness).await;
        let email = address();
        let (session, user_id, _) = entered(&harness, &email, &code).await;

        let account = harness.db.fetch_account(&user_id).await.unwrap();
        let EmailVerification::Pending { token: link, .. } = account.verification else {
            panic!("not pending")
        };

        // no log-in before: there is no password to know, and no reset mail
        let response = harness.client.post("/auth/session/login")
            .json(&json!({ "email": email, "password": "password_insecure" })).dispatch().await;
        assert_eq!(response.status(), Status::Unauthorized);

        // the link says a password is needed, and waits for one
        let response = harness.client.get(format!("/auth/account/verify/{link}")).dispatch().await;
        assert_eq!(response.into_json::<Value>().await.unwrap(), json!({ "needs_password": true }));
        let response = harness.client.post(format!("/auth/account/verify/{link}")).dispatch().await;
        assert_eq!(response.status(), Status::BadRequest);
        assert!(harness.db.fetch_account(&user_id).await.unwrap().provisional.is_some());

        let response = harness.client.post(format!("/auth/account/verify/{link}"))
            .json(&json!({ "password": "a good long password 42" })).dispatch().await;
        assert_eq!(response.status(), Status::Ok);

        let account = harness.db.fetch_account(&user_id).await.unwrap();
        assert!(account.provisional.is_none());
        assert!(matches!(account.verification, EmailVerification::Verified));

        // the password works, and another server is open now
        let response = harness.client.post("/auth/session/login")
            .json(&json!({ "email": email, "password": "a good long password 42" })).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let response = harness.client.post(format!("/invites/{other_code}")).header(token(&session)).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
    }

    #[rocket::async_test]
    async fn after_the_deadline_the_unconfirmed_are_removed_and_nobody_else() {
        use revolt_database::Account;

        let harness = enabled().await;
        let (_, server, channels, code) = server_with_invite(&harness).await;
        let (confirmed_account, _, confirmed_user) = harness.new_user().await;

        // one wrote a message, one did nothing, one signed up a moment ago
        let (wrote_session, wrote, _) = entered(&harness, &address(), &code).await;
        let (_, silent, _) = entered(&harness, &address(), &code).await;
        let (_, fresh, _) = entered(&harness, &address(), &code).await;
        let response = harness
            .client
            .post(format!("/channels/{}/messages", channels[0].id()))
            .header(token(&wrote_session))
            .json(&json!({ "content": "hi everyone" }))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);

        // nothing is due yet
        assert_eq!(Account::remove_unconfirmed(&harness.db, 72).await.unwrap(), 0);

        // 73 hours later, for the first two
        for id in [&wrote, &silent] {
            let mut account = harness.db.fetch_account(id).await.unwrap();
            let provisional = account.provisional.as_mut().unwrap();
            provisional.since = Timestamp::now_utc() - iso8601_timestamp::Duration::hours(73);
            account.save(&harness.db).await.unwrap();
        }
        assert_eq!(Account::remove_unconfirmed(&harness.db, 72).await.unwrap(), 2);

        // the silent one is gone entirely
        assert!(harness.db.fetch_account(&silent).await.is_err());
        assert!(harness.db.fetch_user(&silent).await.is_err());
        assert!(harness.db.fetch_member(&server.id, &silent).await.is_err());

        // the one who wrote is deleted like any account: no address, no way in
        let account = harness.db.fetch_account(&wrote).await.unwrap();
        assert!(account.provisional.is_none());
        assert!(!account.email.contains("validemail.com"));
        let response = harness.client.get("/auth/account").header(token(&wrote_session)).dispatch().await;
        assert_ne!(response.status(), Status::Ok);
        assert!(harness.db.fetch_member(&server.id, &wrote).await.is_err());

        // the new sign-up and the ordinary account are untouched
        assert!(harness.db.fetch_account(&fresh).await.unwrap().provisional.is_some());
        harness.db.fetch_member(&server.id, &fresh).await.expect("`Member`");
        assert_eq!(
            harness.db.fetch_account(&confirmed_account.id).await.unwrap().email,
            confirmed_account.email
        );
        harness.db.fetch_user(&confirmed_user.id).await.expect("`User`");

        // and a second look removes nobody
        assert_eq!(Account::remove_unconfirmed(&harness.db, 72).await.unwrap(), 0);
    }

    #[test]
    fn usernames_from_names() {
        assert_eq!(username_base("Lina Haddad"), "Lina_Haddad");
        assert_eq!(username_base("لينا"), "لينا");
        assert_eq!(username_base("  ✨ L ✨ "), "member");
        assert_eq!(username_base("a@b.c"), "ab.c");
        assert_eq!(username_base("-.dots.-"), "dots");
        assert_eq!(username_base(&"x".repeat(60)).chars().count(), 24);
    }
}
