use revolt_config::config;
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User,
};
use revolt_models::v0::Webhook;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::routes::channels::webhook_create::assert_webhooks_enabled;

/// # Gets a server's webhooks
///
/// Webhooks of every channel of the server where you have Manage Webhooks,
/// newest first. Their tokens are not included.
#[openapi(tag = "Webhooks")]
#[get("/<target>/webhooks")]
pub async fn fetch_server_webhooks(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<Vec<Webhook>>> {
    assert_webhooks_enabled(config().await.features.webhooks_enabled)?;

    let server = target.as_server(db).await?;

    let mut managed = vec![];
    for channel in db.fetch_channels(&server.channels).await? {
        let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
        if calculate_channel_permissions(&mut query)
            .await
            .has_channel_permission(ChannelPermission::ManageWebhooks)
        {
            managed.push(channel.id().to_string());
        }
    }
    if managed.is_empty() {
        return Err(create_error!(MissingPermission {
            permission: ChannelPermission::ManageWebhooks.to_string()
        }));
    }

    let mut webhooks = db.fetch_webhooks_for_channels(&managed).await?;
    webhooks.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(Json(webhooks.into_iter().map(Into::into).collect()))
}
