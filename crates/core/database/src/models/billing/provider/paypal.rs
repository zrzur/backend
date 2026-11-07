//! PayPal (REST API). One-time payments are PayPal orders (Orders v2,
//! captured by the server once the buyer approved); monthly products are
//! PayPal subscriptions (Subscriptions v1) on plans made in the PayPal
//! dashboard. PayPal has no customer object: a member's customer id is the
//! local `paypal:<user id>`. Both the order and the subscription carry
//! `custom_id` = `<zrzur order id>|<customer id>`.
//!
//! Webhooks are verified by PayPal itself (`verify-webhook-signature`) on
//! the exact bytes received; as with Stripe the event is only a hint and the
//! object is fetched again before anything changes.

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_result::{create_error, Result};
use serde_json::{json, Value};
use std::sync::Mutex;

use super::{
    CheckoutMode, CheckoutPaymentStatus, CheckoutRequest, CheckoutSession, CheckoutStatus,
    EventKind, PaymentProvider, ProviderDispute, ProviderEvent, ProviderInvoice, ProviderPayment,
    ProviderRefund, ProviderSubscription, WebhookHeaders, MAX_WEBHOOK_BODY,
};

const LIVE_BASE: &str = "https://api-m.paypal.com";
const SANDBOX_BASE: &str = "https://api-m.sandbox.paypal.com";

/// How old a webhook may be (seconds)
const WEBHOOK_TOLERANCE: i64 = 600;

/// How long a buyer has to approve (PayPal keeps approval links for 3 h)
const APPROVAL_SECONDS: i64 = 3 * 3600;

pub struct PayPalProvider {
    client_id: String,
    secret: String,
    webhook_id: String,
    base: String,
    livemode: bool,
    /// Plan ids of the monthly products
    plus_plan: String,
    supercharge_plan: String,
    http: reqwest::Client,
    token: Mutex<Option<(String, i64)>>,
}

impl std::fmt::Debug for PayPalProvider {
    // never print the keys
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PayPalProvider")
            .field("base", &self.base)
            .field("livemode", &self.livemode)
            .finish()
    }
}

