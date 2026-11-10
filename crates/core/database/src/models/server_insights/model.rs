//! Server insights: daily counters of a server, kept per UTC day.
//!
//! One document per server and UTC day (`<server>:<day>`), with the counts of
//! each hour of that day, so a viewer's days can be counted in their own
//! time zone. How the counts get there, by write cost:
//!
//! - joins, leaves and voice minutes are rare next to messages, so delta and
//!   voice-ingress add one each with an `$inc` upsert as they happen;
//! - messages are never counted on the send path. crond rolls each finished
//!   hour up once: one aggregation over that hour's slice of the message id
//!   index (ids are ULIDs, so an hour is an id range), grouped by channel and
//!   author, then one update per server that had messages. An hour is folded
//!   in only if the document does not list it yet, so a crash and a second
//!   run never count it twice.
//! - active members: the day keeps its authors' ids while it is open and
//!   only their number once crond closes it; the ids never leave the
//!   database and are gone the day after.
//!
//! Reading a page is one indexed find on (server, day). Rows older than
//! `RETENTION_DAYS` (plus two days for time zones) are deleted by crond.

use std::collections::{BTreeMap, HashMap, HashSet};

use iso8601_timestamp::Timestamp;
use revolt_models::v0;
use revolt_result::Result;
use ulid::Ulid;

use crate::util::tz::{civil_from_days, Zone};
use crate::Database;

/// Days of rows kept (the longest range shown)
pub const RETENTION_DAYS: i64 = 90;
/// Ranges a page can show
pub const INSIGHT_RANGES: [u32; 3] = [7, 30, 90];
/// Per-channel counts below this are shown as "fewer than 5"
pub const SMALL_COUNT: u32 = 5;
/// Hours one crond pass rolls up at most (a long outage catches up over
/// several passes)
pub const HOURS_PER_PASS: i64 = 48;
/// Hours rolled up on the very first run (nothing counted before)
pub const FIRST_RUN_HOURS: i64 = 7 * 24;
/// Channels listed at most
pub const TOP_CHANNELS: usize = 10;

