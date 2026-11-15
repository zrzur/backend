//! Linked accounts through the service's own OAuth page: Twitch, GitHub,
//! Battle.net (`util::oauth_link`). Same rules as Steam: the link starts
//! hidden, the person checks the preview, Disconnect deletes the copy, and
//! no token is kept.
//!
//! What may be kept for how long follows each service's terms:
//! - Twitch: names and live status at most 24 hours old (`util::linked_upkeep`
//!   reads names again daily; live status comes from Twitch's own messages).
//! - Battle.net: the BattleTag is kept for 30 days after the person's last
//!   sign-in there and deleted then, unless they confirmed again before.
//! - GitHub: public repositories, read on link and on Refresh only.

use iso8601_timestamp::Timestamp;
use redis_kiss::{get_connection, AsyncCommands};
use revolt_config::ApiConnectionOAuth;
use revolt_database::{
    util::linked_crypto::pkce_challenge, Database, LinkedAccount, User, LINKED_CONSENT_VERSION,
    LINKED_MAX_SHOWN_REPOS,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use crate::util::{
    github,
    oauth_link::{self, Service},
    twitch,
};

/// How long a started sign-in may take
const STATE_SECONDS: usize = 600;
/// Time between two refreshes (GitHub)
pub const REFRESH_COOLDOWN_MS: i64 = 10 * 60 * 1000;
/// Battle.net: how long a copy may be kept after the person's sign-in there
pub const BATTLENET_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Twitch: oldest name or live flag that may be kept or shown
pub const TWITCH_FRESH_MS: i64 = 24 * 60 * 60 * 1000;
/// GitHub: repositories shown after linking, before the person picks
const DEFAULT_SHOWN_REPOS: usize = 3;

pub fn now_ms() -> i64 {
    Timestamp::now_utc()
        .duration_since(Timestamp::UNIX_EPOCH)
        .whole_milliseconds() as i64
}

fn state_key(service: Service, state: &str) -> String {
    format!("linked-{}-state:{state}", service.id())
}

fn service_of(name: &str) -> Result<Service> {
    Service::parse(name).ok_or_else(|| create_error!(NotFound))
}

/// The service can be linked on this server. Twitch also needs its live
/// messages: without them nobody would hear when someone removes Zrzur at
/// Twitch, and the link would stay (Twitch Developer Agreement).
pub fn usable(service: Service, config: &ApiConnectionOAuth) -> bool {
    match service {
        Service::Twitch => config.live_enabled(),
        _ => config.enabled(),
    }
}

async fn enabled_config(service: Service) -> Result<ApiConnectionOAuth> {
    let config = service.config().await;
    if usable(service, &config) {
        Ok(config)
    } else {
        Err(create_error!(FeatureDisabled {
            feature: service.id().to_string()
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

fn repo_view(repo: revolt_database::LinkedRepo) -> v0::LinkedRepo {
    v0::LinkedRepo {
        id: repo.id,
        name: repo.name,
        description: repo.description,
        language: repo.language,
        stars: repo.stars,
        url: repo.url,
    }
}

/// Live status as it may show: only while fresh
fn live_now(account: &LinkedAccount, now: i64) -> (Option<bool>, Option<String>) {
    if account.service != "twitch" {
        return (None, None);
    }
    let fresh = account.live_at.is_some_and(|at| now - at < TWITCH_FRESH_MS);
    if account.live == Some(true) && fresh {
        (Some(true), account.live_title.clone().filter(|t| !t.is_empty()))
    } else {
        (Some(false), None)
    }
}

/// Copy what the service shows publicly about the account (picture, bio,
/// counts) onto the link
pub fn apply_details(account: &mut LinkedAccount, details: oauth_link::Details) {
    account.avatar_url = details.avatar_url;
    account.real_name = details.real_name;
    account.bio = details.bio;
    account.followers = details.followers;
    account.public_repos = details.public_repos;
    account.kind = details.kind;
}

/// The link as its owner sees it
pub fn owner_view(account: LinkedAccount) -> v0::ServiceAccount {
    let (live, live_title) = live_now(&account, now_ms());
    v0::ServiceAccount {
        consent_current: consent_current(&account),
        refresh_after: if account.service == "github" {
            account.refreshed_at + REFRESH_COOLDOWN_MS
        } else {
            0
        },
        service: account.service,
        display_name: account.display_name,
        profile_url: account.profile_url,
        shown: account.shown,
        live,
        live_title,
        repos: account.repos.into_iter().map(repo_view).collect(),
        shown_repos: account.shown_repos,
        expires_at: account.expires_at,
        linked_at: account.linked_at,
        refreshed_at: account.refreshed_at,
        avatar_url: account.avatar_url,
        real_name: account.real_name,
        bio: account.bio,
        followers: account.followers,
        public_repos: account.public_repos,
        kind: account.kind,
        game: account.game,
    }
}

/// The person agreed to the consent text in use, which names the transfer
/// to the company in the United States
pub fn consent_current(account: &LinkedAccount) -> bool {
    account.consent >= LINKED_CONSENT_VERSION
}

/// The link as another person sees it, if it shows at all
pub fn profile_view(account: LinkedAccount, now: i64) -> Option<v0::ProfileConnection> {
    if !account.shown || account.display_name.is_empty() {
        return None;
    }
    match account.service.as_str() {
        // linked before the card named the US transfer and what the copy
        // may reveal (Art. 9): waits for the person's agreement
        "twitch" | "github" if !consent_current(&account) => return None,
        // the 30 days Battle.net allows are over
        "battlenet" if account.expires_at.map_or(true, |at| now >= at) => return None,
        // the daily name check has not run for too long
        "twitch" if now - account.refreshed_at >= TWITCH_FRESH_MS => return None,
        _ => {}
    }
    let (live, live_title) = live_now(&account, now);
    let shown = account.shown_repos.clone();
    Some(v0::ProfileConnection {
        service: account.service,
        display_name: account.display_name,
        profile_url: account.profile_url,
        live,
        live_title,
        // in the person's chosen order
        repos: shown
            .iter()
            .filter_map(|id| account.repos.iter().find(|repo| repo.id == *id).cloned())
            .map(repo_view)
            .collect(),
        avatar_url: account.avatar_url,
        real_name: account.real_name,
        bio: account.bio,
        followers: account.followers,
        public_repos: account.public_repos,
        kind: account.kind,
        game: account.game,
    })
}

/// Linked Twitch, GitHub and Battle.net accounts on this profile, in a
/// fixed order
pub async fn profile_connections(db: &Database, user_id: &str) -> Result<Vec<v0::ProfileConnection>> {
    let now = now_ms();
    let mut list = vec![];
    for service in Service::ALL {
        if let Some(view) = db
            .fetch_linked_account(user_id, service.id())
            .await?
            .and_then(|account| profile_view(account, now))
        {
            list.push(view);
        }
    }
    Ok(list)
}

/// One service for the settings list
pub async fn connection(db: &Database, user_id: &str, service: Service) -> Result<v0::ServiceConnection> {
    let mut account = db.fetch_linked_account(user_id, service.id()).await?;
    // Battle.net's 30 days are over: the copy goes now, not at the next upkeep
    if account
        .as_ref()
        .is_some_and(|a| a.expires_at.is_some_and(|at| now_ms() >= at))
    {
        db.delete_linked_account(user_id, service.id()).await?;
        account = None;
    }
    Ok(v0::ServiceConnection {
        available: usable(service, &service.config().await),
        account: account.map(owner_view),
    })
}

/// # Start Linking a Service
///
/// Returns the service's own sign-in page (`twitch`, `github` or
/// `battlenet`). The service sends the person back to
/// `<origin>/connections/<service>/return`; the web app then calls Finish.
#[openapi(tag = "User Information")]
#[post("/@me/connections/<service>/start", data = "<data>")]
pub async fn service_start(
    user: User,
    service: &str,
    data: Json<v0::DataServiceStart>,
) -> Result<Json<v0::ServiceStartResponse>> {
    no_bots(&user)?;
    let service = service_of(service)?;
    let config = enabled_config(service).await?;
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let origin = data.origin.trim_end_matches('/');
    if !config
        .return_origins
        .iter()
        .any(|allowed| allowed.trim_end_matches('/') == origin)
    {
        return Err(create_error!(InvalidOperation));
    }

    let state = nanoid::nanoid!(32);
    let verifier = if service.uses_pkce() {
        nanoid::nanoid!(64)
    } else {
        String::new()
    };
    let mut redis = get_connection()
        .await
        .map_err(|_| create_error!(InternalError))?
        .into_inner();
    let _: () = redis
        .set_ex(
            state_key(service, &state),
            format!("{}|{origin}|{verifier}", user.id),
            STATE_SECONDS,
        )
        .await
        .map_err(|_| create_error!(InternalError))?;

    let challenge = (!verifier.is_empty()).then(|| pkce_challenge(&verifier));
    let back = oauth_link::return_to(origin, service);
    Ok(Json(v0::ServiceStartResponse {
        url: service.authorize_url(&config, &back, &state, challenge.as_deref()),
    }))
}

/// # Finish Linking a Service
///
/// Swaps the code the service sent back for a token, reads who the person
/// is with it, and drops it. A new link starts hidden: it shows on the
/// profile only after the person checked the preview. Signing in again
/// with the same account keeps their choices (Battle.net: confirms again).
#[openapi(tag = "User Information")]
#[post("/@me/connections/<service>/finish", data = "<data>")]
pub async fn service_finish(
    db: &State<Database>,
    user: User,
    service: &str,
    data: Json<v0::DataServiceFinish>,
) -> Result<Json<v0::ServiceAccount>> {
    no_bots(&user)?;
    let service = service_of(service)?;
    let config = enabled_config(service).await?;
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // the state works once, and only for the person who started
    let mut redis = get_connection()
        .await
        .map_err(|_| create_error!(InternalError))?
        .into_inner();
    let started: Option<String> = redis
        .get_del(state_key(service, &data.state))
        .await
        .map_err(|_| create_error!(InternalError))?;
    let Some(started) = started else {
        return Err(create_error!(LinkedSignInFailed));
    };
    let mut parts = started.splitn(3, '|');
    let (Some(owner), Some(origin), Some(verifier)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(create_error!(LinkedSignInFailed));
    };
    if owner != user.id {
        return Err(create_error!(LinkedSignInFailed));
    }

    let back = oauth_link::return_to(origin, service);
    let verifier = (!verifier.is_empty()).then_some(verifier);
    let token = oauth_link::exchange_code(service, &config, &data.code, &back, verifier).await?;
    let identity = oauth_link::identify(service, &config, &token).await;
    let repos = match (&identity, service) {
        (Ok(identity), Service::Github) => github::repos(&config, &identity.login, Some(&token)).await,
        _ => Ok(vec![]),
    };
    if service == Service::Github {
        github::revoke(&config, &token).await;
    }
    drop(token);
    let identity = identity?;
    let repos = repos?;

    if let Some(other) = db
        .fetch_linked_account_owner(service.id(), &identity.external_id)
        .await?
    {
        if other != user.id {
            return Err(create_error!(LinkedAccountTaken));
        }
    }

    let now = now_ms();
    let previous = db.fetch_linked_account(&user.id, service.id()).await?;
    let same = previous
        .as_ref()
        .filter(|previous| previous.external_id == identity.external_id);

    let mut account = LinkedAccount {
        id: LinkedAccount::key(&user.id, service.id()),
        user_id: user.id.clone(),
        service: service.id().to_string(),
        external_id: identity.external_id.clone(),
        display_name: identity.display_name,
        profile_url: identity.profile_url,
        // a new link, or another account there: preview first
        shown: same.is_some_and(|previous| previous.shown),
        linked_at: same.map_or(now, |previous| previous.linked_at),
        refreshed_at: now,
        // the consent card the app shows before the service is the current
        // one (Battle.net's "Confirm" goes through it again too)
        consent: LINKED_CONSENT_VERSION,
        ..Default::default()
    };

    apply_details(&mut account, identity.details.clone());

    match service {
        Service::Twitch => {
            // the category of the last stream (best effort: the link works without it)
            account.game = twitch::channel_games(&config, &[identity.external_id.clone()])
                .await
                .ok()
                .and_then(|mut games| games.remove(&identity.external_id));
            account.live = Some(false);
            account.live_at = Some(now);
            if let Some(previous) = same {
                account.live = previous.live;
                account.live_title = previous.live_title.clone();
                account.live_at = previous.live_at;
                account.subscriptions = previous.subscriptions.clone();
            } else if let Some(previous) = &previous {
                // another Twitch account before: its channel no longer concerns us
                twitch::unsubscribe(&config, &previous.subscriptions).await;
            }
            if account.subscriptions.is_empty() {
                // live status is a bonus: the link works without it
                match twitch::subscribe(&config, &identity.external_id).await {
                    Ok(ids) => account.subscriptions = ids,
                    Err(_) => log::warn!("Twitch live status could not be set up for a new link"),
                }
                if twitch::subscribe_revocations(&config).await.is_err() {
                    log::warn!("Twitch revocation messages could not be set up; the hourly upkeep tries again");
                }
            }
        }
        Service::Github => {
            let wanted: Vec<u64> = match same {
                Some(previous) => previous.shown_repos.clone(),
                None => repos.iter().take(DEFAULT_SHOWN_REPOS).map(|repo| repo.id).collect(),
            };
            account.shown_repos = wanted
                .into_iter()
                .filter(|id| repos.iter().any(|repo| repo.id == *id))
                .collect();
            account.repos = repos;
        }
        Service::Battlenet => {
            account.expires_at = Some(now + BATTLENET_TTL_MS);
        }
    }

    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account)))
}

/// # Edit Linked Service Account
///
/// Show or hide it on your profile; for GitHub, choose the repositories.
/// `consent` agrees to the consent text in use (its exact version number).
#[openapi(tag = "User Information")]
#[patch("/@me/connections/<service>", data = "<data>")]
pub async fn service_edit(
    db: &State<Database>,
    user: User,
    service: &str,
    data: Json<v0::DataEditServiceAccount>,
) -> Result<Json<v0::ServiceAccount>> {
    no_bots(&user)?;
    let service = service_of(service)?;
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let mut account = db
        .fetch_linked_account(&user.id, service.id())
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
        // Twitch: the upkeep ended the live-status subscriptions while the
        // link waited for this agreement; set them up again (best effort,
        // like a new link)
        if service == Service::Twitch && account.subscriptions.is_empty() {
            let config = service.config().await;
            if usable(service, &config) {
                match twitch::subscribe(&config, &account.external_id).await {
                    Ok(ids) => account.subscriptions = ids,
                    Err(_) => log::warn!("Twitch live status could not be set up again after consent"),
                }
                if twitch::subscribe_revocations(&config).await.is_err() {
                    log::warn!("Twitch revocation messages could not be set up; the hourly upkeep tries again");
                }
            }
        }
    }
    if let Some(wanted) = data.shown_repos {
        if service != Service::Github {
            return Err(create_error!(InvalidOperation));
        }
        let mut chosen: Vec<u64> = vec![];
        for id in wanted {
            if account.repos.iter().any(|repo| repo.id == id) && !chosen.contains(&id) {
                chosen.push(id);
            }
        }
        chosen.truncate(LINKED_MAX_SHOWN_REPOS);
        account.shown_repos = chosen;
    }
    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account)))
}

