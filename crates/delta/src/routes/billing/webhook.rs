//! Payment provider webhooks. The signature is checked on the exact bytes
//! received; the event is stored once (its id is the key) and answered
//! straight away. Handling runs in the background with a cap on how many at
//! once; anything beyond that, and every retry, is picked up by the billing
//! webhook worker. Handling never trusts the event's contents: it fetches
//! the object from the provider (`revolt_database::process_webhook_event`).

use std::sync::{Arc, LazyLock};

use revolt_database::{
    process_webhook_event, receive_webhook, Database, WebhookHeaders, MAX_WEBHOOK_BODY,
};
use revolt_result::ErrorType;
use rocket::{
    data::{Data, ToByteUnit},
    http::Status,
    request::{FromRequest, Outcome},
    Request, State,
};
use tokio::sync::Semaphore;

/// Events handled at once straight after arriving
static IN_FLIGHT: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(8)));

/// Headers a provider signs its webhooks with (Stripe: one header; PayPal:
/// the `Paypal-*` transmission headers). Nothing else is passed on.
pub struct ProviderSignature(WebhookHeaders);

const SIGNATURE_HEADERS: [&str; 6] = [
    "Stripe-Signature",
    "Paypal-Transmission-Id",
    "Paypal-Transmission-Time",
    "Paypal-Transmission-Sig",
    "Paypal-Cert-Url",
    "Paypal-Auth-Algo",
];

#[rocket::async_trait]
impl<'r> FromRequest<'r> for ProviderSignature {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let mut headers = vec![];
        for name in SIGNATURE_HEADERS {
            if let Some(value) = request.headers().get_one(name) {
                if value.is_empty() || value.len() > 2048 {
                    return Outcome::Error((Status::BadRequest, ()));
                }
                headers.push((name.to_string(), value.to_string()));
            }
        }
        if headers.is_empty() {
            return Outcome::Error((Status::BadRequest, ()));
        }
        Outcome::Success(ProviderSignature(WebhookHeaders(headers)))
    }
}

/// Receive a provider webhook
#[post("/webhooks/<provider>", data = "<body>")]
pub async fn provider_webhook(
    db: &State<Database>,
    provider: &str,
    signature: ProviderSignature,
    body: Data<'_>,
) -> Status {
    let body = match body.open((MAX_WEBHOOK_BODY as u64).bytes()).into_bytes().await {
        Ok(body) if body.is_complete() => body.into_inner(),
        Ok(_) => return Status::PayloadTooLarge,
        Err(_) => return Status::BadRequest,
    };

    match receive_webhook(db, provider, &signature.0, &body).await {
        Ok(Some(id)) => {
            if let Ok(permit) = IN_FLIGHT.clone().try_acquire_owned() {
                let db = db.inner().clone();
                tokio::spawn(async move {
                    if let Err(error) = process_webhook_event(&db, &id).await {
                        log::error!("billing: webhook {id}: {error:?}");
                    }
                    drop(permit);
                });
            }
            Status::Ok
        }
        // duplicate or ignored: acknowledged so the provider stops sending
        Ok(None) => Status::Ok,
        Err(error) => match error.error_type {
            ErrorType::NotFound | ErrorType::FeatureDisabled { .. } => Status::NotFound,
            ErrorType::InvalidCredentials => Status::BadRequest,
            ErrorType::PayloadTooLarge => Status::PayloadTooLarge,
            ErrorType::InvalidOperation => Status::BadRequest,
            // storage trouble: the provider retries later
            _ => Status::ServiceUnavailable,
        },
    }
}
