//! Stripe over its REST API (form-encoded requests, JSON answers), with a
//! pinned API version so answers keep their shape.

use std::time::Duration;

use revolt_result::{create_error, Result};
use serde_json::Value;

use super::{
    verify_stripe_signature, CheckoutMode, CheckoutPaymentStatus, CheckoutRequest, CheckoutSession,
    CheckoutStatus, EventKind, PaymentProvider, WebhookHeaders, ProviderDispute, ProviderEvent, ProviderInvoice,
    ProviderPayment, ProviderRefund, ProviderSubscription, MAX_WEBHOOK_BODY,
};

const API_BASE: &str = "https://api.stripe.com";
/// Answers are read in this version's shape
const API_VERSION: &str = "2024-06-20";

pub struct StripeProvider {
    key: String,
    webhook_secrets: Vec<String>,
    base: String,
    livemode: bool,
    http: reqwest::Client,
}

impl std::fmt::Debug for StripeProvider {
    // never print the keys
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StripeProvider")
            .field("base", &self.base)
            .field("livemode", &self.livemode)
            .finish()
    }
}

fn is_local_http(base: &str) -> bool {
    base.starts_with("http://127.0.0.1:") || base.starts_with("http://localhost:")
}

impl StripeProvider {
    /// `api_base` is only honoured for local tests with test keys
    pub fn new(key: String, webhook_secrets: Vec<String>, api_base: String) -> Option<Self> {
        let livemode = key.starts_with("sk_live_") || key.starts_with("rk_live_");
        let testmode = key.starts_with("sk_test_") || key.starts_with("rk_test_");
        if !livemode && !testmode {
            return None;
        }
        let base = if api_base.is_empty() {
            API_BASE.to_string()
        } else if testmode && is_local_http(&api_base) {
            api_base.trim_end_matches('/').to_string()
        } else {
            log::error!("billing: provider api_base is only allowed locally with test keys");
            return None;
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        Some(Self {
            key,
            webhook_secrets,
            base,
            livemode,
            http,
        })
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        form: &[(String, String)],
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut request = self
            .http
            .request(method.clone(), &url)
            .bearer_auth(&self.key)
            .header("Stripe-Version", API_VERSION);
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        if method != reqwest::Method::GET {
            request = request.form(form);
        }

        let response = request.send().await.map_err(|error| {
            log::warn!("billing: stripe {path}: request failed: {}", error.without_url());
            create_error!(ProxyError)
        })?;
        let status = response.status();
        let body: Value = response.json().await.map_err(|_| {
            log::warn!("billing: stripe {path}: unreadable answer ({status})");
            create_error!(ProxyError)
        })?;
        if status.is_success() {
            return Ok(body);
        }

        // error type and code only: messages can echo request data
        let kind = body["error"]["type"].as_str().unwrap_or("unknown");
        let code = body["error"]["code"].as_str().unwrap_or("");
        log::warn!("billing: stripe {path}: {status} {kind} {code}");
        if status == reqwest::StatusCode::NOT_FOUND {
            Err(create_error!(NotFound))
        } else if status == reqwest::StatusCode::BAD_REQUEST {
            Err(create_error!(InvalidOperation))
        } else {
            Err(create_error!(ProxyError))
        }
    }

    /// GET; any query is part of `path` (fixed strings only)
    async fn get(&self, path: &str, _query: &[(String, String)]) -> Result<Value> {
        self.call(reqwest::Method::GET, path, &[], None).await
    }

    async fn post(&self, path: &str, form: &[(String, String)], key: Option<&str>) -> Result<Value> {
        self.call(reqwest::Method::POST, path, form, key).await
    }
}

fn pair(key: &str, value: impl ToString) -> (String, String) {
    (key.to_string(), value.to_string())
}

fn string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        // expanded object
        Value::Object(map) => map.get("id").and_then(|id| id.as_str()).map(String::from),
        _ => None,
    }
}

fn int(value: &Value) -> i64 {
    value.as_i64().unwrap_or_default()
}

/// Path segment from a provider id, refusing anything that isn't one
fn id_segment(id: &str) -> Result<&str> {
    if !id.is_empty()
        && id.len() <= 255
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Ok(id)
    } else {
        Err(create_error!(InvalidOperation))
    }
}

