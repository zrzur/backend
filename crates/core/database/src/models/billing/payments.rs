//! Payments: the order state machine, checkout with a provider, trusted
//! verification, delivery exactly once, refunds, disputes, webhooks and
//! reconciliation.
//!
//! Rules kept everywhere here:
//! - Prices come from the server's catalog; nothing the browser sends is
//!   trusted except which product and how many.
//! - An order only becomes Paid after the provider's API confirms it
//!   (a webhook only says "look again"), with amount, currency, order,
//!   customer and live/test mode matching.
//! - Status moves are single conditional database updates
//!   (`OrderStatus::can_move_to`), so two callers can't both win.
//! - Delivery takes a lease and every grant is keyed by the order, so
//!   retries and duplicate events never deliver twice.
//! - Nothing here ever creates a payment or refund on its own; refunds
//!   only happen when staff ask (`request_refund`).

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::{config, FeaturesBilling};
use revolt_models::v0::{
    self, CatalogProduct, OrderKind, OrderProduct, OrderStatus, PlusSource, ProductBilling,
    SubscriptionStatus, SuperchargeSource,
};
use revolt_result::{create_error, Result};
use ulid::Ulid;

use crate::{
    billing_month, claim_promotion, epoch_ms, from_epoch_ms, provider_named, provider_settings,
    ready_providers, sync_server_supercharge, EventKind, WebhookHeaders,
    sync_user_plus, BillingAuditEntry, BillingCustomer, BillingOrder, BillingWebhookEvent,
    CheckoutLine, CheckoutMode, CheckoutPaymentStatus, CheckoutRequest, CheckoutSession,
    CheckoutStatus, Database, DiscoverWaitlistEntry, Fulfilment, OrderChanges, OrderSearch,
    PaymentProvider, PlusSubscription, ProviderRef, ProviderSubscription, ReconciliationIssue,
    Supercharge, WebhookEventStatus, PROMOTED_KIND,
};

/// Actor id of automatic steps in the audit log
pub const SYSTEM_ACTOR: &str = "system";

/// Gift lengths in months
pub const GIFT_MONTHS: [u32; 3] = [1, 3, 12];

const MONTH_MS: i64 = 30 * 86_400_000;
const DAY_MS: i64 = 86_400_000;
/// How long one delivery attempt may take before another may start
const FULFILMENT_LEASE_MS: i64 = 5 * 60_000;
/// Deliveries tried before staff are asked to look
pub const MAX_FULFILMENT_ATTEMPTS: u32 = 8;
const WEBHOOK_LEASE_MS: i64 = 2 * 60_000;
const MAX_WEBHOOK_ATTEMPTS: u32 = 12;

fn now_ms() -> i64 {
    epoch_ms(Timestamp::now_utc())
}

/// Record something in the billing audit log (never fails the caller)
pub async fn audit(
    db: &Database,
    actor: &str,
    action: &str,
    order_id: Option<&str>,
    user_id: Option<&str>,
    detail: Option<String>,
) {
    let entry = BillingAuditEntry {
        id: Ulid::new().to_string(),
        actor_id: actor.to_string(),
        action: action.to_string(),
        order_id: order_id.map(String::from),
        user_id: user_id.map(String::from),
        detail,
        created_at: Timestamp::now_utc(),
    };
    if let Err(error) = db.insert_billing_audit(&entry).await {
        log::error!("billing: audit {action} not saved: {error:?}");
    }
}

// --- catalog ---

/// Server-side price of an order
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Priced {
    pub product_id: String,
    pub unit_amount: u32,
    pub quantity: u32,
    pub monthly: bool,
    pub name: String,
}

impl Priced {
    pub fn amount(&self) -> Result<u32> {
        self.unit_amount
            .checked_mul(self.quantity)
            .filter(|amount| *amount > 0)
            .ok_or_else(|| create_error!(InvalidOperation))
    }
}

/// Display name of a Discover promotion tier (internal kinds stay Boost / SuperBoost / MegaBoost)
fn discover_tier_label(kind: &str) -> &str {
    match kind {
        "Boost" => "Featured",
        "SuperBoost" => "Spotlight",
        "MegaBoost" => "Headline",
        other => other,
    }
}

fn boost_product_id(kind: &str) -> String {
    format!("discover.{}", kind.to_ascii_lowercase())
}

/// Everything that can be bought, with server-set prices
pub fn catalog(billing: &FeaturesBilling) -> Vec<CatalogProduct> {
    let currency = billing.currency.clone();
    let days = billing.discover.promotion_days;
    let mut products = vec![
        CatalogProduct {
            id: "plus".to_string(),
            billing: ProductBilling::Monthly,
            price: billing.plus_price,
            currency: currency.clone(),
            days: None,
            max_quantity: 1,
        },
        CatalogProduct {
            id: "plus.gift".to_string(),
            billing: ProductBilling::OneTime,
            price: billing.plus_price,
            currency: currency.clone(),
            days: Some(30),
            max_quantity: *GIFT_MONTHS.iter().max().unwrap_or(&1),
        },
        CatalogProduct {
            id: "feather".to_string(),
            billing: ProductBilling::Monthly,
            price: billing.supercharge_price,
            currency: currency.clone(),
            days: None,
            max_quantity: billing.max_supercharges_per_user as u32,
        },
    ];
    for tier in &billing.discover.boosts {
        products.push(CatalogProduct {
            id: boost_product_id(&tier.kind),
            billing: ProductBilling::OneTime,
            price: tier.price,
            currency: currency.clone(),
            days: Some(days),
            max_quantity: 1,
        });
    }
    products.push(CatalogProduct {
        id: "discover.top_row".to_string(),
        billing: ProductBilling::OneTime,
        price: billing.discover.promoted_price,
        currency,
        days: Some(days),
        max_quantity: 1,
    });
    products.retain(|product| product.price > 0);
    products
}

/// Price of a product from the catalog. Checks only what the product
/// itself allows (quantity, gift length, tier); who may buy it is checked
/// by the caller.
pub fn price_of(product: &OrderProduct, billing: &FeaturesBilling) -> Result<Priced> {
    let boost = |kind: &str| -> Result<Priced> {
        let tier = billing
            .discover
            .boosts
            .iter()
            .find(|tier| tier.kind == kind)
            .ok_or_else(|| create_error!(InvalidOperation))?;
        Ok(Priced {
            product_id: boost_product_id(kind),
            unit_amount: tier.price,
            quantity: 1,
            monthly: false,
            name: format!(
                "Zrzur Discover {} ({} days)",
                discover_tier_label(&tier.kind),
                billing.discover.promotion_days
            ),
        })
    };
    let top_row = || Priced {
        product_id: "discover.top_row".to_string(),
        unit_amount: billing.discover.promoted_price,
        quantity: 1,
        monthly: false,
        name: format!(
            "Zrzur Discover top row ({} days)",
            billing.discover.promotion_days
        ),
    };

    let priced = match product {
        OrderProduct::Plus => Priced {
            product_id: "plus".to_string(),
            unit_amount: billing.plus_price,
            quantity: 1,
            monthly: true,
            name: "Zrzur Feather".to_string(),
        },
        OrderProduct::PlusGift { months, .. } => {
            if !GIFT_MONTHS.contains(months) {
                return Err(create_error!(InvalidOperation));
            }
            Priced {
                product_id: "plus.gift".to_string(),
                unit_amount: billing.plus_price,
                quantity: *months,
                monthly: false,
                name: format!("Zrzur Feather gift ({months} months)"),
            }
        }
        OrderProduct::Supercharge { quantity, .. } => {
            let quantity = quantity.unwrap_or(1);
            if quantity == 0 || quantity as usize > billing.max_supercharges_per_user {
                return Err(create_error!(FailedValidation {
                    error: "quantity".to_string()
                }));
            }
            Priced {
                product_id: "feather".to_string(),
                unit_amount: billing.supercharge_price,
                quantity,
                monthly: true,
                name: "Zrzur Wings".to_string(),
            }
        }
        OrderProduct::DiscoverBoost { kind, .. } | OrderProduct::BotBoost { kind, .. } => {
            boost(kind)?
        }
        OrderProduct::DiscoverPromoted { .. } | OrderProduct::BotPromoted { .. } => top_row(),
    };
    priced.amount()?;
    Ok(priced)
}

/// Who receives what an order buys
pub fn beneficiary(order: &BillingOrder) -> &str {
    match &order.product {
        OrderProduct::PlusGift { user, .. } => user,
        _ => &order.user_id,
    }
}

