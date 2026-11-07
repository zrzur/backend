use std::time::Duration;

use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Database, Message, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// # Bulk Delete Messages
///
/// Delete multiple messages you've sent or one you have permission to delete.
///
/// This will always require `ManageMessages` permission regardless of whether you own the message or not.
///
/// Messages must have been sent within the past 1 week.
#[openapi(tag = "Messaging")]
#[delete("/<target>/messages/bulk", data = "<options>", rank = 1)]
pub async fn bulk_delete_messages(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    options: Json<v0::OptionsBulkDelete>,
) -> Result<EmptyResponse> {
    let options = options.into_inner();
    options.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    for id in &options.ids {
        if ulid::Ulid::from_string(id)
            .map_err(|_| create_error!(InvalidOperation))?
            .datetime()
            .elapsed()
            .expect("Time went backwards")
            > Duration::from_hours(7 * 24)  // 7 days
        {
            return Err(create_error!(InvalidOperation));
        }
    }

    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;

    // ids of other channels are skipped; the log counts what was really deleted
    let count = Message::bulk_delete(db, target.id, options.ids.clone()).await?;

    if let Some(server) = channel.server() {
        AuditLogEntryAction::MessageBulkDelete {
            channel: channel.id().to_string(),
            count,
        }
        .insert(db, server.to_string(), reason, user.id, None)
        .await;
    };

    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use revolt_database::{Member, Server};
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    use crate::util::test::TestHarness;

    async fn send(harness: &TestHarness, token: &str, channel: &str, content: &str) -> String {
        let message: v0::Message = harness
            .client
            .post(format!("/channels/{channel}/messages"))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.to_string()))
            .body(json!({ "content": content }).to_string())
            .dispatch()
            .await
            .into_json()
            .await
            .expect("`Message`");
        message.id
    }

    /// Ids of messages in other channels are skipped, and the audit log
    /// must count only what was really deleted.
    #[rocket::async_test]
    async fn audit_log_counts_only_deleted_messages() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = Server::create(
            &harness.db,
            v0::DataCreateServer {
                name: "Bulk".to_string(),
                ..Default::default()
            },
            &user,
            true,
        )
        .await
        .unwrap();
        Member::create(&harness.db, &server, &user, None).await.unwrap();
        let here = channels[0].id().to_string();
        let other: v0::Channel = harness
            .client
            .post(format!("/servers/{}/channels", server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(json!({ "type": "Text", "name": "other" }).to_string())
            .dispatch()
            .await
            .into_json()
            .await
            .expect("`Channel`");
        let other = other.id().to_string();

        let a = send(&harness, &session.token, &here, "one").await;
        let b = send(&harness, &session.token, &here, "two").await;
        let elsewhere = send(&harness, &session.token, &other, "stays").await;

        let status = harness
            .client
            .delete(format!("/channels/{here}/messages/bulk"))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(json!({ "ids": [a, b, elsewhere.clone()] }).to_string())
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::NoContent);

        // the message of the other channel is untouched
        let status = harness
            .client
            .get(format!("/channels/{other}/messages/{elsewhere}"))
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::Ok);

        // the entry is written in a background task (`cfg(test)` in the
        // database crate does not apply to the API's tests), so wait for it
        let mut count = None;
        for _ in 0..40 {
            let logs: v0::AuditLogQueryResponse = harness
                .client
                .get(format!("/servers/{}/audit_logs", server.id))
                .header(Header::new("x-session-token", session.token.clone()))
                .dispatch()
                .await
                .into_json()
                .await
                .expect("`AuditLogQueryResponse`");
            count = logs.audit_logs.iter().find_map(|entry| match &entry.action {
                v0::AuditLogEntryAction::MessageBulkDelete { count, .. } => Some(*count),
                _ => None,
            });
            if count.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(count.expect("bulk delete in the audit log"), 2);
    }
}
