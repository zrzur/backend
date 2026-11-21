//! Deleting servers and bots from the admin console (`deletion.rs`). A
//! child of `test`, so it uses the same staff helpers.

use super::*;
use revolt_config::overwrite_config;
use revolt_database::{Bot, PartialBot, PartialServer, PartialUser, Server};
use revolt_models::v0::BotFlags;
use rocket::http::Method::{Post, Put};

/// A server owner with a server they are a member of
async fn owned_server(harness: &TestHarness, owner: &User) -> Server {
    let (server, channels) = harness.new_server(owner).await;
    revolt_database::Member::create(&harness.db, &server, owner, Some(channels))
        .await
        .unwrap();
    harness.db.fetch_server(&server.id).await.unwrap()
}

async fn make_official(harness: &TestHarness, server: &Server) {
    set_flags(harness, server, 1).await;
}

async fn set_flags(harness: &TestHarness, server: &Server, flags: i32) {
    let mut server = harness.db.fetch_server(&server.id).await.unwrap();
    server
        .update(
            &harness.db,
            PartialServer {
                flags: Some(flags),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

async fn ban(harness: &TestHarness, user: &User) {
    harness
        .db
        .update_user(
            &user.id,
            &PartialUser {
                flags: Some(revolt_models::v0::UserFlags::Banned as i32),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

/// A platform Owner with a fresh step-up
async fn stepped_up_owner(harness: &TestHarness) -> (Member, String) {
    let owner = member(harness, vec![StaffRole::Owner]).await;
    let token = sign_in(harness, &owner, 0).await;
    step_up(harness, &owner, &token).await;
    (owner, token)
}

fn delete_body(reason: &str) -> Value {
    json!({ "action": "delete", "reason": reason })
}

fn count(actions: &[(String, String)], action: &str, outcome: &str) -> usize {
    actions.iter().filter(|(a, o)| a == action && o == outcome).count()
}

#[rocket::async_test]
async fn staff_delete_removes_a_server_and_frees_its_custom_link() {
    overwrite_config(|config| {
        config.features.billing.enabled = false;
    })
    .await;
    let harness = TestHarness::new().await;
    let (_, session, squatter) = harness.new_user().await;
    let server = owned_server(&harness, &squatter).await;
    let (status, body) = as_user(&harness, &session, Put, &format!("/servers/{}/vanity", server.id), json!({ "code": "quiet-garden-club" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(harness.client.get("/invites/quiet-garden-club").dispatch().await.status(), Status::Ok);

    let (owner, token) = stepped_up_owner(&harness).await;
    let path = format!("/staff/servers/{}/actions", server.id);
    // a reason is required
    let (status, _) = send(&harness, &owner, &token, Post, &path, delete_body(" ")).await;
    assert_eq!(status, Status::BadRequest);
    assert!(harness.db.fetch_server(&server.id).await.is_ok());

    let (status, body) = send(&harness, &owner, &token, Post, &path, delete_body("Created only to hold custom links")).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["deleted"], true);
    assert_eq!(body["id"], server.id.as_str());
    assert!(harness.db.fetch_server(&server.id).await.is_err());
    assert!(harness.db.fetch_member(&server.id, &squatter.id).await.is_err());
    assert_eq!(harness.client.get("/invites/quiet-garden-club").dispatch().await.status(), Status::NotFound);

    // the name can be claimed again by another server
    let (_, other_session, other) = harness.new_user().await;
    let other_server = owned_server(&harness, &other).await;
    let (status, body) = as_user(&harness, &other_session, Put, &format!("/servers/{}/vanity", other_server.id), json!({ "code": "quiet-garden-club" })).await;
    assert_eq!(status, Status::Ok, "{body}");

    // already gone: the same answer every time, nothing else deleted
    for _ in 0..2 {
        let (status, _) = send(&harness, &owner, &token, Post, &path, delete_body("Created only to hold custom links")).await;
        assert_eq!(status, Status::NotFound);
    }
    assert!(harness.db.fetch_server(&other_server.id).await.is_ok());

    let actions = audit_actions(&harness).await;
    assert_eq!(count(&actions, "staff.server.delete", "Success"), 1);
}

#[rocket::async_test]
async fn an_official_server_is_never_deleted() {
    let harness = TestHarness::new().await;
    let (_, _, team) = harness.new_user().await;
    let server = owned_server(&harness, &team).await;
    make_official(&harness, &server).await;
    let (owner, token) = stepped_up_owner(&harness).await;
    let (status, body) = send(&harness, &owner, &token, Post, &format!("/staff/servers/{}/actions", server.id), delete_body("Testing the guard")).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(harness.db.fetch_server(&server.id).await.is_ok());
    // official and verified (flags 3) is still official
    let both = owned_server(&harness, &team).await;
    set_flags(&harness, &both, 3).await;
    let (status, _) = send(&harness, &owner, &token, Post, &format!("/staff/servers/{}/actions", both.id), delete_body("Testing the guard")).await;
    assert_eq!(status, Status::Forbidden);
    assert!(harness.db.fetch_server(&both.id).await.is_ok());
    assert_eq!(count(&audit_actions(&harness).await, "staff.server.delete", "Denied"), 2);
}

#[rocket::async_test]
async fn deleting_a_server_never_frees_another_servers_link() {
    overwrite_config(|config| {
        config.features.billing.enabled = false;
    })
    .await;
    let harness = TestHarness::new().await;
    let (_, _, first_owner) = harness.new_user().await;
    let stale = owned_server(&harness, &first_owner).await;
    let (_, session, holder) = harness.new_user().await;
    let theirs = owned_server(&harness, &holder).await;
    let (status, body) = as_user(&harness, &session, Put, &format!("/servers/{}/vanity", theirs.id), json!({ "code": "quiet-garden-club" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    // a field left stale on another server (a repoint that failed)
    let mut stale_server = harness.db.fetch_server(&stale.id).await.unwrap();
    stale_server
        .update(
            &harness.db,
            PartialServer {
                vanity_invite: Some(revolt_models::v0::ServerVanityInvite {
                    code: "quiet-garden-club".to_string(),
                    channel: stale.channels[0].clone(),
                    claimed_at: iso8601_timestamp::Timestamp::now_utc(),
                    below_level_since: None,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let (owner, token) = stepped_up_owner(&harness).await;
    let (status, body) = send(&harness, &owner, &token, Post, &format!("/staff/servers/{}/actions", stale.id), delete_body("Created to grab names")).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(harness.db.fetch_server(&stale.id).await.is_err());
    // the holder keeps its link
    assert_eq!(harness.client.get("/invites/quiet-garden-club").dispatch().await.status(), Status::Ok);
    match harness.db.fetch_invite("quiet-garden-club").await.unwrap() {
        revolt_database::Invite::Server { server, .. } => assert_eq!(server, theirs.id),
        _ => panic!("not a server invite"),
    }
}

#[rocket::async_test]
async fn deleting_needs_the_owner_role_and_a_step_up() {
    let harness = TestHarness::new().await;
    let (_, user_session, user) = harness.new_user().await;
    let server = owned_server(&harness, &user).await;
    let (bot, _) = harness.new_bot(&user).await;
    let server_path = format!("/staff/servers/{}/actions", server.id);
    let bot_path = format!("/staff/bots/{}/delete", bot.id);
    let owned_path = format!("/staff/users/{}/delete-owned", user.id);
    let reason = json!({ "reason": "Created to grab names" });
    ban(&harness, &user).await;

    // an ordinary user (here: the owner of all of it) gets nothing
    for (path, body) in [(&server_path, delete_body("mine")), (&bot_path, reason.clone()), (&owned_path, reason.clone())] {
        let res = harness
            .client
            .post(path.clone())
            .header(Header::new("x-session-token", user_session.token.clone()))
            .header(Header::new("x-staff-session", "made-up"))
            .header(ContentType::JSON)
            .body(body.to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Unauthorized);
    }

    // Trust & Safety may verify servers, but not delete
    let ts = member(&harness, vec![StaffRole::TrustSafety]).await;
    let ts_token = sign_in(&harness, &ts, 0).await;
    // the same refusal with or without a fresh code, recorded each time
    for stepped_up in [false, true] {
        if stepped_up {
            step_up(&harness, &ts, &ts_token).await;
        }
        for (path, body) in [(&server_path, delete_body("Created to grab names")), (&bot_path, reason.clone()), (&owned_path, reason.clone())] {
            let (status, body) = send(&harness, &ts, &ts_token, Post, path, body).await;
            assert_eq!(status, Status::Forbidden, "{body}");
            assert_eq!(body["type"], "NotPrivileged");
        }
    }

    // the Owner, but without a fresh step-up
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let token = sign_in(&harness, &owner, -1).await;
    for (path, body) in [(&server_path, delete_body("Created to grab names")), (&bot_path, reason.clone()), (&owned_path, reason.clone())] {
        let (status, body) = send(&harness, &owner, &token, Post, path, body).await;
        assert_eq!(status, Status::Forbidden, "{body}");
        assert_eq!(body["type"], "NotElevated");
    }

    assert!(harness.db.fetch_server(&server.id).await.is_ok());
    assert!(harness.db.fetch_bot(&bot.id).await.is_ok());
    let actions = audit_actions(&harness).await;
    assert_eq!(count(&actions, "staff.server.delete", "Denied"), 2);
    assert_eq!(count(&actions, "staff.bot.delete", "Denied"), 2);
    assert_eq!(count(&actions, "staff.user.delete_owned", "Denied"), 2);
    assert_eq!(count(&actions, "staff.server.delete", "Success"), 0);
}

#[rocket::async_test]
async fn staff_delete_removes_a_bot_and_its_token_stops_working() {
    let harness = TestHarness::new().await;
    let (_, _, developer) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&developer).await;
    let server = owned_server(&harness, &developer).await;
    let bot_user = harness.db.fetch_user(&bot.id).await.unwrap();
    revolt_database::Member::create(&harness.db, &server, &bot_user, None).await.unwrap();
    let me = |bot: &Bot| {
        harness
            .client
            .get("/users/@me")
            .header(Header::new("x-bot-token", bot.token.clone()))
    };
    assert_eq!(me(&bot).dispatch().await.status(), Status::Ok);

    let (owner, token) = stepped_up_owner(&harness).await;
    let path = format!("/staff/bots/{}/delete", bot.id);
    let (status, _) = send(&harness, &owner, &token, Post, &path, json!({ "reason": "" })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({ "reason": "Belongs to a banned account" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["deleted"], true);
    assert_eq!(body["owner_id"], developer.id.as_str());
    assert!(harness.db.fetch_bot(&bot.id).await.is_err());
    assert!(harness.db.fetch_member(&server.id, &bot.id).await.is_err());
    assert_eq!(me(&bot).dispatch().await.status(), Status::Unauthorized);
    let (status, _) = send(&harness, &owner, &token, Post, &path, json!({ "reason": "Belongs to a banned account" })).await;
    assert_eq!(status, Status::NotFound);

    // an official bot stays
    let (mut official, _) = harness.new_bot(&developer).await;
    official
        .update(
            &harness.db,
            PartialBot {
                flags: Some(BotFlags::Official as i32),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (status, _) = send(&harness, &owner, &token, Post, &format!("/staff/bots/{}/delete", official.id), json!({ "reason": "Testing the guard" })).await;
    assert_eq!(status, Status::Forbidden);
    assert!(harness.db.fetch_bot(&official.id).await.is_ok());
    // verified and official (flags 3) is still official
    let (mut both, _) = harness.new_bot(&developer).await;
    both
        .update(
            &harness.db,
            PartialBot {
                flags: Some(BotFlags::Verified as i32 | BotFlags::Official as i32),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (status, _) = send(&harness, &owner, &token, Post, &format!("/staff/bots/{}/delete", both.id), json!({ "reason": "Testing the guard" })).await;
    assert_eq!(status, Status::Forbidden);
    assert!(harness.db.fetch_bot(&both.id).await.is_ok());

    let actions = audit_actions(&harness).await;
    assert_eq!(count(&actions, "staff.bot.delete", "Success"), 1);
    assert_eq!(count(&actions, "staff.bot.delete", "Denied"), 2);
}

#[rocket::async_test]
async fn delete_owned_needs_a_banned_account_and_removes_everything_but_official() {
    let harness = TestHarness::new().await;
    let (_, _, squatter) = harness.new_user().await;
    let first = owned_server(&harness, &squatter).await;
    let second = owned_server(&harness, &squatter).await;
    let official = owned_server(&harness, &squatter).await;
    make_official(&harness, &official).await;
    let (bot, _) = harness.new_bot(&squatter).await;
    // someone else's server and bot stay
    let (_, _, bystander) = harness.new_user().await;
    let theirs = owned_server(&harness, &bystander).await;
    let (their_bot, _) = harness.new_bot(&bystander).await;

    let (owner, token) = stepped_up_owner(&harness).await;
    let path = format!("/staff/users/{}/delete-owned", squatter.id);
    let body = json!({ "reason": "Account created to grab custom links" });

    // not banned yet: refused, nothing deleted
    let (status, _) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
    assert_eq!(status, Status::BadRequest);
    assert!(harness.db.fetch_server(&first.id).await.is_ok());
    assert!(harness.db.fetch_bot(&bot.id).await.is_ok());
    // an account its owner disabled, or one waiting for its deletion, is
    // not a removed account: refused, its servers stay
    let (_, _, resting) = harness.new_user().await;
    let resting_server = owned_server(&harness, &resting).await;
    let mut account = harness.db.fetch_account(&resting.id).await.unwrap();
    account.disable(&harness.db).await.unwrap();
    let (_, _, leaving) = harness.new_user().await;
    let leaving_server = owned_server(&harness, &leaving).await;
    let mut account = harness.db.fetch_account(&leaving.id).await.unwrap();
    account.schedule_deletion(&harness.db).await.unwrap();
    for (user, server) in [(&resting, &resting_server), (&leaving, &leaving_server)] {
        let (status, _) = send(&harness, &owner, &token, Post, &format!("/staff/users/{}/delete-owned", user.id), body.clone()).await;
        assert_eq!(status, Status::BadRequest);
        assert!(harness.db.fetch_server(&server.id).await.is_ok());
    }
    // a timed suspension is not enough either
    let mut user = harness.db.fetch_user(&squatter.id).await.unwrap();
    user.suspend(&harness.db, Some(3), None).await.unwrap();
    let (status, _) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
    assert_eq!(status, Status::BadRequest);
    assert!(harness.db.fetch_server(&first.id).await.is_ok());

    ban(&harness, &squatter).await;
    let (status, result) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
    assert_eq!(status, Status::Ok, "{result}");
    assert_eq!(result["deleted_server_count"], 2);
    assert_eq!(result["deleted_bot_count"], 1);
    assert_eq!(result["deleted_bots"], json!([bot.id]));
    let mut deleted: Vec<String> = serde_json::from_value(result["deleted_servers"].clone()).unwrap();
    deleted.sort();
    let mut expected = vec![first.id.clone(), second.id.clone()];
    expected.sort();
    assert_eq!(deleted, expected);
    assert_eq!(result["skipped"][0]["id"], official.id.as_str());
    assert_eq!(result["skipped"][0]["why"], "official");
    assert_eq!(result["failed"], json!([]));

    assert!(harness.db.fetch_server(&first.id).await.is_err());
    assert!(harness.db.fetch_server(&second.id).await.is_err());
    assert!(harness.db.fetch_bot(&bot.id).await.is_err());
    assert!(harness.db.fetch_server(&official.id).await.is_ok());
    assert!(harness.db.fetch_server(&theirs.id).await.is_ok());
    assert!(harness.db.fetch_bot(&their_bot.id).await.is_ok());

    // a second run finds nothing left to delete
    let (status, again) = send(&harness, &owner, &token, Post, &path, body).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(again["deleted_server_count"], 0);
    assert_eq!(again["deleted_bot_count"], 0);

    // one record per item, plus the request itself
    let actions = audit_actions(&harness).await;
    assert_eq!(count(&actions, "staff.server.delete", "Success"), 2);
    assert_eq!(count(&actions, "staff.bot.delete", "Success"), 1);
    assert_eq!(count(&actions, "staff.user.delete_owned", "Denied"), 4);
    assert_eq!(count(&actions, "staff.user.delete_owned", "Success"), 2);

    // the account view still opens
    let (status, _) = get(&harness, &owner, &token, &format!("/staff/users/{}", squatter.id)).await;
    assert_eq!(status, Status::Ok);
}
