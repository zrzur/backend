//! Server insights: daily counters of a server, kept per UTC day.
//!
//! One document per server and UTC day (`<server>:<day>`), with the counts of
//! each hour of that day, so a viewer's days can be counted in their own
//! time zone. How the counts get there, by write cost:
//!
//! - joins, leaves and voice time are rare next to messages, so delta and
//!   voice-ingress add each with an `$inc` upsert as it happens;
//! - messages are never counted on the send path. crond rolls each finished
//!   hour up once: one aggregation over that hour's slice of the message id
//!   index (ids are ULIDs, so an hour is an id range), grouped by channel and
//!   author, then one update per server that had messages. An hour is folded
//!   in only if the document does not list it yet, so a crash and a second
//!   run never count it twice. An hour is rolled up only `GRACE_SECONDS`
//!   after its end: a message id carries the clock of the delta that made
//!   it, so a delta up to that much ahead or behind still lands in its hour.
//! - active members: the authors (bots left out) of an open day go into a
//!   Redis HyperLogLog (`insight_authors:<server>:<day>`, 48 h expiry); the
//!   row keeps only the estimate. No user id is ever written to MongoDB.
//!
//! Reading a page is one indexed find on (server, day). Rows older than
//! `RETENTION_DAYS` (plus one day for time zones) are deleted by crond.
//!
//! Time zones: the counts of an hour go to the viewer's day of the start of
//! that hour. In zones with a half-hour (or 45-minute) offset, such as
//! India, the hour that straddles local midnight goes whole to one day.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;

use iso8601_timestamp::Timestamp;
use redis_kiss::{get_connection, redis};
use revolt_models::v0;
use revolt_result::Result;
use ulid::Ulid;

use crate::util::tz::{civil_from_days, Zone};
use crate::Database;

/// Days of rows kept (the longest range shown)
pub const RETENTION_DAYS: i64 = 90;
/// Ranges a page can show
pub const INSIGHT_RANGES: [u32; 3] = [7, 30, 90];
/// Counts below this (and above 0) are shown as "fewer than 5"
pub const SMALL_COUNT: u32 = 5;
/// Hours one crond pass rolls up at most (a long outage catches up over
/// several passes)
pub const HOURS_PER_PASS: i64 = 48;
/// Hours rolled up on the very first run (nothing counted before)
pub const FIRST_RUN_HOURS: i64 = 7 * 24;
/// Channels listed at most
pub const TOP_CHANNELS: usize = 10;
/// An hour is rolled up this long after its end (clock skew between the
/// deltas that make message ids and crond)
pub const GRACE_SECONDS: i64 = 5 * 60;
/// How long the authors of a day stay in Redis (a day and a day's margin)
pub const AUTHORS_TTL_SECONDS: i64 = 48 * 3600;
/// Open days closed in one pass at most
pub const CLOSE_PER_PASS: i64 = 1000;

auto_derived!(
    /// One server's counts for one UTC day. Hour maps are keyed by the hour
    /// of the day ("0" to "23").
    #[derive(Default)]
    pub struct ServerInsightDay {
        /// `<server>:<day>`
        #[serde(rename = "_id")]
        pub id: String,
        /// Server id
        pub server: String,
        /// Days since 1970-01-01 (UTC)
        pub day: i64,
        /// Messages per hour and channel id
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub messages: BTreeMap<String, BTreeMap<String, u32>>,
        /// Joins per hour
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub joins: BTreeMap<String, u32>,
        /// Leaves (also kicks, bans and deleted accounts) per hour
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub leaves: BTreeMap<String, u32>,
        /// Seconds in voice per hour (counted at the hour someone leaves)
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub voice: BTreeMap<String, u32>,
        /// Joins by where they came from (`invite`, `discover`, ...)
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub sources: BTreeMap<String, u32>,
        /// Distinct authors of the day (an estimate, bots left out)
        #[serde(default)]
        pub active: u32,
        /// The day still takes authors (crond closes it)
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pub open: bool,
        /// Hours of the day whose messages are in (0 to 23)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub hours: Vec<i64>,
    }

    /// Where a join came from
    #[derive(Copy)]
    pub enum InsightJoinSource {
        Invite,
        Discover,
        Vanity,
        Other,
    }

    /// One counter to add to
    pub enum InsightCounter {
        Join(InsightJoinSource),
        Leave,
        VoiceSeconds(u32),
    }

    /// Messages of one channel by one author within an hour
    pub struct MessageTally {
        pub channel: String,
        /// None for messages sent through a webhook (not a member)
        pub author: Option<String>,
        pub count: u32,
    }

    /// What one hour adds to one server
    #[derive(Default)]
    pub struct InsightHour {
        /// Messages per channel id
        pub channels: BTreeMap<String, u32>,
        /// Distinct authors of the day so far, when Redis answered
        pub active: Option<u32>,
    }

    /// What a crond pass did
    #[derive(Default)]
    pub struct InsightPassReport {
        pub hours: u32,
        pub servers: u32,
        pub closed: u64,
        pub deleted: u64,
        /// Steps or server updates that failed (logged; the pass went on)
        pub failures: u32,
    }
);

