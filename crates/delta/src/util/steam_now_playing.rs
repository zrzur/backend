//! Steam "now playing" as an activity, for people who linked Steam and
//! chose to share it (`share_steam_activity`, under `share_activity`).
//! This makes "Playing ..." work on the web and phones too, where nothing
//! can see a running game. The desktop and apps over its RPC come first
//! (see `ActivitySource`); this is the third source.
//!
//! - Only people online right now, visible, sharing, with a link agreed
//!   under the current consent text are read. Nothing else is asked.
//! - One `GetPlayerSummaries` call reads up to 100 accounts; only the game's
//!   id and name are taken from the answer.
//! - Calls a day are capped (`steam_daily_calls`, shared Redis count, so
//!   several delta processes keep one budget). Rounds stretch to stay within
//!   it and stop for the day when it is spent.
//! - One delta does a round at a time (a Redis lock).
//!
//! Runs inside delta; running twice at once does no harm.

use std::{collections::HashMap, time::Duration};

use iso8601_timestamp::Timestamp;
use redis_kiss::{get_connection, AsyncCommands};
use revolt_database::{
    util::activity, Database, LinkedAccount, Presence, User, LINKED_CONSENT_VERSION,
};
use revolt_models::v0::{ActivitySource, ActivityType, UserActivity};
use revolt_result::Result;

/// Accounts one call reads (Steam's own limit)
pub const PER_CALL: usize = 100;
const LOCK_KEY: &str = "steam_now_playing:lock";

