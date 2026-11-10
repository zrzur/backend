use revolt_database::{
    util::{
        idempotency::IdempotencyKey, permissions::DatabasePermissionQuery, reference::Reference,
    },
    Database, User, AMQP,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::delivery::{deliver, Delivery};
use crate::util::webhooks::WebhookCaller;

/// # Sends a test message
///
/// Posts a short message through a webhook, for those who manage it: they
/// have no token to do it with, since it is only shown when made.
#[openapi(tag = "Webhooks")]
#[post("/<webhook_id>/test", data = "<data>")]
pub async fn webhook_send_test(
    db: &State<Database>,
    amqp: &State<AMQP>,
    caller: WebhookCaller<'_>,
    user: User,
    webhook_id: Reference<'_>,
    data: Json<v0::DataWebhookTest>,
) -> Result<Json<v0::Message>> {
    let webhook = webhook_id.as_webhook(db).await?;
    let channel = db.fetch_channel(&webhook.channel_id).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageWebhooks)?;

    let content = data.into_inner().content;
    if content.trim().is_empty() || content.chars().count() > 200 {
        return Err(create_error!(FailedValidation {
            error: "a test message has 1 to 200 characters".to_string()
        }));
    }

    let message = v0::DataMessageSend {
        nonce: None,
        content: Some(content),
        attachments: None,
        replies: None,
        embeds: None,
        masquerade: None,
        interactions: None,
        components: None,
        interaction: None,
        poll: None,
        flags: Some(1 << v0::MessageFlags::SuppressNotifications as u32),
    };
    let delivery = Delivery {
        allowed_mentions: Some(Default::default()),
        ..Default::default()
    };
    let key = IdempotencyKey::unchecked_from_string(ulid::Ulid::new().to_string());
    let sent = deliver(db, amqp, &caller, &webhook, message, delivery, key).await?;
    Ok(Json(sent.into_model(None, None)))
}
