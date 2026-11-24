//! Weekly activity of large servers, for Partner. crond counts each
//! finished week once, from metadata only:
//!
//! - contributors: people (not bots, not deleted) who wrote at least one
//!   message in the server's channels that week (author ids of the week's
//!   message id range; never content, system messages and webhooks left
//!   out);
//! - active members: contributors plus people whose read position in one of
//!   the server's channels moved to a message of that week;
//! - joins: the insights join counter of the week's days (at least the
//!   members who joined that week and are still there);
//! - retained: of the people who joined that week, how many were still
//!   members when the week after their week had ended (filled in once).
//!
//! The row keeps the numbers only; the ids are counted in memory and
//! dropped. Rows older than [`ACTIVITY_WEEKS_KEPT`] weeks are deleted.

use std::collections::{BTreeSet, HashMap};

use revolt_config::ApiBadgePrograms;
use revolt_models::v0::UserFlags;
use revolt_result::{ErrorType, Result};
use ulid::Ulid;

use crate::{epoch_ms, Database, Server, ServerActivityWeek};

pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;
pub const WEEK_MS: i64 = 7 * DAY_MS;
/// Monday 1970-01-05 00:00 UTC: week 0 starts here
const FIRST_MONDAY_MS: i64 = 4 * DAY_MS;
/// Weeks of activity rows kept
pub const ACTIVITY_WEEKS_KEPT: i64 = 26;
/// Members read per page
const PAGE: i64 = 1000;
/// Ids per user read
const USER_BATCH: usize = 1000;

/// The week (since Monday 1970-01-05, UTC) a moment falls in
pub fn week_of(ms: i64) -> i64 {
    (ms - FIRST_MONDAY_MS).div_euclid(WEEK_MS)
}

/// When a week starts (Monday 00:00 UTC)
pub fn week_start(week: i64) -> i64 {
    FIRST_MONDAY_MS + week * WEEK_MS
}

/// The smallest message id made at `ms`
fn id_floor(ms: i64) -> String {
    Ulid::from_parts(ms.max(0) as u64, 0).to_string()
}

/// How many of these ids are people (not bots, not deleted accounts)
async fn count_people(db: &Database, ids: &BTreeSet<String>) -> Result<u32> {
    let ids: Vec<String> = ids.iter().cloned().collect();
    let mut people = 0u32;
    for chunk in ids.chunks(USER_BATCH) {
        people += db
            .fetch_users(chunk)
            .await?
            .iter()
            .filter(|user| user.bot.is_none() && user.flags.unwrap_or_default() & UserFlags::Deleted as i32 == 0)
            .count() as u32;
    }
    Ok(people)
}

/// Current members who joined in each of these weeks
async fn joined_in(db: &Database, server_id: &str, weeks: &[i64]) -> Result<HashMap<i64, u32>> {
    let mut counts: HashMap<i64, u32> = weeks.iter().map(|w| (*w, 0)).collect();
    let mut after: Option<String> = None;
    loop {
        let page = db.fetch_members_page(server_id, after.as_deref(), PAGE).await?;
        let Some(last) = page.last() else { break };
        after = Some(last.id.user.clone());
        for member in &page {
            if let Some(count) = counts.get_mut(&week_of(epoch_ms(member.joined_at))) {
                *count += 1;
            }
        }
        if (page.len() as i64) < PAGE {
            break;
        }
    }
    Ok(counts)
}

/// Joins counted by insights over the days of a week
async fn insight_joins(db: &Database, server_id: &str, week: i64) -> Result<u32> {
    let first = week_start(week).div_euclid(DAY_MS);
    let rows = db.fetch_server_insights(server_id, first, first + 6).await?;
    Ok(rows.iter().map(|row| row.joins.values().sum::<u32>()).sum())
}

/// Count one finished week of one server
pub async fn measure_server_week(db: &Database, server: &Server, week: i64, joined: u32, now: i64) -> Result<ServerActivityWeek> {
    let (from, to) = (id_floor(week_start(week)), id_floor(week_start(week + 1)));
    let authors: BTreeSet<String> = db.fetch_message_author_ids(&server.channels, &from, &to).await?.into_iter().collect();
    let readers = db.fetch_channel_reader_ids(&server.channels, &from, &to).await?;
    let mut active = authors.clone();
    active.extend(readers);
    Ok(ServerActivityWeek {
        id: format!("{}:{week}", server.id),
        server_id: server.id.clone(),
        week,
        active: count_people(db, &active).await?,
        contributors: count_people(db, &authors).await?,
        joins: insight_joins(db, &server.id, week).await?.max(joined),
        retained: retention_ready(week, now).then_some(joined),
        computed_at: now,
    })
}