impl InsightJoinSource {
    pub fn key(self) -> &'static str {
        match self {
            InsightJoinSource::Invite => "invite",
            InsightJoinSource::Discover => "discover",
            InsightJoinSource::Vanity => "vanity",
            InsightJoinSource::Other => "other",
        }
    }
}

/// Hours since 1970-01-01T00:00Z
pub fn hour_of(at: Timestamp) -> i64 {
    seconds_of(at).div_euclid(3600)
}

fn seconds_of(at: Timestamp) -> i64 {
    at.duration_since(Timestamp::UNIX_EPOCH).whole_seconds()
}

/// The last hour crond may roll up at `now`: finished, and `GRACE_SECONDS` ago
pub fn last_rollable_hour(now: Timestamp) -> i64 {
    (seconds_of(now) - GRACE_SECONDS).div_euclid(3600) - 1
}

/// The smallest message id of an hour (ULIDs sort by their time)
pub fn first_id_of_hour(hour: i64) -> String {
    Ulid::from_parts((hour.max(0) as u64) * 3_600_000, 0).to_string()
}

/// `YYYY-MM-DD` of a day number
pub fn day_label(day: i64) -> String {
    let (year, month, date) = civil_from_days(day);
    format!("{year:04}-{month:02}-{date:02}")
}

/// A count as shown where one person could be picked out: 1 to 4 become
/// `None` ("fewer than 5")
pub fn shown_count(count: u32) -> Option<u32> {
    if count > 0 && count < SMALL_COUNT {
        None
    } else {
        Some(count)
    }
}

/// Voice seconds as whole minutes (half a minute rounds up)
pub fn voice_minutes(seconds: u64) -> u32 {
    ((seconds + 30) / 60).min(u32::MAX as u64) as u32
}

/// Servers with voice time waiting for crond (a Redis set)
pub const VOICE_PENDING_SERVERS: &str = "insight_voice_servers";

/// A server's voice seconds waiting for crond (a Redis hash: hour → seconds)
pub fn voice_pending_key(server: &str) -> String {
    format!("insight_voice:{server}")
}

/// Takes a server's pending voice time and its set entry in one step
const TAKE_VOICE_SCRIPT: &str = r"
local pending = redis.call('HGETALL', KEYS[1])
redis.call('DEL', KEYS[1])
redis.call('SREM', KEYS[2], ARGV[1])
return pending
";

/// The Redis key of a day's authors
pub fn authors_key(server: &str, day: i64) -> String {
    format!("insight_authors:{server}:{day}")
}

/// Add authors to a day's HyperLogLog and return the estimate; None when
/// Redis did not answer (the row then keeps its last number)
pub async fn add_day_authors(server: &str, day: i64, authors: &[String]) -> Option<u32> {
    let key = authors_key(server, day);
    let mut connection = get_connection().await.ok()?.into_inner();
    let mut pipe = redis::pipe();
    pipe.cmd("PFADD").arg(&key);
    for author in authors {
        pipe.arg(author);
    }
    pipe.ignore()
        .cmd("EXPIRE")
        .arg(&key)
        .arg(AUTHORS_TTL_SECONDS)
        .ignore()
        .cmd("PFCOUNT")
        .arg(&key);
    pipe.query_async::<_, (u32,)>(&mut connection)
        .await
        .ok()
        .map(|(count,)| count)
}

