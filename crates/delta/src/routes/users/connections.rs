//! Linked accounts: Steam.
//!
//! Linking: `start` hands out Steam's own sign-in page and remembers, for ten
//! minutes, which person asked; Steam sends them back to the web app, which
//! passes what Steam sent to `finish`. Only the person's own request reads
//! Steam (link, Refresh): never someone opening a profile, never a schedule.

use iso8601_timestamp::Timestamp;
use redis_kiss::{get_connection, AsyncCommands};
use revolt_config::{config, ApiConnectionSteam};
use revolt_database::{Database, LinkedAccount, User, LINKED_CONSENT_VERSION};
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

/// The link as its owner sees it: everything copied, hidden games included
pub fn owner_view(account: LinkedAccount, blocked: &[u32]) -> v0::LinkedAccount {
    let current = account.consent >= LINKED_CONSENT_VERSION;
    v0::LinkedAccount {
        service: account.service,
        display_name: account.display_name,
        profile_url: account.profile_url,
        shown: account.shown,
        private: account.private,
        game_count: account.game_count,
        total_hours: account.total_hours,
        achievements: achievements_view(&account.games, account.achievements, &[], true, blocked),
        games: account.games.into_iter().map(|g| game_view(g, true, blocked)).collect(),
        hidden_titles: account.hidden_titles,
        linked_at: account.linked_at,
        refreshed_at: account.refreshed_at,
        refresh_after: account.refreshed_at + REFRESH_COOLDOWN_MS,
        consent_current: current,
    }
}

/// One game; `pictures` only when the person agreed to the text naming them
fn game_view(game: revolt_database::LinkedGame, pictures: bool, blocked: &[u32]) -> v0::LinkedGame {
    let pictures = pictures && !blocked.contains(&game.appid);
    v0::LinkedGame {
        icon_url: game
            .icon
            .as_deref()
            .filter(|_| pictures)
            .map(|hash| steam::icon_url(game.appid, hash)),
        header_url: pictures.then(|| steam::header_url(game.appid)),
        store_url: steam::store_url(game.appid),
        appid: game.appid,
        name: game.name,
        hours: game.hours,
        achievements_unlocked: game.achievements_unlocked,
        achievements_total: game.achievements_total,
    }
}

/// Achievements with their game's name, leaving out hidden games. `icons`
/// is false for a viewer who did not declare they are an adult: Steam has
/// no age rating for achievement icons.
fn achievements_view(
    games: &[revolt_database::LinkedGame],
    achievements: Vec<revolt_database::LinkedAchievement>,
    hidden: &[u32],
    icons: bool,
    blocked: &[u32],
) -> Vec<v0::LinkedAchievement> {
    achievements
        .into_iter()
        .filter(|a| !hidden.contains(&a.appid))
        .filter_map(|a| {
            // an achievement of a game no longer in the list is dropped
            let game = games.iter().find(|g| g.appid == a.appid)?;
            Some(v0::LinkedAchievement {
                icon_url: a
                    .icon
                    .as_deref()
                    .filter(|_| icons && !blocked.contains(&a.appid))
                    .map(|hash| steam::icon_url(a.appid, hash)),
                game: game.name.clone(),
                appid: a.appid,
                name: a.name,
                description: a.description,
                unlocked_on: a.unlocked_on,
            })
        })
        .collect()
}

/// The games section as another person sees it: only what the owner shows,
/// without hidden titles, and without name or link while it is private
/// there. A link made under the first consent text shows no pictures and no
/// achievements until the person agrees to the current one.
pub fn profile_view(
    account: LinkedAccount,
    adult_viewer: bool,
    blocked: &[u32],
) -> Option<v0::ProfileGames> {
    if !account.shown {
        return None;
    }
    let hidden = account.hidden_titles;
    let current = account.consent >= LINKED_CONSENT_VERSION;
    Some(if account.private {
        v0::ProfileGames {
            service: account.service,
            display_name: String::new(),
            profile_url: String::new(),
            private: true,
            game_count: None,
            total_hours: None,
            games: vec![],
            achievements: vec![],
        }
    } else {
        v0::ProfileGames {
            service: account.service,
            display_name: account.display_name,
            profile_url: account.profile_url,
            private: false,
            game_count: account.game_count,
            total_hours: account.total_hours,
            achievements: if current {
                achievements_view(&account.games, account.achievements, &hidden, adult_viewer, blocked)
            } else {
                vec![]
            },
            games: account
                .games
                .into_iter()
                .filter(|game| !hidden.contains(&game.appid))
                .map(|game| game_view(game, current, blocked))
                .collect(),
        }
    })
}

