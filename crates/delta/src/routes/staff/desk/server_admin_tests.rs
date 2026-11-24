//! Desk servers (`/desk-servers`): marks, hand-over and deletions run as
//! the console's own cores with the console's records on the desk session;
//! deletions need the Owner role, a fresh code and the target's exact
//! name, typed, checked at the card and again at the confirmation.

use revolt_database::{DeletionInfo, Member, PartialServer, PartialUser, Server, StaffRole, User};
use revolt_models::v0::ApplicationCommand;
use rocket::http::Status;
use serde_json::{Value, json};

use super::moderation_tests::{all_areas, card, confirm, press, recorded, step_up, text, user_opt};
use super::tests::{Person, Stage, error_type, register_served, stage};

async fn register_servers(s: &Stage) -> ApplicationCommand {
    register_served(s).await.remove("desk-servers").unwrap()
}

/// A server whose owner is a member, with `extra` as a member too
async fn owned_server(s: &Stage, owner: &User, extra: Option<&User>) -> Server {
    let (server, channels) = s.harness.new_server(owner).await;
    Member::create(&s.harness.db, &server, owner, Some(channels))
        .await
        .unwrap();
    if let Some(extra) = extra {
        Member::create(&s.harness.db, &server, extra, None)
            .await
            .unwrap();
    }
    s.harness.db.fetch_server(&server.id).await.unwrap()
}

async fn server_update(s: &Stage, id: &str, partial: PartialServer) {
    let mut server = s.harness.db.fetch_server(id).await.unwrap();
    server.update(&s.harness.db, partial, vec![]).await.unwrap();
}

