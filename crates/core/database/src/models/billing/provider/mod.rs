//! Payment providers. Zrzur never sees card details: buyers pay on the
//! provider's hosted checkout page, and the provider tells us about it
//! through signed webhooks. Everything a webhook says is checked again by
//! fetching the object from the provider's API before it changes anything.
//!
//! Several providers can run at once (a buyer picks one); every order
//! remembers its provider by name. Stripe and PayPal are implemented;
//! another provider plugs in by implementing `PaymentProvider` and adding
//! its name to `build`.

use revolt_config::{provider_ready, BillingProvider, FeaturesBilling};
use revolt_result::Result;

mod paypal;
mod signature;
mod stripe;

pub use paypal::PayPalProvider;
pub use signature::*;
pub use stripe::StripeProvider;

/// Request headers a webhook arrived with (names compared without case)
#[derive(Debug, Clone, Default)]
pub struct WebhookHeaders(pub Vec<(String, String)>);

impl WebhookHeaders {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// What a provider event is about, in Zrzur's terms. The event itself is
/// never trusted: each kind is handled by fetching the object again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A checkout was paid, completed, approved or expired (object: checkout)
    CheckoutChanged,
    /// A delayed checkout payment failed (object: checkout)
    CheckoutFailed,
    /// A payment changed (object: payment)
    Payment,
    /// A subscription invoice was paid (object: invoice)
    InvoicePaid,
    /// A subscription invoice failed (object: invoice)
    InvoiceFailed,
    /// A subscription changed (object: subscription)
    Subscription,
    /// A charge was refunded (object: charge)
    ChargeRefund,
    /// A refund changed (object: refund)
    Refund,
    /// A dispute changed (object: dispute)
    Dispute,
    /// Nothing to do
    Other,
}

/// How a checkout charges
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutMode {
    /// Paid once
    Payment,
    /// Renews monthly until cancelled
    Subscription,
}

/// What the buyer pays for (one line)
#[derive(Debug, Clone)]
pub struct CheckoutLine {
    /// Provider price to use; when empty the price is sent inline
    pub price_id: Option<String>,
    /// Price of one unit in minor units (always set: it is checked against
    /// the provider price when `price_id` is used)
    pub unit_amount: u32,
    /// ISO code, lower case
    pub currency: String,
    /// Name shown on the checkout page and invoice
    pub name: String,
    pub quantity: u32,
}

