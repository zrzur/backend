//! Steam: sign-in through Steam's OpenID 2.0 to learn which Steam account a
//! person owns, then the Steam Web API for what that account shows publicly.
//!
//! Steam gives no token, so none is kept. Only the allowlist below leaves
//! this module: name, profile link, game count, hours and the most played
//! games with their achievement counts. Real name, country, avatar, friends,
//! online state and every time stamp are dropped here, on arrival.
//!
//! Errors never carry a Steam address: the Web API key is part of the query.

use std::{collections::BTreeMap, sync::OnceLock, time::Duration};

use revolt_config::ApiConnectionSteam;
use revolt_database::{
    LinkedAchievement, LinkedGame, LINKED_LATEST_ACHIEVEMENTS, LINKED_TOP_GAMES,
};
use revolt_result::{create_error, Result};
use serde::Deserialize;

/// The OpenID 2.0 namespace
pub const OPENID_NS: &str = "http://specs.openid.net/auth/2.0";
const IDENTIFIER_SELECT: &str = "http://specs.openid.net/auth/2.0/identifier_select";
/// Longest game name kept
const MAX_NAME: usize = 128;
/// Most `openid.*` fields accepted back from Steam
const MAX_PARAMS: usize = 24;

/// What Steam shows publicly about one account, after the allowlist
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SteamSnapshot {
    pub display_name: String,
    pub profile_url: String,
    pub private: bool,
    pub game_count: Option<u32>,
    pub total_hours: Option<u32>,
    pub games: Vec<LinkedGame>,
    /// Newest first, at most [LINKED_LATEST_ACHIEVEMENTS]
    pub achievements: Vec<LinkedAchievement>,
}

/// A Steam image file name: 40 hexadecimal characters, nothing else
pub fn image_hash(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

/// The file name from an image address Steam gives (`…/<hash>.jpg`)
pub fn image_hash_of_url(url: &str) -> Option<String> {
    let name = url.rsplit('/').next()?;
    image_hash(name.strip_suffix(".jpg").unwrap_or(name))
}

/// A game's or achievement's icon on Steam's image server
pub fn icon_url(appid: u32, hash: &str) -> String {
    format!("https://media.steampowered.com/steamcommunity/public/images/apps/{appid}/{hash}.jpg")
}

/// A game's wide header picture on Steam's image server
pub fn header_url(appid: u32) -> String {
    format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{appid}/header.jpg")
}

/// A game's page in the Steam store
pub fn store_url(appid: u32) -> String {
    format!("https://store.steampowered.com/app/{appid}/")
}

/// The UTC day of a Unix time, `YYYY-MM-DD`. The time of day is dropped.
pub fn day_of(unix_seconds: i64) -> String {
    // days since 1970-01-01 to a civil date (H. Hinnant's algorithm)
    let z = unix_seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Where Steam sends the person back to, for this web app origin
pub fn return_to(origin: &str, state: &str) -> String {
    format!(
        "{}/connections/steam/return?state={}",
        origin.trim_end_matches('/'),
        state
    )
}

/// Steam's sign-in page for this return address
pub fn login_url(endpoint: &str, origin: &str, return_to: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("openid.ns", OPENID_NS)
        .append_pair("openid.mode", "checkid_setup")
        .append_pair("openid.return_to", return_to)
        .append_pair("openid.realm", origin.trim_end_matches('/'))
        .append_pair("openid.identity", IDENTIFIER_SELECT)
        .append_pair("openid.claimed_id", IDENTIFIER_SELECT)
        .finish();
    format!("{endpoint}?{query}")
}

/// The 64-bit SteamID in a claimed id, if it is one of Steam's
pub fn steam_id_of(claimed_id: &str) -> Option<String> {
    let rest = claimed_id
        .strip_prefix("https://steamcommunity.com/openid/id/")
        .or_else(|| claimed_id.strip_prefix("http://steamcommunity.com/openid/id/"))?;
    // individual accounts are 7656119 followed by ten digits
    (rest.len() == 17 && rest.starts_with("7656119") && rest.bytes().all(|b| b.is_ascii_digit()))
        .then(|| rest.to_string())
}

/// Checks on what Steam sent back that need no call to Steam. Returns the
/// SteamID. The signature itself is checked by Steam in [`confirm`].
pub fn check_fields(
    params: &BTreeMap<String, String>,
    endpoint: &str,
    expected_return_to: &str,
) -> Result<String> {
    let field = |name: &str| params.get(name).map(String::as_str).unwrap_or_default();
    let refused = || create_error!(LinkedSignInFailed);

    if params.len() > MAX_PARAMS
        || params.keys().any(|key| !key.starts_with("openid."))
        || params.values().any(|value| value.len() > 2048)
    {
        return Err(refused());
    }
    if field("openid.ns") != OPENID_NS
        || field("openid.mode") != "id_res"
        || field("openid.op_endpoint") != endpoint
        || field("openid.return_to") != expected_return_to
        || field("openid.claimed_id") != field("openid.identity")
        || field("openid.response_nonce").is_empty()
        || field("openid.sig").is_empty()
    {
        return Err(refused());
    }
    // what we rely on must be covered by Steam's signature
    let signed: Vec<&str> = field("openid.signed").split(',').collect();
    for needed in ["op_endpoint", "claimed_id", "identity", "return_to", "response_nonce"] {
        if !signed.contains(&needed) {
            return Err(refused());
        }
    }
    steam_id_of(field("openid.claimed_id")).ok_or_else(refused)
}

/// The body for Steam's `check_authentication`: the same fields, other mode
pub fn verification_body(params: &BTreeMap<String, String>) -> String {
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in params {
        if key == "openid.mode" {
            body.append_pair(key, "check_authentication");
        } else {
            body.append_pair(key, value);
        }
    }
    body.finish()
}

/// Whether Steam's answer to `check_authentication` says the sign-in is real
pub fn verification_passed(answer: &str) -> bool {
    answer.lines().any(|line| line.trim() == "is_valid:true")
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("Zrzur-Linked-Accounts/1")
            .build()
            .expect("reqwest Client")
    })
}

