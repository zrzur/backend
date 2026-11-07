use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User,
};
use revolt_models::v0::Webhook;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::Result;
use rocket::{serde::json::Json, State};

/// # Gets a webhook
///
/// Gets a webhook, for those who may manage it. Its token is never included.
#[openapi(tag = "Webhooks")]
#[get("/<webhook_id>")]
pub async fn webhook_fetch(
    db: &State<Database>,
    webhook_id: Reference<'_>,
    user: User,
) -> Result<Json<Webhook>> {
    let webhook = webhook_id.as_webhook(db).await?;
    let channel = db.fetch_channel(&webhook.channel_id).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageWebhooks)?;

    Ok(Json(webhook.into()))
}
