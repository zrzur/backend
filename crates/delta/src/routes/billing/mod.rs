//! The member subscription, feathers (server supercharges), gifts and
//! Discover promotion.
//!
//! Everything bought goes through orders (`orders.rs`), paid at a payment
//! provider's hosted checkout and confirmed by the provider
//! (`webhook.rs`). Staff tools are in `admin.rs`. Billing modes and the
//! live gate are in `revolt_config::BillingMode`: with billing off every
//! route here except the plan listing refuses, and while live payments are
//! not open nobody can start a purchase.

use std::collections::HashMap;

use iso8601_timestamp::Timestamp;
use revolt_config::{config, FeaturesBilling, FeaturesLimits};
use revolt_database::{
    audit, catalog, sync_server_supercharge, sync_subscription, Database, User,
    GIFT_MONTHS,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::{serde::json::Json, Route, State};
use validator::Validate;

mod admin;
mod orders;
mod promote;
mod webhook;

#[cfg(test)]
mod tests;

pub fn routes() -> (Vec<Route>, OpenApi) {
    let (mut routes, spec) = openapi_get_routes_spec![
        orders::create_order,
        orders::pay_order,
        orders::fetch_order,
        orders::cancel_order,
        orders::list_orders,
        orders::billing_portal,
        promote::promotion_status,
        promote::bot_promotion_status,
        admin::summary,
        admin::orders,
        admin::order,
        admin::audit_log,
        admin::refund,
        admin::deliver,
        admin::webhooks,
        admin::retry_webhook,
        admin::reconcile_now,
        admin::issues,
        admin::grant,
        plans,
        overview,
        edit_plus,
        apply_supercharge,
        edit_supercharge,
        server_supercharges
    ];
    // signed by the provider, not a client route: kept out of the API docs
    routes.extend(rocket::routes![webhook::provider_webhook]);
    (routes, spec)
}

fn validated<T: Validate>(data: T) -> Result<T> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    Ok(data)
}

/// Billing settings, if purchases can be made
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

fn not_bot(user: &User) -> Result<()> {
    if user.bot.is_some() {
        Err(create_error!(IsBot))
    } else {
        Ok(())
    }
}

fn plan_limits(limits: &FeaturesLimits) -> v0::PlanLimits {
    v0::PlanLimits {
        message_length: limits.message_length as u32,
        attachments: limits
            .file_upload_size_limit
            .get("attachments")
            .copied()
            .unwrap_or_default() as u64,
        bots: limits.bots as u32,
        video_resolution: limits.video_resolution,
        video_framerate: limits.video_framerate,
    }
}

/// # Plans
///
/// Prices and perks of Plus and supercharges. `available` is false when
/// purchases can't be made.
#[openapi(tag = "Billing")]
#[get("/plans")]
pub async fn plans(_user: User) -> Result<Json<v0::BillingPlans>> {
    let config = config().await;
    let billing = &config.features.billing;
    let free = &config.features.limits.default;
    let plus = config.features.limits.roles.get("plus").unwrap_or(free);

    let mode = billing.mode();
    Ok(Json(v0::BillingPlans {
        available: billing.available(),
        checkout_available: mode.checkout_open(),
        mode: mode.name().to_string(),
        // the preferred provider (kept for older apps)
        provider: if mode.uses_provider() {
            billing
                .ready_provider_names()
                .first()
                .map(|name| name.to_string())
                .unwrap_or_default()
        } else if mode.active() {
            "test".to_string()
        } else {
            String::new()
        },
        providers: if mode.uses_provider() {
            billing
                .ready_provider_names()
                .into_iter()
                .map(str::to_string)
                .collect()
        } else if mode.active() {
            vec!["test".to_string()]
        } else {
            vec![]
        },
        portal_available: mode.uses_provider(),
        products: if mode.active() { catalog(billing) } else { vec![] },
        test_mode: mode.active() && !mode.live(),
        currency: billing.currency.clone(),
        plus_price: billing.plus_price,
        supercharge_price: billing.supercharge_price,
        plus_free_supercharges: billing.plus_free_supercharges as u32,
        free_server_emoji: billing.server_emoji(0, config.features.limits.global.server_emoji)
            as u32,
        gift_months: GIFT_MONTHS.to_vec(),
        hq_lounge: !billing.hq_server.is_empty() && !billing.plus_role.is_empty(),
        free: plan_limits(free),
        plus: plan_limits(plus),
        levels: billing
            .levels
            .iter()
            .enumerate()
            .map(|(i, level)| v0::SuperchargeLevelInfo {
                level: i as u32 + 1,
                supercharges: level.supercharges as u32,
                server_emoji: level.server_emoji as u32,
                attachments: level.attachments as u64,
                voice_bitrate: level.voice_bitrate,
                video_resolution: level.video_resolution,
                video_framerate: level.video_framerate,
                vanity_invite: level.vanity_invite,
            })
            .collect(),
    }))
}

