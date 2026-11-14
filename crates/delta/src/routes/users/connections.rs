//! Linked accounts: Steam.
//!
//! Linking: `start` hands out Steam's own sign-in page and remembers, for ten
//! minutes, which person asked; Steam sends them back to the web app, which
//! passes what Steam sent to `finish`. Only the person's own request reads
//! Steam (link, Refresh): never someone opening a profile, never a schedule.

use iso8601_timestamp::Timestamp;
use redis_kiss::{get_connection, AsyncCommands};
use revolt_config::{config, ApiConnectionSteam};
use revolt_database::{Database, LinkedAccount, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use crate::util::steam;

const SERVICE: &str = "steam";
/// How long a started sign-in may take
const STATE_SECONDS: usize = 600;
/// Time between two refreshes
pub const REFRESH_COOLDOWN_MS: i64 = 10 * 60 * 1000;
/// Most hidden games
const MAX_HIDDEN: usize = 50;

fn now_ms() -> i64 {
    Timestamp::now_utc()
        .duration_since(Timestamp::UNIX_EPOCH)
        .whole_milliseconds() as i64
}

fn state_key(state: &str) -> String {
    format!("linked-steam-state:{state}")
}

async fn steam_config() -> ApiConnectionSteam {
    config().await.api.connections.steam
}

async fn enabled_config() -> Result<ApiConnectionSteam> {
    let steam = steam_config().await;
    if steam.enabled() {
        Ok(steam)
    } else {
        Err(create_error!(FeatureDisabled {
            feature: "steam".to_string()
        }))
    }
}

fn no_bots(user: &User) -> Result<()> {
    if user.bot.is_some() {
        Err(create_error!(IsBot))
    } else {
        Ok(())
    }
}

/// The link as its owner sees it
pub fn owner_view(account: LinkedAccount) -> v0::LinkedAccount {
    v0::LinkedAccount {
        service: account.service,
        display_name: account.display_name,
        profile_url: account.profile_url,
        shown: account.shown,
        private: account.private,
        game_count: account.game_count,
        total_hours: account.total_hours,
        games: account.games.into_iter().map(game_view).collect(),
        hidden_titles: account.hidden_titles,
        linked_at: account.linked_at,
        refreshed_at: account.refreshed_at,
        refresh_after: account.refreshed_at + REFRESH_COOLDOWN_MS,
    }
}

fn game_view(game: revolt_database::LinkedGame) -> v0::LinkedGame {
    v0::LinkedGame {
        appid: game.appid,
        name: game.name,
        hours: game.hours,
        achievements_unlocked: game.achievements_unlocked,
        achievements_total: game.achievements_total,
    }
}

/// The games section as another person sees it: only what the owner shows,
/// without hidden titles, and without name or link while it is private there
pub fn profile_view(account: LinkedAccount) -> Option<v0::ProfileGames> {
    if !account.shown {
        return None;
    }
    let hidden = account.hidden_titles;
    Some(if account.private {
        v0::ProfileGames {
            service: account.service,
            display_name: String::new(),
            profile_url: String::new(),
            private: true,
            game_count: None,
            total_hours: None,
            games: vec![],
        }
    } else {
        v0::ProfileGames {
            service: account.service,
            display_name: account.display_name,
            profile_url: account.profile_url,
            private: false,
            game_count: account.game_count,
            total_hours: account.total_hours,
            games: account
                .games
                .into_iter()
                .filter(|game| !hidden.contains(&game.appid))
                .map(game_view)
                .collect(),
        }
    })
}

/// The games section of this profile for this viewer, if any
pub async fn profile_games(db: &Database, user_id: &str) -> Result<Option<v0::ProfileGames>> {
    Ok(db
        .fetch_linked_account(user_id, SERVICE)
        .await?
        .and_then(profile_view))
}

fn apply(account: &mut LinkedAccount, snapshot: steam::SteamSnapshot) {
    account.display_name = snapshot.display_name;
    account.profile_url = snapshot.profile_url;
    account.private = snapshot.private;
    account.game_count = snapshot.game_count;
    account.total_hours = snapshot.total_hours;
    account.games = snapshot.games;
    account.refreshed_at = now_ms();
}

/// # Fetch Linked Accounts
///
/// Your linked accounts, and whether Steam can be linked on this server.
#[openapi(tag = "User Information")]
#[get("/@me/connections")]
pub async fn list(db: &State<Database>, user: User) -> Result<Json<v0::LinkedAccounts>> {
    no_bots(&user)?;
    Ok(Json(v0::LinkedAccounts {
        steam: v0::SteamConnection {
            available: steam_config().await.enabled(),
            account: db
                .fetch_linked_account(&user.id, SERVICE)
                .await?
                .map(owner_view),
        },
    }))
}

/// # Start Linking Steam
///
/// Returns Steam's own sign-in page. Steam sends the person back to
/// `<origin>/connections/steam/return`; the web app then calls Finish.
#[openapi(tag = "User Information")]
#[post("/@me/connections/steam/start", data = "<data>")]
pub async fn steam_start(
    user: User,
    data: Json<v0::DataSteamStart>,
) -> Result<Json<v0::SteamStartResponse>> {
    no_bots(&user)?;
    let steam = enabled_config().await?;
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let origin = data.origin.trim_end_matches('/');
    if !steam
        .return_origins
        .iter()
        .any(|allowed| allowed.trim_end_matches('/') == origin)
    {
        return Err(create_error!(InvalidOperation));
    }

    let state = nanoid::nanoid!(32);
    let mut redis = get_connection()
        .await
        .map_err(|_| create_error!(InternalError))?
        .into_inner();
    let _: () = redis
        .set_ex(state_key(&state), format!("{}|{origin}", user.id), STATE_SECONDS)
        .await
        .map_err(|_| create_error!(InternalError))?;

    let back = steam::return_to(origin, &state);
    Ok(Json(v0::SteamStartResponse {
        url: steam::login_url(&steam.openid_endpoint, origin, &back),
    }))
}

/// # Finish Linking Steam
///
/// Checks what Steam sent back, with Steam, then copies what the account
/// shows publicly. The link starts hidden: it shows on the profile only
/// after the person checked the preview.
#[openapi(tag = "User Information")]
#[post("/@me/connections/steam/finish", data = "<data>")]
pub async fn steam_finish(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataSteamFinish>,
) -> Result<Json<v0::LinkedAccount>> {
    no_bots(&user)?;
    let steam = enabled_config().await?;
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // the state works once, and only for the person who started
    let mut redis = get_connection()
        .await
        .map_err(|_| create_error!(InternalError))?
        .into_inner();
    let started: Option<String> = redis
        .get_del(state_key(&data.state))
        .await
        .map_err(|_| create_error!(InternalError))?;
    let Some((owner, origin)) = started.as_deref().and_then(|value| value.split_once('|')) else {
        return Err(create_error!(LinkedSignInFailed));
    };
    if owner != user.id {
        return Err(create_error!(LinkedSignInFailed));
    }

    let back = steam::return_to(origin, &data.state);
    let steam_id = steam::check_fields(&data.params, &steam.openid_endpoint, &back)?;
    steam::confirm(&steam, &data.params).await?;

    if let Some(other) = db.fetch_linked_account_owner(SERVICE, &steam_id).await? {
        if other != user.id {
            return Err(create_error!(LinkedAccountTaken));
        }
    }

    let snapshot = steam::fetch(&steam, &steam_id).await?;
    let previous = db.fetch_linked_account(&user.id, SERVICE).await?;
    let now = now_ms();
    let mut account = LinkedAccount {
        id: LinkedAccount::key(&user.id, SERVICE),
        user_id: user.id.clone(),
        service: SERVICE.to_string(),
        external_id: steam_id.clone(),
        display_name: String::new(),
        profile_url: String::new(),
        // a new link, or another Steam account: preview first
        shown: false,
        private: false,
        game_count: None,
        total_hours: None,
        games: vec![],
        hidden_titles: previous
            .filter(|previous| previous.external_id == steam_id)
            .map(|previous| previous.hidden_titles)
            .unwrap_or_default(),
        linked_at: now,
        refreshed_at: now,
    };
    apply(&mut account, snapshot);
    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account)))
}

