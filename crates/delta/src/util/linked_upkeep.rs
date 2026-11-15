//! Hourly upkeep of linked accounts, as the services' terms ask:
//! - Battle.net: a copy is deleted when its 30 days are over (Blizzard keeps
//!   copies to 30 days).
//! - Twitch (copies at most 24 hours old): names older than 23 hours are
//!   read again; when Twitch cannot answer, anything over 24 hours is blanked
//!   in the database, keeping only the id. A live flag older than that is
//!   checked again or dropped. Accounts Twitch no longer returns are unlinked.
//! - Twitch: the app-wide "removed at Twitch" subscription is made again if
//!   it is missing, and live/offline subscriptions for channels nobody links
//!   any more (account deleted) are ended.
//! - Twitch links agreed under an older consent text (before it named the US
//!   transfer) send nothing to Twitch until the person agrees again: not read,
//!   and their live-status subscriptions are ended (Consent sets them up
//!   again). Their copy is still blanked after 24 hours.
//!
//! Runs inside delta; running twice at once does no harm.

use std::{collections::HashSet, time::Duration};

use revolt_database::{Database, LinkedAccount};
use revolt_result::Result;

use crate::{
    routes::users::connections_oauth::{consent_current, now_ms, TWITCH_FRESH_MS},
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

/// A Twitch copy over 24 hours old with nothing to renew it: only the id stays
fn blank(account: &mut LinkedAccount) {
    account.display_name = String::new();
    account.profile_url = String::new();
    account.live = Some(false);
    account.live_title = None;
    account.avatar_url = None;
    account.bio = None;
    account.kind = None;
    account.game = None;
}

/// One round, at time `now`
pub async fn run_once(db: &Database, now: i64) -> Result<()> {
    // Battle.net: the 30 days are over
    for account in db.fetch_linked_accounts_of_service("battlenet").await? {
        if account.expires_at.map_or(true, |at| now >= at) {
            db.delete_linked_account(&account.user_id, "battlenet").await?;
        }
    }

    let config = Service::Twitch.config().await;
    if !config.enabled() {
        return Ok(());
    }
    if twitch::subscribe_revocations(&config).await.is_err() {
        log::warn!("Twitch revocation messages are not set up; trying again next hour");
    }
    let (accounts, waiting): (Vec<_>, Vec<_>) = db
        .fetch_linked_accounts_of_service("twitch")
        .await?
        .into_iter()
        .partition(consent_current);

    // linked before the consent card named the US transfer: nothing goes to
    // Twitch for them until the person agrees in Settings, and nothing kept
    // gets older than 24 hours
    for mut account in waiting {
        let mut changed = false;
        // no live-status messages either: the subscriptions end, and Consent
        // in Settings sets them up again. Removing Zrzur at Twitch can't
        // unlink such a link meanwhile; it is hidden and blanked anyway.
        if !account.subscriptions.is_empty() {
            twitch::unsubscribe(&config, &account.subscriptions).await;
            account.subscriptions.clear();
            changed = true;
        }
        if now - account.refreshed_at >= TWITCH_FRESH_MS && !account.display_name.is_empty() {
            blank(&mut account);
            changed = true;
        }
        if account.live == Some(true)
            && account.live_at.map_or(true, |at| now - at >= TWITCH_FRESH_MS)
        {
            account.live = Some(false);
            account.live_title = None;
            changed = true;
        }
        if changed {
            db.save_linked_account(&account).await?;
        }
    }

    // names
    let stale: Vec<_> = accounts
        .iter()
        .filter(|account| now - account.refreshed_at >= RENAME_AFTER_MS)
        .cloned()
        .collect();
    for batch in stale.chunks(100) {
        let ids: Vec<String> = batch.iter().map(|a| a.external_id.clone()).collect();
        match twitch::users(&config, &ids).await {
            Ok(found) => {
                // the category of each channel's last stream, also kept to 24 hours
                let games = twitch::channel_games(&config, &ids).await.ok();
                for mut account in batch.iter().cloned() {
                    match found.iter().find(|identity| identity.external_id == account.external_id) {
                        Some(identity) => {
                            account.display_name = identity.display_name.clone();
                            account.profile_url = identity.profile_url.clone();
                            crate::routes::users::connections_oauth::apply_details(
                                &mut account,
                                identity.details.clone(),
                            );
                            account.game = games
                                .as_ref()
                                .and_then(|games| games.get(&account.external_id).cloned());
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
            // Twitch failed: try again next hour, and keep nothing too old
            Err(_) => {
                for mut account in batch.iter().cloned() {
                    if now - account.refreshed_at >= TWITCH_FRESH_MS && !account.display_name.is_empty() {
                        blank(&mut account);
                        db.save_linked_account(&account).await?;
                    }
                }
            }
        }
    }

    // live flags that Twitch has not touched for a day
    let linked = db.fetch_linked_accounts_of_service("twitch").await?;
    let old_live: Vec<_> = linked
        .iter()
        .filter(|account| {
            consent_current(account)
                && account.live == Some(true)
                && account.live_at.map_or(true, |at| now - at >= RENAME_AFTER_MS)
        })
        .cloned()
        .collect();
    for batch in old_live.chunks(100) {
        let ids: Vec<String> = batch.iter().map(|a| a.external_id.clone()).collect();
        let live = twitch::live_streams(&config, &ids).await.ok();
        for mut account in batch.iter().cloned() {
            match &live {
                Some(live) => {
                    let stream = live.get(&account.external_id).cloned();
                    account.live = Some(stream.is_some());
                    if let Some(stream) = &stream {
                        if !stream.game.is_empty() {
                            account.game = Some(stream.game.clone());
                        }
                    }
                    account.live_title = stream.map(|s| s.title).filter(|t| !t.is_empty());
                    account.live_at = Some(now);
                }
                None if account.live_at.map_or(true, |at| now - at >= TWITCH_FRESH_MS) => {
                    account.live = Some(false);
                    account.live_title = None;
                }
                None => continue,
            }
            db.save_linked_account(&account).await?;
        }
    }

    // subscriptions for channels nobody links any more
    if let Ok(subscriptions) = twitch::stream_subscriptions(&config).await {
        // read after the list, so a link made meanwhile is not taken for an orphan
        let channels: HashSet<String> = db
            .fetch_linked_accounts_of_service("twitch")
            .await?
            .into_iter()
            .map(|a| a.external_id)
            .collect();
        let orphans: Vec<String> = subscriptions
            .into_iter()
            .filter(|(_, channel)| !channels.contains(channel))
            .map(|(id, _)| id)
            .collect();
        if !orphans.is_empty() {
            twitch::unsubscribe(&config, &orphans).await;
        }
    }
    Ok(())
}