/// Ask Steam whether the fields it sent back are genuine. A nonce works once:
/// Steam refuses a second check of the same answer.
pub async fn confirm(config: &ApiConnectionSteam, params: &BTreeMap<String, String>) -> Result<()> {
    #[cfg(test)]
    {
        let _ = config;
        return if params.get("openid.sig").map(String::as_str) == Some(test_steam::BAD_SIGNATURE) {
            Err(create_error!(LinkedSignInFailed))
        } else {
            Ok(())
        };
    }

    #[allow(unreachable_code)]
    {
        let answer = client()
            .post(&config.openid_endpoint)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(verification_body(params))
            .send()
            .await
            .map_err(|_| create_error!(LinkedServiceFailed))?;
        if !answer.status().is_success() {
            return Err(create_error!(LinkedServiceFailed));
        }
        let text = answer.text().await.map_err(|_| create_error!(LinkedServiceFailed))?;
        if verification_passed(&text) {
            Ok(())
        } else {
            Err(create_error!(LinkedSignInFailed))
        }
    }
}

// What the Web API answers, reduced to the fields read. Anything else in the
// answer is never deserialised.

#[derive(Deserialize, Default)]
pub struct SummaryAnswer {
    #[serde(default)]
    pub response: SummaryList,
}

#[derive(Deserialize, Default)]
pub struct SummaryList {
    #[serde(default)]
    pub players: Vec<Summary>,
}

#[derive(Deserialize, Default)]
pub struct Summary {
    #[serde(default)]
    pub personaname: String,
    #[serde(default)]
    pub profileurl: String,
    /// 3 is public; anything else means we may not see the profile
    #[serde(default)]
    pub communityvisibilitystate: u8,
}

#[derive(Deserialize, Default)]
pub struct OwnedAnswer {
    #[serde(default)]
    pub response: OwnedList,
}

