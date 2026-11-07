//! Billing route tests against a local Stripe-compatible mock.
//!
//! The mock speaks the parts of Stripe's REST API the provider adapter
//! uses, on 127.0.0.1 with test keys only (`api_base` is refused with live
//! keys). It proves our side of the flow (signatures, verification, state
//! machine, delivery, refunds); it does not prove the real Stripe sandbox,
//! which needs the owner's sandbox keys.
//!
//! Run each test in its own process (the harness shares global config).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use iso8601_timestamp::Timestamp;
use revolt_database::{
    epoch_ms, sign_stripe_payload, Member, MFATicket, PartialServer, User,
};
use revolt_models::v0::{self, OrderStatus};
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::util::test::TestHarness;

const KEY: &str = "sk_test_zrzur_mock";
const WEBHOOK_SECRET: &str = "whsec_zrzur_mock";

// --- mock provider ---

#[derive(Default)]
struct MockState {
    next: u64,
    customers: HashMap<String, Value>,
    sessions: HashMap<String, Value>,
    subscriptions: HashMap<String, Value>,
    invoices: HashMap<String, Value>,
    payments: HashMap<String, Value>,
    charges: HashMap<String, Value>,
    refunds: HashMap<String, Value>,
    disputes: HashMap<String, Value>,
    /// Every request: (method, path, form)
    requests: Vec<(String, String, HashMap<String, String>)>,
    /// Idempotency keys seen with the object they made
    idempotent: HashMap<String, Value>,
}

#[derive(Clone)]
struct MockStripe {
    state: Arc<Mutex<MockState>>,
    base: String,
}

fn now_s() -> i64 {
    epoch_ms(Timestamp::now_utc()) / 1000
}

impl MockState {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}_test_mock{}", self.next)
    }

    fn handle(&mut self, method: &str, path: &str, form: HashMap<String, String>, idem: Option<String>) -> (u16, Value) {
        self.requests.push((method.to_string(), path.to_string(), form.clone()));
        if let Some(key) = &idem {
            if let Some(previous) = self.idempotent.get(key) {
                return (200, previous.clone());
            }
        }
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let result = match (method, parts.as_slice()) {
            ("POST", ["v1", "customers"]) => {
                let id = self.id("cus");
                let customer = json!({ "id": id, "object": "customer" });
                self.customers.insert(id, customer.clone());
                (200, customer)
            }
            ("POST", ["v1", "checkout", "sessions"]) => {
                let id = self.id("cs");
                let quantity: i64 = form["line_items[0][quantity]"].parse().unwrap();
                let unit: i64 = form
                    .get("line_items[0][price_data][unit_amount]")
                    .map(|v| v.parse().unwrap())
                    .unwrap_or(499);
                let session = json!({
                    "id": id,
                    "object": "checkout.session",
                    "mode": form["mode"],
                    "status": "open",
                    "payment_status": "unpaid",
                    "url": format!("https://checkout.stripe.com/c/pay/{id}"),
                    "amount_subtotal": unit * quantity,
                    "amount_total": unit * quantity,
                    "currency": form.get("line_items[0][price_data][currency]").cloned().unwrap_or("usd".into()),
                    "customer": form["customer"],
                    "client_reference_id": form["client_reference_id"],
                    "metadata": { "zrzur_order_id": form["metadata[zrzur_order_id]"] },
                    "expires_at": form["expires_at"].parse::<i64>().unwrap(),
                    "livemode": false,
                    "payment_intent": null,
                    "subscription": null,
                    "invoice": null,
                });
                self.sessions.insert(id, session.clone());
                (200, session)
            }
            ("GET", ["v1", "checkout", "sessions", id]) => match self.sessions.get(*id) {
                Some(session) => (200, session.clone()),
                None => (404, json!({ "error": { "type": "invalid_request_error" } })),
            },
            ("POST", ["v1", "checkout", "sessions", id, "expire"]) => match self.sessions.get_mut(*id) {
                Some(session) if session["status"] == "open" => {
                    session["status"] = json!("expired");
                    (200, session.clone())
                }
                _ => (400, json!({ "error": { "type": "invalid_request_error" } })),
            },
            ("GET", ["v1", "subscriptions", id]) => match self.subscriptions.get(*id) {
                Some(sub) => (200, sub.clone()),
                None => (404, json!({ "error": {} })),
            },
            ("POST", ["v1", "subscriptions", id]) => match self.subscriptions.get_mut(*id) {
                Some(sub) => {
                    if let Some(cancel) = form.get("cancel_at_period_end") {
                        sub["cancel_at_period_end"] = json!(cancel == "true");
                    }
                    (200, sub.clone())
                }
                None => (404, json!({ "error": {} })),
            },
            ("DELETE", ["v1", "subscriptions", id]) => match self.subscriptions.get_mut(*id) {
                Some(sub) => {
                    sub["status"] = json!("canceled");
                    (200, sub.clone())
                }
                None => (404, json!({ "error": {} })),
            },
            ("GET", ["v1", "invoices", id]) => match self.invoices.get(*id) {
                Some(invoice) => (200, invoice.clone()),
                None => (404, json!({ "error": {} })),
            },
            ("GET", ["v1", "payment_intents", id]) => match self.payments.get(*id) {
                Some(payment) => {
                    let mut payment = payment.clone();
                    let charge = payment["latest_charge"].as_str().unwrap_or_default().to_string();
                    if let Some(charge) = self.charges.get(&charge) {
                        payment["latest_charge"] = charge.clone();
                    }
                    (200, payment)
                }
                None => (404, json!({ "error": {} })),
            },
            ("GET", ["v1", "charges", id]) => match self.charges.get(*id) {
                Some(charge) => (200, charge.clone()),
                None => (404, json!({ "error": {} })),
            },
            ("POST", ["v1", "refunds"]) => {
                let pi = form["payment_intent"].clone();
                let Some(payment) = self.payments.get(&pi).cloned() else {
                    return (404, json!({ "error": {} }));
                };
                let charge_id = payment["latest_charge"].as_str().unwrap().to_string();
                let charge = self.charges.get_mut(&charge_id).unwrap();
                let amount: i64 = form
                    .get("amount")
                    .map(|a| a.parse().unwrap())
                    .unwrap_or(payment["amount"].as_i64().unwrap());
                let refunded = charge["amount_refunded"].as_i64().unwrap() + amount;
                if refunded > payment["amount"].as_i64().unwrap() {
                    return (400, json!({ "error": { "type": "invalid_request_error" } }));
                }
                charge["amount_refunded"] = json!(refunded);
                let id = self.id("re");
                let refund = json!({ "id": id, "object": "refund", "status": "succeeded", "amount": amount, "payment_intent": pi });
                self.refunds.insert(id, refund.clone());
                (200, refund)
            }
            ("GET", ["v1", "refunds", id]) => match self.refunds.get(*id) {
                Some(refund) => (200, refund.clone()),
                None => (404, json!({ "error": {} })),
            },
            ("GET", ["v1", "disputes", id]) => match self.disputes.get(*id) {
                Some(dispute) => (200, dispute.clone()),
                None => (404, json!({ "error": {} })),
            },
            ("POST", ["v1", "billing_portal", "sessions"]) => (
                200,
                json!({ "id": "bps_1", "url": "https://billing.stripe.com/p/session/test_mock" }),
            ),
            _ => (404, json!({ "error": { "type": "invalid_request_error" } })),
        };
        if let (Some(key), (200, value)) = (idem, &result) {
            self.idempotent.insert(key, value.clone());
        }
        result
    }
}

