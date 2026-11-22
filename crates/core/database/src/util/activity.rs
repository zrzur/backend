//! Rich presence: what someone is doing right now.
//!
//! - Never stored in the database. A Redis hash per user
//!   (`revolt_presence::activity_key`) holds one activity per source; the
//!   last session going removes it, and so does turning sharing off.
//! - Set by the person's own session (`SetActivity` on the gateway, for the
//!   `Rpc` and `Desktop` sources) or by delta's Steam reader (`Steam`).
//! - Seen by the people who may see the person online: the same topics as
//!   presence (their servers and their own topic), never while they are
//!   invisible. A bot sees it only with the Presence intent.
//! - Every text is checked for length and run through the platform word
//!   list; pictures only ever point at Zrzur's file server or proxy.

use std::collections::HashMap;

use iso8601_timestamp::{Duration, Timestamp};
use redis_kiss::{get_connection, redis, AsyncCommands};
use revolt_models::v0::{
    self, ActivityButton, ActivitySource, DataActivity, PartialUser, UserActivity,
};
use revolt_result::{create_error, Result};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::{events::client::EventV1, Database, User};

/// Longest name
pub const NAME_MAX: usize = 128;
/// Longest details, state and picture text
pub const TEXT_MAX: usize = 128;
/// Longest button label
pub const LABEL_MAX: usize = 32;
/// Longest address (button or picture)
pub const URL_MAX: usize = 512;
/// Most buttons
pub const BUTTONS_MAX: usize = 2;
/// Longest application id (a ULID is 26)
pub const APPLICATION_ID_MAX: usize = 32;
/// The hash goes on its own after a day even if nothing removes it
const KEEP_SECONDS: usize = 24 * 60 * 60;
/// A start time may be this far ahead of the server's clock
const START_AHEAD_SECONDS: i64 = 60;
/// An end time may be this far ahead
const END_AHEAD_DAYS: i64 = 7;

/// What is kept for one source
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Stored {
    activity: UserActivity,
    /// Login session that set it; the activity goes when that session's
    /// socket goes, even while the person stays online elsewhere
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
}

fn field(source: ActivitySource) -> &'static str {
    match source {
        ActivitySource::Rpc => "Rpc",
        ActivitySource::Desktop => "Desktop",
        ActivitySource::Steam => "Steam",
    }
}

/// Whether the feature is on
pub async fn enabled() -> bool {
    revolt_config::config().await.features.rich_presence.enabled
}

/// Zrzur's own servers a picture may point at
#[derive(Debug, Clone)]
pub struct Hosts {
    /// File server, for example `https://cdn.zrzur.com`
    pub autumn: String,
    /// Proxy as browsers reach it, for example `https://proxy.zrzur.com`
    pub january: String,
}

impl Hosts {
    pub async fn from_config() -> Hosts {
        let config = revolt_config::config().await;
        let january = if config.hosts.january_public.is_empty() {
            config.hosts.january.clone()
        } else {
            config.hosts.january_public.clone()
        };
        Hosts {
            autumn: config.hosts.autumn.trim_end_matches('/').to_string(),
            january: january.trim_end_matches('/').to_string(),
        }
    }
}

fn refuse(error: impl Into<String>) -> revolt_result::Error {
    create_error!(FailedValidation {
        error: error.into(),
    })
}

/// A text as shown: trimmed, one line, no control characters
fn text(value: Option<String>, name: &str, max: usize) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max {
        return Err(refuse(format!("activity.{name} is longer than {max} characters")));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(refuse(format!("activity.{name} must be one line of text")));
    }
    Ok(Some(value.to_string()))
}

/// An `https://` address with a host, nothing hidden in it
fn https_address(value: &str, name: &str) -> Result<()> {
    if value.chars().count() > URL_MAX {
        return Err(refuse(format!("activity.{name} is longer than {URL_MAX} characters")));
    }
    let Some(rest) = value.strip_prefix("https://") else {
        return Err(refuse(format!("activity.{name} must start with https://")));
    };
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(refuse(format!("activity.{name} is not an address")));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(refuse(format!("activity.{name} is not an address")));
    }
    Ok(())
}

