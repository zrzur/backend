use revolt_config::config;
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Database, User,
};
use revolt_models::v0::Webhook;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::Result;
use rocket::{serde::json::Json, State};

use crate::routes::channels::webhook_create::with_token;
use crate::util::audit_log_reason::AuditLogReason;

/// # Regenerates a webhook's token
///
/// Makes a new token. The old one stops working at once. The answer is the
/// only time the new token is shown.
#[openapi(tag = "Webhooks")]
#[post("/<webhook_id>/token")]
pub async fn webhook_regenerate_token(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    webhook_id: Reference<'_>,
) -> Result<Json<Webhook>> {
    let mut webhook = webhook_id.as_webhook(db).await?;
    let channel = db.fetch_channel(&webhook.channel_id).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageWebhooks)?;

    let token = webhook.regenerate_token(db).await?;

    if let Some(server_id) = channel.server() {
        AuditLogEntryAction::WebhookTokenReset {
            webhook: webhook.id.clone(),
            name: webhook.name.clone(),
            channel: webhook.channel_id.clone(),
        }
        .insert(
            db,
            server_id.to_string(),
            reason,
            user.id,
            Some(webhook.creator_id.clone()),
        )
        .await;
    }

    Ok(Json(with_token(webhook.into(), token, &config().await.hosts.api)))
}
