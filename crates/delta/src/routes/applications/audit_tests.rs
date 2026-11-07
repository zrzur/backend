//! Adding and removing a bot each leave one entry in the server's audit log.

use std::time::Duration;

use crate::{rocket, util::test::TestHarness};
use revolt_database::{AuditLogEntry, AuditLogEntryAction, AuditLogQuery};
use rocket::http::{ContentType, Header, Status};

/// Audit entries are written in the background: wait until `count` are there
async fn entries(harness: &TestHarness, server: &str, count: usize) -> Vec<AuditLogEntry> {
    let mut found = vec![];
    for _ in 0..40 {
        found = harness
            .db
            .get_server_audit_logs(
                server,
                AuditLogQuery {
                    user: None,
                    target: None,
                    r#type: None,
                    before: None,
                    after: None,
                    limit: 100,
                },
            )
            .await
            .unwrap();
        if found.len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // anything late would show up here
    tokio::time::sleep(Duration::from_millis(200)).await;
    harness
        .db
        .get_server_audit_logs(
            server,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap_or(found)
}

#[rocket::async_test]
async fn adding_and_removing_a_bot_are_logged_once_each() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    let (server, _) = harness.new_server(&owner).await;

    let response = harness
        .client
        .post(format!("/bots/{}/invite", bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(json!({ "server": server.id }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);

    let log = entries(&harness, &server.id, 1).await;
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0].user, owner.id);
    match &log[0].action {
        AuditLogEntryAction::BotAdd {
            bot: id,
            permissions,
            scopes,
        } => {
            assert_eq!(id, &bot.id);
            assert_eq!(*permissions, 0);
            assert_eq!(scopes, &vec!["bot.install".to_string()]);
        }
        other => panic!("expected BotAdd, got {other:?}"),
    }

    let response = harness
        .client
        .delete(format!("/servers/{}/applications/{}", server.id, bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);

    let log = entries(&harness, &server.id, 2).await;
    assert_eq!(log.len(), 2, "{log:?}");
    let removal = log
        .iter()
        .find(|entry| !matches!(entry.action, AuditLogEntryAction::BotAdd { .. }))
        .unwrap();
    assert_eq!(removal.user, owner.id);
    assert!(
        matches!(&removal.action, AuditLogEntryAction::BotRemove { bot: id } if id == &bot.id),
        "{removal:?}"
    );
}