/// The estimate of a day's authors, and the key dropped (the day is over)
async fn take_day_authors(server: &str, day: i64) -> Option<u32> {
    let key = authors_key(server, day);
    let mut connection = get_connection().await.ok()?.into_inner();
    let (exists, count): (bool, u32) = redis::pipe()
        .cmd("EXISTS")
        .arg(&key)
        .cmd("PFCOUNT")
        .arg(&key)
        .query_async(&mut connection)
        .await
        .ok()?;
    let _ = redis::cmd("DEL")
        .arg(&key)
        .query_async::<_, ()>(&mut connection)
        .await;
    exists.then_some(count)
}

/// The time zone a page counts in
pub enum InsightZone {
    Named(String, Zone),
    /// Whole hours east of UTC
    Fixed(i64),
}

impl InsightZone {
    /// A known IANA name wins; otherwise the offset (rounded to hours,
    /// within -12 and +14); otherwise UTC
    pub fn pick(name: Option<&str>, offset_minutes: Option<i32>) -> InsightZone {
        if let Some(name) = name {
            if let Some(zone) = Zone::load(name) {
                return InsightZone::Named(name.to_string(), zone);
            }
        }
        let minutes = offset_minutes.unwrap_or(0).clamp(-720, 840) as i64;
        InsightZone::Fixed(((minutes as f64) / 60.0).round() as i64)
    }

    pub fn label(&self) -> String {
        match self {
            InsightZone::Named(name, _) => name.clone(),
            InsightZone::Fixed(0) => "UTC".to_string(),
            InsightZone::Fixed(hours) => {
                format!("UTC{}{:02}:00", if *hours < 0 { '-' } else { '+' }, hours.abs())
            }
        }
    }

    /// The local day of the start of a UTC hour
    pub fn local_day(&self, hour: i64) -> i64 {
        let utc = hour * 3600;
        let local = match self {
            InsightZone::Named(_, zone) => zone.to_local(utc),
            InsightZone::Fixed(hours) => utc + hours * 3600,
        };
        local.div_euclid(86_400)
    }
}

impl ServerInsightDay {
    pub fn key(server: &str, day: i64) -> String {
        format!("{server}:{day}")
    }

    /// Add to a counter of a server, now. A failure is logged and never
    /// stops what counted it (a join, a leave). Voice time of a server that
    /// is gone is not counted (a call can outlast its server).
    pub async fn count(db: &Database, server: &str, counter: InsightCounter) {
        if matches!(counter, InsightCounter::VoiceSeconds(_)) && db.fetch_server(server).await.is_err() {
            return;
        }
        if let Err(error) = db
            .add_server_insight(server, hour_of(Timestamp::now_utc()), &counter)
            .await
        {
            log::warn!("Server insights: could not count {counter:?} for {server}: {error:?}");
        }
    }