// --- checkout ---

/// The member's customer at the provider, made once
pub async fn ensure_customer(
    db: &Database,
    provider: &dyn PaymentProvider,
    user_id: &str,
) -> Result<String> {
    let id = format!("{}:{}:{}", provider.name(), provider.livemode(), user_id);
    if let Some(customer) = db.fetch_billing_customer(&id).await? {
        return Ok(customer.customer_id);
    }
    // the provider call is idempotent per member, so a race makes one
    let customer_id = provider.create_customer(user_id).await?;
    let customer = BillingCustomer {
        id: id.clone(),
        user_id: user_id.to_string(),
        provider: provider.name().to_string(),
        livemode: provider.livemode(),
        customer_id: customer_id.clone(),
        created_at: now_ms(),
    };
    if !db.insert_billing_customer(&customer).await? {
        if let Some(existing) = db.fetch_billing_customer(&id).await? {
            return Ok(existing.customer_id);
        }
    }
    Ok(customer_id)
}

/// Return addresses: built from `hosts.app`, carrying only the order id
pub fn return_urls(app: &str, order_id: &str) -> (String, String) {
    let base = app.trim_end_matches('/');
    (
        format!("{base}/?billing_order={order_id}&billing_return=success"),
        format!("{base}/?billing_order={order_id}&billing_return=cancel"),
    )
}

/// Open a hosted checkout for a new order (Created → Pending). A failure
/// leaves the order Created, so trying again (same idempotency key) can
/// still open it.
pub async fn open_checkout(
    db: &Database,
    provider: &dyn PaymentProvider,
    billing: &FeaturesBilling,
    order: &BillingOrder,
) -> Result<BillingOrder> {
    if order.status != OrderStatus::Created {
        return db.fetch_order(&order.id).await;
    }
    let config = config().await;
    let customer_id = ensure_customer(db, provider, &order.user_id).await?;
    let priced = price_of(&order.product, billing)?;
    // the order was priced when made; never charge anything else
    if priced.amount()? != order.amount || priced.quantity != order.quantity {
        return Err(create_error!(InvalidOperation));
    }

    let minutes = billing.provider.checkout_minutes.clamp(30, 1440) as i64;
    let expires_at = order.created_at / 1000 + minutes * 60;
    let (success_url, cancel_url) = return_urls(&config.hosts.app, &order.id);
    // each provider has its own price (Stripe) or plan (PayPal) ids
    let settings = provider_settings(billing, provider.name());
    let price_id = match &order.product {
        OrderProduct::Plus => settings.map(|entry| entry.plus_price_id.clone()),
        OrderProduct::Supercharge { .. } => settings.map(|entry| entry.supercharge_price_id.clone()),
        _ => None,
    }
    .filter(|id| !id.is_empty());

    let session = provider
        .create_checkout(&CheckoutRequest {
            order_id: order.id.clone(),
            user_id: order.user_id.clone(),
            customer_id: customer_id.clone(),
            mode: if priced.monthly {
                CheckoutMode::Subscription
            } else {
                CheckoutMode::Payment
            },
            line: CheckoutLine {
                price_id,
                unit_amount: priced.unit_amount,
                currency: order.currency.to_lowercase(),
                name: priced.name.clone(),
                quantity: priced.quantity,
            },
            success_url,
            cancel_url,
            expires_at,
        })
        .await?;

    if session.livemode != provider.livemode() || session.url.is_none() {
        log::error!("billing: checkout for {} came back unusable", order.id);
        return Err(create_error!(ProxyError));
    }

    db.move_order(
        &order.id,
        OrderStatus::Pending,
        &OrderChanges {
            provider_checkout_id: Some(session.id.clone()),
            checkout_url: session.url.clone(),
            checkout_expires_at: Some(session.expires_at * 1000),
            provider_customer_id: Some(customer_id),
            ..Default::default()
        },
    )
    .await?;
    audit(
        db,
        &order.user_id,
        "checkout.opened",
        Some(&order.id),
        Some(&order.user_id),
        Some(format!("{} {}", priced.product_id, session.id)),
    )
    .await;
    db.fetch_order(&order.id).await
}

/// Why a checkout session can't be accepted for an order (None: it can)
pub fn session_mismatch(
    order: &BillingOrder,
    session: &CheckoutSession,
    livemode: bool,
) -> Option<&'static str> {
    if order.provider_checkout_id.as_deref() != Some(session.id.as_str()) {
        return Some("session");
    }
    if session.client_reference_id.as_deref() != Some(order.id.as_str())
        || session.order_id.as_deref() != Some(order.id.as_str())
    {
        return Some("order");
    }
    if session.livemode != livemode || order.test == livemode {
        return Some("livemode");
    }
    if order.provider_customer_id.is_some() && session.customer != order.provider_customer_id {
        return Some("customer");
    }
    if session.status != CheckoutStatus::Complete
        || session.payment_status != CheckoutPaymentStatus::Paid
    {
        return Some("unpaid");
    }
    if session.currency.as_deref() != Some(order.currency.to_lowercase().as_str()) {
        return Some("currency");
    }
    let subtotal = session.amount_subtotal.or(session.amount_total);
    if subtotal != Some(order.amount as i64) {
        return Some("amount");
    }
    // taxes may add to the total; nothing may take from it (no discounts
    // are offered)
    if session.amount_total.is_some_and(|total| total < order.amount as i64) {
        return Some("discount");
    }
    let monthly = matches!(
        order.product,
        OrderProduct::Plus | OrderProduct::Supercharge { .. }
    );
    if monthly != session.subscription.is_some() {
        return Some("mode");
    }
    None
}