fn session_from(value: &Value) -> Result<CheckoutSession> {
    let id = string(&value["id"]).ok_or_else(|| create_error!(ProxyError))?;
    Ok(CheckoutSession {
        id,
        url: string(&value["url"]),
        status: match value["status"].as_str() {
            Some("complete") => CheckoutStatus::Complete,
            Some("expired") => CheckoutStatus::Expired,
            _ => CheckoutStatus::Open,
        },
        payment_status: match value["payment_status"].as_str() {
            Some("paid") => CheckoutPaymentStatus::Paid,
            Some("no_payment_required") => CheckoutPaymentStatus::NoPaymentRequired,
            _ => CheckoutPaymentStatus::Unpaid,
        },
        amount_subtotal: value["amount_subtotal"].as_i64(),
        amount_total: value["amount_total"].as_i64(),
        currency: string(&value["currency"]).map(|c| c.to_lowercase()),
        customer: string(&value["customer"]),
        payment_intent: string(&value["payment_intent"]),
        subscription: string(&value["subscription"]),
        invoice: string(&value["invoice"]),
        livemode: value["livemode"].as_bool().unwrap_or(false),
        client_reference_id: string(&value["client_reference_id"]),
        order_id: string(&value["metadata"]["zrzur_order_id"]).or_else(|| string(&value["metadata"]["zarzour_order_id"])),
        expires_at: int(&value["expires_at"]),
    })
}

fn subscription_from(value: &Value) -> Result<ProviderSubscription> {
    let id = string(&value["id"]).ok_or_else(|| create_error!(ProxyError))?;
    let item = &value["items"]["data"][0];
    // newer API versions keep the period on the item
    let period_end = value["current_period_end"]
        .as_i64()
        .or_else(|| item["current_period_end"].as_i64())
        .unwrap_or_default();
    let period_start = value["current_period_start"]
        .as_i64()
        .or_else(|| item["current_period_start"].as_i64())
        .unwrap_or_default();
    Ok(ProviderSubscription {
        id,
        current_period_start: period_start,
        status: value["status"].as_str().unwrap_or("unknown").to_string(),
        cancel_at_period_end: value["cancel_at_period_end"].as_bool().unwrap_or(false),
        current_period_end: period_end,
        customer: string(&value["customer"]),
        livemode: value["livemode"].as_bool().unwrap_or(false),
        order_id: string(&value["metadata"]["zrzur_order_id"]).or_else(|| string(&value["metadata"]["zarzour_order_id"])),
        quantity: item["quantity"].as_u64().unwrap_or(1) as u32,
    })
}

fn refund_from(value: &Value) -> Result<ProviderRefund> {
    Ok(ProviderRefund {
        id: string(&value["id"]).ok_or_else(|| create_error!(ProxyError))?,
        status: value["status"].as_str().unwrap_or("unknown").to_string(),
        amount: int(&value["amount"]),
        payment_intent: string(&value["payment_intent"]),
    })
}