#[derive(Deserialize, Default)]
pub struct OwnedList {
    /// Absent when the game details are private
    pub game_count: Option<u32>,
    #[serde(default)]
    pub games: Vec<OwnedGame>,
}

#[derive(Deserialize, Default)]
pub struct OwnedGame {
    pub appid: u32,
    #[serde(default)]
    pub name: String,
    /// Minutes
    #[serde(default)]
    pub playtime_forever: u64,
    /// Icon file name
    #[serde(default)]
    pub img_icon_url: String,
}

#[derive(Deserialize, Default)]
pub struct AchievementsAnswer {
    #[serde(default)]
    pub playerstats: PlayerStats,
}

#[derive(Deserialize, Default)]
pub struct PlayerStats {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub achievements: Vec<Achievement>,
}

#[derive(Deserialize, Default)]
pub struct Achievement {
    #[serde(default)]
    pub apiname: String,
    #[serde(default)]
    pub achieved: u8,
    /// Unix time; only the day is kept
    #[serde(default)]
    pub unlocktime: i64,
    /// Present when asked with `l`
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize, Default)]
pub struct SchemaAnswer {
    #[serde(default)]
    pub game: SchemaGame,
}

#[derive(Deserialize, Default)]
pub struct SchemaGame {
    #[serde(default, rename = "availableGameStats")]
    pub stats: SchemaStats,
}

#[derive(Deserialize, Default)]
pub struct SchemaStats {
    #[serde(default)]
    pub achievements: Vec<SchemaAchievement>,
}

#[derive(Deserialize, Default)]
pub struct SchemaAchievement {
    /// The `apiname` of the player's list
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// Address of the unlocked icon
    #[serde(default)]
    pub icon: String,
}

/// Longest achievement description kept
const MAX_DESCRIPTION: usize = 200;

/// The unlocked achievements of one game, with names and icons from its
/// schema when there is one. Times become days.
pub fn unlocked_of(
    appid: u32,
    answer: &AchievementsAnswer,
    schema: Option<&SchemaAnswer>,
) -> Vec<(i64, LinkedAchievement)> {
    let find = |apiname: &str| {
        schema.and_then(|schema| {
            schema
                .game
                .stats
                .achievements
                .iter()
                .find(|entry| entry.name == apiname)
        })
    };
    answer
        .playerstats
        .achievements
        .iter()
        .filter(|a| a.achieved == 1 && a.unlocktime > 0)
        .filter_map(|a| {
            let entry = find(&a.apiname);
            let name = clean_name(
                entry
                    .map(|e| e.display_name.as_str())
                    .filter(|n| !n.trim().is_empty())
                    .or(Some(a.name.as_str()).filter(|n| !n.trim().is_empty()))
                    .unwrap_or(&a.apiname),
            );
            if name.is_empty() {
                return None;
            }
            let description: String = entry
                .map(|e| e.description.as_str())
                .filter(|d| !d.trim().is_empty())
                .unwrap_or(&a.description)
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_DESCRIPTION)
                .collect::<String>()
                .trim()
                .to_string();
            Some((
                a.unlocktime,
                LinkedAchievement {
                    appid,
                    name,
                    description,
                    icon: entry.and_then(|e| image_hash_of_url(&e.icon)),
                    unlocked_on: day_of(a.unlocktime),
                },
            ))
        })
        .collect()
}

/// The newest achievements across games, at most [LINKED_LATEST_ACHIEVEMENTS]
pub fn latest(mut all: Vec<(i64, LinkedAchievement)>) -> Vec<LinkedAchievement> {
    all.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.appid.cmp(&b.1.appid))
            .then(a.1.name.cmp(&b.1.name))
    });
    all.truncate(LINKED_LATEST_ACHIEVEMENTS);
    all.into_iter().map(|(_, achievement)| achievement).collect()
}

/// A game name as shown: one line, at most [MAX_NAME] characters
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Only Steam's own profile addresses are kept as links
pub fn clean_profile_url(url: &str) -> String {
    if url.starts_with("https://steamcommunity.com/") && !url.contains(char::is_whitespace) {
        url.to_string()
    } else {
        String::new()
    }
}