/// What checking an order with the provider found
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutOutcome {
    Paid,
    StillOpen,
    Closed,
    Rejected(&'static str),
}

/// Ask the provider about an order's checkout and act on the answer. This
/// is the only way a provider order becomes Paid.
pub async fn verify_checkout(
    db: &Database,
    provider: &dyn PaymentProvider,
    order_id: &str,
) -> Result<CheckoutOutcome> {
    let order = db.fetch_order(order_id).await?;
    let Some(session_id) = order.provider_checkout_id.clone() else {
        return Ok(CheckoutOutcome::StillOpen);
    };
    if order.provider != provider.name() {
        return Err(create_error!(InvalidOperation));
    }
    // a checkout is completed (PayPal: captured) only for an order still
    // open here: a cancelled or expired order is never charged
    let session = if order.status.is_open() {
        provider.complete_checkout(&session_id).await?
    } else {
        provider.fetch_checkout(&session_id).await?
    };

    if session.status == CheckoutStatus::Expired {
        if db
            .move_order(
                &order.id,
                OrderStatus::Cancelled,
                &OrderChanges {
                    failure: Some("expired".to_string()),
                    ..Default::default()
                },
            )
            .await?
        {
            audit(db, SYSTEM_ACTOR, "order.expired", Some(&order.id), Some(&order.user_id), None)
                .await;
        }
        return Ok(CheckoutOutcome::Closed);
    }
    if session.status == CheckoutStatus::Open {
        return Ok(CheckoutOutcome::StillOpen);
    }

    if let Some(reason) = session_mismatch(&order, &session, provider.livemode()) {
        if reason == "unpaid" {
            // completed but the money is still on its way (bank transfers)
            db.move_order(&order.id, OrderStatus::Processing, &OrderChanges::default())
                .await?;
            return Ok(CheckoutOutcome::StillOpen);
        }
        log::error!("billing: checkout {} refused for {}: {reason}", session.id, order.id);
        audit(
            db,
            SYSTEM_ACTOR,
            "checkout.rejected",
            Some(&order.id),
            Some(&order.user_id),
            Some(reason.to_string()),
        )
        .await;
        save_issue(
            db,
            "checkout_mismatch",
            Some(&order),
            format!("checkout {} does not match the order: {reason}", session.id),
            false,
        )
        .await;
        return Ok(CheckoutOutcome::Rejected(reason));
    }

    if order.status != OrderStatus::Paid {
        let moved = db
            .move_order(
                &order.id,
                OrderStatus::Paid,
                &OrderChanges {
                    paid_at: Some(now_ms()),
                    provider_payment_id: session.payment_intent.clone(),
                    provider_subscription_id: session.subscription.clone(),
                    provider_invoice_id: session.invoice.clone(),
                    ..Default::default()
                },
            )
            .await?;
        if moved {
            audit(
                db,
                SYSTEM_ACTOR,
                "order.paid",
                Some(&order.id),
                Some(&order.user_id),
                Some(format!("{} {}", order.amount, order.currency)),
            )
            .await;
        }
    }
    attach_documents(db, provider, &order.id).await;
    if let Err(error) = fulfil_order(db, &order.id).await {
        // paid stays paid; the worker tries again
        log::warn!("billing: delivering {} failed: {error:?}", order.id);
    }
    Ok(CheckoutOutcome::Paid)
}

/// Store receipt and invoice links of a paid order (best effort)
async fn attach_documents(db: &Database, provider: &dyn PaymentProvider, order_id: &str) {
    let Ok(order) = db.fetch_order(order_id).await else {
        return;
    };
    let mut changes = OrderChanges::default();
    if let Some(invoice_id) = &order.provider_invoice_id {
        if let Ok(invoice) = provider.fetch_invoice(invoice_id).await {
            changes.invoice_url = invoice.hosted_invoice_url;
            if order.provider_payment_id.is_none() {
                changes.provider_payment_id = invoice.payment_intent;
            }
        }
    }
    let payment_id = changes
        .provider_payment_id
        .clone()
        .or(order.provider_payment_id.clone());
    if let Some(payment_id) = payment_id {
        if let Ok(payment) = provider.fetch_payment(&payment_id).await {
            changes.receipt_url = payment.receipt_url;
        }
    }
    let _ = db.update_order(order_id, &changes).await;
}

// --- delivery ---

/// Deliver a paid order, once. Safe to call any number of times and from
/// several places at once.
pub async fn fulfil_order(db: &Database, order_id: &str) -> Result<()> {
    let now = now_ms();
    if !db
        .claim_fulfilment(order_id, now, now + FULFILMENT_LEASE_MS)
        .await?
    {
        return Ok(());
    }
    let order = db.fetch_order(order_id).await?;
    let result = grant(db, &order).await;
    match &result {
        Ok(()) => {
            db.finish_fulfilment(order_id, true, None).await?;
            audit(
                db,
                SYSTEM_ACTOR,
                "order.delivered",
                Some(order_id),
                Some(beneficiary(&order)),
                None,
            )
            .await;
        }
        Err(error) => {
            let reason = format!("{:?}", error.error_type);
            db.finish_fulfilment(order_id, false, Some(reason.clone()))
                .await?;
            audit(
                db,
                SYSTEM_ACTOR,
                "order.delivery_failed",
                Some(order_id),
                Some(beneficiary(&order)),
                Some(reason.clone()),
            )
            .await;
            if order.provider == "test" {
                // simulated payments: nothing was charged, so nothing waits
                db.move_order(
                    order_id,
                    OrderStatus::Refunded,
                    &OrderChanges {
                        failure: Some(reason),
                        amount_refunded: Some(order.amount),
                        ..Default::default()
                    },
                )
                .await?;
            } else if order.fulfilment_attempts + 1 >= MAX_FULFILMENT_ATTEMPTS {
                save_issue(
                    db,
                    "paid_not_delivered",
                    Some(&order),
                    format!("delivery keeps failing: {reason}; refund or deliver by hand"),
                    false,
                )
                .await;
            }
        }
    }
    result
}

/// Period end of a provider subscription (falls back to a month from now
/// when the provider can't be asked)
async fn provider_period_end(order: &BillingOrder) -> (i64, Option<ProviderSubscription>) {
    let fallback = now_ms() + MONTH_MS;
    let Some(sub_id) = &order.provider_subscription_id else {
        return (fallback, None);
    };
    let billing = config().await.features.billing;
    let Some(provider) = provider_named(&billing, &order.provider) else {
        return (fallback, None);
    };
    match provider.fetch_subscription(sub_id).await {
        Ok(sub) if sub.current_period_end > 0 => (sub.current_period_end * 1000, Some(sub)),
        _ => (fallback, None),
    }
}

/// Give what an order paid for. Every step is keyed by the order, so
/// running it again changes nothing.
async fn grant(db: &Database, order: &BillingOrder) -> Result<()> {
    let billing = config().await.features.billing;
    let now = Timestamp::now_utc();
    match &order.product {
        OrderProduct::Plus => {
            let (period_end, provider_sub) = if order.provider == "test" {
                (epoch_ms(now + billing_month()), None)
            } else {
                provider_period_end(order).await
            };
            let status = provider_sub
                .as_ref()
                .map(|sub| subscription_status(&sub.status))
                .unwrap_or_default();
            if status == SubscriptionStatus::Canceled {
                // cancelled before delivery (e.g. refunded meanwhile)
                return Ok(());
            }

            let mut extra_ms = 0;
            let mut applied = vec![order.id.clone()];
            if let Some(existing) = db.fetch_plus_subscription(&order.user_id).await? {
                if existing.order_id.as_deref() == Some(order.id.as_str()) {
                    return sync_user_plus(db, &order.user_id).await;
                }
                if existing.provider_subscription_id.is_some() {
                    // a second paid subscription: staff refund one
                    return Err(create_error!(InvalidOperation));
                }
                // a gift or grant running: its remaining time goes on top
                extra_ms = (epoch_ms(existing.current_period_end) - epoch_ms(now)).max(0);
                applied.extend(existing.applied_orders.clone());
                db.end_plus_subscription(&existing).await?;
            }
            let subscription = PlusSubscription {
                id: order.user_id.clone(),
                user_id: order.user_id.clone(),
                started_at: now,
                current_period_end: from_epoch_ms(period_end + extra_ms),
                cancel_at_period_end: provider_sub
                    .as_ref()
                    .is_some_and(|sub| sub.cancel_at_period_end),
                source: PlusSource::Purchase,
                test: order.test,
                ended: false,
                status,
                provider_subscription_id: order.provider_subscription_id.clone(),
                order_id: Some(order.id.clone()),
                applied_orders: applied,
                extra_ms,
            };
            if let Err(error) = db.insert_plus_subscription(&subscription).await {
                // a parallel delivery of this order got there first
                let existing = db.fetch_plus_subscription(&order.user_id).await?;
                if existing.and_then(|sub| sub.order_id).as_deref() != Some(order.id.as_str()) {
                    return Err(error);
                }
            }
            sync_user_plus(db, &order.user_id).await
        }
        OrderProduct::PlusGift { user: to, months, note } => {
            let add_ms = MONTH_MS * *months as i64;
            let note = note
                .as_ref()
                .map(|note| note.trim().to_string())
                .filter(|note| !note.is_empty());
            add_plus_for_order(
                db,
                to,
                &order.id,
                add_ms,
                PlusSource::Gift {
                    from: order.user_id.clone(),
                    note,
                },
                order.test,
            )
            .await?;
            sync_user_plus(db, to).await
        }
        OrderProduct::Supercharge { server, quantity } => {
            let quantity = quantity.unwrap_or(1).max(1);
            let period_end = if order.provider == "test" {
                epoch_ms(now + billing_month())
            } else {
                provider_period_end(order).await.0
            };
            // still a member? otherwise they wait unplaced in their list
            let placed = db.fetch_member(server, &order.user_id).await.is_ok();
            for n in 0..quantity {
                db.ensure_supercharge(&Supercharge {
                    id: format!("{}:{n}", order.id),
                    user_id: order.user_id.clone(),
                    server_id: placed.then(|| server.clone()),
                    source: SuperchargeSource::Purchase,
                    created_at: now,
                    current_period_end: Some(from_epoch_ms(period_end)),
                    cancel_at_period_end: false,
                    test: order.test,
                    order_id: Some(order.id.clone()),
                    provider_subscription_id: order.provider_subscription_id.clone(),
                })
                .await?;
            }
            if placed {
                sync_server_supercharge(db, server).await?;
            }
            Ok(())
        }
        OrderProduct::DiscoverBoost { server, kind } => {
            place_or_queue(db, order, "server", server, kind, &billing).await
        }
        OrderProduct::DiscoverPromoted { server } => {
            place_or_queue(db, order, "server", server, PROMOTED_KIND, &billing).await
        }
        OrderProduct::BotBoost { bot, kind } => {
            place_or_queue(db, order, "bot", bot, kind, &billing).await
        }
        OrderProduct::BotPromoted { bot } => {
            place_or_queue(db, order, "bot", bot, PROMOTED_KIND, &billing).await
        }
    }
}

/// Add an order's Plus time for someone: onto their running Plus, or as a
/// new fixed-time Plus
async fn add_plus_for_order(
    db: &Database,
    user_id: &str,
    order_id: &str,
    add_ms: i64,
    source: PlusSource,
    test: bool,
) -> Result<()> {
    if db.fetch_user(user_id).await.is_err() {
        return Err(create_error!(UnknownUser));
    }
    for _ in 0..2 {
        if let Some(sub) = db.fetch_plus_subscription(user_id).await? {
            if sub.applied_orders.iter().any(|id| id == order_id) {
                return Ok(());
            }
            db.add_plus_time(&sub.id, order_id, add_ms).await?;
            return Ok(());
        }
        let now = Timestamp::now_utc();
        let inserted = db
            .insert_plus_subscription(&PlusSubscription {
                id: user_id.to_string(),
                user_id: user_id.to_string(),
                started_at: now,
                current_period_end: now + Duration::milliseconds(add_ms),
                cancel_at_period_end: false,
                source: source.clone(),
                test,
                ended: false,
                status: SubscriptionStatus::Active,
                provider_subscription_id: None,
                order_id: Some(order_id.to_string()),
                applied_orders: vec![order_id.to_string()],
                extra_ms: 0,
            })
            .await;
        if inserted.is_ok() {
            return Ok(());
        }
        // someone started one meanwhile: add onto that instead
    }
    Err(create_error!(InvalidOperation))
}

/// Take a place of a tier, or wait in line for one (paid already). The
/// line entry's id is the order id, so it can't be added twice.
async fn place_or_queue(
    db: &Database,
    order: &BillingOrder,
    target_kind: &str,
    target_id: &str,
    kind: &str,
    billing: &FeaturesBilling,
) -> Result<()> {
    if db.fetch_promotion_by_order(&order.id).await?.is_some()
        || db
            .fetch_waitlist()
            .await?
            .iter()
            .any(|entry| entry.id == order.id)
    {
        return Ok(());
    }
    let days = billing.discover.promotion_days;
    if claim_promotion(
        db,
        target_kind,
        target_id,
        kind,
        &order.user_id,
        days,
        order.test,
        &order.id,
    )
    .await?
    {
        return Ok(());
    }
    db.insert_waitlist_entry(&DiscoverWaitlistEntry {
        id: order.id.clone(),
        target_kind: target_kind.to_string(),
        target_id: target_id.to_string(),
        kind: kind.to_string(),
        buyer_id: order.user_id.clone(),
        order_id: order.id.clone(),
        days,
        created_at: now_ms(),
        test: order.test,
    })
    .await
    .or_else(|error| {
        // inserted by a parallel delivery of the same order
        if matches!(error.error_type, revolt_result::ErrorType::DatabaseError { .. }) {
            Ok(())
        } else {
            Err(error)
        }
    })
}

/// Take back what an order gave (refund or lost dispute). Safe to repeat.
pub async fn revoke(db: &Database, order: &BillingOrder, actor: &str, reason: &str) -> Result<()> {
    let billing = config().await.features.billing;
    let provider = provider_named(&billing, &order.provider);
    let now = Timestamp::now_utc();

    match &order.product {
        OrderProduct::Plus => {
            let sub = match &order.provider_subscription_id {
                Some(id) => db.fetch_plus_by_provider_subscription(id).await?,
                None => db
                    .fetch_plus_subscription(&order.user_id)
                    .await?
                    .filter(|sub| sub.order_id.as_deref() == Some(order.id.as_str())),
            };
            if let (Some(id), Some(provider)) = (&order.provider_subscription_id, &provider) {
                // no more charges for a subscription that was refunded
                if let Err(error) = provider.cancel_subscription_now(id).await {
                    log::warn!("billing: cancelling {id} after refund failed: {error:?}");
                }
            }
            if let Some(sub) = sub {
                db.end_plus_subscription(&sub).await?;
            }
            sync_user_plus(db, &order.user_id).await?;
        }
        OrderProduct::PlusGift { user: to, months, .. } => {
            if let Some(sub) = db.fetch_plus_subscription(to).await? {
                let remove_ms = MONTH_MS * *months as i64;
                if sub.order_id.as_deref() == Some(order.id.as_str())
                    && sub.applied_orders.len() <= 1
                    && sub.provider_subscription_id.is_none()
                {
                    // the gift was all of it
                    db.end_plus_subscription(&sub).await?;
                } else if db.remove_plus_time(&sub.id, &order.id, remove_ms).await? {
                    if let Some(after) = db.fetch_plus_subscription(to).await? {
                        if after.current_period_end <= now && after.provider_subscription_id.is_none()
                        {
                            db.end_plus_subscription(&after).await?;
                        }
                    }
                }
            }
            sync_user_plus(db, to).await?;
        }
        OrderProduct::Supercharge { .. } => {
            let mut items = db.fetch_supercharges_by_order(&order.id).await?;
            if let Some(id) = &order.provider_subscription_id {
                items.extend(db.fetch_supercharges_by_provider_subscription(id).await?);
                if let Some(provider) = &provider {
                    if let Err(error) = provider.cancel_subscription_now(id).await {
                        log::warn!("billing: cancelling {id} after refund failed: {error:?}");
                    }
                }
            }
            items.sort_by(|a, b| a.id.cmp(&b.id));
            items.dedup_by(|a, b| a.id == b.id);
            for item in items {
                db.delete_supercharge(&item.id).await?;
                if let Some(server_id) = item.server_id {
                    sync_server_supercharge(db, &server_id).await?;
                }
            }
        }
        OrderProduct::DiscoverBoost { .. }
        | OrderProduct::DiscoverPromoted { .. }
        | OrderProduct::BotBoost { .. }
        | OrderProduct::BotPromoted { .. } => {
            db.delete_waitlist_entry(&order.id).await?;
            if let Some(promotion) = db.fetch_promotion_by_order(&order.id).await? {
                let remove_ms = billing.discover.promotion_days as i64 * DAY_MS;
                db.remove_promotion_time(&promotion.id, &order.id, remove_ms)
                    .await?;
            }
        }
    }

    db.update_order(
        &order.id,
        &OrderChanges {
            revoked: Some(true),
            ..Default::default()
        },
    )
    .await?;
    audit(
        db,
        actor,
        "order.revoked",
        Some(&order.id),
        Some(beneficiary(order)),
        Some(reason.to_string()),
    )
    .await;
    Ok(())
}

// --- subscriptions ---

/// Our status for a provider subscription status
pub fn subscription_status(status: &str) -> SubscriptionStatus {
    match status {
        "active" | "trialing" => SubscriptionStatus::Active,
        "past_due" | "unpaid" | "paused" => SubscriptionStatus::PastDue,
        "incomplete" => SubscriptionStatus::Incomplete,
        _ => SubscriptionStatus::Canceled,
    }
}

/// Perks end at the provider's period end; while a renewal is retried
/// they last at most the grace period past the period that was paid for.
/// The paid period ends where the provider's current (unpaid) period
/// starts, so syncing again never stretches the grace.
pub fn effective_period_end(
    status: SubscriptionStatus,
    provider_end_ms: i64,
    period_start_ms: i64,
    grace_days: u32,
) -> i64 {
    match status {
        SubscriptionStatus::Active | SubscriptionStatus::Canceled => provider_end_ms,
        SubscriptionStatus::PastDue | SubscriptionStatus::Incomplete => {
            provider_end_ms.min(period_start_ms + grace_days as i64 * DAY_MS)
        }
    }
}

/// Take on the provider's current view of a subscription (Plus or
/// feathers). Fetching instead of trusting the event makes old or
/// repeated events harmless.
pub async fn sync_subscription(
    db: &Database,
    provider: &dyn PaymentProvider,
    subscription_id: &str,
) -> Result<Option<String>> {
    let sub = provider.fetch_subscription(subscription_id).await?;
    if sub.livemode != provider.livemode() {
        return Ok(None);
    }
    let billing = config().await.features.billing;
    let status = subscription_status(&sub.status);
    let provider_end = sub.current_period_end * 1000;
    // unknown start: no grace beyond now (fail closed)
    let provider_start = if sub.current_period_start > 0 {
        sub.current_period_start * 1000
    } else {
        now_ms()
    };
    let order = db
        .fetch_order_by_provider(ProviderRef::Subscription, &sub.id)
        .await?;

    if let Some(plus) = db.fetch_plus_by_provider_subscription(&sub.id).await? {
        if status == SubscriptionStatus::Canceled {
            db.end_plus_subscription(&plus).await?;
            audit(
                db,
                SYSTEM_ACTOR,
                "plus.ended",
                plus.order_id.as_deref(),
                Some(&plus.user_id),
                Some(sub.status.clone()),
            )
            .await;
        } else {
            let end = effective_period_end(
                status,
                provider_end,
                provider_start,
                billing.provider.past_due_grace_days,
            );
            db.set_plus_provider_state(&plus.id, status, end, sub.cancel_at_period_end)
                .await?;
        }
        sync_user_plus(db, &plus.user_id).await?;
    }

    let feathers = db
        .fetch_supercharges_by_provider_subscription(&sub.id)
        .await?;
    if !feathers.is_empty() {
        if status == SubscriptionStatus::Canceled {
            for item in feathers {
                db.delete_supercharge(&item.id).await?;
                if let Some(server_id) = item.server_id {
                    sync_server_supercharge(db, &server_id).await?;
                }
            }
        } else {
            let end = effective_period_end(
                status,
                provider_end,
                provider_start,
                billing.provider.past_due_grace_days,
            );
            db.set_supercharges_provider_state(&sub.id, end, sub.cancel_at_period_end)
                .await?;
        }
    }

    Ok(order.map(|order| order.id))
}

/// Order a provider object belongs to: by our metadata, else by its id
async fn order_for_subscription(
    db: &Database,
    provider: &dyn PaymentProvider,
    subscription_id: &str,
) -> Result<Option<BillingOrder>> {
    if let Some(order) = db
        .fetch_order_by_provider(ProviderRef::Subscription, subscription_id)
        .await?
    {
        return Ok(Some(order));
    }
    let sub = provider.fetch_subscription(subscription_id).await?;
    match sub.order_id {
        Some(order_id) => Ok(db.fetch_order(&order_id).await.ok()),
        None => Ok(None),
    }
}

/// An invoice was paid: the first one confirms a purchase, later ones are
/// renewals recorded once each (id `{provider}:{invoice}`)
pub async fn handle_invoice(
    db: &Database,
    provider: &dyn PaymentProvider,
    invoice_id: &str,
    paid: bool,
) -> Result<Option<String>> {
    let invoice = provider.fetch_invoice(invoice_id).await?;
    if invoice.livemode != provider.livemode() {
        return Ok(None);
    }
    let Some(subscription_id) = invoice.subscription.clone() else {
        return Ok(None);
    };
    let Some(purchase) = order_for_subscription(db, provider, &subscription_id).await? else {
        return Ok(None);
    };

    if invoice.billing_reason.as_deref() == Some("subscription_create") {
        if paid {
            // checkout events can be late or lost: look at the checkout too
            verify_checkout(db, provider, &purchase.id).await?;
        }
        return Ok(Some(purchase.id));
    }

    let renewal_id = format!("{}:{}", provider.name(), invoice.id);
    if invoice.currency != purchase.currency.to_lowercase() {
        save_issue(
            db,
            "renewal_currency",
            Some(&purchase),
            format!("invoice {} is in {}", invoice.id, invoice.currency),
            false,
        )
        .await;
    }
    let amount = if paid {
        invoice.amount_paid
    } else {
        invoice.amount_due
    };
    let mut renewal = BillingOrder::new(
        renewal_id.clone(),
        purchase.user_id.clone(),
        purchase.product.clone(),
        amount.clamp(0, u32::MAX as i64) as u32,
        purchase.currency.clone(),
        provider.name(),
        purchase.test,
    );
    renewal.kind = OrderKind::Renewal;
    renewal.status = if paid {
        OrderStatus::Paid
    } else {
        OrderStatus::Failed
    };
    renewal.paid_at = paid.then(now_ms);
    renewal.failure = (!paid).then(|| "renewal_payment_failed".to_string());
    renewal.quantity = purchase.quantity;
    renewal.unit_amount = purchase.unit_amount;
    renewal.provider_customer_id = invoice.customer.clone();
    renewal.provider_payment_id = invoice.payment_intent.clone();
    renewal.provider_subscription_id = Some(subscription_id.clone());
    renewal.provider_invoice_id = Some(invoice.id.clone());
    renewal.invoice_url = invoice.hosted_invoice_url.clone();
    // a renewal extends what the purchase gave; nothing new to deliver
    if paid {
        renewal.fulfilment = Fulfilment::Done;
    }
    if !db.insert_order(&renewal).await? && paid {
        // a failed attempt was recorded before and the retry succeeded
        db.move_order(
            &renewal_id,
            OrderStatus::Paid,
            &OrderChanges {
                paid_at: Some(now_ms()),
                provider_payment_id: invoice.payment_intent.clone(),
                invoice_url: invoice.hosted_invoice_url.clone(),
                ..Default::default()
            },
        )
        .await?;
    }
    audit(
        db,
        SYSTEM_ACTOR,
        if paid { "renewal.paid" } else { "renewal.failed" },
        Some(&renewal_id),
        Some(&purchase.user_id),
        None,
    )
    .await;
    sync_subscription(db, provider, &subscription_id).await?;
    Ok(Some(renewal_id))
}

// --- refunds and disputes ---

/// Take on the provider's refund total for a payment, and take perks back
/// on a full refund
pub async fn sync_refunds(
    db: &Database,
    provider: &dyn PaymentProvider,
    payment_id: &str,
) -> Result<Option<String>> {
    let Some(order) = db
        .fetch_order_by_provider(ProviderRef::Payment, payment_id)
        .await?
    else {
        return Ok(None);
    };
    let payment = provider.fetch_payment(payment_id).await?;
    if payment.livemode != provider.livemode() {
        return Ok(None);
    }
    let refunded = payment.amount_refunded.clamp(0, order.amount as i64) as u32;
    if refunded == 0 || refunded == order.amount_refunded && order.status != OrderStatus::Paid {
        return Ok(Some(order.id));
    }
    let full = refunded >= order.amount;
    let to = if full {
        OrderStatus::Refunded
    } else {
        OrderStatus::PartiallyRefunded
    };
    let changes = OrderChanges {
        amount_refunded: Some(refunded),
        ..Default::default()
    };
    if !db.move_order(&order.id, to, &changes).await? {
        db.update_order(&order.id, &changes).await?;
    }
    audit(
        db,
        SYSTEM_ACTOR,
        if full { "refund.full" } else { "refund.partial" },
        Some(&order.id),
        Some(&order.user_id),
        Some(format!("{refunded} of {} {}", order.amount, order.currency)),
    )
    .await;
    // policy: a full refund takes the perks back; a partial one keeps them
    if full && !order.revoked {
        revoke(db, &order, SYSTEM_ACTOR, "full refund").await?;
    }
    Ok(Some(order.id))
}

/// Staff give money back. Needs a provider order that was paid; asks the
/// provider with a key per order and amount, so a repeated click refunds
/// once.
pub async fn request_refund(
    db: &Database,
    order_id: &str,
    amount: Option<u32>,
    take_back: Option<bool>,
    actor: &str,
    reason: &str,
) -> Result<BillingOrder> {
    let billing = config().await.features.billing;
    let order = db.fetch_order(order_id).await?;
    if !matches!(
        order.status,
        OrderStatus::Paid | OrderStatus::PartiallyRefunded
    ) {
        return Err(create_error!(InvalidOperation));
    }
    let remaining = order.amount.saturating_sub(order.amount_refunded);
    let amount = amount.unwrap_or(remaining);
    if amount == 0 || amount > remaining {
        return Err(create_error!(FailedValidation {
            error: "amount".to_string()
        }));
    }
    audit(
        db,
        actor,
        "refund.requested",
        Some(&order.id),
        Some(&order.user_id),
        Some(format!("{amount} {}: {reason}", order.currency)),
    )
    .await;

    if order.provider == "test" {
        // simulated: nothing to give back at a provider
        let total = order.amount_refunded + amount;
        let full = total >= order.amount;
        db.move_order(
            &order.id,
            if full {
                OrderStatus::Refunded
            } else {
                OrderStatus::PartiallyRefunded
            },
            &OrderChanges {
                amount_refunded: Some(total),
                ..Default::default()
            },
        )
        .await?;
        if full || take_back == Some(true) {
            revoke(db, &order, actor, reason).await?;
        }
        return db.fetch_order(&order.id).await;
    }

    let provider = provider_named(&billing, &order.provider).ok_or_else(|| create_error!(FeatureDisabled {
        feature: "billing".to_string()
    }))?;
    if order.test == provider.livemode() {
        return Err(create_error!(InvalidOperation));
    }
    let payment_id = order
        .provider_payment_id
        .clone()
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let key = format!(
        "zrzur-refund-{}-{}-{}",
        order.id, order.amount_refunded, amount
    );
    let refund = provider
        .create_refund(&payment_id, Some(amount), &key)
        .await?;
    audit(
        db,
        actor,
        "refund.created",
        Some(&order.id),
        Some(&order.user_id),
        Some(format!("{} {}", refund.id, refund.status)),
    )
    .await;
    if refund.status == "failed" || refund.status == "canceled" {
        return Err(create_error!(OperationFailed));
    }
    if take_back == Some(true) && !order.revoked {
        revoke(db, &order, actor, reason).await?;
    }
    sync_refunds(db, provider.as_ref(), &payment_id).await?;
    db.fetch_order(&order.id).await
}

/// Take on a dispute's state. Policy: perks are taken back while a
/// dispute is open; a won dispute returns the order to Paid and asks
/// staff to look (perks are not given back automatically).
pub async fn sync_dispute(
    db: &Database,
    provider: &dyn PaymentProvider,
    dispute_id: &str,
) -> Result<Option<String>> {
    let dispute = provider.fetch_dispute(dispute_id).await?;
    if dispute.livemode != provider.livemode() {
        return Ok(None);
    }
    let Some(payment_id) = dispute.payment_intent.clone() else {
        return Ok(None);
    };
    let Some(order) = db
        .fetch_order_by_provider(ProviderRef::Payment, &payment_id)
        .await?
    else {
        return Ok(None);
    };
    let changes = OrderChanges {
        dispute_status: Some(dispute.status.clone()),
        ..Default::default()
    };
    match dispute.status.as_str() {
        "won" | "warning_closed" => {
            if !db.move_order(&order.id, OrderStatus::Paid, &changes).await? {
                db.update_order(&order.id, &changes).await?;
            }
            if order.revoked {
                save_issue(
                    db,
                    "dispute_won",
                    Some(&order),
                    "dispute won: give perks back by hand if wanted".to_string(),
                    false,
                )
                .await;
            }
        }
        "lost" => {
            if !db.move_order(&order.id, OrderStatus::Refunded, &changes).await? {
                db.update_order(&order.id, &changes).await?;
            }
            if !order.revoked {
                revoke(db, &order, SYSTEM_ACTOR, "dispute lost").await?;
            }
        }
        _ => {
            if !db.move_order(&order.id, OrderStatus::Disputed, &changes).await? {
                db.update_order(&order.id, &changes).await?;
            }
            if !order.revoked {
                revoke(db, &order, SYSTEM_ACTOR, "dispute opened").await?;
            }
        }
    }
    audit(
        db,
        SYSTEM_ACTOR,
        "dispute.updated",
        Some(&order.id),
        Some(&order.user_id),
        Some(dispute.status),
    )
    .await;
    Ok(Some(order.id))
}

// --- webhooks ---

/// What became of a webhook event
enum EventOutcome {
    Done(Option<String>),
    Ignored,
}

/// Check, store and queue a webhook. Answers fast: handling happens in
/// `process_webhook_event` (right away when possible, else by the worker).
/// Returns the stored event id when there is something to handle.
pub async fn receive_webhook(
    db: &Database,
    provider_name: &str,
    headers: &WebhookHeaders,
    body: &[u8],
) -> Result<Option<String>> {
    let billing = config().await.features.billing;
    let provider = provider_named(&billing, provider_name).ok_or_else(|| create_error!(NotFound))?;
    let now = now_ms();
    let event = provider.verify_webhook(headers, body, now / 1000).await?;
    let id = format!("{}:{}", provider.name(), event.id);

    // test events never touch a live server, nor live events a test one
    let wrong_mode = event.livemode != provider.livemode();
    let stored = BillingWebhookEvent {
        id: id.clone(),
        provider: provider.name().to_string(),
        event_type: event.event_type.clone(),
        object_id: event.object_id.clone(),
        livemode: event.livemode,
        created: event.created * 1000,
        received_at: now,
        status: if wrong_mode {
            WebhookEventStatus::Ignored
        } else {
            WebhookEventStatus::Received
        },
        attempts: 0,
        next_attempt_at: now,
        lease: None,
        processed_at: None,
        last_error: wrong_mode.then(|| "livemode does not match this server".to_string()),
        order_id: None,
    };
    if !db.insert_webhook_event(&stored).await? {
        // seen before: nothing new (the first copy is handled or queued)
        return Ok(None);
    }
    if wrong_mode {
        log::warn!("billing: webhook {} ignored: wrong livemode", event.id);
        return Ok(None);
    }
    Ok(Some(id))
}

/// Handle one stored event (lease-protected, so only one worker runs it)
pub async fn process_webhook_event(db: &Database, id: &str) -> Result<()> {
    let now = now_ms();
    if !db.claim_webhook_event(id, now, now + WEBHOOK_LEASE_MS).await? {
        return Ok(());
    }
    let Some(event) = db.fetch_webhook_event(id).await? else {
        return Ok(());
    };
    let billing = config().await.features.billing;
    let result = match provider_named(&billing, &event.provider) {
        Some(provider) => handle_event(db, provider.as_ref(), &event).await,
        None => Err(create_error!(FeatureDisabled {
            feature: "billing".to_string()
        })),
    };
    let done_at = now_ms();
    match result {
        Ok(EventOutcome::Done(order_id)) => {
            db.finish_webhook_event(id, WebhookEventStatus::Processed, None, done_at, order_id, done_at)
                .await
        }
        Ok(EventOutcome::Ignored) => {
            db.finish_webhook_event(id, WebhookEventStatus::Ignored, None, done_at, None, done_at)
                .await
        }
        Err(error) => {
            let reason = format!("{:?}", error.error_type);
            let attempts = event.attempts + 1;
            if attempts >= MAX_WEBHOOK_ATTEMPTS {
                log::error!("billing: webhook {id} failed for good: {reason}");
                db.finish_webhook_event(
                    id,
                    WebhookEventStatus::Failed,
                    Some(reason),
                    done_at,
                    None,
                    done_at,
                )
                .await
            } else {
                // 20 s, 40 s, … up to an hour
                let delay = (10_000_i64 << attempts.min(9)).min(3_600_000);
                db.finish_webhook_event(
                    id,
                    WebhookEventStatus::Received,
                    Some(reason),
                    done_at + delay,
                    None,
                    done_at,
                )
                .await
            }
        }
    }
}

async fn handle_event(
    db: &Database,
    provider: &dyn PaymentProvider,
    event: &BillingWebhookEvent,
) -> Result<EventOutcome> {
    let object = event.object_id.as_str();
    if object.is_empty() {
        return Ok(EventOutcome::Ignored);
    }
    let order_of_session = |session_id: String| async move {
        db.fetch_order_by_provider(ProviderRef::Checkout, &session_id)
            .await
    };

    match provider.event_kind(&event.event_type) {
        EventKind::CheckoutChanged => {
            let Some(order) = order_of_session(object.to_string()).await? else {
                return Ok(EventOutcome::Ignored);
            };
            verify_checkout(db, provider, &order.id).await?;
            Ok(EventOutcome::Done(Some(order.id)))
        }
        EventKind::CheckoutFailed => {
            let Some(order) = order_of_session(object.to_string()).await? else {
                return Ok(EventOutcome::Ignored);
            };
            let session = provider.fetch_checkout(object).await?;
            if session.payment_status != CheckoutPaymentStatus::Paid
                && db
                    .move_order(
                        &order.id,
                        OrderStatus::Failed,
                        &OrderChanges {
                            failure: Some("payment_failed".to_string()),
                            ..Default::default()
                        },
                    )
                    .await?
            {
                audit(db, SYSTEM_ACTOR, "order.failed", Some(&order.id), Some(&order.user_id), None)
                    .await;
            }
            Ok(EventOutcome::Done(Some(order.id)))
        }
        EventKind::Payment => {
            let payment = provider.fetch_payment(object).await?;
            if payment.livemode != provider.livemode() {
                return Ok(EventOutcome::Ignored);
            }
            let order = match &payment.order_id {
                Some(order_id) => db.fetch_order(order_id).await.ok(),
                None => {
                    db.fetch_order_by_provider(ProviderRef::Payment, &payment.id)
                        .await?
                }
            };
            let Some(order) = order else {
                return Ok(EventOutcome::Ignored);
            };
            match payment.status.as_str() {
                "succeeded" => {
                    verify_checkout(db, provider, &order.id).await?;
                }
                "processing" => {
                    db.move_order(&order.id, OrderStatus::Processing, &OrderChanges::default())
                        .await?;
                }
                "requires_action" => {
                    db.move_order(
                        &order.id,
                        OrderStatus::RequiresAction,
                        &OrderChanges::default(),
                    )
                    .await?;
                }
                _ => {
                    // the checkout stays open for another try; just note it
                    if order.status.is_open() {
                        db.update_order(
                            &order.id,
                            &OrderChanges {
                                failure: Some("payment_attempt_failed".to_string()),
                                ..Default::default()
                            },
                        )
                        .await?;
                    }
                }
            }
            Ok(EventOutcome::Done(Some(order.id)))
        }
        EventKind::InvoicePaid => {
            Ok(EventOutcome::Done(handle_invoice(db, provider, object, true).await?))
        }
        EventKind::InvoiceFailed => {
            Ok(EventOutcome::Done(handle_invoice(db, provider, object, false).await?))
        }
        EventKind::Subscription => Ok(EventOutcome::Done(
            sync_subscription(db, provider, object).await?,
        )),
        EventKind::ChargeRefund => {
            let Some(payment_id) = provider.fetch_charge_payment(object).await? else {
                return Ok(EventOutcome::Ignored);
            };
            Ok(EventOutcome::Done(sync_refunds(db, provider, &payment_id).await?))
        }
        EventKind::Refund => {
            let refund = provider.fetch_refund(object).await?;
            let Some(payment_id) = refund.payment_intent else {
                return Ok(EventOutcome::Ignored);
            };
            Ok(EventOutcome::Done(sync_refunds(db, provider, &payment_id).await?))
        }
        EventKind::Dispute => Ok(EventOutcome::Done(
            sync_dispute(db, provider, object).await?,
        )),
        EventKind::Other => Ok(EventOutcome::Ignored),
    }
}

/// Handle events that are due (new ones the receiver couldn't start, and
/// retries)
pub async fn process_due_webhooks(db: &Database) -> Result<()> {
    for event in db.fetch_due_webhook_events(now_ms(), 50).await? {
        if let Err(error) = process_webhook_event(db, &event.id).await {
            log::error!("billing: webhook {}: {error:?}", event.id);
        }
    }
    Ok(())
}

// --- reconciliation ---

/// Remember a difference for staff. The id is per kind and order, so the
/// same difference found again replaces the old entry.
pub async fn save_issue(
    db: &Database,
    kind: &str,
    order: Option<&BillingOrder>,
    detail: String,
    auto_fixed: bool,
) {
    let issue = ReconciliationIssue {
        id: format!(
            "{kind}:{}",
            order.map(|order| order.id.as_str()).unwrap_or("-")
        ),
        kind: kind.to_string(),
        order_id: order.map(|order| order.id.clone()),
        user_id: order.map(|order| order.user_id.clone()),
        detail,
        auto_fixed,
        found_at: Timestamp::now_utc(),
        resolved: auto_fixed,
    };
    if let Err(error) = db.save_reconciliation_issue(&issue).await {
        log::error!("billing: issue {} not saved: {error:?}", issue.id);
    }
}

/// Summary of one reconciliation run
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub checked_orders: usize,
    pub delivered_missed_payments: usize,
    pub closed_checkouts: usize,
    pub retried_deliveries: usize,
    pub synced_subscriptions: usize,
    pub errors: usize,
}

