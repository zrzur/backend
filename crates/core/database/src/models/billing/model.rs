use iso8601_timestamp::{Duration, Timestamp};
use revolt_models::v0;

auto_derived!(
    /// A member's Plus subscription. Ended ones are kept as history.
    pub struct PlusSubscription {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,
        /// Member
        pub user_id: String,
        pub started_at: Timestamp,
        /// Renewal (bought) or expiry (gifted) time
        pub current_period_end: Timestamp,
        #[serde(default)]
        pub cancel_at_period_end: bool,
        pub source: v0::PlusSource,
        /// Made in test mode: no money was involved
        #[serde(default)]
        pub test: bool,
        /// No longer active
        #[serde(default)]
        pub ended: bool,
        /// State at the payment provider
        #[serde(default)]
        pub status: v0::SubscriptionStatus,
        /// Provider subscription that renews it (bought with a provider)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_subscription_id: Option<String>,
        /// Order that started it
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub order_id: Option<String>,
        /// Gift and grant orders whose time was added, so adding the same
        /// one twice changes nothing
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub applied_orders: Vec<String>,
        /// Time added by gifts and grants on top of the provider's period
        /// (milliseconds), kept when the provider reports a new period end
        #[serde(default)]
        pub extra_ms: i64,
    }

    /// Plus state kept on the user, so limits don't need a lookup
    #[derive(Default)]
    pub struct UserPlus {
        /// Plus is active until this time
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub until: Option<Timestamp>,
        /// Plus badge hidden from others
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub hide_badge: bool,
        /// Bought with real money (only counts on a live server; anything
        /// else only counts on test and sandbox servers)
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub live: bool,
    }
);

auto_derived!(
    /// A paid Discover place. Every tier has a fixed number of places per
    /// kind of listing, one document each (id `{server|bot}:{kind}:{n}`),
    /// taken in one database step, so a place is never sold twice. Times are
    /// milliseconds since the epoch so they can be compared and extended in
    /// one step.
    pub struct DiscoverPromotion {
        #[serde(rename = "_id")]
        pub id: String,
        /// Boost, SuperBoost, MegaBoost or Promoted
        pub kind: String,
        /// `server` or `bot`
        #[serde(default = "default_target_kind")]
        pub target_kind: String,
        /// Server or bot id
        pub server_id: String,
        /// Who paid last
        pub buyer_id: String,
        pub started_at: i64,
        pub ends_at: i64,
        #[serde(default)]
        pub test: bool,
        #[serde(default)]
        pub views: i64,
        #[serde(default)]
        pub clicks: i64,
        #[serde(default)]
        pub joins: i64,
        /// Orders whose time was added to this place since it last
        /// changed hands, so one order never adds twice
        #[serde(default)]
        pub orders: Vec<String>,
    }
);

