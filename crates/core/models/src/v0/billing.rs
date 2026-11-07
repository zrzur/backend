use iso8601_timestamp::Timestamp;

use super::{DiscoverBot, DiscoverServer};

#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// How a Plus subscription came to be
    #[serde(tag = "type")]
    pub enum PlusSource {
        /// Bought by the member, renews monthly until cancelled
        Purchase,
        /// Given by another member for a fixed time, does not renew
        Gift {
            /// Who gave it
            from: String,
            /// Note from the giver
            #[serde(skip_serializing_if = "Option::is_none")]
            note: Option<String>,
        },
        /// Granted by Zrzur staff for a fixed time (recorded in the
        /// billing audit log), does not renew
        Grant,
    }

    /// State of a subscription at the payment provider
    #[derive(Default, Copy)]
    pub enum SubscriptionStatus {
        /// Paid up
        #[default]
        Active,
        /// A renewal payment failed and is being retried; perks continue
        /// for the grace period
        PastDue,
        /// Payment needs the member (e.g. 3-D Secure) before it can go on
        Incomplete,
        /// Ended
        Canceled,
    }

    /// A member's Zrzur Feather subscription
    pub struct PlusSubscription {
        /// Unique Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// When Plus started
        pub started_at: Timestamp,
        /// When the current period ends (renewal or expiry)
        pub current_period_end: Timestamp,
        /// Ends at the period end instead of renewing
        pub cancel_at_period_end: bool,
        /// Bought or gifted
        pub source: PlusSource,
        /// Made in test mode: no money was involved
        pub test: bool,
        /// State at the payment provider (bought ones)
        #[cfg_attr(feature = "serde", serde(default))]
        pub status: SubscriptionStatus,
        /// Managed by a payment provider (cancel there, invoices there)
        #[cfg_attr(feature = "serde", serde(default))]
        pub provider_managed: bool,
    }

    /// Where a supercharge comes from
    pub enum SuperchargeSource {
        /// Included with Plus, lasts as long as Plus does
        Plus,
        /// Bought on its own, renews monthly until cancelled
        Purchase,
    }

    /// One supercharge owned by a member
    pub struct Supercharge {
        /// Unique Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Owner
        pub user_id: String,
        /// Server it is on, if it is placed
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub server_id: Option<String>,
        /// Included with Plus or bought
        pub source: SuperchargeSource,
        /// When it was made
        pub created_at: Timestamp,
        /// When the current period ends (bought ones only)
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub current_period_end: Option<Timestamp>,
        /// Ends at the period end instead of renewing
        pub cancel_at_period_end: bool,
        /// Made in test mode: no money was involved
        pub test: bool,
        /// Order that bought it (bought ones)
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub order_id: Option<String>,
        /// Provider subscription that renews it; every feather bought in
        /// one purchase shares it and is cancelled together
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub provider_subscription_id: Option<String>,
    }

    /// A server's supercharge state, shown on the server
    #[derive(Default)]
    pub struct ServerSupercharge {
        /// Active supercharges on the server
        pub count: u32,
        /// Level reached (0 = none)
        pub level: u32,
    }

    /// Perks of one supercharge level
    pub struct SuperchargeLevelInfo {
        /// Level number, from 1
        pub level: u32,
        /// Supercharges needed
        pub supercharges: u32,
        /// Emoji slots
        pub server_emoji: u32,
        /// Attachment size for everyone (bytes)
        pub attachments: u64,
        /// Voice bitrate (bits per second)
        pub voice_bitrate: u32,
        /// Stream resolution (width, height)
        pub video_resolution: [u32; 2],
        /// Stream frame rate
        pub video_framerate: u32,
        /// The server may claim a custom invite link (zrzur.cc/<name>)
        #[cfg_attr(feature = "serde", serde(default))]
        pub vanity_invite: bool,
    }

    /// Limits of a plan, for comparing free and Plus
    pub struct PlanLimits {
        pub message_length: u32,
        /// Attachment size (bytes)
        pub attachments: u64,
        pub bots: u32,
        pub video_resolution: [u32; 2],
        pub video_framerate: u32,
    }

    /// How a product is paid for
    pub enum ProductBilling {
        /// Renews every month until cancelled
        Monthly,
        /// Paid once for a fixed time
        OneTime,
    }

    /// Something that can be bought, with its server-set price
    pub struct CatalogProduct {
        /// Stable product id, e.g. `plus`, `feather`, `discover.boost`
        pub id: String,
        pub billing: ProductBilling,
        /// Price in minor units for one unit and one period
        pub price: u32,
        pub currency: String,
        /// Days one purchase lasts (one-time products)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub days: Option<u32>,
        /// Most units in one purchase
        pub max_quantity: u32,
    }

    /// Prices and perks
    pub struct BillingPlans {
        /// Whether billing is on (store, perks). Purchases also need
        /// `checkout_available`.
        pub available: bool,
        /// Purchases can be started right now. False while live payments
        /// are not open yet: show that instead of a pay button.
        #[serde(default)]
        pub checkout_available: bool,
        /// `internal_test`, `sandbox`, `live_ready` or `live_enabled`
        #[serde(default)]
        pub mode: String,
        /// Payment provider (`test` for the simulated one)
        #[serde(default)]
        pub provider: String,
        /// Every provider a buyer can pay with right now, preferred first
        /// (`stripe`, `paypal`; `test` for simulated payments)
        #[serde(default)]
        pub providers: Vec<String>,
        /// Payment methods and invoices can be managed at the provider
        #[serde(default)]
        pub portal_available: bool,
        /// Everything that can be bought
        #[serde(default)]
        pub products: Vec<CatalogProduct>,
        /// No real money: simulated or provider sandbox
        pub test_mode: bool,
        /// ISO currency code
        pub currency: String,
        /// Monthly Plus price in minor units
        pub plus_price: u32,
        /// Monthly supercharge price in minor units
        pub supercharge_price: u32,
        /// Supercharges included with Plus
        pub plus_free_supercharges: u32,
        /// Emoji slots of a server with no supercharges
        pub free_server_emoji: u32,
        /// Gift lengths in months
        pub gift_months: Vec<u32>,
        /// Plus members get a role in the official server
        pub hq_lounge: bool,
        pub free: PlanLimits,
        pub plus: PlanLimits,
        pub levels: Vec<SuperchargeLevelInfo>,
    }

    /// Your Plus and supercharges
    pub struct BillingOverview {
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "Option::is_none", default)
        )]
        pub plus: Option<PlusSubscription>,
        /// Plus badge hidden from others
        pub hide_badge: bool,
        pub supercharges: Vec<Supercharge>,
    }

    /// One member supercharging a server
    pub struct ServerSupercharger {
        pub user_id: String,
        pub count: u32,
        /// Earliest supercharge still active
        pub since: Timestamp,
    }

    /// Supercharge state of a server with who is supercharging it
    pub struct ServerSuperchargeInfo {
        pub count: u32,
        pub level: u32,
        pub superchargers: Vec<ServerSupercharger>,
    }

    /// Give Plus to someone
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataGiftPlus {
        /// Who gets it
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub user: String,
        /// 1, 3 or 12
        pub months: u32,
        /// Note shown to them
        #[cfg_attr(feature = "validator", validate(length(max = 200)))]
        pub note: Option<String>,
    }

    /// Put a supercharge on a server
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataApplySupercharge {
        /// Server to supercharge
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub server: String,
        /// One of your supercharges to move there; without it, an unplaced one
        /// is used, or a new one is bought
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub supercharge: Option<String>,
    }

    /// Change how Plus shows
    pub struct DataEditPlus {
        /// Hide the Plus badge from others
        pub hide_badge: Option<bool>,
        /// Cancel (true) or keep renewing (false)
        pub cancel_at_period_end: Option<bool>,
    }

    /// A boost tier you can buy, with where it would put your server
    pub struct DiscoverBoostOffer {
        /// Boost, SuperBoost or MegaBoost
        pub kind: String,
        /// Price in minor units for `days`
        pub price: u32,
        /// Guaranteed place on Discover (this or higher)
        pub guarantee: u32,
        /// Place the server would have with this boost
        pub projected_rank: u32,
        /// Places of this tier
        pub capacity: u32,
        /// Places taken right now
        pub taken: u32,
        /// People waiting for a place
        pub waiting: u32,
        /// Your place in line, if you're waiting
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub position: Option<u32>,
    }

    /// A running boost
    pub struct DiscoverActiveBoost {
        pub kind: String,
        pub ends_at: Timestamp,
    }

    /// A running top-row placement and how it is doing
    pub struct DiscoverPromotedStats {
        pub started_at: Timestamp,
        pub ends_at: Timestamp,
        /// People who saw it in the top row (once a day each)
        pub views: u64,
        /// People who opened it from Discover (once a day each)
        pub clicks: u64,
        /// People who joined from Discover while it was promoted
        pub joins: u64,
    }

    /// Promotion state of a server or bot, for people who manage it
    pub struct DiscoverPromotionStatus {
        /// `server` or `bot`
        pub target_kind: String,
        /// Billing is on
        pub available: bool,
        /// Purchases can be started right now (false while live payments
        /// are not open yet)
        #[serde(default)]
        pub checkout_available: bool,
        pub test_mode: bool,
        pub currency: String,
        /// Length of one purchase
        pub days: u32,
        /// Approved for Discover by moderators
        pub approved: bool,
        /// Shows on Discover right now
        pub listed: bool,
        /// Place on Discover (popular order), if listed
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub rank: Option<u32>,
        pub member_count: u32,
        /// Members needed to show without a boost
        pub min_members: u32,
        /// Biggest running boost
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub boost: Option<DiscoverActiveBoost>,
        pub boosts: Vec<DiscoverBoostOffer>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub promoted: Option<DiscoverPromotedStats>,
        pub promoted_price: u32,
        pub promoted_slots: u32,
        /// Top-row places free right now
        pub promoted_open: u32,
        /// People waiting for a top-row place
        pub promoted_waiting: u32,
        /// Your place in that line, if you're waiting
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub promoted_position: Option<u32>,
        /// Signed-in people who browsed Discover in the last 7 days
        pub weekly_browsers: u64,
    }

    /// The paid top row of Discover
    pub struct DiscoverPromotedRow {
        pub slots: u32,
        pub open: u32,
        pub servers: Vec<DiscoverServer>,
    }

    /// The paid top row of Discover's bots
    pub struct DiscoverPromotedBots {
        pub slots: u32,
        pub open: u32,
        pub bots: Vec<DiscoverBot>,
    }

    /// Buy a boost
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataDiscoverBoost {
        /// Boost, SuperBoost or MegaBoost
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub kind: String,
    }

    /// Something a person did with a Discover listing
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataDiscoverEvent {
        /// `view` (seen in the top row) or `click` (opened)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 16)))]
        pub kind: String,
    }

    /// What an order buys. Prices are always worked out by the server.
    #[serde(tag = "type")]
    pub enum OrderProduct {
        /// The member subscription for yourself, monthly
        Plus,
        /// The member subscription for someone else, paid once
        PlusGift {
            user: String,
            months: u32,
            #[serde(skip_serializing_if = "Option::is_none", default)]
            note: Option<String>,
        },
        /// Feathers (supercharges), monthly each, placed on a server
        Supercharge {
            server: String,
            /// How many (1 when left out)
            #[serde(skip_serializing_if = "Option::is_none", default)]
            quantity: Option<u32>,
        },
        /// A Discover boost for the promotion length, paid once
        DiscoverBoost { server: String, kind: String },
        /// A top-row place on Discover for the promotion length, paid once
        DiscoverPromoted { server: String },
        /// A Discover boost for a bot you own, paid once
        BotBoost { bot: String, kind: String },
        /// A place in the bots' top row on Discover, paid once
        BotPromoted { bot: String },
    }

    /// Where an order is. Allowed moves are in `OrderStatus::can_move_to`.
    #[derive(Copy, Hash)]
    pub enum OrderStatus {
        /// Made, checkout not opened yet
        Created,
        /// Waiting for payment (checkout open)
        Pending,
        /// The payer has to act (e.g. 3-D Secure)
        RequiresAction,
        /// Payment submitted, the provider is confirming it
        Processing,
        /// Paid (and delivered, or being delivered)
        Paid,
        /// Payment failed for good
        Failed,
        /// Not paid in time or cancelled
        Cancelled,
        /// All money given back
        Refunded,
        /// Some money given back
        PartiallyRefunded,
        /// The payer disputed the payment with their bank
        Disputed,
    }

    /// Why an order exists
    pub enum OrderKind {
        /// Bought by the member
        Purchase,
        /// A monthly renewal of a subscription
        Renewal,
    }

    /// One payment, as it shows in your billing history
    pub struct Order {
        #[serde(rename = "_id")]
        pub id: String,
        pub product: OrderProduct,
        pub kind: OrderKind,
        /// Amount in minor units
        pub amount: u32,
        pub currency: String,
        pub status: OrderStatus,
        /// Payment provider (`test` while in test mode)
        pub provider: String,
        pub created_at: Timestamp,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub paid_at: Option<Timestamp>,
        /// Why it failed, was cancelled or refunded
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub failure: Option<String>,
        /// Made in test mode: no money was involved
        pub test: bool,
        /// Units bought
        #[serde(default = "one")]
        pub quantity: u32,
        /// Amount given back so far, in minor units
        #[serde(default)]
        pub amount_refunded: u32,
        /// What was paid for has been delivered
        #[serde(default)]
        pub fulfilled: bool,
        /// Unpaid checkout closes at this time
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub checkout_expires_at: Option<Timestamp>,
        /// Invoice page at the provider
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub invoice_url: Option<String>,
        /// Receipt page at the provider
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub receipt_url: Option<String>,
    }

    /// Start a purchase
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateOrder {
        pub product: OrderProduct,
        /// How to pay (one of `BillingPlans::providers`); the preferred
        /// one when left out
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub provider: Option<String>,
        /// Random key from the client; sending the same key again returns
        /// the same order instead of buying twice
        #[cfg_attr(feature = "validator", validate(length(min = 16, max = 64)))]
        pub idempotency_key: String,
    }

    /// An order and how to pay for it
    pub struct OrderCheckout {
        pub order: Order,
        /// Page of the payment provider, when it has one. In test mode the
        /// app confirms the payment itself.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub checkout_url: Option<String>,
    }

    /// Change a supercharge
    pub struct DataEditSupercharge {
        /// Cancel (true) or keep renewing (false)
        pub cancel_at_period_end: Option<bool>,
        /// Take it off its server
        pub unplace: Option<bool>,
    }

    /// Page at the payment provider to manage payment methods and invoices
    pub struct BillingPortal {
        pub url: String,
    }

    /// An order with everything staff need to look into it
    pub struct AdminOrder {
        pub order: Order,
        pub user_id: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_checkout_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_payment_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_subscription_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub provider_invoice_id: Option<String>,
        /// Delivery state: none, processing, done or failed
        pub fulfilment: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub fulfilment_error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub dispute_status: Option<String>,
    }

    /// One webhook event as it was received and processed
    pub struct WebhookEventInfo {
        #[serde(rename = "_id")]
        pub id: String,
        pub provider: String,
        /// Provider event type, e.g. `checkout.session.completed`
        pub event_type: String,
        pub livemode: bool,
        /// Received, Processed, Ignored or Failed
        pub status: String,
        pub attempts: u32,
        pub received_at: Timestamp,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub processed_at: Option<Timestamp>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub last_error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub order_id: Option<String>,
    }

    /// Something billing did that staff can audit
    pub struct BillingAuditEntry {
        #[serde(rename = "_id")]
        pub id: String,
        /// Who did it (`system` for automatic steps)
        pub actor_id: String,
        /// e.g. `order.paid`, `refund.requested`, `plus.granted`
        pub action: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub order_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub user_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub detail: Option<String>,
        pub created_at: Timestamp,
    }

    /// A difference between Zrzur and the payment provider
    pub struct ReconciliationIssue {
        #[serde(rename = "_id")]
        pub id: String,
        /// e.g. `paid_not_recorded`, `paid_not_delivered`, `status_mismatch`
        pub kind: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub order_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub user_id: Option<String>,
        pub detail: String,
        /// Fixed automatically under the safe policy (delivering a payment
        /// the provider confirmed); everything else waits for staff
        pub auto_fixed: bool,
        pub found_at: Timestamp,
        pub resolved: bool,
    }

    /// Give money back for an order (staff only, needs an MFA ticket)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataRefundOrder {
        /// Minor units; the whole remaining amount when left out
        pub amount: Option<u32>,
        /// Why (kept in the audit log)
        #[cfg_attr(feature = "validator", validate(length(min = 3, max = 500)))]
        pub reason: String,
        /// Also take away what was bought (default: only for full refunds)
        pub revoke: Option<bool>,
    }

    /// Give Plus to someone without payment (staff only, needs an MFA
    /// ticket, kept in the audit log)
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataGrantPlus {
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub user: String,
        /// 1 to 366
        pub days: u32,
        #[cfg_attr(feature = "validator", validate(length(min = 3, max = 500)))]
        pub reason: String,
    }

    /// Billing state staff can read: mode, gate and totals
    pub struct BillingAdminSummary {
        pub mode: String,
        pub provider: String,
        /// Live gate checks and whether each passes (no secrets)
        pub live_gate: Vec<LiveGateCheck>,
        pub orders_by_status: Vec<StatusCount>,
        pub webhooks_failed: u64,
        pub webhooks_pending: u64,
        pub open_issues: u64,
    }

    pub struct LiveGateCheck {
        pub name: String,
        pub passed: bool,
    }

    pub struct StatusCount {
        pub status: OrderStatus,
        pub count: u64,
    }
);