/// Compare our orders and subscriptions with the provider. Safe policy
/// only: payments the provider confirms are recorded and delivered (the
/// same path a webhook takes), expired checkouts are closed, and
/// subscription state is copied from the provider. Never creates a payment
/// or refund; everything else becomes an issue for staff.
pub async fn reconcile(db: &Database) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();
    let billing = config().await.features.billing;
    let now = now_ms();

    // deliveries that failed or crashed (any provider, test included)
    for order in db
        .fetch_undelivered_orders(now, MAX_FULFILMENT_ATTEMPTS)
        .await?
    {
        report.retried_deliveries += 1;
        if fulfil_order(db, &order.id).await.is_err() {
            report.errors += 1;
        }
    }

    // every provider still running checks its own orders
    let providers = ready_providers(&billing);
    if providers.is_empty() {
        return Ok(report);
    }
    let provider_of = |order: &BillingOrder| {
        providers
            .iter()
            .find(|provider| provider.name() == order.provider && order.test != provider.livemode())
    };

    // checkouts still open on our side: the webhook may be late or lost
    for order in db.fetch_open_provider_orders(now - 2 * 60_000, 100).await? {
        let Some(provider) = provider_of(&order) else {
            continue;
        };
        report.checked_orders += 1;
        if order.provider_checkout_id.is_none() {
            // checkout never opened: nothing at the provider to wait for
            if now - order.created_at > 60 * 60_000 {
                db.move_order(
                    &order.id,
                    OrderStatus::Cancelled,
                    &OrderChanges {
                        failure: Some("checkout_not_opened".to_string()),
                        ..Default::default()
                    },
                )
                .await?;
                report.closed_checkouts += 1;
            }
            continue;
        }
        match verify_checkout(db, provider.as_ref(), &order.id).await {
            Ok(CheckoutOutcome::Paid) => {
                report.delivered_missed_payments += 1;
                save_issue(
                    db,
                    "paid_missed_webhook",
                    Some(&order),
                    "payment confirmed by the provider API, delivered".to_string(),
                    true,
                )
                .await;
            }
            Ok(CheckoutOutcome::Closed) => report.closed_checkouts += 1,
            Ok(CheckoutOutcome::StillOpen) => {
                // past its time: close it at the provider so it can't be paid
                if order
                    .checkout_expires_at
                    .is_some_and(|expires| now > expires + 5 * 60_000)
                {
                    if let Some(session_id) = &order.provider_checkout_id {
                        let _ = provider.expire_checkout(session_id).await;
                    }
                    let _ = verify_checkout(db, provider.as_ref(), &order.id).await;
                }
            }
            Ok(CheckoutOutcome::Rejected(_)) => {}
            Err(_) => report.errors += 1,
        }
    }

    // subscriptions: copy the provider's state
    let mut seen = std::collections::HashSet::new();
    for order in db
        .search_orders(
            &OrderSearch {
                status: Some(OrderStatus::Paid),
                ..Default::default()
            },
            500,
        )
        .await?
    {
        let Some(sub_id) = order.provider_subscription_id.clone() else {
            continue;
        };
        let Some(provider) = provider_of(&order) else {
            continue;
        };
        if !seen.insert(sub_id.clone()) {
            continue;
        }
        match sync_subscription(db, provider.as_ref(), &sub_id).await {
            Ok(_) => report.synced_subscriptions += 1,
            Err(_) => report.errors += 1,
        }
    }

    Ok(report)
}