/// Start a hosted checkout
#[derive(Debug, Clone)]
pub struct CheckoutRequest {
    pub order_id: String,
    pub user_id: String,
    pub customer_id: String,
    pub mode: CheckoutMode,
    pub line: CheckoutLine,
    /// Where the provider sends the buyer afterwards. Built by the server
    /// from `hosts.app`, never taken from the request.
    pub success_url: String,
    pub cancel_url: String,
    /// Unix seconds
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutStatus {
    Open,
    Complete,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutPaymentStatus {
    Paid,
    Unpaid,
    NoPaymentRequired,
}

/// A checkout session as the provider sees it
#[derive(Debug, Clone)]
pub struct CheckoutSession {
    pub id: String,
    pub url: Option<String>,
    pub status: CheckoutStatus,
    pub payment_status: CheckoutPaymentStatus,
    /// Before discounts and taxes
    pub amount_subtotal: Option<i64>,
    /// After discounts and taxes
    pub amount_total: Option<i64>,
    /// Lower case
    pub currency: Option<String>,
    pub customer: Option<String>,
    pub payment_intent: Option<String>,
    pub subscription: Option<String>,
    pub invoice: Option<String>,
    pub livemode: bool,
    pub client_reference_id: Option<String>,
    /// `zrzur_order_id` in the session metadata
    pub order_id: Option<String>,
    /// Unix seconds
    pub expires_at: i64,
}

/// A subscription as the provider sees it
#[derive(Debug, Clone)]
pub struct ProviderSubscription {
    pub id: String,
    /// Provider status, e.g. `active`, `past_due`, `canceled`
    pub status: String,
    pub cancel_at_period_end: bool,
    /// Unix seconds: start of the period being charged (= end of the
    /// last period that was paid for)
    pub current_period_start: i64,
    /// Unix seconds
    pub current_period_end: i64,
    pub customer: Option<String>,
    pub livemode: bool,
    pub order_id: Option<String>,
    pub quantity: u32,
}

/// An invoice as the provider sees it
#[derive(Debug, Clone)]
pub struct ProviderInvoice {
    pub id: String,
    pub subscription: Option<String>,
    /// e.g. `subscription_create`, `subscription_cycle`
    pub billing_reason: Option<String>,
    /// e.g. `paid`, `open`, `void`, `uncollectible`
    pub status: Option<String>,
    pub amount_paid: i64,
    pub amount_due: i64,
    pub currency: String,
    pub hosted_invoice_url: Option<String>,
    pub payment_intent: Option<String>,
    pub customer: Option<String>,
    pub livemode: bool,
}

/// A payment as the provider sees it
#[derive(Debug, Clone)]
pub struct ProviderPayment {
    pub id: String,
    /// e.g. `succeeded`, `processing`, `requires_action`
    pub status: String,
    pub amount: i64,
    pub amount_received: i64,
    pub currency: String,
    pub amount_refunded: i64,
    pub disputed: bool,
    pub receipt_url: Option<String>,
    pub customer: Option<String>,
    pub livemode: bool,
    pub order_id: Option<String>,
}

/// A refund as the provider sees it
#[derive(Debug, Clone)]
pub struct ProviderRefund {
    pub id: String,
    /// `pending`, `succeeded`, `failed`, `canceled` or `requires_action`
    pub status: String,
    pub amount: i64,
    pub payment_intent: Option<String>,
}

/// A dispute as the provider sees it
#[derive(Debug, Clone)]
pub struct ProviderDispute {
    pub id: String,
    /// e.g. `needs_response`, `under_review`, `won`, `lost`
    pub status: String,
    pub payment_intent: Option<String>,
    pub amount: i64,
    pub livemode: bool,
}

/// A verified webhook event (signature checked; nothing else trusted yet)
#[derive(Debug, Clone)]
pub struct ProviderEvent {
    pub id: String,
    pub event_type: String,
    pub livemode: bool,
    /// Unix seconds
    pub created: i64,
    pub object_id: String,
    pub object_type: String,
}

#[async_trait]
pub trait PaymentProvider: Send + Sync {
    /// Short name stored on orders, e.g. `stripe`
    fn name(&self) -> &'static str;

    /// Live keys (real money)
    fn livemode(&self) -> bool;

    /// Make a customer for a member (no personal data is sent)
    async fn create_customer(&self, user_id: &str) -> Result<String>;

    async fn create_checkout(&self, request: &CheckoutRequest) -> Result<CheckoutSession>;

    async fn fetch_checkout(&self, id: &str) -> Result<CheckoutSession>;

    /// Close an open checkout so it can't be paid any more
    async fn expire_checkout(&self, id: &str) -> Result<CheckoutSession>;

    async fn fetch_subscription(&self, id: &str) -> Result<ProviderSubscription>;

    /// Stop (true) or resume (false) renewal at the period end
    async fn set_cancel_at_period_end(&self, id: &str, cancel: bool)
        -> Result<ProviderSubscription>;

    /// End a subscription now (no more charges)
    async fn cancel_subscription_now(&self, id: &str) -> Result<ProviderSubscription>;

    async fn fetch_invoice(&self, id: &str) -> Result<ProviderInvoice>;

    async fn fetch_payment(&self, id: &str) -> Result<ProviderPayment>;

    /// Payment a charge belongs to
    async fn fetch_charge_payment(&self, charge_id: &str) -> Result<Option<String>>;

    /// Give money back. The key makes a repeated request refund once.
    async fn create_refund(
        &self,
        payment_id: &str,
        amount: Option<u32>,
        idempotency_key: &str,
    ) -> Result<ProviderRefund>;

    async fn fetch_refund(&self, id: &str) -> Result<ProviderRefund>;

    async fn fetch_dispute(&self, id: &str) -> Result<ProviderDispute>;

    /// Page where a member manages payment methods and sees invoices
    async fn create_portal(&self, customer_id: &str, return_url: &str) -> Result<String>;

    /// Check a webhook's signature on the raw body and read the event
    async fn verify_webhook(
        &self,
        headers: &WebhookHeaders,
        body: &[u8],
        now: i64,
    ) -> Result<ProviderEvent>;

    /// What an event type means
    fn event_kind(&self, event_type: &str) -> EventKind;

    /// Finish a checkout the buyer approved and return it as it now is.
    /// Most providers settle on their own; PayPal needs the server to
    /// capture. Only called for orders still open on Zrzur's side.
    async fn complete_checkout(&self, id: &str) -> Result<CheckoutSession> {
        self.fetch_checkout(id).await
    }
}

/// Build one configured provider, if it is ready and fits the mode
fn build(entry: &BillingProvider, live: bool) -> Option<Box<dyn PaymentProvider>> {
    if provider_ready(entry) != Some(live) {
        return None;
    }
    let provider: Box<dyn PaymentProvider> = match entry.name.as_str() {
        "stripe" => Box::new(StripeProvider::new(
            entry.secret_key()?,
            entry.webhook_secrets(),
            entry.api_base.clone(),
        )?),
        "paypal" => Box::new(PayPalProvider::new(
            entry.client_id()?,
            entry.secret_key()?,
            entry.webhook_secrets().into_iter().next()?,
            live,
            entry.api_base.clone(),
            entry.plus_price_id.clone(),
            entry.supercharge_price_id.clone(),
        )?),
        _ => return None,
    };
    // keys were checked when the mode was worked out; check again in case
    // the environment changed since
    if provider.livemode() != live {
        log::error!("billing: {} keys no longer match the billing mode", entry.name);
        return None;
    }
    Some(provider)
}

/// Every provider taking payments right now, in order of preference
pub fn ready_providers(billing: &FeaturesBilling) -> Vec<Box<dyn PaymentProvider>> {
    let mode = billing.mode();
    if !mode.uses_provider() {
        return vec![];
    }
    billing
        .ready_providers()
        .into_iter()
        .filter_map(|entry| build(entry, mode.live()))
        .collect()
}

/// The provider an order, customer or webhook names, if it still runs
pub fn provider_named(billing: &FeaturesBilling, name: &str) -> Option<Box<dyn PaymentProvider>> {
    ready_providers(billing)
        .into_iter()
        .find(|provider| provider.name() == name)
}

/// The settings entry of a provider (its price and plan ids)
pub fn provider_settings<'a>(billing: &'a FeaturesBilling, name: &str) -> Option<&'a BillingProvider> {
    billing.all_providers().find(|entry| entry.name == name)
}

/// The preferred provider (the first ready one), for new purchases when
/// the buyer didn't pick one
pub fn provider(billing: &FeaturesBilling) -> Option<Box<dyn PaymentProvider>> {
    ready_providers(billing).into_iter().next()
}