/// Percent-encode a value for a query string
pub fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A picture stays on Zrzur's servers: an address on the file server or
/// the proxy is kept, any other `https://` picture is shown through the proxy
fn picture(value: Option<String>, name: &str, hosts: &Hosts) -> Result<Option<String>> {
    let Some(value) = value.map(|value| value.trim().to_string()) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > URL_MAX {
        return Err(refuse(format!("activity.{name} is longer than {URL_MAX} characters")));
    }
    for own in [&hosts.autumn, &hosts.january] {
        if !own.is_empty()
            && value.starts_with(&format!("{own}/"))
            && !value.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Ok(Some(value));
        }
    }
    https_address(&value, name)?;
    if hosts.january.is_empty() {
        return Err(refuse(format!("activity.{name} cannot be shown on this server")));
    }
    Ok(Some(format!(
        "{}/proxy?url={}",
        hosts.january,
        encode_query(&value)
    )))
}

/// Check an activity as sent and turn it into what others see. The word
/// list is not part of this (see `check_words`).
pub fn check(
    input: DataActivity,
    source: ActivitySource,
    now: Timestamp,
    hosts: &Hosts,
) -> Result<UserActivity> {
    let name = text(Some(input.name), "name", NAME_MAX)?
        .ok_or_else(|| refuse("activity.name is required"))?;

    let started_at = input.started_at;
    if let Some(start) = started_at {
        if start > now + Duration::seconds(START_AHEAD_SECONDS) {
            return Err(refuse("activity.started_at is in the future"));
        }
    }
    let ends_at = input.ends_at;
    if let Some(end) = ends_at {
        if end > now + Duration::days(END_AHEAD_DAYS) {
            return Err(refuse("activity.ends_at is more than 7 days ahead"));
        }
        if started_at.is_some_and(|start| end <= start) {
            return Err(refuse("activity.ends_at must come after started_at"));
        }
    }

    let application_id = match input.application_id.map(|id| id.trim().to_string()) {
        Some(id) if id.is_empty() => None,
        Some(id) => {
            if id.len() > APPLICATION_ID_MAX || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(refuse("activity.application_id is not an id"));
            }
            Some(id)
        }
        None => None,
    };

    if input.buttons.len() > BUTTONS_MAX {
        return Err(refuse(format!("activity.buttons holds at most {BUTTONS_MAX}")));
    }
    let mut buttons = Vec::with_capacity(input.buttons.len());
    for button in input.buttons {
        let label = text(Some(button.label), "buttons.label", LABEL_MAX)?
            .ok_or_else(|| refuse("activity.buttons.label is required"))?;
        let url = button.url.trim().to_string();
        https_address(&url, "buttons.url")?;
        buttons.push(ActivityButton { label, url });
    }

    Ok(UserActivity {
        source,
        kind: input.kind,
        name,
        details: text(input.details, "details", TEXT_MAX)?,
        state: text(input.state, "state", TEXT_MAX)?,
        started_at,
        ends_at,
        application_id,
        large_image: picture(input.large_image, "large_image", hosts)?,
        large_text: text(input.large_text, "large_text", TEXT_MAX)?,
        small_image: picture(input.small_image, "small_image", hosts)?,
        small_text: text(input.small_text, "small_text", TEXT_MAX)?,
        buttons,
    })
}

/// Every text others would read, one per line, for the word list
pub fn texts_of(activity: &UserActivity) -> String {
    let mut all = vec![activity.name.as_str()];
    for value in [
        &activity.details,
        &activity.state,
        &activity.large_text,
        &activity.small_text,
    ]
    .into_iter()
    .flatten()
    {
        all.push(value.as_str());
    }
    for button in &activity.buttons {
        all.push(button.label.as_str());
    }
    all.join("\n")
}

/// Refuse an activity the platform word list matches
pub async fn check_words(db: &Database, activity: &UserActivity) -> Result<()> {
    match crate::check_platform_words(db, &texts_of(activity)).await? {
        Some(category) => Err(create_error!(ProfileTextFiltered {
            category: format!("{category:?}"),
        })),
        None => Ok(()),
    }
}

/// Whether this person shares their activity (the setting in force: unset
/// is on for adults, off under 18)
pub async fn shares(db: &Database, user: &User) -> bool {
    user.bot.is_none()
        && user
            .effective_privacy(db)
            .await
            .share_activity
            .unwrap_or(false)
}