auto_derived!(
    /// A payment. Its id is `{user}-{idempotency key}`, so the same request
    /// sent twice finds the first order instead of buying again.
    pub struct BillingOrder {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub product: v0::OrderProduct,
        pub kind: v0::OrderKind,
        pub amount: u32,
        pub currency: String,
        pub status: v0::OrderStatus,
        pub provider: String,
        /// Milliseconds since the epoch
        pub created_at: i64,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub paid_at: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub failure: Option<String>,
        #[serde(default)]
        pub test: bool,
        /// Units bought
        #[serde(default = "one")]
        pub quantity: u32,
        /// Price of one unit (amount = unit_amount × quantity)
        #[serde(default)]
        pub unit_amount: u32,
        /// Provider checkout session
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_checkout_id: Option<String>,
        /// Provider checkout page, kept so a reload returns to the same one
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub checkout_url: Option<String>,
        /// Milliseconds since the epoch
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub checkout_expires_at: Option<i64>,
        /// Provider customer the checkout was made for
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_customer_id: Option<String>,
        /// Provider payment (payment intent)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_payment_id: Option<String>,
        /// Provider subscription (monthly products)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_subscription_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_invoice_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub invoice_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub receipt_url: Option<String>,
        /// Given back so far, minor units
        #[serde(default)]
        pub amount_refunded: u32,
        /// Delivery of what was paid for
        #[serde(default)]
        pub fulfilment: Fulfilment,
        /// Delivery is being done until this time (milliseconds); a crashed
        /// delivery is picked up again after it
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub fulfilment_lease: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub fulfilment_error: Option<String>,
        #[serde(default)]
        pub fulfilment_attempts: u32,
        /// Perks were taken back (refund or lost dispute)
        #[serde(default)]
        pub revoked: bool,
        /// Provider dispute state
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub dispute_status: Option<String>,
        /// Last time the order was checked with the provider (milliseconds)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub synced_at: Option<i64>,
    }

    /// Delivery of an order
    #[derive(Default, Copy)]
    pub enum Fulfilment {
        /// Not delivered (not paid yet, or nothing to deliver)
        #[default]
        None,
        /// Being delivered now (see `fulfilment_lease`)
        Processing,
        Done,
        /// Delivery failed; retried by the billing worker
        Failed,
    }

    /// A provider webhook event. Its id is `{provider}:{event id}`, so the
    /// same event is stored and handled once however often it is sent. The
    /// payload is not kept: handling fetches the current object from the
    /// provider, which also makes the order events arrive in harmless.
    pub struct BillingWebhookEvent {
        #[serde(rename = "_id")]
        pub id: String,
        pub provider: String,
        pub event_type: String,
        /// Object the event is about (checkout session, invoice, …)
        pub object_id: String,
        pub livemode: bool,
        /// Provider's creation time (milliseconds)
        pub created: i64,
        /// Milliseconds since the epoch
        pub received_at: i64,
        pub status: WebhookEventStatus,
        #[serde(default)]
        pub attempts: u32,
        /// Retry after this time (milliseconds)
        #[serde(default)]
        pub next_attempt_at: i64,
        /// Being handled until this time (milliseconds)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub lease: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub processed_at: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub last_error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub order_id: Option<String>,
    }

    #[derive(Copy)]
    pub enum WebhookEventStatus {
        Received,
        Processed,
        /// Not about anything Zrzur sells, or nothing to do
        Ignored,
        /// Gave up after too many attempts (staff can look into it)
        Failed,
    }

    /// A member's customer record at a payment provider
    pub struct BillingCustomer {
        /// `{provider}:{livemode}:{user id}`
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        pub provider: String,
        pub livemode: bool,
        pub customer_id: String,
        pub created_at: i64,
    }
);

fn one() -> u32 {
    1
}

impl BillingOrder {
    /// A new order waiting for payment (Created; one unit)
    pub fn new(
        id: String,
        user_id: String,
        product: v0::OrderProduct,
        amount: u32,
        currency: String,
        provider: &str,
        test: bool,
    ) -> Self {
        BillingOrder {
            id,
            user_id,
            product,
            kind: v0::OrderKind::Purchase,
            amount,
            currency,
            status: v0::OrderStatus::Created,
            provider: provider.to_string(),
            created_at: epoch_ms(Timestamp::now_utc()),
            paid_at: None,
            failure: None,
            test,
            quantity: 1,
            unit_amount: amount,
            provider_checkout_id: None,
            checkout_url: None,
            checkout_expires_at: None,
            provider_customer_id: None,
            provider_payment_id: None,
            provider_subscription_id: None,
            provider_invoice_id: None,
            invoice_url: None,
            receipt_url: None,
            amount_refunded: 0,
            fulfilment: Fulfilment::None,
            fulfilment_lease: None,
            fulfilment_error: None,
            fulfilment_attempts: 0,
            revoked: false,
            dispute_status: None,
            synced_at: None,
        }
    }
}

/// Something billing did, for staff to audit
pub type BillingAuditEntry = v0::BillingAuditEntry;

/// A difference found between Zrzur and the provider
pub type ReconciliationIssue = v0::ReconciliationIssue;

