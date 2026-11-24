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

/// The custom link rule (`vanity_eligibility`) met by a one-member server
/// with no messages, the same way the vanity tests lower it, so these tests
/// can claim a link and check that a staff delete frees it
fn rule_met_by_any_server(config: &mut revolt_config::Settings) {
    config.features.vanity.min_members = 1;
    config.features.vanity.active_messages = 0;
    config.features.vanity.active_authors = 0;
}

fn delete_body(reason: &str) -> Value {
    json!({ "action": "delete", "reason": reason })
}

fn count(actions: &[(String, String)], action: &str, outcome: &str) -> usize {
    actions
        .iter()
        .filter(|(a, o)| a == action && o == outcome)
        .count()
}

#[rocket::async_test]
async fn staff_delete_removes_a_server_and_frees_its_custom_link() {
    overwrite_config(|config| {
        config.features.billing.enabled = false;
        rule_met_by_any_server(config);
    })
    .await;
    let harness = TestHarness::new().await;
    let (_, session, squatter) = harness.new_user().await;
    let server = owned_server(&harness, &squatter).await;
    let (status, body) = as_user(
        &harness,
        &session,
        Put,
        &format!("/servers/{}/vanity", server.id),
        json!({ "code": "quiet-garden-club" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(
        harness
            .client
            .get("/invites/quiet-garden-club")
            .dispatch()
            .await
            .status(),
        Status::Ok
    );

    let (owner, token) = stepped_up_owner(&harness).await;
    let path = format!("/staff/servers/{}/actions", server.id);
    // a reason is required
    let (status, _) = send(&harness, &owner, &token, Post, &path, delete_body(" ")).await;
    assert_eq!(status, Status::BadRequest);
    assert!(harness.db.fetch_server(&server.id).await.is_ok());

    let (status, body) = send(
        &harness,
        &owner,
        &token,
        Post,
        &path,
        delete_body("Created only to hold custom links"),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["deleted"], true);
    assert_eq!(body["id"], server.id.as_str());
    assert!(harness.db.fetch_server(&server.id).await.is_err());
    assert!(
        harness
            .db
            .fetch_member(&server.id, &squatter.id)
            .await
            .is_err()
    );
    assert_eq!(
        harness
            .client
            .get("/invites/quiet-garden-club")
            .dispatch()
            .await
            .status(),
        Status::NotFound
    );

    // the name can be claimed again by another server
    let (_, other_session, other) = harness.new_user().await;
    let other_server = owned_server(&harness, &other).await;
    let (status, body) = as_user(
        &harness,
        &other_session,
        Put,
        &format!("/servers/{}/vanity", other_server.id),
        json!({ "code": "quiet-garden-club" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");

    // already gone: the same answer every time, nothing else deleted
    for _ in 0..2 {
        let (status, _) = send(
            &harness,
            &owner,
            &token,
            Post,
            &path,
            delete_body("Created only to hold custom links"),
        )
        .await;
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
    let (status, body) = send(
        &harness,
        &owner,
        &token,
        Post,
        &format!("/staff/servers/{}/actions", server.id),
        delete_body("Testing the guard"),
    )
    .await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(harness.db.fetch_server(&server.id).await.is_ok());
    // official and verified (flags 3) is still official
    let both = owned_server(&harness, &team).await;
    set_flags(&harness, &both, 3).await;
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        Post,
        &format!("/staff/servers/{}/actions", both.id),
        delete_body("Testing the guard"),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(harness.db.fetch_server(&both.id).await.is_ok());
    assert_eq!(
        count(
            &audit_actions(&harness).await,
            "staff.server.delete",
            "Denied"
        ),
        2
    );
}

#[rocket::async_test]
async fn deleting_a_server_never_frees_another_servers_link() {
    overwrite_config(|config| {
        config.features.billing.enabled = false;
        rule_met_by_any_server(config);
    })
    .await;
    let harness = TestHarness::new().await;
    let (_, _, first_owner) = harness.new_user().await;
    let stale = owned_server(&harness, &first_owner).await;
    let (_, session, holder) = harness.new_user().await;
    let theirs = owned_server(&harness, &holder).await;
    let (status, body) = as_user(
        &harness,
        &session,
        Put,
        &format!("/servers/{}/vanity", theirs.id),
        json!({ "code": "quiet-garden-club" }),
    )
    .await;
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
                    via: None,
                    notice_sent_at: None,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let (owner, token) = stepped_up_owner(&harness).await;
    let (status, body) = send(
        &harness,
        &owner,
        &token,
        Post,
        &format!("/staff/servers/{}/actions", stale.id),
        delete_body("Created to grab names"),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(harness.db.fetch_server(&stale.id).await.is_err());
    // the holder keeps its link
    assert_eq!(
        harness
            .client
            .get("/invites/quiet-garden-club")
            .dispatch()
            .await
            .status(),
        Status::Ok
    );
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
    for (path, body) in [
        (&server_path, delete_body("mine")),
        (&bot_path, reason.clone()),
        (&owned_path, reason.clone()),
    ] {
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
        for (path, body) in [
            (&server_path, delete_body("Created to grab names")),
            (&bot_path, reason.clone()),
            (&owned_path, reason.clone()),
        ] {
            let (status, body) = send(&harness, &ts, &ts_token, Post, path, body).await;
            assert_eq!(status, Status::Forbidden, "{body}");
            assert_eq!(body["type"], "NotPrivileged");
        }
    }

    // the Owner, but without a fresh step-up
    let owner = member(&harness, vec![StaffRole::Owner]).await;
    let token = sign_in(&harness, &owner, -1).await;
    for (path, body) in [
        (&server_path, delete_body("Created to grab names")),
        (&bot_path, reason.clone()),
        (&owned_path, reason.clone()),
    ] {
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
    revolt_database::Member::create(&harness.db, &server, &bot_user, None)
        .await
        .unwrap();
    let me = |bot: &Bot| {
        harness
            .client
            .get("/users/@me")
            .header(Header::new("x-bot-token", bot.token.clone()))
    };
    assert_eq!(me(&bot).dispatch().await.status(), Status::Ok);

    let (owner, token) = stepped_up_owner(&harness).await;
    let path = format!("/staff/bots/{}/delete", bot.id);
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        Post,
        &path,
        json!({ "reason": "" }),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(
        &harness,
        &owner,
        &token,
        Post,
        &path,
        json!({ "reason": "Belongs to a banned account" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["deleted"], true);
    assert_eq!(body["owner_id"], developer.id.as_str());
    assert!(harness.db.fetch_bot(&bot.id).await.is_err());
    assert!(harness.db.fetch_member(&server.id, &bot.id).await.is_err());
    assert_eq!(me(&bot).dispatch().await.status(), Status::Unauthorized);
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        Post,
        &path,
        json!({ "reason": "Belongs to a banned account" }),
    )
    .await;
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
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        Post,
        &format!("/staff/bots/{}/delete", official.id),
        json!({ "reason": "Testing the guard" }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(harness.db.fetch_bot(&official.id).await.is_ok());
    // verified and official (flags 3) is still official
    let (mut both, _) = harness.new_bot(&developer).await;
    both.update(
        &harness.db,
        PartialBot {
            flags: Some(BotFlags::Verified as i32 | BotFlags::Official as i32),
            ..Default::default()
        },
        vec![],
    )
    .await
    .unwrap();
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        Post,
        &format!("/staff/bots/{}/delete", both.id),
        json!({ "reason": "Testing the guard" }),
    )
    .await;
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
        let (status, _) = send(
            &harness,
            &owner,
            &token,
            Post,
            &format!("/staff/users/{}/delete-owned", user.id),
            body.clone(),
        )
        .await;
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
    let mut deleted: Vec<String> =
        serde_json::from_value(result["deleted_servers"].clone()).unwrap();
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
    let (status, _) = get(
        &harness,
        &owner,
        &token,
        &format!("/staff/users/{}", squatter.id),
    )
    .await;
    assert_eq!(status, Status::Ok);
}

/// `zrzur-staff delete-owned` calls this shared code with the command-line
/// actor: the same refusal, plan, deletion and audit records as the API
#[rocket::async_test]
async fn the_command_line_runs_the_same_deletion() {
    use revolt_database::{
        AuditRecord, STAFF_ACTOR_CLI,
        staff_deletion::{plan_delete_owned, staff_delete_owned},
        voice::VoiceClient,
    };
    let harness = TestHarness::new().await;
    let voice = harness.client.rocket().state::<VoiceClient>().unwrap();
    let actor = AuditRecord {
        actor_id: STAFF_ACTOR_CLI.to_string(),
        ..Default::default()
    };
    let (_, _, squatter) = harness.new_user().await;
    let server = owned_server(&harness, &squatter).await;
    let official = owned_server(&harness, &squatter).await;
    make_official(&harness, &official).await;
    let (bot, _) = harness.new_bot(&squatter).await;
    let no_forget: revolt_database::staff_deletion::BotForget = |_| {};

    // not banned: refused, nothing deleted, the refusal recorded
    let error = staff_delete_owned(
        &harness.db,
        voice,
        &actor,
        &squatter,
        "Grabbed custom links",
        no_forget,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.error_type,
        revolt_result::ErrorType::InvalidOperation
    ));
    assert!(harness.db.fetch_server(&server.id).await.is_ok());
    assert!(harness.db.fetch_bot(&bot.id).await.is_ok());

    ban(&harness, &squatter).await;
    let squatter = harness.db.fetch_user(&squatter.id).await.unwrap();
    let plan = plan_delete_owned(&harness.db, &squatter).await.unwrap();
    assert_eq!(
        plan.servers
            .iter()
            .map(|s| s.server.id.clone())
            .collect::<Vec<_>>(),
        vec![server.id.clone()]
    );
    assert_eq!(plan.servers[0].members, 1);
    assert_eq!(
        plan.bots
            .iter()
            .map(|b| b.bot.id.clone())
            .collect::<Vec<_>>(),
        vec![bot.id.clone()]
    );
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].id, official.id);

    let result = staff_delete_owned(
        &harness.db,
        voice,
        &actor,
        &squatter,
        "Grabbed custom links",
        no_forget,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.deleted_servers, vec![server.id.clone()]);
    assert_eq!(result.deleted_bots, vec![bot.id.clone()]);
    assert_eq!(
        (result.deleted_server_count, result.deleted_bot_count),
        (1, 1)
    );
    assert_eq!(result.skipped[0].id, official.id);
    assert!(result.failed.is_empty());
    assert!(harness.db.fetch_server(&server.id).await.is_err());
    assert!(harness.db.fetch_bot(&bot.id).await.is_err());
    assert!(harness.db.fetch_server(&official.id).await.is_ok());

    let cli: Vec<(String, String)> = harness
        .db
        .fetch_platform_audit(revolt_database::PlatformAuditQuery {
            limit: 500,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            e.actor_id == STAFF_ACTOR_CLI && e.reason.as_deref() == Some("Grabbed custom links")
        })
        .map(|e| (e.action, format!("{:?}", e.outcome)))
        .collect();
    assert_eq!(count(&cli, "staff.user.delete_owned", "Denied"), 1);
    assert_eq!(count(&cli, "staff.user.delete_owned", "Success"), 1);
    assert_eq!(count(&cli, "staff.server.delete", "Success"), 1);
    assert_eq!(count(&cli, "staff.bot.delete", "Success"), 1);
}

/// A bot marked through `zrzur-staff official-bot` (its account's
/// `official_bot`), with no Official flag on the bot
async fn mark_official_bot(harness: &TestHarness, bot: &Bot) {
    harness
        .db
        .update_user(
            &bot.id,
            &PartialUser {
                official_bot: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

#[rocket::async_test]
async fn a_bot_with_only_the_official_bot_mark_is_kept() {
    let harness = TestHarness::new().await;
    let (_, _, developer) = harness.new_user().await;
    let (marked, _) = harness.new_bot(&developer).await;
    mark_official_bot(&harness, &marked).await;
    let (owner, token) = stepped_up_owner(&harness).await;

    // the single delete refuses it, and the list shows it as official
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        Post,
        &format!("/staff/bots/{}/delete", marked.id),
        json!({ "reason": "Testing the guard" }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(harness.db.fetch_bot(&marked.id).await.is_ok());
    let (status, list) = get(
        &harness,
        &owner,
        &token,
        &format!("/staff/bots?q={}", marked.id),
    )
    .await;
    assert_eq!(status, Status::Ok, "{list}");
    assert_eq!(list[0]["official"], true);

    // delete-owned keeps it and lists it
    ban(&harness, &developer).await;
    let (status, result) = send(
        &harness,
        &owner,
        &token,
        Post,
        &format!("/staff/users/{}/delete-owned", developer.id),
        json!({ "reason": "Testing the guard" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{result}");
    assert_eq!(result["deleted_bot_count"], 0);
    assert_eq!(result["skipped"][0]["id"], marked.id.as_str());
    assert_eq!(result["skipped"][0]["why"], "official");
    assert!(harness.db.fetch_bot(&marked.id).await.is_ok());
}

/// Removes a bot's own account record, so deleting the bot fails
async fn drop_user_record(harness: &TestHarness, id: &str) {
    match &harness.db {
        Database::Reference(db) => {
            db.users.lock().await.remove(id);
        }
        Database::MongoDb(db) => {
            db.col::<Document>("users")
                .delete_one(doc! { "_id": id })
                .await
                .unwrap();
        }
    }
}

struct Cli {
    code: i32,
    out: String,
    set_up: bool,
}

/// `zrzur-staff delete-owned` as the command runs it, with `typed` on stdin.
/// `before_question` runs where the command connects RabbitMQ and LiveKit.
async fn cli<F>(
    harness: &TestHarness,
    user_id: &str,
    args: &[&str],
    typed: &str,
    before_question: F,
) -> Cli
where
    F: std::future::Future<Output = ()>,
{
    use revolt_database::{
        AuditRecord, STAFF_ACTOR_CLI, staff_deletion::run_delete_owned_command, voice::VoiceClient,
    };
    let actor = AuditRecord {
        actor_id: STAFF_ACTOR_CLI.to_string(),
        ..Default::default()
    };
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let mut input = std::io::Cursor::new(typed.as_bytes().to_vec());
    let mut out = Vec::new();
    let set_up = std::cell::Cell::new(false);
    let code = run_delete_owned_command(
        &harness.db,
        &actor,
        user_id,
        &args,
        &mut input,
        &mut out,
        || async {
            set_up.set(true);
            before_question.await;
            let nodes = revolt_config::config().await.api.livekit.nodes.clone();
            Ok(VoiceClient::new(nodes))
        },
    )
    .await;
    Cli {
        code,
        out: String::from_utf8(out).unwrap(),
        set_up: set_up.get(),
    }
}

fn handle(user: &User) -> String {
    format!("{}#{}\n", user.username, user.discriminator)
}

const REASON: [&str; 5] = ["Account", "created", "to", "grab", "links"];

fn with_reason(extra: &[&'static str]) -> Vec<&'static str> {
    let mut args = extra.to_vec();
    args.extend(REASON);
    args
}

#[rocket::async_test]
async fn the_command_shows_the_plan_and_deletes_only_after_the_name() {
    let harness = TestHarness::new().await;
    let (_, _, squatter) = harness.new_user().await;
    let server = owned_server(&harness, &squatter).await;
    let (bot, _) = harness.new_bot(&squatter).await;
    let (marked, _) = harness.new_bot(&squatter).await;
    mark_official_bot(&harness, &marked).await;
    ban(&harness, &squatter).await;
    let squatter = harness.db.fetch_user(&squatter.id).await.unwrap();

    // only --dry-run is an option; there is no way to skip the question
    let run = cli(
        &harness,
        &squatter.id,
        &with_reason(&["--yes"]),
        &handle(&squatter),
        async {},
    )
    .await;
    assert_eq!(run.code, 2);
    assert!(!run.set_up);

    // a dry run shows the plan and changes nothing, not even the audit log
    let before = audit_actions(&harness).await.len();
    let run = cli(
        &harness,
        &squatter.id,
        &with_reason(&["--dry-run"]),
        "",
        async {},
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.out);
    assert!(
        run.out.contains(&server.id) && run.out.contains(&bot.id),
        "{}",
        run.out
    );
    assert!(
        run.out
            .contains(&format!("{}#{}", squatter.username, squatter.discriminator))
    );
    assert!(run.out.contains("kept (official): 1"), "{}", run.out);
    assert!(!run.set_up);
    assert_eq!(audit_actions(&harness).await.len(), before);

    // a wrong name, or no answer at all, deletes nothing
    for typed in ["someone#0000\n", "", "\n"] {
        let run = cli(&harness, &squatter.id, &REASON, typed, async {}).await;
        assert_eq!(run.code, 1, "{typed:?}: {}", run.out);
        assert!(harness.db.fetch_server(&server.id).await.is_ok());
        assert!(harness.db.fetch_bot(&bot.id).await.is_ok());
    }
    assert_eq!(audit_actions(&harness).await.len(), before);

    // a plan that changes after it was shown is not carried out
    let run = cli(&harness, &squatter.id, &REASON, &handle(&squatter), async {
        owned_server(&harness, &squatter).await;
    })
    .await;
    assert_eq!(run.code, 1, "{}", run.out);
    assert!(harness.db.fetch_server(&server.id).await.is_ok());

    // the exact name: everything shown goes, the official bot stays
    let run = cli(&harness, &squatter.id, &REASON, &handle(&squatter), async {
    })
    .await;
    assert_eq!(run.code, 0, "{}", run.out);
    assert!(harness.db.fetch_server(&server.id).await.is_err());
    assert!(harness.db.fetch_bot(&bot.id).await.is_err());
    assert!(harness.db.fetch_bot(&marked.id).await.is_ok());
    assert!(
        run.out.contains("deleted 2 servers") && run.out.contains("deleted 1 bots"),
        "{}",
        run.out
    );
    let actions = audit_actions(&harness).await;
    assert_eq!(count(&actions, "staff.user.delete_owned", "Denied"), 1);
    assert_eq!(count(&actions, "staff.user.delete_owned", "Success"), 1);
}

#[rocket::async_test]
async fn the_command_refuses_an_account_that_is_not_banned() {
    let harness = TestHarness::new().await;
    let (_, _, user) = harness.new_user().await;
    let server = owned_server(&harness, &user).await;

    // a dry run says so with exit 1, and writes nothing
    let run = cli(
        &harness,
        &user.id,
        &with_reason(&["--dry-run"]),
        "",
        async {},
    )
    .await;
    assert_eq!(run.code, 1, "{}", run.out);
    assert!(audit_actions(&harness).await.is_empty());

    // the right name typed, and the account banned in the meantime: still
    // refused at the first read, nothing set up, nothing deleted
    let run = cli(&harness, &user.id, &REASON, &handle(&user), async {
        ban(&harness, &user).await;
    })
    .await;
    assert_eq!(run.code, 1, "{}", run.out);
    assert!(!run.set_up);
    assert!(harness.db.fetch_server(&server.id).await.is_ok());
    let actions = audit_actions(&harness).await;
    assert_eq!(count(&actions, "staff.user.delete_owned", "Denied"), 1);
    assert_eq!(count(&actions, "staff.server.delete", "Success"), 0);
}

#[rocket::async_test]
async fn the_command_exits_1_when_one_item_fails() {
    let harness = TestHarness::new().await;
    let (_, _, squatter) = harness.new_user().await;
    let server = owned_server(&harness, &squatter).await;
    let (broken, _) = harness.new_bot(&squatter).await;
    drop_user_record(&harness, &broken.id).await;
    ban(&harness, &squatter).await;
    let squatter = harness.db.fetch_user(&squatter.id).await.unwrap();

    let run = cli(&harness, &squatter.id, &REASON, &handle(&squatter), async {
    })
    .await;
    assert_eq!(run.code, 1, "{}", run.out);
    // the rest still went ahead, and the summary was given
    assert!(harness.db.fetch_server(&server.id).await.is_err());
    assert!(run.out.contains("failed: 1"), "{}", run.out);
    assert!(run.out.contains(&broken.id), "{}", run.out);
}

/// `POST /staff/users/<id>/delete-request` and its cancel: the Owner role,
/// a fresh step-up, refusals recorded; the account is scheduled and
/// disabled, listed, then the cancel enables it again. Accounts that own an
/// official server are refused; a person's own deletion is not cancelled
/// by staff.
#[rocket::async_test]
async fn staff_delete_request_schedules_and_cancels() {
    use revolt_database::DeletionInfo;
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    owned_server(&harness, &person).await;
    let path = format!("/staff/users/{}/delete-request", person.id);
    let body = json!({ "reason": "Asked by email to delete", "category": "Spam" });

    // not an Owner: refused, recorded
    let safety = member(&harness, vec![StaffRole::TrustSafety]).await;
    let safety_token = sign_in(&harness, &safety, 0).await;
    step_up(&harness, &safety, &safety_token).await;
    let (status, _) = send(&harness, &safety, &safety_token, Post, &path, body.clone()).await;
    assert_eq!(status, Status::Forbidden);

    let (owner, token) = stepped_up_owner(&harness).await;
    // never yourself
    let own = format!("/staff/users/{}/delete-request", owner.user.id);
    let (status, _) = send(&harness, &owner, &token, Post, &own, body.clone()).await;
    assert_ne!(status, Status::Ok);
    // never an account that owns an official server
    let (_, _, keeper) = harness.new_user().await;
    let official = owned_server(&harness, &keeper).await;
    make_official(&harness, &official).await;
    let kept = format!("/staff/users/{}/delete-request", keeper.id);
    let (status, _) = send(&harness, &owner, &token, Post, &kept, body.clone()).await;
    assert_eq!(status, Status::Forbidden);
    assert!(
        harness
            .db
            .fetch_account(&keeper.id)
            .await
            .unwrap()
            .deletion
            .is_none()
    );

    let (status, scheduled) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
    assert_eq!(status, Status::Ok, "{scheduled}");
    assert!(scheduled["after"].as_i64().unwrap() > 0);
    let account = harness.db.fetch_account(&person.id).await.unwrap();
    assert!(matches!(
        account.deletion,
        Some(DeletionInfo::Scheduled { .. })
    ));
    assert!(account.disabled);
    // asked twice: no effect
    let (status, _) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
    assert_ne!(status, Status::Ok);
    // listed
    let (status, list) = get(&harness, &owner, &token, "/staff/deletions").await;
    assert_eq!(status, Status::Ok);
    assert!(list.to_string().contains(&person.id), "{list}");
    // listing is personal data: not for Trust & Safety
    let (status, _) = get(&harness, &safety, &safety_token, "/staff/deletions").await;
    assert_eq!(status, Status::Forbidden);

    // cancelled: enabled again
    let cancel = format!("{path}/cancel");
    let (status, cancelled) = send(&harness, &owner, &token, Post, &cancel, body.clone()).await;
    assert_eq!(status, Status::Ok, "{cancelled}");
    let account = harness.db.fetch_account(&person.id).await.unwrap();
    assert!(account.deletion.is_none());
    assert!(!account.disabled);
    let (status, _) = send(&harness, &owner, &token, Post, &cancel, body.clone()).await;
    assert_ne!(status, Status::Ok);

    // a person's own deletion stays theirs
    let (_, _, leaving) = harness.new_user().await;
    let mut account = harness.db.fetch_account(&leaving.id).await.unwrap();
    account.schedule_deletion(&harness.db).await.unwrap();
    let theirs = format!("/staff/users/{}/delete-request/cancel", leaving.id);
    let (status, _) = send(&harness, &owner, &token, Post, &theirs, body).await;
    assert_ne!(status, Status::Ok);
    assert!(matches!(
        harness
            .db
            .fetch_account(&leaving.id)
            .await
            .unwrap()
            .deletion,
        Some(DeletionInfo::Scheduled { .. })
    ));

    let actions = audit_actions(&harness).await;
    for (action, outcome) in [
        ("staff.user.delete_request", "Success"),
        ("staff.user.delete_request", "Denied"),
        ("staff.user.delete_request_cancel", "Success"),
        ("staff.user.delete_request_cancel", "Denied"),
    ] {
        assert!(count(&actions, action, outcome) >= 1, "{action} {outcome}");
    }
}

/// A cancelled staff request restores exactly the state before it: an
/// account that was disabled already stays disabled, a suspended one is
/// never enabled; the request records who asked
#[rocket::async_test]
async fn cancelling_a_staff_request_restores_the_state_before() {
    use revolt_database::DeletionInfo;
    let harness = TestHarness::new().await;
    let (owner, token) = stepped_up_owner(&harness).await;
    let body = json!({ "reason": "Asked by email to delete", "category": "Spam" });

    // disabled before the request
    let (mut quiet_account, _, quiet) = harness.new_user().await;
    quiet_account.disabled = true;
    quiet_account.save(&harness.db).await.unwrap();
    // suspended after the request
    let (_, _, suspended) = harness.new_user().await;
    // enabled before
    let (_, _, plain) = harness.new_user().await;

    for user in [&quiet, &suspended, &plain] {
        let path = format!("/staff/users/{}/delete-request", user.id);
        let (status, answer) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
        assert_eq!(status, Status::Ok, "{answer}");
        let account = harness.db.fetch_account(&user.id).await.unwrap();
        let Some(DeletionInfo::Scheduled { requested_by, .. }) = account.deletion else {
            panic!("not scheduled");
        };
        assert_eq!(requested_by.as_deref(), Some(owner.user.id.as_str()));
    }
    harness
        .db
        .update_user(
            &suspended.id,
            &PartialUser {
                flags: Some(revolt_models::v0::UserFlags::SuspendedUntil as i32),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    for (user, disabled) in [(&quiet, true), (&suspended, true), (&plain, false)] {
        let path = format!("/staff/users/{}/delete-request/cancel", user.id);
        let (status, answer) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
        assert_eq!(status, Status::Ok, "{answer}");
        let account = harness.db.fetch_account(&user.id).await.unwrap();
        assert!(account.deletion.is_none());
        assert_eq!(account.disabled, disabled, "{}", user.id);
    }
}

/// A safety case about `user` with one piece of evidence; gives the case
async fn case_about(
    harness: &TestHarness,
    user: &User,
    status: &str,
    category: &str,
    hold: bool,
) -> revolt_models::v0::SafetyCase {
    let now = revolt_database::staff_now();
    let id = ulid::Ulid::new().to_string();
    let case: revolt_models::v0::SafetyCase = serde_json::from_value(json!({
        "_id": id, "reference": format!("R-{}", &id[20..]), "reporter_id": "reporter",
        "target": { "kind": "User", "id": user.id }, "route": "Platform",
        "category": category, "description": "", "evidence_ids": [],
        "status": status, "priority": "Normal", "policy_version": "1",
        "created_at": now, "updated_at": now, "version": 1, "legal_hold": hold
    }))
    .unwrap();
    harness.db.insert_safety_case(&case).await.unwrap();
    let evidence: revolt_models::v0::SafetyEvidence = serde_json::from_value(json!({
        "_id": ulid::Ulid::new().to_string(), "case_id": case.id, "kind": "User",
        "object_id": user.id, "captured_at": now, "content": "kept",
        "expires_at": now + 30 * 86_400_000_i64, "legal_hold": false
    }))
    .unwrap();
    harness.db.insert_safety_evidence(&evidence).await.unwrap();
    case
}

/// DK-10: no deletion while a safety matter is open: an open case, a held
/// case, preserved evidence of illegal content, a pending appeal; each
/// refused with a record that counts them and an answer that names them
#[rocket::async_test]
async fn delete_request_waits_for_open_safety_matters() {
    let harness = TestHarness::new().await;
    let (owner, token) = stepped_up_owner(&harness).await;
    let body = json!({ "reason": "Asked by email to delete", "category": "Spam" });

    let (_, _, open) = harness.new_user().await;
    let open_case = case_about(&harness, &open, "UnderReview", "Spam", false).await;
    let (_, _, held) = harness.new_user().await;
    let held_case = case_about(&harness, &held, "Closed", "Spam", true).await;
    let (_, _, preserved) = harness.new_user().await;
    let preserved_case = case_about(&harness, &preserved, "Closed", "IllegalContent", false).await;
    let (_, _, appealing) = harness.new_user().await;
    let appeal: revolt_models::v0::SafetyAppeal = serde_json::from_value(json!({
        "_id": ulid::Ulid::new().to_string(), "action_id": ulid::Ulid::new().to_string(),
        "user_id": appealing.id, "statement": "I did not do it", "status": "Pending",
        "created_at": revolt_database::staff_now()
    }))
    .unwrap();
    harness.db.insert_safety_appeal(&appeal).await.unwrap();

    for (user, named) in [
        (&open, open_case.reference.clone()),
        (&held, held_case.reference.clone()),
        (&preserved, preserved_case.reference.clone()),
        (&appealing, appeal.id.clone()),
    ] {
        let path = format!("/staff/users/{}/delete-request", user.id);
        let (status, answer) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
        assert_eq!(status, Status::BadRequest, "{answer}");
        assert!(answer.to_string().contains(&named), "{named}: {answer}");
        assert!(
            harness
                .db
                .fetch_account(&user.id)
                .await
                .unwrap()
                .deletion
                .is_none()
        );
    }
    let records = harness
        .db
        .fetch_platform_audit(revolt_database::PlatformAuditQuery {
            limit: 500,
            ..Default::default()
        })
        .await
        .unwrap();
    let refused = records
        .iter()
        .filter(|e| {
            e.action == "staff.user.delete_request"
                && format!("{:?}", e.outcome) == "Denied"
                && e.summary.as_deref() == Some("open safety matters: 1")
        })
        .count();
    assert_eq!(refused, 4);
}

/// DK-10: an account with only a closed case is deleted, and the deletion
/// job leaves the case and its evidence; DK-09: the person gets the
/// statement of reasons and the delivery is recorded by category only
#[rocket::async_test]
async fn deleted_accounts_leave_safety_evidence_and_get_a_statement() {
    use revolt_database::DeletionInfo;
    let harness = TestHarness::new().await;
    let (owner, token) = stepped_up_owner(&harness).await;
    let (_, _, person) = harness.new_user().await;
    let case = case_about(&harness, &person, "Closed", "Spam", false).await;
    let path = format!("/staff/users/{}/delete-request", person.id);
    // the ground is required, and the facts are short
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        Post,
        &path,
        json!({ "reason": "Spam waves" }),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    let long = "x".repeat(301);
    let (status, _) = send(
        &harness,
        &owner,
        &token,
        Post,
        &path,
        json!({ "reason": long, "category": "Spam" }),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    let (status, answer) = send(
        &harness,
        &owner,
        &token,
        Post,
        &path,
        json!({ "reason": "Sent the same advert to 400 servers", "category": "Spam" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{answer}");

    let records = harness
        .db
        .fetch_platform_audit(revolt_database::PlatformAuditQuery {
            limit: 500,
            ..Default::default()
        })
        .await
        .unwrap();
    let notice = records
        .iter()
        .find(|e| e.action == "staff.user.delete_request_notice")
        .expect("the delivery is recorded");
    assert_eq!(format!("{:?}", notice.outcome), "Success");
    // the test stack may or may not have a mail server: both are delivery
    // states, never text
    let summary = notice.summary.clone().unwrap_or_default();
    assert!(
        summary == "category=Spam; message=sent; email=sent"
            || summary == "category=Spam; message=sent; email=off",
        "{summary}"
    );
    assert!(
        notice.reason.is_none(),
        "no free text in the delivery record"
    );

    // the deletion job runs: the case and its evidence stay
    let mut account = harness.db.fetch_account(&person.id).await.unwrap();
    assert!(matches!(
        account.deletion,
        Some(DeletionInfo::Scheduled { .. })
    ));
    account.mark_deleted(&harness.db).await.unwrap();
    assert!(harness.db.fetch_safety_case(&case.id).await.is_ok());
    assert_eq!(
        harness
            .db
            .fetch_safety_evidence(&case.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// DK-09: the statement of reasons in ar, en and de: a person decided, the
/// ground, the facts, the date, objection and appeal with the pause, a copy
/// of the data, the out-of-court line; the cancel text has no free text
#[test]
fn deletion_notice_says_what_the_law_asks_in_three_languages() {
    use crate::routes::staff::deletion::{Notice, notice_text};
    let facts = "Sent the same advert to 400 servers";
    let scheduled = Notice::Scheduled {
        date: "2026-12-08".to_string(),
        category: revolt_models::v0::SafetyCategory::Spam,
        facts: facts.to_string(),
    };
    for (language, words) in [
        (
            "en",
            [
                "person",
                "appeal",
                "paused",
                "copy of your data",
                "data protection",
            ],
        ),
        (
            "ar",
            [
                "إنسان",
                "الطعن",
                "يتوقف الحذف",
                "نسخة من بياناتك",
                "حماية البيانات",
            ],
        ),
        (
            "de",
            [
                "Mensch",
                "Einspruch",
                "angehalten",
                "Kopie deiner Daten",
                "Datenschutzbehörde",
            ],
        ),
    ] {
        let (title, body) = notice_text(language, &scheduled, "2026-11-24");
        assert!(!title.is_empty());
        for part in [
            facts,
            "2026-12-08",
            "2026-11-24",
            revolt_database::ACCOUNT_CONTACT_EMAIL,
        ] {
            assert!(body.contains(part), "{language} {part}: {body}");
        }
        for word in words {
            assert!(body.contains(word), "{language} {word}: {body}");
        }
        let ground =
            crate::util::official::rule_name(language, revolt_models::v0::SafetyCategory::Spam);
        assert!(body.contains(&ground), "{language} ground: {body}");
        assert!(body.contains(&crate::util::official::out_of_court_lines(language)));
        let (_, cancelled) = notice_text(
            language,
            &Notice::Cancelled {
                stays_disabled: false,
            },
            "2026-11-24",
        );
        assert!(!cancelled.contains(facts));
    }
}