impl MockStripe {
    async fn start() -> MockStripe {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let mock = MockStripe {
            state: Arc::new(Mutex::new(MockState::default())),
            base,
        };
        let state = mock.state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    continue;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let mut buffer = vec![];
                    let mut chunk = [0u8; 8192];
                    let (head_end, length) = loop {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
                            let length = head
                                .lines()
                                .find_map(|line| line.strip_prefix("content-length:"))
                                .map(|value| value.trim().parse::<usize>().unwrap())
                                .unwrap_or(0);
                            break (end + 4, length);
                        }
                    };
                    while buffer.len() < head_end + length {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                    let mut request_line = head.lines().next().unwrap().split(' ');
                    let method = request_line.next().unwrap().to_string();
                    let target = request_line.next().unwrap().to_string();
                    let path = target.split('?').next().unwrap().to_string();
                    let authorised = head
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case(&format!("authorization: Bearer {KEY}")));
                    let idem = head.lines().find_map(|line| {
                        line.to_lowercase()
                            .starts_with("idempotency-key:")
                            .then(|| line.split_once(':').unwrap().1.trim().to_string())
                    });
                    let form: HashMap<String, String> =
                        url::form_urlencoded::parse(&buffer[head_end..])
                            .into_owned()
                            .collect();
                    let (status, body) = if authorised {
                        state.lock().unwrap().handle(&method, &path, form, idem)
                    } else {
                        (401, json!({ "error": { "type": "authentication_error" } }))
                    };
                    let body = body.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        mock
    }

    /// The buyer pays the checkout (what Stripe does after a successful
    /// card or 3-D Secure payment)
    fn pay(&self, session_id: &str) {
        let mut state = self.state.lock().unwrap();
        let session = state.sessions.get(session_id).unwrap().clone();
        let order_id = session["metadata"]["zrzur_order_id"].clone();
        let amount = session["amount_total"].as_i64().unwrap();
        let pi = state.id("pi");
        let charge = state.id("ch");
        state.charges.insert(
            charge.clone(),
            json!({ "id": charge, "object": "charge", "payment_intent": pi, "amount_refunded": 0,
                    "disputed": false, "receipt_url": "https://pay.stripe.com/receipts/test_mock" }),
        );
        state.payments.insert(
            pi.clone(),
            json!({ "id": pi, "object": "payment_intent", "status": "succeeded", "amount": amount,
                    "amount_received": amount, "currency": session["currency"], "customer": session["customer"],
                    "livemode": false, "latest_charge": charge, "metadata": { "zrzur_order_id": order_id } }),
        );
        let invoice = state.id("in");
        let mut subscription = Value::Null;
        if session["mode"] == "subscription" {
            let sub = state.id("sub");
            state.subscriptions.insert(
                sub.clone(),
                json!({ "id": sub, "object": "subscription", "status": "active", "cancel_at_period_end": false,
                        "current_period_start": now_s(), "current_period_end": now_s() + 30 * 86_400, "customer": session["customer"], "livemode": false,
                        "items": { "data": [{ "quantity": 1 }] }, "metadata": { "zrzur_order_id": order_id } }),
            );
            subscription = json!(sub);
        }
        state.invoices.insert(
            invoice.clone(),
            json!({ "id": invoice, "object": "invoice", "subscription": subscription, "billing_reason":
                    if subscription.is_null() { "manual" } else { "subscription_create" }, "status": "paid",
                    "amount_paid": amount, "amount_due": amount, "currency": session["currency"],
                    "hosted_invoice_url": "https://invoice.stripe.com/i/test_mock", "payment_intent": pi,
                    "customer": session["customer"], "livemode": false }),
        );
        let session = state.sessions.get_mut(session_id).unwrap();
        session["status"] = json!("complete");
        session["payment_status"] = json!("paid");
        session["payment_intent"] = json!(pi);
        session["subscription"] = subscription;
        session["invoice"] = json!(invoice);
    }

    /// A monthly renewal invoice (paid or failed)
    fn renew(&self, sub_id: &str, paid: bool) -> String {
        let mut state = self.state.lock().unwrap();
        let invoice = state.id("in");
        let pi = state.id("pi");
        let sub = state.subscriptions.get_mut(sub_id).unwrap();
        let end = sub["current_period_end"].as_i64().unwrap();
        sub["current_period_start"] = json!(end);
        sub["current_period_end"] = json!(end + 30 * 86_400);
        sub["status"] = json!(if paid { "active" } else { "past_due" });
        let customer = sub["customer"].clone();
        state.invoices.insert(
            invoice.clone(),
            json!({ "id": invoice, "object": "invoice", "subscription": sub_id, "billing_reason": "subscription_cycle",
                    "status": if paid { "paid" } else { "open" }, "amount_paid": if paid { 299 } else { 0 },
                    "amount_due": 299, "currency": "usd", "hosted_invoice_url": "https://invoice.stripe.com/i/renewal",
                    "payment_intent": pi, "customer": customer, "livemode": false }),
        );
        invoice
    }

    fn session(&self, id: &str) -> Value {
        self.state.lock().unwrap().sessions.get(id).unwrap().clone()
    }

    fn edit_session(&self, id: &str, change: impl FnOnce(&mut Value)) {
        change(self.state.lock().unwrap().sessions.get_mut(id).unwrap());
    }

    fn requests(&self, path_prefix: &str) -> Vec<HashMap<String, String>> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(_, path, _)| path.starts_with(path_prefix))
            .map(|(_, _, form)| form.clone())
            .collect()
    }
}

// --- helpers ---

/// A sandbox billing setup talking to the mock. `change` adjusts the config
/// further.
async fn sandbox(change: impl FnOnce(&mut revolt_config::Settings)) -> (TestHarness, MockStripe) {
    let mock = MockStripe::start().await;
    unsafe {
        std::env::set_var("ZRZUR_STRIPE_SECRET_KEY", KEY);
        std::env::set_var("ZRZUR_STRIPE_WEBHOOK_SECRET", WEBHOOK_SECRET);
    }
    let base = mock.base.clone();
    revolt_config::overwrite_config(move |config| {
        let billing = &mut config.features.billing;
        billing.enabled = true;
        billing.test_mode = false;
        billing.provider.name = "stripe".to_string();
        billing.provider.environment = "sandbox".to_string();
        billing.provider.api_base = base;
        change(config);
    })
    .await;
    (TestHarness::new().await, mock)
}

fn auth(session: &revolt_database::Session) -> Header<'static> {
    Header::new("x-session-token", session.token.to_string())
}

async fn post_json(
    harness: &TestHarness,
    session: &revolt_database::Session,
    path: &str,
    body: Value,
) -> (Status, Value) {
    let response = harness
        .client
        .post(path)
        .header(auth(session))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = response.status();
    let body = response
        .into_string()
        .await
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    (status, body)
}

async fn get_json(harness: &TestHarness, session: &revolt_database::Session, path: &str) -> (Status, Value) {
    let response = harness.client.get(path).header(auth(session)).dispatch().await;
    let status = response.status();
    let body = response
        .into_string()
        .await
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    (status, body)
}

async fn buy(
    harness: &TestHarness,
    session: &revolt_database::Session,
    product: Value,
) -> (Status, Value) {
    post_json(
        harness,
        session,
        "/billing/orders",
        json!({ "product": product, "idempotency_key": TestHarness::rand_string() }),
    )
    .await
}

fn session_id_of(checkout: &Value) -> String {
    checkout["checkout_url"]
        .as_str()
        .expect("checkout url")
        .rsplit('/')
        .next()
        .unwrap()
        .to_string()
}

/// Send a signed webhook as Stripe would
async fn webhook(harness: &TestHarness, event_id: &str, kind: &str, object_id: &str) -> Status {
    webhook_signed(harness, event_id, kind, object_id, WEBHOOK_SECRET, now_s(), false).await
}

async fn webhook_signed(
    harness: &TestHarness,
    event_id: &str,
    kind: &str,
    object_id: &str,
    secret: &str,
    timestamp: i64,
    livemode: bool,
) -> Status {
    let body = json!({
        "id": event_id, "object": "event", "type": kind, "livemode": livemode, "created": now_s(),
        "data": { "object": { "id": object_id, "object": kind.split('.').next().unwrap() } }
    })
    .to_string();
    let signature = sign_stripe_payload(body.as_bytes(), secret, timestamp);
    harness
        .client
        .post("/billing/webhooks/stripe")
        .header(Header::new("Stripe-Signature", signature))
        .header(ContentType::JSON)
        .body(body)
        .dispatch()
        .await
        .status()
}