fn one() -> u32 {
    1
}

impl OrderStatus {
    /// Statuses an order may move to from this one. Paid orders never go
    /// back to unpaid ones, and nothing becomes Paid from Failed or
    /// Cancelled except a payment the provider confirms for a checkout
    /// that was closed meanwhile (money taken must be recorded).
    pub fn can_move_to(self, to: OrderStatus) -> bool {
        use OrderStatus::*;
        match (self, to) {
            (Created, Pending | Cancelled | Failed) => true,
            (Pending, RequiresAction | Processing | Paid | Failed | Cancelled) => true,
            (RequiresAction, Pending | Processing | Paid | Failed | Cancelled) => true,
            (Processing, Paid | Failed | RequiresAction) => true,
            // the provider took money after the checkout was given up
            (Cancelled | Failed, Paid) => true,
            (Paid, PartiallyRefunded | Refunded | Disputed) => true,
            (PartiallyRefunded, PartiallyRefunded | Refunded | Disputed) => true,
            // dispute won (back to paid) or lost (money gone)
            (Disputed, Paid | PartiallyRefunded | Refunded) => true,
            _ => false,
        }
    }

    /// Statuses that can move to `to`
    pub fn sources_of(to: OrderStatus) -> Vec<OrderStatus> {
        use OrderStatus::*;
        [
            Created,
            Pending,
            RequiresAction,
            Processing,
            Paid,
            Failed,
            Cancelled,
            Refunded,
            PartiallyRefunded,
            Disputed,
        ]
        .into_iter()
        .filter(|from| from.can_move_to(to))
        .collect()
    }