/// The provider that runs the subscription an order started
async fn provider_of_order(
    db: &Database,
    order_id: Option<&str>,
) -> Result<Box<dyn revolt_database::PaymentProvider>> {
    let order = db
        .fetch_order(order_id.ok_or_else(|| create_error!(NotFound))?)
        .await?;
    let billing = config().await.features.billing;
    revolt_database::provider_named(&billing, &order.provider).ok_or_else(|| {
        create_error!(FeatureDisabled {
            feature: "billing".to_string()
        })
    })
}

pub(crate) async fn overview_of(db: &Database, user_id: &str) -> Result<v0::BillingOverview> {
    let user = db.fetch_user(user_id).await?;
    let mut supercharges = db.fetch_supercharges_by_user(user_id).await?;
    supercharges.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(v0::BillingOverview {
        plus: db
            .fetch_plus_subscription(user_id)
            .await?
            .map(|sub| sub.into()),
        hide_badge: user.plus.is_some_and(|plus| plus.hide_badge),
        supercharges,
    })
}

/// # Your subscriptions
///
/// Your Plus subscription and supercharges.
#[openapi(tag = "Billing")]
#[get("/@me")]
pub async fn overview(db: &State<Database>, user: User) -> Result<Json<v0::BillingOverview>> {
    not_bot(&user)?;
    Ok(Json(overview_of(db, &user.id).await?))
}

/// # Change Plus
///
/// Cancel or resume renewal, or hide the badge.
#[openapi(tag = "Billing")]
#[patch("/plus", data = "<data>")]
pub async fn edit_plus(
    db: &State<Database>,
    mut user: User,
    data: Json<v0::DataEditPlus>,
) -> Result<Json<v0::BillingOverview>> {
    billing().await?;
    not_bot(&user)?;
    let data = data.into_inner();

    if let Some(cancel) = data.cancel_at_period_end {
        let mut sub = db
            .fetch_plus_subscription(&user.id)
            .await?
            .ok_or_else(|| create_error!(NotFound))?;
        // gifts end on their own
        if !matches!(sub.source, v0::PlusSource::Purchase) {
            return Err(create_error!(InvalidOperation));
        }
        match sub.provider_subscription_id.clone() {
            // the provider renews it: stop or resume renewal there, then
            // take on what the provider says
            Some(provider_id) => {
                let provider = provider_of_order(db, sub.order_id.as_deref()).await?;
                provider.set_cancel_at_period_end(&provider_id, cancel).await?;
                sync_subscription(db, provider.as_ref(), &provider_id).await?;
            }
            None => {
                sub.cancel_at_period_end = cancel;
                db.save_plus_subscription(&sub).await?;
            }
        }
        audit(
            db,
            &user.id,
            if cancel { "plus.cancel" } else { "plus.resume" },
            sub.order_id.as_deref(),
            Some(&user.id),
            None,
        )
        .await;
    }

    if let Some(hidden) = data.hide_badge {
        revolt_database::set_plus_badge_hidden(db, &mut user, hidden).await?;
    }

    Ok(Json(overview_of(db, &user.id).await?))
}

