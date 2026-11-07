use futures::TryStreamExt;
use revolt_database::mongodb::bson::{doc, Document};
use revolt_database::{
    staff_now, Account, Database, Session, StaffGrant, StaffRole, Totp, User, AUDIT_GENESIS_HASH,
};
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use crate::util::test::TestHarness;

const SECRET: &str = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";

/// What is stored for every staff session, read past the API
async fn stored_token_hashes(harness: &TestHarness) -> Vec<String> {
    match &harness.db {
        Database::Reference(db) => {
            let staff = db.platform_staff.lock().await;
            staff.sessions.values().map(|s| s.token_hash.clone()).collect()
        }
        Database::MongoDb(db) => {
            let sessions: Vec<Document> = db
                .col::<Document>("staff_sessions")
                .find(doc! {})
                .await
                .unwrap()
                .try_collect()
                .await
                .unwrap();
            sessions
                .iter()
                .map(|s| s.get_str("token_hash").unwrap().to_string())
                .collect()
        }
    }
}

/// Changes a stored audit record, as someone with database access could
async fn tamper_with_audit_record(harness: &TestHarness, seq: i64) {
    match &harness.db {
        Database::Reference(db) => {
            let mut staff = db.platform_staff.lock().await;
            staff.audit[seq as usize - 1].reason = Some("edited".to_string());
        }
        Database::MongoDb(db) => {
            db.col::<Document>("platform_audit")
                .update_one(doc! { "seq": seq }, doc! { "$set": { "reason": "edited" } })
                .await
                .unwrap();
        }
    }
}

/// A TOTP code `offset` steps from now (-1, 0 and +1 are accepted)
fn code(offset: i64) -> String {
    let key = base32::decode(base32::Alphabet::RFC4648 { padding: false }, SECRET).unwrap();
    let step = (staff_now() / 1000) as u64 / 30;
    let step = (step as i64 + offset) as u64;
    totp_lite::totp_custom::<totp_lite::Sha1>(30, 6, &key, step * 30)
}

struct Member {
    user: User,
    session: Session,
}

async fn enable_totp(harness: &TestHarness, account: &mut Account) {
    account.mfa.totp_token = Totp::Enabled {
        secret: SECRET.to_string(),
    };
    account.save(&harness.db).await.unwrap();
}

async fn grant(harness: &TestHarness, user_id: &str, roles: Vec<StaffRole>) {
    let current = harness.db.fetch_staff_grant(user_id).await.unwrap();
    let grant = StaffGrant {
        id: user_id.to_string(),
        roles,
        disabled: false,
        expires_at: None,
        last_totp_step: current.as_ref().map(|g| g.last_totp_step).unwrap_or(0),
        version: current.as_ref().map(|g| g.version + 1).unwrap_or(1),
        updated_at: staff_now(),
        updated_by: "test".to_string(),
    };
    assert!(harness
        .db
        .save_staff_grant(&grant, current.map(|g| g.version))
        .await
        .unwrap());
}

/// A user with TOTP on and the given staff roles
async fn member(harness: &TestHarness, roles: Vec<StaffRole>) -> Member {
    let (mut account, session, user) = harness.new_user().await;
    enable_totp(harness, &mut account).await;
    if !roles.is_empty() {
        grant(harness, &user.id, roles).await;
    }
    Member { user, session }
}

async fn open(harness: &TestHarness, session: &Session, code: &str) -> (Status, Value) {
    let res = harness
        .client
        .post("/staff/session")
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "totp_code": code }).to_string())
        .dispatch()
        .await;
    let status = res.status();
    (status, res.into_json().await.unwrap_or(Value::Null))
}

/// Opens a staff session, returns its token
async fn sign_in(harness: &TestHarness, m: &Member, offset: i64) -> String {
    let (status, body) = open(harness, &m.session, &code(offset)).await;
    assert_eq!(status, Status::Ok, "{body}");
    body["token"].as_str().unwrap().to_string()
}

async fn get(harness: &TestHarness, m: &Member, token: &str, path: &str) -> (Status, Value) {
    let res = harness
        .client
        .get(path.to_string())
        .header(Header::new("x-session-token", m.session.token.clone()))
        .header(Header::new("x-staff-session", token.to_string()))
        .dispatch()
        .await;
    let status = res.status();
    (status, res.into_json().await.unwrap_or(Value::Null))
}

async fn send(
    harness: &TestHarness,
    m: &Member,
    token: &str,
    method: rocket::http::Method,
    path: &str,
    body: Value,
) -> (Status, Value) {
    let res = harness
        .client
        .req(method, path.to_string())
        .header(Header::new("x-session-token", m.session.token.clone()))
        .header(Header::new("x-staff-session", token.to_string()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = res.status();
    (status, res.into_json().await.unwrap_or(Value::Null))
}

async fn audit_actions(harness: &TestHarness) -> Vec<(String, String)> {
    harness
        .db
        .fetch_platform_audit(revolt_database::PlatformAuditQuery {
            limit: 500,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.action, format!("{:?}", e.outcome)))
        .collect()
}

#[rocket::async_test]
async fn outsiders_are_refused() {
    let harness = TestHarness::new().await;

    // no session at all
    let res = harness.client.get("/staff/me").dispatch().await;
    assert_eq!(res.status(), Status::Unauthorized);

    // an ordinary user: no staff session, and none can be opened
    let (_, session, user) = harness.new_user().await;
    let res = harness
        .client
        .get("/staff/me")
        .header(Header::new("x-session-token", session.token.clone()))
        .header(Header::new("x-staff-session", "made-up"))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Unauthorized);
    assert_eq!(open(&harness, &session, &code(0)).await.0, Status::Forbidden);

    // a server owner (server Administrator and more) gets nothing either
    harness.new_server(&user).await;
    assert_eq!(open(&harness, &session, &code(0)).await.0, Status::Forbidden);

    // nor does the old `privileged` flag
    let (mut account, session, user) = harness.new_user().await;
    enable_totp(&harness, &mut account).await;
    harness
        .db
        .update_user(
            &user.id,
            &revolt_database::PartialUser {
                privileged: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(open(&harness, &session, &code(0)).await.0, Status::Forbidden);

    // and ordinary refusals leave the audit log alone
    assert!(audit_actions(&harness).await.is_empty());
}

#[rocket::async_test]
async fn signing_in_needs_a_fresh_totp_code() {
    let harness = TestHarness::new().await;

    // staff without TOTP can't sign in
    let (_, session, user) = harness.new_user().await;
    grant(&harness, &user.id, vec![StaffRole::Security]).await;
    assert_eq!(open(&harness, &session, &code(0)).await.0, Status::BadRequest);

    let m = member(&harness, vec![StaffRole::Security]).await;
    // wrong code: refused and recorded
    assert_eq!(open(&harness, &m.session, "000000").await.0, Status::Unauthorized);
    assert!(audit_actions(&harness)
        .await
        .contains(&("staff.session.create".to_string(), "Denied".to_string())));

    let token = sign_in(&harness, &m, 0).await;
    assert_eq!(token.len(), 64);
    // the same code can't be used twice, nor an older one
    assert_eq!(open(&harness, &m.session, &code(0)).await.0, Status::Unauthorized);
    assert_eq!(open(&harness, &m.session, &code(-1)).await.0, Status::Unauthorized);

    let (status, me) = get(&harness, &m, &token, "/staff/me").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(me["roles"], json!(["security"]));
    assert!(me["permissions"]
        .as_array()
        .unwrap()
        .contains(&json!("platform.audit.read")));
    assert!(!me["permissions"]
        .as_array()
        .unwrap()
        .contains(&json!("platform.staff.manage")));

    // the token is stored hashed only
    let hashes = stored_token_hashes(&harness).await;
    assert!(hashes.iter().all(|hash| *hash != token));
    assert!(hashes.contains(&revolt_database::staff_token_hash(&token)));
}

#[rocket::async_test]
async fn staff_sessions_are_bound_and_end() {
    let harness = TestHarness::new().await;
    let m = member(&harness, vec![StaffRole::Auditor]).await;
    let token = sign_in(&harness, &m, 0).await;

    // the staff token alone, with another session of the same user, fails
    // (this replaces the account, so switch TOTP back on)
    let (mut account, other) = harness.account_from_user(m.user.id.clone()).await;
    enable_totp(&harness, &mut account).await;
    let other = Member {
        user: m.user.clone(),
        session: other,
    };
    assert_eq!(get(&harness, &other, &token, "/staff/me").await.0, Status::Unauthorized);

    // browsers must come from a configured admin origin: a stranger stops at
    // CORS, one of our own sites at the staff guard
    let res = harness
        .client
        .get("/staff/me")
        .header(Header::new("x-session-token", m.session.token.clone()))
        .header(Header::new("x-staff-session", token.clone()))
        .header(Header::new("origin", "https://evil.example"))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Forbidden);
    let res = harness
        .client
        .get("/staff/me")
        .header(Header::new("x-session-token", m.session.token.clone()))
        .header(Header::new("x-staff-session", token.clone()))
        .header(Header::new("origin", "https://app.zrzur.com"))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Unauthorized);
    let res = harness
        .client
        .get("/staff/me")
        .header(Header::new("x-session-token", m.session.token.clone()))
        .header(Header::new("x-staff-session", token.clone()))
        .header(Header::new("origin", "http://localhost:31030"))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);

    // signing out ends the token
    let (status, _) = send(
        &harness,
        &m,
        &token,
        rocket::http::Method::Delete,
        "/staff/session",
        json!({}),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(get(&harness, &m, &token, "/staff/me").await.0, Status::Unauthorized);

    // removing the grant ends an open session at once
    let token = sign_in(&harness, &m, 1).await;
    grant(&harness, &m.user.id, vec![]).await;
    assert_eq!(get(&harness, &m, &token, "/staff/me").await.0, Status::Unauthorized);
}

/// The console in production: an https origin, not covered by the localhost
/// rule of development, must pass CORS as well as the staff guard.
#[rocket::async_test]
async fn the_api_answers_vary_origin_for_every_origin() {
    let harness = TestHarness::new().await;
    for origin in ["https://discover.zrzur.com", "https://evil.example"] {
        let res = harness
            .client
            .get("/")
            .header(Header::new("origin", origin))
            .dispatch()
            .await;
        let vary: Vec<&str> = res.headers().get("vary").collect();
        assert!(
            vary.iter().any(|v| v.to_lowercase().contains("origin")),
            "{origin}: {vary:?}"
        );
    }
}

#[rocket::async_test]
async fn the_configured_admin_origin_passes_cors() {
    revolt_config::overwrite_config(|config| {
        config.hosts.app = "https://app.zrzur.com".to_string();
        config.api.security.staff_origins = vec!["https://admin.zrzur.com".to_string()];
    })
    .await;
    let harness = TestHarness::new().await;
    let m = member(&harness, vec![StaffRole::Auditor]).await;
    let token = sign_in(&harness, &m, 0).await;
    let res = harness
        .client
        .get("/staff/me")
        .header(Header::new("x-session-token", m.session.token.clone()))
        .header(Header::new("x-staff-session", token.clone()))
        .header(Header::new("origin", "https://admin.zrzur.com"))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(
        res.headers().get_one("access-control-allow-origin"),
        Some("https://admin.zrzur.com")
    );
}

#[rocket::async_test]
async fn each_role_sees_only_its_sections() {
    let harness = TestHarness::new().await;
    // (role, can read audit, can read staff)
    let matrix = [
        (StaffRole::Owner, true, true),
        (StaffRole::Security, true, true),
        (StaffRole::Operations, false, false),
        (StaffRole::TrustSafety, false, false),
        (StaffRole::SeniorSupport, false, false),
        (StaffRole::Support, false, false),
        (StaffRole::Billing, false, false),
        (StaffRole::Analyst, false, false),
        (StaffRole::Auditor, true, true),
    ];
    for (role, audit, staff) in matrix {
        let m = member(&harness, vec![role]).await;
        let token = sign_in(&harness, &m, 0).await;
        let expect = |ok: bool| if ok { Status::Ok } else { Status::Forbidden };
        assert_eq!(get(&harness, &m, &token, "/staff/audit").await.0, expect(audit), "{role:?}");
        assert_eq!(get(&harness, &m, &token, "/staff/members").await.0, expect(staff), "{role:?}");
        assert_eq!(get(&harness, &m, &token, "/staff/roles").await.0, expect(staff), "{role:?}");
        assert_eq!(get(&harness, &m, &token, "/staff/me").await.0, Status::Ok, "{role:?}");
        // nobody but Owner manages staff
        let (_, _, target) = harness.new_user().await;
        let (status, _) = send(
            &harness,
            &m,
            &token,
            rocket::http::Method::Put,
            &format!("/staff/members/{}", target.id),
            json!({ "roles": ["analyst"], "reason": "test grant", "version": null }),
        )
        .await;
        assert_eq!(status, Status::Forbidden, "{role:?}");
    }
    // refused attempts are recorded
    let denied = audit_actions(&harness)
        .await
        .into_iter()
        .filter(|(a, o)| a == "staff.access.denied" && o == "Denied")
        .count();
    assert!(denied >= 6 * 3, "denied records: {denied}");
}

#[rocket::async_test]
async fn staff_changes_follow_the_rules() {
    let harness = TestHarness::new().await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let token = sign_in(&harness, &owner, -1).await;
    let target = member(&harness, vec![]).await;
    let put = |roles: Value, version: Value| {
        json!({ "roles": roles, "reason": "onboarding", "version": version })
    };
    let path = format!("/staff/members/{}", target.user.id);

    // high-risk: needs a step-up first
    let (status, body) = send(
        &harness,
        &owner,
        &token,
        rocket::http::Method::Put,
        &path,
        put(json!(["support"]), Value::Null),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["type"], "NotElevated");
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        rocket::http::Method::Post,
        "/staff/session/step-up",
        json!({ "totp_code": code(0) }),
    )
    .await;
    assert_eq!(status, Status::Ok);

    // grant Support
    let (status, body) = send(
        &harness,
        &owner,
        &token,
        rocket::http::Method::Put,
        &path,
        put(json!(["support"]), Value::Null),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["version"], 1);

    // the new member signs in; changing their roles ends that session
    let target_token = sign_in(&harness, &target, 0).await;
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        rocket::http::Method::Put,
        &path,
        put(json!(["senior_support"]), json!(1)),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(
        get(&harness, &target, &target_token, "/staff/me").await.0,
        Status::Unauthorized
    );

    // stale version, Owner over the API, unknown roles, yourself: refused
    for (body, expected) in [
        (put(json!(["analyst"]), json!(1)), Status::BadRequest),
        (put(json!(["owner"]), json!(2)), Status::Forbidden),
        (put(json!(["root"]), json!(2)), Status::BadRequest),
        (json!({ "roles": ["analyst"], "reason": "x", "version": 2 }), Status::BadRequest),
    ] {
        let (status, _) =
            send(&harness, &owner, &token, rocket::http::Method::Put, &path, body).await;
        assert_eq!(status, expected);
    }
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        rocket::http::Method::Put,
        &format!("/staff/members/{}", owner.user.id),
        put(json!(["owner", "billing"]), json!(1)),
    )
    .await;
    assert_eq!(status, Status::Forbidden);

    // disabling ends access; the Owner can't be disabled over the API
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        rocket::http::Method::Post,
        &format!("/staff/members/{}/disable", target.user.id),
        json!({ "reason": "left the team" }),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(open(&harness, &target.session, &code(1)).await.0, Status::Forbidden);
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        rocket::http::Method::Post,
        &format!("/staff/members/{}/disable", owner.user.id),
        json!({ "reason": "trying it" }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);

    let actions = audit_actions(&harness).await;
    for expected in [
        ("staff.grant.update", "Success"),
        ("staff.grant.update", "Denied"),
        ("staff.grant.disable", "Success"),
        ("staff.grant.disable", "Denied"),
        ("staff.session.step_up", "Success"),
    ] {
        assert!(
            actions.contains(&(expected.0.to_string(), expected.1.to_string())),
            "missing {expected:?}"
        );
    }
}