    /// Money was taken at some point
    pub fn was_paid(self) -> bool {
        matches!(
            self,
            OrderStatus::Paid
                | OrderStatus::Refunded
                | OrderStatus::PartiallyRefunded
                | OrderStatus::Disputed
        )
    }

    /// Still waiting for the payer or the provider
    pub fn is_open(self) -> bool {
        matches!(
            self,
            OrderStatus::Created
                | OrderStatus::Pending
                | OrderStatus::RequiresAction
                | OrderStatus::Processing
        )
    }
}

#[cfg(test)]
mod tests {
    use super::OrderStatus::{self, *};

    const ALL: [OrderStatus; 10] = [
        Created,
        Pending,
        RequiresAction,
        Processing,
        Paid,
        Failed,
        Cancelled,
        Refunded,
        PartiallyRefunded,
        Disputed,
    ];

    #[test]
    fn paid_orders_never_become_unpaid() {
        for to in [Created, Pending, RequiresAction, Processing, Failed, Cancelled] {
            assert!(!Paid.can_move_to(to), "Paid -> {to:?}");
            assert!(!Refunded.can_move_to(to), "Refunded -> {to:?}");
            assert!(!Disputed.can_move_to(to), "Disputed -> {to:?}");
        }
    }

    #[test]
    fn refunded_is_final() {
        for to in ALL {
            assert!(!Refunded.can_move_to(to), "Refunded -> {to:?}");
        }
    }

    #[test]
    fn nothing_goes_back_to_created() {
        for from in ALL {
            assert!(!from.can_move_to(Created));
        }
    }

    #[test]
    fn refunds_need_a_payment() {
        for from in [Created, Pending, RequiresAction, Processing, Failed, Cancelled] {
            assert!(!from.can_move_to(Refunded));
            assert!(!from.can_move_to(PartiallyRefunded));
            assert!(!from.can_move_to(Disputed));
        }
    }

    #[test]
    fn sources_match_rules() {
        assert_eq!(
            OrderStatus::sources_of(Paid),
            vec![Pending, RequiresAction, Processing, Failed, Cancelled, Disputed]
        );
        assert!(OrderStatus::sources_of(Created).is_empty());
    }
}