/// # Place a supercharge
///
/// Put one of your supercharges on a server you're in: the one you name,
/// else one that isn't placed. New supercharges are bought with an order.
#[openapi(tag = "Billing")]
#[post("/supercharges", data = "<data>")]
pub async fn apply_supercharge(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataApplySupercharge>,
) -> Result<Json<v0::BillingOverview>> {
    billing().await?;
    not_bot(&user)?;
    let data = validated(data.into_inner())?;

    let server = db.fetch_server(&data.server).await?;
    db.fetch_member(&server.id, &user.id)
        .await
        .map_err(|_| create_error!(NotFound))?;

    let mine = db.fetch_supercharges_by_user(&user.id).await?;
    let item = match &data.supercharge {
        Some(id) => mine
            .into_iter()
            .find(|item| &item.id == id)
            .ok_or_else(|| create_error!(NotFound))?,
        None => mine
            .into_iter()
            .find(|item| item.server_id.is_none())
            .ok_or_else(|| create_error!(NotFound))?,
    };

    let previous = item.server_id.clone();
    if previous.as_deref() != Some(server.id.as_str()) {
        // someone else moved it meanwhile (another tab): nothing changes
        if !db
            .move_supercharge(&item.id, &user.id, previous.as_deref(), Some(&server.id))
            .await?
        {
            return Err(create_error!(InvalidOperation));
        }
        if let Some(previous) = previous {
            sync_server_supercharge(db, &previous).await?;
        }
        sync_server_supercharge(db, &server.id).await?;
    }

    Ok(Json(overview_of(db, &user.id).await?))
}

/// # Change a supercharge
///
/// Take it off its server, or cancel or resume renewal of a bought one.
#[openapi(tag = "Billing")]
#[patch("/supercharges/<id>", data = "<data>")]
pub async fn edit_supercharge(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataEditSupercharge>,
) -> Result<Json<v0::BillingOverview>> {
    billing().await?;
    not_bot(&user)?;
    let data = data.into_inner();

    let mut item = db.fetch_supercharge(id).await?;
    if item.user_id != user.id {
        return Err(create_error!(NotFound));
    }

    if let Some(cancel) = data.cancel_at_period_end {
        if item.source != v0::SuperchargeSource::Purchase {
            return Err(create_error!(InvalidOperation));
        }
        match item.provider_subscription_id.clone() {
            // every feather of that purchase renews together at the provider
            Some(provider_id) => {
                let provider = provider_of_order(db, item.order_id.as_deref()).await?;
                provider.set_cancel_at_period_end(&provider_id, cancel).await?;
                sync_subscription(db, provider.as_ref(), &provider_id).await?;
            }
            None => {
                item.cancel_at_period_end = cancel;
                db.save_supercharge(&item).await?;
            }
        }
        audit(
            db,
            &user.id,
            if cancel { "feather.cancel" } else { "feather.resume" },
            item.order_id.as_deref(),
            Some(&user.id),
            None,
        )
        .await;
    }

    if data.unplace == Some(true) {
        if let Some(server_id) = item.server_id.clone() {
            if db
                .move_supercharge(&item.id, &user.id, Some(&server_id), None)
                .await?
            {
                sync_server_supercharge(db, &server_id).await?;
            }
        }
    }

    Ok(Json(overview_of(db, &user.id).await?))
}

/// # Server supercharges
///
/// A server's level and who is supercharging it. Members only.
#[openapi(tag = "Billing")]
#[get("/servers/<id>")]
pub async fn server_supercharges(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<v0::ServerSuperchargeInfo>> {
    let server = db.fetch_server(id).await?;
    db.fetch_member(&server.id, &user.id)
        .await
        .map_err(|_| create_error!(NotFound))?;

    let billing = config().await.features.billing;
    let items: Vec<_> = if billing.available() {
        db.fetch_supercharges_by_server(&server.id)
            .await?
            .into_iter()
            .filter(|item| billing.counts(item.test))
            .collect()
    } else {
        vec![]
    };

    let mut by_user: HashMap<String, (u32, Timestamp)> = HashMap::new();
    for item in &items {
        let entry = by_user
            .entry(item.user_id.clone())
            .or_insert((0, item.created_at));
        entry.0 += 1;
        if item.created_at < entry.1 {
            entry.1 = item.created_at;
        }
    }
    let mut superchargers: Vec<v0::ServerSupercharger> = by_user
        .into_iter()
        .map(|(user_id, (count, since))| v0::ServerSupercharger {
            user_id,
            count,
            since,
        })
        .collect();
    superchargers.sort_by(|a, b| a.since.cmp(&b.since));

    Ok(Json(v0::ServerSuperchargeInfo {
        count: items.len() as u32,
        level: billing.level_for(items.len()) as u32,
        superchargers,
    }))
}
