//! Hourly upkeep of linked accounts, as the services' terms ask:
//! - Twitch: names older than 23 hours are read again (copies may be at most
//!   24 hours old); a live flag older than that is checked again. Accounts
//!   Twitch no longer returns (deleted, suspended) are unlinked.
//! - Battle.net: a copy whose 30 days ran out more than 7 days ago is deleted.
//!
//! Runs inside delta; running twice at once does no harm.

use std::time::Duration;

use revolt_database::Database;
use revolt_result::Result;

use crate::{
    routes::users::connections_oauth::{now_ms, BATTLENET_GRACE_MS},
    util::{oauth_link::Service, twitch},
};

const RENAME_AFTER_MS: i64 = 23 * 60 * 60 * 1000;

/// Start the hourly loop
pub async fn run(db: Database) {
    // let the server settle first
    tokio::time::sleep(Duration::from_secs(5 * 60)).await;
    loop {
        if let Err(error) = run_once(&db, now_ms()).await {
            log::warn!("linked accounts upkeep failed: {error:?}");
        }
        tokio::time::sleep(Duration::from_secs(60 * 60)).await;
    }
}

/// One round, at time `now`
pub async fn run_once(db: &Database, now: i64) -> Result<()> {
    // Battle.net: hidden for 7 days, now gone
    for account in db.fetch_linked_accounts_of_service("battlenet").await? {
        if account
            .expires_at
            .map_or(true, |at| now >= at + BATTLENET_GRACE_MS)
        {
            db.delete_linked_account(&account.user_id, "battlenet").await?;
        }
    }

    let config = Service::Twitch.config().await;
    if !config.enabled() {
        return Ok(());
    }
    let accounts = db.fetch_linked_accounts_of_service("twitch").await?;

    // names
    let stale: Vec<_> = accounts
        .iter()
        .filter(|account| now - account.refreshed_at >= RENAME_AFTER_MS)
        .cloned()
        .collect();
    for batch in stale.chunks(100) {
        let ids: Vec<String> = batch.iter().map(|a| a.external_id.clone()).collect();
        let Ok(found) = twitch::users(&config, &ids).await else {
            // Twitch failed: try again next hour; the profile hides the
            // name once it is over 25 hours old
            continue;
        };
        for mut account in batch.iter().cloned() {
            match found.iter().find(|identity| identity.external_id == account.external_id) {
                Some(identity) => {
                    account.display_name = identity.display_name.clone();
                    account.profile_url = identity.profile_url.clone();
                    account.refreshed_at = now;
                    db.save_linked_account(&account).await?;
                }
                None => {
                    twitch::unsubscribe(&config, &account.subscriptions).await;
                    db.delete_linked_account(&account.user_id, "twitch").await?;
                }
            }
        }
    }

    // live flags that Twitch has not touched for a day
    let old_live: Vec<_> = db
        .fetch_linked_accounts_of_service("twitch")
        .await?
        .into_iter()
        .filter(|account| {
            account.live == Some(true)
                && account.live_at.map_or(true, |at| now - at >= RENAME_AFTER_MS)
        })
        .collect();
    for batch in old_live.chunks(100) {
        let ids: Vec<String> = batch.iter().map(|a| a.external_id.clone()).collect();
        let Ok(live) = twitch::live_streams(&config, &ids).await else {
            continue;
        };
        for mut account in batch.iter().cloned() {
            let title = live.get(&account.external_id).cloned();
            account.live = Some(title.is_some());
            account.live_title = title.filter(|t| !t.is_empty());
            account.live_at = Some(now);
            db.save_linked_account(&account).await?;
        }
    }
    Ok(())
}