/// Staff give Plus without payment. No order is made (nothing was paid);
/// the grant id keys the added time and the audit log records who, whom,
/// how long and why. Returns the grant id.
pub async fn grant_plus(
    db: &Database,
    actor: &str,
    user_id: &str,
    days: u32,
    reason: &str,
) -> Result<String> {
    let billing = config().await.features.billing;
    if !billing.available() {
        return Err(create_error!(FeatureDisabled {
            feature: "billing".to_string()
        }));
    }
    if !(1..=366).contains(&days) {
        return Err(create_error!(FailedValidation {
            error: "days".to_string()
        }));
    }
    let user = db.fetch_user(user_id).await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let id = format!("grant-{}", Ulid::new());
    add_plus_for_order(
        db,
        user_id,
        &id,
        days as i64 * DAY_MS,
        PlusSource::Grant,
        billing.new_test_flag(),
    )
    .await?;
    sync_user_plus(db, user_id).await?;
    audit(
        db,
        actor,
        "plus.granted",
        Some(&id),
        Some(user_id),
        Some(format!("{days} days: {reason}")),
    )
    .await;
    Ok(id)
}

/// Staff view of billing
pub async fn admin_summary(db: &Database) -> Result<v0::BillingAdminSummary> {
    let config = config().await;
    let billing = &config.features.billing;
    Ok(v0::BillingAdminSummary {
        mode: billing.mode().name().to_string(),
        provider: billing.provider.name.clone(),
        live_gate: billing
            .live_gate_report(&config.hosts, &config.features.legal_links)
            .into_iter()
            .map(|(name, passed)| v0::LiveGateCheck {
                name: name.to_string(),
                passed,
            })
            .collect(),
        orders_by_status: db
            .count_orders_by_status()
            .await?
            .into_iter()
            .map(|(status, count)| v0::StatusCount { status, count })
            .collect(),
        webhooks_failed: db.count_webhook_events(WebhookEventStatus::Failed).await?,
        webhooks_pending: db.count_webhook_events(WebhookEventStatus::Received).await?,
        open_issues: db.count_open_reconciliation_issues().await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_config::{BoostTier, DiscoverPromotionConfig};

    fn billing() -> FeaturesBilling {
        FeaturesBilling {
            currency: "USD".to_string(),
            plus_price: 499,
            supercharge_price: 299,
            max_supercharges_per_user: 30,
            discover: DiscoverPromotionConfig {
                min_members: 5,
                promotion_days: 7,
                boosts: vec![
                    BoostTier {
                        kind: "Boost".to_string(),
                        price: 149,
                        guarantee: 100,
                        capacity: 0,
                    },
                    BoostTier {
                        kind: "MegaBoost".to_string(),
                        price: 499,
                        guarantee: 20,
                        capacity: 0,
                    },
                ],
                promoted_price: 799,
                promoted_slots: 4,
            },
            ..Default::default()
        }
    }

    #[test]
    fn prices_come_from_the_catalog() {
        let b = billing();
        assert_eq!(price_of(&OrderProduct::Plus, &b).unwrap().amount().unwrap(), 499);
        let gift = OrderProduct::PlusGift {
            user: "u".into(),
            months: 3,
            note: None,
        };
        assert_eq!(price_of(&gift, &b).unwrap().amount().unwrap(), 1497);
        let feathers = OrderProduct::Supercharge {
            server: "s".into(),
            quantity: Some(4),
        };
        let priced = price_of(&feathers, &b).unwrap();
        assert_eq!((priced.unit_amount, priced.quantity), (299, 4));
        let boost = OrderProduct::DiscoverBoost {
            server: "s".into(),
            kind: "MegaBoost".into(),
        };
        assert_eq!(price_of(&boost, &b).unwrap().amount().unwrap(), 499);
        let top = OrderProduct::BotPromoted { bot: "b".into() };
        assert_eq!(price_of(&top, &b).unwrap().amount().unwrap(), 799);
    }

    #[test]
    fn refuses_bad_products_and_quantities() {
        let b = billing();
        for months in [0, 2, 13, u32::MAX] {
            let gift = OrderProduct::PlusGift {
                user: "u".into(),
                months,
                note: None,
            };
            assert!(price_of(&gift, &b).is_err(), "{months} months");
        }
        for quantity in [0, 31, u32::MAX] {
            let feathers = OrderProduct::Supercharge {
                server: "s".into(),
                quantity: Some(quantity),
            };
            assert!(price_of(&feathers, &b).is_err(), "{quantity} feathers");
        }
        let unknown = OrderProduct::DiscoverBoost {
            server: "s".into(),
            kind: "FreeBoost".into(),
        };
        assert!(price_of(&unknown, &b).is_err());
        // a product priced at zero can't be bought
        let mut free = billing();
        free.plus_price = 0;
        assert!(price_of(&OrderProduct::Plus, &free).is_err());
    }

    #[test]
    fn catalog_lists_every_product_with_stable_ids() {
        let ids: Vec<String> = catalog(&billing()).into_iter().map(|p| p.id).collect();
        assert_eq!(
            ids,
            vec![
                "plus",
                "plus.gift",
                "feather",
                "discover.boost",
                "discover.megaboost",
                "discover.top_row"
            ]
        );
    }

    fn order() -> BillingOrder {
        let mut order = BillingOrder::new(
            "u1-key".into(),
            "u1".into(),
            OrderProduct::Plus,
            499,
            "USD".into(),
            "stripe",
            true,
        );
        order.status = OrderStatus::Pending;
        order.provider_checkout_id = Some("cs_1".into());
        order.provider_customer_id = Some("cus_1".into());
        order
    }

    fn session() -> CheckoutSession {
        CheckoutSession {
            id: "cs_1".into(),
            url: None,
            status: CheckoutStatus::Complete,
            payment_status: CheckoutPaymentStatus::Paid,
            amount_subtotal: Some(499),
            amount_total: Some(499),
            currency: Some("usd".into()),
            customer: Some("cus_1".into()),
            payment_intent: None,
            subscription: Some("sub_1".into()),
            invoice: Some("in_1".into()),
            livemode: false,
            client_reference_id: Some("u1-key".into()),
            order_id: Some("u1-key".into()),
            expires_at: 0,
        }
    }

    #[test]
    fn accepts_matching_session() {
        assert_eq!(session_mismatch(&order(), &session(), false), None);
    }

    #[test]
    fn refuses_sessions_that_do_not_match() {
        let o = order();
        let cases: Vec<(&str, Box<dyn Fn(&mut CheckoutSession)>)> = vec![
            ("session", Box::new(|s| s.id = "cs_other".into())),
            ("order", Box::new(|s| s.client_reference_id = Some("u2-key".into()))),
            ("order", Box::new(|s| s.order_id = None)),
            ("livemode", Box::new(|s| s.livemode = true)),
            ("customer", Box::new(|s| s.customer = Some("cus_attacker".into()))),
            ("unpaid", Box::new(|s| s.payment_status = CheckoutPaymentStatus::Unpaid)),
            (
                "unpaid",
                Box::new(|s| s.payment_status = CheckoutPaymentStatus::NoPaymentRequired),
            ),
            ("unpaid", Box::new(|s| s.status = CheckoutStatus::Open)),
            ("currency", Box::new(|s| s.currency = Some("eur".into()))),
            ("amount", Box::new(|s| s.amount_subtotal = Some(1))),
            ("amount", Box::new(|s| s.amount_subtotal = Some(0))),
            ("discount", Box::new(|s| s.amount_total = Some(100))),
            ("mode", Box::new(|s| s.subscription = None)),
        ];
        for (expected, change) in cases {
            let mut s = session();
            change(&mut s);
            assert_eq!(session_mismatch(&o, &s, false), Some(expected));
        }
        // a sandbox payment never pays a live order
        let mut live_order = order();
        live_order.test = false;
        assert_eq!(session_mismatch(&live_order, &session(), false), Some("livemode"));
    }

    #[test]
    fn taxes_on_top_are_accepted() {
        let mut s = session();
        s.amount_total = Some(594);
        assert_eq!(session_mismatch(&order(), &s, false), None);
    }

    #[test]
    fn grace_period_is_bounded() {
        let day = DAY_MS;
        // active: provider's period
        assert_eq!(
            effective_period_end(SubscriptionStatus::Active, 40 * day, 10 * day, 3),
            40 * day
        );
        // past due: paid period plus grace, never past the provider's end,
        // and the same however often it is synced
        assert_eq!(
            effective_period_end(SubscriptionStatus::PastDue, 40 * day, 10 * day, 3),
            13 * day
        );
        let again = effective_period_end(SubscriptionStatus::PastDue, 40 * day, 10 * day, 3);
        assert_eq!(again, 13 * day);
        assert_eq!(
            effective_period_end(SubscriptionStatus::PastDue, 11 * day, 10 * day, 3),
            11 * day
        );
        assert_eq!(
            effective_period_end(SubscriptionStatus::PastDue, 40 * day, 10 * day, 0),
            10 * day
        );
    }

    #[test]
    fn provider_statuses_map_safely() {
        assert_eq!(subscription_status("active"), SubscriptionStatus::Active);
        assert_eq!(subscription_status("past_due"), SubscriptionStatus::PastDue);
        assert_eq!(subscription_status("incomplete"), SubscriptionStatus::Incomplete);
        assert_eq!(subscription_status("canceled"), SubscriptionStatus::Canceled);
        assert_eq!(subscription_status("incomplete_expired"), SubscriptionStatus::Canceled);
        assert_eq!(subscription_status("something_new"), SubscriptionStatus::Canceled);
    }

    #[test]
    fn return_urls_carry_only_the_order() {
        let (ok, cancel) = return_urls("https://app.zrzur.com/", "u1-abc");
        assert_eq!(ok, "https://app.zrzur.com/?billing_order=u1-abc&billing_return=success");
        assert_eq!(cancel, "https://app.zrzur.com/?billing_order=u1-abc&billing_return=cancel");
    }
}
