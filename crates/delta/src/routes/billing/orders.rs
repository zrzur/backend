//! Orders: every purchase goes through here.
//!
//! 1. The app creates an order with a random idempotency key, naming only
//!    the product (and how many feathers). The server checks the purchase is
//!    allowed and prices it from its own catalog.
//! 2. With a payment provider the server opens the provider's hosted
//!    checkout and returns its address; the buyer pays there. Zrzur never
//!    sees card details.
//! 3. The order becomes Paid only when the provider's API confirms the
//!    payment (webhook, the order page asking again, or reconciliation), and
//!    it is delivered exactly once (`revolt_database::verify_checkout`).
//!
//! The built-in test provider (local development only) skips 2 and 3: the
//! app confirms its own order with `/pay`; nothing is charged.

use iso8601_timestamp::Timestamp;
use revolt_config::{config, BillingMode, FeaturesBilling};
use revolt_database::{
    audit, epoch_ms, fulfil_order, open_checkout, price_of, provider, provider_named,
    ready_providers, verify_checkout,
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    BillingOrder, Database, OrderChanges, OrderSearch, Priced, RelationshipStatus, Server, User,
};
use revolt_models::v0::{self, OrderProduct, OrderStatus};
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

/// Billing settings, if billing is on
async fn billing() -> Result<FeaturesBilling> {
    let billing = config().await.features.billing;
    if billing.available() {
        Ok(billing)
    } else {
        Err(create_error!(FeatureDisabled {
            feature: "billing".to_string()
        }))
    }
}

/// Whether this member may start a purchase right now. Live checkout stays
/// closed until the owner opens it; a production server running the
/// sandbox only lets its listed testers buy.
async fn checkout_allowed(billing: &FeaturesBilling, user: &User) -> Result<()> {
    let locked = || {
        Err(create_error!(FeatureDisabled {
            feature: "billing_checkout".to_string()
        }))
    };
    if !billing.mode().checkout_open() {
        return locked();
    }
    if billing.mode() == BillingMode::Sandbox
        && config().await.production
        && !billing.provider.sandbox_testers.contains(&user.id)
    {
        return locked();
    }
    Ok(())
}

/// Purchases are open only to people who declared an age of 18 or over
/// (owner decision Q4, paid services terms, PS-2). No declared age counts
/// as under 18.
async fn adult_buyer(db: &Database, user: &User) -> Result<()> {
    if revolt_database::is_declared_adult(db, user).await {
        Ok(())
    } else {
        Err(create_error!(AgeRequirementNotMet))
    }
}

/// The server, if the user may manage it
pub async fn managed_server(db: &Database, user: &User, id: &str) -> Result<Server> {
    let server = db.fetch_server(id).await?;
    let mut query = DatabasePermissionQuery::new(db, user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    Ok(server)
}

/// A bot of yours that moderators approved for Discover
pub async fn listed_own_bot(db: &Database, user: &User, id: &str) -> Result<revolt_database::Bot> {
    let bot = db.fetch_bot(id).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }
    if !bot.discoverable || !bot.public {
        return Err(create_error!(NotOnDiscover));
    }
    Ok(bot)
}

/// Check a purchase is allowed and price it from the catalog
async fn check(
    db: &Database,
    user: &User,
    product: &OrderProduct,
    billing: &FeaturesBilling,
) -> Result<Priced> {
    let priced = price_of(product, billing)?;
    match product {
        OrderProduct::Plus => {
            if db.fetch_plus_subscription(&user.id).await?.is_some() {
                // a cancelled one is resumed for free, not bought again
                return Err(create_error!(InvalidOperation));
            }
        }
        OrderProduct::PlusGift { user: to, note, .. } => {
            if note.as_ref().is_some_and(|note| note.chars().count() > 200) {
                return Err(create_error!(FailedValidation {
                    error: "note is too long".to_string()
                }));
            }
            let target = Reference::from_unchecked(to).as_user(db).await?;
            if target.bot.is_some() {
                return Err(create_error!(IsBot));
            }
            if target.id == user.id {
                return Err(create_error!(InvalidOperation));
            }
            if matches!(
                user.relationship_with(&target.id),
                RelationshipStatus::Blocked | RelationshipStatus::BlockedOther
            ) {
                return Err(create_error!(Blocked));
            }
        }
        OrderProduct::Supercharge { server, .. } => {
            db.fetch_member(server, &user.id)
                .await
                .map_err(|_| create_error!(NotFound))?;
            let owned = db.fetch_supercharges_by_user(&user.id).await?.len();
            if owned + priced.quantity as usize > billing.max_supercharges_per_user {
                return Err(create_error!(TooManySupercharges {
                    max: billing.max_supercharges_per_user
                }));
            }
        }
        // a full tier doesn't refuse: the buyer waits in line
        OrderProduct::DiscoverBoost { server, .. } | OrderProduct::DiscoverPromoted { server } => {
            let server = managed_server(db, user, server).await?;
            if !server.discoverable {
                return Err(create_error!(NotOnDiscover));
            }
        }
        OrderProduct::BotBoost { bot, .. } | OrderProduct::BotPromoted { bot } => {
            listed_own_bot(db, user, bot).await?;
        }
    }
    Ok(priced)
}