    /// Roll the messages of one finished UTC hour into the day rows of the
    /// servers they were sent in. Returns the servers counted and the
    /// servers whose update failed (logged and skipped, so the hour
    /// completes; their messages of this hour are not counted).
    pub async fn roll_hour(db: &Database, hour: i64) -> Result<(u32, u32)> {
        let tallies = db
            .tally_messages_between(&first_id_of_hour(hour), &first_id_of_hour(hour + 1))
            .await?;
        if tallies.is_empty() {
            return Ok((0, 0));
        }

        // which server each channel belongs to (one read for all of them)
        let mut channel_ids: Vec<String> = tallies.iter().map(|t| t.channel.clone()).collect();
        channel_ids.sort();
        channel_ids.dedup();
        let channel_servers: HashMap<String, String> = existing_channels(db, &channel_ids)
            .await?
            .into_iter()
            .filter_map(|channel| {
                channel
                    .server()
                    .map(|server| (channel.id().to_string(), server.to_string()))
            })
            .collect();

        // servers that still exist (one read)
        let mut server_ids: Vec<String> = channel_servers.values().cloned().collect();
        server_ids.sort();
        server_ids.dedup();
        let live: HashSet<String> = existing_servers(db, &server_ids)
            .await?
            .into_iter()
            .map(|server| server.id)
            .collect();

        // bots do not count as active members (one read)
        let mut author_ids: Vec<String> = tallies.iter().filter_map(|t| t.author.clone()).collect();
        author_ids.sort();
        author_ids.dedup();
        let people: HashSet<String> = if author_ids.is_empty() {
            HashSet::new()
        } else {
            existing_users(db, &author_ids)
                .await?
                .into_iter()
                .filter(|user| user.bot.is_none())
                .map(|user| user.id)
                .collect()
        };

        let mut by_server: HashMap<String, (BTreeMap<String, u32>, HashSet<String>)> =
            HashMap::new();
        for tally in tallies {
            let Some(server) = channel_servers.get(&tally.channel) else {
                continue; // direct messages, groups, channels already gone
            };
            if !live.contains(server) {
                continue;
            }
            let (channels, authors) = by_server.entry(server.clone()).or_default();
            *channels.entry(tally.channel).or_default() += tally.count;
            if let Some(author) = tally.author.filter(|author| people.contains(author)) {
                authors.insert(author);
            }
        }

        let day = hour.div_euclid(24);
        let (mut counted, mut failed) = (0, 0);
        for (server, (channels, authors)) in by_server {
            let mut authors: Vec<String> = authors.into_iter().collect();
            authors.sort();
            let active = add_day_authors(&server, day, &authors).await;
            match db
                .add_server_insight_hour(&server, hour, &InsightHour { channels, active })
                .await
            {
                Ok(true) => counted += 1,
                Ok(false) => {}
                Err(error) => {
                    failed += 1;
                    log::error!("Server insights: hour {hour} of {server} not counted: {error:?}");
                }
            }
        }
        Ok((counted, failed))
    }

    /// Move the voice time that left seats noted in Redis into the rows of
    /// their servers (servers that are gone are dropped). Returns the
    /// servers counted.
    pub async fn flush_voice(db: &Database) -> Result<u32> {
        let Ok(connection) = get_connection().await else {
            return Err(create_error!(InternalError));
        };
        let mut connection = connection.into_inner();
        let servers: Vec<String> = redis::cmd("SMEMBERS")
            .arg(VOICE_PENDING_SERVERS)
            .query_async(&mut connection)
            .await
            .map_err(|_| create_error!(InternalError))?;
        let mut counted = 0;
        for server in servers {
            let pending: Vec<String> = redis::cmd("EVAL")
                .arg(TAKE_VOICE_SCRIPT)
                .arg(2)
                .arg(voice_pending_key(&server))
                .arg(VOICE_PENDING_SERVERS)
                .arg(&server)
                .query_async(&mut connection)
                .await
                .map_err(|_| create_error!(InternalError))?;
            if pending.is_empty() || db.fetch_server(&server).await.is_err() {
                continue;
            }
            for pair in pending.chunks(2) {
                let [hour, seconds] = pair else { continue };
                let (Ok(hour), Ok(seconds)) = (hour.parse::<i64>(), seconds.parse::<u32>()) else {
                    continue;
                };
                db.add_server_insight(&server, hour, &InsightCounter::VoiceSeconds(seconds))
                    .await?;
            }
            counted += 1;
        }
        Ok(counted)
    }

    /// Close the open days before `day`: their author estimate is final
    /// and the Redis key goes
    pub async fn close_days(db: &Database, day: i64) -> Result<u64> {
        let mut closed = 0;
        for (server, open_day) in db.fetch_open_server_insight_days(day, CLOSE_PER_PASS).await? {
            let active = take_day_authors(&server, open_day).await;
            db.close_server_insight_day(&server, open_day, active).await?;
            closed += 1;
        }
        Ok(closed)
    }

    /// One crond pass. Retention and closing days run first and each on
    /// its own, so a failing roll-up never holds them up; then the finished
    /// hours not rolled yet (at most `HOURS_PER_PASS`).
    pub async fn run_pass(db: &Database, now: Timestamp) -> InsightPassReport {
        Self::run_pass_with(db, now, |hour| Self::roll_hour(db, hour)).await
    }