/// Start the loop
pub async fn run(db: Database) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        let config = revolt_config::config().await;
        let settings = config.features.rich_presence.clone();
        let mut wait = settings.steam_interval_seconds.max(30);
        if settings.enabled && settings.steam_enabled && config.api.connections.steam.enabled() {
            if take_lock(wait).await {
                match run_once(&db, Timestamp::now_utc()).await {
                    Ok(calls) => wait = stretched(wait, calls, settings.steam_daily_calls),
                    Err(error) => log::warn!("Steam now playing failed: {error:?}"),
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

/// Seconds to the next round so that rounds of `calls` calls stay within
/// `daily` calls a day, never under `interval`
pub fn stretched(interval: u64, calls: usize, daily: i64) -> u64 {
    if calls == 0 || daily <= 0 {
        return interval;
    }
    let needed = (calls as u64 * 86_400).div_ceil(daily as u64);
    interval.max(needed)
}

async fn take_lock(seconds: u64) -> bool {
    let Ok(conn) = get_connection().await else {
        return false;
    };
    let taken: Option<String> = redis_kiss::redis::cmd("SET")
        .arg(LOCK_KEY)
        .arg(1)
        .arg("NX")
        .arg("EX")
        .arg(seconds.saturating_sub(1).max(1))
        .query_async(&mut conn.into_inner())
        .await
        .unwrap_or(None);
    taken.is_some()
}

fn today() -> String {
    Timestamp::now_utc().to_string()[..10].to_string()
}

/// Count one call against today's budget; false once it is spent
async fn spend(daily: i64) -> bool {
    let Ok(mut conn) = get_connection().await else {
        return false;
    };
    let key = format!("steam_now_playing:calls:{}", today());
    let used: i64 = conn.incr(&key, 1).await.unwrap_or(i64::MAX);
    if used == 1 {
        let _: std::result::Result<(), _> = conn.expire(&key, 2 * 86_400).await;
    }
    used <= daily
}

/// Calls made today (for tests and the report)
pub async fn calls_today() -> i64 {
    let Ok(mut conn) = get_connection().await else {
        return 0;
    };
    conn.get::<_, Option<i64>>(format!("steam_now_playing:calls:{}", today()))
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// The people to read: online, visible, sharing Steam, link agreed
async fn readers(db: &Database) -> Result<Vec<(User, LinkedAccount)>> {
    let accounts: Vec<LinkedAccount> = db
        .fetch_linked_accounts_of_service("steam")
        .await?
        .into_iter()
        .filter(|account| account.consent >= LINKED_CONSENT_VERSION && !account.external_id.is_empty())
        .collect();
    if accounts.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<String> = accounts.iter().map(|account| account.user_id.clone()).collect();
    let online = revolt_presence::filter_online(&ids).await;
    let mut by_user: HashMap<String, LinkedAccount> = accounts
        .into_iter()
        .filter(|account| online.contains(&account.user_id))
        .map(|account| (account.user_id.clone(), account))
        .collect();
    if by_user.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<String> = by_user.keys().cloned().collect();
    let mut chosen = vec![];
    for user in db.fetch_users(&ids).await? {
        let invisible = user
            .status
            .as_ref()
            .and_then(|status| status.presence.as_ref())
            == Some(&Presence::Invisible);
        if invisible || !user.privacy().share_steam_activity || !activity::shares(db, &user).await {
            continue;
        }
        if let Some(account) = by_user.remove(&user.id) {
            chosen.push((user, account));
        }
    }
    Ok(chosen)
}

/// What Steam's answer means for one person
pub fn activity_of(
    playing: Option<&crate::util::steam::NowPlaying>,
    before: Option<&UserActivity>,
    account: &LinkedAccount,
    no_picture: &[u32],
    january: &str,
    now: Timestamp,
) -> Option<UserActivity> {
    let game = playing?;
    if account.hidden_titles.contains(&game.appid) {
        return None;
    }
    // the same game keeps the time it started
    let started_at = before
        .filter(|before| before.name == game.name)
        .and_then(|before| before.started_at)
        .or(Some(now));
    let large_image = (!no_picture.contains(&game.appid) && !january.is_empty()).then(|| {
        format!(
            "{january}/proxy?url={}",
            activity::encode_query(&crate::util::steam::header_url(game.appid))
        )
    });
    Some(UserActivity {
        source: ActivitySource::Steam,
        kind: ActivityType::Playing,
        name: game.name.clone(),
        details: None,
        state: None,
        started_at,
        ends_at: None,
        application_id: None,
        large_image,
        large_text: None,
        small_image: None,
        small_text: None,
        buttons: vec![],
    })
}

/// One round: read, keep and announce. Gives back the calls made.
pub async fn run_once(db: &Database, now: Timestamp) -> Result<usize> {
    let config = revolt_config::config().await;
    let settings = config.features.rich_presence.clone();
    let steam = config.api.connections.steam.clone();
    let hosts = activity::Hosts::from_config().await;

    let people = readers(db).await?;
    let mut calls = 0;
    for chunk in people.chunks(PER_CALL) {
        if !spend(settings.steam_daily_calls).await {
            log::info!("Steam now playing: today's calls are spent");
            break;
        }
        calls += 1;
        let ids: Vec<&str> = chunk.iter().map(|(_, account)| account.external_id.as_str()).collect();
        let playing = crate::util::steam::now_playing(&steam, &ids).await?;
        for (user, account) in chunk {
            let before = activity::list(&user.id)
                .await
                .into_iter()
                .find(|activity| activity.source == ActivitySource::Steam);
            let next = activity_of(
                playing.get(&account.external_id),
                before.as_ref(),
                account,
                &steam.no_picture_appids,
                &hosts.january,
                now,
            );
            if activity::store(&user.id, ActivitySource::Steam, next, None).await {
                activity::publish(db, user).await;
            }
        }
    }
    Ok(calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_stretch_to_the_budget() {
        // 14 calls a minute is 20,160 a day: over 20,000, so a little slower
        assert_eq!(stretched(60, 14, 20_000), 61);
        // 13 calls a minute fit
        assert_eq!(stretched(60, 13, 20_000), 60);
        // 100 calls (10,000 people) every 432 seconds
        assert_eq!(stretched(60, 100, 20_000), 432);
        assert_eq!(stretched(60, 0, 20_000), 60);
    }
}