/// # Refresh Linked Service Account
///
/// GitHub only: read the public repositories again. Once every 10 minutes.
/// (Twitch updates by itself; Battle.net is confirmed by signing in again.)
/// A link agreed under an older consent text must agree to the current one
/// first (`consent` in Edit).
#[openapi(tag = "User Information")]
#[post("/@me/connections/<service>/refresh")]
pub async fn service_refresh(
    db: &State<Database>,
    user: User,
    service: &str,
) -> Result<Json<v0::ServiceAccount>> {
    no_bots(&user)?;
    let service = service_of(service)?;
    if service != Service::Github {
        return Err(create_error!(InvalidOperation));
    }
    let config = enabled_config(service).await?;
    let mut account = db
        .fetch_linked_account(&user.id, service.id())
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    if !consent_current(&account) {
        return Err(create_error!(InvalidOperation));
    }

    let wait = account.refreshed_at + REFRESH_COOLDOWN_MS - now_ms();
    if wait > 0 {
        return Err(create_error!(LinkedRefreshTooSoon {
            retry_after: (wait as u64).div_ceil(1000)
        }));
    }

    // the login can change, and an old one can go to someone else: ask
    // GitHub who this id is now
    let Some(identity) = github::user_of(&config, &account.external_id).await? else {
        db.delete_linked_account(&user.id, service.id()).await?;
        return Err(create_error!(NotFound));
    };
    let login = identity.login.clone();
    account.display_name = identity.display_name;
    account.profile_url = identity.profile_url;
    apply_details(&mut account, identity.details);
    let repos = github::repos(&config, &login, None).await?;
    account
        .shown_repos
        .retain(|id| repos.iter().any(|repo| repo.id == *id));
    account.repos = repos;
    account.refreshed_at = now_ms();
    db.save_linked_account(&account).await?;
    Ok(Json(owner_view(account)))
}

/// # Disconnect a Service
///
/// Removes the link and everything copied from the service, at once.
#[openapi(tag = "User Information")]
#[delete("/@me/connections/<service>")]
pub async fn service_disconnect(
    db: &State<Database>,
    user: User,
    service: &str,
) -> Result<EmptyResponse> {
    no_bots(&user)?;
    let service = service_of(service)?;
    if let Some(account) = db.fetch_linked_account(&user.id, service.id()).await? {
        if service == Service::Twitch && !account.subscriptions.is_empty() {
            twitch::unsubscribe(&service.config().await, &account.subscriptions).await;
        }
        db.delete_linked_account(&user.id, service.id()).await?;
    }
    Ok(EmptyResponse)
}