#[rocket::async_test]
async fn platform_sections_follow_roles() {
    let harness = TestHarness::new().await;
    let (_, _, someone) = harness.new_user().await;
    let (server, _) = harness.new_server(&someone).await;
    // (role, stats, daily, users, servers, health)
    let matrix = [
        (StaffRole::Analyst, true, true, false, false, false),
        (StaffRole::Support, true, false, true, true, false),
        (StaffRole::Operations, true, true, false, false, true),
        (StaffRole::Auditor, true, false, false, false, false),
        (StaffRole::Owner, true, true, true, true, true),
    ];
    for (role, stats, daily, users, servers, health) in matrix {
        let m = member(&harness, vec![role]).await;
        let token = sign_in(&harness, &m, 0).await;
        let expect = |ok: bool| if ok { Status::Ok } else { Status::Forbidden };
        for (path, ok) in [
            ("/staff/stats".to_string(), stats),
            ("/staff/stats/daily?metric=users&days=7".to_string(), daily),
            ("/staff/users".to_string(), users),
            (format!("/staff/users/{}", someone.id), users),
            ("/staff/servers".to_string(), servers),
            (format!("/staff/servers/{}", server.id), servers),
            ("/staff/health".to_string(), health),
        ] {
            assert_eq!(get(&harness, &m, &token, &path).await.0, expect(ok), "{role:?} {path}");
        }
    }
}

#[rocket::async_test]
async fn platform_data_is_real_and_private() {
    let harness = TestHarness::new().await;
    let (_, _, someone) = harness.new_user().await;
    let (server, _) = harness.new_server(&someone).await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let token = sign_in(&harness, &owner, 0).await;

    let (_, stats) = get(&harness, &owner, &token, "/staff/stats").await;
    assert!(stats["users"]["total"].as_u64().unwrap() >= 2, "{stats}");
    assert!(stats["users"]["last_24h"].as_u64().unwrap() >= 2);
    assert!(stats["servers"]["total"].as_u64().unwrap() >= 1);
    assert!(stats["open_reports"].is_u64());

    let (_, days) = get(&harness, &owner, &token, "/staff/stats/daily?metric=servers&days=3").await;
    let days = days.as_array().unwrap();
    assert_eq!(days.len(), 3);
    assert!(days[2]["count"].as_u64().unwrap() >= 1);
    assert_eq!(
        get(&harness, &owner, &token, "/staff/stats/daily?metric=passwords").await.0,
        Status::BadRequest
    );

    // the owner sees emails, and that view is recorded
    let (_, detail) = get(&harness, &owner, &token, &format!("/staff/users/{}", someone.id)).await;
    assert_eq!(detail["id"], json!(someone.id));
    assert!(detail["email"].is_string());
    assert!(detail.get("password").is_none());
    assert!(audit_actions(&harness)
        .await
        .contains(&("staff.user.view_pii".to_string(), "Success".to_string())));

    // support sees users without emails
    let support = member(&harness, vec![StaffRole::Support]).await;
    let s_token = sign_in(&harness, &support, 0).await;
    let (_, detail) = get(&harness, &support, &s_token, &format!("/staff/users/{}", someone.id)).await;
    assert!(detail.get("email").is_none(), "{detail}");
    let (_, list) = get(&harness, &support, &s_token, "/staff/users?limit=500").await;
    let list = list.as_array().unwrap();
    assert!(list.len() <= 100);
    assert!(list.iter().all(|u| u.get("email").is_none()));

    let (_, found) = get(&harness, &owner, &token, &format!("/staff/servers?q={}", server.id)).await;
    assert_eq!(found[0]["id"], json!(server.id));
    let (_, s) = get(&harness, &owner, &token, &format!("/staff/servers/{}", server.id)).await;
    // (the harness creates servers without adding the owner as a member)
    assert!(s["members"].is_u64(), "{s}");
    assert_eq!(s["owner_id"], json!(someone.id));
    assert!(s.get("messages").is_none());
}