auto_derived!(
    /// One server's counts for one UTC day. Maps are keyed by the hour of
    /// the day ("0" to "23").
    #[derive(Default)]
    pub struct ServerInsightDay {
        /// `<server>:<day>`
        #[serde(rename = "_id")]
        pub id: String,
        /// Server id
        pub server: String,
        /// Days since 1970-01-01 (UTC)
        pub day: i64,
        /// Messages per hour
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub messages: BTreeMap<String, u32>,
        /// Joins per hour
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub joins: BTreeMap<String, u32>,
        /// Leaves (also kicks and bans) per hour
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub leaves: BTreeMap<String, u32>,
        /// Voice minutes per hour (counted at the hour someone leaves)
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub voice: BTreeMap<String, u32>,
        /// Joins by where they came from (`invite`, `discover`, ...)
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub sources: BTreeMap<String, u32>,
        /// Messages per channel id
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub channels: BTreeMap<String, u32>,
        /// Distinct authors of the day
        #[serde(default)]
        pub active: u32,
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
        VoiceMinutes(u32),
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
        pub messages: u32,
        pub channels: BTreeMap<String, u32>,
        pub authors: Vec<String>,
    }

    /// What a crond pass did
    #[derive(Default)]
    pub struct InsightPassReport {
        pub hours: u32,
        pub servers: u32,
        pub closed: u64,
        pub deleted: u64,
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
    (at.duration_since(Timestamp::UNIX_EPOCH).whole_seconds()).div_euclid(3600)
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

/// A per-channel count as shown: 1 to 4 become `None`
pub fn shown_count(count: u32) -> Option<u32> {
    if count > 0 && count < SMALL_COUNT {
        None
    } else {
        Some(count)
    }
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

    /// Add one to a counter of a server, now. A failure is logged and never
    /// stops what counted it (a join, a leave).
    pub async fn count(db: &Database, server: &str, counter: InsightCounter) {
        if let Err(error) = db
            .add_server_insight(server, hour_of(Timestamp::now_utc()), &counter)
            .await
        {
            log::warn!("Server insights: could not count {counter:?} for {server}: {error:?}");
        }
    }

    /// Roll the messages of one finished UTC hour into the day rows of the
    /// servers they were sent in. Returns how many servers got counts.
    pub async fn roll_hour(db: &Database, hour: i64) -> Result<u32> {
        let tallies = db
            .tally_messages_between(&first_id_of_hour(hour), &first_id_of_hour(hour + 1))
            .await?;
        if tallies.is_empty() {
            return Ok(0);
        }

        // which server each channel belongs to (one read for all of them)
        let mut channel_ids: Vec<String> = tallies.iter().map(|t| t.channel.clone()).collect();
        channel_ids.sort();
        channel_ids.dedup();
        let servers: HashMap<String, String> = db
            .fetch_channels(&channel_ids)
            .await?
            .into_iter()
            .filter_map(|channel| {
                channel
                    .server()
                    .map(|server| (channel.id().to_string(), server.to_string()))
            })
            .collect();

        let mut by_server: HashMap<String, (InsightHour, HashSet<String>)> = HashMap::new();
        for tally in tallies {
            let Some(server) = servers.get(&tally.channel) else {
                continue; // direct messages, groups, channels already gone
            };
            let (hour_counts, authors) = by_server.entry(server.clone()).or_default();
            hour_counts.messages += tally.count;
            *hour_counts.channels.entry(tally.channel).or_default() += tally.count;
            if let Some(author) = tally.author {
                authors.insert(author);
            }
        }

        let mut counted = 0;
        for (server, (mut hour_counts, authors)) in by_server {
            hour_counts.authors = authors.into_iter().collect();
            hour_counts.authors.sort();
            if db.add_server_insight_hour(&server, hour, &hour_counts).await? {
                counted += 1;
            }
        }
        Ok(counted)
    }

    /// One crond pass: roll up the finished hours not rolled yet (at most
    /// `HOURS_PER_PASS`), close the days before the next hour to roll, and
    /// delete rows past the retention.
    pub async fn run_pass(db: &Database, now: Timestamp) -> Result<InsightPassReport> {
        let mut report = InsightPassReport::default();
        let current = hour_of(now);
        let mut next = match db.fetch_server_insight_cursor().await? {
            Some(done) => done + 1,
            None => current - FIRST_RUN_HOURS,
        };
        let last = (next + HOURS_PER_PASS - 1).min(current - 1);
        while next <= last {
            report.servers += Self::roll_hour(db, next).await?;
            db.set_server_insight_cursor(next).await?;
            report.hours += 1;
            next += 1;
        }

        // every hour of the days before `next` is in
        report.closed = db.close_server_insight_days(next.div_euclid(24)).await?;
        // keep the longest range in any time zone (the local day can be a
        // UTC day either side)
        report.deleted = db
            .delete_server_insights_before(current.div_euclid(24) - RETENTION_DAYS - 1)
            .await?;
        Ok(report)
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

/// Everything of a page but the channel list, which the route filters by
/// what the viewer may see
pub struct InsightSummary {
    pub rows: Vec<v0::ServerInsightsDay>,
    pub join_sources: v0::ServerInsightsJoinSources,
    /// Channel ids and their messages in the range, most first
    pub channels: Vec<(String, u32)>,
}

/// Count stored rows into the viewer's days. `members_now` is the server's
/// member count now; the counts of earlier days are worked back from it.
pub fn summarise_insights(
    stored: &[ServerInsightDay],
    days: u32,
    zone: &InsightZone,
    now: Timestamp,
    members_now: u32,
) -> InsightSummary {
    let now_hour = hour_of(now);
    let today = zone.local_day(now_hour);
    let first = today - days as i64 + 1;
    let len = days as usize;
    let index = |day: i64| -> Option<usize> {
        (day >= first && day <= today).then(|| (day - first) as usize)
    };

    let mut rows: Vec<v0::ServerInsightsDay> = (0..len)
        .map(|i| v0::ServerInsightsDay {
            day: day_label(first + i as i64),
            joins: 0,
            leaves: 0,
            members: 0,
            messages: 0,
            active_members: 0,
            voice_minutes: 0,
        })
        .collect();
    let mut sources = v0::ServerInsightsJoinSources::default();
    let mut channels: HashMap<&str, u32> = HashMap::new();

    for doc in stored {
        let hourly = |map: &BTreeMap<String, u32>| -> Vec<(usize, u32)> {
            map.iter()
                .filter_map(|(hour, count)| {
                    let hour: i64 = hour.parse().ok().filter(|h| (0..24).contains(h))?;
                    index(zone.local_day(doc.day * 24 + hour)).map(|i| (i, *count))
                })
                .collect()
        };
        for (i, n) in hourly(&doc.messages) {
            rows[i].messages = rows[i].messages.saturating_add(n);
        }
        for (i, n) in hourly(&doc.joins) {
            rows[i].joins = rows[i].joins.saturating_add(n);
        }
        for (i, n) in hourly(&doc.leaves) {
            rows[i].leaves = rows[i].leaves.saturating_add(n);
        }
        for (i, n) in hourly(&doc.voice) {
            rows[i].voice_minutes = rows[i].voice_minutes.saturating_add(n);
        }

        // whole-day counts go to the viewer's day of the same date
        if let Some(i) = index(doc.day) {
            rows[i].active_members = doc.active;
            for (source, n) in &doc.sources {
                let slot = match source.as_str() {
                    "invite" => &mut sources.invite,
                    "discover" => &mut sources.discover,
                    "vanity" => &mut sources.vanity,
                    _ => &mut sources.other,
                };
                *slot = slot.saturating_add(*n);
            }
            for (channel, n) in &doc.channels {
                let slot = channels.entry(channel.as_str()).or_default();
                *slot = slot.saturating_add(*n);
            }
        }
    }

    // members at the end of each day, worked back from now
    let mut members = members_now as i64;
    for row in rows.iter_mut().rev() {
        row.members = members.max(0) as u32;
        members = members - row.joins as i64 + row.leaves as i64;
    }

    let mut channels: Vec<(String, u32)> = channels
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(id, n)| (id.to_string(), n))
        .collect();
    channels.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    InsightSummary {
        rows,
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
    fn small_channel_counts_are_hidden() {
        assert_eq!(shown_count(0), Some(0));
        for n in 1..5 {
            assert_eq!(shown_count(n), None, "{n}");
        }
        assert_eq!(shown_count(5), Some(5));
    }

    #[test]
    fn days_follow_the_viewers_offset() {
        // 23:00 UTC on day 100 is 01:00 of day 101 at UTC+2
        let mut doc = row("s", 100);
        doc.messages.insert("23".into(), 7);
        doc.messages.insert("10".into(), 3);
        doc.joins.insert("23".into(), 2);
        let now = at(101, 12);

        let utc = summarise_insights(&[doc.clone()], 7, &InsightZone::Fixed(0), now, 10);
        assert_eq!(utc.rows.len(), 7);
        assert_eq!(utc.rows[5].messages, 10);
        assert_eq!(utc.rows[6].messages, 0);

        let plus_two = summarise_insights(&[doc], 7, &InsightZone::Fixed(2), now, 10);
        assert_eq!(plus_two.rows[5].messages, 3);
        assert_eq!(plus_two.rows[6].messages, 7);
        assert_eq!(plus_two.rows[6].joins, 2);
        assert_eq!(plus_two.rows[6].day, day_label(101));
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
        );
        let members: Vec<u32> = summary.rows.iter().map(|r| r.members).collect();
        assert_eq!(members, vec![15, 15, 15, 15, 15, 17, 20]);
    }

    #[test]
    fn channels_and_sources_add_up_over_the_range_only() {
        let mut inside = row("s", 50);
        inside.channels.insert("a".into(), 3);
        inside.channels.insert("b".into(), 9);
        inside.sources.insert("discover".into(), 2);
        inside.active = 4;
        let mut also = row("s", 49);
        also.channels.insert("a".into(), 8);
        also.sources.insert("invite".into(), 1);
        let mut outside = row("s", 40);
        outside.channels.insert("c".into(), 100);
        outside.sources.insert("vanity".into(), 5);
        let summary = summarise_insights(
            &[inside, also, outside],
            7,
            &InsightZone::Fixed(0),
            at(50, 1),
            1,
        );
        assert_eq!(summary.channels, vec![("a".to_string(), 11), ("b".to_string(), 9)]);
        assert_eq!(summary.join_sources.discover, 2);
        assert_eq!(summary.join_sources.invite, 1);
        assert_eq!(summary.join_sources.vanity, 0);
        assert_eq!(summary.rows[6].active_members, 4);
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