impl PayPalProvider {
    /// `api_base` is only honoured for local tests in the sandbox
    pub fn new(
        client_id: String,
        secret: String,
        webhook_id: String,
        livemode: bool,
        api_base: String,
        plus_plan: String,
        supercharge_plan: String,
    ) -> Option<Self> {
        if client_id.is_empty() || secret.is_empty() || webhook_id.is_empty() {
            return None;
        }
        let base = if api_base.is_empty() {
            if livemode { LIVE_BASE } else { SANDBOX_BASE }.to_string()
        } else if !livemode && api_base.starts_with("http://127.0.0.1:") {
            api_base.trim_end_matches('/').to_string()
        } else {
            log::error!("billing: paypal api_base is only allowed locally in the sandbox");
            return None;
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        Some(Self {
            client_id,
            secret,
            webhook_id,
            base,
            livemode,
            plus_plan,
            supercharge_plan,
            http,
            token: Mutex::new(None),
        })
    }

    /// OAuth access token, kept until shortly before it expires
    async fn access_token(&self) -> Result<String> {
        let now = now_s();
        if let Some((token, expires)) = self.token.lock().ok().and_then(|cached| cached.clone()) {
            if expires > now + 60 {
                return Ok(token);
            }
        }
        let response = self
            .http
            .post(format!("{}/v1/oauth2/token", self.base))
            .basic_auth(&self.client_id, Some(&self.secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .map_err(|error| {
                log::warn!("billing: paypal token: request failed: {}", error.without_url());
                create_error!(ProxyError)
            })?;
        let status = response.status();
        let body: Value = response.json().await.map_err(|_| create_error!(ProxyError))?;
        let Some(token) = body["access_token"].as_str().filter(|t| !t.is_empty()) else {
            log::warn!("billing: paypal token refused ({status})");
            return Err(create_error!(ProxyError));
        };
        let expires = now + body["expires_in"].as_i64().unwrap_or(300);
        // two calls at once may both fetch a token: harmless, both are valid
        if let Ok(mut cached) = self.token.lock() {
            *cached = Some((token.to_string(), expires));
        }
        Ok(token.to_string())
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<String>,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let token = self.access_token().await?;
        let mut request = self
            .http
            .request(method.clone(), format!("{}{}", self.base, path))
            .bearer_auth(token)
            .header("Prefer", "return=representation");
        if let Some(id) = request_id {
            // PayPal's idempotency key: a repeated request does it once
            request = request.header("PayPal-Request-Id", id);
        }
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body);
        }
        let response = request.send().await.map_err(|error| {
            log::warn!("billing: paypal {path}: request failed: {}", error.without_url());
            create_error!(ProxyError)
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|_| create_error!(ProxyError))?;
        // some calls (cancel, suspend) answer 204 without a body
        let value: Value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).map_err(|_| {
                log::warn!("billing: paypal {path}: unreadable answer ({status})");
                create_error!(ProxyError)
            })?
        };
        if status.is_success() {
            return Ok(value);
        }
        // name only: details can echo request data
        let name = value["name"].as_str().unwrap_or("unknown");
        log::warn!("billing: paypal {path}: {status} {name}");
        if status == reqwest::StatusCode::NOT_FOUND {
            Err(create_error!(NotFound))
        } else if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
            || status == reqwest::StatusCode::BAD_REQUEST
        {
            Err(create_error!(InvalidOperation))
        } else {
            Err(create_error!(ProxyError))
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.call(reqwest::Method::GET, path, None, None).await
    }

    async fn post(&self, path: &str, body: Value, request_id: Option<&str>) -> Result<Value> {
        self.call(reqwest::Method::POST, path, Some(body.to_string()), request_id)
            .await
    }

    /// Price of a configured plan × quantity in minor units, and its
    /// currency; None for any other plan (never accepted)
    async fn plan_amount(&self, plan_id: &str, quantity: u32) -> Option<(i64, String)> {
        if plan_id.is_empty() || (plan_id != self.plus_plan && plan_id != self.supercharge_plan) {
            return None;
        }
        let plan = self.get(&format!("/v1/billing/plans/{}", id_segment(plan_id).ok()?)).await.ok()?;
        let cycle = plan["billing_cycles"]
            .as_array()?
            .iter()
            .find(|cycle| cycle["tenure_type"] == "REGULAR")?;
        let interval = &cycle["frequency"];
        if interval["interval_unit"] != "MONTH" || interval["interval_count"].as_i64() != Some(1) {
            log::error!("billing: paypal plan {plan_id} is not monthly");
            return None;
        }
        let price = &cycle["pricing_scheme"]["fixed_price"];
        let unit = minor_units(price["value"].as_str()?)?;
        Some((unit * quantity as i64, price["currency_code"].as_str()?.to_lowercase()))
    }

    fn session_from_order(&self, value: &Value) -> Result<CheckoutSession> {
        let id = value["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| create_error!(ProxyError))?
            .to_string();
        let unit = &value["purchase_units"][0];
        let (order_id, customer) = split_custom(unit["custom_id"].as_str());
        let capture = unit["payments"]["captures"]
            .as_array()
            .and_then(|captures| captures.first())
            .cloned()
            .unwrap_or(Value::Null);
        let capture_status = capture["status"].as_str().unwrap_or_default();
        let money = if capture.is_null() { &unit["amount"] } else { &capture["amount"] };
        let amount = money["value"].as_str().and_then(minor_units);
        let created = parse_time(value["create_time"].as_str()).unwrap_or_else(now_s);
        Ok(CheckoutSession {
            url: approval_link(value, "payer-action").or_else(|| approval_link(value, "approve")),
            status: match value["status"].as_str() {
                Some("COMPLETED") => CheckoutStatus::Complete,
                Some("VOIDED") => CheckoutStatus::Expired,
                _ => CheckoutStatus::Open,
            },
            payment_status: match capture_status {
                "COMPLETED" | "PARTIALLY_REFUNDED" | "REFUNDED" => CheckoutPaymentStatus::Paid,
                _ => CheckoutPaymentStatus::Unpaid,
            },
            amount_subtotal: amount,
            amount_total: amount,
            currency: money["currency_code"].as_str().map(str::to_lowercase),
            customer,
            payment_intent: capture["id"].as_str().map(String::from),
            subscription: None,
            invoice: None,
            livemode: self.livemode,
            client_reference_id: order_id.clone(),
            order_id,
            expires_at: created + APPROVAL_SECONDS,
            id,
        })
    }

    async fn session_from_subscription(&self, value: &Value) -> Result<CheckoutSession> {
        let id = value["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| create_error!(ProxyError))?
            .to_string();
        let (order_id, customer) = split_custom(value["custom_id"].as_str());
        let quantity = quantity_of(value);
        let paid = value["billing_info"]["last_payment"]["amount"]["value"].is_string();
        let status = value["status"].as_str().unwrap_or_default();
        // the amount is the configured plan's price, fetched from PayPal
        let plan = self
            .plan_amount(value["plan_id"].as_str().unwrap_or_default(), quantity)
            .await;
        let created = parse_time(value["create_time"].as_str()).unwrap_or_else(now_s);
        Ok(CheckoutSession {
            url: approval_link(value, "approve"),
            status: match status {
                "ACTIVE" => CheckoutStatus::Complete,
                // ended without ever running
                "CANCELLED" | "EXPIRED" if !paid => CheckoutStatus::Expired,
                "CANCELLED" | "EXPIRED" | "SUSPENDED" => CheckoutStatus::Complete,
                _ => CheckoutStatus::Open,
            },
            payment_status: if paid {
                CheckoutPaymentStatus::Paid
            } else {
                CheckoutPaymentStatus::Unpaid
            },
            amount_subtotal: plan.as_ref().map(|(amount, _)| *amount),
            amount_total: plan.as_ref().map(|(amount, _)| *amount),
            currency: plan.map(|(_, currency)| currency),
            customer,
            payment_intent: None,
            subscription: Some(id.clone()),
            invoice: None,
            livemode: self.livemode,
            client_reference_id: order_id.clone(),
            order_id,
            expires_at: created + APPROVAL_SECONDS,
            id,
        })
    }
}

#[async_trait]
impl PaymentProvider for PayPalProvider {
    fn name(&self) -> &'static str {
        "paypal"
    }

    fn livemode(&self) -> bool {
        self.livemode
    }

    async fn create_customer(&self, user_id: &str) -> Result<String> {
        // PayPal keeps no customers for merchants: a stable local id
        Ok(format!("paypal:{user_id}"))
    }

    async fn create_checkout(&self, request: &CheckoutRequest) -> Result<CheckoutSession> {
        let custom = format!("{}|{}", request.order_id, request.customer_id);
        if custom.len() > 127 {
            return Err(create_error!(InvalidOperation));
        }
        match request.mode {
            CheckoutMode::Payment => {
                let currency = request.line.currency.to_uppercase();
                let unit = decimal(request.line.unit_amount as i64);
                let total = decimal(request.line.unit_amount as i64 * request.line.quantity as i64);
                let body = json!({
                    "intent": "CAPTURE",
                    "purchase_units": [{
                        "reference_id": "zrzur",
                        "custom_id": custom,
                        "invoice_id": request.order_id,
                        "description": request.line.name,
                        "amount": {
                            "currency_code": currency,
                            "value": total,
                            "breakdown": { "item_total": { "currency_code": currency, "value": total } }
                        },
                        "items": [{
                            "name": request.line.name,
                            "quantity": request.line.quantity.to_string(),
                            "category": "DIGITAL_GOODS",
                            "unit_amount": { "currency_code": currency, "value": unit }
                        }]
                    }],
                    "payment_source": { "paypal": { "experience_context": {
                        "brand_name": "Zrzur",
                        "shipping_preference": "NO_SHIPPING",
                        "user_action": "PAY_NOW",
                        "return_url": request.success_url,
                        "cancel_url": request.cancel_url
                    } } }
                });
                let value = self
                    .post("/v2/checkout/orders", body, Some(&request.order_id))
                    .await?;
                self.session_from_order(&value)
            }
            CheckoutMode::Subscription => {
                // plans are made in the PayPal dashboard: no inline prices
                let Some(plan) = request.line.price_id.clone().filter(|p| !p.is_empty()) else {
                    log::error!("billing: paypal needs a plan id for monthly products");
                    return Err(create_error!(FeatureDisabled {
                        feature: "billing_checkout".to_string()
                    }));
                };
                let body = json!({
                    "plan_id": plan,
                    "quantity": request.line.quantity.to_string(),
                    "custom_id": custom,
                    "application_context": {
                        "brand_name": "Zrzur",
                        "shipping_preference": "NO_SHIPPING",
                        "user_action": "SUBSCRIBE_NOW",
                        "return_url": request.success_url,
                        "cancel_url": request.cancel_url
                    }
                });
                let value = self
                    .post("/v1/billing/subscriptions", body, Some(&request.order_id))
                    .await?;
                self.session_from_subscription(&value).await
            }
        }
    }

    async fn fetch_checkout(&self, id: &str) -> Result<CheckoutSession> {
        let id = id_segment(id)?;
        if is_subscription(id) {
            let value = self.get(&format!("/v1/billing/subscriptions/{id}")).await?;
            self.session_from_subscription(&value).await
        } else {
            let value = self.get(&format!("/v2/checkout/orders/{id}")).await?;
            self.session_from_order(&value)
        }
    }

    async fn complete_checkout(&self, id: &str) -> Result<CheckoutSession> {
        let session = self.fetch_checkout(id).await?;
        if is_subscription(id) {
            // PayPal activates subscriptions itself after the first payment
            return Ok(session);
        }
        let id = id_segment(id)?;
        let value = self.get(&format!("/v2/checkout/orders/{id}")).await?;
        if value["status"] != "APPROVED" {
            return Ok(session);
        }
        // the buyer approved: take the money, once (same request id)
        let captured = self
            .post(
                &format!("/v2/checkout/orders/{id}/capture"),
                json!({}),
                Some(&format!("capture-{id}")),
            )
            .await?;
        self.session_from_order(&captured)
    }

    async fn expire_checkout(&self, id: &str) -> Result<CheckoutSession> {
        let session = self.fetch_checkout(id).await?;
        if is_subscription(id) {
            if session.status == CheckoutStatus::Open {
                // not approved yet: PayPal lets it lapse; never activated here
                return Ok(CheckoutSession {
                    status: CheckoutStatus::Expired,
                    ..session
                });
            }
            return Err(create_error!(InvalidOperation));
        }
        let value = self.get(&format!("/v2/checkout/orders/{}", id_segment(id)?)).await?;
        match value["status"].as_str() {
            // approved or paid: the caller asks what happened instead
            Some("APPROVED") | Some("COMPLETED") => Err(create_error!(InvalidOperation)),
            // an order that is never captured takes no money
            _ => Ok(CheckoutSession {
                status: CheckoutStatus::Expired,
                ..session
            }),
        }
    }

    async fn fetch_subscription(&self, id: &str) -> Result<ProviderSubscription> {
        let value = self
            .get(&format!("/v1/billing/subscriptions/{}", id_segment(id)?))
            .await?;
        Ok(subscription_from(&value, self.livemode, now_s()))
    }

    async fn set_cancel_at_period_end(
        &self,
        id: &str,
        cancel: bool,
    ) -> Result<ProviderSubscription> {
        let current = self.fetch_subscription(id).await?;
        if cancel {
            if !current.cancel_at_period_end {
                // PayPal ends renewal for good; the paid month still counts
                self.post(
                    &format!("/v1/billing/subscriptions/{}/cancel", id_segment(id)?),
                    json!({ "reason": "Renewal cancelled in Zrzur" }),
                    Some(&format!("cancel-{id}")),
                )
                .await?;
            }
            return self.fetch_subscription(id).await;
        }
        if current.cancel_at_period_end {
            // a cancelled PayPal subscription can't be resumed: buy again
            // once it ends
            return Err(create_error!(InvalidOperation));
        }
        Ok(current)
    }

    async fn cancel_subscription_now(&self, id: &str) -> Result<ProviderSubscription> {
        let current = self.fetch_subscription(id).await?;
        if !current.cancel_at_period_end && current.status != "canceled" {
            self.post(
                &format!("/v1/billing/subscriptions/{}/cancel", id_segment(id)?),
                json!({ "reason": "Ended by Zrzur" }),
                Some(&format!("cancel-{id}")),
            )
            .await?;
        }
        // ended now: no paid time is kept
        let mut sub = self.fetch_subscription(id).await?;
        sub.status = "canceled".to_string();
        Ok(sub)
    }

    async fn fetch_invoice(&self, _id: &str) -> Result<ProviderInvoice> {
        // renewals arrive as subscription changes; PayPal sends no invoices
        Err(create_error!(NotFound))
    }

    async fn fetch_payment(&self, id: &str) -> Result<ProviderPayment> {
        let id = id_segment(id)?;
        match self.get(&format!("/v2/payments/captures/{id}")).await {
            Ok(capture) => {
                let amount = capture["amount"]["value"].as_str().and_then(minor_units).unwrap_or_default();
                let status = capture["status"].as_str().unwrap_or_default();
                let refunded = match status {
                    "REFUNDED" => amount,
                    "PARTIALLY_REFUNDED" => {
                        // the order lists its refunds
                        let order_id = capture["supplementary_data"]["related_ids"]["order_id"]
                            .as_str()
                            .unwrap_or_default();
                        match id_segment(order_id) {
                            Ok(order_id) => self
                                .get(&format!("/v2/checkout/orders/{order_id}"))
                                .await
                                .map(|order| refunded_total(&order))
                                .unwrap_or_default(),
                            Err(_) => 0,
                        }
                    }
                    _ => 0,
                };
                let (order_id, customer) = split_custom(capture["custom_id"].as_str());
                Ok(ProviderPayment {
                    id: id.to_string(),
                    status: payment_status(status).to_string(),
                    amount,
                    amount_received: if payment_status(status) == "succeeded" { amount } else { 0 },
                    currency: capture["amount"]["currency_code"].as_str().unwrap_or_default().to_lowercase(),
                    amount_refunded: refunded,
                    disputed: false,
                    receipt_url: None,
                    customer,
                    livemode: self.livemode,
                    order_id,
                })
            }
            // subscription payments are "sales"
            Err(error) if matches!(error.error_type, revolt_result::ErrorType::NotFound) => {
                let sale = self.get(&format!("/v1/payments/sale/{id}")).await?;
                let amount = sale["amount"]["total"].as_str().and_then(minor_units).unwrap_or_default();
                let state = sale["state"].as_str().unwrap_or_default();
                Ok(ProviderPayment {
                    id: id.to_string(),
                    status: match state {
                        "completed" | "refunded" | "partially_refunded" => "succeeded",
                        "pending" => "processing",
                        _ => "failed",
                    }
                    .to_string(),
                    amount,
                    amount_received: amount,
                    currency: sale["amount"]["currency"].as_str().unwrap_or_default().to_lowercase(),
                    // PayPal gives no total for partial refunds of sales:
                    // only a full refund is taken as refunded
                    amount_refunded: if state == "refunded" { amount } else { 0 },
                    disputed: false,
                    receipt_url: None,
                    customer: None,
                    livemode: self.livemode,
                    order_id: None,
                })
            }
            Err(error) => Err(error),
        }
    }

    async fn fetch_charge_payment(&self, charge_id: &str) -> Result<Option<String>> {
        // PayPal has no separate charge: the capture or sale is the payment
        Ok(Some(id_segment(charge_id)?.to_string()))
    }

    async fn create_refund(
        &self,
        payment_id: &str,
        amount: Option<u32>,
        idempotency_key: &str,
    ) -> Result<ProviderRefund> {
        let payment = self.fetch_payment(payment_id).await?;
        let id = id_segment(payment_id)?;
        let value = match self
            .get(&format!("/v2/payments/captures/{id}"))
            .await
        {
            Ok(_) => {
                let body = match amount {
                    Some(amount) => json!({ "amount": {
                        "value": decimal(amount as i64),
                        "currency_code": payment.currency.to_uppercase()
                    } }),
                    None => json!({}),
                };
                self.post(&format!("/v2/payments/captures/{id}/refund"), body, Some(idempotency_key))
                    .await?
            }
            Err(_) => {
                let body = match amount {
                    Some(amount) => json!({ "amount": {
                        "total": decimal(amount as i64),
                        "currency": payment.currency.to_uppercase()
                    } }),
                    None => json!({}),
                };
                self.post(&format!("/v1/payments/sale/{id}/refund"), body, Some(idempotency_key))
                    .await?
            }
        };
        Ok(refund_from(&value, payment_id))
    }

    async fn fetch_refund(&self, id: &str) -> Result<ProviderRefund> {
        let value = self
            .get(&format!("/v2/payments/refunds/{}", id_segment(id)?))
            .await?;
        // the capture is the "up" link
        let capture = value["links"]
            .as_array()
            .and_then(|links| links.iter().find(|link| link["rel"] == "up"))
            .and_then(|link| link["href"].as_str())
            .and_then(|href| href.rsplit('/').next())
            .unwrap_or_default()
            .to_string();
        Ok(refund_from(&value, &capture))
    }

    async fn fetch_dispute(&self, id: &str) -> Result<ProviderDispute> {
        let value = self
            .get(&format!("/v1/customer/disputes/{}", id_segment(id)?))
            .await?;
        let outcome = value["dispute_outcome"]["outcome_code"].as_str().unwrap_or_default();
        Ok(ProviderDispute {
            id: id.to_string(),
            status: match (value["status"].as_str().unwrap_or_default(), outcome) {
                ("RESOLVED", "RESOLVED_BUYER_FAVOUR") => "lost",
                ("RESOLVED", _) => "won",
                ("UNDER_REVIEW", _) => "under_review",
                _ => "needs_response",
            }
            .to_string(),
            payment_intent: value["disputed_transactions"][0]["seller_transaction_id"]
                .as_str()
                .map(String::from),
            amount: value["dispute_amount"]["value"].as_str().and_then(minor_units).unwrap_or_default(),
            livemode: self.livemode,
        })
    }

    async fn create_portal(&self, _customer_id: &str, _return_url: &str) -> Result<String> {
        // PayPal's own page for automatic payments
        Ok(if self.livemode {
            "https://www.paypal.com/myaccount/autopay/".to_string()
        } else {
            "https://www.sandbox.paypal.com/myaccount/autopay/".to_string()
        })
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
        let header = |name: &str| {
            headers
                .get(name)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| create_error!(InvalidCredentials))
        };
        let transmission_id = header("Paypal-Transmission-Id")?;
        let transmission_time = header("Paypal-Transmission-Time")?;
        let signature = header("Paypal-Transmission-Sig")?;
        let cert_url = header("Paypal-Cert-Url")?;
        let algo = header("Paypal-Auth-Algo")?;
        // old deliveries are refused before asking PayPal (replays)
        let sent = parse_time(Some(transmission_time)).ok_or_else(|| create_error!(InvalidCredentials))?;
        if (now - sent).abs() > WEBHOOK_TOLERANCE {
            return Err(create_error!(InvalidCredentials));
        }
        let raw = std::str::from_utf8(body).map_err(|_| create_error!(InvalidOperation))?;
        let event: Value = serde_json::from_str(raw).map_err(|_| create_error!(InvalidOperation))?;
        // PayPal checks its signature on the event exactly as it was sent
        let request = format!(
            r#"{{"auth_algo":{},"cert_url":{},"transmission_id":{},"transmission_sig":{},"transmission_time":{},"webhook_id":{},"webhook_event":{}}}"#,
            Value::from(algo),
            Value::from(cert_url),
            Value::from(transmission_id),
            Value::from(signature),
            Value::from(transmission_time),
            Value::from(self.webhook_id.as_str()),
            raw
        );
        let answer = self
            .call(
                reqwest::Method::POST,
                "/v1/notifications/verify-webhook-signature",
                Some(request),
                None,
            )
            .await
            .map_err(|_| create_error!(InvalidCredentials))?;
        if answer["verification_status"] != "SUCCESS" {
            log::warn!("billing: paypal webhook signature refused");
            return Err(create_error!(InvalidCredentials));
        }

        let event_type = event["event_type"].as_str().unwrap_or_default().to_string();
        let resource = &event["resource"];
        // the object each kind is handled by
        let object_id = match event_type.as_str() {
            t if t.starts_with("PAYMENT.SALE.REFUNDED") || t.starts_with("PAYMENT.SALE.REVERSED") => {
                resource["sale_id"].as_str()
            }
            t if t.starts_with("PAYMENT.SALE.") => resource["billing_agreement_id"].as_str(),
            t if t.starts_with("CUSTOMER.DISPUTE.") => resource["dispute_id"].as_str(),
            _ => resource["id"].as_str(),
        }
        .unwrap_or_default()
        .to_string();
        let event = ProviderEvent {
            id: event["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| create_error!(InvalidOperation))?
                .to_string(),
            event_type,
            livemode: self.livemode,
            created: parse_time(event["create_time"].as_str()).unwrap_or(now),
            object_id,
            object_type: event["resource_type"].as_str().unwrap_or_default().to_string(),
        };
        if event.event_type.is_empty() || event.event_type.len() > 128 || event.id.len() > 255 {
            return Err(create_error!(InvalidOperation));
        }
        Ok(event)
    }

    fn event_kind(&self, event_type: &str) -> EventKind {
        match event_type {
            "CHECKOUT.ORDER.APPROVED" | "CHECKOUT.ORDER.COMPLETED" | "CHECKOUT.ORDER.VOIDED" => {
                EventKind::CheckoutChanged
            }
            // the subscription is the checkout of a monthly product
            "BILLING.SUBSCRIPTION.ACTIVATED" => EventKind::CheckoutChanged,
            "PAYMENT.CAPTURE.COMPLETED"
            | "PAYMENT.CAPTURE.PENDING"
            | "PAYMENT.CAPTURE.DENIED"
            | "PAYMENT.CAPTURE.DECLINED" => EventKind::Payment,
            "PAYMENT.CAPTURE.REFUNDED" | "PAYMENT.CAPTURE.REVERSED" => EventKind::Refund,
            "PAYMENT.SALE.REFUNDED" | "PAYMENT.SALE.REVERSED" => EventKind::ChargeRefund,
            "PAYMENT.SALE.COMPLETED"
            | "PAYMENT.SALE.DENIED"
            | "BILLING.SUBSCRIPTION.UPDATED"
            | "BILLING.SUBSCRIPTION.CANCELLED"
            | "BILLING.SUBSCRIPTION.EXPIRED"
            | "BILLING.SUBSCRIPTION.SUSPENDED"
            | "BILLING.SUBSCRIPTION.RE-ACTIVATED"
            | "BILLING.SUBSCRIPTION.PAYMENT.FAILED" => EventKind::Subscription,
            "CUSTOMER.DISPUTE.CREATED" | "CUSTOMER.DISPUTE.UPDATED" | "CUSTOMER.DISPUTE.RESOLVED" => {
                EventKind::Dispute
            }
            _ => EventKind::Other,
        }
    }
}

// --- helpers (pure, unit tested) ---

fn now_s() -> i64 {
    crate::epoch_ms(Timestamp::now_utc()) / 1000
}

/// PayPal subscription ids start with `I-`; order ids never do
fn is_subscription(id: &str) -> bool {
    id.starts_with("I-")
}

/// Path segment from a PayPal id, refusing anything that isn't one
fn id_segment(id: &str) -> Result<&str> {
    if !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(id)
    } else {
        Err(create_error!(InvalidOperation))
    }
}

/// `<order>|<customer>` back into its parts
fn split_custom(custom: Option<&str>) -> (Option<String>, Option<String>) {
    match custom.and_then(|custom| custom.split_once('|')) {
        Some((order, customer)) if !order.is_empty() && !customer.is_empty() => {
            (Some(order.to_string()), Some(customer.to_string()))
        }
        _ => (None, None),
    }
}

/// "4.99" → 499. Only plain amounts with at most two decimals.
fn minor_units(value: &str) -> Option<i64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || fraction.len() > 2
        || !whole.chars().all(|c| c.is_ascii_digit())
        || !fraction.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let cents = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction.parse::<i64>().ok()?,
    };
    whole.parse::<i64>().ok()?.checked_mul(100)?.checked_add(cents)
}

