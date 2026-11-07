use revolt_database::util::reference::Reference;
use revolt_database::{Database, File, PartialWebhook};
use revolt_models::v0::{DataEditWebhook, ResponseWebhook, Webhook};
use revolt_models::validator::Validate;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::webhooks::WebhookCaller;

/// # Edits a webhook
///
/// Edits a webhook's name and avatar with its token. Its permissions and
/// channel can only be changed by a manager.
#[openapi(tag = "Webhooks")]
#[patch("/<webhook_id>/<token>", data = "<data>")]
pub async fn webhook_edit_token(
    db: &State<Database>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    data: Json<DataEditWebhook>,
) -> Result<Json<ResponseWebhook>> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let mut webhook = caller.authorise(db, webhook_id, &token).await?;

    // whoever holds a token must not gain more than the webhook was given
    if data.permissions.is_some() || data.channel_id.is_some() {
        return Err(create_error!(MissingPermission {
            permission: "ManageWebhooks".to_string()
        }));
    }

    if data.name.is_none() && data.avatar.is_none() && data.remove.is_empty() {
        return Ok(Json(Webhook::from(webhook).into()));
    };

    let DataEditWebhook {
        name,
        avatar,
        remove,
        ..
    } = data;

    if let Some(name) = &name {
        revolt_database::assert_webhook_name(db, name).await?;
    }

    let mut partial = PartialWebhook {
        name,
        ..Default::default()
    };

    if let Some(avatar) = avatar {
        let file = File::use_webhook_avatar(db, &avatar, &webhook.id, &webhook.creator_id).await?;
        partial.avatar = Some(file)
    }

    webhook
        .update(db, partial, remove.into_iter().map(|v| v.into()).collect())
        .await?;

    Ok(Json(Webhook::from(webhook).into()))
}