/// # Edit Linked Steam Account
///
/// Show or hide it on your profile, and choose games not to show.
#[openapi(tag = "User Information")]
#[patch("/@me/connections/steam", data = "<data>")]
pub async fn steam_edit(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataEditLinkedAccount>,
) -> Result<Json<v0::LinkedAccount>> {
    no_bots(&user)?;
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let mut account = db
        .fetch_linked_account(&user.id, SERVICE)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;

    if let Some(shown) = data.shown {
        account.shown = shown;
    }
    if let Some(mut hidden) = data.hidden_titles {
        hidden.sort_unstable();
        hidden.dedup();
        hidden.truncate(MAX_HIDDEN);
        account.hidden_titles = hidden;
    }
    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account)))
}

/// # Refresh Linked Steam Account
///
/// Copy again what the Steam account shows publicly. Once every 10 minutes.
#[openapi(tag = "User Information")]
#[post("/@me/connections/steam/refresh")]
pub async fn steam_refresh(
    db: &State<Database>,
    user: User,
) -> Result<Json<v0::LinkedAccount>> {
    no_bots(&user)?;
    let steam = enabled_config().await?;
    let mut account = db
        .fetch_linked_account(&user.id, SERVICE)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;

    let wait = account.refreshed_at + REFRESH_COOLDOWN_MS - now_ms();
    if wait > 0 {
        return Err(create_error!(LinkedRefreshTooSoon {
            retry_after: (wait as u64).div_ceil(1000)
        }));
    }

    let snapshot = steam::fetch(&steam, &account.external_id).await?;
    apply(&mut account, snapshot);
    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account)))
}

/// # Disconnect Steam
///
/// Removes the link and everything copied from Steam, at once.
#[openapi(tag = "User Information")]
#[delete("/@me/connections/steam")]
pub async fn steam_disconnect(db: &State<Database>, user: User) -> Result<EmptyResponse> {
    no_bots(&user)?;
    db.delete_linked_account(&user.id, SERVICE).await?;
    Ok(EmptyResponse)
}