/// An order and where to pay it (only for its owner)
async fn checkout_of(db: &Database, order: BillingOrder) -> Result<v0::OrderCheckout> {
    let mut order = order;
    if order.status == OrderStatus::Created && order.provider != "test" {
        // the checkout couldn't be opened before: try again
        let billing = config().await.features.billing;
        let provider = provider_named(&billing, &order.provider).ok_or_else(|| {
            create_error!(FeatureDisabled {
                feature: "billing_checkout".to_string()
            })
        })?;
        order = open_checkout(db, provider.as_ref(), &billing, &order).await?;
    }
    let now = epoch_ms(Timestamp::now_utc());
    let checkout_url = order
        .checkout_url
        .clone()
        .filter(|_| order.status.is_open())
        .filter(|_| order.checkout_expires_at.is_none_or(|expires| expires > now));
    Ok(v0::OrderCheckout {
        order: order.into(),
        checkout_url,
    })
}

/// # Start a purchase
///
/// Creates an order for something to buy. The server checks it is allowed
/// and sets the price. Sending the same idempotency key again, or buying
/// the same thing while its checkout is still open, returns that order
/// instead of a new one. With a payment provider, `checkout_url` is the
/// provider's payment page.
#[openapi(tag = "Billing")]
#[post("/orders", data = "<data>")]
pub async fn create_order(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataCreateOrder>,
) -> Result<Json<v0::OrderCheckout>> {
    let billing = billing().await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    checkout_allowed(&billing, &user).await?;
    adult_buyer(db, &user).await?;
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    if !data
        .idempotency_key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(create_error!(FailedValidation {
            error: "bad idempotency key".to_string()
        }));
    }

    // how to pay: the buyer's pick, else the preferred provider
    let mode = billing.mode();
    let provider = if mode == BillingMode::InternalTest {
        if data.provider.as_deref().is_some_and(|name| name != "test") {
            return Err(create_error!(FeatureDisabled {
                feature: "billing_checkout".to_string()
            }));
        }
        None
    } else {
        let chosen = match data.provider.as_deref() {
            Some(name) => provider_named(&billing, name),
            None => provider(&billing),
        };
        Some(chosen.ok_or_else(|| create_error!(FeatureDisabled {
            feature: "billing_checkout".to_string()
        }))?)
    };
    let provider_name = provider.as_ref().map(|p| p.name()).unwrap_or("test");

    let id = format!("{}-{}", user.id, data.idempotency_key);
    if let Ok(existing) = db.fetch_order(&id).await {
        // same key, different purchase or way of paying: refuse rather
        // than guess
        if existing.product != data.product || existing.provider != provider_name {
            return Err(create_error!(InvalidOperation));
        }
        return Ok(Json(checkout_of(db, existing).await?));
    }

    // a reload or second tab: continue the checkout that is still open
    // rather than opening another one that could be paid as well
    let now = epoch_ms(Timestamp::now_utc());
    let open = db
        .search_orders(
            &OrderSearch {
                user_id: Some(user.id.clone()),
                ..Default::default()
            },
            20,
        )
        .await?
        .into_iter()
        .find(|order| {
            order.status.is_open()
                && order.product == data.product
                && order.provider != "test"
                && order.provider == provider_name
                && order
                    .checkout_expires_at
                    .is_some_and(|expires| expires > now + 60_000)
        });
    if let Some(open) = open {
        return Ok(Json(checkout_of(db, open).await?));
    }

    let priced = check(db, &user, &data.product, &billing).await?;

    let mut order = BillingOrder::new(
        id.clone(),
        user.id.clone(),
        data.product,
        priced.amount()?,
        billing.currency.clone(),
        provider_name,
        billing.new_test_flag(),
    );
    order.quantity = priced.quantity;
    order.unit_amount = priced.unit_amount;
    if provider.is_none() {
        // simulated: waiting for the app to confirm
        order.status = OrderStatus::Pending;
    }
    if !db.insert_order(&order).await? {
        // a parallel request with the same key won
        return Ok(Json(checkout_of(db, db.fetch_order(&id).await?).await?));
    }
    audit(
        db,
        &user.id,
        "order.created",
        Some(&id),
        Some(&user.id),
        Some(format!("{} x{} {} {}", priced.product_id, priced.quantity, order.amount, order.currency)),
    )
    .await;

    if let Some(provider) = provider {
        open_checkout(db, provider.as_ref(), &billing, &order).await?;
    }
    Ok(Json(checkout_of(db, db.fetch_order(&id).await?).await?))
}

