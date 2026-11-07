use crate::{rocket, util::test::TestHarness};
use revolt_database::{hash_secret, Bot, MFATicket, Member, OAuthCode, Server, User};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

// RFC 7636 appendix B
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT: &str = "http://localhost:9999/callback";

struct Setup {
    harness: TestHarness,
    bot: Bot,
    owner: User,
    owner_session: String,
    server: Server,
    secret: String,
}

/// An app with a registered redirect URI and a client secret, and a server its owner manages
async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    Member::create(&harness.db, &server, &owner, None).await.unwrap();
    let (bot, _) = harness.new_bot(&owner).await;

    let response = harness
        .client
        .patch(format!("/applications/{}/oauth", bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "redirect_uris": [REDIRECT] }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);

    let ticket = MFATicket::new(owner.id.clone(), true);
    ticket.save(&harness.db).await.unwrap();
    let response = harness
        .client
        .post(format!("/applications/{}/oauth/secret", bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(Header::new("x-mfa-ticket", ticket.token))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let secret = response.into_json::<v0::OAuthClientSecret>().await.unwrap().client_secret;

    Setup {
        harness,
        bot,
        owner,
        owner_session: session.token,
        server,
        secret,
    }
}

fn query(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

impl Setup {
    fn authorize_body(&self, scope: &str, approve: bool, extra: Value) -> String {
        let mut body = json!({
            "client_id": self.bot.id,
            "redirect_uri": REDIRECT,
            "response_type": "code",
            "scope": scope,
            "state": "xyz-state",
            "code_challenge": CHALLENGE,
            "code_challenge_method": "S256",
            "approve": approve,
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body.to_string()
    }

    async fn authorize(&self, session: &str, body: String) -> (Status, Option<String>) {
        let response = self
            .harness
            .client
            .post("/oauth2/authorize")
            .header(Header::new("x-session-token", session.to_string()))
            .header(ContentType::JSON)
            .body(body)
            .dispatch()
            .await;
        let status = response.status();
        let redirect = if status == Status::Ok {
            response
                .into_json::<v0::AuthorizeRedirect>()
                .await
                .map(|r| r.redirect)
        } else {
            None
        };
        (status, redirect)
    }

    async fn token(&self, pairs: &[(&str, &str)]) -> (Status, Value) {
        let response = self
            .harness
            .client
            .post("/oauth2/token")
            .header(ContentType::Form)
            .body(query(pairs))
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    async fn exchange(&self, code: &str) -> (Status, Value) {
        self.token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("client_id", &self.bot.id),
            ("client_secret", &self.secret),
            ("code_verifier", VERIFIER),
        ])
        .await
    }

    async fn bearer_get(&self, path: &str, token: &str) -> Status {
        self.harness
            .client
            .get(path)
            .header(Header::new("Authorization", format!("Bearer {token}")))
            .dispatch()
            .await
            .status()
    }
}

fn code_of(redirect: &str) -> String {
    let url = url::Url::parse(redirect).unwrap();
    assert!(redirect.starts_with(REDIRECT));
    assert_eq!(
        url.query_pairs().find(|(k, _)| k == "state").unwrap().1,
        "xyz-state"
    );
    url.query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned()
}

#[rocket::async_test]
async fn consent_install_and_tokens() {
    let s = setup().await;

    // Consent page data lists the managed server
    let response = s
        .harness
        .client
        .get(format!(
            "/oauth2/authorize?{}",
            query(&[
                ("client_id", &s.bot.id),
                ("redirect_uri", REDIRECT),
                ("response_type", "code"),
                ("scope", "identify bot.install applications.commands"),
                ("state", "xyz-state"),
                ("code_challenge", CHALLENGE),
                ("code_challenge_method", "S256"),
                ("permissions", "1048576"),
            ])
        ))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let preview: v0::AuthorizationPreview = response.into_json().await.unwrap();
    assert_eq!(preview.servers.len(), 1);
    assert_eq!(preview.servers[0].id, s.server.id);

    // Approve: the bot joins the server and a code comes back with the state
    let (status, redirect) = s
        .authorize(
            &s.owner_session,
            s.authorize_body(
                "identify bot.install applications.commands",
                true,
                json!({ "server_id": s.server.id, "permissions": 1048576 }),
            ),
        )
        .await;
    assert_eq!(status, Status::Ok);
    let code = code_of(&redirect.unwrap());
    assert!(s.harness.db.fetch_member(&s.server.id, &s.bot.id).await.is_ok());
    let installs = s.harness.db.fetch_app_installs(&s.server.id).await.unwrap();
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].installed_by, s.owner.id);
    assert_eq!(installs[0].via, "oauth");
    assert_eq!(installs[0].permissions, 1048576);

    // Exchange the code (PKCE + secret)
    let (status, tokens) = s.exchange(&code).await;
    assert_eq!(status, Status::Ok, "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 3600);
    let access = tokens["access_token"].as_str().unwrap().to_string();
    let refresh = tokens["refresh_token"].as_str().unwrap().to_string();

    // A code works once
    let (status, error) = s.exchange(&code).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["error"], "invalid_grant");

    // identify works; servers.read was not granted; the token is not a session
    assert_eq!(s.bearer_get("/oauth2/@me", &access).await, Status::Ok);
    assert_eq!(s.bearer_get("/oauth2/@me/servers", &access).await, Status::Forbidden);
    assert_eq!(s.bearer_get("/users/@me", &access).await, Status::Unauthorized);

    // Refresh replaces both tokens; the old refresh token used again ends the family
    let (status, renewed) = s
        .token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &s.bot.id),
            ("client_secret", &s.secret),
        ])
        .await;
    assert_eq!(status, Status::Ok, "{renewed}");
    let new_access = renewed["access_token"].as_str().unwrap().to_string();
    assert_eq!(s.bearer_get("/oauth2/@me", &access).await, Status::Unauthorized);
    assert_eq!(s.bearer_get("/oauth2/@me", &new_access).await, Status::Ok);

    let (status, error) = s
        .token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &s.bot.id),
            ("client_secret", &s.secret),
        ])
        .await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["error"], "invalid_grant");
    assert_eq!(s.bearer_get("/oauth2/@me", &new_access).await, Status::Unauthorized);

    // The user sees the app among authorized apps and can revoke it
    let response = s
        .harness
        .client
        .get("/oauth2/authorized")
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    let apps: Vec<v0::AuthorizedApplication> = response.into_json().await.unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].application.id, s.bot.id);
}