async fn ban(s: &Stage, user: &User) {
    s.harness
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

fn handle(user: &User) -> String {
    revolt_models::v0::user_handle(&user.username, &user.discriminator)
}

/// The card is refused at `/run` with this error
async fn refused(
    s: &Stage,
    who: &Person,
    command: &ApplicationCommand,
    group: &str,
    name: &str,
    options: Value,
) -> String {
    let pressed = press(s, who, command, group, name, options).await;
    let (status, body) = s.hand("run", &pressed).await;
    assert_ne!(status, Status::Ok, "{group} {name}: {body}");
    error_type(&body).to_string()
}

/// The last record of `who`: (action, outcome, summary)
async fn last(s: &Stage, who: &Person) -> (String, String, String) {
    s.records(who).await.pop().unwrap()
}

/// Every command, confirmed by an Owner: the console's records on the desk
/// session and the change made; a fresh code for all of them
#[rocket::async_test]
async fn desk_servers_run_the_consoles_actions() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_servers(&s).await;

    let (_, _, someone) = s.harness.new_user().await;
    let (_, _, heir) = s.harness.new_user().await;
    let marked = owned_server(&s, &someone, Some(&heir)).await;
    let handed = owned_server(&s, &someone, Some(&heir)).await;
    let doomed = owned_server(&s, &someone, None).await;
    let (bot, bot_user) = s.harness.new_bot(&someone).await;
    let (_, _, banned) = s.harness.new_user().await;
    let banned_server = owned_server(&s, &banned, None).await;
    let (banned_bot, _) = s.harness.new_bot(&banned).await;
    ban(&s, &banned).await;
    let (_, _, leaving) = s.harness.new_user().await;

    let mark = card(
        &s,
        &owner,
        &command,
        "server",
        "mark",
        json!([
            text("server", &marked.id),
            text("action", "verify"),
            text("reason", "A known community")
        ]),
    )
    .await;
    let transfer = card(
        &s,
        &owner,
        &command,
        "server",
        "transfer",
        json!([
            text("server", &handed.id),
            user_opt(&heir.id),
            text("reason", "The owner asked us")
        ]),
    )
    .await;
    assert!(
        transfer.view["content"]
            .as_str()
            .unwrap()
            .contains(&heir.id)
    );
    let delete = card(
        &s,
        &owner,
        &command,
        "server",
        "delete",
        json!([
            text("server", &doomed.id),
            text("confirm_name", &doomed.name),
            text("reason", "Spam server")
        ]),
    )
    .await;
    // the card says what goes
    let content = delete.view["content"].as_str().unwrap().to_string();
    assert!(
        content.contains(&format!("{} channels", doomed.channels.len())),
        "{content}"
    );
    assert!(content.contains("1 members"), "{content}");
    let bot_delete = card(
        &s,
        &owner,
        &command,
        "bot",
        "delete",
        json!([
            text("bot", &bot.id),
            text("confirm_name", &bot_user.username),
            text("reason", "Spam bot")
        ]),
    )
    .await;
    let owned = card(
        &s,
        &owner,
        &command,
        "user",
        "delete-owned",
        json!([
            user_opt(&banned.id),
            text("confirm_name", &handle(&banned)),
            text("reason", "Banned spammer")
        ]),
    )
    .await;
    let content = owned.view["content"].as_str().unwrap().to_string();
    assert!(
        content.contains(&banned_server.id) && content.contains(&banned_bot.id),
        "{content}"
    );
    let request = card(
        &s,
        &owner,
        &command,
        "user",
        "delete-request",
        json!([
            user_opt(&leaving.id),
            text("confirm_name", &handle(&leaving)),
            text("reason", "Asked by email")
        ]),
    )
    .await;
    assert!(
        request.view["content"]
            .as_str()
            .unwrap()
            .contains("14 days")
    );

    // ---- without a fresh code: nothing ----
    let cards = [&mark, &transfer, &delete, &bot_delete, &owned, &request];
    for c in cards {
        assert!(c.danger(), "{}", c.view);
        let (_, body) = confirm(&s, &owner, c).await;
        assert_eq!(error_type(&body), "NotElevated", "{body}");
    }
    step_up(&s, &owner, 0).await;
    for (c, action) in [
        (&mark, "staff.server.action"),
        (&transfer, "staff.server.action"),
        (&delete, "staff.server.delete"),
        (&bot_delete, "staff.bot.delete"),
        (&owned, "staff.user.delete_owned"),
        (&request, "staff.user.delete_request"),
    ] {
        let (status, body) = confirm(&s, &owner, c).await;
        assert_eq!(status, Status::Ok, "{action}: {body}");
        recorded(&s, &owner, action).await;
    }
    let db = &s.harness.db;
    assert_eq!(
        db.fetch_server(&marked.id)
            .await
            .unwrap()
            .flags
            .unwrap_or_default()
            & 2,
        2
    );
    assert_eq!(db.fetch_server(&handed.id).await.unwrap().owner, heir.id);
    assert!(db.fetch_server(&doomed.id).await.is_err());
    assert!(db.fetch_bot(&bot.id).await.is_err());
    assert!(db.fetch_server(&banned_server.id).await.is_err());
    assert!(db.fetch_bot(&banned_bot.id).await.is_err());
    assert!(matches!(
        db.fetch_account(&leaving.id).await.unwrap().deletion,
        Some(DeletionInfo::Scheduled { .. })
    ));

    // the waiting list shows it, in the presser's language
    let pressed = press(&s, &owner, &command, "deletion", "list", json!([])).await;
    let (status, view) = s.hand("run", &pressed).await;
    assert_eq!(status, Status::Ok, "{view}");
    assert!(view.to_string().contains(&leaving.id), "{view}");

    // asked again: no effect, refused at the card
    let again = refused(
        &s,
        &owner,
        &command,
        "user",
        "delete-request",
        json!([
            user_opt(&leaving.id),
            text("confirm_name", &handle(&leaving)),
            text("reason", "Asked by email")
        ]),
    )
    .await;
    assert_eq!(again, "NoEffect");
}

