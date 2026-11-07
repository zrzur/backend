use iso8601_timestamp::Timestamp;
use revolt_result::Result;

use revolt_models::v0::{OrderStatus, SubscriptionStatus};

use crate::{
    BillingAuditEntry, BillingCustomer, BillingOrder, BillingWebhookEvent, DiscoverPromotion,
    DiscoverWaitlistEntry, PlusSubscription, ReconciliationIssue, Supercharge, WebhookEventStatus,
};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

/// Which promotion counter to bump
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromotionStat {
    Views,
    Clicks,
    Joins,
}

impl PromotionStat {
    pub fn field(self) -> &'static str {
        match self {
            PromotionStat::Views => "views",
            PromotionStat::Clicks => "clicks",
            PromotionStat::Joins => "joins",
        }
    }
}

/// Buy or extend one promotion document atomically
pub struct PromotionPurchase<'a> {
    /// Place id, see `promotion_slot_id`
    pub id: &'a str,
    pub kind: &'a str,
    /// `server` or `bot`
    pub target_kind: &'a str,
    pub server_id: &'a str,
    pub buyer_id: &'a str,
    pub now_ms: i64,
    /// Time bought
    pub add_ms: i64,
    pub test: bool,
    /// Only take the document if it is free (ended or already this
    /// server's); used for top-row places
    pub only_if_free: bool,
    /// Order paying for it (recorded so it never adds twice)
    pub order_id: &'a str,
}

/// Fields to set on an order (None leaves a field as it is)
#[derive(Default, Clone, Debug)]
pub struct OrderChanges {
    pub paid_at: Option<i64>,
    pub failure: Option<String>,
    pub provider_checkout_id: Option<String>,
    pub checkout_url: Option<String>,
    pub checkout_expires_at: Option<i64>,
    pub provider_customer_id: Option<String>,
    pub provider_payment_id: Option<String>,
    pub provider_subscription_id: Option<String>,
    pub provider_invoice_id: Option<String>,
    pub invoice_url: Option<String>,
    pub receipt_url: Option<String>,
    pub amount_refunded: Option<u32>,
    pub dispute_status: Option<String>,
    pub revoked: Option<bool>,
}

/// Staff search over orders
#[derive(Default, Clone, Debug)]
pub struct OrderSearch {
    pub user_id: Option<String>,
    pub status: Option<OrderStatus>,
    /// Matches an order id or any provider id exactly
    pub reference: Option<String>,
    /// Only orders whose delivery failed
    pub fulfilment_failed: bool,
    /// Created before this time (milliseconds), for paging
    pub before_ms: Option<i64>,
}

/// Which provider id to find an order by
#[derive(Clone, Copy, Debug)]
pub enum ProviderRef {
    Checkout,
    Payment,
    Subscription,
    Invoice,
}

impl ProviderRef {
    pub fn field(self) -> &'static str {
        match self {
            ProviderRef::Checkout => "provider_checkout_id",
            ProviderRef::Payment => "provider_payment_id",
            ProviderRef::Subscription => "provider_subscription_id",
            ProviderRef::Invoice => "provider_invoice_id",
        }
    }
}

#[async_trait]
pub trait AbstractBilling: Sync + Send {
    /// The member's Plus subscription that has not ended
    async fn fetch_plus_subscription(&self, user_id: &str) -> Result<Option<PlusSubscription>>;

    /// Start a subscription. Fails if one is already running for that
    /// member (the running one is stored under the member's id).
    async fn insert_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()>;

    /// Save changes to a running subscription
    async fn save_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()>;

    /// Add time to a running subscription in one step
    async fn extend_plus_subscription(&self, id: &str, add_ms: i64) -> Result<()>;

    /// Move a running subscription into history
    async fn end_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()>;

    /// Subscriptions that have not ended whose period is over
    async fn fetch_lapsed_plus_subscriptions(
        &self,
        now: Timestamp,
    ) -> Result<Vec<PlusSubscription>>;

    async fn fetch_supercharge(&self, id: &str) -> Result<Supercharge>;

    async fn fetch_supercharges_by_user(&self, user_id: &str) -> Result<Vec<Supercharge>>;

    async fn fetch_supercharges_by_server(&self, server_id: &str) -> Result<Vec<Supercharge>>;

