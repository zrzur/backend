//! Paid Discover placement: boosts that guarantee a place in the list and
//! the top row. Nothing here applies while billing is unavailable.

use std::collections::HashMap;

use iso8601_timestamp::Timestamp;
use redis_kiss::{
    get_connection,
    redis::{ExistenceCheck, SetExpiry, SetOptions},
    AsyncCommands,
};
use revolt_config::config;
use revolt_result::Result;

use crate::{epoch_ms, Database, DiscoverPromotion, PromotionStat, PROMOTED_KIND};

/// What a server has running on Discover
#[derive(Default, Clone, Debug)]
pub struct ServerPromotion {
    /// Biggest running boost and when it ends
    pub boost: Option<(String, i64)>,
    /// Top-row place
    pub promoted: Option<DiscoverPromotion>,
}

/// Running promotions by server (empty while billing is unavailable)
pub async fn active_promotions(db: &Database) -> Result<HashMap<String, ServerPromotion>> {
    let billing = config().await.features.billing;
    let mut map: HashMap<String, ServerPromotion> = HashMap::new();
    if !billing.available() {
        return Ok(map);
    }

    for item in db
        .fetch_active_promotions(epoch_ms(Timestamp::now_utc()))
        .await?
    {
        // sandbox places never show on a live server, nor live ones on a test server
        if !billing.counts(item.test) {
            continue;
        }
        let entry = map.entry(item.server_id.clone()).or_default();
        if item.kind == PROMOTED_KIND {
            if entry
                .promoted
                .as_ref()
                .is_none_or(|current| current.ends_at < item.ends_at)
            {
                entry.promoted = Some(item);
            }
        } else if let Some(rank) = billing.discover.boost_rank(&item.kind) {
            // only the biggest boost counts
            let bigger = entry.boost.as_ref().is_none_or(|(kind, _)| {
                billing.discover.boost_rank(kind).unwrap_or(0) < rank
            });
            if bigger {
                entry.boost = Some((item.kind.clone(), item.ends_at));
            }
        }
    }
    Ok(map)
}

/// Places of a tier (`Promoted` for the top row) per kind of listing
pub async fn promotion_capacity(kind: &str) -> usize {
    let billing = config().await.features.billing;
    if kind == PROMOTED_KIND {
        billing.discover.promoted_slots
    } else {
        billing.discover.boost_capacity(kind)
    }
}

/// Places taken right now, by tier, for servers or bots
pub async fn taken_places(db: &Database, target_kind: &str) -> Result<HashMap<String, usize>> {
    let billing = config().await.features.billing;
    let mut taken: HashMap<String, usize> = HashMap::new();
    for item in db
        .fetch_active_promotions(epoch_ms(Timestamp::now_utc()))
        .await?
    {
        if item.target_kind == target_kind && billing.counts(item.test) {
            *taken.entry(item.kind).or_default() += 1;
        }
    }
    Ok(taken)
}

