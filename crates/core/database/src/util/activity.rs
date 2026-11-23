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
/// Longest application id (a ULID is 26; a local app's own id up to 64)
pub const APPLICATION_ID_MAX: usize = 64;
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

/// Characters that may hide or reorder text: control characters, line and
/// paragraph separators (Zl, Zp) and format characters (Cf: bidi overrides
/// and isolates, zero-width spaces, word joiners, the byte order mark, tags).
/// Zero-width non-joiner and joiner (U+200C, U+200D) stay: Persian words and
/// emoji sequences need them, and they reorder nothing.
pub fn hidden_char(c: char) -> bool {
    if c.is_control() {
        return true;
    }
    let n = c as u32;
    matches!(
        n,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B
            | 0x200E..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// A text as shown: trimmed, one line, nothing hidden in it
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
    if value.chars().any(hidden_char) {
        return Err(refuse(format!("activity.{name} must be one line of plain text")));
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
            if id.len() > APPLICATION_ID_MAX
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            {
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

/// Whether a platform-wide block stops this activity (legal RP-07): its app
/// id, or a picture, as the app sent it or as it is shown through the proxy
pub fn blocked_by(activity: &UserActivity, blocks: &[v0::ActivityBlock]) -> bool {
    blocks.iter().any(|block| match block.kind {
        v0::ActivityBlockKind::Application => {
            activity.application_id.as_deref() == Some(block.value.as_str())
        }
        v0::ActivityBlockKind::Picture => {
            let proxied = encode_query(&block.value);
            [&activity.large_image, &activity.small_image]
                .into_iter()
                .flatten()
                .any(|shown| shown == &block.value || shown.ends_with(&format!("url={proxied}")))
        }
    })
}

/// The values a block on this activity would carry: its app id, its
/// pictures, and the address inside a proxied picture
pub fn block_candidates(activity: &UserActivity) -> Vec<String> {
    let mut values: Vec<String> = activity.application_id.iter().cloned().collect();
    for shown in [&activity.large_image, &activity.small_image].into_iter().flatten() {
        values.push(shown.clone());
        if let Some((_, inner)) = shown.rsplit_once("url=") {
            values.push(decode_query(inner));
        }
    }
    values.sort();
    values.dedup();
    values
}

/// The reverse of `encode_query`
pub fn decode_query(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // bytes, not a str slice: a '%' before a multi-byte character
            // must not split it
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Refuse an activity a platform-wide block stops (one indexed lookup by
/// value, not every block)
pub async fn check_blocks(db: &Database, activity: &UserActivity) -> Result<()> {
    let candidates = block_candidates(activity);
    if candidates.is_empty() {
        return Ok(());
    }
    if blocked_by(activity, &db.fetch_activity_blocks_with_values(&candidates).await?) {
        return Err(create_error!(ProfileTextFiltered {
            category: "ActivityBlocked".to_string(),
        }));
    }
    Ok(())
}

/// Stop an app id or a picture everywhere (Trust & Safety)
pub async fn block(
    db: &Database,
    kind: v0::ActivityBlockKind,
    value: &str,
    case_id: Option<&str>,
    actor_id: &str,
    now: i64,
) -> Result<()> {
    db.insert_activity_block(&v0::ActivityBlock {
        id: Ulid::new().to_string(),
        kind,
        value: value.chars().take(URL_MAX).collect(),
        case_id: case_id.map(str::to_string),
        actor_id: actor_id.to_string(),
        created_at: now,
    })
    .await
}

/// Lift the blocks taken on one case (an action reversed)
pub async fn unblock_case(db: &Database, case_id: &str) -> Result<()> {
    for block in db.fetch_activity_blocks_of_case(case_id).await? {
        db.delete_activity_block(&block.id).await?;
    }
    Ok(())
}

/// A copy of shown activities for a report: everything others saw, and the
/// app ids and picture addresses staff can block
pub fn evidence_text(activities: &[UserActivity]) -> String {
    activities
        .iter()
        .map(|activity| {
            let mut parts = vec![format!("{:?} {:?}: {}", activity.source, activity.kind, activity.name)];
            for (label, value) in [
                ("details", &activity.details),
                ("state", &activity.state),
                ("app", &activity.application_id),
                ("large_image", &activity.large_image),
                ("large_text", &activity.large_text),
                ("small_image", &activity.small_image),
                ("small_text", &activity.small_text),
            ] {
                if let Some(value) = value {
                    parts.push(format!("{label}: {value}"));
                }
            }
            for button in &activity.buttons {
                parts.push(format!("button: {} <{}>", button.label, button.url));
            }
            parts.join(" | ")
        })
        .collect::<Vec<_>>()
        .join("
")
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

/// Field of the hash that says only friends may see the activities
const AUDIENCE_FIELD: &str = "Audience";
const FRIENDS_ONLY: &str = "Friends";

/// Whether this person shares their activity (the setting in force: off
/// until chosen), and if so whether only friends may see it (under 18, no
/// stated age, or an age that cannot be read)
pub async fn sharing(db: &Database, user: &User) -> Option<bool> {
    if user.bot.is_some() {
        return None;
    }
    let privacy = user.effective_privacy(db).await;
    privacy
        .share_activity
        .unwrap_or(false)
        .then_some(privacy.activity_friends_only)
}

/// Whether this person shares their activity
pub async fn shares(db: &Database, user: &User) -> bool {
    sharing(db, user).await.is_some()
}

/// Record who may see a person's activities, kept with them so every read
/// can check it without asking the database
pub async fn set_audience(user_id: &str, friends_only: bool) {
    let Ok(mut conn) = get_connection().await else {
        return;
    };
    let key = revolt_presence::activity_key(user_id);
    if friends_only {
        write_while_online(user_id, AUDIENCE_FIELD, FRIENDS_ONLY).await;
    } else {
        let _: std::result::Result<(), _> = conn.hdel(&key, AUDIENCE_FIELD).await;
    }
}

/// Write one field of the hash only while the person has a session, in one
/// step: a write racing the last session's end can never leave an activity
/// behind with nobody online. False when nothing was written.
async fn write_while_online(user_id: &str, field: &str, value: &str) -> bool {
    let Ok(conn) = get_connection().await else {
        return false;
    };
    let mut conn = conn.into_inner();
    let written: std::result::Result<i64, _> = redis::Script::new(
        r"if redis.call('SCARD', KEYS[2]) > 0 then
            redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
            redis.call('EXPIRE', KEYS[1], ARGV[3])
            return 1
          end
          return 0",
    )
    .key(revolt_presence::activity_key(user_id))
    .key(format!("sessions:{user_id}"))
    .arg(field)
    .arg(value)
    .arg(KEEP_SECONDS)
    .invoke_async(&mut conn)
    .await;
    written.unwrap_or(0) == 1
}

/// Members of a server, from the same one-hour cache the server list uses
async fn member_count(db: &Database, server_id: &str) -> usize {
    let key = format!("member_count:{server_id}");
    if let Ok(mut conn) = get_connection().await {
        if let Ok(Some(count)) = conn.get::<_, Option<usize>>(&key).await {
            return count;
        }
        let Ok(count) = db.fetch_member_count(server_id).await else {
            return usize::MAX;
        };
        let _: std::result::Result<(), _> = conn.set_ex(&key, count, 60 * 60).await;
        return count;
    }
    db.fetch_member_count(server_id).await.unwrap_or(usize::MAX)
}

fn friends_only_in(values: &HashMap<String, String>) -> bool {
    values.get(AUDIENCE_FIELD).map(String::as_str) == Some(FRIENDS_ONLY)
}

/// Whether either of them blocked the other
fn blocked_between(viewer: &User, target: &str) -> bool {
    viewer.relations.iter().flatten().any(|relation| {
        relation.id == target
            && matches!(
                relation.status,
                crate::RelationshipStatus::Blocked | crate::RelationshipStatus::BlockedOther
            )
    })
}

/// Whether `viewer` and `target` are friends, from the viewer's side
fn are_friends(viewer: &User, target: &str) -> bool {
    viewer.relations.iter().flatten().any(|relation| {
        relation.id == target && relation.status == crate::RelationshipStatus::Friend
    })
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

/// Activities of many people at once, with whether only friends may see them
pub async fn list_many(user_ids: &[String]) -> HashMap<String, (Vec<UserActivity>, bool)> {
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
            let friends_only = friends_only_in(&values);
            let activities: Vec<UserActivity> =
                parse(values).into_iter().map(|(activity, _)| activity).collect();
            if !activities.is_empty() {
                found.insert(id.clone(), (activities, friends_only));
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
            if !write_while_online(user_id, field(source), &value).await {
                return false;
            }
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
    let Some(friends_only) = sharing(db, &user).await else {
        // nothing is kept for someone who does not share
        if forget_all(user_id).await {
            publish(db, &user).await;
        }
        return Ok(());
    };
    set_audience(user_id, friends_only).await;

    let activity = match input {
        Some(input) => {
            let hosts = Hosts::from_config().await;
            let activity = check(input, source, Timestamp::now_utc(), &hosts)?;
            check_words(db, &activity).await?;
            check_blocks(db, &activity).await?;
            Some(activity)
        }
        None => None,
    };
    if store(user_id, source, activity, Some(owner)).await {
        publish(db, &user).await;
    }
    Ok(())
}

/// Put the activities on users shown to `viewer`. Only users shown online
/// get them (being online already takes blocks and invisibility into
/// account), and where only friends may see them, only a friend of theirs;
/// nothing when the feature is off.
pub async fn fill(users: &mut [v0::User], viewer: Option<&User>) {
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
        if let Some((activities, friends_only)) = found.remove(&user.id) {
            let own = viewer.is_some_and(|viewer| viewer.id == user.id);
            // hidden both ways when one of them blocked the other
            let blocked = viewer.is_some_and(|viewer| blocked_between(viewer, &user.id));
            let allowed = own
                || (!blocked
                    && (!friends_only
                        || viewer.is_some_and(|viewer| are_friends(viewer, &user.id))));
            if allowed {
                user.activities = activities;
            }
        }
    }
}

/// `fill` for one user
pub async fn fill_one(user: &mut v0::User, viewer: Option<&User>) {
    fill(std::slice::from_mut(user), viewer).await;
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
/// real one. When only friends may see it, only friends are told (each on
/// their private topic) and nobody else gets anything.
pub async fn publish(db: &Database, user: &User) {
    let current = list(&user.id).await;
    if let Some(friends_only) = sharing(db, user).await {
        set_audience(&user.id, friends_only).await;
        if friends_only {
            let friends: Vec<String> = user
                .relations
                .iter()
                .flatten()
                .filter(|relation| relation.status == crate::RelationshipStatus::Friend)
                .map(|relation| relation.id.clone())
                .collect();
            if is_invisible(user) {
                update(&user.id, vec![]).private_broadcast(friends).await;
                update(&user.id, current).private(user.id.clone()).await;
            } else {
                let mut everyone = friends;
                everyone.push(user.id.clone());
                update(&user.id, current).private_broadcast(everyone).await;
            }
            return;
        }
    }
    // servers up to the size limit are told at once; larger ones read it
    // when someone opens the profile or the member list
    let limit = revolt_config::config()
        .await
        .features
        .rich_presence
        .broadcast_max_members;
    let mut topics: Vec<String> = vec![];
    for member in db.fetch_all_memberships(&user.id).await.unwrap_or_default() {
        if member_count(db, &member.id.server).await <= limit {
            topics.push(member.id.server);
        }
    }
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

    #[test]
    fn a_block_is_found_by_its_value_also_inside_a_proxied_picture() {
        let picture = "https://pics.example/a b/ü.png?x=1&y=2";
        assert_eq!(decode_query(&encode_query(picture)), picture);
        assert_eq!(decode_query("100%"), "100%", "a lone percent stays");
        assert_eq!(decode_query("%E2%9C%93%zz"), "\u{2713}%zz");
        let activity = UserActivity {
            application_id: Some("com.example.app".into()),
            large_image: Some(format!("https://proxy.zrzur.test/proxy?url={}", encode_query(picture))),
            ..game_activity()
        };
        let candidates = block_candidates(&activity);
        assert!(candidates.contains(&"com.example.app".to_string()));
        assert!(candidates.contains(&picture.to_string()));
        // what is looked up is what blocked_by then matches
        let block = v0::ActivityBlock {
            id: "b".into(),
            kind: v0::ActivityBlockKind::Picture,
            value: picture.into(),
            case_id: None,
            actor_id: "staff".into(),
            created_at: 0,
        };
        assert!(blocked_by(&activity, &[block]));
    }

    fn game_activity() -> UserActivity {
        check(game("Star Kebab"), ActivitySource::Rpc, Timestamp::now_utc(), &hosts()).unwrap()
    }

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
    fn hidden_and_reordering_characters_are_refused() {
        let now = Timestamp::now_utc();
        for bad in [
            "Game\u{202E}gnp.exe", // right-to-left override
            "Ga\u{200B}me",        // zero-width space
            "Ga\u{2066}me",        // left-to-right isolate
            "Line\u{2028}break",   // line separator
            "Para\u{2029}graph",   // paragraph separator
            "Ga\u{FEFF}me",        // byte order mark
            "Ga\u{00AD}me",        // soft hyphen
            "Tag\u{E0041}",        // tag character
        ] {
            assert!(check(game(bad), ActivitySource::Rpc, now, &hosts()).is_err(), "{bad:?}");
        }
        // Persian words and emoji sequences keep their joiners
        assert!(check(game("می\u{200C}خواهم"), ActivitySource::Rpc, now, &hosts()).is_ok());
        assert!(check(game("👩\u{200D}💻 Coding"), ActivitySource::Rpc, now, &hosts()).is_ok());
        // ordinary Arabic and German text
        assert!(check(game("لعبة الشطرنج"), ActivitySource::Rpc, now, &hosts()).is_ok());
        assert!(check(game("Straßenbahn-Simulator"), ActivitySource::Rpc, now, &hosts()).is_ok());
    }

    #[test]
    fn app_ids_may_be_a_local_app_name() {
        let now = Timestamp::now_utc();
        let mut ok = game("Game");
        ok.application_id = Some("zrzur-vscode".into());
        assert!(check(ok, ActivitySource::Rpc, now, &hosts()).is_ok());
        let mut long = game("Game");
        long.application_id = Some("x".repeat(65));
        assert!(check(long, ActivitySource::Rpc, now, &hosts()).is_err());
    }

    #[test]
    fn blocks_match_the_app_id_and_the_picture_as_sent_or_proxied() {
        let now = Timestamp::now_utc();
        let mut input = game("Game");
        input.application_id = Some("bad-app".into());
        input.large_image = Some("https://img.example/bad.png".into());
        let shown = check(input, ActivitySource::Rpc, now, &hosts()).unwrap();
        let block = |kind, value: &str| v0::ActivityBlock {
            id: "b".into(),
            kind,
            value: value.into(),
            case_id: None,
            actor_id: "a".into(),
            created_at: 0,
        };
        assert!(blocked_by(&shown, &[block(v0::ActivityBlockKind::Application, "bad-app")]));
        assert!(!blocked_by(&shown, &[block(v0::ActivityBlockKind::Application, "good-app")]));
        assert!(blocked_by(&shown, &[block(v0::ActivityBlockKind::Picture, "https://img.example/bad.png")]));
        let proxied = shown.large_image.clone().unwrap();
        assert!(blocked_by(&shown, &[block(v0::ActivityBlockKind::Picture, &proxied)]));
        assert!(!blocked_by(&shown, &[block(v0::ActivityBlockKind::Picture, "https://img.example/other.png")]));
        assert!(!blocked_by(&shown, &[]));
    }

    #[test]
    fn the_report_copy_holds_what_others_saw() {
        let now = Timestamp::now_utc();
        let mut input = game("Star Kebab");
        input.details = Some("Level 3".into());
        input.application_id = Some("my-game".into());
        input.buttons = vec![ActivityButton {
            label: "Join".into(),
            url: "https://example.com/join".into(),
        }];
        let shown = check(input, ActivitySource::Rpc, now, &hosts()).unwrap();
        let text = evidence_text(&[shown]);
        for part in ["Star Kebab", "details: Level 3", "app: my-game", "button: Join <https://example.com/join>"] {
            assert!(text.contains(part), "{part} in {text}");
        }
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