/// Count one change; `Err(retry_after_ms)` when there were too many.
/// Without Redis nothing is refused.
pub async fn count_change(user_id: &str, limit: i64, window_seconds: u64) -> std::result::Result<(), u64> {
    let Ok(conn) = get_connection().await else {
        return Ok(());
    };
    let mut conn = conn.into_inner();
    let key = format!("activity_changes:{user_id}");
    let counted: std::result::Result<(i64, i64), _> = redis::pipe()
        .atomic()
        .incr(&key, 1i64)
        .cmd("PTTL")
        .arg(&key)
        .query_async(&mut conn)
        .await;
    let Ok((count, ttl)) = counted else {
        return Ok(());
    };
    if ttl < 0 {
        let _: std::result::Result<(), _> = conn.expire(&key, window_seconds as usize).await;
    }
    if count > limit {
        let wait = if ttl > 0 { ttl as u64 } else { window_seconds * 1000 };
        return Err(wait);
    }
    Ok(())
}

fn parse(values: HashMap<String, String>) -> Vec<(UserActivity, Option<String>)> {
    let mut all: Vec<(UserActivity, Option<String>)> = values
        .into_values()
        .filter_map(|value| serde_json::from_str::<Stored>(&value).ok())
        .map(|stored| (stored.activity, stored.owner))
        .collect();
    all.sort_by_key(|(activity, _)| activity.source);
    all
}

/// Activities of one person, the main one first
pub async fn list(user_id: &str) -> Vec<UserActivity> {
    let Ok(mut conn) = get_connection().await else {
        return vec![];
    };
    let values: HashMap<String, String> = conn
        .hgetall(revolt_presence::activity_key(user_id))
        .await
        .unwrap_or_default();
    parse(values).into_iter().map(|(activity, _)| activity).collect()
}

/// Activities of many people at once
pub async fn list_many(user_ids: &[String]) -> HashMap<String, Vec<UserActivity>> {
    let mut found = HashMap::new();
    if user_ids.is_empty() {
        return found;
    }
    let Ok(conn) = get_connection().await else {
        return found;
    };
    let mut conn = conn.into_inner();
    for chunk in user_ids.chunks(500) {
        let mut pipe = redis::pipe();
        for id in chunk {
            pipe.hgetall(revolt_presence::activity_key(id));
        }
        let answers: Vec<HashMap<String, String>> =
            pipe.query_async(&mut conn).await.unwrap_or_default();
        for (id, values) in chunk.iter().zip(answers) {
            let activities: Vec<UserActivity> =
                parse(values).into_iter().map(|(activity, _)| activity).collect();
            if !activities.is_empty() {
                found.insert(id.clone(), activities);
            }
        }
    }
    found
}

/// Keep (or with `None` remove) one source's activity. False when nothing
/// changed.
pub async fn store(
    user_id: &str,
    source: ActivitySource,
    activity: Option<UserActivity>,
    owner: Option<&str>,
) -> bool {
    let Ok(mut conn) = get_connection().await else {
        return false;
    };
    let key = revolt_presence::activity_key(user_id);
    let before: Option<String> = conn.hget(&key, field(source)).await.unwrap_or(None);
    match activity {
        Some(activity) => {
            let value = serde_json::to_string(&Stored {
                activity: activity.clone(),
                owner: owner.map(str::to_string),
            })
            .unwrap_or_default();
            let same = before
                .and_then(|before| serde_json::from_str::<Stored>(&before).ok())
                .is_some_and(|before| before.activity == activity);
            let _: std::result::Result<(), _> = conn.hset(&key, field(source), value).await;
            let _: std::result::Result<(), _> = conn.expire(&key, KEEP_SECONDS).await;
            !same
        }
        None => {
            if before.is_none() {
                return false;
            }
            let _: std::result::Result<(), _> = conn.hdel(&key, field(source)).await;
            true
        }
    }
}