#[rocket::async_test]
async fn bad_requests_are_never_redirected() {
    let s = setup().await;

    for extra in [
        json!({ "redirect_uri": "http://localhost:9999/other" }),
        json!({ "redirect_uri": "https://evil.example/callback" }),
        json!({ "response_type": "token" }),
        json!({ "code_challenge_method": "plain" }),
        json!({ "code_challenge": "" }),
        json!({ "state": "" }),
        json!({ "scope": "identify email" }),
        json!({ "permissions": 8 }),
    ] {
        let (status, redirect) = s
            .authorize(&s.owner_session, s.authorize_body("identify", true, extra.clone()))
            .await;
        assert_eq!(status, Status::BadRequest, "{extra}");
        assert!(redirect.is_none());
    }

    // Denying goes back with access_denied and no code
    let (status, redirect) = s
        .authorize(&s.owner_session, s.authorize_body("identify", false, json!({})))
        .await;
    assert_eq!(status, Status::Ok);
    let redirect = redirect.unwrap();
    assert!(redirect.contains("error=access_denied") && !redirect.contains("code="));
}

#[rocket::async_test]
async fn token_endpoint_checks_everything() {
    let s = setup().await;
    let mut codes = vec![];
    for _ in 0..3 {
        let (_, redirect) = s
            .authorize(&s.owner_session, s.authorize_body("identify", true, json!({})))
            .await;
        codes.push(code_of(&redirect.unwrap()));
    }

    // Wrong secret, missing secret
    let (status, error) = s
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &codes[0]),
            ("redirect_uri", REDIRECT),
            ("client_id", &s.bot.id),
            ("client_secret", "zcs_wrong"),
            ("code_verifier", VERIFIER),
        ])
        .await;
    assert_eq!(status, Status::Unauthorized);
    assert_eq!(error["error"], "invalid_client");

    // Wrong PKCE verifier (the code is spent anyway)
    let (status, error) = s
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &codes[1]),
            ("redirect_uri", REDIRECT),
            ("client_id", &s.bot.id),
            ("client_secret", &s.secret),
            ("code_verifier", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXX"),
        ])
        .await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["error"], "invalid_grant");
    let (status, _) = s.exchange(&codes[1]).await;
    assert_eq!(status, Status::BadRequest);

    // Redirect URI must match the authorization request
    let (status, error) = s
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &codes[2]),
            ("redirect_uri", "http://localhost:9999/other"),
            ("client_id", &s.bot.id),
            ("client_secret", &s.secret),
            ("code_verifier", VERIFIER),
        ])
        .await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["error"], "invalid_grant");

    // Expired code
    s.harness
        .db
        .insert_oauth_code(&OAuthCode {
            code_hash: hash_secret("expired-code"),
            application_id: s.bot.id.clone(),
            user_id: s.owner.id.clone(),
            redirect_uri: REDIRECT.to_string(),
            scopes: vec!["identify".to_string()],
            code_challenge: CHALLENGE.to_string(),
            expires_at: iso8601_timestamp::Timestamp::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let (status, error) = s.exchange("expired-code").await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["error"], "invalid_grant");

    let (status, error) = s.token(&[("grant_type", "password"), ("client_id", &s.bot.id), ("client_secret", &s.secret)]).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["error"], "unsupported_grant_type");
}