#[rocket::async_test]
async fn ending_user_sessions_is_guarded() {
    let harness = TestHarness::new().await;
    let (_, victim_session, victim) = harness.new_user().await;
    let senior = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let token = sign_in(&harness, &senior, 0).await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let o_token = sign_in(&harness, &owner, 0).await;

    let path = format!("/staff/users/{}/revoke-sessions", victim.id);
    let (status, _) = send(&harness, &senior, &token, rocket::http::Method::Post, &path, json!({ "reason": "x" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, count) = send(
        &harness,
        &senior,
        &token,
        rocket::http::Method::Post,
        &path,
        json!({ "reason": "account reported as compromised" }),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert!(count.as_u64().unwrap() >= 1);
    assert!(harness.db.fetch_session_by_token(&victim_session.token).await.is_err());

    // senior support can't kick an owner; the owner session survives
    let (status, _) = send(
        &harness,
        &senior,
        &token,
        rocket::http::Method::Post,
        &format!("/staff/users/{}/revoke-sessions", owner.user.id),
        json!({ "reason": "trying to kick the owner" }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(get(&harness, &owner, &o_token, "/staff/me").await.0, Status::Ok);

    // a role without the permission is refused
    let auditor = member(&harness, vec![StaffRole::Auditor]).await;
    let a_token = sign_in(&harness, &auditor, 0).await;
    let (status, _) = send(
        &harness,
        &auditor,
        &a_token,
        rocket::http::Method::Post,
        &path,
        json!({ "reason": "not allowed for me" }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
}

#[rocket::async_test]
async fn audit_chain_is_verifiable() {
    let harness = TestHarness::new().await;
    let m = member(&harness, vec![StaffRole::Auditor]).await;
    let token = sign_in(&harness, &m, 0).await;
    for _ in 0..3 {
        // refused attempts add records
        get(&harness, &m, &token, "/staff/staff-does-not-exist").await;
        send(
            &harness,
            &m,
            &token,
            rocket::http::Method::Post,
            "/staff/members/x/revoke-sessions",
            json!({ "reason": "not allowed" }),
        )
        .await;
    }
    let (status, check) = get(&harness, &m, &token, "/staff/audit/verify").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(check["ok"], true, "{check}");
    assert!(check["checked"].as_u64().unwrap() >= 4);

    let (_, page) = get(&harness, &m, &token, "/staff/audit?limit=2").await;
    let page = page.as_array().unwrap().clone();
    assert_eq!(page.len(), 2);
    assert!(page[0]["seq"].as_i64() > page[1]["seq"].as_i64());
    let first = harness
        .db
        .fetch_platform_audit_range(1, 1)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(first.prev_hash, AUDIT_GENESIS_HASH);

    // editing a stored record is detected
    tamper_with_audit_record(&harness, 2).await;
    let (_, check) = get(&harness, &m, &token, "/staff/audit/verify").await;
    assert_eq!(check["ok"], false);
    assert_eq!(check["first_bad_seq"], 2);
}

/// A reported message: (case id, author)
async fn reported_message(harness: &TestHarness) -> (String, User) {
    let (_, _, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let (_, _, author) = harness.new_user().await;
    let (_, _, message) = harness.new_message(&author, &server, channels).await;
    let (_, reporter_session, reporter) = harness.new_user().await;
    revolt_database::Member::create(&harness.db, &server, &reporter, None)
        .await
        .unwrap();
    let res = harness
        .client
        .post("/safety/reports")
        .header(Header::new("x-session-token", reporter_session.token.clone()))
        .header(ContentType::JSON)
        .body(
            json!({
                "target": { "kind": "Message", "id": message.id },
                "category": "Harassment",
                "subcategory": "targeted",
                "description": "keeps insulting me"
            })
            .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    let receipt: Value = res.into_json().await.unwrap();
    (receipt["_id"].as_str().unwrap().to_string(), author)
}

#[rocket::async_test]
async fn trust_and_safety_follows_staff_roles() {
    use rocket::http::Method::{Get, Post};
    let harness = TestHarness::new().await;
    let (case_id, author) = reported_message(&harness).await;
    let case_path = format!("/staff/safety/cases/{case_id}");

    let support = member(&harness, vec![StaffRole::Support]).await;
    let senior = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let support_token = sign_in(&harness, &support, -1).await;
    let senior_token = sign_in(&harness, &senior, 0).await;
    let ts_token = sign_in(&harness, &ts, -1).await;

    // Support has no reports access
    let (status, _) = get(&harness, &support, &support_token, "/staff/safety/cases").await;
    assert_eq!(status, Status::Forbidden);

    // Senior Support reads the queue and the case, but can't act
    let (status, page) = get(&harness, &senior, &senior_token, "/staff/safety/cases").await;
    assert_eq!(status, Status::Ok);
    assert!(page["cases"].as_array().unwrap().iter().any(|c| c["_id"] == case_id.as_str()));
    let (status, details) = get(&harness, &senior, &senior_token, &case_path).await;
    assert_eq!(status, Status::Ok, "{details}");
    assert!(!details["evidence"].as_array().unwrap().is_empty());
    let (status, body) = send(&harness, &senior, &senior_token, Post, &format!("{case_path}/notes"), json!({ "text": "looks bad" })).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["permission"], "platform.reports.resolve");

    // a staff session does not open the older /safety/admin routes
    let (status, _) = get(&harness, &ts, &ts_token, "/safety/admin/reports").await;
    assert_eq!(status, Status::Forbidden);

    // Trust & Safety triages
    let version = harness.db.fetch_safety_case(&case_id).await.unwrap().version;
    let (status, body) = send(&harness, &ts, &ts_token, Post, &format!("{case_path}/status"),
        json!({ "status": "UnderReview", "version": version })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["assignee_id"], ts.user.id.as_str());

    // suspending an account needs a step-up
    let version = harness.db.fetch_safety_case(&case_id).await.unwrap().version;
    let suspend = json!({ "kind": "SuspendAccount", "public_reason": "Repeated harassment", "duration_days": 3, "version": version });
    let (status, body) = send(&harness, &ts, &ts_token, Post, &format!("{case_path}/actions"), suspend.clone()).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["type"], "NotElevated");
    // signed in with the previous step, so this code is fresh
    let (status, _) = send(&harness, &ts, &ts_token, Post, "/staff/session/step-up", json!({ "totp_code": code(0) })).await;
    assert_eq!(status, Status::Ok);
    let (status, action) = send(&harness, &ts, &ts_token, Post, &format!("{case_path}/actions"), suspend).await;
    assert_eq!(status, Status::Ok, "{action}");
    assert!(harness.db.fetch_account(&author.id).await.unwrap().disabled);

    // and it can be reversed
    let action_id = action["_id"].as_str().unwrap().to_string();
    let (status, body) = send(&harness, &ts, &ts_token, Post, &format!("/staff/safety/actions/{action_id}/revoke"),
        json!({ "reason": "Wrong account" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(harness.db.fetch_user(&author.id).await.unwrap().suspended_until.is_none());

    // account history shows the action
    let (status, history) = send(&harness, &ts, &ts_token, Get, &format!("/staff/safety/users/{}", author.id), Value::Null).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(history["actions"].as_array().unwrap().len(), 1);

    // staff accounts can't be actioned through cases
    grant(&harness, &author.id, vec![StaffRole::Support]).await;
    let version = harness.db.fetch_safety_case(&case_id).await.unwrap().version;
    let (status, _) = send(&harness, &ts, &ts_token, Post, &format!("{case_path}/actions"),
        json!({ "kind": "Warning", "public_reason": "Be kind", "version": version })).await;
    assert_eq!(status, Status::Forbidden);

    let actions = audit_actions(&harness).await;
    for expected in [
        ("staff.safety.case.view", "Success"),
        ("staff.safety.case.status", "Success"),
        ("staff.safety.action.take", "Success"),
        ("staff.safety.action.take", "Denied"),
        ("staff.safety.action.revoke", "Success"),
    ] {
        assert!(
            actions.iter().any(|(a, o)| a == expected.0 && o == expected.1),
            "missing {expected:?} in {actions:?}"
        );
    }
    let (_, check) = get(&harness, &ts, &ts_token, "/staff/audit/verify").await;
    // Trust & Safety can't read the audit log; the owner can
    assert!(check["ok"].is_null());
}

#[rocket::async_test]
async fn discover_approval_follows_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    let res = harness
        .client
        .put(format!("/servers/{}/discover", server.id))
        .header(Header::new("x-session-token", owner_session.token.clone()))
        .header(ContentType::JSON)
        .body(r##"{"tags":["Gaming"],"category":"gaming","language":"ar"}"##)
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    drop(res);

    let support = member(&harness, vec![StaffRole::Support]).await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let ts_token = sign_in(&harness, &ts, 0).await;
    let path = format!("/staff/discover/requests/server/{}", server.id);

    // Support sees the queue but can't decide
    let (status, queue) = get(&harness, &support, &support_token, "/staff/discover/requests").await;
    assert_eq!(status, Status::Ok);
    assert!(queue.as_array().unwrap().iter().any(|e| e["request"]["id"] == server.id.as_str()));
    let (status, body) = send(&harness, &support, &support_token, Post, &path, json!({ "decision": "Approve" })).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["permission"], "platform.servers.restrict");

    // denying needs a reason the owner can read
    let (status, _) = send(&harness, &ts, &ts_token, Post, &path, json!({ "decision": "Deny" })).await;
    assert_eq!(status, Status::BadRequest);
    // unknown kinds and servers are refused
    let (status, _) = send(&harness, &ts, &ts_token, Post, &format!("/staff/discover/requests/user/{}", server.id), json!({ "decision": "Approve" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &ts, &ts_token, Post, "/staff/discover/requests/server/01ZZZZZZZZZZZZZZZZZZZZZZZZ", json!({ "decision": "Approve" })).await;
    assert_eq!(status, Status::NotFound);

    // Trust & Safety approves: the server is listed
    let (status, body) = send(&harness, &ts, &ts_token, Post, &path, json!({ "decision": "Approve", "reason": "Welcome" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(harness.db.fetch_server(&server.id).await.unwrap().discoverable);
    let (_, queue) = get(&harness, &ts, &ts_token, "/staff/discover/requests").await;
    assert!(!queue.as_array().unwrap().iter().any(|e| e["request"]["id"] == server.id.as_str()));

    // and can remove it again, with a reason
    let (status, _) = send(&harness, &ts, &ts_token, Post, &path, json!({ "decision": "Remove", "reason": "Breaks the rules" })).await;
    assert_eq!(status, Status::Ok);
    assert!(!harness.db.fetch_server(&server.id).await.unwrap().discoverable);

    let actions = audit_actions(&harness).await;
    assert!(actions.iter().filter(|(a, o)| a == "staff.discover.decide" && o == "Success").count() >= 2);
    assert!(actions.iter().any(|(a, o)| a == "staff.access.denied" && o == "Denied"));
}

#[rocket::async_test]
async fn accounts_can_be_actioned_from_the_user_page() {
    use rocket::http::Method::{Get, Post};
    let harness = TestHarness::new().await;
    let (_, _, target) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let ts_token = sign_in(&harness, &ts, -1).await;
    let path = format!("/staff/users/{}/actions", target.id);
    let warn = json!({ "kind": "Warning", "category": "Spam", "public_reason": "Sending the same link everywhere" });

    // Support may not act
    let (status, body) = send(&harness, &support, &support_token, Post, &path, warn.clone()).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["permission"], "platform.reports.resolve");

    // a warning needs no step-up; it opens a staff case
    let (status, action) = send(&harness, &ts, &ts_token, Post, &path, warn).await;
    assert_eq!(status, Status::Ok, "{action}");
    let case = harness.db.fetch_safety_case(action["case_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(case.reporter_id, ts.user.id);
    assert_eq!(case.target_owner_id.as_deref(), Some(target.id.as_str()));

    // suspension: step-up first, then the account is disabled for the days given
    let suspend = json!({ "kind": "SuspendAccount", "category": "Spam", "public_reason": "Spam after a warning", "duration_days": 2 });
    let (status, body) = send(&harness, &ts, &ts_token, Post, &path, suspend.clone()).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["type"], "NotElevated");
    let (status, _) = send(&harness, &ts, &ts_token, Post, "/staff/session/step-up", json!({ "totp_code": code(0) })).await;
    assert_eq!(status, Status::Ok);
    let (status, action) = send(&harness, &ts, &ts_token, Post, &path, suspend).await;
    assert_eq!(status, Status::Ok, "{action}");
    assert!(harness.db.fetch_account(&target.id).await.unwrap().disabled);
    let (status, _) = send(&harness, &ts, &ts_token, Post, &format!("/staff/safety/actions/{}/revoke", action["_id"].as_str().unwrap()), json!({ "reason": "Lifted after talking" })).await;
    assert_eq!(status, Status::Ok);
    assert!(!harness.db.fetch_account(&target.id).await.unwrap().disabled);

    // both show in the account's history
    let (_, history) = send(&harness, &ts, &ts_token, Get, &format!("/staff/safety/users/{}", target.id), Value::Null).await;
    assert_eq!(history["actions"].as_array().unwrap().len(), 2);

    // staff accounts, bots and yourself are protected, and nothing is opened for them
    let before = harness.db.fetch_safety_actions_for_user(&support.user.id).await.unwrap().len();
    let (status, _) = send(&harness, &ts, &ts_token, Post, &format!("/staff/users/{}/actions", support.user.id),
        json!({ "kind": "Warning", "category": "Spam", "public_reason": "test test" })).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(harness.db.fetch_safety_actions_for_user(&support.user.id).await.unwrap().len(), before);
    let (status, _) = send(&harness, &ts, &ts_token, Post, &format!("/staff/users/{}/actions", ts.user.id),
        json!({ "kind": "Warning", "category": "Spam", "public_reason": "test test" })).await;
    assert_eq!(status, Status::BadRequest);
    // only account actions here
    let (status, _) = send(&harness, &ts, &ts_token, Post, &path,
        json!({ "kind": "RemoveMessage", "category": "Spam", "public_reason": "test test" })).await;
    assert_eq!(status, Status::BadRequest);

    let actions = audit_actions(&harness).await;
    assert!(actions.iter().any(|(a, o)| a == "staff.user.action" && o == "Success"));
    assert!(actions.iter().any(|(a, o)| a == "staff.user.action" && o == "Denied"));
}

#[rocket::async_test]
async fn the_reporter_gets_an_answer() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let (_, _, author) = harness.new_user().await;
    let (_, _, message) = harness.new_message(&author, &server, channels).await;
    let (_, reporter_session, reporter) = harness.new_user().await;
    revolt_database::Member::create(&harness.db, &server, &reporter, None).await.unwrap();
    let res = harness
        .client
        .post("/safety/reports")
        .header(Header::new("x-session-token", reporter_session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "target": { "kind": "Message", "id": message.id }, "category": "Spam" }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    let receipt: Value = res.into_json().await.unwrap();
    let case_id = receipt["_id"].as_str().unwrap().to_string();

    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let token = sign_in(&harness, &ts, 0).await;
    let version = harness.db.fetch_safety_case(&case_id).await.unwrap().version;
    let (status, body) = send(&harness, &ts, &token, Post, &format!("/staff/safety/cases/{case_id}/actions"),
        json!({ "kind": "RemoveMessage", "public_reason": "Spam", "reporter_note": "Thanks, we removed the message.", "version": version })).await;
    assert_eq!(status, Status::Ok, "{body}");

    let res = harness
        .client
        .get(format!("/safety/reports/{case_id}"))
        .header(Header::new("x-session-token", reporter_session.token.clone()))
        .dispatch()
        .await;
    let mine: Value = res.into_json().await.unwrap();
    assert_eq!(mine["public_note"], "Thanks, we removed the message.");
    assert_eq!(mine["status"], "ActionTaken");
}

#[rocket::async_test]
async fn badges_follow_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, _, target) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let senior = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let senior_token = sign_in(&harness, &senior, 0).await;
    let owner_token = sign_in(&harness, &owner, -1).await;
    let grant = format!("/staff/users/{}/badges/grant", target.id);
    let revoke = format!("/staff/users/{}/badges/revoke", target.id);

    // everyone with users.read sees the record; Support can't give badges
    let (status, record) = get(&harness, &support, &support_token, &format!("/staff/users/{}/badges", target.id)).await;
    assert_eq!(status, Status::Ok, "{record}");
    assert!(record["eligibility"].as_array().unwrap().iter().all(|e| e["allowed"] == false));
    let (status, body) = send(&harness, &support, &support_token, Post, &grant, json!({ "badge": "partner", "reason": "Official partner" })).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["permission"], "platform.badges.manage");

    // Senior Support gives and takes a staff-rule badge
    let (status, body) = send(&harness, &senior, &senior_token, Post, &grant, json!({ "badge": "partner", "reason": "Official partner" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["held"], true);
    let (status, body) = send(&harness, &senior, &senior_token, Post, &revoke, json!({ "badge": "partner", "reason": "Partnership ended" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["held"], false);
    // but not the Staff badge, and never automatic ones
    let (status, _) = send(&harness, &senior, &senior_token, Post, &grant, json!({ "badge": "staff", "reason": "try try" })).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = send(&harness, &senior, &senior_token, Post, &grant, json!({ "badge": "plus", "reason": "try try" })).await;
    assert_eq!(status, Status::BadRequest);
    // nor to themselves
    let (status, _) = send(&harness, &senior, &senior_token, Post, &format!("/staff/users/{}/badges/grant", senior.user.id),
        json!({ "badge": "partner", "reason": "self self" })).await;
    assert_eq!(status, Status::Forbidden);

    // the Staff badge: Owner only, after a step-up
    let (status, body) = send(&harness, &owner, &owner_token, Post, &grant, json!({ "badge": "staff", "reason": "Joined the team" })).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["type"], "NotElevated");
    let (status, _) = send(&harness, &owner, &owner_token, Post, "/staff/session/step-up", json!({ "totp_code": code(0) })).await;
    assert_eq!(status, Status::Ok);
    let (status, body) = send(&harness, &owner, &owner_token, Post, &grant, json!({ "badge": "staff", "reason": "Joined the team" })).await;
    assert_eq!(status, Status::Ok, "{body}");

    let actions = audit_actions(&harness).await;
    assert!(actions.iter().filter(|(a, o)| a == "staff.badge.grant" && o == "Success").count() >= 2);
    assert!(actions.iter().any(|(a, o)| a == "staff.badge.revoke" && o == "Success"));
    assert!(actions.iter().any(|(a, o)| a == "staff.badge.grant" && o == "Denied"));
}

#[rocket::async_test]
async fn bots_and_verification_follow_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, owner_session, bot_owner) = harness.new_user().await;
    let (bot, _) = revolt_database::Bot::create(&harness.db, TestHarness::rand_string(), &bot_owner, None)
        .await
        .unwrap();
    let as_owner = |method: rocket::http::Method, path: String, body: Option<Value>| {
        let mut req = harness
            .client
            .req(method, path)
            .header(Header::new("x-session-token", owner_session.token.clone()));
        if let Some(body) = body {
            req = req.header(ContentType::JSON).body(body.to_string());
        }
        req
    };

    // the owner applies; a second request while one is open is refused
    let res = as_owner(Post, format!("/bots/{}/verification", bot.id),
        Some(json!({ "statement": "Moderation helper used in 40 Arabic communities.", "website": "https://example.org", "privacy_policy_url": "https://example.org/privacy", "terms_of_service_url": "https://example.org/terms" })))
        .dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    let state: Value = res.into_json().await.unwrap();
    assert_eq!(state["request"]["status"], "Pending");
    let request_id = state["request"]["_id"].as_str().unwrap().to_string();
    let res = as_owner(Post, format!("/bots/{}/verification", bot.id),
        Some(json!({ "statement": "Moderation helper used in 40 Arabic communities.", "privacy_policy_url": "https://example.org/privacy", "terms_of_service_url": "https://example.org/terms" }))).dispatch().await;
    assert_eq!(res.status(), Status::BadRequest);
    drop(res);
    // links must be https
    let (_, _, other) = harness.new_user().await;
    let _ = other;

    let support = member(&harness, vec![StaffRole::Support]).await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let ts_token = sign_in(&harness, &ts, 0).await;

    // Support finds the bot and sees the queue, but can't decide or disable
    let (status, list) = get(&harness, &support, &support_token, &format!("/staff/bots?q={}", bot_owner.id)).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list[0]["id"], bot.id.as_str());
    let (status, queue) = get(&harness, &support, &support_token, "/staff/bot-verifications").await;
    assert_eq!(status, Status::Ok);
    assert!(queue.as_array().unwrap().iter().any(|e| e["id"] == request_id.as_str()));
    let decide = format!("/staff/bot-verifications/{request_id}");
    let (status, _) = send(&harness, &support, &support_token, Post, &decide, json!({ "approve": true })).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = send(&harness, &support, &support_token, Post, &format!("/staff/bots/{}/disable", bot.id),
        json!({ "category": "BotAbuse", "public_reason": "Spamming servers" })).await;
    assert_eq!(status, Status::Forbidden);

    // denying needs a note; approving sets the verified flag
    let (status, _) = send(&harness, &ts, &ts_token, Post, &decide, json!({ "approve": false })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &ts, &ts_token, Post, &decide, json!({ "approve": true, "note": "Welcome" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(harness.db.fetch_bot(&bot.id).await.unwrap().flags.unwrap_or_default() & 1 != 0);
    // decided once only
    let (status, _) = send(&harness, &ts, &ts_token, Post, &decide, json!({ "approve": false, "note": "again" })).await;
    assert_eq!(status, Status::BadRequest);
    // the owner sees the decision without the reviewer's identity
    let res = as_owner(rocket::http::Method::Get, format!("/bots/{}/verification", bot.id), None).dispatch().await;
    let state: Value = res.into_json().await.unwrap();
    assert_eq!(state["verified"], true);
    assert_eq!(state["request"]["status"], "Approved");
    assert!(state["request"].get("decided_by").is_none());

    // the mark can be taken away
    let (status, _) = send(&harness, &ts, &ts_token, Post, &format!("/staff/bots/{}/unverify", bot.id), json!({ "reason": "Changed what it does" })).await;
    assert_eq!(status, Status::Ok);
    assert!(harness.db.fetch_bot(&bot.id).await.unwrap().flags.unwrap_or_default() & 1 == 0);

    // disabling opens a case against the owner's standing and suspends the bot
    let (status, action) = send(&harness, &ts, &ts_token, Post, &format!("/staff/bots/{}/disable", bot.id),
        json!({ "category": "BotAbuse", "public_reason": "Spamming servers" })).await;
    assert_eq!(status, Status::Ok, "{action}");
    assert_eq!(action["subject_user_id"], bot_owner.id.as_str());
    assert!(harness.db.fetch_bot(&bot.id).await.unwrap().flags.unwrap_or_default() & 4 != 0);
    let (_, detail) = get(&harness, &ts, &ts_token, &format!("/staff/bots/{}", bot.id)).await;
    assert_eq!(detail["suspended"], true);
    assert_eq!(detail["verifications"][0]["status"], "Revoked");

    let actions = audit_actions(&harness).await;
    for expected in ["staff.bot.verification", "staff.bot.unverify", "staff.bot.disable"] {
        assert!(actions.iter().any(|(a, o)| a == expected && o == "Success"), "{expected}");
    }
}

#[rocket::async_test]
async fn approving_a_request_approves_the_intents_it_asked_for() {
    use revolt_database::{enabled_intents, Member as ServerMember, MemberCompositeKey, PartialBot};
    use revolt_models::v0::GatewayIntents;
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (bot, bot_user) = harness.new_bot(&owner).await;
    harness
        .db
        .update_bot(
            &bot.id,
            &PartialBot {
                privileged_intents: Some(
                    (GatewayIntents::MESSAGE_CONTENT | GatewayIntents::SERVER_MEMBERS) as i64,
                ),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    // past 100 servers only approved privileged intents stay on
    for _ in 0..101 {
        harness
            .db
            .insert_or_merge_member(&ServerMember {
                id: MemberCompositeKey {
                    server: ulid::Ulid::new().to_string(),
                    user: bot.id.clone(),
                },
                ..Default::default()
            })
            .await
            .unwrap();
    }
    assert_eq!(enabled_intents(&harness.db, &bot_user).await & GatewayIntents::PRIVILEGED, 0);

    let apply = |body: Value| {
        harness
            .client
            .post(format!("/bots/{}/verification", bot.id))
            .header(Header::new("x-session-token", owner_session.token.clone()))
            .header(ContentType::JSON)
            .body(body.to_string())
    };
    let statement = "It moderates links in messages, so it has to read them.";
    // only privileged intents can be asked for
    let res = apply(json!({ "statement": statement, "intents": GatewayIntents::SERVERS })).dispatch().await;
    assert_eq!(res.status(), Status::BadRequest);
    drop(res);
    let res = apply(json!({ "statement": statement, "privacy_policy_url": "https://example.org/privacy", "terms_of_service_url": "https://example.org/terms", "intents": GatewayIntents::MESSAGE_CONTENT })).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    let state: Value = res.into_json().await.unwrap();
    assert_eq!(state["request"]["intents"], GatewayIntents::MESSAGE_CONTENT);
    let request_id = state["request"]["_id"].as_str().unwrap().to_string();

    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let token = sign_in(&harness, &ts, 0).await;
    let (status, body) = send(&harness, &ts, &token, Post, &format!("/staff/bot-verifications/{request_id}"),
        json!({ "approve": true })).await;
    assert_eq!(status, Status::Ok, "{body}");

    // only the intent that was asked for comes back, and the profile shows the mark
    let intents = enabled_intents(&harness.db, &bot_user).await;
    assert_ne!(intents & GatewayIntents::MESSAGE_CONTENT, 0);
    assert_eq!(intents & GatewayIntents::SERVER_MEMBERS, 0);
    assert!(harness.db.fetch_user(&bot.id).await.unwrap().bot.unwrap().verified);

    // taking the mark away clears it on the profile too
    let (status, _) = send(&harness, &ts, &token, Post, &format!("/staff/bots/{}/unverify", bot.id),
        json!({ "reason": "Changed what it does" })).await;
    assert_eq!(status, Status::Ok);
    assert!(!harness.db.fetch_user(&bot.id).await.unwrap().bot.unwrap().verified);
}

#[rocket::async_test]
async fn profile_changes_wait_for_the_users_consent() {
    use rocket::http::Method::{Get, Post};
    let harness = TestHarness::new().await;
    let (_, user_session, target) = harness.new_user().await;
    let (_, other_session, _) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let analyst = member(&harness, vec![StaffRole::Analyst]).await;
    let token = sign_in(&harness, &support, 0).await;
    let analyst_token = sign_in(&harness, &analyst, 0).await;
    let path = format!("/staff/users/{}/change-requests", target.id);
    let as_user = |session: &revolt_database::Session, method: rocket::http::Method, path: String, body: Option<Value>| {
        let mut req = harness
            .client
            .req(method, path)
            .header(Header::new("x-session-token", session.token.clone()));
        if let Some(body) = body {
            req = req.header(ContentType::JSON).body(body.to_string());
        }
        req
    };
    let rename = json!({ "changes": { "display_name": "Ahmad Ali", "remove": ["Avatar"] }, "reason": "You asked support to fix your name" });

    // only support.manage may ask, and only for allowed, valid changes
    let (status, _) = send(&harness, &analyst, &analyst_token, Post, &path, rename.clone()).await;
    assert_eq!(status, Status::Forbidden);
    for bad in [
        json!({ "changes": { "remove": ["Badges"] }, "reason": "try try" }),
        json!({ "changes": {}, "reason": "nothing here" }),
        json!({ "changes": { "display_name": "a" }, "reason": "too short name" }),
    ] {
        let (status, _) = send(&harness, &support, &token, Post, &path, bad).await;
        assert_eq!(status, Status::BadRequest);
    }
    let (status, created) = send(&harness, &support, &token, Post, &path, rename).await;
    assert_eq!(status, Status::Ok, "{created}");
    let request_id = created["_id"].as_str().unwrap().to_string();

    // nothing changed yet; the user sees the request, without who asked
    assert!(harness.db.fetch_user(&target.id).await.unwrap().display_name.is_none());
    let res = as_user(&user_session, Get, "/users/@me/change-requests".to_string(), None).dispatch().await;
    let mine: Value = res.into_json().await.unwrap();
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert!(mine[0].get("requested_by").is_none());
    assert_eq!(mine[0]["reason"], "You asked support to fix your name");

    // someone else can't answer it
    let answer = format!("/users/@me/change-requests/{request_id}");
    let res = as_user(&other_session, Post, answer.clone(), Some(json!({ "accept": true }))).dispatch().await;
    assert_eq!(res.status(), Status::NotFound);
    drop(res);

    // accepting applies it, once
    let res = as_user(&user_session, Post, answer.clone(), Some(json!({ "accept": true }))).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    drop(res);
    assert_eq!(harness.db.fetch_user(&target.id).await.unwrap().display_name.as_deref(), Some("Ahmad Ali"));
    let res = as_user(&user_session, Post, answer, Some(json!({ "accept": true }))).dispatch().await;
    assert_eq!(res.status(), Status::BadRequest);
    drop(res);

    // declining changes nothing
    let (_, second) = send(&harness, &support, &token, Post, &path,
        json!({ "changes": { "display_name": "Someone Else" }, "reason": "Second try at the name" })).await;
    let res = as_user(&user_session, Post, format!("/users/@me/change-requests/{}", second["_id"].as_str().unwrap()), Some(json!({ "accept": false }))).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    drop(res);
    assert_eq!(harness.db.fetch_user(&target.id).await.unwrap().display_name.as_deref(), Some("Ahmad Ali"));

    // a withdrawn request can't be accepted
    let (_, third) = send(&harness, &support, &token, Post, &path,
        json!({ "changes": { "pronouns": "he/him" }, "reason": "Pronouns as asked" })).await;
    let third_id = third["_id"].as_str().unwrap().to_string();
    let (status, _) = send(&harness, &support, &token, Post, &format!("/staff/change-requests/{third_id}/cancel"), Value::Null).await;
    assert_eq!(status, Status::Ok);
    let res = as_user(&user_session, Post, format!("/users/@me/change-requests/{third_id}"), Some(json!({ "accept": true }))).dispatch().await;
    assert_eq!(res.status(), Status::BadRequest);
    drop(res);

    // staff see the whole history, with who asked
    let (_, history) = get(&harness, &support, &token, &path).await;
    let statuses: Vec<&str> = history.as_array().unwrap().iter().map(|r| r["status"].as_str().unwrap()).collect();
    assert_eq!(statuses, vec!["Cancelled", "Declined", "Accepted"]);
    assert_eq!(history[0]["requested_by"], support.user.id.as_str());
    let actions = audit_actions(&harness).await;
    assert!(actions.iter().filter(|(a, o)| a == "staff.user.change_request" && o == "Success").count() == 3);
    assert!(actions.iter().any(|(a, _)| a == "staff.user.change_request.cancel"));
}

#[rocket::async_test]
async fn email_and_two_factor_changes_need_the_emailed_link() {
    use rocket::http::Method::{Get, Post};
    let harness = TestHarness::new().await;
    let (mut account, user_session, target) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let token = sign_in(&harness, &support, -1).await;
    let path = format!("/staff/users/{}/security-requests", target.id);
    let reset = json!({ "kind": "two_factor", "reason": "Lost their phone, checked by support" });

    // sensitive: needs a step-up first
    let (status, body) = send(&harness, &support, &token, Post, &path, reset.clone()).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["type"], "NotElevated");
    let (status, _) = send(&harness, &support, &token, Post, "/staff/session/step-up", json!({ "totp_code": code(0) })).await;
    assert_eq!(status, Status::Ok);
    // nothing to reset while two-factor is off
    let (status, _) = send(&harness, &support, &token, Post, &path, reset.clone()).await;
    assert_eq!(status, Status::BadRequest);
    enable_totp(&harness, &mut account).await;
    // the link goes to the account's address (maildev); the request waits
    // for the user and nothing changes before they open it
    let (status, body) = send(&harness, &support, &token, Post, &path, reset).await;
    assert_eq!(status, Status::Ok, "{body}");
    let requests = harness.db.fetch_account_change_requests_for_user(&target.id).await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].status, revolt_database::AccountChangeStatus::Pending);
    harness.assert_email(&account.email).await;
    assert!(audit_actions(&harness).await.iter().any(|(a, o)| a == "staff.user.security_request" && o == "Success"));
    assert!(harness.db.fetch_account(&account.id).await.unwrap().mfa.is_active());

    // the link flow, with a request as the console would create it
    let link = |raw: &str| format!("/auth/account/change-request/{raw}");
    let make = |raw: &str, changes: revolt_database::AccountChanges| revolt_database::AccountChangeRequest {
        id: ulid::Ulid::new().to_string(),
        user_id: target.id.clone(),
        requested_by: support.user.id.clone(),
        reason: "Lost their phone".to_string(),
        changes,
        status: revolt_database::AccountChangeStatus::Pending,
        created_at: staff_now(),
        expires_at: staff_now() + revolt_database::ACCOUNT_SECURITY_CHANGE_TTL_MS,
        decided_at: None,
        token_hash: Some(revolt_database::staff_token_hash(raw)),
    };
    let raw = "t".repeat(48);
    harness
        .db
        .insert_account_change_request(&make(&raw, revolt_database::AccountChanges { reset_totp: true, ..Default::default() }))
        .await
        .unwrap();

    // it is not answerable in the app, only by the link
    let res = harness.client.get("/users/@me/change-requests").header(Header::new("x-session-token", user_session.token.clone())).dispatch().await;
    let mine: Value = res.into_json().await.unwrap();
    assert!(mine.as_array().unwrap().is_empty());
    let res = harness.client.req(Get, link(&raw)).dispatch().await;
    let view: Value = res.into_json().await.unwrap();
    assert_eq!(view["kind"], "two_factor");
    assert_eq!(view["status"], "Pending");
    let res = harness.client.req(Get, link(&"x".repeat(48))).dispatch().await;
    assert_eq!(res.status(), Status::NotFound);
    drop(res);

    // confirming turns two-factor off and signs out everywhere, once
    let res = harness.client.req(Post, link(&raw)).header(ContentType::JSON).body(json!({ "accept": true }).to_string()).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    drop(res);
    assert!(!harness.db.fetch_account(&target.id).await.unwrap().mfa.is_active());
    let res = harness.client.get("/users/@me").header(Header::new("x-session-token", user_session.token.clone())).dispatch().await;
    assert_eq!(res.status(), Status::Unauthorized);
    drop(res);
    let res = harness.client.req(Post, link(&raw)).header(ContentType::JSON).body(json!({ "accept": true }).to_string()).dispatch().await;
    assert_eq!(res.status(), Status::BadRequest);
    drop(res);

    // an email change: declining changes nothing
    let raw2 = "e".repeat(48);
    harness
        .db
        .insert_account_change_request(&make(&raw2, revolt_database::AccountChanges { email: Some("new-address@zrzur.test".to_string()), ..Default::default() }))
        .await
        .unwrap();
    let res = harness.client.req(Post, link(&raw2)).header(ContentType::JSON).body(json!({ "accept": false }).to_string()).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    drop(res);
    assert_ne!(harness.db.fetch_account(&target.id).await.unwrap().email, "new-address@zrzur.test");
    let raw3 = "f".repeat(48);
    harness
        .db
        .insert_account_change_request(&make(&raw3, revolt_database::AccountChanges { email: Some("new-address@zrzur.test".to_string()), ..Default::default() }))
        .await
        .unwrap();
    // confirming sends a verification link to the new address; the email
    // changes only once that link is opened
    let res = harness.client.req(Post, link(&raw3)).header(ContentType::JSON).body(json!({ "accept": true }).to_string()).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    drop(res);
    harness.assert_email("new-address@zrzur.test").await;
    assert_ne!(harness.db.fetch_account(&target.id).await.unwrap().email, "new-address@zrzur.test");
    let res = harness.client.req(Get, link(&raw3)).dispatch().await;
    let view: Value = res.into_json().await.unwrap();
    assert_eq!(view["status"], "Accepted");

    // Support can't read personal data: the new address is masked for them
    let (_, list) = get(&harness, &support, &token, &format!("/staff/users/{}/change-requests", target.id)).await;
    let emails: Vec<&Value> = list.as_array().unwrap().iter().filter_map(|r| r.get("email")).collect();
    assert!(!emails.is_empty());
    assert!(emails.iter().all(|e| *e == "•••"));
}

#[rocket::async_test]
async fn plugins_and_designs_are_visible_read_only() {
    let harness = TestHarness::new().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let analyst = member(&harness, vec![StaffRole::Analyst]).await;
    let token = sign_in(&harness, &support, 0).await;
    let analyst_token = sign_in(&harness, &analyst, 0).await;

    for path in ["/staff/plugins", "/staff/designs", "/staff/plugins?status=Listed", "/staff/designs?status=Published&kind=Nameplate"] {
        let (status, body) = get(&harness, &support, &token, path).await;
        assert_eq!(status, Status::Ok, "{path} {body}");
        assert!(body.is_array());
        // roles without users.read see nothing
        let (status, _) = get(&harness, &analyst, &analyst_token, path).await;
        assert_eq!(status, Status::Forbidden, "{path}");
    }
    for path in ["/staff/plugins?status=Nope", "/staff/designs?kind=Banner", &format!("/staff/designs?q={}", "a".repeat(65))] {
        let (status, _) = get(&harness, &support, &token, path).await;
        assert_eq!(status, Status::BadRequest, "{path}");
    }
    let (status, _) = get(&harness, &support, &token, "/staff/plugins/missing.plugin").await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn creators_follow_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, _, target) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let ts_token = sign_in(&harness, &ts, 0).await;
    let path = format!("/staff/creators/{}", target.id);
    let can_publish = |harness: &TestHarness| {
        let id = target.id.clone();
        let db = harness.db.clone();
        async move {
            let user = db.fetch_user(&id).await.unwrap();
            revolt_database::platform_permissions(&db, &user)
                .await
                .unwrap()
                .has(revolt_database::PlatformPermission::CosmeticsSubmit)
        }
    };

    // Support sees the list but can't change it
    let (status, _) = get(&harness, &support, &support_token, "/staff/creators").await;
    assert_eq!(status, Status::Ok);
    let (status, body) = send(&harness, &support, &support_token, Post, &path, json!({ "action": "grant", "reason": "Great artist" })).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["permission"], "platform.creators.manage");

    // Trust & Safety grants: the person can publish designs now
    assert!(!can_publish(&harness).await);
    let (status, record) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "grant", "reason": "Great artist" })).await;
    assert_eq!(status, Status::Ok, "{record}");
    assert_eq!(record["creator"], "Active");
    assert!(can_publish(&harness).await);
    let (_, list) = get(&harness, &ts, &ts_token, "/staff/creators").await;
    assert!(list.as_array().unwrap().iter().any(|r| r["user"]["_id"] == target.id.as_str() || r["user"]["id"] == target.id.as_str()));

    // suspend, revoke, reinstate
    let (_, record) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "suspend", "reason": "Copied artwork" })).await;
    assert_eq!(record["creator"], "Suspended");
    let (_, record) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "revoke", "reason": "Copied again" })).await;
    assert_eq!(record["creator"], "Revoked");
    assert!(!can_publish(&harness).await);
    let (_, record) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "reinstate", "reason": "Appeal accepted" })).await;
    assert_eq!(record["creator"], "Active");
    let (_, history) = get(&harness, &ts, &ts_token, &format!("/staff/creators/{}/history", target.id)).await;
    assert_eq!(history.as_array().unwrap().len(), 4);

    // not for yourself, no unknown actions, a reason is needed
    let (status, _) = send(&harness, &ts, &ts_token, Post, &format!("/staff/creators/{}", ts.user.id), json!({ "action": "grant", "reason": "myself" })).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "promote", "reason": "test test" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "grant", "reason": "" })).await;
    assert_eq!(status, Status::BadRequest);

    let actions = audit_actions(&harness).await;
    assert!(actions.iter().filter(|(a, o)| a == "staff.creator.change" && o == "Success").count() >= 4);
}