    /// `run_pass` with the hour roll-up given (tests make it fail)
    pub async fn run_pass_with<F, Fut>(db: &Database, now: Timestamp, roll: F) -> InsightPassReport
    where
        F: Fn(i64) -> Fut,
        Fut: Future<Output = Result<(u32, u32)>>,
    {
        let mut report = InsightPassReport::default();
        let today = hour_of(now).div_euclid(24);

        // keep the longest range in any time zone (a local day can be a
        // UTC day either side)
        match db.delete_server_insights_before(today - RETENTION_DAYS - 1).await {
            Ok(deleted) => report.deleted = deleted,
            Err(error) => {
                report.failures += 1;
                log::error!("Server insights: retention failed: {error:?}");
            }
        }

        if let Err(error) = Self::flush_voice(db).await {
            report.failures += 1;
            log::error!("Server insights: voice time not moved: {error:?}");
        }

        let cursor = match db.fetch_server_insight_cursor().await {
            Ok(cursor) => cursor,
            Err(error) => {
                report.failures += 1;
                log::error!("Server insights: cursor not read: {error:?}");
                return report;
            }
        };
        let first_run = cursor.is_none();
        let mut next = match cursor {
            Some(done) => done + 1,
            None => last_rollable_hour(now) + 1 - FIRST_RUN_HOURS,
        };

        // every hour of the days before `next` is in
        match Self::close_days(db, next.div_euclid(24)).await {
            Ok(closed) => report.closed = closed,
            Err(error) => {
                report.failures += 1;
                log::error!("Server insights: closing days failed: {error:?}");
            }
        }

        let since = next;
        let last = (next + HOURS_PER_PASS - 1).min(last_rollable_hour(now));
        while next <= last {
            match roll(next).await {
                Ok((servers, failed)) => {
                    report.servers += servers;
                    report.failures += failed;
                }
                Err(error) => {
                    // the hour is not noted: the next pass tries it again
                    report.failures += 1;
                    log::error!("Server insights: hour {next} not rolled up: {error:?}");
                    break;
                }
            }
            if let Err(error) = db
                .set_server_insight_cursor(next, first_run.then_some(since))
                .await
            {
                report.failures += 1;
                log::error!("Server insights: cursor not saved: {error:?}");
                break;
            }
            report.hours += 1;
            next += 1;
        }
        report
    }

    /// Rows of a server for a page of `days` days ending today, read with
    /// one indexed find
    pub async fn fetch_page(
        db: &Database,
        server: &str,
        days: u32,
        zone: &InsightZone,
        now: Timestamp,
    ) -> Result<Vec<ServerInsightDay>> {
        let today = zone.local_day(hour_of(now));
        let first = today - days as i64 + 1;
        // the local days are within a UTC day either side
        db.fetch_server_insights(server, first - 1, today + 1).await
    }
}

/// The channels of these ids that still exist. One read; a driver that
/// answers NotFound for a missing id (the reference one) is asked one by one.
pub async fn existing_channels(db: &Database, ids: &[String]) -> Result<Vec<crate::Channel>> {
    match db.fetch_channels(ids).await {
        Err(error) if matches!(error.error_type, revolt_result::ErrorType::NotFound) => {
            let mut found = vec![];
            for id in ids {
                if let Ok(channel) = db.fetch_channel(id).await {
                    found.push(channel);
                }
            }
            Ok(found)
        }
        other => other,
    }
}

async fn existing_servers(db: &Database, ids: &[String]) -> Result<Vec<crate::Server>> {
    match db.fetch_servers(ids).await {
        Err(error) if matches!(error.error_type, revolt_result::ErrorType::NotFound) => {
            let mut found = vec![];
            for id in ids {
                if let Ok(server) = db.fetch_server(id).await {
                    found.push(server);
                }
            }
            Ok(found)
        }
        other => other,
    }
}

async fn existing_users(db: &Database, ids: &[String]) -> Result<Vec<crate::User>> {
    match db.fetch_users(ids).await {
        Err(error) if matches!(error.error_type, revolt_result::ErrorType::NotFound) => {
            let mut found = vec![];
            for id in ids {
                if let Ok(user) = db.fetch_user(id).await {
                    found.push(user);
                }
            }
            Ok(found)
        }
        other => other,
    }
}