/// Wait until the background handling of webhooks settles
async fn settle(harness: &TestHarness) {
    for _ in 0..50 {
        revolt_database::process_due_webhooks(&harness.db).await.unwrap();
        let pending = harness
            .db
            .count_webhook_events(revolt_database::WebhookEventStatus::Received)
            .await
            .unwrap();
        if pending == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn order_status(harness: &TestHarness, id: &str) -> OrderStatus {
    harness.db.fetch_order(id).await.unwrap().status
}

async fn has_plus(harness: &TestHarness, user: &User) -> bool {
    harness.db.fetch_user(&user.id).await.unwrap().has_plus().await
}

async fn deliveries(harness: &TestHarness, order_id: &str) -> usize {
    harness
        .db
        .fetch_billing_audit(Some(order_id), None, 100)
        .await
        .unwrap()
        .iter()
        .filter(|entry| entry.action == "order.delivered")
        .count()
}

async fn staff_session(harness: &TestHarness) -> (revolt_database::Session, User, String) {
    let (_, session, user) = harness.new_user().await;
    harness
        .db
        .update_user(
            &user.id,
            &revolt_database::PartialUser {
                privileged: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let ticket = MFATicket::new(user.id.clone(), true);
    ticket.save(&harness.db).await.unwrap();
    (session, user, ticket.token)
}

// --- tests ---

#[rocket::async_test]
async fn plus_purchase_is_delivered_exactly_once() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;

    let (status, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    assert_eq!(status, Status::Ok, "{checkout}");
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    assert_eq!(checkout["order"]["status"], "Pending");
    assert_eq!(checkout["order"]["amount"], 499);
    let session_id = session_id_of(&checkout);
    assert!(checkout["checkout_url"].as_str().unwrap().starts_with("https://checkout.stripe.com/"));

    // the hosted checkout got the server's price, our order and return
    // addresses built from hosts.app, never card data
    let form = &mock.requests("/v1/checkout/sessions")[0];
    assert_eq!(form["mode"], "subscription");
    assert_eq!(form["line_items[0][price_data][unit_amount]"], "499");
    assert_eq!(form["client_reference_id"], order_id);
    assert!(form["success_url"].starts_with("http://127.0.0.1:14701/?billing_order="));
    assert!(!form.keys().any(|key| key.contains("card")));

    // the success page alone gives nothing
    assert!(!has_plus(&harness, &user).await);

    mock.pay(&session_id);
    // the same event delivered three times, plus a second event for it
    for _ in 0..3 {
        assert_eq!(webhook(&harness, "evt_paid_1", "checkout.session.completed", &session_id).await, Status::Ok);
    }
    assert_eq!(webhook(&harness, "evt_paid_2", "checkout.session.completed", &session_id).await, Status::Ok);
    settle(&harness).await;

    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Paid);
    assert!(has_plus(&harness, &user).await);
    assert_eq!(deliveries(&harness, &order_id).await, 1);
    let order = harness.db.fetch_order(&order_id).await.unwrap();
    assert!(order.provider_subscription_id.is_some());
    assert_eq!(order.invoice_url.as_deref(), Some("https://invoice.stripe.com/i/test_mock"));
    // one stored event per event id
    let events = harness.db.fetch_webhook_events(None, 50).await.unwrap();
    assert_eq!(events.iter().filter(|e| e.id == "stripe:evt_paid_1").count(), 1);

    // history shows it, with the invoice
    let (_, history) = get_json(&harness, &session, "/billing/orders").await;
    assert_eq!(history[0]["_id"], order_id);
    assert_eq!(history[0]["fulfilled"], true);
}

#[rocket::async_test]
async fn concurrent_duplicate_webhooks_deliver_once() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    let (_, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let session_id = session_id_of(&checkout);
    mock.pay(&session_id);

    let sends = (0..6).map(|n| {
        let id = format!("evt_race_{}", n % 2);
        let harness = &harness;
        let session_id = session_id.clone();
        async move { webhook(harness, &id, "checkout.session.completed", &session_id).await }
    });
    for status in futures::future::join_all(sends).await {
        assert_eq!(status, Status::Ok);
    }
    // and the order page asking at the same time
    let _ = get_json(&harness, &session, &format!("/billing/orders/{order_id}?refresh=true")).await;
    settle(&harness).await;

    assert!(has_plus(&harness, &user).await);
    assert_eq!(deliveries(&harness, &order_id).await, 1);
    let subs: Vec<_> = harness
        .db
        .fetch_supercharges_by_user(&user.id)
        .await
        .unwrap();
    // only the feathers that come with Plus, once
    assert_eq!(subs.len(), 1);
}

#[rocket::async_test]
async fn unpaid_or_expired_checkout_grants_nothing() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    let (_, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let session_id = session_id_of(&checkout);

    // a "completed" event for an open, unpaid checkout (forged order or
    // early): the provider says unpaid, so nothing happens
    assert_eq!(webhook(&harness, "evt_early", "checkout.session.completed", &session_id).await, Status::Ok);
    settle(&harness).await;
    assert!(order_status(&harness, &order_id).await.is_open());
    assert!(!has_plus(&harness, &user).await);

    // the buyer closes the page: the order is cancelled and the checkout
    // closed at the provider so it can't be paid later
    let response = harness
        .client
        .delete(format!("/billing/orders/{order_id}"))
        .header(auth(&session))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Cancelled);
    assert_eq!(mock.session(&session_id)["status"], "expired");
    assert!(!has_plus(&harness, &user).await);
}

#[rocket::async_test]
async fn forged_and_replayed_webhooks_are_refused() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    let (_, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    let session_id = session_id_of(&checkout);
    mock.pay(&session_id);

    // wrong secret, too old, from the future, no signature
    assert_eq!(
        webhook_signed(&harness, "evt_f1", "checkout.session.completed", &session_id, "whsec_attacker", now_s(), false).await,
        Status::BadRequest
    );
    assert_eq!(
        webhook_signed(&harness, "evt_f2", "checkout.session.completed", &session_id, WEBHOOK_SECRET, now_s() - 3600, false).await,
        Status::BadRequest
    );
    assert_eq!(
        webhook_signed(&harness, "evt_f3", "checkout.session.completed", &session_id, WEBHOOK_SECRET, now_s() + 3600, false).await,
        Status::BadRequest
    );
    let response = harness
        .client
        .post("/billing/webhooks/stripe")
        .header(ContentType::JSON)
        .body("{}")
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);
    // unknown provider
    let response = harness
        .client
        .post("/billing/webhooks/paypal")
        .header(Header::new("Stripe-Signature", "t=1,v1=00"))
        .body("{}")
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NotFound);
    drop(response);
    // a live event on a sandbox server is stored as ignored, does nothing
    assert_eq!(
        webhook_signed(&harness, "evt_live", "checkout.session.completed", &session_id, WEBHOOK_SECRET, now_s(), true).await,
        Status::Ok
    );
    settle(&harness).await;

    assert!(!has_plus(&harness, &user).await);
    let events: Vec<_> = harness
        .db
        .fetch_webhook_events(None, 200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.id.starts_with("stripe:evt_f") || e.id == "stripe:evt_live")
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, "stripe:evt_live");
    assert_eq!(events[0].status, revolt_database::WebhookEventStatus::Ignored);

    // a body bigger than the limit
    let big = "x".repeat(revolt_database::MAX_WEBHOOK_BODY + 10);
    let signature = sign_stripe_payload(big.as_bytes(), WEBHOOK_SECRET, now_s());
    let response = harness
        .client
        .post("/billing/webhooks/stripe")
        .header(Header::new("Stripe-Signature", signature))
        .body(big)
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::PayloadTooLarge);
}

#[rocket::async_test]
async fn tampered_payments_are_rejected() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;

    for (n, tamper) in [
        Box::new(|s: &mut Value| s["amount_subtotal"] = json!(1)) as Box<dyn Fn(&mut Value)>,
        Box::new(|s: &mut Value| s["currency"] = json!("eur")),
        Box::new(|s: &mut Value| s["customer"] = json!("cus_attacker")),
        Box::new(|s: &mut Value| s["client_reference_id"] = json!("someone-elses-order")),
        Box::new(|s: &mut Value| s["amount_total"] = json!(1)),
        Box::new(|s: &mut Value| s["livemode"] = json!(true)),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
        let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
        let session_id = session_id_of(&checkout);
        mock.pay(&session_id);
        mock.edit_session(&session_id, tamper);
        webhook(&harness, &format!("evt_tamper_{n}"), "checkout.session.completed", &session_id).await;
        settle(&harness).await;
        assert_ne!(order_status(&harness, &order_id).await, OrderStatus::Paid, "case {n}");
        assert!(!has_plus(&harness, &user).await, "case {n}");
        // close it so the next case opens a new checkout
        mock.edit_session(&session_id, |s| s["status"] = json!("expired"));
        let _ = get_json(&harness, &session, &format!("/billing/orders/{order_id}?refresh=true")).await;
        harness
            .db
            .move_order(&order_id, OrderStatus::Cancelled, &Default::default())
            .await
            .unwrap();
    }
    let issues = harness.db.fetch_reconciliation_issues(false, 50).await.unwrap();
    assert!(issues.iter().any(|issue| issue.kind == "checkout_mismatch"));
}

#[rocket::async_test]
async fn client_cannot_choose_price_or_touch_other_orders() {
    let (harness, _mock) = sandbox(|_| {}).await;
    let (_, session, _) = harness.new_user().await;
    let (_, other_session, _) = harness.new_user().await;

    // extra fields (price, currency, status) are ignored; the catalog prices
    let (status, checkout) = post_json(
        &harness,
        &session,
        "/billing/orders",
        json!({ "product": { "type": "Plus" }, "idempotency_key": TestHarness::rand_string(),
                "amount": 1, "currency": "XXX", "status": "Paid" }),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(checkout["order"]["amount"], 499);
    assert_eq!(checkout["order"]["currency"], "USD");
    assert_eq!(checkout["order"]["status"], "Pending");
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();

    // invalid products and quantities
    for product in [
        json!({ "type": "PlusGift", "user": "x", "months": 2 }),
        json!({ "type": "Supercharge", "server": "x", "quantity": 0 }),
        json!({ "type": "Supercharge", "server": "x", "quantity": 1000 }),
        json!({ "type": "DiscoverBoost", "server": "x", "kind": "FreeBoost" }),
        json!({ "type": "Unknown" }),
    ] {
        let (status, _) = buy(&harness, &session, product.clone()).await;
        assert!(status.code >= 400, "{product}");
    }

    // someone else's order: invisible, can't be cancelled or paid
    let (status, _) = get_json(&harness, &other_session, &format!("/billing/orders/{order_id}")).await;
    assert_eq!(status, Status::NotFound);
    let response = harness
        .client
        .delete(format!("/billing/orders/{order_id}"))
        .header(auth(&other_session))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NotFound);
    drop(response);
    // the app can never confirm a provider payment itself
    let (status, _) = post_json(&harness, &session, &format!("/billing/orders/{order_id}/pay"), json!({})).await;
    assert_eq!(status, Status::BadRequest);
    assert!(order_status(&harness, &order_id).await.is_open());

    // a reload with a new key continues the open checkout instead of a second
    let (_, again) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    assert_eq!(again["order"]["_id"], order_id);
    assert_eq!(again["checkout_url"], checkout["checkout_url"]);
}

