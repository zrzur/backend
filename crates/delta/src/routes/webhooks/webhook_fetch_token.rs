use revolt_database::{util::reference::Reference, Database};
use revolt_models::v0::{ResponseWebhook, Webhook};
use revolt_result::Result;
use rocket::{serde::json::Json, State};

use crate::util::webhooks::WebhookCaller;

/// # Gets a webhook
///
/// Gets a webhook with its token. Who made it is not included.
#[openapi(tag = "Webhooks")]
#[get("/<webhook_id>/<token>")]
pub async fn webhook_fetch_token(
    db: &State<Database>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
) -> Result<Json<ResponseWebhook>> {
    let webhook = caller.authorise(db, webhook_id, &token).await?;
    Ok(Json(Webhook::from(webhook).into()))
}