/// The channel ids that have messages in these rows
pub fn insight_channels(stored: &[ServerInsightDay]) -> Vec<String> {
    let mut ids: Vec<String> = stored
        .iter()
        .flat_map(|row| row.messages.values().flat_map(|hour| hour.keys().cloned()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Everything of a page but the channel list, which the route filters by
/// what the viewer may see
pub struct InsightSummary {
    pub rows: Vec<v0::ServerInsightsDay>,
    pub totals: v0::ServerInsightsTotals,
    pub join_sources: v0::ServerInsightsJoinSources,
    /// Visible channel ids and their messages in the range, most first
    pub channels: Vec<(String, u32)>,
}

/// Count stored rows into the viewer's days. `members_now` is the server's
/// member count now; the counts of earlier days are worked back from it.
/// Messages count only in channels `visible` accepts, so a channel the
/// viewer can not see never shows up, not even in a daily total.
pub fn summarise_insights(
    stored: &[ServerInsightDay],
    days: u32,
    zone: &InsightZone,
    now: Timestamp,
    members_now: u32,
    visible: &dyn Fn(&str) -> bool,
) -> InsightSummary {
    let now_hour = hour_of(now);
    let today = zone.local_day(now_hour);
    let first = today - days as i64 + 1;
    let len = days as usize;
    let index = |day: i64| -> Option<usize> {
        (day >= first && day <= today).then(|| (day - first) as usize)
    };

    let mut messages = vec![0u32; len];
    let mut joins = vec![0u32; len];
    let mut leaves = vec![0u32; len];
    let mut voice = vec![0u64; len];
    let mut active = vec![0u32; len];
    let mut sources = v0::ServerInsightsJoinSources::default();
    let mut channels: HashMap<&str, u32> = HashMap::new();

    for doc in stored {
        let slot = |hour: &String| -> Option<usize> {
            let hour: i64 = hour.parse().ok().filter(|h| (0..24).contains(h))?;
            index(zone.local_day(doc.day * 24 + hour))
        };
        for (hour, per_channel) in &doc.messages {
            let Some(i) = slot(hour) else { continue };
            for (channel, n) in per_channel {
                if visible(channel) {
                    messages[i] = messages[i].saturating_add(*n);
                    let total = channels.entry(channel.as_str()).or_default();
                    *total = total.saturating_add(*n);
                }
            }
        }
        for (hour, n) in &doc.joins {
            if let Some(i) = slot(hour) {
                joins[i] = joins[i].saturating_add(*n);
            }
        }
        for (hour, n) in &doc.leaves {
            if let Some(i) = slot(hour) {
                leaves[i] = leaves[i].saturating_add(*n);
            }
        }
        for (hour, n) in &doc.voice {
            if let Some(i) = slot(hour) {
                voice[i] += *n as u64;
            }
        }

        // whole-day counts go to the viewer's day of the same date
        if let Some(i) = index(doc.day) {
            active[i] = doc.active;
            for (source, n) in &doc.sources {
                let slot = match source.as_str() {
                    "invite" => &mut sources.invite,
                    "discover" => &mut sources.discover,
                    "vanity" => &mut sources.vanity,
                    _ => &mut sources.other,
                };
                *slot = slot.saturating_add(*n);
            }
        }
    }

    // members at the end of each day, worked back from now
    let mut members = vec![0u32; len];
    let mut running = members_now as i64;
    for i in (0..len).rev() {
        members[i] = running.max(0) as u32;
        running = running - joins[i] as i64 + leaves[i] as i64;
    }

    let rows = (0..len)
        .map(|i| v0::ServerInsightsDay {
            day: day_label(first + i as i64),
            joins: joins[i],
            leaves: leaves[i],
            members: members[i],
            messages: messages[i],
            active_members: shown_count(active[i]),
            voice_minutes: shown_count(voice_minutes(voice[i])),
        })
        .collect();

    let active_total: u64 = active.iter().map(|n| *n as u64).sum();
    let totals = v0::ServerInsightsTotals {
        joins: joins.iter().fold(0u32, |a, n| a.saturating_add(*n)),
        leaves: leaves.iter().fold(0u32, |a, n| a.saturating_add(*n)),
        messages: messages.iter().fold(0u32, |a, n| a.saturating_add(*n)),
        voice_minutes: shown_count(voice_minutes(voice.iter().sum())),
        active_members: shown_count(((active_total as f64) / (len.max(1) as f64)).round() as u32),
    };

    let mut channels: Vec<(String, u32)> = channels
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(id, n)| (id.to_string(), n))
        .collect();
    channels.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    InsightSummary {
        rows,
        totals,
        join_sources: sources,
        channels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: i64, hour: i64) -> Timestamp {
        Timestamp::UNIX_EPOCH + iso8601_timestamp::Duration::seconds(day * 86_400 + hour * 3600)
    }

    fn row(server: &str, day: i64) -> ServerInsightDay {
        ServerInsightDay {
            id: ServerInsightDay::key(server, day),
            server: server.to_string(),
            day,
            ..Default::default()
        }
    }

    fn messages(doc: &mut ServerInsightDay, hour: &str, channel: &str, n: u32) {
        doc.messages
            .entry(hour.to_string())
            .or_default()
            .insert(channel.to_string(), n);
    }

    const ALL: &dyn Fn(&str) -> bool = &|_| true;

    #[test]
    fn hour_ids_bound_the_messages_of_that_hour() {
        let hour = 493_000;
        let inside = Ulid::from_parts((hour as u64) * 3_600_000 + 1_234, 99).to_string();
        let next = Ulid::from_parts((hour as u64 + 1) * 3_600_000, 0).to_string();
        assert!(first_id_of_hour(hour) <= inside);
        assert!(inside < first_id_of_hour(hour + 1));
        assert_eq!(first_id_of_hour(hour + 1), next);
        assert_eq!(hour_of(at(20_402, 13)), 20_402 * 24 + 13);
    }

    #[test]
    fn an_hour_waits_for_its_grace_period() {
        let end = at(100, 11); // the end of hour 10 of day 100
        let plus = |seconds: i64| end + iso8601_timestamp::Duration::seconds(seconds);
        assert_eq!(last_rollable_hour(plus(GRACE_SECONDS - 1)), 100 * 24 + 9);
        assert_eq!(last_rollable_hour(plus(GRACE_SECONDS)), 100 * 24 + 10);
    }

    #[test]
    fn small_counts_are_hidden() {
        assert_eq!(shown_count(0), Some(0));
        for n in 1..5 {
            assert_eq!(shown_count(n), None, "{n}");
        }
        assert_eq!(shown_count(5), Some(5));
    }

    #[test]
    fn voice_seconds_round_to_minutes() {
        assert_eq!(voice_minutes(29), 0);
        assert_eq!(voice_minutes(30), 1);
        assert_eq!(voice_minutes(89), 1);
        assert_eq!(voice_minutes(90), 2);
    }

    #[test]
    fn days_follow_the_viewers_offset() {
        // 23:00 UTC on day 100 is 01:00 of day 101 at UTC+2
        let mut doc = row("s", 100);
        messages(&mut doc, "23", "c", 7);
        messages(&mut doc, "10", "c", 3);
        doc.joins.insert("23".into(), 2);
        let now = at(101, 12);

        let utc = summarise_insights(&[doc.clone()], 7, &InsightZone::Fixed(0), now, 10, ALL);
        assert_eq!(utc.rows.len(), 7);
        assert_eq!(utc.rows[5].messages, 10);
        assert_eq!(utc.rows[6].messages, 0);

        let plus_two = summarise_insights(&[doc], 7, &InsightZone::Fixed(2), now, 10, ALL);
        assert_eq!(plus_two.rows[5].messages, 3);
        assert_eq!(plus_two.rows[6].messages, 7);
        assert_eq!(plus_two.rows[6].joins, 2);
        assert_eq!(plus_two.rows[6].day, day_label(101));
    }

    #[test]
    fn days_follow_daylight_saving() {
        // without the zone files (some test machines) there is nothing to check
        let Some(vienna) = Zone::load("Europe/Vienna") else { return };
        let zone = InsightZone::Named("Europe/Vienna".into(), vienna);
        // 2026-03-29 02:00 local: clocks go forward (UTC+1 -> UTC+2)
        let before = crate::util::tz::days_from_civil(2026, 3, 28);
        let after = before + 1;
        // 23:00 UTC the day before is 00:00 local (UTC+1)
        assert_eq!(zone.local_day(before * 24 + 23), after);
        // 22:00 UTC on the day after the change is 00:00 local (UTC+2)
        assert_eq!(zone.local_day(after * 24 + 22), after + 1);
        assert_eq!(zone.local_day(after * 24 + 21), after);
        // unknown names are not kept, so a typo can not fill memory
        assert!(Zone::load("No/Such_Zone_Insights").is_none());
    }

    #[test]
    fn members_are_worked_back_from_today() {
        let mut today = row("s", 50);
        today.joins.insert("3".into(), 4);
        today.leaves.insert("4".into(), 1);
        let mut yesterday = row("s", 49);
        yesterday.joins.insert("9".into(), 2);
        let summary = summarise_insights(
            &[today, yesterday],
            7,
            &InsightZone::Fixed(0),
            at(50, 20),
            20,
            ALL,
        );
        let members: Vec<u32> = summary.rows.iter().map(|r| r.members).collect();
        assert_eq!(members, vec![15, 15, 15, 15, 15, 17, 20]);
        assert_eq!(summary.totals.joins, 6);
        assert_eq!(summary.totals.leaves, 1);
    }

    #[test]
    fn hidden_channels_never_show_up() {
        let mut doc = row("s", 50);
        messages(&mut doc, "3", "open", 6);
        messages(&mut doc, "3", "secret", 40);
        let summary = summarise_insights(
            &[doc],
            7,
            &InsightZone::Fixed(0),
            at(50, 20),
            1,
            &|channel| channel != "secret",
        );
        assert_eq!(summary.rows[6].messages, 6);
        assert_eq!(summary.totals.messages, 6);
        assert_eq!(summary.channels, vec![("open".to_string(), 6)]);
    }

    #[test]
    fn active_members_and_voice_hide_small_numbers() {
        let mut few = row("s", 50);
        few.active = 3;
        few.voice.insert("2".into(), 4 * 60);
        let mut many = row("s", 49);
        many.active = 12;
        many.voice.insert("2".into(), 10 * 60);
        let summary =
            summarise_insights(&[few, many], 7, &InsightZone::Fixed(0), at(50, 20), 1, ALL);
        assert_eq!(summary.rows[6].active_members, None);
        assert_eq!(summary.rows[6].voice_minutes, None);
        assert_eq!(summary.rows[5].active_members, Some(12));
        assert_eq!(summary.rows[5].voice_minutes, Some(10));
        assert_eq!(summary.rows[4].active_members, Some(0));
        assert_eq!(summary.totals.voice_minutes, Some(14));
        // (3 + 12) / 7 days rounds to 2: hidden too
        assert_eq!(summary.totals.active_members, None);
    }

    #[test]
    fn channels_and_sources_add_up_over_the_range_only() {
        let mut inside = row("s", 50);
        messages(&mut inside, "1", "a", 3);
        messages(&mut inside, "1", "b", 9);
        inside.sources.insert("discover".into(), 2);
        let mut also = row("s", 49);
        messages(&mut also, "5", "a", 8);
        also.sources.insert("invite".into(), 1);
        let mut outside = row("s", 40);
        messages(&mut outside, "1", "c", 100);
        outside.sources.insert("vanity".into(), 5);
        let summary = summarise_insights(
            &[inside, also, outside],
            7,
            &InsightZone::Fixed(0),
            at(50, 1),
            1,
            ALL,
        );
        assert_eq!(summary.channels, vec![("a".to_string(), 11), ("b".to_string(), 9)]);
        assert_eq!(summary.join_sources.discover, 2);
        assert_eq!(summary.join_sources.invite, 1);
        assert_eq!(summary.join_sources.vanity, 0);
    }

    #[test]
    fn offsets_round_to_hours_and_stay_in_range() {
        assert_eq!(InsightZone::pick(None, Some(330)).label(), "UTC+06:00");
        assert_eq!(InsightZone::pick(None, Some(-300)).label(), "UTC-05:00");
        assert_eq!(InsightZone::pick(None, Some(5_000)).label(), "UTC+14:00");
        assert_eq!(InsightZone::pick(Some("No/Such_Zone"), None).label(), "UTC");
        assert_eq!(InsightZone::pick(Some("../../etc/passwd"), Some(60)).label(), "UTC+01:00");
    }
}