impl From<BillingOrder> for v0::Order {
    fn from(value: BillingOrder) -> Self {
        v0::Order {
            id: value.id,
            product: value.product,
            kind: value.kind,
            amount: value.amount,
            currency: value.currency,
            status: value.status,
            provider: value.provider,
            created_at: from_epoch_ms(value.created_at),
            paid_at: value.paid_at.map(from_epoch_ms),
            failure: value.failure,
            test: value.test,
            quantity: value.quantity,
            amount_refunded: value.amount_refunded,
            fulfilled: value.fulfilment == Fulfilment::Done,
            checkout_expires_at: if value.status.is_open() {
                value.checkout_expires_at.map(from_epoch_ms)
            } else {
                None
            },
            invoice_url: value.invoice_url,
            receipt_url: value.receipt_url,
        }
    }
}

impl From<BillingOrder> for v0::AdminOrder {
    fn from(value: BillingOrder) -> Self {
        v0::AdminOrder {
            user_id: value.user_id.clone(),
            provider_checkout_id: value.provider_checkout_id.clone(),
            provider_payment_id: value.provider_payment_id.clone(),
            provider_subscription_id: value.provider_subscription_id.clone(),
            provider_invoice_id: value.provider_invoice_id.clone(),
            fulfilment: format!("{:?}", value.fulfilment).to_lowercase(),
            fulfilment_error: value.fulfilment_error.clone(),
            dispute_status: value.dispute_status.clone(),
            order: value.into(),
        }
    }
}

impl From<BillingWebhookEvent> for v0::WebhookEventInfo {
    fn from(value: BillingWebhookEvent) -> Self {
        v0::WebhookEventInfo {
            id: value.id,
            provider: value.provider,
            event_type: value.event_type,
            livemode: value.livemode,
            status: format!("{:?}", value.status),
            attempts: value.attempts,
            received_at: from_epoch_ms(value.received_at),
            processed_at: value.processed_at.map(from_epoch_ms),
            last_error: value.last_error,
            order_id: value.order_id,
        }
    }
}

auto_derived!(
    /// Someone who paid while every place of a tier was taken. They start,
    /// first come first served, when a place frees up.
    pub struct DiscoverWaitlistEntry {
        #[serde(rename = "_id")]
        pub id: String,
        /// `server` or `bot`
        pub target_kind: String,
        /// Server or bot id
        pub target_id: String,
        /// Boost, SuperBoost, MegaBoost or Promoted
        pub kind: String,
        pub buyer_id: String,
        /// Order that paid for it
        pub order_id: String,
        /// Days bought
        pub days: u32,
        /// Milliseconds since the epoch; the line is ordered by this
        pub created_at: i64,
        #[serde(default)]
        pub test: bool,
    }
);

fn default_target_kind() -> String {
    "server".to_string()
}

/// Kind of a top-row placement
pub const PROMOTED_KIND: &str = "Promoted";

/// Id of the n-th place of a tier
pub fn promotion_slot_id(target_kind: &str, kind: &str, n: usize) -> String {
    format!("{target_kind}:{kind}:{n}")
}

/// A supercharge as stored (same shape as the API model)
pub type Supercharge = v0::Supercharge;

/// Milliseconds since the epoch
pub fn epoch_ms(value: Timestamp) -> i64 {
    value
        .duration_since(Timestamp::UNIX_EPOCH)
        .whole_milliseconds() as i64
}

/// Timestamp from milliseconds since the epoch
pub fn from_epoch_ms(ms: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + Duration::milliseconds(ms)
}

/// Length of one billing period
pub fn billing_month() -> Duration {
    Duration::days(30)
}

impl UserPlus {
    /// Whether Plus is active right now
    pub fn active(&self) -> bool {
        self.until.is_some_and(|until| until > Timestamp::now_utc())
    }
}

impl From<PlusSubscription> for v0::PlusSubscription {
    fn from(value: PlusSubscription) -> Self {
        v0::PlusSubscription {
            id: value.id,
            started_at: value.started_at,
            current_period_end: value.current_period_end,
            cancel_at_period_end: value.cancel_at_period_end,
            source: value.source,
            test: value.test,
            status: value.status,
            provider_managed: value.provider_subscription_id.is_some(),
        }
    }
}