#[rocket::async_test]
async fn revocation_and_uninstall() {
    let s = setup().await;
    let (_, redirect) = s
        .authorize(
            &s.owner_session,
            s.authorize_body("identify servers.read bot.install", true, json!({ "server_id": s.server.id })),
        )
        .await;
    let (_, tokens) = s.exchange(&code_of(&redirect.unwrap())).await;
    let access = tokens["access_token"].as_str().unwrap().to_string();
    assert_eq!(s.bearer_get("/oauth2/@me/servers", &access).await, Status::Ok);

    // RFC 7009 revocation by the client
    let response = s
        .harness
        .client
        .post("/oauth2/revoke")
        .header(ContentType::Form)
        .body(query(&[("token", &access), ("client_id", &s.bot.id), ("client_secret", &s.secret)]))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    assert_eq!(s.bearer_get("/oauth2/@me", &access).await, Status::Unauthorized);

    // A member without Manage Server cannot see or remove apps
    let (_, member_session, member) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &member, None).await.unwrap();
    let response = s
        .harness
        .client
        .delete(format!("/servers/{}/applications/{}", s.server.id, s.bot.id))
        .header(Header::new("x-session-token", member_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);
    drop(response);

    // The owner uninstalls: membership and record go
    let response = s
        .harness
        .client
        .delete(format!("/servers/{}/applications/{}", s.server.id, s.bot.id))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);
    assert!(s.harness.db.fetch_member(&s.server.id, &s.bot.id).await.is_err());
    assert!(s.harness.db.fetch_app_installs(&s.server.id).await.unwrap().is_empty());

    // The user revokes the app: its grant goes away
    let response = s
        .harness
        .client
        .delete(format!("/oauth2/authorized/{}", s.bot.id))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
}

#[rocket::async_test]
async fn install_cannot_escalate_permissions() {
    let s = setup().await;
    // A moderator with Manage Server and Manage Roles, but not Ban Members
    let (_, session, moderator) = s.harness.new_user().await;
    let role = s
        .harness
        .new_role(
            &s.server,
            1,
            Some(revolt_permissions::OverrideField { a: 2 | 8, d: 0 }),
        )
        .await;
    let (mut member, _) = Member::create(&s.harness.db, &s.server, &moderator, None).await.unwrap();
    member
        .update(
            &s.harness.db,
            revolt_database::PartialMember {
                roles: Some(vec![role.id.clone()]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // The app is public so others may install it
    s.harness
        .db
        .update_bot(
            &s.bot.id,
            &revolt_database::PartialBot {
                public: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let (status, redirect) = s
        .authorize(
            &session.token,
            s.authorize_body(
                "bot.install",
                true,
                // Ban Members
                json!({ "server_id": s.server.id, "permissions": 128 }),
            ),
        )
        .await;
    assert_eq!(status, Status::Forbidden);
    assert!(redirect.is_none());
    assert!(s.harness.db.fetch_member(&s.server.id, &s.bot.id).await.is_err());
    assert!(s.harness.db.fetch_app_installs(&s.server.id).await.unwrap().is_empty());

    // The old invite route records installs too
    let response = s
        .harness
        .client
        .post(format!("/bots/{}/invite", s.bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "server": s.server.id }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);
    let installs = s.harness.db.fetch_app_installs(&s.server.id).await.unwrap();
    assert_eq!(installs[0].via, "invite");
    assert_eq!(installs[0].installed_by, moderator.id);
}

#[rocket::async_test]
async fn client_secret_needs_the_owner_and_reauthentication() {
    let s = setup().await;
    // Without an MFA ticket
    let response = s
        .harness
        .client
        .post(format!("/applications/{}/oauth/secret", s.bot.id))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Unauthorized);
    drop(response);
    // With the bot's own token
    let response = s
        .harness
        .client
        .post(format!("/applications/{}/oauth/secret", s.bot.id))
        .header(Header::new("x-bot-token", s.bot.token.clone()))
        .dispatch()
        .await;
    assert_ne!(response.status(), Status::Ok);
    drop(response);
    // Invalid redirect URIs are refused
    let response = s
        .harness
        .client
        .patch(format!("/applications/{}/oauth", s.bot.id))
        .header(Header::new("x-session-token", s.owner_session.clone()))
        .header(ContentType::JSON)
        .body(json!({ "redirect_uris": ["http://example.com/cb"] }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
}