/// Take a place of a tier for a server or bot, or add time to the one it
/// already has. Returns false when every place is taken.
pub async fn claim_promotion(
    db: &Database,
    target_kind: &str,
    target_id: &str,
    kind: &str,
    buyer_id: &str,
    days: u32,
    test: bool,
    order_id: &str,
) -> Result<bool> {
    let now_ms = epoch_ms(Timestamp::now_utc());
    let add_ms = days as i64 * 86_400_000;
    let capacity = promotion_capacity(kind).await;

    // the place it already holds, if any
    let current = db
        .fetch_active_promotions(now_ms)
        .await?
        .into_iter()
        .find(|item| {
            item.server_id == target_id && item.kind == kind && item.target_kind == target_kind
        })
        .map(|item| item.id);
    let candidates: Vec<String> = match current {
        Some(id) => vec![id],
        None => (0..capacity)
            .map(|n| crate::promotion_slot_id(target_kind, kind, n))
            .collect(),
    };

    for id in candidates {
        if db
            .purchase_promotion(crate::PromotionPurchase {
                id: &id,
                kind,
                target_kind,
                server_id: target_id,
                buyer_id,
                now_ms,
                add_ms,
                test,
                only_if_free: true,
                order_id,
            })
            .await?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Start whoever waits in line while places are free, first come first
/// served. Entries for listings that left Discover are dropped (the place
/// would only show nothing).
pub async fn process_waitlist(db: &Database) -> Result<()> {
    if !config().await.features.billing.available() {
        return Ok(());
    }
    let mut full: Vec<(String, String)> = vec![];
    for entry in db.fetch_waitlist().await? {
        let key = (entry.target_kind.clone(), entry.kind.clone());
        if full.contains(&key) {
            continue;
        }
        if claim_promotion(
            db,
            &entry.target_kind,
            &entry.target_id,
            &entry.kind,
            &entry.buyer_id,
            entry.days,
            entry.test,
            &entry.order_id,
        )
        .await?
        {
            db.delete_waitlist_entry(&entry.id).await?;
        } else {
            // everyone behind waits too
            full.push(key);
        }
    }
    Ok(())
}

/// Where a server or bot stands in line, by tier: (position from 1, people
/// in line for that tier)
pub async fn waitlist_positions(
    db: &Database,
    target_kind: &str,
    target_id: &str,
) -> Result<(HashMap<String, (usize, usize)>, HashMap<String, usize>)> {
    let mut mine: HashMap<String, (usize, usize)> = HashMap::new();
    let mut lengths: HashMap<String, usize> = HashMap::new();
    for entry in db.fetch_waitlist().await? {
        if entry.target_kind != target_kind {
            continue;
        }
        let length = lengths.entry(entry.kind.clone()).or_default();
        *length += 1;
        if entry.target_id == target_id {
            mine.entry(entry.kind.clone()).or_insert((*length, 0));
        }
    }
    for (kind, (_, total)) in mine.iter_mut() {
        *total = lengths.get(kind).copied().unwrap_or_default();
    }
    Ok((mine, lengths))
}

/// Count a view, click or join for a server in the top row. Each person
/// counts once a day per kind, so reloading doesn't inflate the numbers.
pub async fn record_promotion_event(
    db: &Database,
    server_id: &str,
    user_id: &str,
    stat: PromotionStat,
) -> Result<()> {
    let Some(promoted) = active_promotions(db)
        .await?
        .remove(server_id)
        .and_then(|promotion| promotion.promoted)
    else {
        return Ok(());
    };

    // without the dedupe store nothing is counted rather than overcounted
    let Ok(mut redis) = get_connection().await else {
        return Ok(());
    };
    let key = format!("promo:{}:{}:{}:{}", stat.field(), promoted.id, promoted.started_at, user_id);
    let first: Option<String> = redis
        .set_options(
            &key,
            1,
            SetOptions::default()
                .conditional_set(ExistenceCheck::NX)
                .with_expiration(SetExpiry::EX(60 * 60 * 24)),
        )
        .await
        .unwrap_or(None);
    if first.is_some() {
        db.increment_promotion_stat(&promoted.id, stat).await?;
    }
    Ok(())
}

fn browser_day_key(days_ago: i64) -> String {
    let day = epoch_ms(Timestamp::now_utc()) / 86_400_000 - days_ago;
    format!("discover:browsers:{day}")
}

/// Remember that a signed-in person browsed Discover today
pub async fn note_discover_browser(user_id: &str) {
    let Ok(mut redis) = get_connection().await else {
        return;
    };
    let key = browser_day_key(0);
    let _: std::result::Result<(), _> = redis.pfadd(&key, user_id).await;
    let _: std::result::Result<(), _> = redis.expire(&key, 60 * 60 * 24 * 8).await;
}

/// Different signed-in people who browsed Discover in the last 7 days
pub async fn weekly_discover_browsers() -> u64 {
    let Ok(mut redis) = get_connection().await else {
        return 0;
    };
    let keys: Vec<String> = (0..7).map(browser_day_key).collect();
    redis.pfcount(keys).await.unwrap_or(0)
}