#[async_trait]
impl PaymentProvider for StripeProvider {
    fn name(&self) -> &'static str {
        "stripe"
    }

    fn livemode(&self) -> bool {
        self.livemode
    }

    async fn create_customer(&self, user_id: &str) -> Result<String> {
        let value = self
            .post(
                "/v1/customers",
                &[pair("metadata[zrzur_user_id]", user_id)],
                Some(&format!("zrzur-customer-{user_id}")),
            )
            .await?;
        string(&value["id"]).ok_or_else(|| create_error!(ProxyError))
    }

    async fn create_checkout(&self, request: &CheckoutRequest) -> Result<CheckoutSession> {
        let line = &request.line;
        let mut form = vec![
            pair(
                "mode",
                match request.mode {
                    CheckoutMode::Payment => "payment",
                    CheckoutMode::Subscription => "subscription",
                },
            ),
            pair("customer", &request.customer_id),
            pair("client_reference_id", &request.order_id),
            pair("success_url", &request.success_url),
            pair("cancel_url", &request.cancel_url),
            pair("expires_at", request.expires_at),
            pair("locale", "auto"),
            pair("metadata[zrzur_order_id]", &request.order_id),
            pair("metadata[zrzur_user_id]", &request.user_id),
            pair("line_items[0][quantity]", line.quantity),
        ];
        match &line.price_id {
            Some(price) => form.push(pair("line_items[0][price]", price)),
            None => {
                form.push(pair("line_items[0][price_data][currency]", &line.currency));
                form.push(pair("line_items[0][price_data][unit_amount]", line.unit_amount));
                form.push(pair("line_items[0][price_data][product_data][name]", &line.name));
                if request.mode == CheckoutMode::Subscription {
                    form.push(pair("line_items[0][price_data][recurring][interval]", "month"));
                }
            }
        }
        match request.mode {
            CheckoutMode::Payment => {
                form.push(pair(
                    "payment_intent_data[metadata][zrzur_order_id]",
                    &request.order_id,
                ));
                form.push(pair("invoice_creation[enabled]", "true"));
            }
            CheckoutMode::Subscription => {
                form.push(pair(
                    "subscription_data[metadata][zrzur_order_id]",
                    &request.order_id,
                ));
            }
        }

        let value = self
            .post(
                "/v1/checkout/sessions",
                &form,
                Some(&format!("zrzur-checkout-{}", request.order_id)),
            )
            .await?;
        session_from(&value)
    }

    async fn fetch_checkout(&self, id: &str) -> Result<CheckoutSession> {
        let value = self
            .get(&format!("/v1/checkout/sessions/{}", id_segment(id)?), &[])
            .await?;
        session_from(&value)
    }

    async fn expire_checkout(&self, id: &str) -> Result<CheckoutSession> {
        let value = self
            .post(
                &format!("/v1/checkout/sessions/{}/expire", id_segment(id)?),
                &[],
                None,
            )
            .await?;
        session_from(&value)
    }

    async fn fetch_subscription(&self, id: &str) -> Result<ProviderSubscription> {
        let value = self
            .get(&format!("/v1/subscriptions/{}", id_segment(id)?), &[])
            .await?;
        subscription_from(&value)
    }

    async fn set_cancel_at_period_end(
        &self,
        id: &str,
        cancel: bool,
    ) -> Result<ProviderSubscription> {
        let value = self
            .post(
                &format!("/v1/subscriptions/{}", id_segment(id)?),
                &[pair("cancel_at_period_end", cancel)],
                None,
            )
            .await?;
        subscription_from(&value)
    }

    async fn cancel_subscription_now(&self, id: &str) -> Result<ProviderSubscription> {
        let value = self
            .call(
                reqwest::Method::DELETE,
                &format!("/v1/subscriptions/{}", id_segment(id)?),
                &[],
                None,
            )
            .await?;
        subscription_from(&value)
    }

    async fn fetch_invoice(&self, id: &str) -> Result<ProviderInvoice> {
        let value = self
            .get(&format!("/v1/invoices/{}", id_segment(id)?), &[])
            .await?;
        Ok(ProviderInvoice {
            id: string(&value["id"]).ok_or_else(|| create_error!(ProxyError))?,
            subscription: string(&value["subscription"])
                .or_else(|| string(&value["parent"]["subscription_details"]["subscription"])),
            billing_reason: string(&value["billing_reason"]),
            status: string(&value["status"]),
            amount_paid: int(&value["amount_paid"]),
            amount_due: int(&value["amount_due"]),
            currency: string(&value["currency"]).unwrap_or_default().to_lowercase(),
            hosted_invoice_url: string(&value["hosted_invoice_url"]),
            payment_intent: string(&value["payment_intent"]),
            customer: string(&value["customer"]),
            livemode: value["livemode"].as_bool().unwrap_or(false),
        })
    }

    async fn fetch_payment(&self, id: &str) -> Result<ProviderPayment> {
        let value = self
            .get(
                &format!(
                    "/v1/payment_intents/{}?expand%5B%5D=latest_charge",
                    id_segment(id)?
                ),
                &[],
            )
            .await?;
        let charge = &value["latest_charge"];
        Ok(ProviderPayment {
            id: string(&value["id"]).ok_or_else(|| create_error!(ProxyError))?,
            status: value["status"].as_str().unwrap_or("unknown").to_string(),
            amount: int(&value["amount"]),
            amount_received: int(&value["amount_received"]),
            currency: string(&value["currency"]).unwrap_or_default().to_lowercase(),
            amount_refunded: int(&charge["amount_refunded"]),
            disputed: charge["disputed"].as_bool().unwrap_or(false),
            receipt_url: string(&charge["receipt_url"]),
            customer: string(&value["customer"]),
            livemode: value["livemode"].as_bool().unwrap_or(false),
            order_id: string(&value["metadata"]["zrzur_order_id"]).or_else(|| string(&value["metadata"]["zarzour_order_id"])),
        })
    }

    async fn fetch_charge_payment(&self, charge_id: &str) -> Result<Option<String>> {
        let value = self
            .get(&format!("/v1/charges/{}", id_segment(charge_id)?), &[])
            .await?;
        Ok(string(&value["payment_intent"]))
    }

    async fn create_refund(
        &self,
        payment_id: &str,
        amount: Option<u32>,
        idempotency_key: &str,
    ) -> Result<ProviderRefund> {
        let mut form = vec![pair("payment_intent", id_segment(payment_id)?)];
        if let Some(amount) = amount {
            form.push(pair("amount", amount));
        }
        let value = self
            .post("/v1/refunds", &form, Some(idempotency_key))
            .await?;
        refund_from(&value)
    }

    async fn fetch_refund(&self, id: &str) -> Result<ProviderRefund> {
        let value = self
            .get(&format!("/v1/refunds/{}", id_segment(id)?), &[])
            .await?;
        refund_from(&value)
    }

    async fn fetch_dispute(&self, id: &str) -> Result<ProviderDispute> {
        let value = self
            .get(&format!("/v1/disputes/{}", id_segment(id)?), &[])
            .await?;
        Ok(ProviderDispute {
            id: string(&value["id"]).ok_or_else(|| create_error!(ProxyError))?,
            status: value["status"].as_str().unwrap_or("unknown").to_string(),
            payment_intent: string(&value["payment_intent"]),
            amount: int(&value["amount"]),
            livemode: value["livemode"].as_bool().unwrap_or(false),
        })
    }

    async fn create_portal(&self, customer_id: &str, return_url: &str) -> Result<String> {
        let value = self
            .post(
                "/v1/billing_portal/sessions",
                &[
                    pair("customer", id_segment(customer_id)?),
                    pair("return_url", return_url),
                ],
                None,
            )
            .await?;
        string(&value["url"]).ok_or_else(|| create_error!(ProxyError))
    }

    async fn verify_webhook(
        &self,
        headers: &WebhookHeaders,
        body: &[u8],
        now: i64,
    ) -> Result<ProviderEvent> {
        if body.len() > MAX_WEBHOOK_BODY {
            return Err(create_error!(PayloadTooLarge));
        }
        let signature = headers
            .get("Stripe-Signature")
            .filter(|value| !value.is_empty() && value.len() <= 2048)
            .ok_or_else(|| create_error!(InvalidCredentials))?;
        verify_stripe_signature(signature, body, &self.webhook_secrets, now).map_err(|error| {
            log::warn!("billing: webhook signature refused: {error:?}");
            create_error!(InvalidCredentials)
        })?;
        let value: Value =
            serde_json::from_slice(body).map_err(|_| create_error!(InvalidOperation))?;
        let object = &value["data"]["object"];
        let event = ProviderEvent {
            id: string(&value["id"]).ok_or_else(|| create_error!(InvalidOperation))?,
            event_type: value["type"].as_str().unwrap_or_default().to_string(),
            livemode: value["livemode"].as_bool().unwrap_or(false),
            created: int(&value["created"]),
            object_id: string(&object["id"]).unwrap_or_default(),
            object_type: object["object"].as_str().unwrap_or_default().to_string(),
        };
        if event.event_type.is_empty() || event.event_type.len() > 128 || event.id.len() > 255 {
            return Err(create_error!(InvalidOperation));
        }
        Ok(event)
    }

    fn event_kind(&self, event_type: &str) -> EventKind {
        match event_type {
            "checkout.session.completed"
            | "checkout.session.async_payment_succeeded"
            | "checkout.session.expired" => EventKind::CheckoutChanged,
            "checkout.session.async_payment_failed" => EventKind::CheckoutFailed,
            "payment_intent.succeeded"
            | "payment_intent.processing"
            | "payment_intent.requires_action"
            | "payment_intent.payment_failed" => EventKind::Payment,
            "invoice.paid" | "invoice.payment_succeeded" => EventKind::InvoicePaid,
            "invoice.payment_failed" => EventKind::InvoiceFailed,
            "customer.subscription.created"
            | "customer.subscription.updated"
            | "customer.subscription.deleted"
            | "customer.subscription.paused"
            | "customer.subscription.resumed" => EventKind::Subscription,
            "charge.refunded" | "charge.refund.updated" => EventKind::ChargeRefund,
            "refund.created" | "refund.updated" | "refund.failed" => EventKind::Refund,
            "charge.dispute.created"
            | "charge.dispute.updated"
            | "charge.dispute.closed"
            | "charge.dispute.funds_withdrawn"
            | "charge.dispute.funds_reinstated" => EventKind::Dispute,
            _ => EventKind::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign_stripe_payload;

    fn provider() -> StripeProvider {
        StripeProvider::new(
            "sk_test_unit".to_string(),
            vec!["whsec_unit".to_string()],
            String::new(),
        )
        .unwrap()
    }

    #[test]
    fn refuses_unknown_keys_and_remote_test_bases() {
        assert!(StripeProvider::new("pk_test_x".into(), vec![], String::new()).is_none());
        assert!(StripeProvider::new("sk_test_x".into(), vec![], "https://evil.example".into())
            .is_none());
        // a live key never talks to a local test server
        assert!(StripeProvider::new(
            "sk_live_x".into(),
            vec![],
            "http://127.0.0.1:12111".into()
        )
        .is_none());
        assert!(StripeProvider::new(
            "sk_test_x".into(),
            vec![],
            "http://127.0.0.1:12111".into()
        )
        .is_some());
    }

    #[test]
    fn debug_hides_keys() {
        let text = format!("{:?}", provider());
        assert!(!text.contains("sk_test_unit"));
        assert!(!text.contains("whsec_unit"));
    }

    fn signed(header: String) -> WebhookHeaders {
        WebhookHeaders(vec![("stripe-signature".to_string(), header)])
    }

    #[tokio::test]
    async fn reads_signed_event() {
        let body = br#"{"id":"evt_1","type":"invoice.paid","livemode":false,"created":5,"data":{"object":{"id":"in_1","object":"invoice"}}}"#;
        let header = sign_stripe_payload(body, "whsec_unit", 1_000);
        let event = provider()
            .verify_webhook(&signed(header), body, 1_000)
            .await
            .unwrap();
        assert_eq!(event.id, "evt_1");
        assert_eq!(event.event_type, "invoice.paid");
        assert_eq!(event.object_id, "in_1");
        assert!(!event.livemode);
    }

    #[tokio::test]
    async fn refuses_oversized_body() {
        let body = vec![b' '; MAX_WEBHOOK_BODY + 1];
        let header = sign_stripe_payload(&body, "whsec_unit", 1_000);
        assert!(provider()
            .verify_webhook(&signed(header), &body, 1_000)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn refuses_missing_signature_header() {
        let body = br#"{"id":"evt_1","type":"invoice.paid"}"#;
        assert!(provider()
            .verify_webhook(&WebhookHeaders::default(), body, 1_000)
            .await
            .is_err());
    }

    #[test]
    fn maps_event_kinds() {
        let p = provider();
        assert_eq!(p.event_kind("checkout.session.completed"), EventKind::CheckoutChanged);
        assert_eq!(p.event_kind("invoice.payment_failed"), EventKind::InvoiceFailed);
        assert_eq!(p.event_kind("charge.dispute.closed"), EventKind::Dispute);
        assert_eq!(p.event_kind("something.else"), EventKind::Other);
    }

    #[test]
    fn id_segments_are_plain() {
        assert!(id_segment("cs_test_abc123").is_ok());
        for bad in ["", "../v1/customers", "cs_1?expand=x", "a/b", "a b"] {
            assert!(id_segment(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_subscription_period_from_item_too() {
        let value = serde_json::json!({
            "id": "sub_1", "status": "active", "cancel_at_period_end": false,
            "items": { "data": [{ "quantity": 3, "current_period_end": 1_900_000_000 }] },
            "metadata": { "zrzur_order_id": "o1" }
        });
        let sub = subscription_from(&value).unwrap();
        assert_eq!(sub.current_period_end, 1_900_000_000);
        assert_eq!(sub.quantity, 3);
        assert_eq!(sub.order_id.as_deref(), Some("o1"));
    }
}