/// Remove what one login session set. True when something went.
pub async fn forget_owned_by(user_id: &str, owner: &str) -> bool {
    let Ok(mut conn) = get_connection().await else {
        return false;
    };
    let key = revolt_presence::activity_key(user_id);
    let values: HashMap<String, String> = conn.hgetall(&key).await.unwrap_or_default();
    let mut gone = false;
    for (name, value) in values {
        let owned = serde_json::from_str::<Stored>(&value)
            .ok()
            .and_then(|stored| stored.owner)
            .is_some_and(|by| by == owner);
        if owned {
            let _: std::result::Result<(), _> = conn.hdel(&key, name).await;
            gone = true;
        }
    }
    gone
}

/// Remove every activity of a person. True when there was one.
pub async fn forget_all(user_id: &str) -> bool {
    let Ok(mut conn) = get_connection().await else {
        return false;
    };
    let removed: i64 = conn
        .del(revolt_presence::activity_key(user_id))
        .await
        .unwrap_or(0);
    removed > 0
}

/// `SetActivity` from a person's own session (`owner` is its login
/// session id; bots have none)
pub async fn set_from_session(
    db: &Database,
    user_id: &str,
    owner: &str,
    source: ActivitySource,
    input: Option<DataActivity>,
) -> Result<()> {
    let config = revolt_config::config().await;
    let settings = &config.features.rich_presence;
    if !settings.enabled {
        return Err(create_error!(FeatureDisabled {
            feature: "rich_presence".to_string(),
        }));
    }
    if owner.is_empty() {
        return Err(create_error!(IsBot));
    }
    if source == ActivitySource::Steam {
        return Err(refuse("activity source Steam is set by the server"));
    }
    let user = db.fetch_user(user_id).await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if let Err(retry_after) =
        count_change(user_id, settings.updates_per_window, settings.window_seconds).await
    {
        return Err(create_error!(RateLimited {
            retry_after,
            global: false,
        }));
    }
    if !shares(db, &user).await {
        // nothing is kept for someone who does not share
        if forget_all(user_id).await {
            publish(db, &user).await;
        }
        return Ok(());
    }

    let activity = match input {
        Some(input) => {
            let hosts = Hosts::from_config().await;
            let activity = check(input, source, Timestamp::now_utc(), &hosts)?;
            check_words(db, &activity).await?;
            Some(activity)
        }
        None => None,
    };
    if store(user_id, source, activity, Some(owner)).await {
        publish(db, &user).await;
    }
    Ok(())
}

/// Put the activities on users shown to someone. Only users shown online
/// get them (being online already takes blocks and invisibility into
/// account); nothing when the feature is off.
pub async fn fill(users: &mut [v0::User]) {
    if !enabled().await {
        return;
    }
    let online: Vec<String> = users
        .iter()
        .filter(|user| user.online)
        .map(|user| user.id.clone())
        .collect();
    let mut found = list_many(&online).await;
    for user in users.iter_mut() {
        if let Some(activities) = found.remove(&user.id) {
            user.activities = activities;
        }
    }
}

/// `fill` for one user
pub async fn fill_one(user: &mut v0::User) {
    fill(std::slice::from_mut(user)).await;
}

/// A person's own activities, for themselves: also while they are
/// invisible (others see nothing then)
pub async fn fill_own(user: &mut v0::User) {
    if enabled().await {
        user.activities = list(&user.id).await;
    }
}

fn is_invisible(user: &User) -> bool {
    matches!(
        user.status,
        Some(crate::UserStatus {
            presence: Some(crate::Presence::Invisible),
            ..
        })
    )
}

fn update(user_id: &str, activities: Vec<UserActivity>) -> EventV1 {
    EventV1::UserUpdate {
        id: user_id.to_string(),
        data: PartialUser {
            activities: Some(activities),
            ..Default::default()
        },
        clear: vec![],
        event_id: Some(Ulid::new().to_string()),
    }
}