/// # Pay an order (built-in test provider)
///
/// Confirms payment of one of your pending orders with the built-in test
/// provider: nothing is charged. Only on local development servers; with a
/// real provider payment is confirmed by the provider, never by the app.
#[openapi(tag = "Billing")]
#[post("/orders/<id>/pay")]
pub async fn pay_order(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::Order>> {
    let billing = billing().await?;
    if billing.mode() != BillingMode::InternalTest {
        return Err(create_error!(InvalidOperation));
    }
    let order = db.fetch_order(id).await?;
    if order.user_id != user.id || order.provider != "test" {
        return Err(create_error!(NotFound));
    }
    adult_buyer(db, &user).await?;
    match order.status {
        OrderStatus::Pending => {
            if db
                .move_order(
                    &order.id,
                    OrderStatus::Paid,
                    &OrderChanges {
                        paid_at: Some(epoch_ms(Timestamp::now_utc())),
                        ..Default::default()
                    },
                )
                .await?
            {
                audit(db, &user.id, "order.paid", Some(&order.id), Some(&user.id), Some("simulated".to_string())).await;
            }
            // a failed delivery is refunded inside (nothing was charged)
            let _ = fulfil_order(db, &order.id).await;
            Ok(Json(db.fetch_order(&order.id).await?.into()))
        }
        // paying twice changes nothing
        status if status.was_paid() => Ok(Json(order.into())),
        _ => Err(create_error!(InvalidOperation)),
    }
}

/// # Fetch an order
///
/// One of your orders. With `refresh`, an order still waiting for payment
/// is checked with the payment provider first (at most every 5 seconds),
/// so returning from the checkout page shows the real state without
/// waiting for the provider's webhook.
#[openapi(tag = "Billing")]
#[get("/orders/<id>?<refresh>")]
pub async fn fetch_order(
    db: &State<Database>,
    user: User,
    id: &str,
    refresh: Option<bool>,
) -> Result<Json<v0::OrderCheckout>> {
    let order = db.fetch_order(id).await?;
    if order.user_id != user.id {
        return Err(create_error!(NotFound));
    }
    if refresh == Some(true) && order.status.is_open() && order.provider != "test" {
        let billing = config().await.features.billing;
        if let Some(provider) = provider_named(&billing, &order.provider) {
            let now = epoch_ms(Timestamp::now_utc());
            if db.mark_order_synced(id, now, now - 5_000).await? {
                if let Err(error) = verify_checkout(db, provider.as_ref(), id).await {
                    log::warn!("billing: checking {id} failed: {error:?}");
                }
            }
        }
    }
    Ok(Json(checkout_of(db, db.fetch_order(id).await?).await?))
}

/// # Cancel an order
///
/// Drops one of your orders that isn't paid yet and closes its checkout
/// page so it can't be paid any more.
#[openapi(tag = "Billing")]
#[delete("/orders/<id>")]
pub async fn cancel_order(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::Order>> {
    let order = db.fetch_order(id).await?;
    if order.user_id != user.id {
        return Err(create_error!(NotFound));
    }
    if !order.status.is_open() {
        return Ok(Json(order.into()));
    }
    if let Some(session_id) = &order.provider_checkout_id {
        let billing = config().await.features.billing;
        if let Some(provider) = provider_named(&billing, &order.provider) {
            if provider.expire_checkout(session_id).await.is_err() {
                // already paid (or the provider is away): ask what happened
                // instead of cancelling a payment that may have gone through
                verify_checkout(db, provider.as_ref(), id).await?;
                let order = db.fetch_order(id).await?;
                if !order.status.is_open() {
                    return Ok(Json(order.into()));
                }
                return Err(create_error!(ProxyError));
            }
        }
    }
    if db
        .move_order(
            id,
            OrderStatus::Cancelled,
            &OrderChanges {
                failure: Some("cancelled".to_string()),
                ..Default::default()
            },
        )
        .await?
    {
        audit(db, &user.id, "order.cancelled", Some(id), Some(&user.id), None).await;
    }
    Ok(Json(db.fetch_order(id).await?.into()))
}

/// # Billing history
///
/// Your last 50 orders, newest first.
#[openapi(tag = "Billing")]
#[get("/orders")]
pub async fn list_orders(db: &State<Database>, user: User) -> Result<Json<Vec<v0::Order>>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    Ok(Json(
        db.fetch_orders_by_user(&user.id, 50)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// # Payment methods and invoices
///
/// A page at the payment provider where you manage your payment methods
/// and see your invoices. Only once you have bought something there.
#[openapi(tag = "Billing")]
#[post("/portal")]
pub async fn billing_portal(db: &State<Database>, user: User) -> Result<Json<v0::BillingPortal>> {
    let billing = billing().await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let providers = ready_providers(&billing);
    if providers.is_empty() {
        return Err(create_error!(FeatureDisabled {
            feature: "billing_portal".to_string()
        }));
    }
    // the provider the member has bought with (the preferred one first)
    for provider in providers {
        let Some(customer) = db
            .fetch_billing_customer(&format!(
                "{}:{}:{}",
                provider.name(),
                provider.livemode(),
                user.id
            ))
            .await?
        else {
            continue;
        };
        let app = config().await.hosts.app;
        let url = provider
            .create_portal(&customer.customer_id, &format!("{}/", app.trim_end_matches('/')))
            .await?;
        return Ok(Json(v0::BillingPortal { url }));
    }
    Err(create_error!(NotFound))
}