#[rocket::async_test]
async fn billing_follows_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, _, target) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let billing = member(&harness, vec![StaffRole::Billing]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let token = sign_in(&harness, &billing, -1).await;

    // Billing reads; Support sees nothing
    for path in ["/staff/billing/summary", "/staff/billing/orders", "/staff/billing/webhooks", "/staff/billing/issues", "/staff/billing/audit"] {
        let (status, body) = get(&harness, &billing, &token, path).await;
        assert_eq!(status, Status::Ok, "{path} {body}");
        let (status, _) = get(&harness, &support, &support_token, path).await;
        assert_eq!(status, Status::Forbidden, "{path}");
    }
    let (status, _) = get(&harness, &billing, &token, "/staff/billing/orders?status=Nope").await;
    assert_eq!(status, Status::BadRequest);

    // changes need a step-up
    let grant = json!({ "user": target.id, "days": 7, "reason": "Thanks for the bug reports" });
    let (status, body) = send(&harness, &billing, &token, Post, "/staff/billing/grants", grant.clone()).await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(body["type"], "NotElevated");
    let (status, _) = send(&harness, &billing, &token, Post, "/staff/session/step-up", json!({ "totp_code": code(0) })).await;
    assert_eq!(status, Status::Ok);
    // billing is switched off in the test config: refused, and the attempt
    // is followed by a "failed" audit record
    let (status, body) = send(&harness, &billing, &token, Post, "/staff/billing/grants", grant).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert_eq!(body["type"], "FeatureDisabled");
    // never to yourself
    let (status, _) = send(&harness, &billing, &token, Post, "/staff/billing/grants",
        json!({ "user": billing.user.id, "days": 7, "reason": "for me please" })).await;
    assert_eq!(status, Status::Forbidden);
    // unknown order
    let (status, _) = send(&harness, &billing, &token, Post, "/staff/billing/orders/nope/refund",
        json!({ "reason": "test refund", "revoke": false })).await;
    assert_eq!(status, Status::NotFound);

    let actions = audit_actions(&harness).await;
    assert!(actions.iter().any(|(a, o)| a == "staff.billing.grant" && o == "Success"));
    assert!(actions.iter().any(|(a, o)| a == "staff.billing.grant" && o == "Failed"));
}