/// 499 → "4.99"
fn decimal(minor: i64) -> String {
    format!("{}.{:02}", minor / 100, minor % 100)
}

fn parse_time(value: Option<&str>) -> Option<i64> {
    let parsed = Timestamp::parse(value?)?;
    Some(crate::epoch_ms(parsed) / 1000)
}

fn approval_link(value: &Value, rel: &str) -> Option<String> {
    value["links"]
        .as_array()?
        .iter()
        .find(|link| link["rel"] == rel)?["href"]
        .as_str()
        .map(String::from)
}

fn quantity_of(value: &Value) -> u32 {
    value["quantity"]
        .as_str()
        .and_then(|q| q.parse().ok())
        .or_else(|| value["quantity"].as_u64().map(|q| q as u32))
        .unwrap_or(1)
}

/// One calendar month after a time (PayPal plans renew by calendar month)
fn add_month(seconds: i64) -> i64 {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let d = d.min(days_in_month(y, m));
    days_from_civil(y, m, d) * 86_400 + rest
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

// Howard Hinnant's civil date algorithms
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A PayPal subscription in Zrzur's terms. A cancelled one keeps running
/// until the end of the month that was paid for, then ends; a suspended
/// one (payment failed) is past due.
fn subscription_from(value: &Value, livemode: bool, now: i64) -> ProviderSubscription {
    let last_payment = parse_time(value["billing_info"]["last_payment"]["time"].as_str());
    let next_billing = parse_time(value["billing_info"]["next_billing_time"].as_str());
    let paid_through = last_payment.map(add_month);
    let (order_id, customer) = split_custom(value["custom_id"].as_str());
    let (status, cancel_at_period_end, start, end) = match value["status"].as_str() {
        Some("ACTIVE") => (
            "active",
            false,
            last_payment.unwrap_or(0),
            next_billing.or(paid_through).unwrap_or(0),
        ),
        Some("CANCELLED") | Some("EXPIRED") => match paid_through {
            Some(end) if end > now => ("active", true, last_payment.unwrap_or(0), end),
            _ => ("canceled", true, last_payment.unwrap_or(0), paid_through.unwrap_or(0)),
        },
        Some("SUSPENDED") => {
            let end = paid_through.unwrap_or(0);
            ("past_due", false, end, add_month(end))
        }
        Some("APPROVAL_PENDING") | Some("APPROVED") => ("incomplete", false, 0, 0),
        _ => ("canceled", false, 0, 0),
    };
    ProviderSubscription {
        id: value["id"].as_str().unwrap_or_default().to_string(),
        status: status.to_string(),
        cancel_at_period_end,
        current_period_start: start,
        current_period_end: end,
        customer,
        livemode,
        order_id,
        quantity: quantity_of(value),
    }
}

fn payment_status(capture_status: &str) -> &'static str {
    match capture_status {
        "COMPLETED" | "PARTIALLY_REFUNDED" | "REFUNDED" => "succeeded",
        "PENDING" => "processing",
        _ => "failed",
    }
}