/// Whether the people who joined in `week` have had their first week
fn retention_ready(week: i64, now: i64) -> bool {
    now >= week_start(week + 2)
}

/// What one pass did
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ActivityPassReport {
    pub servers: usize,
    pub weeks: usize,
    pub retention: usize,
    pub failures: usize,
}

/// Count the finished weeks of every large server that are missing, fill in
/// retention once it is ready, and delete old rows. Safe to run again.
pub async fn activity_pass(db: &Database, programs: &ApiBadgePrograms, now: i64) -> Result<ActivityPassReport> {
    let mut report = ActivityPassReport::default();
    let current = week_of(now);
    let window: Vec<i64> = (current - programs.partner_weeks.max(1) as i64..current).collect();
    for (server_id, _) in db.fetch_server_member_totals(programs.metrics_min_members).await? {
        report.servers += 1;
        match measure_server(db, &server_id, &window, now).await {
            Ok((weeks, retention)) => {
                report.weeks += weeks;
                report.retention += retention;
            }
            Err(error) => {
                log::warn!("activity of server {server_id}: {:?}", error.error_type);
                report.failures += 1;
            }
        }
    }
    db.delete_server_activity_weeks_before(current - ACTIVITY_WEEKS_KEPT).await?;
    Ok(report)
}

/// Missing weeks and retention of one server: (weeks counted, retention
/// filled)
pub async fn measure_server(db: &Database, server_id: &str, window: &[i64], now: i64) -> Result<(usize, usize)> {
    let Some(first) = window.first() else { return Ok((0, 0)) };
    let server = match db.fetch_server(server_id).await {
        Ok(server) => server,
        Err(error) if matches!(error.error_type, ErrorType::NotFound | ErrorType::UnknownServer) => return Ok((0, 0)),
        Err(error) => return Err(error),
    };
    let existing = db.fetch_server_activity_weeks(server_id, *first).await?;
    let missing: Vec<i64> = window.iter().copied().filter(|w| !existing.iter().any(|row| row.week == *w)).collect();
    let unretained: Vec<ServerActivityWeek> = existing
        .into_iter()
        .filter(|row| row.retained.is_none() && retention_ready(row.week, now))
        .collect();
    if missing.is_empty() && unretained.is_empty() {
        return Ok((0, 0));
    }
    let weeks: Vec<i64> = missing.iter().copied().chain(unretained.iter().map(|row| row.week)).collect();
    let joined = joined_in(db, server_id, &weeks).await?;
    for week in &missing {
        let row = measure_server_week(db, &server, *week, joined[week], now).await?;
        db.save_server_activity_week(&row).await?;
    }
    for mut row in unretained.iter().cloned() {
        row.retained = Some(joined[&row.week]);
        row.joins = row.joins.max(joined[&row.week]);
        db.save_server_activity_week(&row).await?;
    }
    Ok((missing.len(), unretained.len()))
}

/// Partner's activity figures over the last finished weeks
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartnerActivity {
    /// Finished weeks with a row (of `partner_weeks`)
    pub weeks: u32,
    pub avg_active: u32,
    pub avg_contributors: u32,
    /// Per cent of new members still there after their first week; `None`
    /// when fewer than `partner_retention_min_joins` joined
    pub retention_percent: Option<u32>,
}

/// The averages Partner needs, from the stored weeks
pub fn partner_activity(rows: &[ServerActivityWeek], programs: &ApiBadgePrograms, now: i64) -> PartnerActivity {
    let current = week_of(now);
    let span = programs.partner_weeks.max(1) as i64;
    let rows: Vec<&ServerActivityWeek> = rows.iter().filter(|r| r.week >= current - span && r.week < current).collect();
    let total = |f: fn(&ServerActivityWeek) -> u32| rows.iter().map(|r| f(r) as u64).sum::<u64>();
    let (joins, retained) = rows
        .iter()
        .filter_map(|r| r.retained.map(|kept| (r.joins as u64, kept as u64)))
        .fold((0u64, 0u64), |(j, k), (rj, rk)| (j + rj, k + rk));
    PartnerActivity {
        weeks: rows.len() as u32,
        avg_active: (total(|r| r.active) / span as u64) as u32,
        avg_contributors: (total(|r| r.contributors) / span as u64) as u32,
        retention_percent: (joins > 0 && joins >= programs.partner_retention_min_joins as u64)
            .then(|| (retained * 100 / joins).min(100) as u32),
    }
}