/// The typed name: a wrong one refused at the card; a target renamed after
/// the card refused at the confirmation, recorded, and nothing deleted
#[rocket::async_test]
async fn desk_deletions_need_the_exact_name() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_servers(&s).await;
    let (_, _, someone) = s.harness.new_user().await;
    let server = owned_server(&s, &someone, None).await;
    let (bot, bot_user) = s.harness.new_bot(&someone).await;
    let (_, _, leaving) = s.harness.new_user().await;

    for (group, name, options) in [
        (
            "server",
            "delete",
            json!([
                text("server", &server.id),
                text("confirm_name", "not the name"),
                text("reason", "Spam server")
            ]),
        ),
        (
            "bot",
            "delete",
            json!([
                text("bot", &bot.id),
                text("confirm_name", &format!("{}x", bot_user.username)),
                text("reason", "Spam bot")
            ]),
        ),
        (
            "user",
            "delete-request",
            json!([
                user_opt(&leaving.id),
                text("confirm_name", &format!("{}x", handle(&leaving))),
                text("reason", "Asked by email")
            ]),
        ),
    ] {
        let error = refused(&s, &owner, &command, group, name, options).await;
        assert_eq!(error, "FailedValidation", "{group} {name}");
    }

    let c = card(
        &s,
        &owner,
        &command,
        "server",
        "delete",
        json!([
            text("server", &server.id),
            text("confirm_name", &server.name),
            text("reason", "Spam server")
        ]),
    )
    .await;
    server_update(
        &s,
        &server.id,
        PartialServer {
            name: Some("Renamed".to_string()),
            ..Default::default()
        },
    )
    .await;
    step_up(&s, &owner, 0).await;
    let (_, body) = confirm(&s, &owner, &c).await;
    assert_eq!(error_type(&body), "InvalidOperation", "{body}");
    assert_eq!(last(&s, &owner).await.1, "Denied");
    assert!(s.harness.db.fetch_server(&server.id).await.is_ok());
}

/// An official server or bot is never deleted: refused at the card with
/// the console's Denied record
#[rocket::async_test]
async fn desk_official_targets_are_refused() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    let command = register_servers(&s).await;
    let (_, _, someone) = s.harness.new_user().await;
    let server = owned_server(&s, &someone, None).await;
    server_update(
        &s,
        &server.id,
        PartialServer {
            flags: Some(1),
            ..Default::default()
        },
    )
    .await;
    let error = refused(
        &s,
        &owner,
        &command,
        "server",
        "delete",
        json!([
            text("server", &server.id),
            text("confirm_name", &server.name),
            text("reason", "Spam server")
        ]),
    )
    .await;
    assert_eq!(error, "NotPrivileged");
    let (action, outcome, summary) = last(&s, &owner).await;
    assert_eq!(
        (action.as_str(), outcome.as_str(), summary.as_str()),
        ("staff.server.delete", "Denied", "official")
    );
    assert!(s.harness.db.fetch_server(&server.id).await.is_ok());
}