/// Apply the allowlist to the two answers. `games` comes back without
/// achievement counts; those are added for the top games only.
pub fn snapshot_of(summary: Option<Summary>, owned: OwnedAnswer) -> SteamSnapshot {
    let summary = summary.unwrap_or_default();
    let visible = summary.communityvisibilitystate == 3;
    let private = !visible || owned.response.game_count.is_none();
    if private {
        return SteamSnapshot {
            display_name: clean_name(&summary.personaname),
            profile_url: clean_profile_url(&summary.profileurl),
            private: true,
            ..Default::default()
        };
    }

    let mut games: Vec<OwnedGame> = owned.response.games;
    let total_minutes: u64 = games.iter().map(|game| game.playtime_forever).sum();
    games.retain(|game| game.playtime_forever > 0 && !clean_name(&game.name).is_empty());
    games.sort_by(|a, b| b.playtime_forever.cmp(&a.playtime_forever).then(a.appid.cmp(&b.appid)));
    games.truncate(LINKED_TOP_GAMES);

    SteamSnapshot {
        display_name: clean_name(&summary.personaname),
        profile_url: clean_profile_url(&summary.profileurl),
        private: false,
        game_count: owned.response.game_count,
        total_hours: Some((total_minutes / 60).min(u32::MAX as u64) as u32),
        games: games
            .into_iter()
            .map(|game| LinkedGame {
                appid: game.appid,
                name: clean_name(&game.name),
                hours: (game.playtime_forever / 60).min(u32::MAX as u64) as u32,
                achievements_unlocked: None,
                achievements_total: None,
                icon: image_hash(&game.img_icon_url),
            })
            .collect(),
        achievements: vec![],
    }
}

/// Unlocked and total achievements, when Steam shows any for this game
pub fn achievement_counts(answer: AchievementsAnswer) -> Option<(u32, u32)> {
    let stats = answer.playerstats;
    if !stats.success || stats.achievements.is_empty() {
        return None;
    }
    let total = stats.achievements.len() as u32;
    let unlocked = stats.achievements.iter().filter(|a| a.achieved == 1).count() as u32;
    Some((unlocked, total))
}

async fn get_json<T: for<'de> Deserialize<'de> + Default>(
    config: &ApiConnectionSteam,
    path: &str,
    query: &[(&str, &str)],
) -> Result<Option<T>> {
    let url = format!("{}/{path}", config.api_base.trim_end_matches('/'));
    let answer = client()
        .get(url)
        .query(&[("key", config.web_api_key.as_str()), ("format", "json")])
        .query(query)
        .send()
        .await
        .map_err(|_| create_error!(LinkedServiceFailed))?;
    match answer.status().as_u16() {
        200 => answer
            .json::<T>()
            .await
            .map(Some)
            .map_err(|_| create_error!(LinkedServiceFailed)),
        // private game details or a game without stats
        400 | 401 | 403 => Ok(None),
        _ => Err(create_error!(LinkedServiceFailed)),
    }
}

