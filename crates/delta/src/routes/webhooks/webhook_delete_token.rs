use revolt_database::{util::reference::Reference, AuditLogEntryAction, Database};
use revolt_result::Result;
use rocket::State;
use rocket_empty::EmptyResponse;

use crate::util::{audit_log_reason::AuditLogReason, webhooks::WebhookCaller};

/// # Deletes a webhook
///
/// Deletes a webhook with a token
#[openapi(tag = "Webhooks")]
#[delete("/<webhook_id>/<token>")]
pub async fn webhook_delete_token(
    db: &State<Database>,
    caller: WebhookCaller<'_>,
    reason: AuditLogReason,
    webhook_id: Reference<'_>,
    token: String,
) -> Result<EmptyResponse> {
    let webhook = caller.authorise(db, webhook_id, &token).await?;
    let channel = db.fetch_channel(&webhook.channel_id).await?;

    webhook.delete(db).await?;

    // no user here: the entry names the webhook's creator
    if let Some(server_id) = channel.server() {
        AuditLogEntryAction::WebhookDelete {
            webhook: webhook.id,
            name: webhook.name,
            channel: webhook.channel_id,
        }
        .insert(
            db,
            server_id.to_string(),
            reason,
            webhook.creator_id.clone(),
            Some(webhook.creator_id),
        )
        .await;
    }

    Ok(EmptyResponse)
}