#[rocket::async_test]
async fn beta_program_follows_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, _, target) = harness.new_user().await;
    let ops = member(&harness, vec![StaffRole::Operations]).await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let ops_token = sign_in(&harness, &ops, 0).await;
    let token = sign_in(&harness, &owner, -1).await;
    let feature = json!({ "id": "new-composer", "name": "New composer", "description": "Try the new message box", "enabled": true, "all_testers": true });

    // Operations reads, but can't change
    let (status, _) = get(&harness, &ops, &ops_token, "/staff/beta/features").await;
    assert_eq!(status, Status::Ok);
    let (status, _) = send(&harness, &ops, &ops_token, Post, "/staff/beta/features", feature.clone()).await;
    assert_eq!(status, Status::Forbidden);

    // Owner, after a step-up
    let (status, body) = send(&harness, &owner, &token, Post, "/staff/beta/features", feature.clone()).await;
    assert_eq!(body["type"], "NotElevated", "{status}");
    let (status, _) = send(&harness, &owner, &token, Post, "/staff/session/step-up", json!({ "totp_code": code(0) })).await;
    assert_eq!(status, Status::Ok);
    let (status, body) = send(&harness, &owner, &token, Post, "/staff/beta/features", feature).await;
    assert_eq!(status, Status::Ok, "{body}");
    let (status, body) = send(&harness, &owner, &token, Post, "/staff/beta/members", json!({ "user": target.id, "action": "invite" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Invited");
    let (_, members) = get(&harness, &ops, &ops_token, "/staff/beta/members?status=Invited").await;
    assert_eq!(members.as_array().unwrap().len(), 1);
    // unknown actions and features are refused
    let (status, _) = send(&harness, &owner, &token, Post, "/staff/beta/members", json!({ "user": target.id, "action": "promote" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &owner, &token, Post, "/staff/beta/members", json!({ "user": target.id, "action": "features", "features": ["nope"] })).await;
    assert_eq!(status, Status::BadRequest);

    let actions = audit_actions(&harness).await;
    assert!(actions.iter().any(|(a, o)| a == "staff.beta.feature" && o == "Success"));
    assert!(actions.iter().any(|(a, o)| a == "staff.beta.member" && o == "Success"));
}

/// A request as an ordinary signed-in user (no staff session)
async fn as_user(harness: &TestHarness, session: &Session, method: rocket::http::Method, path: &str, body: Value) -> (Status, Value) {
    let res = harness
        .client
        .req(method, path.to_string())
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = res.status();
    (status, res.into_json().await.unwrap_or(Value::Null))
}

/// Step up with the next code (sign-in codes can't be used twice)
async fn step_up(harness: &TestHarness, m: &Member, token: &str) {
    let (status, body) = send(harness, m, token, rocket::http::Method::Post, "/staff/session/step-up", json!({ "totp_code": code(1) })).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn plugin_review_follows_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let token = sign_in(&harness, &ts, -1).await;

    // Trust & Safety sees the queue; Support doesn't
    let (status, body) = get(&harness, &ts, &token, "/staff/plugins/review").await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body.as_array().unwrap().is_empty());
    let (status, _) = get(&harness, &support, &support_token, "/staff/plugins/review").await;
    assert_eq!(status, Status::Forbidden);

    // decisions need a step-up, a known action, a reason and a real target
    let (_, body) = send(&harness, &ts, &token, Post, "/staff/plugins/review/nope", json!({ "action": "reject", "reason": "unsafe code" })).await;
    assert_eq!(body["type"], "NotElevated");
    step_up(&harness, &ts, &token).await;
    let (status, _) = send(&harness, &ts, &token, Post, "/staff/plugins/review/nope", json!({ "action": "publish" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &ts, &token, Post, "/staff/plugins/review/nope", json!({ "action": "reject" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &ts, &token, Post, "/staff/plugins/review/nope", json!({ "action": "reject", "reason": "unsafe code" })).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = send(&harness, &support, &support_token, Post, "/staff/plugins/review/nope", json!({ "action": "approve" })).await;
    assert_eq!(status, Status::Forbidden);
    // publisher actions work on a real user
    let (status, body) = send(&harness, &ts, &token, Post, &format!("/staff/plugins/review/{}", support.user.id), json!({ "action": "verify" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(audit_actions(&harness).await.iter().any(|(a, o)| a == "staff.plugin.review" && o == "Success"));
}

#[rocket::async_test]
async fn bug_reports_follow_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, reporter_session, _) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let security = member(&harness, vec![StaffRole::Security]).await;
    let analyst = member(&harness, vec![StaffRole::Analyst]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let security_token = sign_in(&harness, &security, -1).await;
    let analyst_token = sign_in(&harness, &analyst, 1).await;

    let report = |kind: &str| json!({ "kind": kind, "title": "Send button stuck", "description": "The send button stays grey after typing", "platform": "web" });
    let (status, bug) = as_user(&harness, &reporter_session, Post, "/bugs", report("bug")).await;
    assert_eq!(status, Status::Ok, "{bug}");
    let (_, secret) = as_user(&harness, &reporter_session, Post, "/bugs", report("security")).await;

    // Support sees bugs, not security reports; Security sees those
    let (status, list) = get(&harness, &support, &support_token, "/staff/bugs").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, _) = get(&harness, &support, &support_token, "/staff/bugs?security=true").await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = get(&harness, &support, &support_token, &format!("/staff/bugs/{}", secret["_id"].as_str().unwrap())).await;
    assert_eq!(status, Status::NotFound);
    let (status, list) = get(&harness, &security, &security_token, "/staff/bugs?security=true").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, _) = get(&harness, &analyst, &analyst_token, "/staff/bugs").await;
    assert_eq!(status, Status::Forbidden);

    // Support triages the bug; the reporter's view follows
    let id = bug["_id"].as_str().unwrap();
    let review = json!({ "action": "confirm", "severity": "medium", "note": "Reproduced", "updated_at": bug["updated_at"] });
    let (status, body) = send(&harness, &support, &support_token, Post, &format!("/staff/bugs/{id}/review"), review.clone()).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["report"]["status"], "Confirmed");
    // the same stale update is refused
    let (status, _) = send(&harness, &support, &support_token, Post, &format!("/staff/bugs/{id}/review"), review).await;
    assert_ne!(status, Status::Ok);
    // Support can't act on a security report
    let secret_id = secret["_id"].as_str().unwrap();
    let (status, _) = send(&harness, &support, &support_token, Post, &format!("/staff/bugs/{secret_id}/review"),
        json!({ "action": "triage", "updated_at": secret["updated_at"] })).await;
    assert_eq!(status, Status::NotFound);
    let (status, body) = send(&harness, &security, &security_token, Post, &format!("/staff/bugs/{secret_id}/review"),
        json!({ "action": "triage", "updated_at": secret["updated_at"] })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(audit_actions(&harness).await.iter().any(|(a, o)| a == "staff.bug.review" && o == "Success"));
}

#[rocket::async_test]
async fn policy_updates_reach_users() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let ops = member(&harness, vec![StaffRole::Operations]).await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let ops_token = sign_in(&harness, &ops, 0).await;
    let token = sign_in(&harness, &owner, -1).await;
    let change = json!({
        "messages": { "ar": "حدّثنا شروط الاستخدام", "en": "We updated the terms of use", "de": "Wir haben die Nutzungsbedingungen aktualisiert" },
        "url": "https://zrzur.com/terms",
        "effective_in_days": 14
    });

    let (status, _) = send(&harness, &ops, &ops_token, Post, "/staff/policies", change.clone()).await;
    assert_eq!(status, Status::Forbidden);
    let (_, body) = send(&harness, &owner, &token, Post, "/staff/policies", change.clone()).await;
    assert_eq!(body["type"], "NotElevated");
    step_up(&harness, &owner, &token).await;
    // every language, an https link and at most 90 days
    let mut missing = change.clone();
    missing["messages"].as_object_mut().unwrap().remove("de");
    let (status, _) = send(&harness, &owner, &token, Post, "/staff/policies", missing).await;
    assert_eq!(status, Status::BadRequest);
    let mut plain = change.clone();
    plain["url"] = json!("http://zrzur.com/terms");
    let (status, _) = send(&harness, &owner, &token, Post, "/staff/policies", plain).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &owner, &token, Post, "/staff/policies", change).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["translations"]["ar"], "حدّثنا شروط الاستخدام");
    let id = body["_id"].as_str().unwrap().to_string();

    // the change is listed and reaches users who haven't acknowledged it
    let (status, list) = get(&harness, &ops, &ops_token, "/staff/policies").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let pending = harness.db.fetch_policy_changes().await.unwrap();
    assert_eq!(pending.len(), 1);

    let (status, _) = send(&harness, &owner, &token, Post, &format!("/staff/policies/{id}/withdraw"), json!({ "reason": "wrong link" })).await;
    assert_eq!(status, Status::Ok);
    assert!(harness.db.fetch_policy_changes().await.unwrap().is_empty());
    let actions = audit_actions(&harness).await;
    assert!(actions.iter().any(|(a, _)| a == "staff.policy.publish"));
    assert!(actions.iter().any(|(a, _)| a == "staff.policy.withdraw"));
}

#[rocket::async_test]
async fn notices_reach_the_right_people() {
    use rocket::http::Method::{Get, Post};
    let harness = TestHarness::new().await;
    let (_, user_session, user) = harness.new_user().await;
    let (_, other_session, _) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let owner_token = sign_in(&harness, &owner, -1).await;
    let notice = |user: Option<&str>| json!({
        "user": user,
        "title": { "ar": "تحديث لحسابك", "en": "An update on your account", "de": "Neuigkeiten zu deinem Konto" },
        "body": { "ar": "راجعنا طلبك.", "en": "We looked at your request.", "de": "Wir haben deine Anfrage geprüft." },
        "tone": "info"
    });

    step_up(&harness, &support, &support_token).await;
    let (status, body) = send(&harness, &support, &support_token, Post, "/staff/notices", notice(Some(&user.id))).await;
    assert_eq!(status, Status::Ok, "{body}");
    let id = body["_id"].as_str().unwrap().to_string();
    // Support can't write to everyone
    let (status, _) = send(&harness, &support, &support_token, Post, "/staff/notices", notice(None)).await;
    assert_eq!(status, Status::Forbidden);

    // a notice to one person arrives in their private messages (not as an
    // in-app popup), and only theirs
    let channel = harness
        .db
        .find_direct_message_channel(revolt_database::OfficialAccount::Updates.id(), &user.id)
        .await
        .expect("the notice arrives as a private message from Zrzur Updates");
    let _ = channel;
    let (_, mine) = as_user(&harness, &user_session, Get, "/support/notices", Value::Null).await;
    assert!(mine.as_array().unwrap().is_empty());
    let (_, theirs) = as_user(&harness, &other_session, Get, "/support/notices", Value::Null).await;
    assert!(theirs.as_array().unwrap().is_empty());
    let (status, _) = as_user(&harness, &other_session, Post, &format!("/support/notices/{id}/dismiss"), Value::Null).await;
    assert_eq!(status, Status::NotFound);

    // the owner writes to everyone, in the app only
    step_up(&harness, &owner, &owner_token).await;
    let mut everyone = notice(None);
    everyone["email"] = json!(true);
    let (status, _) = send(&harness, &owner, &owner_token, Post, "/staff/notices", everyone).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &owner, &owner_token, Post, "/staff/notices", notice(None)).await;
    assert_eq!(status, Status::Ok, "{body}");
    let (_, theirs) = as_user(&harness, &other_session, Get, "/support/notices", Value::Null).await;
    assert_eq!(theirs.as_array().unwrap().len(), 1);
    // a withdrawn notice disappears
    let everyone_id = body["_id"].as_str().unwrap();
    let (status, _) = send(&harness, &owner, &owner_token, Post, &format!("/staff/notices/{everyone_id}/withdraw"), json!({ "reason": "sent too early" })).await;
    assert_eq!(status, Status::Ok);
    let (_, theirs) = as_user(&harness, &other_session, Get, "/support/notices", Value::Null).await;
    assert!(theirs.as_array().unwrap().is_empty());
    // missing language
    let mut partial = notice(Some(&user.id));
    partial["title"].as_object_mut().unwrap().remove("ar");
    let (status, _) = send(&harness, &support, &support_token, Post, "/staff/notices", partial).await;
    assert_eq!(status, Status::BadRequest);
    let (status, list) = get(&harness, &support, &support_token, "/staff/notices").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list.as_array().unwrap().len(), 2);
}

#[rocket::async_test]
async fn support_tickets_go_both_ways() {
    use rocket::http::Method::{Get, Post};
    let harness = TestHarness::new().await;
    let (_, user_session, user) = harness.new_user().await;
    let (_, other_session, _) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let analyst = member(&harness, vec![StaffRole::Analyst]).await;
    let token = sign_in(&harness, &support, 0).await;
    let analyst_token = sign_in(&harness, &analyst, -1).await;

    let (status, ticket) = as_user(&harness, &user_session, Post, "/support/tickets",
        json!({ "category": "billing", "subject": "Charged twice", "body": "I was charged twice for Feather this month." })).await;
    assert_eq!(status, Status::Ok, "{ticket}");
    assert_eq!(ticket["status"], "Open");
    let id = ticket["_id"].as_str().unwrap().to_string();
    // others can't see it
    let (status, _) = as_user(&harness, &other_session, Get, &format!("/support/tickets/{id}"), Value::Null).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = as_user(&harness, &user_session, Post, "/support/tickets",
        json!({ "category": "nope", "subject": "x", "body": "y" })).await;
    assert_eq!(status, Status::BadRequest);

    // the queue: Support yes, Analyst no
    let (status, list) = get(&harness, &support, &token, "/staff/support/tickets?status=Open").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["waiting"], true);
    let (status, _) = get(&harness, &analyst, &analyst_token, "/staff/support/tickets").await;
    assert_eq!(status, Status::Forbidden);

    // a staff note stays inside; an answer reaches the person
    let path = format!("/staff/support/tickets/{id}/messages");
    let (status, _) = send(&harness, &support, &token, Post, &path, json!({ "body": "Check the order list first", "internal": true })).await;
    assert_eq!(status, Status::Ok);
    let (status, body) = send(&harness, &support, &token, Post, &path, json!({ "body": "We refunded the second charge." })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Answered");
    assert_eq!(body["assigned_to"], support.user.id.as_str());
    let (_, mine) = as_user(&harness, &user_session, Get, &format!("/support/tickets/{id}"), Value::Null).await;
    assert_eq!(mine["status"], "Answered");
    let messages = mine["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "the staff note must not show");
    assert_eq!(messages[1]["staff"], true);
    assert!(messages.iter().all(|m| m.get("author_id").is_none()));

    // the person answers back, then closes it; reopening works
    let (_, mine) = as_user(&harness, &user_session, Post, &format!("/support/tickets/{id}/messages"), json!({ "body": "Thank you!" })).await;
    assert_eq!(mine["status"], "Open");
    let (_, mine) = as_user(&harness, &user_session, Post, &format!("/support/tickets/{id}/close"), Value::Null).await;
    assert_eq!(mine["status"], "Closed");
    let (status, mine) = as_user(&harness, &user_session, Post, &format!("/support/tickets/{id}/reopen"), Value::Null).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(mine["status"], "Open");

    // assignment only to someone who works on tickets
    let (status, _) = send(&harness, &support, &token, Post, &format!("/staff/support/tickets/{id}"), json!({ "action": "assign", "to": analyst.user.id })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &support, &token, Post, &format!("/staff/support/tickets/{id}"), json!({ "action": "close" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Closed");

    // at most three open tickets
    for n in 0..3 {
        let (status, _) = as_user(&harness, &user_session, Post, "/support/tickets",
            json!({ "category": "other", "subject": format!("Question {n}"), "body": "A question" })).await;
        assert_eq!(status, Status::Ok);
    }
    let (status, _) = as_user(&harness, &user_session, Post, "/support/tickets",
        json!({ "category": "other", "subject": "One more", "body": "A question" })).await;
    assert_eq!(status, Status::BadRequest);
    let (_, all) = as_user(&harness, &user_session, Get, "/support/tickets", Value::Null).await;
    assert_eq!(all.as_array().unwrap().len(), 4);
    let _ = user;
    let actions = audit_actions(&harness).await;
    assert!(actions.iter().any(|(a, _)| a == "staff.support.note"));
    assert!(actions.iter().any(|(a, _)| a == "staff.support.reply"));
    assert!(actions.iter().any(|(a, _)| a == "staff.support.change"));
}

#[rocket::async_test]
async fn server_actions_follow_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let (_, _, server_owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&server_owner).await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let ts_token = sign_in(&harness, &ts, 0).await;
    let token = sign_in(&harness, &owner, -1).await;
    let path = format!("/staff/servers/{}/actions", server.id);

    step_up(&harness, &ts, &ts_token).await;
    let (status, body) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "verify", "reason": "Known community" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(harness.db.fetch_server(&server.id).await.unwrap().flags, Some(2));
    let (status, _) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "verify", "reason": "Known community" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "mark_nsfw", "reason": "Adult content" })).await;
    assert_eq!(status, Status::Ok);
    assert!(harness.db.fetch_server(&server.id).await.unwrap().nsfw);
    // handing a server over is for the owner, and only to a member
    let (status, _) = send(&harness, &ts, &ts_token, Post, &path, json!({ "action": "transfer", "to": ts.user.id, "reason": "Owner lost access" })).await;
    assert_eq!(status, Status::Forbidden);
    step_up(&harness, &owner, &token).await;
    let (status, _) = send(&harness, &owner, &token, Post, &path, json!({ "action": "transfer", "to": ts.user.id, "reason": "Owner lost access" })).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = send(&harness, &owner, &token, Post, &path, json!({ "action": "delete", "reason": "nope" })).await;
    assert_eq!(status, Status::BadRequest);
    assert!(audit_actions(&harness).await.iter().any(|(a, o)| a == "staff.server.action" && o == "Success"));
}

#[rocket::async_test]
async fn security_centre_and_metrics_follow_staff_roles() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let security = member(&harness, vec![StaffRole::Security]).await;
    let ops = member(&harness, vec![StaffRole::Operations]).await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let token = sign_in(&harness, &security, 0).await;
    let ops_token = sign_in(&harness, &ops, -1).await;
    let support_token = sign_in(&harness, &support, 1).await;
    // a refused request shows up in the overview
    let (status, _) = get(&harness, &support, &support_token, "/staff/security").await;
    assert_eq!(status, Status::Forbidden);

    let (status, body) = get(&harness, &security, &token, "/staff/security").await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["week"]["denied"].as_u64().unwrap() >= 1);
    assert!(!body["recent"].as_array().unwrap().is_empty());
    let (status, _) = get(&harness, &ops, &ops_token, "/staff/security").await;
    assert_eq!(status, Status::Ok);

    // ending someone's staff sessions needs security.respond and a step-up
    let lock = format!("/staff/security/members/{}/lock", support.user.id);
    let (status, _) = send(&harness, &ops, &ops_token, Post, &lock, json!({ "reason": "session looks stolen" })).await;
    assert_eq!(status, Status::Forbidden);
    step_up(&harness, &security, &token).await;
    let (status, body) = send(&harness, &security, &token, Post, &lock, json!({ "reason": "session looks stolen" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body, json!(1));
    let (status, _) = get(&harness, &support, &support_token, "/staff/me").await;
    assert_eq!(status, Status::Unauthorized);

    // metrics: Operations yes, Support no
    let (status, body) = get(&harness, &ops, &ops_token, "/staff/operations/metrics").await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["routes"].is_array());
    let (status, _) = get(&harness, &security, &token, "/staff/operations/metrics").await;
    assert_eq!(status, Status::Ok);
}
#[rocket::async_test]
async fn official_messages_arrive_and_cannot_be_answered() {
    use rocket::http::Method::{Get, Post, Put};
    let harness = TestHarness::new().await;
    let (_, user_session, user) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let token = sign_in(&harness, &support, 0).await;

    let (_, ticket) = as_user(&harness, &user_session, Post, "/support/tickets",
        json!({ "category": "account", "subject": "Name change", "body": "Please help me" })).await;
    let id = ticket["_id"].as_str().unwrap();
    let (status, _) = send(&harness, &support, &token, Post, &format!("/staff/support/tickets/{id}/messages"), json!({ "body": "Done, all fixed." })).await;
    assert_eq!(status, Status::Ok);

    // the answer is in their private messages, from Zrzur Support
    let official = revolt_database::OfficialAccount::Support.id();
    let channel = harness.db.find_direct_message_channel(official, &user.id).await.expect("a DM with the official account");
    let messages = harness
        .db
        .fetch_messages(revolt_database::MessageQuery {
            limit: Some(10),
            filter: revolt_database::MessageFilter { channel: Some(channel.id().to_string()), ..Default::default() },
            time_period: revolt_database::MessageTimePeriod::Absolute { before: None, after: None, sort: None },
        })
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].author, official);
    let content = messages[0].content.clone().unwrap();
    // no language known yet: English, with the official footer
    assert!(content.contains("Done, all fixed.") && content.contains("Support answered") && content.ends_with("The Zrzur team"), "{content}");
    assert!(!content.contains("> "), "no quote boxes");
    assert!(!content.contains("الدعم"));
    // the official account has no sign-in
    assert!(harness.db.fetch_account(official).await.is_err());
    // and its picture is the logo: the controller face, version B, as supplied
    // (controller-app-platforms/linux/02-laugh-B/dark, 256 px); the file decides
    let res = harness.client.get(format!("/users/{official}/default_avatar")).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    let picture = res.into_bytes().await.unwrap();
    assert_eq!(picture, include_bytes!("../users/avatars/official.png").to_vec());

    // "Message" on the official profile opens the same conversation
    let (status, opened) = as_user(&harness, &user_session, Get, &format!("/users/{official}/dm"), Value::Null).await;
    assert_eq!(status, Status::Ok, "{opened}");
    assert_eq!(opened["_id"], channel.id());
    // they can read it but not answer
    let (status, _) = as_user(&harness, &user_session, Get, &format!("/channels/{}/messages", channel.id()), Value::Null).await;
    assert_eq!(status, Status::Ok);
    let (status, body) = as_user(&harness, &user_session, Post, &format!("/channels/{}/messages", channel.id()), json!({ "content": "thanks!" })).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    // no friend request, no block
    let (status, _) = as_user(&harness, &user_session, Put, &format!("/users/{official}/friend"), Value::Null).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = as_user(&harness, &user_session, Put, &format!("/users/{official}/block"), Value::Null).await;
    assert_eq!(status, Status::Forbidden);

    // the person uses the app in German: the next message is German, in the
    // same conversation
    let (status, _) = as_user(&harness, &user_session, Put, "/support/locale", json!({ "locale": "de-DE" })).await;
    assert_eq!(status, Status::NoContent);
    let (status, _) = as_user(&harness, &user_session, Put, "/support/locale", json!({ "locale": "<script>" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &support, &token, Post, &format!("/staff/support/tickets/{id}/messages"), json!({ "body": "Noch etwas.", "close": true })).await;
    assert_eq!(status, Status::Ok);
    let again = harness.db.find_direct_message_channel(official, &user.id).await.unwrap();
    assert_eq!(again.id(), channel.id());
    let latest = harness
        .db
        .fetch_messages(revolt_database::MessageQuery {
            limit: Some(10),
            filter: revolt_database::MessageFilter { channel: Some(channel.id().to_string()), ..Default::default() },
            time_period: revolt_database::MessageTimePeriod::Absolute { before: None, after: None, sort: None },
        })
        .await
        .unwrap();
    let german = latest.iter().find(|m| m.content.as_deref().unwrap_or_default().contains("Noch etwas.")).expect("second message");
    let text = german.content.clone().unwrap();
    assert!(text.contains("beantwortet und geschlossen"), "{text}");

    // replying to an official message opens a ticket that shows it to staff
    let (status, reply) = as_user(&harness, &user_session, Post, "/support/tickets",
        json!({ "category": "other", "subject": "Re: your message", "body": "Thanks, one question", "related": german.id })).await;
    assert_eq!(status, Status::Ok, "{reply}");
    let (_, staff_view) = get(&harness, &support, &token, &format!("/staff/support/tickets/{}", reply["_id"].as_str().unwrap())).await;
    assert!(staff_view["in_reply_to"].as_str().unwrap_or_default().contains("Noch etwas."), "{staff_view}");

    // other decisions arrive the same way from Zrzur, e.g. a badge
    let senior = member(&harness, vec![StaffRole::SeniorSupport]).await;
    let senior_token = sign_in(&harness, &senior, -1).await;
    step_up(&harness, &senior, &senior_token).await;
    let (status, body) = send(&harness, &senior, &senior_token, Post, &format!("/staff/users/{}/badges/grant", user.id),
        json!({ "badge": "partner", "reason": "Official partner since today" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let zrzur = harness.db.find_direct_message_channel(revolt_database::OfficialAccount::Zrzur.id(), &user.id).await.unwrap();
    let all = harness
        .db
        .fetch_messages(revolt_database::MessageQuery {
            limit: Some(20),
            filter: revolt_database::MessageFilter { channel: Some(zrzur.id().to_string()), ..Default::default() },
            time_period: revolt_database::MessageTimePeriod::Absolute { before: None, after: None, sort: None },
        })
        .await
        .unwrap();
    assert!(all.iter().any(|m| m.content.as_deref().unwrap_or_default().contains("„Partner“")), "badge message in German");
}
#[rocket::async_test]
async fn the_official_account_cannot_be_abused() {
    use rocket::http::Method::{Delete, Get, Patch, Post, Put};
    let harness = TestHarness::new().await;
    let (_, user_session, user) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let token = sign_in(&harness, &support, 0).await;
    let official = revolt_database::OfficialAccount::Support.id();

    // an official message to `user`
    let (_, ticket) = as_user(&harness, &user_session, Post, "/support/tickets",
        json!({ "category": "account", "subject": "Help", "body": "Please help" })).await;
    let tid = ticket["_id"].as_str().unwrap().to_string();
    send(&harness, &support, &token, Post, &format!("/staff/support/tickets/{tid}/messages"), json!({ "body": "Answer" })).await;
    let channel = harness.db.find_direct_message_channel(official, &user.id).await.unwrap();
    let cid = channel.id().to_string();
    let message = harness
        .db
        .fetch_messages(revolt_database::MessageQuery {
            limit: Some(1),
            filter: revolt_database::MessageFilter { channel: Some(cid.clone()), ..Default::default() },
            time_period: revolt_database::MessageTimePeriod::Absolute { before: None, after: None, sort: None },
        })
        .await
        .unwrap()
        .remove(0);

    // 1. nobody else can read someone's official conversation
    let (status, _) = as_user(&harness, &other_session, Get, &format!("/channels/{cid}/messages"), Value::Null).await;
    assert!([403, 404].contains(&status.code), "{status}");
    let (status, _) = as_user(&harness, &other_session, Get, &format!("/channels/{cid}"), Value::Null).await;
    assert!([403, 404].contains(&status.code), "{status}");

    // 2. the person can't write, edit, delete or react there
    // (pinning: `pinning_in_private_conversations_needs_access`)
    let mid = &message.id;
    for (method, path, body) in [
        (Post, format!("/channels/{cid}/messages"), json!({ "content": "hi" })),
        (Patch, format!("/channels/{cid}/messages/{mid}"), json!({ "content": "changed" })),
        (Delete, format!("/channels/{cid}/messages/{mid}"), Value::Null),
        (Put, format!("/channels/{cid}/messages/{mid}/reactions/%F0%9F%91%8D"), Value::Null),
    ] {
        let (status, body) = as_user(&harness, &user_session, method, &path, body).await;
        assert!([400, 403, 404].contains(&status.code), "{path}: {status} {body}");
    }
    let unchanged = harness.db.fetch_message(mid).await.unwrap();
    assert_eq!(unchanged.content, message.content);

    // 3. no friend request, no block, no group with it
    let (status, _) = as_user(&harness, &user_session, Put, &format!("/users/{official}/friend"), Value::Null).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = as_user(&harness, &user_session, Put, &format!("/users/{official}/block"), Value::Null).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = as_user(&harness, &user_session, Post, "/users/friend", json!({ "username": "Zrzur#0001" })).await;
    assert_ne!(status, Status::Ok);
    let (status, body) = as_user(&harness, &user_session, Post, "/channels/create", json!({ "name": "group", "users": [official] })).await;
    assert_ne!(status, Status::Ok, "{body}");

    // 4. nobody can look like it: display names, lookalike letters, masquerade, webhooks
    for name in ["Zrzur", "ZRZUR", "zrzur support", "Zrzur_Official", "زرزور", "Zrzuг", "Ｚｒｚｕｒ"] {
        let (status, body) = as_user(&harness, &other_session, Patch, "/users/@me", json!({ "display_name": name })).await;
        assert_eq!(status, Status::BadRequest, "display name {name}: {body}");
    }
    let (status, _) = as_user(&harness, &other_session, Patch, "/users/@me", json!({ "display_name": "Zrzur fan club" })).await;
    assert_eq!(status, Status::BadRequest, "a name containing the word is refused");
    let (status, _) = as_user(&harness, &other_session, Patch, "/users/@me", json!({ "display_name": "Support fan club" })).await;
    assert_eq!(status, Status::Ok, "the plain word is fine");
    let (server, channels) = harness.new_server(&other).await;
    let text = channels[0].id().to_string();
    let (status, body) = as_user(&harness, &other_session, Post, &format!("/channels/{text}/messages"),
        json!({ "content": "Your account is suspended", "masquerade": { "name": "Zrzur" } })).await;
    assert_eq!(status, Status::BadRequest, "masquerade: {body}");
    let (status, body) = as_user(&harness, &other_session, Post, &format!("/channels/{text}/webhooks"), json!({ "name": "Zrzur Support" })).await;
    assert_ne!(status, Status::Ok, "webhook: {body}");
    let _ = server;

    // 5. official messages can only come from staff routes
    let (status, _) = as_user(&harness, &user_session, Post, "/staff/notices",
        json!({ "user": other.id, "title": { "ar": "x", "en": "x", "de": "x" }, "body": { "ar": "x", "en": "x", "de": "x" }, "tone": "info" })).await;
    assert_eq!(status, Status::Unauthorized);
    let (status, _) = as_user(&harness, &user_session, Post, &format!("/staff/support/tickets/{tid}/messages"), json!({ "body": "fake answer" })).await;
    assert_eq!(status, Status::Unauthorized);

    // 6. tickets stay private: another person can't read, answer or close them
    for (method, path, body) in [
        (Get, format!("/support/tickets/{tid}"), Value::Null),
        (Post, format!("/support/tickets/{tid}/messages"), json!({ "body": "mine now" })),
        (Post, format!("/support/tickets/{tid}/close"), Value::Null),
    ] {
        let (status, _) = as_user(&harness, &other_session, method, &path, body).await;
        assert_eq!(status, Status::NotFound, "{path}");
    }
    // a reply ticket naming someone else's message shows staff nothing
    let (_, foreign) = as_user(&harness, &other_session, Post, "/support/tickets",
        json!({ "category": "other", "subject": "peek", "body": "show me", "related": mid })).await;
    let (_, mine) = as_user(&harness, &other_session, Get, &format!("/support/tickets/{}", foreign["_id"].as_str().unwrap()), Value::Null).await;
    assert!(mine.get("in_reply_to").is_none() && mine.get("assigned_to").is_none());
    let (_, staff_view) = get(&harness, &support, &token, &format!("/staff/support/tickets/{}", foreign["_id"].as_str().unwrap())).await;
    // the official message is shown (staff wrote it), but only to staff
    assert!(staff_view["in_reply_to"].is_string());
    let (_, user_ticket) = as_user(&harness, &user_session, Get, &format!("/support/tickets/{tid}"), Value::Null).await;
    assert!(user_ticket["messages"].as_array().unwrap().iter().all(|m| m.get("author_id").is_none()));

    // 7. signed-out requests and bad input are refused
    let res = harness.client.get("/support/tickets").dispatch().await;
    assert_eq!(res.status(), Status::Unauthorized);
    drop(res);
    let res = harness.client.put("/support/locale").header(ContentType::JSON).body(r#"{"locale":"ar"}"#).dispatch().await;
    assert_eq!(res.status(), Status::Unauthorized);
    drop(res);
    for bad in ["", "a", "<script>", "ar;drop", "aaaaaaaaaaaaaaaaaaaaaaaa"] {
        let (status, _) = as_user(&harness, &user_session, Put, "/support/locale", json!({ "locale": bad })).await;
        assert_eq!(status, Status::BadRequest, "locale {bad}");
    }
    // 8. the official account has no sign-in and no password reset
    assert!(harness.db.fetch_account(official).await.is_err());
}
#[rocket::async_test]
async fn pinning_in_private_conversations_needs_access() {
    use rocket::http::Method::{Delete, Post};
    let harness = TestHarness::new().await;
    let (_, user_session, user) = harness.new_user().await;
    let (_, other_session, _) = harness.new_user().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let token = sign_in(&harness, &support, 0).await;

    // an official message to `user`
    let (_, ticket) = as_user(&harness, &user_session, Post, "/support/tickets",
        json!({ "category": "account", "subject": "Help", "body": "Please help" })).await;
    let tid = ticket["_id"].as_str().unwrap().to_string();
    send(&harness, &support, &token, Post, &format!("/staff/support/tickets/{tid}/messages"), json!({ "body": "Answer" })).await;
    let channel = harness.db.find_direct_message_channel(revolt_database::OfficialAccount::Support.id(), &user.id).await.unwrap();
    let cid = channel.id().to_string();
    let mid = harness
        .db
        .fetch_messages(revolt_database::MessageQuery {
            limit: Some(1),
            filter: revolt_database::MessageFilter { channel: Some(cid.clone()), ..Default::default() },
            time_period: revolt_database::MessageTimePeriod::Absolute { before: None, after: None, sort: None },
        })
        .await
        .unwrap()
        .remove(0)
        .id;
    let pin = format!("/channels/{cid}/messages/{mid}/pin");

    // the person can't pin in the read-only official conversation
    let (status, body) = as_user(&harness, &user_session, Post, &pin, Value::Null).await;
    assert!([400, 403, 404].contains(&status.code), "own pin: {status} {body}");
    // a stranger can't pin or unpin in someone else's private conversation
    // (before the fix DMs skipped the permission check entirely)
    for method in [Post, Delete] {
        let (status, _) = as_user(&harness, &other_session, method, &pin, Value::Null).await;
        assert!([403, 404].contains(&status.code), "stranger pin: {status}");
    }
    assert!(!harness.db.fetch_message(&mid).await.unwrap().pinned.unwrap_or_default());
}
#[rocket::async_test]
async fn the_official_mark_needs_two_people() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let first = member(&harness, vec![StaffRole::Owner]).await;
    let second = member(&harness, vec![StaffRole::Security]).await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let first_token = sign_in(&harness, &first, 0).await;
    let second_token = sign_in(&harness, &second, -1).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let (_, _, subject) = harness.new_user().await;
    let propose = format!("/staff/users/{}/official", subject.id);
    let give = json!({ "grant": true, "reason": "New team account" });

    // without the permission
    let (status, _) = send(&harness, &support, &support_token, Post, &propose, give.clone()).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = get(&harness, &support, &support_token, "/staff/official").await;
    assert_eq!(status, Status::Forbidden);

    // without a fresh TOTP
    let (status, _) = send(&harness, &first, &first_token, Post, &propose, give.clone()).await;
    assert_eq!(status, Status::Forbidden);

    step_up(&harness, &first, &first_token).await;
    let (status, request) = send(&harness, &first, &first_token, Post, &propose, give.clone()).await;
    assert_eq!(status, Status::Ok, "{request}");
    assert_eq!(request["status"], "Pending");
    let id = request["id"].as_str().unwrap().to_string();

    // a proposal changes nothing, and one at a time
    assert!(!harness.db.fetch_user(&subject.id).await.unwrap().official);
    let (status, _) = send(&harness, &first, &first_token, Post, &propose, give.clone()).await;
    assert_eq!(status, Status::BadRequest);

    // the proposer can't confirm their own proposal
    let confirm = format!("/staff/official/requests/{id}/confirm");
    let (status, _) = send(&harness, &first, &first_token, Post, &confirm, Value::Null).await;
    assert_eq!(status, Status::Forbidden);
    assert!(!harness.db.fetch_user(&subject.id).await.unwrap().official);

    // a different one can, once
    step_up(&harness, &second, &second_token).await;
    let (status, confirmed) = send(&harness, &second, &second_token, Post, &confirm, Value::Null).await;
    assert_eq!(status, Status::Ok, "{confirmed}");
    assert_eq!(confirmed["status"], "Confirmed");
    assert!(harness.db.fetch_user(&subject.id).await.unwrap().official);
    let (status, _) = send(&harness, &second, &second_token, Post, &confirm, Value::Null).await;
    assert_eq!(status, Status::BadRequest);

    // every step is in the log, with who did it
    let actions = audit_actions(&harness).await;
    assert!(actions.iter().any(|(action, outcome)| action == "staff.official.propose" && outcome == "Success"), "{actions:?}");
    assert!(actions.iter().any(|(action, outcome)| action == "staff.official.confirm" && outcome == "Denied"), "{actions:?}");
    assert!(actions.iter().any(|(action, outcome)| action == "staff.official.confirm" && outcome == "Success"), "{actions:?}");

    // the list shows the account
    let (status, overview) = get(&harness, &first, &first_token, "/staff/official").await;
    assert_eq!(status, Status::Ok, "{overview}");
    let accounts = overview["accounts"].as_array().unwrap();
    assert!(accounts.iter().any(|a| a["user_id"] == subject.id.as_str() && a["seeded"] == false), "{overview}");

    // removing takes two people as well; a refusal changes nothing
    let (status, request) = send(&harness, &second, &second_token, Post, &propose, json!({ "grant": false, "reason": "Account retired" })).await;
    assert_eq!(status, Status::Ok, "{request}");
    let refuse = format!("/staff/official/requests/{}/refuse", request["id"].as_str().unwrap());
    let (status, refused) = send(&harness, &first, &first_token, Post, &refuse, json!({ "reason": "Still in use" })).await;
    assert_eq!(status, Status::Ok, "{refused}");
    assert_eq!(refused["status"], "Refused");
    assert!(harness.db.fetch_user(&subject.id).await.unwrap().official);

    // the accounts the server keeps can't lose the mark
    let seeded = revolt_database::OfficialAccount::Safety.id();
    let (status, _) = send(&harness, &first, &first_token, Post, &format!("/staff/users/{seeded}/official"), json!({ "grant": false, "reason": "No thanks" })).await;
    assert_eq!(status, Status::BadRequest);

    // nor can anyone mark themselves
    let (status, _) = send(&harness, &first, &first_token, Post, &format!("/staff/users/{}/official", first.user.id), give).await;
    assert_ne!(status, Status::Ok);
}

#[rocket::async_test]
async fn an_expired_official_proposal_is_void() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let confirmer = member(&harness, vec![StaffRole::Owner]).await;
    let token = sign_in(&harness, &confirmer, 0).await;
    step_up(&harness, &confirmer, &token).await;
    let (_, _, subject) = harness.new_user().await;
    let request = revolt_database::OfficialMarkRequest {
        id: ulid::Ulid::new().to_string(),
        user_id: subject.id.clone(),
        grant: true,
        requested_by: "someone else".to_string(),
        reason: "Too old".to_string(),
        status: revolt_database::OfficialMarkStatus::Pending,
        created_at: 0,
        expires_at: 1,
        decided_by: None,
        decided_at: None,
    };
    harness.db.insert_official_mark_request(&request).await.unwrap();
    let (status, _) = send(&harness, &confirmer, &token, Post, &format!("/staff/official/requests/{}/confirm", request.id), Value::Null).await;
    assert_eq!(status, Status::BadRequest);
    assert!(!harness.db.fetch_user(&subject.id).await.unwrap().official);
}

#[rocket::async_test]
async fn names_that_break_the_rule_are_listed_not_changed() {
    let harness = TestHarness::new().await;
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let token = sign_in(&harness, &owner, 0).await;
    let (_, _, mut old) = harness.new_user().await;
    old.update(
        &harness.db,
        revolt_database::PartialUser { display_name: Some("Zrzur Team".to_string()), ..Default::default() },
        vec![],
    )
    .await
    .unwrap();

    let mut found = vec![];
    let mut offset = 0;
    loop {
        let (status, page) = get(&harness, &owner, &token, &format!("/staff/official/name-conflicts?list=users&offset={offset}")).await;
        assert_eq!(status, Status::Ok, "{page}");
        found.extend(page["conflicts"].as_array().unwrap().clone());
        match page["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert!(found.iter().any(|record| record["id"] == old.id.as_str() && record["name"] == "Zrzur Team"), "{found:?}");
    assert!(!found.iter().any(|record| record["id"] == revolt_database::OfficialAccount::Zrzur.id()), "{found:?}");
    assert_eq!(harness.db.fetch_user(&old.id).await.unwrap().display_name.as_deref(), Some("Zrzur Team"));
}

async fn pending_effect(harness: &TestHarness, creator: &User) -> String {
    let id = ulid::Ulid::new().to_string();
    let item: revolt_database::CommunityItem = serde_json::from_value(json!({
        "_id": id,
        "kind": "Effect",
        "name": "Snow",
        "creator_id": creator.id,
        "file": {
            "_id": TestHarness::rand_string(),
            "tag": "effects",
            "filename": "snow.gif",
            "metadata": { "type": "Image", "width": 576, "height": 1024 },
            "content_type": "image/gif",
            "size": 100
        },
        "status": "PendingReview",
        "revision": 1,
        "rights": { "terms_version": "test", "confirmed_at": 1 }
    }))
    .unwrap();
    harness.db.insert_community_item(&item).await.unwrap();
    id
}

#[rocket::async_test]
async fn designs_are_decided_in_the_console() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let support = member(&harness, vec![StaffRole::Support]).await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let token = sign_in(&harness, &ts, -1).await;
    let (_, _, creator) = harness.new_user().await;
    let id = pending_effect(&harness, &creator).await;
    let refused = pending_effect(&harness, &creator).await;
    let path = format!("/staff/designs/review/{id}");

    // Trust & Safety sees every kind waiting; Support doesn't
    let (status, body) = get(&harness, &ts, &token, "/staff/designs/review").await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["pending"].as_array().unwrap().len(), 2);
    assert_eq!(body["pending"][0]["kind"], "Effect");
    let (status, _) = get(&harness, &support, &support_token, "/staff/designs/review").await;
    assert_eq!(status, Status::Forbidden);

    // a decision needs a step-up, a known action and, to refuse, a reason
    let (_, body) = send(&harness, &ts, &token, Post, &path, json!({ "action": "approve", "version": 0 })).await;
    assert_eq!(body["type"], "NotElevated");
    step_up(&harness, &ts, &token).await;
    let (status, _) = send(&harness, &ts, &token, Post, &path, json!({ "action": "publish", "version": 0 })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &ts, &token, Post, &path, json!({ "action": "reject", "version": 0 })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &support, &support_token, Post, &path, json!({ "action": "approve", "version": 0 })).await;
    assert_eq!(status, Status::Forbidden);

    // refuse one, approve the other, then take that one down
    let (status, body) = send(&harness, &ts, &token, Post, &format!("/staff/designs/review/{refused}"), json!({ "action": "reject", "version": 0, "reason": "It shows a real person" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Rejected");
    let (status, body) = send(&harness, &ts, &token, Post, &path, json!({ "action": "approve", "version": 0 })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Published");
    let (status, body) = send(&harness, &ts, &token, Post, &path, json!({ "action": "suspend", "version": 1, "reason": "It flashes" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["status"], "Suspended");

    // the takedown is a Safety action the creator can appeal, and it is logged
    let actions = harness.db.fetch_safety_actions_for_user(&creator.id).await.unwrap();
    assert_eq!(actions.len(), 1);
    assert!(actions[0].appealable);
    assert_eq!(actions[0].public_reason, "It flashes");
    let logged = audit_actions(&harness).await;
    assert_eq!(logged.iter().filter(|(a, o)| a == "staff.design.review" && o == "Success").count(), 3);
}

#[rocket::async_test]
async fn a_decision_names_the_version_the_reviewer_saw() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let token = sign_in(&harness, &ts, -1).await;
    step_up(&harness, &ts, &token).await;
    let (_, _, creator) = harness.new_user().await;
    let id = pending_effect(&harness, &creator).await;
    let path = format!("/staff/designs/review/{id}");

    let (_, body) = get(&harness, &ts, &token, "/staff/designs/review").await;
    let version = body["pending"][0]["version"].as_i64().expect("the queue gives the version");

    let (status, _) = send(&harness, &ts, &token, Post, &path, json!({ "action": "approve" })).await;
    assert_ne!(status, Status::Ok);
    let stale = json!({ "action": "approve", "version": version + 1 });
    let (status, _) = send(&harness, &ts, &token, Post, &path, stale).await;
    assert_eq!(status, Status::Conflict);
    let seen = json!({ "action": "approve", "version": version });
    let (status, body) = send(&harness, &ts, &token, Post, &path, seen).await;
    assert_eq!(status, Status::Ok, "{body}");
}

#[rocket::async_test]
async fn nobody_decides_on_their_own_design() {
    use rocket::http::Method::Post;
    let harness = TestHarness::new().await;
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let token = sign_in(&harness, &ts, -1).await;
    step_up(&harness, &ts, &token).await;
    let id = pending_effect(&harness, &ts.user).await;
    let (status, _) = send(&harness, &ts, &token, Post, &format!("/staff/designs/review/{id}"), json!({ "action": "approve", "version": 0 })).await;
    assert_ne!(status, Status::Ok);
    let item = harness.db.fetch_community_item(&id).await.unwrap();
    assert_eq!(item.status, revolt_database::CommunityItemStatus::PendingReview);
}
