//! Paid Discover placement for servers and bots: boosts and the top row.
//! Bought with orders (`orders.rs`); this shows where a listing stands,
//! the places left in each tier and the line for full tiers.
//!
//! Only people who can manage a server (or own a bot) may see its numbers
//! or pay for it, and only listings moderators approved for Discover can be
//! promoted, so paying never skips moderation. A listing that stops being
//! listed (locked, taken off Discover, deleted) stops showing; nothing is
//! refunded.

use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_database::{
    active_promotions, epoch_ms, from_epoch_ms, promotion_capacity, taken_places,
    waitlist_positions, weekly_discover_browsers, Database, User, PROMOTED_KIND,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::orders::{listed_own_bot, managed_server};
use crate::routes::discover::listing::{ranked_bots, ranked_servers};

/// What is being promoted
struct Target {
    kind: &'static str,
    id: String,
    approved: bool,
    /// members (servers) or servers it is in (bots)
    size: u32,
    min_size: u32,
}

async fn status_of(db: &Database, target: Target) -> Result<v0::DiscoverPromotionStatus> {
    let billing = config().await.features.billing;
    let discover = &billing.discover;
    let now = epoch_ms(Timestamp::now_utc());

    let ranked_ids: Vec<String> = if target.kind == "bot" {
        ranked_bots(db, None).await?.into_iter().map(|card| card.id).collect()
    } else {
        ranked_servers(db, None).await?.into_iter().map(|card| card.id).collect()
    };
    let rank = ranked_ids
        .iter()
        .position(|id| id == &target.id)
        .map(|index| index as u32 + 1);
    let fallback = ranked_ids.len() as u32 + 1;

    let mine = active_promotions(db)
        .await?
        .remove(&target.id)
        .unwrap_or_default();
    let taken = taken_places(db, target.kind).await?;
    let (positions, lengths) = waitlist_positions(db, target.kind, &target.id).await?;

    let mut boosts = vec![];
    for tier in &discover.boosts {
        boosts.push(v0::DiscoverBoostOffer {
            kind: tier.kind.clone(),
            price: tier.price,
            guarantee: tier.guarantee as u32,
            projected_rank: rank.unwrap_or(fallback).min(tier.guarantee as u32),
            capacity: promotion_capacity(&tier.kind).await as u32,
            taken: taken.get(&tier.kind).copied().unwrap_or_default() as u32,
            waiting: lengths.get(&tier.kind).copied().unwrap_or_default() as u32,
            position: positions.get(&tier.kind).map(|(position, _)| *position as u32),
        });
    }

    let promoted_taken = taken.get(PROMOTED_KIND).copied().unwrap_or_default();
    Ok(v0::DiscoverPromotionStatus {
        target_kind: target.kind.to_string(),
        available: billing.available(),
        checkout_available: billing.mode().checkout_open(),
        test_mode: billing.available() && !billing.mode().live(),
        currency: billing.currency.clone(),
        days: discover.promotion_days,
        approved: target.approved,
        listed: rank.is_some(),
        rank,
        member_count: target.size,
        min_members: target.min_size,
        boost: mine.boost.as_ref().map(|(kind, ends_at)| v0::DiscoverActiveBoost {
            kind: kind.clone(),
            ends_at: from_epoch_ms(*ends_at),
        }),
        boosts,
        promoted: mine
            .promoted
            .filter(|item| item.ends_at > now)
            .map(|item| v0::DiscoverPromotedStats {
                started_at: from_epoch_ms(item.started_at),
                ends_at: from_epoch_ms(item.ends_at),
                views: item.views.max(0) as u64,
                clicks: item.clicks.max(0) as u64,
                joins: item.joins.max(0) as u64,
            }),
        promoted_price: discover.promoted_price,
        promoted_slots: discover.promoted_slots as u32,
        promoted_open: discover.promoted_slots.saturating_sub(promoted_taken) as u32,
        promoted_waiting: lengths.get(PROMOTED_KIND).copied().unwrap_or_default() as u32,
        promoted_position: positions
            .get(PROMOTED_KIND)
            .map(|(position, _)| *position as u32),
        weekly_browsers: if billing.available() {
            weekly_discover_browsers().await
        } else {
            0
        },
    })
}

/// # Discover promotion of a server
///
/// Where a server stands on Discover, the places left in each tier and the
/// line for full ones. For people who can manage the server.
#[openapi(tag = "Billing")]
#[get("/discover/<id>")]
pub async fn promotion_status(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<v0::DiscoverPromotionStatus>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let server = managed_server(db, &user, id).await?;
    let billing = config().await.features.billing;
    let target = Target {
        kind: "server",
        id: server.id.clone(),
        approved: server.discoverable,
        size: server.get_approximate_member_count(db).await as u32,
        min_size: if billing.available() {
            billing.discover.min_members as u32
        } else {
            0
        },
    };
    Ok(Json(status_of(db, target).await?))
}

/// # Discover promotion of a bot
///
/// The same as for servers, for a bot you own.
#[openapi(tag = "Billing")]
#[get("/discover/bots/<id>")]
pub async fn bot_promotion_status(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<v0::DiscoverPromotionStatus>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    // owner check first, approval is shown rather than refused
    let bot = db.fetch_bot(id).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }
    let approved = listed_own_bot(db, &user, id).await.is_ok();
    let target = Target {
        kind: "bot",
        id: bot.id.clone(),
        approved,
        size: db.fetch_server_count(&bot.id).await.unwrap_or(0) as u32,
        min_size: 0,
    };
    Ok(Json(status_of(db, target).await?))
}