/// Read what Steam shows publicly about this account, on its owner's request
pub async fn fetch(config: &ApiConnectionSteam, steam_id: &str) -> Result<SteamSnapshot> {
    #[cfg(test)]
    {
        let _ = config;
        return Ok(test_steam::snapshot(steam_id));
    }

    #[allow(unreachable_code)]
    {
        let summary: Option<SummaryAnswer> = get_json(
            config,
            "ISteamUser/GetPlayerSummaries/v2/",
            &[("steamids", steam_id)],
        )
        .await?;
        let summary = summary.and_then(|answer| answer.response.players.into_iter().next());
        if summary.is_none() {
            return Err(create_error!(LinkedServiceFailed));
        }
        let owned: OwnedAnswer = get_json(
            config,
            "IPlayerService/GetOwnedGames/v1/",
            &[
                ("steamid", steam_id),
                ("include_appinfo", "true"),
                ("include_played_free_games", "true"),
            ],
        )
        .await?
        .unwrap_or_default();

        let mut snapshot = snapshot_of(summary, owned);
        let mut unlocked = vec![];
        for game in snapshot.games.iter_mut() {
            let appid = game.appid.to_string();
            // a game without achievements answers 400: that is no failure
            let answer: Option<AchievementsAnswer> = get_json(
                config,
                "ISteamUserStats/GetPlayerAchievements/v1/",
                &[
                    ("steamid", steam_id),
                    ("appid", appid.as_str()),
                    ("l", "english"),
                ],
            )
            .await
            .unwrap_or(None);
            let Some(answer) = answer else { continue };
            // names and icons only for a game where something was unlocked
            let schema: Option<SchemaAnswer> =
                if answer.playerstats.achievements.iter().any(|a| a.achieved == 1) {
                    get_json(
                        config,
                        "ISteamUserStats/GetSchemaForGame/v2/",
                        &[("appid", appid.as_str()), ("l", "english")],
                    )
                    .await
                    .unwrap_or(None)
                } else {
                    None
                };
            unlocked.extend(unlocked_of(game.appid, &answer, schema.as_ref()));
            if let Some((count, total)) = achievement_counts(answer) {
                game.achievements_unlocked = Some(count);
                game.achievements_total = Some(total);
            }
        }
        snapshot.achievements = latest(unlocked);
        Ok(snapshot)
    }
}

/// Steam as the tests see it: no network, fixed accounts
#[cfg(test)]
pub mod test_steam {
    use std::collections::BTreeMap;

    use super::*;

    /// A public account with games
    pub const PUBLIC: &str = "76561197960287930";
    /// A second public account
    pub const OTHER: &str = "76561197960287932";
    /// An account whose game details are private
    pub const PRIVATE: &str = "76561197960287931";
    /// A signature Steam refuses
    pub const BAD_SIGNATURE: &str = "bad-signature";

    pub fn snapshot(steam_id: &str) -> SteamSnapshot {
        let summary = Summary {
            personaname: format!("Player {}", &steam_id[13..]),
            profileurl: format!("https://steamcommunity.com/profiles/{steam_id}/"),
            communityvisibilitystate: if steam_id == PRIVATE { 1 } else { 3 },
        };
        let owned = OwnedAnswer {
            response: OwnedList {
                game_count: Some(3),
                games: vec![
                    OwnedGame { appid: 620, name: "Portal 2".into(), playtime_forever: 600, ..Default::default() },
                    OwnedGame { appid: 570, name: "Dota 2".into(), playtime_forever: 6000, img_icon_url: "0bbb630d63262dd66d2fdd0f7d37e8661a410075".into() },
                    OwnedGame { appid: 440, name: "Unplayed".into(), playtime_forever: 0, ..Default::default() },
                ],
            },
        };
        let mut snapshot = snapshot_of(Some(summary), owned);
        if let Some(first) = snapshot.games.first_mut() {
            first.achievements_unlocked = Some(12);
            first.achievements_total = Some(30);
        }
        let icon = |n: u8| format!("{:040x}", n);
        snapshot.achievements = vec![
            LinkedAchievement {
                appid: 570,
                name: "First blood".into(),
                description: "Win your first match".into(),
                icon: Some(icon(1)),
                unlocked_on: "2026-11-12".into(),
            },
            LinkedAchievement {
                appid: 620,
                name: "Wake up call".into(),
                description: "Survive the manual override".into(),
                icon: Some(icon(2)),
                unlocked_on: "2026-11-09".into(),
            },
        ];
        if steam_id == OTHER {
            // text the platform word list catches
            snapshot.achievements.push(LinkedAchievement {
                appid: 570,
                name: "kys".into(),
                description: String::new(),
                icon: None,
                unlocked_on: "2026-11-01".into(),
            });
        }
        snapshot
    }