fn refunded_total(order: &Value) -> i64 {
    order["purchase_units"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|unit| unit["payments"]["refunds"].as_array().cloned().unwrap_or_default())
        .filter(|refund| refund["status"] == "COMPLETED")
        .filter_map(|refund| refund["amount"]["value"].as_str().and_then(minor_units))
        .sum()
}

fn refund_from(value: &Value, payment_id: &str) -> ProviderRefund {
    let status = value["status"]
        .as_str()
        .or_else(|| value["state"].as_str())
        .unwrap_or_default();
    ProviderRefund {
        id: value["id"].as_str().unwrap_or_default().to_string(),
        status: match status {
            "COMPLETED" | "completed" => "succeeded",
            "PENDING" | "pending" => "pending",
            "CANCELLED" => "canceled",
            _ => "failed",
        }
        .to_string(),
        amount: value["amount"]["value"]
            .as_str()
            .or_else(|| value["amount"]["total"].as_str())
            .and_then(minor_units)
            .unwrap_or_default(),
        payment_intent: (!payment_id.is_empty()).then(|| payment_id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> PayPalProvider {
        PayPalProvider::new(
            "client".into(),
            "secret".into(),
            "WH-1".into(),
            false,
            String::new(),
            "P-PLUS".into(),
            "P-WINGS".into(),
        )
        .unwrap()
    }

    #[test]
    fn debug_hides_keys() {
        let text = format!("{:?}", provider());
        assert!(!text.contains("secret"));
        assert!(!text.contains("client"));
    }

    #[test]
    fn money_round_trips_and_refuses_odd_amounts() {
        assert_eq!(minor_units("4.99"), Some(499));
        assert_eq!(minor_units("10"), Some(1000));
        assert_eq!(minor_units("2.5"), Some(250));
        for bad in ["", "1.234", "-1.00", "1e3", " 1.00", "1,00", "."] {
            assert_eq!(minor_units(bad), None, "{bad}");
        }
        assert_eq!(decimal(499), "4.99");
        assert_eq!(decimal(5), "0.05");
    }

    #[test]
    fn ids_are_plain_segments() {
        assert!(id_segment("5O190127TN364715T").is_ok());
        assert!(id_segment("I-BW452GLLEP1G").is_ok());
        for bad in ["", "../v1/x", "a/b", "a?b", "a b"] {
            assert!(id_segment(bad).is_err(), "{bad}");
        }
        assert!(is_subscription("I-BW452GLLEP1G"));
        assert!(!is_subscription("5O190127TN364715T"));
    }

    #[test]
    fn custom_id_carries_order_and_customer() {
        assert_eq!(
            split_custom(Some("01ABC-key|paypal:01ABC")),
            (Some("01ABC-key".into()), Some("paypal:01ABC".into()))
        );
        assert_eq!(split_custom(Some("no-separator")), (None, None));
        assert_eq!(split_custom(Some("|x")), (None, None));
        assert_eq!(split_custom(None), (None, None));
    }

    #[test]
    fn months_follow_the_calendar() {
        let t = |s: &str| parse_time(Some(s)).unwrap();
        assert_eq!(add_month(t("2026-01-31T10:00:00Z")), t("2026-02-28T10:00:00Z"));
        assert_eq!(add_month(t("2028-01-31T10:00:00Z")), t("2028-02-29T10:00:00Z"));
        assert_eq!(add_month(t("2026-12-15T00:00:00Z")), t("2027-01-15T00:00:00Z"));
    }

    #[test]
    fn cancelled_subscription_runs_to_the_end_of_the_paid_month() {
        let t = |s: &str| parse_time(Some(s)).unwrap();
        let value = json!({
            "id": "I-1", "status": "CANCELLED", "quantity": "2",
            "custom_id": "01A-k|paypal:01A",
            "billing_info": { "last_payment": { "time": "2026-11-01T00:00:00Z",
                "amount": { "value": "5.98", "currency_code": "USD" } } }
        });
        let during = subscription_from(&value, false, t("2026-11-20T00:00:00Z"));
        assert_eq!(during.status, "active");
        assert!(during.cancel_at_period_end);
        assert_eq!(during.current_period_end, t("2026-12-01T00:00:00Z"));
        assert_eq!(during.quantity, 2);
        assert_eq!(during.order_id.as_deref(), Some("01A-k"));
        let after = subscription_from(&value, false, t("2026-12-02T00:00:00Z"));
        assert_eq!(after.status, "canceled");
    }

    #[test]
    fn suspended_is_past_due_and_pending_is_incomplete() {
        let now = parse_time(Some("2026-11-20T00:00:00Z")).unwrap();
        let suspended = json!({ "id": "I-2", "status": "SUSPENDED",
            "billing_info": { "last_payment": { "time": "2026-10-01T00:00:00Z" } } });
        assert_eq!(subscription_from(&suspended, false, now).status, "past_due");
        let pending = json!({ "id": "I-3", "status": "APPROVAL_PENDING" });
        assert_eq!(subscription_from(&pending, false, now).status, "incomplete");
    }

    #[test]
    fn order_session_reads_capture() {
        let value = json!({
            "id": "ORDER1", "status": "COMPLETED", "create_time": "2026-11-02T10:00:00Z",
            "purchase_units": [{ "custom_id": "01A-k|paypal:01A",
                "amount": { "value": "4.99", "currency_code": "USD" },
                "payments": { "captures": [{ "id": "CAP1", "status": "COMPLETED",
                    "amount": { "value": "4.99", "currency_code": "USD" } }] } }]
        });
        let session = provider().session_from_order(&value).unwrap();
        assert_eq!(session.status, CheckoutStatus::Complete);
        assert_eq!(session.payment_status, CheckoutPaymentStatus::Paid);
        assert_eq!(session.amount_total, Some(499));
        assert_eq!(session.currency.as_deref(), Some("usd"));
        assert_eq!(session.payment_intent.as_deref(), Some("CAP1"));
        assert_eq!(session.order_id.as_deref(), Some("01A-k"));
        assert_eq!(session.customer.as_deref(), Some("paypal:01A"));
        assert!(!session.livemode);
    }

    #[test]
    fn approved_but_not_captured_is_not_paid() {
        let value = json!({
            "id": "ORDER2", "status": "APPROVED",
            "purchase_units": [{ "custom_id": "01A-k|paypal:01A",
                "amount": { "value": "4.99", "currency_code": "USD" } }]
        });
        let session = provider().session_from_order(&value).unwrap();
        assert_eq!(session.status, CheckoutStatus::Open);
        assert_eq!(session.payment_status, CheckoutPaymentStatus::Unpaid);
    }

    #[test]
    fn event_kinds() {
        let p = provider();
        assert_eq!(p.event_kind("CHECKOUT.ORDER.APPROVED"), EventKind::CheckoutChanged);
        assert_eq!(p.event_kind("BILLING.SUBSCRIPTION.ACTIVATED"), EventKind::CheckoutChanged);
        assert_eq!(p.event_kind("PAYMENT.SALE.COMPLETED"), EventKind::Subscription);
        assert_eq!(p.event_kind("PAYMENT.CAPTURE.REFUNDED"), EventKind::Refund);
        assert_eq!(p.event_kind("CUSTOMER.DISPUTE.RESOLVED"), EventKind::Dispute);
        assert_eq!(p.event_kind("SOMETHING.ELSE"), EventKind::Other);
    }

    #[tokio::test]
    async fn webhook_without_paypal_headers_is_refused() {
        let body = br#"{"id":"WH-EVT","event_type":"CHECKOUT.ORDER.APPROVED"}"#;
        assert!(provider()
            .verify_webhook(&WebhookHeaders::default(), body, now_s())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn old_webhook_is_refused_before_asking_paypal() {
        let headers = WebhookHeaders(vec![
            ("Paypal-Transmission-Id".into(), "t1".into()),
            ("Paypal-Transmission-Time".into(), "2020-01-01T00:00:00Z".into()),
            ("Paypal-Transmission-Sig".into(), "sig".into()),
            ("Paypal-Cert-Url".into(), "https://api.paypal.com/cert".into()),
            ("Paypal-Auth-Algo".into(), "SHA256withRSA".into()),
        ]);
        let body = br#"{"id":"WH-EVT","event_type":"CHECKOUT.ORDER.APPROVED"}"#;
        // no network: refused on the timestamp alone
        assert!(provider().verify_webhook(&headers, body, now_s()).await.is_err());
    }

    #[test]
    fn test_address_only_locally_in_the_sandbox() {
        let make = |live, base: &str| {
            PayPalProvider::new("c".into(), "s".into(), "w".into(), live, base.into(), String::new(), String::new())
        };
        assert!(make(false, "http://127.0.0.1:9000").is_some());
        assert!(make(true, "http://127.0.0.1:9000").is_none());
        assert!(make(false, "http://evil.example").is_none());
        assert!(make(false, "").is_some());
    }
}