/// A staff member with the permissions but not the Owner role: the marks
/// work, the hand-over and every deletion are refused, the deletions with
/// the console's Denied record
#[rocket::async_test]
async fn desk_server_deletions_need_the_owner_role() {
    let s = stage().await;
    all_areas(&s);
    let safety = s.person(vec![StaffRole::TrustSafety]).await;
    s.sign_in(&safety, -1).await;
    let command = register_servers(&s).await;
    let (_, _, someone) = s.harness.new_user().await;
    let (_, _, heir) = s.harness.new_user().await;
    let server = owned_server(&s, &someone, Some(&heir)).await;
    let (bot, bot_user) = s.harness.new_bot(&someone).await;
    ban(&s, &someone).await;
    let (_, _, leaving) = s.harness.new_user().await;

    let transfer = refused(
        &s,
        &safety,
        &command,
        "server",
        "transfer",
        json!([
            text("server", &server.id),
            user_opt(&heir.id),
            text("reason", "The owner asked us")
        ]),
    )
    .await;
    assert_eq!(transfer, "NotPrivileged");
    for (group, name, options, action) in [
        (
            "server",
            "delete",
            json!([
                text("server", &server.id),
                text("confirm_name", &server.name),
                text("reason", "Spam server")
            ]),
            "staff.server.delete",
        ),
        (
            "bot",
            "delete",
            json!([
                text("bot", &bot.id),
                text("confirm_name", &bot_user.username),
                text("reason", "Spam bot")
            ]),
            "staff.bot.delete",
        ),
        (
            "user",
            "delete-owned",
            json!([
                user_opt(&someone.id),
                text("confirm_name", &handle(&someone)),
                text("reason", "Banned spammer")
            ]),
            "staff.user.delete_owned",
        ),
        (
            "user",
            "delete-request",
            json!([
                user_opt(&leaving.id),
                text("confirm_name", &handle(&leaving)),
                text("reason", "Asked by email")
            ]),
            "staff.user.delete_request",
        ),
    ] {
        let error = refused(&s, &safety, &command, group, name, options).await;
        assert_eq!(error, "NotPrivileged", "{group} {name}");
        let (recorded_action, outcome, summary) = last(&s, &safety).await;
        assert_eq!(
            (recorded_action.as_str(), outcome.as_str(), summary.as_str()),
            (action, "Denied", "owner role required")
        );
    }

    // the mark is theirs to change, with a fresh code
    let c = card(
        &s,
        &safety,
        &command,
        "server",
        "mark",
        json!([
            text("server", &server.id),
            text("action", "mark_nsfw"),
            text("reason", "Adult content")
        ]),
    )
    .await;
    step_up(&s, &safety, 0).await;
    let (status, body) = confirm(&s, &safety, &c).await;
    assert_eq!(status, Status::Ok, "{body}");
    recorded(&s, &safety, "staff.server.action").await;
    assert!(s.harness.db.fetch_server(&server.id).await.unwrap().nsfw);
}

/// Account deletion requests: never yourself or a staff member
#[rocket::async_test]
async fn desk_delete_request_refuses_protected_accounts() {
    let s = stage().await;
    all_areas(&s);
    let owner = s.person(vec![StaffRole::Owner]).await;
    let colleague = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&owner, -1).await;
    let command = register_servers(&s).await;
    for (who, error) in [
        (&owner.user, "CannotReportYourself"),
        (&colleague.user, "NotPrivileged"),
    ] {
        let got = refused(
            &s,
            &owner,
            &command,
            "user",
            "delete-request",
            json!([
                user_opt(&who.id),
                text("confirm_name", &handle(who)),
                text("reason", "Asked by email")
            ]),
        )
        .await;
        assert_eq!(got, error);
    }
}

/// Without the permission, no card: refused as the presser, recorded
#[rocket::async_test]
async fn desk_servers_refused_without_permission() {
    let s = stage().await;
    all_areas(&s);
    let analyst = s.person(vec![StaffRole::Analyst]).await;
    s.sign_in(&analyst, -1).await;
    let command = register_servers(&s).await;
    let entries: Vec<_> = super::registry::entries()
        .filter(|e| e.area == Some("servers"))
        .collect();
    assert_eq!(entries.len(), 7);
    for entry in entries {
        let before = s.records(&analyst).await.len();
        let pressed = press(
            &s,
            &analyst,
            &command,
            entry.group.unwrap(),
            entry.name,
            json!([]),
        )
        .await;
        let (status, body) = s.hand("run", &pressed).await;
        assert_eq!(status, Status::Forbidden, "{}: {body}", entry.key());
        assert_eq!(
            error_type(&body),
            "MissingPlatformPermission",
            "{}",
            entry.key()
        );
        let records = s.records(&analyst).await;
        assert_eq!(records.len(), before + 1, "{}", entry.key());
        assert_eq!(
            (records[before].0.as_str(), records[before].1.as_str()),
            ("staff.access.denied", "Denied")
        );
    }
}