#[rocket::async_test]
async fn feathers_quantity_renewal_and_failed_renewal() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    let (server, _) = harness.new_server(&user).await;
    Member::create(&harness.db, &server, &user, None).await.unwrap();

    let (status, checkout) = buy(
        &harness,
        &session,
        json!({ "type": "Supercharge", "server": server.id, "quantity": 3 }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{checkout}");
    assert_eq!(checkout["order"]["amount"], 897);
    assert_eq!(checkout["order"]["quantity"], 3);
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let session_id = session_id_of(&checkout);
    mock.pay(&session_id);
    webhook(&harness, "evt_f_paid", "checkout.session.completed", &session_id).await;
    webhook(&harness, "evt_f_paid", "checkout.session.completed", &session_id).await;
    settle(&harness).await;

    let feathers = harness.db.fetch_supercharges_by_order(&order_id).await.unwrap();
    assert_eq!(feathers.len(), 3);
    assert!(feathers.iter().all(|f| f.server_id.as_deref() == Some(server.id.as_str())));
    let server_now = harness.db.fetch_server(&server.id).await.unwrap();
    assert_eq!(server_now.supercharge.unwrap().count, 3);

    // a renewal extends the period, recorded once however often it arrives
    let sub_id = harness.db.fetch_order(&order_id).await.unwrap().provider_subscription_id.unwrap();
    let before = feathers[0].current_period_end.unwrap();
    let invoice = mock.renew(&sub_id, true);
    webhook(&harness, "evt_renew_1", "invoice.paid", &invoice).await;
    webhook(&harness, "evt_renew_1b", "invoice.paid", &invoice).await;
    settle(&harness).await;
    let renewals: Vec<_> = harness
        .db
        .fetch_orders_by_user(&user.id, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|o| o.kind == v0::OrderKind::Renewal)
        .collect();
    assert_eq!(renewals.len(), 1);
    assert_eq!(renewals[0].status, OrderStatus::Paid);
    let after = harness.db.fetch_supercharges_by_order(&order_id).await.unwrap()[0]
        .current_period_end
        .unwrap();
    assert!(after > before);

    // a failed renewal: past due, perks only for the grace period
    let failed = mock.renew(&sub_id, false);
    webhook(&harness, "evt_renew_fail", "invoice.payment_failed", &failed).await;
    settle(&harness).await;
    let failed_order = harness.db.fetch_order(&format!("stripe:{failed}")).await.unwrap();
    assert_eq!(failed_order.status, OrderStatus::Failed);
    let grace_end = harness.db.fetch_supercharges_by_order(&order_id).await.unwrap()[0]
        .current_period_end
        .unwrap();
    assert_eq!(epoch_ms(grace_end), epoch_ms(after) + 3 * 86_400_000);
    // syncing again (another event) never stretches the grace
    webhook(&harness, "evt_sub_resync", "customer.subscription.updated", &sub_id).await;
    settle(&harness).await;
    let resynced = harness.db.fetch_supercharges_by_order(&order_id).await.unwrap()[0]
        .current_period_end
        .unwrap();
    assert_eq!(resynced, grace_end);

    // cancelling one feather stops renewal of the whole purchase
    let response = harness
        .client
        .patch(format!("/billing/supercharges/{}", feathers[0].id))
        .header(auth(&session))
        .header(ContentType::JSON)
        .body(json!({ "cancel_at_period_end": true }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    assert!(harness
        .db
        .fetch_supercharges_by_order(&order_id)
        .await
        .unwrap()
        .iter()
        .all(|f| f.cancel_at_period_end));
}

#[rocket::async_test]
async fn plus_cancel_resume_and_provider_end() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    let (_, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let session_id = session_id_of(&checkout);
    // out of order: the subscription update comes before the checkout
    mock.pay(&session_id);
    let sub_id = mock.session(&session_id)["subscription"].as_str().unwrap().to_string();
    webhook(&harness, "evt_sub_early", "customer.subscription.updated", &sub_id).await;
    settle(&harness).await;
    assert!(!has_plus(&harness, &user).await);
    webhook(&harness, "evt_paid", "checkout.session.completed", &session_id).await;
    settle(&harness).await;
    assert!(has_plus(&harness, &user).await);

    let patch = |cancel: bool| {
        let harness = &harness;
        let session = &session;
        async move {
            harness
                .client
                .patch("/billing/plus")
                .header(auth(session))
                .header(ContentType::JSON)
                .body(json!({ "cancel_at_period_end": cancel }).to_string())
                .dispatch()
                .await
                .status()
        }
    };
    assert_eq!(patch(true).await, Status::Ok);
    let sub = harness.db.fetch_plus_subscription(&user.id).await.unwrap().unwrap();
    assert!(sub.cancel_at_period_end);
    assert!(has_plus(&harness, &user).await, "kept until the period ends");
    assert_eq!(patch(false).await, Status::Ok);
    assert!(!harness.db.fetch_plus_subscription(&user.id).await.unwrap().unwrap().cancel_at_period_end);

    // the provider ends it (e.g. after failed retries)
    mock.state.lock().unwrap().subscriptions.get_mut(&sub_id).unwrap()["status"] = json!("canceled");
    webhook(&harness, "evt_sub_deleted", "customer.subscription.deleted", &sub_id).await;
    settle(&harness).await;
    assert!(!has_plus(&harness, &user).await);
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Paid);
}

#[rocket::async_test]
async fn discover_boost_and_staff_refund() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    let (mut server, _) = harness.new_server(&user).await;
    server
        .update(
            &harness.db,
            PartialServer {
                discoverable: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let (status, checkout) = buy(
        &harness,
        &session,
        json!({ "type": "DiscoverBoost", "server": server.id, "kind": "Boost" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{checkout}");
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let session_id = session_id_of(&checkout);
    assert_eq!(mock.requests("/v1/checkout/sessions")[0]["mode"], "payment");
    mock.pay(&session_id);
    webhook(&harness, "evt_boost", "checkout.session.completed", &session_id).await;
    settle(&harness).await;

    let promotion = harness.db.fetch_promotion_by_order(&order_id).await.unwrap().unwrap();
    let now = epoch_ms(Timestamp::now_utc());
    let seven_days = 7 * 86_400_000;
    assert!((promotion.ends_at - now - seven_days).abs() < 60_000);
    assert!(promotion.test);

    // staff refund: privileged and MFA
    let (member_session, _, _) = {
        let (_, s, u) = harness.new_user().await;
        (s, u, ())
    };
    let (status, _) = post_json(
        &harness,
        &member_session,
        &format!("/billing/admin/orders/{order_id}/refund"),
        json!({ "reason": "test refund" }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    let (staff, _, ticket) = staff_session(&harness).await;
    let (status, _) = post_json(
        &harness,
        &staff,
        &format!("/billing/admin/orders/{order_id}/refund"),
        json!({ "reason": "test refund" }),
    )
    .await;
    assert_eq!(status, Status::Unauthorized, "needs an MFA ticket");
    let response = harness
        .client
        .post(format!("/billing/admin/orders/{order_id}/refund"))
        .header(auth(&staff))
        .header(Header::new("x-mfa-ticket", ticket))
        .header(ContentType::JSON)
        .body(json!({ "reason": "customer asked" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    // the provider's refund event arrives as well: nothing changes twice
    let charge = mock.state.lock().unwrap().charges.keys().next().unwrap().clone();
    webhook(&harness, "evt_refund", "charge.refunded", &charge).await;
    settle(&harness).await;

    let order = harness.db.fetch_order(&order_id).await.unwrap();
    assert_eq!(order.status, OrderStatus::Refunded);
    assert_eq!(order.amount_refunded, order.amount);
    assert!(order.revoked);
    assert!(harness.db.fetch_promotion_by_order(&order_id).await.unwrap().is_none());
    assert_eq!(mock.requests("/v1/refunds").len(), 1);
    let audit = harness.db.fetch_billing_audit(Some(&order_id), None, 50).await.unwrap();
    assert!(audit.iter().any(|e| e.action == "refund.requested"));
    assert_eq!(audit.iter().filter(|e| e.action == "order.revoked").count(), 1);
}

#[rocket::async_test]
async fn gift_gives_one_right_and_refund_takes_it_back() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, _) = harness.new_user().await;
    let (_, _, friend) = harness.new_user().await;

    let (status, checkout) = buy(
        &harness,
        &session,
        json!({ "type": "PlusGift", "user": friend.id, "months": 3, "note": "enjoy" }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{checkout}");
    assert_eq!(checkout["order"]["amount"], 1497);
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let session_id = session_id_of(&checkout);
    mock.pay(&session_id);
    webhook(&harness, "evt_gift", "checkout.session.completed", &session_id).await;
    webhook(&harness, "evt_gift_again", "checkout.session.async_payment_succeeded", &session_id).await;
    settle(&harness).await;
    // the worker retrying delivery changes nothing either
    revolt_database::fulfil_order(&harness.db, &order_id).await.unwrap();

    let sub = harness.db.fetch_plus_subscription(&friend.id).await.unwrap().unwrap();
    assert_eq!(sub.applied_orders, vec![order_id.clone()]);
    let length = epoch_ms(sub.current_period_end) - epoch_ms(sub.started_at);
    assert!((length - 3 * 30 * 86_400_000).abs() < 60_000);
    assert!(has_plus(&harness, &friend).await);

    // a refund ends the gift
    revolt_database::request_refund(&harness.db, &order_id, None, None, "staff", "refund").await.unwrap();
    assert!(!has_plus(&harness, &friend).await);
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Refunded);
}

#[rocket::async_test]
async fn dispute_takes_perks_back_and_lost_dispute_is_final() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    let (_, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let session_id = session_id_of(&checkout);
    mock.pay(&session_id);
    webhook(&harness, "evt_paid", "checkout.session.completed", &session_id).await;
    settle(&harness).await;
    assert!(has_plus(&harness, &user).await);

    let pi = mock.session(&session_id)["payment_intent"].as_str().unwrap().to_string();
    mock.state.lock().unwrap().disputes.insert(
        "dp_1".into(),
        json!({ "id": "dp_1", "status": "needs_response", "payment_intent": pi, "amount": 499, "livemode": false }),
    );
    webhook(&harness, "evt_dispute", "charge.dispute.created", "dp_1").await;
    settle(&harness).await;
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Disputed);
    assert!(!has_plus(&harness, &user).await);

    mock.state.lock().unwrap().disputes.get_mut("dp_1").unwrap()["status"] = json!("lost");
    webhook(&harness, "evt_dispute_closed", "charge.dispute.closed", "dp_1").await;
    settle(&harness).await;
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Refunded);
}

#[rocket::async_test]
async fn missed_webhook_is_recovered_from_the_provider() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    let (_, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    mock.pay(&session_id_of(&checkout));

    // no webhook arrives (outage); the return page asks the backend, which
    // asks the provider
    let (status, body) = get_json(&harness, &session, &format!("/billing/orders/{order_id}?refresh=true")).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(body["order"]["status"], "Paid");
    assert_eq!(body["order"]["fulfilled"], true);
    assert!(body["checkout_url"].is_null());
    assert!(has_plus(&harness, &user).await);
}

#[rocket::async_test]
async fn failed_delivery_is_retried_without_double_grant() {
    let (harness, mock) = sandbox(|_| {}).await;
    let (_, session, _) = harness.new_user().await;
    let (_, _, friend) = harness.new_user().await;
    let (_, checkout) = buy(
        &harness,
        &session,
        json!({ "type": "PlusGift", "user": friend.id, "months": 1 }),
    )
    .await;
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let session_id = session_id_of(&checkout);
    mock.pay(&session_id);

    // simulate a crash mid-delivery: the order is paid and claimed, but
    // never finished
    harness
        .db
        .move_order(&order_id, OrderStatus::Paid, &Default::default())
        .await
        .unwrap();
    let now = epoch_ms(Timestamp::now_utc());
    assert!(harness.db.claim_fulfilment(&order_id, now, now - 1).await.unwrap());
    // the lease has run out, so the next attempt takes over
    revolt_database::fulfil_order(&harness.db, &order_id).await.unwrap();
    revolt_database::fulfil_order(&harness.db, &order_id).await.unwrap();
    webhook(&harness, "evt_late", "checkout.session.completed", &session_id).await;
    settle(&harness).await;

    let sub = harness.db.fetch_plus_subscription(&friend.id).await.unwrap().unwrap();
    let length = epoch_ms(sub.current_period_end) - epoch_ms(sub.started_at);
    assert!((length - 30 * 86_400_000).abs() < 60_000, "one month, not two");
    assert_eq!(deliveries(&harness, &order_id).await, 1);
}

#[rocket::async_test]
async fn staff_tools_need_privilege() {
    let (harness, _mock) = sandbox(|_| {}).await;
    let (_, session, user) = harness.new_user().await;
    for path in ["/billing/admin/summary", "/billing/admin/orders", "/billing/admin/webhooks", "/billing/admin/issues"] {
        let (status, _) = get_json(&harness, &session, path).await;
        assert_eq!(status, Status::Forbidden, "{path}");
    }
    let (status, _) = post_json(
        &harness,
        &session,
        "/billing/admin/grants",
        json!({ "user": user.id, "days": 30, "reason": "free plus please" }),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert!(!has_plus(&harness, &user).await);

    let (staff, staff_user, ticket) = staff_session(&harness).await;
    let (status, summary) = get_json(&harness, &staff, "/billing/admin/summary").await;
    assert_eq!(status, Status::Ok);
    assert_eq!(summary["mode"], "sandbox");
    // no secrets anywhere in the summary
    let text = summary.to_string();
    assert!(!text.contains(KEY) && !text.contains(WEBHOOK_SECRET));

    let response = harness
        .client
        .post("/billing/admin/grants")
        .header(auth(&staff))
        .header(Header::new("x-mfa-ticket", ticket))
        .header(ContentType::JSON)
        .body(json!({ "user": user.id, "days": 7, "reason": "support goodwill" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    assert!(has_plus(&harness, &user).await);
    let audit = harness.db.fetch_billing_audit(None, Some(&user.id), 10).await.unwrap();
    assert!(audit.iter().any(|e| e.action == "plus.granted" && e.actor_id == staff_user.id));
}

#[rocket::async_test]
async fn live_ready_never_starts_a_checkout() {
    unsafe {
        std::env::set_var("ZRZUR_STRIPE_SECRET_KEY", "sk_live_not_a_real_key");
        std::env::set_var("ZRZUR_STRIPE_WEBHOOK_SECRET", WEBHOOK_SECRET);
        std::env::remove_var(revolt_config::LIVE_ACTIVATION_ENV);
    }
    revolt_config::overwrite_config(|config| {
        let billing = &mut config.features.billing;
        billing.enabled = true;
        billing.test_mode = false;
        billing.provider.name = "stripe".to_string();
        billing.provider.environment = "live".to_string();
        billing.provider.checkout_enabled = true;
    })
    .await;
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;

    let (_, plans) = get_json(&harness, &session, "/billing/plans").await;
    assert_eq!(plans["mode"], "live_ready");
    assert_eq!(plans["checkout_available"], false);
    let (status, body) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert_eq!(body["feature"], "billing_checkout");

    // sandbox / simulated Plus on the account doesn't count on a live server
    harness
        .db
        .update_user(
            &user.id,
            &revolt_database::PartialUser {
                plus: Some(revolt_database::UserPlus {
                    until: Some(Timestamp::now_utc() + iso8601_timestamp::Duration::days(30)),
                    hide_badge: false,
                    live: false,
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    assert!(!has_plus(&harness, &user).await);
}

#[rocket::async_test]
async fn simulated_payments_refused_in_production() {
    // first: production refuses to start on the test settings' example secrets
    let harness = TestHarness::new().await;
    revolt_config::overwrite_config(|config| {
        config.production = true;
        config.features.billing.enabled = true;
        config.features.billing.test_mode = true;
        config.features.billing.provider.name = String::new();
    })
    .await;
    let (_, session, _) = harness.new_user().await;
    let (_, plans) = get_json(&harness, &session, "/billing/plans").await;
    assert_eq!(plans["available"], false);
    let (status, body) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(body["type"], "FeatureDisabled");
}

#[rocket::async_test]
async fn simulated_payments_still_work_locally() {
    revolt_config::overwrite_config(|config| {
        config.features.billing.enabled = true;
        config.features.billing.test_mode = true;
        config.features.billing.provider.name = String::new();
    })
    .await;
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (status, checkout) = buy(&harness, &session, json!({ "type": "Plus" })).await;
    assert_eq!(status, Status::Ok, "{checkout}");
    assert!(checkout["checkout_url"].is_null());
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let (status, paid) = post_json(&harness, &session, &format!("/billing/orders/{order_id}/pay"), json!({})).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(paid["status"], "Paid");
    assert_eq!(paid["provider"], "test");
    assert!(has_plus(&harness, &user).await);
    // paying twice changes nothing
    let (_, again) = post_json(&harness, &session, &format!("/billing/orders/{order_id}/pay"), json!({})).await;
    assert_eq!(again["status"], "Paid");
    assert_eq!(deliveries(&harness, &order_id).await, 1);
}

// =====================================================================
// PayPal, and Stripe + PayPal side by side
// =====================================================================
//
// A local PayPal-compatible mock (token, Orders v2, Subscriptions v1,
// plans, captures, refunds, webhook verification) on 127.0.0.1, sandbox
// only. Like the Stripe mock it proves our side of the flow, not the real
// PayPal sandbox.

const PP_CLIENT: &str = "pp-client-mock";
const PP_SECRET: &str = "pp-secret-mock";
const PP_WEBHOOK: &str = "WH-MOCK";
const PP_GOOD_SIG: &str = "mock-valid-signature";
const PP_BASIC: &str = "cHAtY2xpZW50LW1vY2s6cHAtc2VjcmV0LW1vY2s=";

#[derive(Default)]
struct PayPalState {
    next: u64,
    orders: HashMap<String, Value>,
    subscriptions: HashMap<String, Value>,
    captures: HashMap<String, Value>,
    refunds: HashMap<String, Value>,
    /// PayPal-Request-Id → answer
    request_ids: HashMap<String, Value>,
    /// (method, path, body)
    requests: Vec<(String, String, Value)>,
}

#[derive(Clone)]
struct MockPayPal {
    state: Arc<Mutex<PayPalState>>,
    base: String,
}

fn pp_time(seconds: i64) -> String {
    revolt_database::from_epoch_ms(seconds * 1000).to_string()
}

fn pp_plan(id: &str) -> Option<Value> {
    let price = match id {
        "P-PLUS" => "4.99",
        "P-WINGS" => "2.99",
        "P-ODD" => "0.99",
        _ => return None,
    };
    Some(json!({ "id": id, "status": "ACTIVE", "quantity_supported": true, "billing_cycles": [{
        "tenure_type": "REGULAR", "sequence": 1,
        "frequency": { "interval_unit": "MONTH", "interval_count": 1 },
        "pricing_scheme": { "fixed_price": { "value": price, "currency_code": "USD" } } }] }))
}

impl PayPalState {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}MOCK", self.next)
    }

    fn handle(&mut self, method: &str, path: &str, body: Value, request_id: Option<String>) -> (u16, Value) {
        self.requests.push((method.to_string(), path.to_string(), body.clone()));
        if let Some(key) = &request_id {
            if let Some(previous) = self.request_ids.get(key) {
                return (200, previous.clone());
            }
        }
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let unprocessable = || (422, json!({ "name": "UNPROCESSABLE_ENTITY" }));
        let missing = || (404, json!({ "name": "RESOURCE_NOT_FOUND" }));
        let result = match (method, parts.as_slice()) {
            ("POST", ["v2", "checkout", "orders"]) => {
                let id = self.id("ORDER");
                let unit = &body["purchase_units"][0];
                let order = json!({
                    "id": id, "status": "PAYER_ACTION_REQUIRED", "create_time": pp_time(now_s()),
                    "purchase_units": [{ "custom_id": unit["custom_id"], "amount": unit["amount"],
                        "payments": { "captures": [], "refunds": [] } }],
                    "links": [{ "rel": "payer-action",
                        "href": format!("https://www.sandbox.paypal.com/checkoutnow?token={id}") }]
                });
                self.orders.insert(id, order.clone());
                (200, order)
            }
            ("GET", ["v2", "checkout", "orders", id]) => match self.orders.get(*id) {
                Some(order) => (200, order.clone()),
                None => missing(),
            },
            ("POST", ["v2", "checkout", "orders", id, "capture"]) => {
                let Some(order) = self.orders.get(*id).cloned() else {
                    return missing();
                };
                if order["status"] != "APPROVED" {
                    return unprocessable();
                }
                let capture_id = self.id("CAP");
                let unit = &order["purchase_units"][0];
                let capture = json!({ "id": capture_id, "status": "COMPLETED", "amount": unit["amount"],
                    "custom_id": unit["custom_id"],
                    "supplementary_data": { "related_ids": { "order_id": id } } });
                self.captures.insert(capture_id.clone(), capture.clone());
                let order = self.orders.get_mut(*id).unwrap();
                order["status"] = json!("COMPLETED");
                order["purchase_units"][0]["payments"]["captures"] = json!([capture]);
                (201, order.clone())
            }
            ("GET", ["v2", "payments", "captures", id]) => match self.captures.get(*id) {
                Some(capture) => (200, capture.clone()),
                None => missing(),
            },
            ("POST", ["v2", "payments", "captures", id, "refund"]) => {
                let Some(capture) = self.captures.get(*id).cloned() else {
                    return missing();
                };
                let total = capture["amount"]["value"].as_str().unwrap().to_string();
                let value = body["amount"]["value"].as_str().map(String::from).unwrap_or(total.clone());
                let refund_id = self.id("REF");
                let refund = json!({ "id": refund_id, "status": "COMPLETED",
                    "amount": { "value": value, "currency_code": "USD" },
                    "links": [{ "rel": "up", "href": format!("https://api-m.sandbox.paypal.com/v2/payments/captures/{id}") }] });
                self.refunds.insert(refund_id, refund.clone());
                let order_id = capture["supplementary_data"]["related_ids"]["order_id"].as_str().unwrap().to_string();
                let capture = self.captures.get_mut(*id).unwrap();
                capture["status"] = json!(if value == total { "REFUNDED" } else { "PARTIALLY_REFUNDED" });
                let order = self.orders.get_mut(&order_id).unwrap();
                order["purchase_units"][0]["payments"]["refunds"]
                    .as_array_mut()
                    .unwrap()
                    .push(refund.clone());
                (201, refund)
            }
            ("GET", ["v2", "payments", "refunds", id]) => match self.refunds.get(*id) {
                Some(refund) => (200, refund.clone()),
                None => missing(),
            },
            ("GET", ["v1", "payments", "sale", _]) => missing(),
            ("POST", ["v1", "billing", "subscriptions"]) => {
                let id = self.id("I-SUB");
                let sub = json!({
                    "id": id, "status": "APPROVAL_PENDING", "plan_id": body["plan_id"],
                    "quantity": body["quantity"], "custom_id": body["custom_id"], "create_time": pp_time(now_s()),
                    "links": [{ "rel": "approve",
                        "href": format!("https://www.sandbox.paypal.com/webapps/billing/subscriptions?ba_token={id}") }]
                });
                self.subscriptions.insert(id, sub.clone());
                (201, sub)
            }
            ("GET", ["v1", "billing", "subscriptions", id]) => match self.subscriptions.get(*id) {
                Some(sub) => (200, sub.clone()),
                None => missing(),
            },
            ("POST", ["v1", "billing", "subscriptions", id, "cancel"]) => match self.subscriptions.get_mut(*id) {
                Some(sub) if sub["status"] == "ACTIVE" || sub["status"] == "SUSPENDED" => {
                    sub["status"] = json!("CANCELLED");
                    (204, Value::Null)
                }
                Some(_) => unprocessable(),
                None => missing(),
            },
            ("GET", ["v1", "billing", "plans", id]) => match pp_plan(id) {
                Some(plan) => (200, plan),
                None => missing(),
            },
            ("POST", ["v1", "notifications", "verify-webhook-signature"]) => {
                let ok = body["webhook_id"] == PP_WEBHOOK
                    && body["transmission_sig"] == PP_GOOD_SIG
                    && body["webhook_event"]["id"].is_string();
                (200, json!({ "verification_status": if ok { "SUCCESS" } else { "FAILURE" } }))
            }
            _ => missing(),
        };
        if let (Some(key), (200 | 201, value)) = (&request_id, &result) {
            self.request_ids.insert(key.clone(), value.clone());
        }
        result
    }
}

impl MockPayPal {
    async fn start() -> MockPayPal {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let mock = MockPayPal {
            state: Arc::new(Mutex::new(PayPalState::default())),
            base,
        };
        let state = mock.state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    continue;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let mut buffer = vec![];
                    let mut chunk = [0u8; 8192];
                    let (head_end, length) = loop {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
                            let length = head
                                .lines()
                                .find_map(|line| line.strip_prefix("content-length:"))
                                .map(|value| value.trim().parse::<usize>().unwrap())
                                .unwrap_or(0);
                            break (end + 4, length);
                        }
                    };
                    while buffer.len() < head_end + length {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                    let mut request_line = head.lines().next().unwrap().split(' ');
                    let method = request_line.next().unwrap().to_string();
                    let path = request_line.next().unwrap().split('?').next().unwrap().to_string();
                    let header = |name: &str| {
                        head.lines().find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_string())
                        })
                    };
                    // base64 of "pp-client-mock:pp-secret-mock"
                    let basic = format!("Basic {PP_BASIC}");
                    let (status, body) = if path == "/v1/oauth2/token" {
                        if header("authorization").as_deref() == Some(basic.as_str()) {
                            (200, json!({ "access_token": "A21-mock", "token_type": "Bearer", "expires_in": 3600 }))
                        } else {
                            (401, json!({ "error": "invalid_client" }))
                        }
                    } else if header("authorization").as_deref() == Some("Bearer A21-mock") {
                        let body: Value = serde_json::from_slice(&buffer[head_end..]).unwrap_or(Value::Null);
                        state.lock().unwrap().handle(&method, &path, body, header("paypal-request-id"))
                    } else {
                        (401, json!({ "name": "AUTHENTICATION_FAILURE" }))
                    };
                    let body = if body.is_null() { String::new() } else { body.to_string() };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        mock
    }

    /// The buyer approves an order on PayPal's page
    fn approve(&self, order_id: &str) {
        self.state.lock().unwrap().orders.get_mut(order_id).unwrap()["status"] = json!("APPROVED");
    }

    /// PayPal takes the first payment and activates the subscription
    fn activate(&self, sub_id: &str, amount: &str) {
        let mut state = self.state.lock().unwrap();
        let sub = state.subscriptions.get_mut(sub_id).unwrap();
        sub["status"] = json!("ACTIVE");
        sub["billing_info"] = json!({
            "last_payment": { "time": pp_time(now_s()), "amount": { "value": amount, "currency_code": "USD" } },
            "next_billing_time": pp_time(now_s() + 30 * 86_400)
        });
    }

    fn edit_subscription(&self, id: &str, change: impl FnOnce(&mut Value)) {
        change(self.state.lock().unwrap().subscriptions.get_mut(id).unwrap());
    }

    fn count(&self, method: &str, path_suffix: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(m, path, _)| m == method && path.ends_with(path_suffix))
            .count()
    }

    fn captures(&self) -> usize {
        self.state.lock().unwrap().captures.len()
    }

    fn first_body(&self) -> Value {
        self.state.lock().unwrap().requests[0].2.clone()
    }
}

fn paypal_entry(base: &str) -> revolt_config::BillingProvider {
    revolt_config::BillingProvider {
        name: "paypal".to_string(),
        environment: "sandbox".to_string(),
        api_base: base.to_string(),
        plus_price_id: "P-PLUS".to_string(),
        supercharge_price_id: "P-WINGS".to_string(),
        ..Default::default()
    }
}

fn set_paypal_env() {
    unsafe {
        std::env::set_var("ZRZUR_PAYPAL_CLIENT_ID", PP_CLIENT);
        std::env::set_var("ZRZUR_PAYPAL_CLIENT_SECRET", PP_SECRET);
        std::env::set_var("ZRZUR_PAYPAL_WEBHOOK_ID", PP_WEBHOOK);
    }
}

/// PayPal as the only provider
async fn paypal_sandbox() -> (TestHarness, MockPayPal) {
    let mock = MockPayPal::start().await;
    set_paypal_env();
    let base = mock.base.clone();
    revolt_config::overwrite_config(move |config| {
        let billing = &mut config.features.billing;
        billing.enabled = true;
        billing.test_mode = false;
        billing.provider = paypal_entry(&base);
        billing.providers = vec![];
    })
    .await;
    (TestHarness::new().await, mock)
}

/// Send a webhook as PayPal would (headers + body)
async fn paypal_webhook(
    harness: &TestHarness,
    event_id: &str,
    kind: &str,
    resource: Value,
    signature: &str,
    sent: i64,
) -> Status {
    let body = json!({ "id": event_id, "event_type": kind, "resource_type": "x",
        "create_time": pp_time(now_s()), "resource": resource })
    .to_string();
    harness
        .client
        .post("/billing/webhooks/paypal")
        .header(Header::new("Paypal-Transmission-Id", format!("t-{event_id}")))
        .header(Header::new("Paypal-Transmission-Time", pp_time(sent)))
        .header(Header::new("Paypal-Transmission-Sig", signature.to_string()))
        .header(Header::new("Paypal-Cert-Url", "https://api.sandbox.paypal.com/v1/notifications/certs/CERT"))
        .header(Header::new("Paypal-Auth-Algo", "SHA256withRSA"))
        .header(ContentType::JSON)
        .body(body)
        .dispatch()
        .await
        .status()
}

async fn buy_with(
    harness: &TestHarness,
    session: &revolt_database::Session,
    product: Value,
    provider: &str,
) -> (Status, Value) {
    post_json(
        harness,
        session,
        "/billing/orders",
        json!({ "product": product, "provider": provider, "idempotency_key": TestHarness::rand_string() }),
    )
    .await
}

async fn checkout_id(harness: &TestHarness, order_id: &str) -> String {
    harness
        .db
        .fetch_order(order_id)
        .await
        .unwrap()
        .provider_checkout_id
        .unwrap()
}

async fn patch_json(
    harness: &TestHarness,
    session: &revolt_database::Session,
    path: &str,
    body: Value,
) -> (Status, Value) {
    let response = harness
        .client
        .patch(path)
        .header(auth(session))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await;
    let status = response.status();
    let body = response
        .into_string()
        .await
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    (status, body)
}

#[rocket::async_test]
async fn paypal_gift_is_captured_and_delivered_once() {
    let (harness, mock) = paypal_sandbox().await;
    let (_, session, _) = harness.new_user().await;
    let (_, _, friend) = harness.new_user().await;

    let (status, checkout) = buy_with(
        &harness,
        &session,
        json!({ "type": "PlusGift", "user": friend.id, "months": 3 }),
        "paypal",
    )
    .await;
    assert_eq!(status, Status::Ok, "{checkout}");
    assert_eq!(checkout["order"]["provider"], "paypal");
    assert_eq!(checkout["order"]["amount"], 1497);
    assert!(checkout["checkout_url"]
        .as_str()
        .unwrap()
        .starts_with("https://www.sandbox.paypal.com/"));
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let paypal_order = checkout_id(&harness, &order_id).await;

    // PayPal got the server's price, our order id and no card data
    let sent = mock.first_body();
    assert_eq!(sent["purchase_units"][0]["amount"]["value"], "14.97");
    assert!(sent["purchase_units"][0]["custom_id"]
        .as_str()
        .unwrap()
        .starts_with(&format!("{order_id}|paypal:")));
    assert!(!sent.to_string().contains("card"));

    // nothing before the buyer approves
    assert!(!has_plus(&harness, &friend).await);

    mock.approve(&paypal_order);
    let resource = json!({ "id": paypal_order });
    for event in ["WH-A", "WH-A", "WH-B"] {
        assert_eq!(
            paypal_webhook(&harness, event, "CHECKOUT.ORDER.APPROVED", resource.clone(), PP_GOOD_SIG, now_s()).await,
            Status::Ok
        );
    }
    settle(&harness).await;
    // the order page asking as well
    get_json(&harness, &session, &format!("/billing/orders/{order_id}?refresh=true")).await;

    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Paid);
    assert!(has_plus(&harness, &friend).await);
    assert_eq!(deliveries(&harness, &order_id).await, 1);
    assert_eq!(mock.captures(), 1, "the money is taken once");
    let order = harness.db.fetch_order(&order_id).await.unwrap();
    assert!(order.provider_payment_id.is_some());

    // a refund gives the money back and ends the gift
    revolt_database::request_refund(&harness.db, &order_id, None, None, "staff", "refund").await.unwrap();
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Refunded);
    assert!(!has_plus(&harness, &friend).await);
    assert_eq!(mock.count("POST", "/refund"), 1);
}

#[rocket::async_test]
async fn paypal_cancelled_order_is_never_captured() {
    let (harness, mock) = paypal_sandbox().await;
    let (_, session, _) = harness.new_user().await;
    let (_, _, friend) = harness.new_user().await;
    let (_, checkout) = buy_with(
        &harness,
        &session,
        json!({ "type": "PlusGift", "user": friend.id, "months": 1 }),
        "paypal",
    )
    .await;
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let paypal_order = checkout_id(&harness, &order_id).await;

    let response = harness
        .client
        .delete(format!("/billing/orders/{order_id}"))
        .header(auth(&session))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Cancelled);

    // the buyer approves anyway, late: no capture, nothing given
    mock.approve(&paypal_order);
    paypal_webhook(&harness, "WH-LATE", "CHECKOUT.ORDER.APPROVED", json!({ "id": paypal_order }), PP_GOOD_SIG, now_s()).await;
    settle(&harness).await;
    revolt_database::reconcile(&harness.db).await.unwrap();
    assert_eq!(mock.captures(), 0);
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Cancelled);
    assert!(!has_plus(&harness, &friend).await);
}

#[rocket::async_test]
async fn paypal_feather_subscription_runs_and_cancel_keeps_the_paid_month() {
    let (harness, mock) = paypal_sandbox().await;
    let (_, session, user) = harness.new_user().await;

    let (status, checkout) = buy_with(&harness, &session, json!({ "type": "Plus" }), "paypal").await;
    assert_eq!(status, Status::Ok, "{checkout}");
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let sub_id = checkout_id(&harness, &order_id).await;
    assert!(sub_id.starts_with("I-"));
    let sent = mock.first_body();
    assert_eq!(sent["plan_id"], "P-PLUS");
    assert_eq!(sent["quantity"], "1");

    mock.activate(&sub_id, "4.99");
    paypal_webhook(&harness, "WH-ACT", "BILLING.SUBSCRIPTION.ACTIVATED", json!({ "id": sub_id }), PP_GOOD_SIG, now_s()).await;
    settle(&harness).await;
    assert_eq!(order_status(&harness, &order_id).await, OrderStatus::Paid);
    assert!(has_plus(&harness, &user).await);

    // cancelling renewal ends it at PayPal, the paid month stays
    let (status, overview) = patch_json(&harness, &session, "/billing/plus", json!({ "cancel_at_period_end": true })).await;
    assert_eq!(status, Status::Ok, "{overview}");
    assert_eq!(overview["plus"]["cancel_at_period_end"], true);
    assert_eq!(mock.count("POST", "/cancel"), 1);
    assert!(has_plus(&harness, &user).await);
    // a cancelled PayPal subscription can't be resumed
    let (status, _) = patch_json(&harness, &session, "/billing/plus", json!({ "cancel_at_period_end": false })).await;
    assert_eq!(status, Status::BadRequest);

    // after the paid month PayPal reports it ended: perks go
    mock.edit_subscription(&sub_id, |sub| {
        sub["billing_info"]["last_payment"]["time"] = json!(pp_time(now_s() - 40 * 86_400));
    });
    paypal_webhook(&harness, "WH-END", "BILLING.SUBSCRIPTION.EXPIRED", json!({ "id": sub_id }), PP_GOOD_SIG, now_s()).await;
    settle(&harness).await;
    assert!(!has_plus(&harness, &user).await);
}

#[rocket::async_test]
async fn paypal_wrong_plan_gives_nothing() {
    paypal_tampered("plan").await;
}

#[rocket::async_test]
async fn paypal_wrong_quantity_gives_nothing() {
    paypal_tampered("quantity").await;
}

async fn paypal_tampered(tamper: &str) {
    let (harness, mock) = paypal_sandbox().await;
    let (_, session, user) = harness.new_user().await;
    let (server, _) = harness.new_server(&user).await;
    Member::create(&harness.db, &server, &user, None).await.unwrap();
    let (status, checkout) = buy_with(
        &harness,
        &session,
        json!({ "type": "Supercharge", "server": server.id, "quantity": 2 }),
        "paypal",
    )
    .await;
    assert_eq!(status, Status::Ok, "{checkout}");
    let order_id = checkout["order"]["_id"].as_str().unwrap().to_string();
    let sub_id = checkout_id(&harness, &order_id).await;
    mock.edit_subscription(&sub_id, |sub| match tamper {
        "plan" => sub["plan_id"] = json!("P-ODD"),
        _ => sub["quantity"] = json!("1"),
    });
    mock.activate(&sub_id, "0.99");
    paypal_webhook(&harness, "WH-T", "BILLING.SUBSCRIPTION.ACTIVATED", json!({ "id": sub_id }), PP_GOOD_SIG, now_s()).await;
    settle(&harness).await;
    assert_ne!(order_status(&harness, &order_id).await, OrderStatus::Paid, "{tamper}");
    assert!(harness.db.fetch_supercharges_by_user(&user.id).await.unwrap().is_empty(), "{tamper}");
}

#[rocket::async_test]
async fn paypal_unverified_webhooks_are_refused() {
    let (harness, _mock) = paypal_sandbox().await;
    let resource = json!({ "id": "ORDER1MOCK" });
    // bad signature (PayPal says FAILURE)
    assert_eq!(
        paypal_webhook(&harness, "WH-X1", "CHECKOUT.ORDER.APPROVED", resource.clone(), "forged", now_s()).await,
        Status::BadRequest
    );
    // replay of an old delivery
    assert_eq!(
        paypal_webhook(&harness, "WH-X2", "CHECKOUT.ORDER.APPROVED", resource.clone(), PP_GOOD_SIG, now_s() - 3600).await,
        Status::BadRequest
    );
    // no PayPal headers at all
    let response = harness
        .client
        .post("/billing/webhooks/paypal")
        .header(ContentType::JSON)
        .body(json!({ "id": "WH-X3", "event_type": "CHECKOUT.ORDER.APPROVED" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);
    // a Stripe header doesn't open the PayPal door
    let response = harness
        .client
        .post("/billing/webhooks/paypal")
        .header(Header::new("Stripe-Signature", "t=1,v1=00"))
        .header(ContentType::JSON)
        .body("{}")
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);
    // a provider that isn't running has no door
    assert_eq!(webhook(&harness, "evt_x", "checkout.session.completed", "cs_1").await, Status::NotFound);
    assert!(harness.db.fetch_webhook_events(None, 50).await.unwrap().is_empty());
}

#[rocket::async_test]
async fn stripe_and_paypal_side_by_side() {
    let paypal = MockPayPal::start().await;
    set_paypal_env();
    let paypal_base = paypal.base.clone();
    let (harness, _stripe) = sandbox(move |config| {
        config.features.billing.providers = vec![paypal_entry(&paypal_base)];
    })
    .await;
    let (_, session, _) = harness.new_user().await;
    let (_, _, friend) = harness.new_user().await;

    let (_, plans) = get_json(&harness, &session, "/billing/plans").await;
    assert_eq!(plans["providers"], json!(["stripe", "paypal"]));
    assert_eq!(plans["provider"], "stripe");

    let gift = json!({ "type": "PlusGift", "user": friend.id, "months": 1 });
    // no choice: the preferred provider
    let (_, by_default) = buy(&harness, &session, gift.clone()).await;
    assert_eq!(by_default["order"]["provider"], "stripe");
    // the buyer's choice
    let (_, by_paypal) = buy_with(&harness, &session, gift.clone(), "paypal").await;
    assert_eq!(by_paypal["order"]["provider"], "paypal");
    assert_eq!(paypal.count("POST", "/v2/checkout/orders"), 1);
    // an open Stripe checkout is never handed out for a PayPal choice
    assert_ne!(by_default["order"]["_id"], by_paypal["order"]["_id"]);
    // unknown or test providers are refused
    let orders_before = harness.db.fetch_orders_by_user(&by_default["order"]["_id"].as_str().unwrap()[..26], 50).await.unwrap().len();
    for name in ["bogus", "test"] {
        let (status, _) = buy_with(&harness, &session, gift.clone(), name).await;
        assert!(status.code >= 400 && status.code < 500, "{name}: {status}");
    }
    let orders_after = harness.db.fetch_orders_by_user(&by_default["order"]["_id"].as_str().unwrap()[..26], 50).await.unwrap().len();
    assert_eq!(orders_before, orders_after, "nothing created for a refused provider");

    // each order is settled by its own provider
    let paypal_order_id = by_paypal["order"]["_id"].as_str().unwrap().to_string();
    let paypal_checkout = checkout_id(&harness, &paypal_order_id).await;
    paypal.approve(&paypal_checkout);
    paypal_webhook(&harness, "WH-S1", "CHECKOUT.ORDER.APPROVED", json!({ "id": paypal_checkout }), PP_GOOD_SIG, now_s()).await;
    settle(&harness).await;
    assert_eq!(order_status(&harness, &paypal_order_id).await, OrderStatus::Paid);
    let stripe_order_id = by_default["order"]["_id"].as_str().unwrap().to_string();
    assert_eq!(order_status(&harness, &stripe_order_id).await, OrderStatus::Pending);

    // one idempotency key never switches provider (a product with no
    // open checkout yet, so the key really makes the order)
    let gift = json!({ "type": "PlusGift", "user": friend.id, "months": 12 });
    let key = TestHarness::rand_string();
    let (first, _) = post_json(&harness, &session, "/billing/orders",
        json!({ "product": gift, "provider": "stripe", "idempotency_key": key })).await;
    assert_eq!(first, Status::Ok);
    let (switched, _) = post_json(&harness, &session, "/billing/orders",
        json!({ "product": gift, "provider": "paypal", "idempotency_key": key })).await;
    assert_eq!(switched, Status::BadRequest);
}