    /// What Steam sends back after a sign-in as `steam_id`
    pub fn assertion(steam_id: &str, return_to: &str) -> BTreeMap<String, String> {
        let claimed = format!("https://steamcommunity.com/openid/id/{steam_id}");
        [
            ("openid.ns", OPENID_NS.to_string()),
            ("openid.mode", "id_res".into()),
            ("openid.op_endpoint", "https://steamcommunity.com/openid/login".into()),
            ("openid.claimed_id", claimed.clone()),
            ("openid.identity", claimed),
            ("openid.return_to", return_to.to_string()),
            ("openid.response_nonce", "2026-11-14T10:00:00Zabc".into()),
            ("openid.assoc_handle", "1234567890".into()),
            (
                "openid.signed",
                "signed,op_endpoint,claimed_id,identity,return_to,response_nonce,assoc_handle"
                    .into(),
            ),
            ("openid.sig", "c2lnbmF0dXJl".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_steam::*;
    use super::*;

    const ENDPOINT: &str = "https://steamcommunity.com/openid/login";

    #[test]
    fn login_url_asks_steam_to_pick_the_account() {
        let back = return_to("https://app.zrzur.com/", "abc");
        assert_eq!(back, "https://app.zrzur.com/connections/steam/return?state=abc");
        let url = login_url(ENDPOINT, "https://app.zrzur.com", &back);
        let parsed = url::Url::parse(&url).unwrap();
        let query: BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.host_str(), Some("steamcommunity.com"));
        assert_eq!(query["openid.mode"], "checkid_setup");
        assert_eq!(query["openid.return_to"], back);
        assert_eq!(query["openid.realm"], "https://app.zrzur.com");
        assert_eq!(query["openid.identity"], IDENTIFIER_SELECT);
    }

    #[test]
    fn only_steam_ids_of_individual_accounts() {
        assert_eq!(
            steam_id_of("https://steamcommunity.com/openid/id/76561197960287930").as_deref(),
            Some(PUBLIC)
        );
        assert_eq!(
            steam_id_of("http://steamcommunity.com/openid/id/76561197960287930").as_deref(),
            Some(PUBLIC)
        );
        for bad in [
            "https://steamcommunity.com.evil.example/openid/id/76561197960287930",
            "https://evil.example/openid/id/76561197960287930",
            "https://steamcommunity.com/openid/id/7656119796028793",
            "https://steamcommunity.com/openid/id/765611979602879301",
            "https://steamcommunity.com/openid/id/12345678901234567",
            "https://steamcommunity.com/openid/id/7656119796028793x",
            "https://steamcommunity.com/openid/id/76561197960287930/",
            "",
        ] {
            assert_eq!(steam_id_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn fields_steam_signed_are_checked() {
        let back = return_to("http://localhost:5173", "state-0123456789abcdef");
        let good = assertion(PUBLIC, &back);
        assert_eq!(check_fields(&good, ENDPOINT, &back).unwrap(), PUBLIC);

        let mut cases: Vec<BTreeMap<String, String>> = vec![];
        for (key, value) in [
            ("openid.mode", "cancel"),
            ("openid.ns", "http://openid.net/signon/1.1"),
            ("openid.op_endpoint", "https://evil.example/openid/login"),
            ("openid.return_to", "https://evil.example/connections/steam/return?state=x"),
            ("openid.identity", "https://steamcommunity.com/openid/id/76561197960287932"),
            ("openid.signed", "signed,op_endpoint,identity,return_to,response_nonce"),
            ("openid.response_nonce", ""),
            ("openid.sig", ""),
        ] {
            let mut params = good.clone();
            params.insert(key.into(), value.into());
            cases.push(params);
        }
        let mut extra = good.clone();
        extra.insert("redirect".into(), "x".into());
        cases.push(extra);
        let mut missing = good.clone();
        missing.remove("openid.claimed_id");
        cases.push(missing);
        for params in cases {
            assert!(check_fields(&params, ENDPOINT, &back).is_err(), "{params:?}");
        }
    }

    #[test]
    fn verification_asks_steam_with_the_same_fields() {
        let back = return_to("http://localhost:5173", "s");
        let body = verification_body(&assertion(PUBLIC, &back));
        let fields: BTreeMap<_, _> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
        assert_eq!(fields["openid.mode"], "check_authentication");
        assert_eq!(fields["openid.return_to"], back);
        assert!(verification_passed("ns:http://specs.openid.net/auth/2.0\nis_valid:true\n"));
        assert!(!verification_passed("ns:http://specs.openid.net/auth/2.0\nis_valid:false\n"));
        assert!(!verification_passed("is_valid:truex"));
    }

    #[test]
    fn allowlist_keeps_top_games_by_hours() {
        let summary: SummaryAnswer = serde_json::from_str(
            r#"{"response":{"players":[{"steamid":"76561197960287930","personaname":"Gabe\u0007",
            "realname":"Real Name","loccountrycode":"US","lastlogoff":1,"avatar":"a.jpg",
            "profileurl":"https://steamcommunity.com/id/gabe/","communityvisibilitystate":3}]}}"#,
        )
        .unwrap();
        let owned: OwnedAnswer = serde_json::from_str(
            r#"{"response":{"game_count":12,"games":[
            {"appid":1,"name":"A","playtime_forever":120,"rtime_last_played":1,"playtime_2weeks":5},
            {"appid":2,"name":"B","playtime_forever":59},
            {"appid":3,"name":"C","playtime_forever":0},
            {"appid":4,"name":"D","playtime_forever":600}]}}"#,
        )
        .unwrap();
        let snapshot = snapshot_of(summary.response.players.into_iter().next(), owned);
        assert!(!snapshot.private);
        assert_eq!(snapshot.display_name, "Gabe");
        assert_eq!(snapshot.game_count, Some(12));
        assert_eq!(snapshot.total_hours, Some(12));
        let ids: Vec<u32> = snapshot.games.iter().map(|g| g.appid).collect();
        assert_eq!(ids, vec![4, 1, 2]);
        assert_eq!(snapshot.games[0].hours, 10);
        assert_eq!(snapshot.games[2].hours, 0);
    }

    #[test]
    fn at_most_ten_games() {
        let games = (1..=25)
            .map(|appid| OwnedGame { appid, name: format!("G{appid}"), playtime_forever: 60 * appid as u64, ..Default::default() })
            .collect();
        let snapshot = snapshot_of(
            Some(Summary { communityvisibilitystate: 3, ..Default::default() }),
            OwnedAnswer { response: OwnedList { game_count: Some(25), games } },
        );
        assert_eq!(snapshot.games.len(), LINKED_TOP_GAMES);
        assert_eq!(snapshot.games[0].appid, 25);
    }

    #[test]
    fn private_profile_or_game_details_keep_nothing() {
        let hidden_profile = snapshot_of(
            Some(Summary { personaname: "P".into(), communityvisibilitystate: 1, ..Default::default() }),
            OwnedAnswer::default(),
        );
        assert!(hidden_profile.private);
        assert!(hidden_profile.games.is_empty());
        assert_eq!(hidden_profile.game_count, None);

        // public profile, private game details: Steam answers `{"response":{}}`
        let owned: OwnedAnswer = serde_json::from_str(r#"{"response":{}}"#).unwrap();
        let hidden_games =
            snapshot_of(Some(Summary { communityvisibilitystate: 3, ..Default::default() }), owned);
        assert!(hidden_games.private);
        assert_eq!(hidden_games.total_hours, None);
    }

    #[test]
    fn only_steam_profile_links() {
        assert_eq!(clean_profile_url("https://steamcommunity.com/id/x/"), "https://steamcommunity.com/id/x/");
        assert_eq!(clean_profile_url("https://evil.example/"), "");
        assert_eq!(clean_profile_url("javascript:alert(1)"), "");
    }

    #[test]
    fn days_never_carry_a_time() {
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(86_399), "1970-01-01");
        assert_eq!(day_of(1_763_078_399), "2025-11-13");
        assert_eq!(day_of(1_763_078_400), "2025-11-14");
        assert_eq!(day_of(951_782_400), "2000-02-29");
        assert_eq!(day_of(-1), "1969-12-31");
    }

    #[test]
    fn only_plain_image_names() {
        let hash = "0bbb630d63262dd66d2fdd0f7d37e8661a410075";
        assert_eq!(image_hash(hash).as_deref(), Some(hash));
        assert_eq!(image_hash(&hash.to_uppercase()).as_deref(), Some(hash));
        assert_eq!(
            image_hash_of_url(&format!("https://steamcdn-a.akamaihd.net/steamcommunity/public/images/apps/570/{hash}.jpg")).as_deref(),
            Some(hash)
        );
        for bad in ["", "../etc", "0bbb630d63262dd66d2fdd0f7d37e8661a41007", "0bbb630d63262dd66d2fdd0f7d37e8661a41007z", "https://evil.example/x.jpg"] {
            assert_eq!(image_hash_of_url(bad), None, "{bad}");
        }
        assert_eq!(icon_url(570, hash), format!("https://media.steampowered.com/steamcommunity/public/images/apps/570/{hash}.jpg"));
        assert_eq!(header_url(570), "https://cdn.cloudflare.steamstatic.com/steam/apps/570/header.jpg");
        assert_eq!(store_url(570), "https://store.steampowered.com/app/570/");
    }

    #[test]
    fn unlocked_achievements_take_schema_names_and_days() {
        let hash = "0bbb630d63262dd66d2fdd0f7d37e8661a410075";
        let player: AchievementsAnswer = serde_json::from_str(
            r#"{"playerstats":{"success":true,"achievements":[
            {"apiname":"WIN_1","achieved":1,"unlocktime":1763078500,"name":"Win one","description":"player text"},
            {"apiname":"WIN_2","achieved":0,"unlocktime":0},
            {"apiname":"OLD","achieved":1,"unlocktime":0}]}}"#,
        )
        .unwrap();
        let schema: SchemaAnswer = serde_json::from_str(&format!(
            r#"{{"game":{{"availableGameStats":{{"achievements":[
            {{"name":"WIN_1","displayName":"Winner","description":"Win a match","icon":"https://steamcdn-a.akamaihd.net/steamcommunity/public/images/apps/570/{hash}.jpg","icongray":"x"}}]}}}}}}"#
        ))
        .unwrap();
        let got = unlocked_of(570, &player, Some(&schema));
        assert_eq!(got.len(), 1, "locked ones and ones without a time are left out");
        let (time, a) = &got[0];
        assert_eq!(*time, 1_763_078_500);
        assert_eq!(a.name, "Winner");
        assert_eq!(a.description, "Win a match");
        assert_eq!(a.icon.as_deref(), Some(hash));
        assert_eq!(a.unlocked_on, "2025-11-14");

        // without a schema, the player's own text and no icon
        let plain = unlocked_of(570, &player, None);
        assert_eq!(plain[0].1.name, "Win one");
        assert_eq!(plain[0].1.icon, None);
    }

    #[test]
    fn latest_keeps_the_newest_sixty() {
        let all = (0..80)
            .map(|n| {
                (
                    n as i64 * 1000,
                    LinkedAchievement {
                        appid: 1,
                        name: format!("A{n}"),
                        description: String::new(),
                        icon: None,
                        unlocked_on: day_of(n as i64 * 1000),
                    },
                )
            })
            .collect();
        let kept = latest(all);
        assert_eq!(kept.len(), LINKED_LATEST_ACHIEVEMENTS);
        assert_eq!(kept[0].name, "A79");
        assert_eq!(kept.last().unwrap().name, "A20");
    }

    #[test]
    fn achievement_counts_from_steam() {
        let answer: AchievementsAnswer = serde_json::from_str(
            r#"{"playerstats":{"success":true,"achievements":[
            {"apiname":"a","achieved":1,"unlocktime":1},{"apiname":"b","achieved":0}]}}"#,
        )
        .unwrap();
        assert_eq!(achievement_counts(answer), Some((1, 2)));
        let none: AchievementsAnswer =
            serde_json::from_str(r#"{"playerstats":{"error":"Requested app has no stats","success":false}}"#)
                .unwrap();
        assert_eq!(achievement_counts(none), None);
    }
}