/// Tell everyone who may see this person what they are doing now: their
/// servers and their own topic, as with presence. While they are invisible
/// the others get an empty list and only the person's own sessions the
/// real one.
pub async fn publish(db: &Database, user: &User) {
    let current = list(&user.id).await;
    let mut topics: Vec<String> = db
        .fetch_all_memberships(&user.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|member| member.id.server)
        .collect();
    topics.push(user.id.clone());

    if is_invisible(user) {
        update(&user.id, vec![]).p_broadcast(topics).await;
        update(&user.id, current).private(user.id.clone()).await;
    } else {
        update(&user.id, current).p_broadcast(topics).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Hosts {
        Hosts {
            autumn: "https://cdn.zrzur.test".into(),
            january: "https://proxy.zrzur.test".into(),
        }
    }

    fn game(name: &str) -> DataActivity {
        DataActivity {
            name: name.into(),
            ..Default::default()
        }
    }

    fn rule_of(error: revolt_result::Error) -> String {
        match error.error_type {
            revolt_result::ErrorType::FailedValidation { error, .. } => error,
            other => panic!("not a validation error: {other:?}"),
        }
    }

    #[test]
    fn a_plain_game_is_kept_trimmed() {
        let now = Timestamp::now_utc();
        let activity = check(
            DataActivity {
                name: "  Star Kebab  ".into(),
                details: Some("Level 3".into()),
                state: Some("   ".into()),
                started_at: Some(now - Duration::minutes(5)),
                ..Default::default()
            },
            ActivitySource::Rpc,
            now,
            &hosts(),
        )
        .unwrap();
        assert_eq!(activity.name, "Star Kebab");
        assert_eq!(activity.details.as_deref(), Some("Level 3"));
        assert_eq!(activity.state, None, "blank text is left out");
        assert_eq!(activity.source, ActivitySource::Rpc);
    }

    #[test]
    fn lengths_are_held() {
        let now = Timestamp::now_utc();
        let long = "x".repeat(NAME_MAX + 1);
        assert!(rule_of(check(game(&long), ActivitySource::Rpc, now, &hosts()).unwrap_err())
            .contains("name is longer"));
        // characters count, not bytes: 128 Arabic letters are fine
        let arabic = "ز".repeat(NAME_MAX);
        assert!(check(game(&arabic), ActivitySource::Rpc, now, &hosts()).is_ok());
        assert!(rule_of(check(game("   "), ActivitySource::Rpc, now, &hosts()).unwrap_err())
            .contains("required"));
        let mut details = game("Game");
        details.details = Some("d".repeat(TEXT_MAX + 1));
        assert!(check(details, ActivitySource::Rpc, now, &hosts()).is_err());
        let mut lines = game("Game");
        lines.state = Some("one\ntwo".into());
        assert!(rule_of(check(lines, ActivitySource::Rpc, now, &hosts()).unwrap_err())
            .contains("one line"));
    }

    #[test]
    fn buttons_are_two_https_links_at_most() {
        let now = Timestamp::now_utc();
        let button = |url: &str| ActivityButton {
            label: "Join".into(),
            url: url.into(),
        };
        let mut ok = game("Game");
        ok.buttons = vec![button("https://example.com/join"), button("https://example.com/b")];
        assert_eq!(check(ok, ActivitySource::Rpc, now, &hosts()).unwrap().buttons.len(), 2);

        let mut three = game("Game");
        three.buttons = vec![
            button("https://a.example"),
            button("https://b.example"),
            button("https://c.example"),
        ];
        assert!(check(three, ActivitySource::Rpc, now, &hosts()).is_err());

        for bad in [
            "http://example.com",
            "javascript:alert(1)",
            "https://",
            "https://user@example.com/",
            "https://exa mple.com",
        ] {
            let mut one = game("Game");
            one.buttons = vec![button(bad)];
            assert!(
                check(one, ActivitySource::Rpc, now, &hosts()).is_err(),
                "{bad} is refused"
            );
        }

        let mut long_label = game("Game");
        long_label.buttons = vec![ActivityButton {
            label: "l".repeat(LABEL_MAX + 1),
            url: "https://example.com".into(),
        }];
        assert!(check(long_label, ActivitySource::Rpc, now, &hosts()).is_err());
    }

    #[test]
    fn pictures_stay_on_zrzur_servers() {
        let now = Timestamp::now_utc();
        let mut own = game("Game");
        own.large_image = Some("https://cdn.zrzur.test/icons/abc".into());
        own.small_image = Some("https://proxy.zrzur.test/proxy?url=x".into());
        let kept = check(own, ActivitySource::Rpc, now, &hosts()).unwrap();
        assert_eq!(kept.large_image.as_deref(), Some("https://cdn.zrzur.test/icons/abc"));
        assert_eq!(kept.small_image.as_deref(), Some("https://proxy.zrzur.test/proxy?url=x"));

        let mut outside = game("Game");
        outside.large_image = Some("https://img.example/a b?.png".into());
        assert!(check(outside, ActivitySource::Rpc, now, &hosts()).is_err());

        let mut outside = game("Game");
        outside.large_image = Some("https://img.example/cover.png?s=1&t=2".into());
        let proxied = check(outside, ActivitySource::Rpc, now, &hosts()).unwrap();
        assert_eq!(
            proxied.large_image.as_deref(),
            Some("https://proxy.zrzur.test/proxy?url=https%3A%2F%2Fimg.example%2Fcover.png%3Fs%3D1%26t%3D2")
        );

        for bad in ["http://img.example/a.png", "data:image/png;base64,AAAA", "file:///c:/x"] {
            let mut one = game("Game");
            one.small_image = Some(bad.into());
            assert!(check(one, ActivitySource::Rpc, now, &hosts()).is_err(), "{bad} is refused");
        }
        // a look-alike of our host is not ours
        let mut lookalike = game("Game");
        lookalike.large_image = Some("https://cdn.zrzur.test.evil.example/a.png".into());
        let proxied = check(lookalike, ActivitySource::Rpc, now, &hosts()).unwrap();
        assert!(proxied.large_image.unwrap().starts_with("https://proxy.zrzur.test/proxy?url="));
    }

    #[test]
    fn times_make_sense() {
        let now = Timestamp::now_utc();
        let mut ahead = game("Game");
        ahead.started_at = Some(now + Duration::minutes(10));
        assert!(check(ahead, ActivitySource::Rpc, now, &hosts()).is_err());

        let mut backwards = game("Game");
        backwards.started_at = Some(now);
        backwards.ends_at = Some(now - Duration::minutes(1));
        assert!(check(backwards, ActivitySource::Rpc, now, &hosts()).is_err());

        let mut far = game("Game");
        far.ends_at = Some(now + Duration::days(8));
        assert!(check(far, ActivitySource::Rpc, now, &hosts()).is_err());

        let mut song = game("Song");
        song.started_at = Some(now - Duration::minutes(1));
        song.ends_at = Some(now + Duration::minutes(2));
        assert!(check(song, ActivitySource::Rpc, now, &hosts()).is_ok());
    }

    #[test]
    fn application_ids_are_plain_ids() {
        let now = Timestamp::now_utc();
        let mut ok = game("Game");
        ok.application_id = Some("01HZZRZURAPP00000000000000".into());
        assert!(check(ok, ActivitySource::Rpc, now, &hosts()).is_ok());
        let mut bad = game("Game");
        bad.application_id = Some("../../etc".into());
        assert!(check(bad, ActivitySource::Rpc, now, &hosts()).is_err());
    }

    #[test]
    fn every_shown_text_meets_the_word_list() {
        let now = Timestamp::now_utc();
        let mut all = game("Name");
        all.details = Some("Details".into());
        all.state = Some("State".into());
        all.large_text = Some("Large".into());
        all.small_text = Some("Small".into());
        all.buttons = vec![ActivityButton {
            label: "Label".into(),
            url: "https://example.com".into(),
        }];
        let checked = check(all, ActivitySource::Rpc, now, &hosts()).unwrap();
        let texts = texts_of(&checked);
        for part in ["Name", "Details", "State", "Large", "Small", "Label"] {
            assert!(texts.contains(part), "{part} goes through the word list");
        }
        assert!(!texts.contains("example.com"), "addresses are not texts");
    }

    #[test]
    fn the_app_itself_comes_first() {
        let make = |source| UserActivity {
            source,
            kind: v0::ActivityType::Playing,
            name: "x".into(),
            details: None,
            state: None,
            started_at: None,
            ends_at: None,
            application_id: None,
            large_image: None,
            large_text: None,
            small_image: None,
            small_text: None,
            buttons: vec![],
        };
        let mut all = vec![
            make(ActivitySource::Steam),
            make(ActivitySource::Rpc),
            make(ActivitySource::Desktop),
        ];
        UserActivity::sort(&mut all);
        let order: Vec<_> = all.iter().map(|a| a.source).collect();
        assert_eq!(
            order,
            vec![ActivitySource::Rpc, ActivitySource::Desktop, ActivitySource::Steam]
        );
    }
}