/// The games section of this profile for this viewer, if any
pub async fn profile_games(
    db: &Database,
    user_id: &str,
    viewer: &User,
) -> Result<Option<v0::ProfileGames>> {
    let Some(account) = db.fetch_linked_account(user_id, SERVICE).await? else {
        return Ok(None);
    };
    let adult = revolt_database::util::age::owner_band(db, viewer).await
        == v0::AgeBand::AdultDeclared;
    Ok(profile_view(account, adult, &steam_config().await.no_picture_appids))
}

/// Steam's text goes through the platform word list like profile text: a
/// game or achievement whose text it matches is left out, and a name it
/// matches is not shown
async fn filter_words(db: &Database, snapshot: &mut steam::SteamSnapshot) -> Result<()> {
    let words = db.fetch_platform_words().await?;
    let hit = |text: &str| revolt_database::match_platform_words(text, &words).is_some();
    if hit(&snapshot.display_name) {
        snapshot.display_name = String::new();
    }
    snapshot.games.retain(|game| !hit(&game.name));
    let games = &snapshot.games;
    snapshot.achievements.retain(|a| {
        games.iter().any(|g| g.appid == a.appid) && !hit(&a.name) && !hit(&a.description)
    });
    Ok(())
}

fn apply(account: &mut LinkedAccount, snapshot: steam::SteamSnapshot) {
    account.display_name = snapshot.display_name;
    account.profile_url = snapshot.profile_url;
    account.private = snapshot.private;
    account.game_count = snapshot.game_count;
    account.total_hours = snapshot.total_hours;
    account.games = snapshot.games;
    account.achievements = snapshot.achievements;
    account.refreshed_at = now_ms();
}

/// # Fetch Linked Accounts
///
/// Your linked accounts, and whether Steam can be linked on this server.
#[openapi(tag = "User Information")]
#[get("/@me/connections")]
pub async fn list(db: &State<Database>, user: User) -> Result<Json<v0::LinkedAccounts>> {
    no_bots(&user)?;
    let steam = steam_config().await;
    let blocked = steam.no_picture_appids.clone();
    Ok(Json(v0::LinkedAccounts {
        steam: v0::SteamConnection {
            available: steam.enabled(),
            account: db
                .fetch_linked_account(&user.id, SERVICE)
                .await?
                .map(|account| owner_view(account, &blocked)),
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

    let mut snapshot = steam::fetch(&steam, &steam_id).await?;
    filter_words(db, &mut snapshot).await?;
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
        achievements: vec![],
        hidden_titles: previous
            .filter(|previous| previous.external_id == steam_id)
            .map(|previous| previous.hidden_titles)
            .unwrap_or_default(),
        // the consent dialog the app shows before Steam is the current one
        consent: LINKED_CONSENT_VERSION,
        linked_at: now,
        refreshed_at: now,
    };
    apply(&mut account, snapshot);
    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account, &steam_config().await.no_picture_appids)))
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
    if let Some(version) = data.consent {
        if version != LINKED_CONSENT_VERSION {
            return Err(create_error!(InvalidOperation));
        }
        account.consent = version;
    }
    if let Some(mut hidden) = data.hidden_titles {
        hidden.sort_unstable();
        hidden.dedup();
        hidden.truncate(MAX_HIDDEN);
        account.hidden_titles = hidden;
    }
    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account, &steam_config().await.no_picture_appids)))
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

    let mut snapshot = steam::fetch(&steam, &account.external_id).await?;
    filter_words(db, &mut snapshot).await?;
    apply(&mut account, snapshot);
    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account, &steam_config().await.no_picture_appids)))
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