    /// Bought supercharges whose period is over
    async fn fetch_lapsed_supercharges(&self, now: Timestamp) -> Result<Vec<Supercharge>>;

    /// Save a supercharge (insert or replace)
    async fn save_supercharge(&self, supercharge: &Supercharge) -> Result<()>;

    /// Insert a supercharge unless one with its id exists already
    async fn ensure_supercharge(&self, supercharge: &Supercharge) -> Result<()>;

    /// Move a supercharge from one place to another, only if it is still
    /// where the caller saw it. Returns whether it moved.
    async fn move_supercharge(
        &self,
        id: &str,
        user_id: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<bool>;

    async fn delete_supercharge(&self, id: &str) -> Result<()>;

    /// Take every supercharge off a server (when it is deleted)
    async fn unplace_supercharges_from_server(&self, server_id: &str) -> Result<()>;

    /// Servers that have supercharges, or show a supercharge state
    async fn fetch_supercharge_server_ids(&self) -> Result<Vec<String>>;

    /// Buy or extend a promotion. Returns false when a top-row place is
    /// taken by another server.
    async fn purchase_promotion(&self, purchase: PromotionPurchase<'_>) -> Result<bool>;

    /// Promotions running at this time
    async fn fetch_active_promotions(&self, now_ms: i64) -> Result<Vec<DiscoverPromotion>>;

    /// Count something on a promotion
    async fn increment_promotion_stat(&self, id: &str, stat: PromotionStat) -> Result<()>;

    /// Save a new order. Returns false if one with its id exists already.
    async fn insert_order(&self, order: &BillingOrder) -> Result<bool>;

    async fn fetch_order(&self, id: &str) -> Result<BillingOrder>;

    /// A member's orders, newest first
    async fn fetch_orders_by_user(&self, user_id: &str, limit: i64) -> Result<Vec<BillingOrder>>;

    /// Move an order from one status to another in one step. Returns
    /// whether it was still in `from` (so only one caller wins).
    async fn transition_order(
        &self,
        id: &str,
        from: OrderStatus,
        to: OrderStatus,
        paid_at: Option<i64>,
        failure: Option<String>,
    ) -> Result<bool>;

    /// Cancel orders still pending since before this time
    async fn cancel_stale_orders(&self, before_ms: i64) -> Result<()>;

    /// Join the line for a full tier
    async fn insert_waitlist_entry(&self, entry: &DiscoverWaitlistEntry) -> Result<()>;

    /// The whole line, oldest first
    async fn fetch_waitlist(&self) -> Result<Vec<DiscoverWaitlistEntry>>;

    async fn delete_waitlist_entry(&self, id: &str) -> Result<()>;

    /// Free every place and leave every line held by a server or bot that
    /// is being deleted
    async fn delete_promotions_for_target(&self, target_id: &str) -> Result<()>;

    // --- payments ---

    /// Move an order to `to` in one step, from any status allowed to move
    /// there (`OrderStatus::can_move_to`), setting `changes` too. Returns
    /// whether it moved (so only one caller wins).
    async fn move_order(&self, id: &str, to: OrderStatus, changes: &OrderChanges)
        -> Result<bool>;

    /// Set fields on an order without moving it
    async fn update_order(&self, id: &str, changes: &OrderChanges) -> Result<()>;

    /// Find an order by a provider id
    async fn fetch_order_by_provider(
        &self,
        reference: ProviderRef,
        value: &str,
    ) -> Result<Option<BillingOrder>>;

    /// Take delivery of a paid order for a while. Returns false when it is
    /// delivered already or someone else is delivering it.
    async fn claim_fulfilment(&self, id: &str, now_ms: i64, lease_until: i64) -> Result<bool>;

    /// Record how a delivery went
    async fn finish_fulfilment(&self, id: &str, ok: bool, error: Option<String>) -> Result<()>;

    /// Paid orders not delivered yet (failed, never started, or a crashed
    /// delivery whose lease ran out)
    async fn fetch_undelivered_orders(&self, now_ms: i64, max_attempts: u32)
        -> Result<Vec<BillingOrder>>;

    /// Orders waiting at a provider that were made before this time
    async fn fetch_open_provider_orders(&self, before_ms: i64, limit: i64)
        -> Result<Vec<BillingOrder>>;

    /// Note that an order was checked with the provider, unless that
    /// happened after `not_since`. Returns whether this caller may check.
    async fn mark_order_synced(&self, id: &str, now_ms: i64, not_since: i64) -> Result<bool>;

    /// Staff search, newest first
    async fn search_orders(&self, search: &OrderSearch, limit: i64) -> Result<Vec<BillingOrder>>;

    /// Number of orders per status
    async fn count_orders_by_status(&self) -> Result<Vec<(OrderStatus, u64)>>;

    /// Store a webhook event. Returns false when it was stored before.
    async fn insert_webhook_event(&self, event: &BillingWebhookEvent) -> Result<bool>;

    async fn fetch_webhook_event(&self, id: &str) -> Result<Option<BillingWebhookEvent>>;

    /// Take a received event for handling for a while. Returns false when
    /// it is handled already, being handled, or not due yet.
    async fn claim_webhook_event(&self, id: &str, now_ms: i64, lease_until: i64) -> Result<bool>;

    /// Record how handling went
    async fn finish_webhook_event(
        &self,
        id: &str,
        status: WebhookEventStatus,
        error: Option<String>,
        next_attempt_at: i64,
        order_id: Option<String>,
        now_ms: i64,
    ) -> Result<()>;

    /// Received events due for (another) attempt, oldest first
    async fn fetch_due_webhook_events(&self, now_ms: i64, limit: i64)
        -> Result<Vec<BillingWebhookEvent>>;

    /// Events for staff, newest first
    async fn fetch_webhook_events(
        &self,
        status: Option<WebhookEventStatus>,
        limit: i64,
    ) -> Result<Vec<BillingWebhookEvent>>;

    async fn count_webhook_events(&self, status: WebhookEventStatus) -> Result<u64>;

    async fn fetch_billing_customer(&self, id: &str) -> Result<Option<BillingCustomer>>;

    /// Store a customer. Returns false when one with its id exists already.
    async fn insert_billing_customer(&self, customer: &BillingCustomer) -> Result<bool>;

    async fn insert_billing_audit(&self, entry: &BillingAuditEntry) -> Result<()>;

    /// Audit entries for an order or member (or all), newest first
    async fn fetch_billing_audit(
        &self,
        order_id: Option<&str>,
        user_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<BillingAuditEntry>>;

    /// Save a reconciliation issue (same id: replaced, so runs don't pile
    /// up duplicates)
    async fn save_reconciliation_issue(&self, issue: &ReconciliationIssue) -> Result<()>;

    async fn fetch_reconciliation_issues(&self, resolved: bool, limit: i64)
        -> Result<Vec<ReconciliationIssue>>;

    async fn count_open_reconciliation_issues(&self) -> Result<u64>;

    // --- entitlements bought through a provider ---

    async fn fetch_plus_by_provider_subscription(&self, id: &str)
        -> Result<Option<PlusSubscription>>;

    /// Take on the provider's view of a subscription: status, renewal
    /// switch and period end (plus any time added by gifts or grants)
    async fn set_plus_provider_state(
        &self,
        id: &str,
        status: SubscriptionStatus,
        period_end_ms: i64,
        cancel_at_period_end: bool,
    ) -> Result<()>;

    /// Add time for an order to a running subscription, once. Returns
    /// false when that order's time was added before.
    async fn add_plus_time(&self, id: &str, order_id: &str, add_ms: i64) -> Result<bool>;

    /// Take an order's time off again. Returns false when it wasn't added.
    async fn remove_plus_time(&self, id: &str, order_id: &str, remove_ms: i64) -> Result<bool>;

    async fn fetch_supercharges_by_order(&self, order_id: &str) -> Result<Vec<Supercharge>>;

    async fn fetch_supercharges_by_provider_subscription(&self, id: &str)
        -> Result<Vec<Supercharge>>;

    /// Take on the provider's view of a feather subscription
    async fn set_supercharges_provider_state(
        &self,
        subscription_id: &str,
        period_end_ms: i64,
        cancel_at_period_end: bool,
    ) -> Result<()>;

    /// The place an order's time went to, if it is still there
    async fn fetch_promotion_by_order(&self, order_id: &str) -> Result<Option<DiscoverPromotion>>;

    /// Take an order's time off a place again. Returns whether it was there.
    async fn remove_promotion_time(&self, id: &str, order_id: &str, remove_ms: i64)
        -> Result<bool>;
}
