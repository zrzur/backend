//! The daily volunteer job (crond, 00:30 UTC): count what each volunteer
//! did, apply the gentle monthly rule, follow roles taken away by hand, tell
//! the staff about trials that ended, and delete what is past its time.
//!
//! Counts only: how many messages, replies to other people, moderation
//! actions, reviews and ticket answers; never content, never channel names.
//! Everything here is safe to run twice: a counted day is written whole
//! (upsert), and every reminder, trial notice and status change of a month
//! leaves a fixed history word (`reminded_202611`, `inactive_202611`,
//! `active_202611`, `trial_end_notified`) that is checked before anything is
//! sent or changed again. A month is judged only when every day of it was
//! counted. While `volunteers.enabled` is off only the retention runs.
//!
//! The DMs go through [`TeamMessenger`], so the job is tested without a
//! message bus; crond passes [`OfficialMessenger`] (the official account).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use revolt_config::FeaturesTeamApplications;
use revolt_models::v0::MessageSort;
use revolt_result::{ErrorType, Result};
use ulid::Ulid;

use crate::{
    send_official_message, util::team_texts, AuditLogQuery, Channel, Database, Message,
    MessageFilter, MessageQuery, MessageTimePeriod, OfficialAccount, PartialMember,
    RemovalIntention, TeamActivityCount, TeamActivityDay, TeamHistory, TeamMember,
    TeamMemberFilter, TeamMemberStatus, AMQP,
};

/// Milliseconds of a day
pub const DAY_MS: i64 = 86_400_000;
/// The actor of what the job does by itself
pub const SYSTEM: &str = "system";
/// Days counted at most when the job missed some (yesterday and six before)
pub const CATCH_UP_DAYS: i32 = 7;
/// The monthly part runs on the first days of a month (the 1st, or later
/// when crond was down then); its history words keep it to once a month
pub const MONTHLY_DAYS: i32 = 7;
/// Activity days are kept at least this long, whatever the setting says:
/// the monthly rule reads the whole month before
pub const MIN_ACTIVITY_KEEP_DAYS: i64 = 62;
/// The job runs every day at this minute of the UTC day
const RUN_AT_MS: i64 = 30 * 60_000;
/// Rows read per query, and pages read at most per channel and day
const PAGE: i64 = 100;
const MAX_PAGES: usize = 100;
/// Audit log actions that count as moderation
const MODERATION: &[&str] = &[
    "MessageDelete",
    "MessageBulkDelete",
    "BanCreate",
    "BanDelete",
    "MemberKick",
    "MemberEdit",
];

// ---- days (yyyymmdd, UTC) ----

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The UTC day (yyyymmdd) a moment (milliseconds since the epoch) falls in
pub fn day_of(ms: i64) -> i32 {
    let (year, month, day) = civil_from_days(ms.div_euclid(DAY_MS));
    (year * 10_000 + month * 100 + day) as i32
}

/// The first millisecond of a UTC day (yyyymmdd)
pub fn day_start(day: i32) -> i64 {
    let day = i64::from(day);
    days_from_civil(day / 10_000, (day / 100) % 100, day % 100) * DAY_MS
}

/// The day `n` days after (or before, negative) `day`
pub fn add_days(day: i32, n: i64) -> i32 {
    day_of(day_start(day) + n * DAY_MS)
}

/// First millisecond of a month (yyyymm) and the first of the month after
pub fn month_range(month: i32) -> (i64, i64) {
    let next = if month % 100 >= 12 {
        (month / 100 + 1) * 100 + 1
    } else {
        month + 1
    };
    (day_start(month * 100 + 1), day_start(next * 100 + 1))
}

/// The month (yyyymm) before `month`
pub fn previous_month(month: i32) -> i32 {
    if month % 100 <= 1 {
        (month / 100 - 1) * 100 + 12
    } else {
        month - 1
    }
}

/// Milliseconds from `now` until the next run (00:30 UTC)
pub fn until_next_run(now: i64) -> i64 {
    let into_day = now.rem_euclid(DAY_MS);
    if into_day < RUN_AT_MS {
        RUN_AT_MS - into_day
    } else {
        DAY_MS - into_day + RUN_AT_MS
    }
}

fn days_ms(days: i64) -> i64 {
    days.clamp(0, 3650) * DAY_MS
}

/// The largest id made before `ms`: `> id_after(ms)` is "from `ms` on"
fn id_after(ms: i64) -> String {
    Ulid::from_parts((ms - 1).max(0) as u64, u128::MAX).to_string()
}

/// The smallest id made at `ms`: `< id_before(ms)` is "before `ms`"
fn id_before(ms: i64) -> String {
    Ulid::from_parts(ms.max(0) as u64, 0).to_string()
}

// ---- counting one day ----

/// Everything a person did in the current or any month, all five kinds of
/// count together (one contribution each)
pub fn month_total(days: &[TeamActivityDay]) -> u32 {
    days.iter()
        .flat_map(|day| day.counts.iter())
        .fold(0_u32, |total, count| total.saturating_add(contributions(count)))
}

/// The contributions of one count: the sum of its five kinds
pub fn contributions(count: &TeamActivityCount) -> u32 {
    count
        .messages
        .saturating_add(count.replies)
        .saturating_add(count.moderation)
        .saturating_add(count.reviews)
        .saturating_add(count.tickets)
}

fn empty_count(server: &str) -> TeamActivityCount {
    TeamActivityCount {
        server: server.to_string(),
        messages: 0,
        replies: 0,
        moderation: 0,
        reviews: 0,
        tickets: 0,
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Messages by `user` in one channel with ids in `[from, to)`, read through
/// the `{channel, _id}` index a page at a time
async fn messages_in(
    db: &Database,
    channel: &str,
    user: &str,
    from: i64,
    to: i64,
) -> Result<Vec<Message>> {
    let before = id_before(to);
    let mut after = id_after(from);
    let mut found = Vec::new();
    for _ in 0..MAX_PAGES {
        let page = db
            .fetch_messages(MessageQuery {
                limit: Some(PAGE),
                filter: MessageFilter {
                    channel: Some(channel.to_string()),
                    author: Some(user.to_string()),
                    ..Default::default()
                },
                time_period: MessageTimePeriod::Absolute {
                    before: Some(before.clone()),
                    after: Some(after.clone()),
                    sort: Some(MessageSort::Oldest),
                },
            })
            .await?;
        let full = page.len() as i64 >= PAGE;
        match page.last() {
            Some(last) => after = last.id.clone(),
            None => break,
        }
        found.extend(page);
        if !full {
            break;
        }
    }
    Ok(found)
}

/// How many of `messages` answer a message by someone else
async fn replies_to_others(db: &Database, user: &str, messages: &[Message]) -> u32 {
    let mut authors: HashMap<String, Option<String>> = HashMap::new();
    let mut count = 0;
    for message in messages {
        let Some(replies) = &message.replies else {
            continue;
        };
        for id in replies {
            if !authors.contains_key(id) {
                let author = db.fetch_message(id).await.ok().map(|m| m.author);
                authors.insert(id.clone(), author);
            }
            if authors[id].as_deref().is_some_and(|author| author != user) {
                count += 1;
                break;
            }
        }
    }
    count
}

/// Moderation actions by `user` in the server's audit log in `[from, to)`;
/// what a person did to themselves does not count
async fn moderation_in(db: &Database, server: &str, user: &str, from: i64, to: i64) -> Result<u32> {
    let floor = id_after(from);
    let mut before = id_before(to);
    let mut count = 0_u32;
    for _ in 0..MAX_PAGES {
        let page = db
            .get_server_audit_logs(
                server,
                AuditLogQuery {
                    user: Some(user.to_string()),
                    target: None,
                    r#type: Some(MODERATION.iter().map(|kind| kind.to_string()).collect()),
                    before: Some(before.clone()),
                    after: Some(floor.clone()),
                    limit: PAGE,
                },
            )
            .await?;
        let full = page.len() as i64 >= PAGE;
        let fresh: Vec<_> = page
            .into_iter()
            .filter(|entry| entry.id < before && entry.id > floor)
            .collect();
        let Some(oldest) = fresh.iter().map(|entry| entry.id.clone()).min() else {
            break;
        };
        count = count.saturating_add(to_u32(
            fresh
                .iter()
                .filter(|entry| entry.target.as_deref() != Some(user))
                .count(),
        ));
        if !full {
            break;
        }
        before = oldest;
    }
    Ok(count)
}

/// The channels of a server that had a message since `from`: threads of a
/// work channel, and tickets the person did not open themselves
async fn busy_channels(
    db: &Database,
    server: &str,
    user: &str,
    work: &BTreeSet<String>,
    from: i64,
) -> Result<(Vec<String>, Vec<String>)> {
    let floor = id_after(from);
    let busy = |last: &Option<String>| last.as_ref().is_some_and(|last| last > &floor);

    // threads are not in the server's channel list: read them per work
    // channel, active and archived (an archived thread may have had a
    // message that day before it was archived)
    let mut threads = Vec::new();
    for parent in work {
        for archived in [false, true] {
            let mut before: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let page = db
                    .fetch_threads(parent, archived, before.as_deref(), PAGE)
                    .await?;
                let full = page.len() as i64 >= PAGE;
                let Some(last) = page.last().map(|thread| thread.id().to_string()) else {
                    break;
                };
                for thread in page {
                    if let Channel::TextChannel {
                        id,
                        last_message_id,
                        ..
                    } = thread
                    {
                        if busy(&last_message_id) && !work.contains(&id) && !threads.contains(&id)
                        {
                            threads.push(id);
                        }
                    }
                }
                if !full {
                    break;
                }
                before = Some(last);
            }
        }
    }

    let server = match db.fetch_server(server).await {
        Ok(server) => server,
        Err(error) if matches!(error.error_type, ErrorType::NotFound) => {
            return Ok((threads, vec![]))
        }
        Err(error) => return Err(error),
    };
    let mut tickets = Vec::new();
    for channel in db.fetch_channels(&server.channels).await? {
        let Channel::TextChannel {
            id,
            last_message_id,
            ticket: Some(ticket),
            ..
        } = channel
        else {
            continue;
        };
        if busy(&last_message_id) && !work.contains(&id) && ticket.opener != user {
            tickets.push(id);
        }
    }
    Ok((threads, tickets))
}

/// What `user` did on one UTC `day` (yyyymmdd), one count per official
/// server of their current memberships, by server id:
/// - `messages`: their messages in the role's work channels (and threads
///   of those channels);
/// - `replies`: those of them that answer a message by someone else;
/// - `moderation`: moderation entries by them in the server's audit log;
/// - `reviews`: applications they last decided or interviewed that day,
///   credited to the desk server;
/// - `tickets`: their messages in the server's tickets they did not open.
pub async fn count_day(
    db: &Database,
    settings: &FeaturesTeamApplications,
    member_user: &str,
    day: i32,
) -> Result<Vec<TeamActivityCount>> {
    let from = day_start(day);
    let to = from + DAY_MS;

    let mut servers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for member in db.fetch_team_members_of(member_user).await? {
        if !member.is_current() || member.server.trim().is_empty() {
            continue;
        }
        let work = servers.entry(member.server.clone()).or_default();
        if let Some(role) = settings.role(&member.role) {
            work.extend(
                role.work_channels
                    .iter()
                    .map(|id| id.trim().to_string())
                    .filter(|id| !id.is_empty()),
            );
        }
    }

    let mut counts = Vec::new();
    for (server, work) in &servers {
        let mut count = empty_count(server);
        let (threads, tickets) = busy_channels(db, server, member_user, work, from).await?;
        for channel in work.iter().chain(threads.iter()) {
            let found = messages_in(db, channel, member_user, from, to).await?;
            count.messages = count.messages.saturating_add(to_u32(found.len()));
            count.replies = count
                .replies
                .saturating_add(replies_to_others(db, member_user, &found).await);
        }
        for channel in &tickets {
            let found = messages_in(db, channel, member_user, from, to).await?;
            count.tickets = count.tickets.saturating_add(to_u32(found.len()));
        }
        count.moderation = moderation_in(db, server, member_user, from, to).await?;
        counts.push(count);
    }

    let desk = settings.server.trim();
    if !desk.is_empty() {
        let reviews = db
            .fetch_team_applications_acted(from, to - 1)
            .await?
            .iter()
            .filter(|application| {
                application.acted_by.as_deref() == Some(member_user)
                    || application.interviewer.as_deref() == Some(member_user)
            })
            .count();
        if reviews > 0 {
            match counts.iter_mut().find(|count| count.server == desk) {
                Some(count) => count.reviews = to_u32(reviews),
                None => {
                    let mut count = empty_count(desk);
                    count.reviews = to_u32(reviews);
                    counts.push(count);
                    counts.sort_by(|a, b| a.server.cmp(&b.server));
                }
            }
        }
    }
    Ok(counts)
}

/// Count one day and write it whole: a day counted again replaces the old
/// row, it never adds to it
pub async fn store_day(
    db: &Database,
    settings: &FeaturesTeamApplications,
    user_id: &str,
    day: i32,
) -> Result<TeamActivityDay> {
    let counts = count_day(db, settings, user_id, day).await?;
    let row = TeamActivityDay {
        id: format!("{user_id}:{day}"),
        user_id: user_id.to_string(),
        day,
        counts,
    };
    db.upsert_team_activity_day(&row).await?;
    Ok(row)
}

// ---- the monthly rule ----

/// What the month means for one membership
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonthOutcome {
    /// Nothing to do
    Ok,
    /// A friendly reminder (once per person and month)
    Remind,
    /// The second low month in a row: shown to the owner as inactive
    MarkInactive,
    /// Inactive, but the month reached the minimum again
    BackToActive,
}

/// The gentle rule for one membership and one finished month:
/// - away during any part of the month: nothing (never a low month);
/// - below the minimum: a reminder; the second low month in a row marks a
///   non-trial volunteer inactive (a trial is never marked, it gets the
///   reminder); one already inactive gets nothing more;
/// - at or above the minimum: an inactive volunteer is active again.
pub fn month_rule(
    member: &TeamMember,
    month_total: u32,
    minimum: i64,
    away_covers_month: bool,
) -> MonthOutcome {
    if member.status == TeamMemberStatus::Left || away_covers_month {
        return MonthOutcome::Ok;
    }
    let low = i64::from(month_total) < minimum.max(0);
    match (low, member.status) {
        (false, TeamMemberStatus::Inactive) => MonthOutcome::BackToActive,
        (false, _) => MonthOutcome::Ok,
        (true, TeamMemberStatus::Inactive) => MonthOutcome::Ok,
        (true, TeamMemberStatus::Trial) => MonthOutcome::Remind,
        (true, _) if member.low_months >= 1 => MonthOutcome::MarkInactive,
        (true, _) => MonthOutcome::Remind,
    }
}

fn is_away_start(action: &str) -> bool {
    action == "away" || action == "away_from_inactive"
}

fn ends_away(action: &str) -> bool {
    matches!(action, "back" | "left" | "removed" | "role_removed" | "rejoined")
}

/// Whether the membership was away during any part of `[start, end)`. The
/// periods come from the history (from `away` / `away_from_inactive` to
/// `back` or the end of the membership); the open one lasts until
/// `away_until`, also when that date passed in the middle of the month.
pub fn away_covers(member: &TeamMember, start: i64, end: i64) -> bool {
    let history = &member.history;
    for (index, entry) in history.iter().enumerate() {
        if !is_away_start(&entry.action) {
            continue;
        }
        let closed = history[index + 1..]
            .iter()
            .find(|later| ends_away(&later.action) || is_away_start(&later.action))
            .map(|later| later.at);
        let until = match closed {
            Some(at) => at,
            None if member.status == TeamMemberStatus::Away => {
                member.away_until.unwrap_or(i64::MAX)
            }
            None => entry.at,
        };
        if entry.at < end && until >= start {
            return true;
        }
    }
    // away without a history entry (an older record)
    member.status == TeamMemberStatus::Away
        && member.away_until.is_some_and(|until| until >= start)
}

fn history(action: String) -> TeamHistory {
    TeamHistory {
        at: crate::staff_now(),
        actor: SYSTEM.to_string(),
        action,
    }
}

fn has_action(member: &TeamMember, action: &str) -> bool {
    member.history.iter().any(|entry| entry.action == action)
}

// ---- messages ----

/// Who sends the job's DMs
#[async_trait]
pub trait TeamMessenger: Sync + Send {
    /// Best effort: a failure is logged by the sender, nothing is retried.
    /// Returns whether the message went out.
    async fn send(&self, user_id: &str, content: String) -> bool;
}

/// DMs from the official Zrzur account
pub struct OfficialMessenger<'a> {
    pub db: &'a Database,
    pub amqp: Option<&'a AMQP>,
}

#[async_trait]
impl TeamMessenger for OfficialMessenger<'_> {
    async fn send(&self, user_id: &str, content: String) -> bool {
        match send_official_message(self.db, self.amqp, OfficialAccount::Zrzur, user_id, content)
            .await
        {
            Ok(_) => true,
            Err(error) => {
                log::warn!("team volunteer message not sent: {:?}", error.error_type);
                false
            }
        }
    }
}

/// The staff accounts who look after the volunteers: the holders of the staff
/// permission `TeamManage` right now (the owner role only, by default).
async fn team_managers(db: &Database, now: i64) -> Result<Vec<String>> {
    Ok(db
        .fetch_staff_grants()
        .await?
        .into_iter()
        .filter(|grant| grant.effective(now).contains(&crate::StaffPermission::TeamManage))
        .map(|grant| grant.id)
        .collect())
}

// ---- ending a membership ----

/// Whether `person` holds a reviewer role right now: the desk's common
/// reviewer role in the desk server, or a team role's own reviewer role in
/// that role's server. Read from the database; a lookup that fails is an
/// error, never an answer: the caller decides (see [`grants_reviewer`]).
pub async fn holds_reviewer_role(
    db: &Database,
    settings: &FeaturesTeamApplications,
    person: &str,
) -> Result<bool> {
    let mut places: Vec<(&str, &str)> = vec![(settings.server.as_str(), settings.reviewer_role.as_str())];
    places.extend(
        settings
            .roles
            .iter()
            .map(|role| (role.server.as_str(), role.reviewer_role.as_str())),
    );
    for (server, role) in places {
        if server.trim().is_empty() || role.trim().is_empty() {
            continue;
        }
        match db.fetch_member(server, person).await {
            Ok(member) if member.roles.iter().any(|held| held == role) => return Ok(true),
            Ok(_) => {}
            Err(error) if matches!(error.error_type, ErrorType::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// What a reviewer check lets a person do: only a sure yes grants anything
/// (fails closed). Keeping data is the other way round: an identity is not
/// treated as unheld on a failed lookup (see `out_of_reach`).
pub fn grants_reviewer(found: &Result<bool>) -> bool {
    matches!(found, Ok(true))
}

/// The person has no open application, no current membership and no
/// reviewer role
async fn out_of_reach(db: &Database, settings: &FeaturesTeamApplications, person: &str) -> bool {
    let busy = db
        .fetch_team_applications_of(person)
        .await
        .map(|all| all.iter().any(|a| a.is_open()))
        .unwrap_or(true);
    let member = db
        .fetch_team_members_of(person)
        .await
        .map(|all| all.iter().any(|m| m.is_current()))
        .unwrap_or(true);
    // a failed lookup keeps the identity for now (the next run looks again);
    // it never grants anything
    let reviewer = holds_reviewer_role(db, settings, person).await.unwrap_or(true);
    !busy && !member && !reviewer
}

/// Delete the team-server invite made for a membership, if any (best effort)
pub async fn drop_invite(db: &Database, member: &TeamMember) {
    if let Some(code) = member.invite.as_deref().filter(|code| !code.is_empty()) {
        if let Err(error) = db.delete_invite(code).await {
            if !matches!(error.error_type, ErrorType::NotFound) {
                log::warn!("team server invite not deleted: {:?}", error.error_type);
            }
        }
    }
}

/// Take server roles off a person in one server (they may not be there any
/// more, or the roles gone: nothing to do then)
async fn take_roles(db: &Database, server: &str, user_id: &str, drop: &[String]) {
    let Ok(mut member) = db.fetch_member(server, user_id).await else {
        return;
    };
    let roles: Vec<String> = member
        .roles
        .iter()
        .filter(|id| !drop.contains(id))
        .cloned()
        .collect();
    if roles.len() == member.roles.len() {
        return;
    }
    if let Err(error) = member
        .update(
            db,
            PartialMember {
                roles: Some(roles),
                ..Default::default()
            },
            vec![],
        )
        .await
    {
        log::warn!("team roles not taken back: {:?}", error.error_type);
    }
}

/// The person leaves the team server, unless that is one of the servers a
/// team role belongs to or the person owns it
async fn leave_team_server(db: &Database, settings: &FeaturesTeamApplications, user_id: &str) {
    let id = settings.volunteers.team_server.trim();
    if id.is_empty() {
        return;
    }
    if settings.roles.iter().any(|role| role.server == id) {
        log::warn!("the team server is also a team role's server: nobody is removed from it");
        return;
    }
    let Ok(server) = db.fetch_server(id).await else {
        return;
    };
    if server.owner == user_id {
        return;
    }
    let Ok(member) = db.fetch_member(id, user_id).await else {
        return;
    };
    if let Err(error) = member
        .remove(db, &server, RemovalIntention::Kick, true)
        .await
    {
        log::warn!("not removed from the team server: {:?}", error.error_type);
    }
}

/// End one membership, without any message (the caller sends one if it
/// should). The role of the team role goes; the server's "Zrzur Team" role
/// goes only when no other current membership is left in that server; the
/// person leaves the team server only when nothing current is left anywhere,
/// and their identity is then set to go after `identity_keep_days`.
/// `action` is the history word (`left`, `removed`, `role_removed`).
/// Returns false when the membership had already ended.
pub async fn end_membership(
    db: &Database,
    settings: &FeaturesTeamApplications,
    member: &mut TeamMember,
    actor: &str,
    action: &str,
) -> Result<bool> {
    // already gone: no second history entry, no second thank-you
    if member.status == TeamMemberStatus::Left {
        return Ok(false);
    }
    let now = crate::staff_now();
    member.status = TeamMemberStatus::Left;
    member.left_at = Some(now);
    member.away_until = None;
    member.history.push(TeamHistory {
        at: now,
        actor: actor.to_string(),
        action: action.to_string(),
    });
    // the team-server invite made for this membership dies with it
    drop_invite(db, member).await;
    member.invite = None;
    db.update_team_member(member).await?;

    let others: Vec<TeamMember> = db
        .fetch_team_members_of(&member.user_id)
        .await?
        .into_iter()
        .filter(|other| other.is_current() && other.id != member.id)
        .collect();
    let same_server: Vec<&TeamMember> = others
        .iter()
        .filter(|other| other.server == member.server)
        .collect();

    if let Some(role) = settings.role(&member.role) {
        let mut drop = Vec::new();
        // a role only the owner gives by hand is taken back by hand
        let shared_role = same_server
            .iter()
            .filter_map(|other| settings.role(&other.role))
            .any(|other| other.role == role.role || other.team_role == role.role);
        if !role.is_manual() && !role.role.trim().is_empty() && !shared_role {
            drop.push(role.role.clone());
        }
        // the server's Zrzur Team role stays while another membership is there
        if !role.team_role.trim().is_empty() && same_server.is_empty() {
            drop.push(role.team_role.clone());
        }
        if !drop.is_empty() {
            take_roles(db, &member.server, &member.user_id, &drop).await;
        }
    }

    if others.is_empty() {
        leave_team_server(db, settings, &member.user_id).await;
        // the details are deleted after the keeping time, unless an open
        // application still holds them
        // (one conditional write: a record erased or renewed in between is
        // never written back)
        if out_of_reach(db, settings, &member.user_id).await {
            let at = now + days_ms(settings.volunteers.identity_keep_days);
            let _ = db.mark_team_identity_purge(&member.user_id, at).await;
        }
    }
    Ok(true)
}

// ---- the daily job ----

/// What one run did, for the log and the tests
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DailyRun {
    pub ran: bool,
    pub days_counted: u32,
    pub ended: u32,
    /// One-time notices that contributions are counted
    pub counting_notices: u32,
    pub trial_notices: u32,
    pub reminders: u32,
    pub marked_inactive: u32,
    pub back_to_active: u32,
    pub away_ended: u32,
    /// People whose month was not judged because days were not counted
    pub months_skipped: u32,
}

fn group_by_person(members: Vec<TeamMember>) -> BTreeMap<String, Vec<TeamMember>> {
    let mut people: BTreeMap<String, Vec<TeamMember>> = BTreeMap::new();
    for member in members {
        people.entry(member.user_id.clone()).or_default().push(member);
    }
    people
}

/// How long "About you" stays when nothing holds it (no open application,
/// no current membership): a form filled in without sending step 2
pub const ORPHAN_IDENTITY_MS: i64 = DAY_MS;
/// An identity written less than this long ago is not judged unheld yet: an
/// application sent right after "About you" claims it first
pub const ORPHAN_GRACE_MS: i64 = 3_600_000;

/// Identities whose time is up, identities nothing holds any more (set to
/// go after one day), ended memberships past `identity_keep_days` (with
/// their history and staff notes, the same time the identity goes), and
/// activity days older than `activity_keep_days` or of people with no
/// membership left at all
async fn retention(db: &Database, settings: &FeaturesTeamApplications, now: i64) -> Result<()> {
    db.delete_team_identities_due(now).await?;

    // an identity that nothing holds is not kept for ever
    // (one conditional write, never the whole record: a withdrawal, a
    // rejection or a fresh "About you" in between is not undone)
    // A reviewer's own identity (given without a membership) is released
    // `identity_keep_days` after they lose the role, not after one day.
    for identity in db.fetch_team_identities_kept().await? {
        if now - identity.rules_at < ORPHAN_GRACE_MS {
            continue;
        }
        if out_of_reach(db, settings, &identity.id).await {
            let after = if identity.reviewer {
                days_ms(settings.volunteers.identity_keep_days)
            } else {
                ORPHAN_IDENTITY_MS
            };
            db.mark_team_identity_purge(&identity.id, now + after).await?;
        }
    }

    let keep = days_ms(settings.volunteers.identity_keep_days);
    let people = group_by_person(db.fetch_team_members(&TeamMemberFilter::default()).await?);
    for (user, members) in people {
        let mut remaining = members.len() as u64;
        for member in &members {
            // an ended membership goes whole once its keeping time is over;
            // the database checks it is still ended and due in the same
            // write, so one reopened in between stays. One ended without a
            // date is left alone.
            if !member.is_current() && member.left_at.is_some_and(|at| at + keep <= now) {
                let gone = db.delete_team_member_if_left(&member.id, now - keep).await?;
                if gone > 0 {
                    // an invite left over from before (normally gone at the end)
                    drop_invite(db, member).await;
                }
                remaining -= gone;
            }
        }
        // nothing left of the person in the team (read again): their counts go too
        if remaining == 0 && db.fetch_team_members_of(&user).await?.is_empty() {
            db.delete_team_activity_of(&user).await?;
        }
    }

    // never fewer days than the monthly rule still reads (last month whole)
    let activity_keep = activity_keep_days(settings);
    db.delete_team_activity_before(add_days(day_of(now), -activity_keep))
        .await?;
    Ok(())
}

/// A current membership whose server role was taken away by hand (or whose
/// person left the server) follows to `left`, quietly. Roles the owner
/// gives by hand, and roles waiting for the person to join, are not looked at.
async fn follow_roles(db: &Database, settings: &FeaturesTeamApplications) -> Result<u32> {
    let waiting: HashSet<(String, String)> = db
        .fetch_team_applications_pending_grant()
        .await?
        .into_iter()
        .map(|application| (application.user_id, application.role))
        .collect();
    let mut ended = 0;
    for member in db.fetch_team_members(&TeamMemberFilter::default()).await? {
        if !member.is_current() {
            continue;
        }
        let Some(role) = settings.role(&member.role) else {
            continue;
        };
        if role.is_manual()
            || role.role.trim().is_empty()
            || waiting.contains(&(member.user_id.clone(), member.role.clone()))
        {
            continue;
        }
        let holds = match db.fetch_member(&member.server, &member.user_id).await {
            Ok(server_member) => server_member.roles.contains(&role.role),
            Err(error) if matches!(error.error_type, ErrorType::NotFound) => false,
            // not sure: look again tomorrow
            Err(_) => continue,
        };
        if holds {
            continue;
        }
        // read again right before the write: left or changed in the meantime
        let mut member = db.fetch_team_member(&member.id).await?;
        if member.is_current()
            && end_membership(db, settings, &mut member, SYSTEM, "role_removed").await?
        {
            ended += 1;
        }
    }
    Ok(ended)
}

/// The history word of the notice that contributions are counted: written
/// when the welcome (which says it) or the one-time notice DM went out
pub const COUNTING_NOTICE: &str = "counting_notice";

/// Days the counts are kept, as the texts state it
pub fn activity_keep_days(settings: &FeaturesTeamApplications) -> i64 {
    settings
        .volunteers
        .activity_keep_days
        .clamp(MIN_ACTIVITY_KEEP_DAYS, 3650)
}

/// When the person was told that their contributions are counted: the
/// earliest notice on a current membership, `None` when never
fn told_at(members: &[TeamMember]) -> Option<i64> {
    members
        .iter()
        .filter(|m| m.is_current())
        .flat_map(|m| m.history.iter())
        .filter(|entry| entry.action == COUNTING_NOTICE)
        .map(|entry| entry.at)
        .min()
}

/// The history words staff write when a person objects to the counting,
/// and when the counting starts again (`POST /staff/team/<id>/counting`)
pub const COUNTING_STOPPED: &str = "counting_stopped";
pub const COUNTING_RESUMED: &str = "counting_resumed";

/// Whether the person objected and is not counted: the newest of the two
/// words on any of their memberships, ended ones included, is
/// `counting_stopped`. The objection belongs to the person: a membership
/// that is opened or reopened later gets the word copied in (`enrol`), so it
/// outlives the deletion of the ended record too.
pub fn counting_stopped(members: &[TeamMember]) -> bool {
    members
        .iter()
        .flat_map(|m| m.history.iter())
        .filter(|entry| entry.action == COUNTING_STOPPED || entry.action == COUNTING_RESUMED)
        .max_by_key(|entry| entry.at)
        .is_some_and(|entry| entry.action == COUNTING_STOPPED)
}

/// When counting last started again after an objection, if ever
fn resumed_at(members: &[TeamMember]) -> Option<i64> {
    members
        .iter()
        .filter(|m| m.is_current())
        .flat_map(|m| m.history.iter())
        .filter(|entry| entry.action == COUNTING_RESUMED)
        .map(|entry| entry.at)
        .max()
}

/// Every current volunteer who was never told that their contributions are
/// counted (people who were volunteers before the counting existed) gets
/// one notice DM, in their language, with the right to object. The history
/// word is written only once the DM went out (a failed one is tried again
/// the next day). Nobody is counted before it went out.
async fn send_counting_notices(
    db: &Database,
    settings: &FeaturesTeamApplications,
    messenger: &dyn TeamMessenger,
    now: i64,
) -> Result<u32> {
    let people = group_by_person(db.fetch_team_members(&TeamMemberFilter::default()).await?);
    let mut sent = 0;
    for (user, members) in people {
        if !members.iter().any(|m| m.is_current())
            || told_at(&members).is_some()
            || counting_stopped(&members)
        {
            continue;
        }
        let locale = db.fetch_user(&user).await.ok().and_then(|u| u.locale);
        let language = team_texts::language_of(locale.as_deref());
        let went = messenger
            .send(
                &user,
                team_texts::message(
                    language,
                    "volunteer.counting_notice",
                    &[
                        ("days", activity_keep_days(settings).to_string()),
                        ("privacy", settings.privacy_url.trim().to_string()),
                    ],
                ),
            )
            .await;
        if !went {
            continue;
        }
        // dated with the run: counting starts the day after
        let entry = TeamHistory {
            at: now,
            actor: SYSTEM.to_string(),
            action: COUNTING_NOTICE.to_string(),
        };
        for member in members.iter().filter(|m| m.is_current()) {
            // read again right before the write
            let mut member = db.fetch_team_member(&member.id).await?;
            if !member.is_current() || has_action(&member, COUNTING_NOTICE) {
                continue;
            }
            member.history.push(entry.clone());
            db.update_team_member(&member).await?;
        }
        sent += 1;
    }
    Ok(sent)
}

/// Count the days not counted yet: yesterday and up to six days before,
/// never before the person joined and only from the day after they were
/// told that their contributions are counted (nothing of the notice day
/// itself). A person who objected is not counted.
async fn count_missing_days(
    db: &Database,
    settings: &FeaturesTeamApplications,
    now: i64,
) -> Result<u32> {
    let yesterday = add_days(day_of(now), -1);
    let first = add_days(yesterday, -i64::from(CATCH_UP_DAYS - 1));
    let people = group_by_person(db.fetch_team_members(&TeamMemberFilter::default()).await?);
    let mut counted = 0;
    for (user, members) in people {
        let Some(joined) = members
            .iter()
            .filter(|m| m.is_current())
            .map(|m| m.joined_at)
            .min()
        else {
            continue;
        };
        // not told yet, or objected: nothing is counted
        let Some(told) = told_at(&members) else {
            continue;
        };
        if counting_stopped(&members) {
            continue;
        }
        let have: HashSet<i32> = db
            .fetch_team_activity(&user, first, yesterday)
            .await?
            .into_iter()
            .map(|row| row.day)
            .collect();
        let mut day = first.max(day_of(joined)).max(add_days(day_of(told), 1));
        // the days while counting was stopped are never counted afterwards
        if let Some(resumed) = resumed_at(&members) {
            day = day.max(add_days(day_of(resumed), 1));
        }
        while day <= yesterday {
            if !have.contains(&day) {
                match store_day(db, settings, &user, day).await {
                    Ok(_) => counted += 1,
                    // left for the next run, which counts missing days again
                    Err(error) => {
                        log::warn!("team activity day not counted: {:?}", error.error_type);
                        break;
                    }
                }
            }
            day = add_days(day, 1);
        }
    }
    Ok(counted)
}

/// Trials whose time is over and that nobody confirmed: one English DM to
/// every team manager per membership, with the account name only
async fn notify_trials(
    db: &Database,
    settings: &FeaturesTeamApplications,
    messenger: &dyn TeamMessenger,
    now: i64,
) -> Result<u32> {
    // a trial whose person is away ends all the same
    let in_trial = |m: &TeamMember| {
        matches!(m.status, TeamMemberStatus::Trial | TeamMemberStatus::Away)
            && m.confirmed_at.is_none()
            && m.trial_ends_at <= now
            && !has_action(m, "trial_end_notified")
    };
    let mut due: Vec<TeamMember> = Vec::new();
    for status in [TeamMemberStatus::Trial, TeamMemberStatus::Away] {
        let found = db
            .fetch_team_members(&TeamMemberFilter {
                status: Some(status),
                ..Default::default()
            })
            .await?;
        due.extend(found.into_iter().filter(|m| in_trial(m)));
    }
    if due.is_empty() {
        return Ok(0);
    }
    let managers = team_managers(db, now).await?;
    if managers.is_empty() {
        log::warn!("trials ended but nobody holds the team permission");
        return Ok(0);
    }
    let mut sent = 0;
    for member in due {
        // read again: confirmed or notified in the meantime, nothing to do
        let mut member = db.fetch_team_member(&member.id).await?;
        if !in_trial(&member) {
            continue;
        }
        member
            .history
            .push(history("trial_end_notified".to_string()));
        // written first: a crash after this sends nothing twice
        db.update_team_member(&member).await?;

        let account = db
            .fetch_user(&member.user_id)
            .await
            .map(|user| user.username)
            .unwrap_or_else(|_| member.user_id.clone());
        let role = settings
            .role(&member.role)
            .map(|role| role.name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| member.role.clone());
        let text = team_texts::message(
            "en",
            "staff.trial_ended",
            &[
                ("account", team_texts::plain(&account)),
                ("role", team_texts::plain(&role)),
            ],
        );
        for manager in &managers {
            messenger.send(manager, text.clone()).await;
        }
        sent += 1;
    }
    Ok(sent)
}

/// An away whose `away_until` has passed ends by itself, as pressing "back"
/// does: one who was inactive before is inactive again; otherwise a trial
/// while the trial runs and nobody confirmed it, else active. The history
/// word is `back`, dated when the away ran out.
async fn expire_away(db: &Database, now: i64) -> Result<u32> {
    let mut ended = 0;
    let away = db
        .fetch_team_members(&TeamMemberFilter {
            status: Some(TeamMemberStatus::Away),
            ..Default::default()
        })
        .await?;
    for member in away {
        if !member.away_until.is_some_and(|until| until <= now) {
            continue;
        }
        // read again right before the write
        let mut member = db.fetch_team_member(&member.id).await?;
        let Some(until) = member.away_until.filter(|until| *until <= now) else {
            continue;
        };
        if member.status != TeamMemberStatus::Away {
            continue;
        }
        let was_inactive = member
            .history
            .iter()
            .rev()
            .find(|entry| is_away_start(&entry.action))
            .is_some_and(|entry| entry.action == "away_from_inactive");
        member.status = if was_inactive {
            TeamMemberStatus::Inactive
        } else if now < member.trial_ends_at && member.confirmed_at.is_none() {
            TeamMemberStatus::Trial
        } else {
            TeamMemberStatus::Active
        };
        member.away_until = None;
        member.history.push(TeamHistory {
            at: until,
            actor: SYSTEM.to_string(),
            action: "back".to_string(),
        });
        db.update_team_member(&member).await?;
        ended += 1;
    }
    Ok(ended)
}

/// Whether every day of `month` (yyyymm) has its row: a month with a gap
/// (the job was down for long, a count failed, the job was switched on in
/// the middle) is not judged
fn month_complete(days: &[TeamActivityDay], month: i32) -> bool {
    let (start, end) = month_range(month);
    let have: HashSet<i32> = days.iter().map(|row| row.day).collect();
    let mut day = day_of(start);
    let last = day_of(end - 1);
    while day <= last {
        if !have.contains(&day) {
            return false;
        }
        day = add_days(day, 1);
    }
    true
}

/// The words a month leaves in a history (yyyymm)
fn month_words(month: i32) -> [String; 3] {
    [
        format!("reminded_{month}"),
        format!("inactive_{month}"),
        format!("active_{month}"),
    ]
}

/// Apply the rule for the month before `now` to every membership that was
/// there the whole month; at most one reminder per person and month
async fn monthly(
    db: &Database,
    settings: &FeaturesTeamApplications,
    messenger: &dyn TeamMessenger,
    now: i64,
    run: &mut DailyRun,
) -> Result<()> {
    let month = previous_month(day_of(now) / 100);
    let (start, end) = month_range(month);
    let minimum = settings.volunteers.monthly_minimum.max(0);
    let words = month_words(month);
    let [reminded, inactive, active] = &words;

    let people = group_by_person(db.fetch_team_members(&TeamMemberFilter::default()).await?);
    for (user, members) in people {
        let due: Vec<String> = members
            .iter()
            .filter(|m| m.is_current() && m.joined_at < start)
            .map(|m| m.id.clone())
            .collect();
        if due.is_empty() {
            continue;
        }
        let reminded_before = members.iter().any(|m| has_action(m, reminded));
        let days = db
            .fetch_team_activity(&user, month * 100 + 1, month * 100 + 31)
            .await?;
        if !month_complete(&days, month) {
            log::info!("team volunteers: a month with days not counted is not judged");
            run.months_skipped += 1;
            continue;
        }
        let total = month_total(&days);
        let before_words = month_words(previous_month(month));
        let [low_before, inactive_before, _] = &before_words;
        let low = i64::from(total) < minimum;

        let mut remind = false;
        for id in due {
            // read again right before the write
            let mut member = db.fetch_team_member(&id).await?;
            if !member.is_current() || words.iter().any(|word| has_action(&member, word)) {
                continue;
            }
            // a row of low months holds only when the month before was
            // judged low; a month not judged (a gap) ends the row
            let reset = member.low_months != 0
                && !has_action(&member, low_before)
                && !has_action(&member, inactive_before);
            if reset {
                member.low_months = 0;
            }
            let covers = away_covers(&member, start, end);
            let outcome = month_rule(&member, total, minimum, covers);
            match outcome {
                MonthOutcome::Ok => {
                    // an away month or a good month ends a row of low ones
                    if (covers || !low) && member.low_months != 0 {
                        member.low_months = 0;
                    } else if !reset {
                        continue;
                    }
                }
                MonthOutcome::Remind => {
                    member.low_months = member.low_months.saturating_add(1);
                    member.history.push(history(reminded.clone()));
                }
                MonthOutcome::MarkInactive => {
                    member.low_months = member.low_months.saturating_add(1);
                    member.status = TeamMemberStatus::Inactive;
                    member.away_until = None;
                    member.history.push(history(inactive.clone()));
                }
                MonthOutcome::BackToActive => {
                    member.low_months = 0;
                    member.status = TeamMemberStatus::Active;
                    member.history.push(history(active.clone()));
                }
            }
            db.update_team_member(&member).await?;
            match outcome {
                MonthOutcome::Remind => remind = true,
                MonthOutcome::MarkInactive => run.marked_inactive += 1,
                MonthOutcome::BackToActive => run.back_to_active += 1,
                MonthOutcome::Ok => {}
            }
        }

        // the history word is written before the message: never twice
        if remind && !reminded_before {
            let locale = db.fetch_user(&user).await.ok().and_then(|u| u.locale);
            let language = team_texts::language_of(locale.as_deref());
            messenger
                .send(&user, team_texts::message(language, "volunteer.reminder", &[]))
                .await;
            run.reminders += 1;
        }
    }
    Ok(())
}

/// One run of the daily job at `now`. While `volunteers.enabled` is off only
/// the retention runs (deleting what is past its time is a duty, not a
/// feature); nothing is counted, changed or sent.
pub async fn run_daily(
    db: &Database,
    settings: &FeaturesTeamApplications,
    messenger: &dyn TeamMessenger,
    now: i64,
) -> Result<DailyRun> {
    let mut run = DailyRun::default();
    retention(db, settings, now).await?;
    if !settings.volunteers.enabled {
        return Ok(run);
    }
    run.ran = true;
    run.ended = follow_roles(db, settings).await?;
    run.counting_notices = send_counting_notices(db, settings, messenger, now).await?;
    // before the away ends: an away trial that ran out is told about once
    run.trial_notices = notify_trials(db, settings, messenger, now).await?;
    run.away_ended = expire_away(db, now).await?;
    run.days_counted = count_missing_days(db, settings, now).await?;
    if day_of(now) % 100 <= MONTHLY_DAYS {
        monthly(db, settings, messenger, now, &mut run).await?;
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use iso8601_timestamp::Timestamp;
    use revolt_config::{FeaturesTeamApplications, TeamRole};
    use tokio::sync::Mutex;

    use super::*;
    use crate::{
        AuditLogEntry, AuditLogEntryAction, Member, MemberCompositeKey, ReferenceDb, StaffGrant,
        StaffRole, TeamApplication, TeamApplicationStatus, TeamGrant, TeamIdentity, User,
    };

    const SERVER: &str = "server1";
    const DESK: &str = "desk1";
    const HELP: &str = "help1";
    const GENERAL: &str = "general1";
    const MOD_ROLE: &str = "role_mod";
    /// 2026-11-10 00:00 UTC
    const NOV_10: i64 = 1_794_268_800_000;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<(String, String)>>);

    #[async_trait]
    impl TeamMessenger for Recorder {
        async fn send(&self, user_id: &str, content: String) -> bool {
            self.0.lock().await.push((user_id.to_string(), content));
            true
        }
    }

    /// A messenger whose messages never go out
    struct Unreachable;

    #[async_trait]
    impl TeamMessenger for Unreachable {
        async fn send(&self, _user_id: &str, _content: String) -> bool {
            false
        }
    }

    impl Recorder {
        async fn to(&self, user: &str) -> Vec<String> {
            self.0
                .lock()
                .await
                .iter()
                .filter(|(to, _)| to == user)
                .map(|(_, text)| text.clone())
                .collect()
        }
    }

    fn db() -> Database {
        Database::Reference(ReferenceDb::default())
    }

    fn settings() -> FeaturesTeamApplications {
        let mut settings = FeaturesTeamApplications {
            server: DESK.to_string(),
            roles: vec![TeamRole {
                key: "moderator".to_string(),
                server: SERVER.to_string(),
                role: MOD_ROLE.to_string(),
                work_channels: vec![HELP.to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };
        settings.volunteers.enabled = true;
        settings
    }

    fn member(id: &str, user: &str, status: TeamMemberStatus) -> TeamMember {
        TeamMember {
            id: id.to_string(),
            user_id: user.to_string(),
            role: "moderator".to_string(),
            server: SERVER.to_string(),
            status,
            joined_at: day_start(20260101),
            trial_ends_at: day_start(20260131),
            confirmed_at: None,
            away_until: None,
            left_at: None,
            low_months: 0,
            notes: None,
            invite: None,
            // told at once that contributions are counted
            history: vec![TeamHistory {
                at: day_start(20260101),
                actor: SYSTEM.to_string(),
                action: COUNTING_NOTICE.to_string(),
            }],
        }
    }

    fn entry(at: i64, action: &str) -> TeamHistory {
        TeamHistory {
            at,
            actor: "u1".to_string(),
            action: action.to_string(),
        }
    }

    async fn message(db: &Database, at: i64, channel: &str, author: &str, replies: Option<Vec<String>>) -> String {
        let id = Ulid::from_parts(at as u64, rand_part(at, channel, author)).to_string();
        db.insert_message(&Message {
            id: id.clone(),
            channel: channel.to_string(),
            author: author.to_string(),
            replies,
            ..Default::default()
        })
        .await
        .unwrap();
        id
    }

    fn rand_part(at: i64, channel: &str, author: &str) -> u128 {
        let mut seed = at as u128;
        for byte in channel.bytes().chain(author.bytes()) {
            seed = seed.wrapping_mul(31).wrapping_add(u128::from(byte));
        }
        seed & ((1 << 80) - 1)
    }

    async fn holder(db: &Database, user: &str, roles: Vec<String>) {
        db.insert_or_merge_member(&Member {
            id: MemberCompositeKey {
                server: SERVER.to_string(),
                user: user.to_string(),
            },
            roles,
            ..Default::default()
        })
        .await
        .unwrap();
    }

    fn total(counts: &[TeamActivityCount]) -> u32 {
        counts.iter().map(contributions).sum()
    }

    fn of<'a>(counts: &'a [TeamActivityCount], server: &str) -> &'a TeamActivityCount {
        counts.iter().find(|c| c.server == server).expect("server counted")
    }

    #[test]
    fn days_and_months() {
        assert_eq!(day_of(NOV_10), 20261110);
        assert_eq!(day_of(NOV_10 - 1), 20261109);
        assert_eq!(day_start(20261110), NOV_10);
        assert_eq!(add_days(20261231, 1), 20270101);
        assert_eq!(add_days(20260301, -1), 20260228);
        assert_eq!(add_days(20240301, -1), 20240229);
        assert_eq!(previous_month(202601), 202512);
        assert_eq!(previous_month(202611), 202610);
        let (start, end) = month_range(202612);
        assert_eq!(day_of(start), 20261201);
        assert_eq!(day_of(end), 20270101);
        assert_eq!(until_next_run(NOV_10), 30 * 60_000);
        assert_eq!(until_next_run(NOV_10 + 30 * 60_000), DAY_MS);
        assert_eq!(until_next_run(NOV_10 + 31 * 60_000), DAY_MS - 60_000);
    }

    #[tokio::test]
    async fn counts_only_work_channels() {
        let db = db();
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        message(&db, NOV_10 + 1_000, GENERAL, "u1", None).await;
        let counts = count_day(&db, &settings(), "u1", 20261110).await.unwrap();
        assert_eq!(total(&counts), 0, "a message in #general counts 0");

        message(&db, NOV_10 + 2_000, HELP, "u1", None).await;
        message(&db, NOV_10 + 3_000, HELP, "u2", None).await;
        let counts = count_day(&db, &settings(), "u1", 20261110).await.unwrap();
        assert_eq!(of(&counts, SERVER).messages, 1);
        assert_eq!(total(&counts), 1);
    }

    #[tokio::test]
    async fn reply_to_self_not_counted() {
        let db = db();
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        let own = message(&db, NOV_10 + 1_000, HELP, "u1", None).await;
        let other = message(&db, NOV_10 + 2_000, HELP, "u2", None).await;
        message(&db, NOV_10 + 3_000, HELP, "u1", Some(vec![own])).await;
        message(&db, NOV_10 + 4_000, HELP, "u1", Some(vec![other])).await;
        let counts = count_day(&db, &settings(), "u1", 20261110).await.unwrap();
        let count = of(&counts, SERVER);
        assert_eq!(count.messages, 3);
        assert_eq!(count.replies, 1, "only the answer to someone else");
    }

    #[tokio::test]
    async fn moderation_from_audit_log() {
        let db = db();
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        let log = |n: u64, at: i64, user: &str, action: AuditLogEntryAction| AuditLogEntry {
            id: Ulid::from_parts(at as u64, u128::from(n)).to_string(),
            expires_at: Timestamp::now_utc(),
            server: SERVER.to_string(),
            reason: None,
            user: user.to_string(),
            target: Some("x".to_string()),
            action,
        };
        let ban = || AuditLogEntryAction::BanCreate { user: "x".to_string() };
        for entry in [
            log(1, NOV_10 + 10, "u1", ban()),
            log(2, NOV_10 + 20, "u1", AuditLogEntryAction::MemberKick { user: "x".to_string() }),
            // not moderation
            log(3, NOV_10 + 30, "u1", AuditLogEntryAction::ChannelCreate {
                channel: "c".to_string(),
                name: "c".to_string(),
            }),
            // someone else
            log(4, NOV_10 + 40, "u2", ban()),
            // the day before and the day after
            log(5, NOV_10 - 1, "u1", ban()),
            log(6, NOV_10 + DAY_MS, "u1", ban()),
        ] {
            db.insert_audit_log_entry(&entry).await.unwrap();
        }
        let counts = count_day(&db, &settings(), "u1", 20261110).await.unwrap();
        assert_eq!(of(&counts, SERVER).moderation, 2);
    }

    #[tokio::test]
    async fn day_bounds_are_utc() {
        let db = db();
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        // the last millisecond of the 9th, the first and last of the 10th,
        // the first of the 11th
        message(&db, NOV_10 - 1, HELP, "u1", None).await;
        message(&db, NOV_10, HELP, "u1", None).await;
        message(&db, NOV_10 + DAY_MS - 1, HELP, "u1", None).await;
        message(&db, NOV_10 + DAY_MS, HELP, "u1", None).await;
        let counts = count_day(&db, &settings(), "u1", 20261110).await.unwrap();
        assert_eq!(of(&counts, SERVER).messages, 2);
    }

    #[tokio::test]
    async fn reviews_counted_for_the_desk() {
        let db = db();
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        let application = |id: &str, acted_by: Option<&str>, interviewer: Option<&str>, at: i64| {
            TeamApplication {
                id: id.to_string(),
                user_id: "applicant".to_string(),
                role: "moderator".to_string(),
                status: TeamApplicationStatus::Accepted,
                adult: false,
                answers: vec![],
                submitted_at: 0,
                acted_by: acted_by.map(str::to_string),
                acted_at: Some(at),
                interviewer: interviewer.map(str::to_string),
                reason: None,
                grant: Some(TeamGrant::Granted),
                purge_at: None,
            }
        };
        for a in [
            application("a1", Some("u1"), None, NOV_10 + 5),
            application("a2", Some("u2"), Some("u1"), NOV_10 + 6),
            application("a3", Some("u1"), None, NOV_10 - 5),
        ] {
            db.insert_team_application(&a).await.unwrap();
        }
        let counts = count_day(&db, &settings(), "u1", 20261110).await.unwrap();
        assert_eq!(of(&counts, DESK).reviews, 2);
        assert_eq!(of(&counts, SERVER).reviews, 0);
    }

    #[tokio::test]
    async fn day_recount_overwrites() {
        let db = db();
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        message(&db, NOV_10 + 1_000, HELP, "u1", None).await;
        store_day(&db, &settings(), "u1", 20261110).await.unwrap();
        store_day(&db, &settings(), "u1", 20261110).await.unwrap();
        let rows = db.fetch_team_activity("u1", 20261110, 20261110).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(month_total(&rows), 1, "counted again, not added");

        message(&db, NOV_10 + 2_000, HELP, "u1", None).await;
        store_day(&db, &settings(), "u1", 20261110).await.unwrap();
        let rows = db.fetch_team_activity("u1", 20261110, 20261110).await.unwrap();
        assert_eq!(month_total(&rows), 2);
    }

    #[tokio::test]
    async fn missing_days_are_counted_once_at_most_seven_back() {
        let db = db();
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        let recorder = Recorder::default();
        // the 10th: yesterday is the 9th, the first day looked at the 3rd
        let run = run_daily(&db, &settings(), &recorder, NOV_10 + 3_600_000)
            .await
            .unwrap();
        assert_eq!(run.days_counted, 7);
        let rows = db.fetch_team_activity("u1", 20261101, 20261131).await.unwrap();
        assert_eq!(rows.first().map(|r| r.day), Some(20261103));
        assert_eq!(rows.last().map(|r| r.day), Some(20261109));
        // the same day again: nothing left to count
        let run = run_daily(&db, &settings(), &recorder, NOV_10 + 7_200_000)
            .await
            .unwrap();
        assert_eq!(run.days_counted, 0);
    }

    // ---- the monthly rule ----

    #[test]
    fn month_rule_remind_when_low() {
        let m = member("m1", "u1", TeamMemberStatus::Active);
        assert_eq!(month_rule(&m, 1, 2, false), MonthOutcome::Remind);
        assert_eq!(month_rule(&m, 2, 2, false), MonthOutcome::Ok);
    }

    #[test]
    fn month_rule_second_low_month_marks_inactive() {
        let mut m = member("m1", "u1", TeamMemberStatus::Active);
        m.low_months = 1;
        assert_eq!(month_rule(&m, 0, 2, false), MonthOutcome::MarkInactive);
        // a good month in between ends the row
        assert_eq!(month_rule(&m, 5, 2, false), MonthOutcome::Ok);
    }

    #[test]
    fn month_rule_trial_never_inactive() {
        let mut m = member("m1", "u1", TeamMemberStatus::Trial);
        m.low_months = 3;
        assert_eq!(month_rule(&m, 0, 2, false), MonthOutcome::Remind);
    }

    #[test]
    fn month_rule_inactive_back_to_active() {
        let mut m = member("m1", "u1", TeamMemberStatus::Inactive);
        m.low_months = 2;
        assert_eq!(month_rule(&m, 2, 2, false), MonthOutcome::BackToActive);
        // still low: no second reminder
        assert_eq!(month_rule(&m, 0, 2, false), MonthOutcome::Ok);
    }

    #[test]
    fn month_rule_away_is_never_low() {
        let mut m = member("m1", "u1", TeamMemberStatus::Active);
        m.low_months = 1;
        assert_eq!(month_rule(&m, 0, 2, true), MonthOutcome::Ok);
        // a minimum below zero is no minimum
        assert_eq!(month_rule(&m, 0, -5, false), MonthOutcome::Ok);
    }

    #[test]
    fn month_rule_away_mid_month() {
        // November 2026; away from Oct 20 until Nov 10 (it ran out mid-month)
        let (start, end) = month_range(202611);
        let mut m = member("m1", "u1", TeamMemberStatus::Away);
        m.history = vec![entry(day_start(20261020), "away")];
        m.away_until = Some(day_start(20261110));
        assert!(away_covers(&m, start, end));
        assert_eq!(month_rule(&m, 0, 2, away_covers(&m, start, end)), MonthOutcome::Ok);

        // back on the 3rd: still part of the month
        let mut back = member("m2", "u1", TeamMemberStatus::Active);
        back.history = vec![
            entry(day_start(20261001), "away"),
            entry(day_start(20261103), "back"),
        ];
        assert!(away_covers(&back, start, end));

        // away only in September: November is an ordinary month
        let mut earlier = member("m3", "u1", TeamMemberStatus::Active);
        earlier.history = vec![
            entry(day_start(20260901), "away"),
            entry(day_start(20260920), "back"),
        ];
        assert!(!away_covers(&earlier, start, end));

        // away from the 25th on
        let mut late = member("m4", "u1", TeamMemberStatus::Away);
        late.history = vec![entry(day_start(20261125), "away")];
        late.away_until = Some(day_start(20270101));
        assert!(away_covers(&late, start, end));
    }

    /// A row (with no counts) for every day of `month` that has none: the
    /// month was counted whole
    async fn fill_month(db: &Database, user: &str, month: i32) {
        let have: Vec<i32> = db
            .fetch_team_activity(user, month * 100 + 1, month * 100 + 31)
            .await
            .unwrap()
            .iter()
            .map(|row| row.day)
            .collect();
        let (start, end) = month_range(month);
        let mut day = day_of(start);
        while day <= day_of(end - 1) {
            if !have.contains(&day) {
                db.upsert_team_activity_day(&TeamActivityDay {
                    id: format!("{user}:{day}"),
                    user_id: user.to_string(),
                    day,
                    counts: vec![],
                })
                .await
                .unwrap();
            }
            day = add_days(day, 1);
        }
    }

    async fn december_first(db: &Database, recorder: &Recorder) -> DailyRun {
        run_daily(db, &settings(), recorder, day_start(20261201) + 3_600_000)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reminder_sent_once_per_month() {
        let db = db();
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        fill_month(&db, "u1", 202611).await;
        let recorder = Recorder::default();
        let first = december_first(&db, &recorder).await;
        let second = december_first(&db, &recorder).await;
        assert_eq!(first.reminders, 1);
        assert_eq!(second.reminders, 0);
        assert_eq!(recorder.to("u1").await.len(), 1);
        let m = db.fetch_team_member("m1").await.unwrap();
        assert_eq!(m.low_months, 1, "the month was looked at once");
        assert_eq!(m.status, TeamMemberStatus::Active);
        assert!(has_action(&m, "reminded_202611"));
        // the next day of the catch-up window changes nothing either
        run_daily(&db, &settings(), &recorder, day_start(20261202) + 3_600_000)
            .await
            .unwrap();
        assert_eq!(recorder.to("u1").await.len(), 1);
        assert_eq!(db.fetch_team_member("m1").await.unwrap().low_months, 1);
    }

    #[tokio::test]
    async fn one_reminder_for_two_memberships_and_inactive_after_two_months() {
        let db = db();
        let mut settings = settings();
        settings.roles.push(TeamRole {
            key: "support".to_string(),
            server: SERVER.to_string(),
            role: "role_support".to_string(),
            ..Default::default()
        });
        holder(&db, "u1", vec![MOD_ROLE.to_string(), "role_support".to_string()]).await;
        let mut first = member("m1", "u1", TeamMemberStatus::Active);
        // October was judged low already
        first.low_months = 1;
        first.history = vec![entry(day_start(20261101), "reminded_202610")];
        let mut second = member("m2", "u1", TeamMemberStatus::Active);
        second.role = "support".to_string();
        db.insert_team_member(&first).await.unwrap();
        db.insert_team_member(&second).await.unwrap();
        fill_month(&db, "u1", 202611).await;
        let recorder = Recorder::default();
        let run = run_daily(&db, &settings, &recorder, day_start(20261201) + 3_600_000)
            .await
            .unwrap();
        assert_eq!(run.reminders, 1);
        assert_eq!(run.marked_inactive, 1);
        assert_eq!(recorder.to("u1").await.len(), 1);
        let m1 = db.fetch_team_member("m1").await.unwrap();
        assert_eq!(m1.status, TeamMemberStatus::Inactive);
        assert!(has_action(&m1, "inactive_202611"));
    }

    #[tokio::test]
    async fn inactive_comes_back_after_a_good_month() {
        let db = db();
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        let mut m = member("m1", "u1", TeamMemberStatus::Inactive);
        m.low_months = 2;
        db.insert_team_member(&m).await.unwrap();
        message(&db, day_start(20261115), HELP, "u1", None).await;
        message(&db, day_start(20261116), HELP, "u1", None).await;
        store_day(&db, &settings(), "u1", 20261115).await.unwrap();
        store_day(&db, &settings(), "u1", 20261116).await.unwrap();
        fill_month(&db, "u1", 202611).await;
        let recorder = Recorder::default();
        let run = december_first(&db, &recorder).await;
        assert_eq!(run.back_to_active, 1);
        let m = db.fetch_team_member("m1").await.unwrap();
        assert_eq!(m.status, TeamMemberStatus::Active);
        assert_eq!(m.low_months, 0);
        assert!(recorder.to("u1").await.is_empty());
    }

    #[tokio::test]
    async fn job_off_when_disabled() {
        let db = db();
        let mut settings = settings();
        settings.volunteers.enabled = false;
        // a trial that ended, a role taken away, a low month: all ignored
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Trial))
            .await
            .unwrap();
        save_owner(&db, "boss").await;
        let recorder = Recorder::default();
        let run = run_daily(&db, &settings, &recorder, day_start(20261201) + 3_600_000)
            .await
            .unwrap();
        assert_eq!(run, DailyRun::default());
        assert!(recorder.0.lock().await.is_empty());
        let m = db.fetch_team_member("m1").await.unwrap();
        assert_eq!(m, member("m1", "u1", TeamMemberStatus::Trial));
        assert!(db.fetch_team_activity("u1", 20261101, 20261131).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn hand_removed_role_ends_membership() {
        let db = db();
        holder(&db, "u1", vec![]).await;
        holder(&db, "u2", vec![MOD_ROLE.to_string()]).await;
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        db.insert_team_member(&member("m2", "u2", TeamMemberStatus::Active))
            .await
            .unwrap();
        let recorder = Recorder::default();
        let run = run_daily(&db, &settings(), &recorder, NOV_10 + 3_600_000)
            .await
            .unwrap();
        assert_eq!(run.ended, 1);
        let ended = db.fetch_team_member("m1").await.unwrap();
        assert_eq!(ended.status, TeamMemberStatus::Left);
        let last = ended.history.last().unwrap();
        assert_eq!((last.actor.as_str(), last.action.as_str()), (SYSTEM, "role_removed"));
        assert_eq!(
            db.fetch_team_member("m2").await.unwrap().status,
            TeamMemberStatus::Active
        );
        // quietly: no message
        assert!(recorder.0.lock().await.is_empty());
    }

    async fn save_owner(db: &Database, id: &str) {
        let grant = StaffGrant {
            id: id.to_string(),
            roles: vec![StaffRole::Owner],
            disabled: false,
            expires_at: None,
            last_totp_step: 0,
            version: 0,
            updated_at: 0,
            updated_by: "system:cli".to_string(),
        };
        assert!(db.save_staff_grant(&grant, None).await.unwrap());
    }

    #[tokio::test]
    async fn trial_end_notifies_team_managers_once() {
        let db = db();
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        db.insert_user(&User {
            id: "u1".to_string(),
            username: "sara_k".to_string(),
            discriminator: "0001".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        save_owner(&db, "boss").await;
        let mut trial = member("m1", "u1", TeamMemberStatus::Trial);
        trial.trial_ends_at = NOV_10 - DAY_MS;
        db.insert_team_member(&trial).await.unwrap();
        let mut running = member("m2", "u2", TeamMemberStatus::Trial);
        running.trial_ends_at = NOV_10 + 5 * DAY_MS;
        holder(&db, "u2", vec![MOD_ROLE.to_string()]).await;
        db.insert_team_member(&running).await.unwrap();

        let recorder = Recorder::default();
        for _ in 0..2 {
            run_daily(&db, &settings(), &recorder, NOV_10 + 3_600_000)
                .await
                .unwrap();
        }
        let sent = recorder.to("boss").await;
        assert_eq!(sent.len(), 1, "one notice, however often the job runs");
        // the account name, made harmless, and nothing else about the person
        assert!(sent[0].contains("sara\\_k"));
        assert!(!sent[0].contains("u1"));
        assert!(recorder.to("u1").await.is_empty());
        assert!(has_action(
            &db.fetch_team_member("m1").await.unwrap(),
            "trial_end_notified"
        ));
    }

    #[tokio::test]
    async fn notes_go_with_the_identity() {
        let db = db();
        let mut left = member("m1", "u1", TeamMemberStatus::Left);
        left.left_at = Some(NOV_10 - 100 * DAY_MS);
        left.notes = Some("helpful".to_string());
        db.insert_team_member(&left).await.unwrap();
        let mut current = member("m2", "u2", TeamMemberStatus::Active);
        current.notes = Some("kept".to_string());
        holder(&db, "u2", vec![MOD_ROLE.to_string()]).await;
        db.insert_team_member(&current).await.unwrap();
        run_daily(&db, &settings(), &Recorder::default(), NOV_10 + 3_600_000)
            .await
            .unwrap();
        // the ended membership went whole, notes included
        assert!(db.fetch_team_member("m1").await.is_err());
        assert_eq!(
            db.fetch_team_member("m2").await.unwrap().notes.as_deref(),
            Some("kept")
        );
    }

    #[tokio::test]
    async fn ended_membership_and_its_days_go_after_the_keeping_time() {
        let db = db();
        let mut settings = settings();
        // the retention is a duty: it runs with the care switched off too
        settings.volunteers.enabled = false;
        let row = |user: &str, day: i32| TeamActivityDay {
            id: format!("{user}:{day}"),
            user_id: user.to_string(),
            day,
            counts: vec![],
        };
        // u1 left 91 days ago: the record, its history and the days go
        let mut old = member("m1", "u1", TeamMemberStatus::Left);
        old.left_at = Some(NOV_10 - 91 * DAY_MS);
        old.history.push(entry(NOV_10 - 91 * DAY_MS, "removed"));
        db.insert_team_member(&old).await.unwrap();
        db.upsert_team_activity_day(&row("u1", 20261020)).await.unwrap();
        // u2 left 89 days ago: still within the keeping time
        let mut recent = member("m2", "u2", TeamMemberStatus::Left);
        recent.left_at = Some(NOV_10 - 89 * DAY_MS);
        db.insert_team_member(&recent).await.unwrap();
        db.upsert_team_activity_day(&row("u2", 20261020)).await.unwrap();
        // u3 left one team long ago and is still in another: only the old one goes
        let mut gone = member("m3", "u3", TeamMemberStatus::Left);
        gone.left_at = Some(NOV_10 - 200 * DAY_MS);
        db.insert_team_member(&gone).await.unwrap();
        let mut still = member("m4", "u3", TeamMemberStatus::Active);
        still.role = "support".to_string();
        db.insert_team_member(&still).await.unwrap();
        db.upsert_team_activity_day(&row("u3", 20261020)).await.unwrap();
        // u5 ended with no date (an old record): left alone, never guessed
        let undated = member("m5", "u5", TeamMemberStatus::Left);
        db.insert_team_member(&undated).await.unwrap();

        let run = run_daily(&db, &settings, &Recorder::default(), NOV_10 + 3_600_000)
            .await
            .unwrap();
        assert!(!run.ran);
        assert!(db.fetch_team_member("m1").await.is_err());
        assert!(db.fetch_team_activity("u1", 20250101, 20271231).await.unwrap().is_empty());
        assert!(db.fetch_team_member("m2").await.is_ok());
        assert_eq!(db.fetch_team_activity("u2", 20250101, 20271231).await.unwrap().len(), 1);
        assert!(db.fetch_team_member("m3").await.is_err());
        assert!(db.fetch_team_member("m4").await.is_ok());
        assert_eq!(db.fetch_team_activity("u3", 20250101, 20271231).await.unwrap().len(), 1);
        assert!(db.fetch_team_member("m5").await.is_ok());
    }

    #[tokio::test]
    async fn identity_nothing_holds_goes_after_a_day() {
        let db = db();
        let mut settings = settings();
        settings.volunteers.enabled = false;
        let identity = |id: &str| TeamIdentity {
            id: id.to_string(),
            name: "Sara".to_string(),
            age: 24,
            country: "Austria".to_string(),
            rules_version: "1".to_string(),
            rules_at: 0,
            purge_at: None,
            reviewer: false,
        };
        // "About you" without step 2, one of a current volunteer, one of an open applicant
        db.upsert_team_identity(&identity("alone")).await.unwrap();
        db.upsert_team_identity(&identity("volunteer")).await.unwrap();
        db.insert_team_member(&member("m1", "volunteer", TeamMemberStatus::Active))
            .await
            .unwrap();
        db.upsert_team_identity(&identity("applicant")).await.unwrap();
        db.insert_team_application(&TeamApplication {
            id: "a1".to_string(),
            user_id: "applicant".to_string(),
            role: "moderator".to_string(),
            status: TeamApplicationStatus::Open,
            adult: false,
            answers: vec![],
            submitted_at: NOV_10,
            acted_by: None,
            acted_at: None,
            interviewer: None,
            reason: None,
            grant: None,
            purge_at: None,
        })
        .await
        .unwrap();

        let recorder = Recorder::default();
        run_daily(&db, &settings, &recorder, NOV_10).await.unwrap();
        let alone = db.fetch_team_identity("alone").await.unwrap().unwrap();
        assert_eq!(alone.purge_at, Some(NOV_10 + ORPHAN_IDENTITY_MS));
        assert_eq!(db.fetch_team_identity("volunteer").await.unwrap().unwrap().purge_at, None);
        assert_eq!(db.fetch_team_identity("applicant").await.unwrap().unwrap().purge_at, None);
        // a day later it is gone; the held ones stay
        run_daily(&db, &settings, &recorder, NOV_10 + ORPHAN_IDENTITY_MS + 1).await.unwrap();
        assert!(db.fetch_team_identity("alone").await.unwrap().is_none());
        assert!(db.fetch_team_identity("volunteer").await.unwrap().is_some());
        assert!(db.fetch_team_identity("applicant").await.unwrap().is_some());
    }

    #[test]
    fn a_reviewer_check_grants_only_on_a_sure_yes() {
        assert!(grants_reviewer(&Ok(true)));
        assert!(!grants_reviewer(&Ok(false)));
        let failed: Result<bool> = Err(revolt_result::create_error!(InternalError));
        assert!(!grants_reviewer(&failed));
    }

    #[tokio::test]
    async fn a_reviewers_identity_is_held_by_the_role_then_kept_the_keeping_time() {
        let db = db();
        let mut settings = settings();
        settings.server = SERVER.to_string();
        settings.reviewer_role = "role_rev".to_string();
        settings.volunteers.enabled = false;
        let identity = |id: &str, reviewer: bool, rules_at: i64| TeamIdentity {
            id: id.to_string(),
            name: "Rami".to_string(),
            age: 30,
            country: "Jordan".to_string(),
            rules_version: "1".to_string(),
            rules_at,
            purge_at: None,
            reviewer,
        };
        holder(&db, "rev", vec!["role_rev".to_string()]).await;
        db.upsert_team_identity(&identity("rev", true, NOV_10 - 10 * DAY_MS)).await.unwrap();
        // "About you" just sent: an application may still claim it
        db.upsert_team_identity(&identity("fresh", false, NOV_10 - 60_000)).await.unwrap();

        let recorder = Recorder::default();
        run_daily(&db, &settings, &recorder, NOV_10).await.unwrap();
        assert_eq!(db.fetch_team_identity("rev").await.unwrap().unwrap().purge_at, None);
        assert_eq!(db.fetch_team_identity("fresh").await.unwrap().unwrap().purge_at, None);

        // the role is taken away: kept for the keeping time, not one day
        db.remove_member_role(
            &MemberCompositeKey {
                server: SERVER.to_string(),
                user: "rev".to_string(),
            },
            "role_rev",
        )
        .await
        .unwrap();
        let later = NOV_10 + 2 * ORPHAN_GRACE_MS;
        run_daily(&db, &settings, &recorder, later).await.unwrap();
        let keep = days_ms(settings.volunteers.identity_keep_days);
        assert_eq!(db.fetch_team_identity("rev").await.unwrap().unwrap().purge_at, Some(later + keep));
        // and the fresh one, now past the hour, gets its day
        assert_eq!(
            db.fetch_team_identity("fresh").await.unwrap().unwrap().purge_at,
            Some(later + ORPHAN_IDENTITY_MS)
        );
    }

    #[tokio::test]
    async fn counting_waits_for_the_one_time_notice() {
        let db = db();
        let mut settings = settings();
        settings.privacy_url = "https://zrzur.com/privacy".to_string();
        settings.volunteers.activity_keep_days = 190;
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        // a volunteer from before the counting: never told
        let mut before = member("m1", "u1", TeamMemberStatus::Active);
        before.history = vec![];
        db.insert_team_member(&before).await.unwrap();
        message(&db, day_start(20261108) + 5_000, HELP, "u1", None).await;
        // on the notice day, before the notice went out
        message(&db, NOV_10 + 60_000, HELP, "u1", None).await;

        // the DM does not go out: no marker, nothing counted, tried again
        let run = run_daily(&db, &settings, &Unreachable, NOV_10 + 1_800_000).await.unwrap();
        assert_eq!(run.counting_notices, 0);
        assert!(!has_action(&db.fetch_team_member("m1").await.unwrap(), COUNTING_NOTICE));

        let recorder = Recorder::default();
        let run = run_daily(&db, &settings, &recorder, NOV_10 + 3_600_000).await.unwrap();
        assert_eq!(run.counting_notices, 1);
        assert_eq!(run.days_counted, 0, "nothing is counted before the notice");
        assert!(db.fetch_team_activity("u1", 20250101, 20271231).await.unwrap().is_empty());
        let told = recorder.to("u1").await;
        assert_eq!(told.len(), 1);
        assert!(told[0].contains("190"), "{}", told[0]);
        assert!(told[0].contains("https://zrzur.com/privacy"), "{}", told[0]);
        assert!(told[0].contains("tell the owner and your counting stops"), "{}", told[0]);
        assert!(has_action(&db.fetch_team_member("m1").await.unwrap(), COUNTING_NOTICE));

        // again the same day: no second notice
        let run = run_daily(&db, &settings, &recorder, NOV_10 + 7_200_000).await.unwrap();
        assert_eq!(run.counting_notices, 0);
        assert_eq!(recorder.to("u1").await.len(), 1);

        // the next day: the notice day itself is not counted
        let run = run_daily(&db, &settings, &recorder, NOV_10 + DAY_MS + 3_600_000)
            .await
            .unwrap();
        assert_eq!(run.days_counted, 0);
        assert!(db.fetch_team_activity("u1", 20250101, 20271231).await.unwrap().is_empty());
        // the day after: the first full day after the notice
        let run = run_daily(&db, &settings, &recorder, NOV_10 + 2 * DAY_MS + 3_600_000)
            .await
            .unwrap();
        assert_eq!(run.days_counted, 1);
        let rows = db.fetch_team_activity("u1", 20250101, 20271231).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.day).collect::<Vec<_>>(), vec![20261111]);
    }

    #[tokio::test]
    async fn an_objection_stops_the_counting_until_resumed() {
        let db = db();
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        let mut m = member("m1", "u1", TeamMemberStatus::Active);
        m.history.push(TeamHistory { at: NOV_10 - 20 * DAY_MS, actor: "boss".to_string(), action: COUNTING_STOPPED.to_string() });
        db.insert_team_member(&m).await.unwrap();
        let recorder = Recorder::default();
        let run = run_daily(&db, &settings(), &recorder, NOV_10 + 3_600_000).await.unwrap();
        assert_eq!(run.days_counted, 0);
        assert!(db.fetch_team_activity("u1", 20250101, 20271231).await.unwrap().is_empty());
        assert!(recorder.0.lock().await.is_empty());

        // resumed later: counted again
        let mut m = db.fetch_team_member("m1").await.unwrap();
        m.history.push(TeamHistory { at: day_start(20261107) + 3_600_000, actor: "boss".to_string(), action: COUNTING_RESUMED.to_string() });
        db.update_team_member(&m).await.unwrap();
        let run = run_daily(&db, &settings(), &recorder, NOV_10 + 7_200_000).await.unwrap();
        // only the days after it, never the stopped ones
        assert_eq!(run.days_counted, 2);
        let rows = db.fetch_team_activity("u1", 20250101, 20271231).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.day).collect::<Vec<_>>(), vec![20261108, 20261109]);
    }

    // ---- review fixes ----

    #[tokio::test]
    async fn switched_on_mid_month_nobody_reminded() {
        let db = db();
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        // the job counted only from the 15th on: November has a gap
        for day in 15..=30 {
            db.upsert_team_activity_day(&TeamActivityDay {
                id: format!("u1:202611{day}"),
                user_id: "u1".to_string(),
                day: 20261100 + day,
                counts: vec![],
            })
            .await
            .unwrap();
        }
        let recorder = Recorder::default();
        let run = december_first(&db, &recorder).await;
        assert_eq!(run.reminders, 0);
        assert_eq!(run.months_skipped, 1);
        assert!(recorder.to("u1").await.is_empty());
        let m = db.fetch_team_member("m1").await.unwrap();
        assert_eq!(m.low_months, 0);
        assert!(month_words(202611).iter().all(|word| !has_action(&m, word)));
    }

    #[tokio::test]
    async fn a_gap_ends_the_row_of_low_months() {
        let db = db();
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        // low once long ago (no word for October): November is a first low month
        let mut m = member("m1", "u1", TeamMemberStatus::Active);
        m.low_months = 1;
        m.history = vec![entry(day_start(20260801), "reminded_202607")];
        db.insert_team_member(&m).await.unwrap();
        fill_month(&db, "u1", 202611).await;
        let recorder = Recorder::default();
        let run = december_first(&db, &recorder).await;
        assert_eq!(run.marked_inactive, 0);
        assert_eq!(run.reminders, 1);
        let m = db.fetch_team_member("m1").await.unwrap();
        assert_eq!(m.status, TeamMemberStatus::Active);
        assert_eq!(m.low_months, 1);
    }

    #[tokio::test]
    async fn thread_messages_count_for_the_work_channel() {
        let db = db();
        db.insert_team_member(&member("m1", "u1", TeamMemberStatus::Active))
            .await
            .unwrap();
        let thread = |id: &str, parent: &str, archived: bool, last: &str| -> Channel {
            serde_json::from_value(serde_json::json!({
                "channel_type": "TextChannel",
                "_id": id,
                "server": SERVER,
                "name": "thread",
                "parent": parent,
                "thread": { "owner": "u2", "archived": archived },
                "last_message_id": last,
            }))
            .unwrap()
        };
        let a = message(&db, NOV_10 + 1_000, "thread_a", "u1", None).await;
        let b = message(&db, NOV_10 + 2_000, "thread_b", "u1", None).await;
        let c = message(&db, NOV_10 + 3_000, "thread_c", "u1", None).await;
        db.insert_channel(&thread("thread_a", HELP, false, &a)).await.unwrap();
        db.insert_channel(&thread("thread_b", HELP, true, &b)).await.unwrap();
        // a thread of #general does not count
        db.insert_channel(&thread("thread_c", GENERAL, false, &c)).await.unwrap();
        let counts = count_day(&db, &settings(), "u1", 20261110).await.unwrap();
        assert_eq!(of(&counts, SERVER).messages, 2);
    }

    #[tokio::test]
    async fn retention_runs_when_disabled_and_keeps_two_months() {
        let db = db();
        let mut settings = settings();
        settings.volunteers.enabled = false;
        settings.volunteers.activity_keep_days = 0;
        db.upsert_team_identity(&TeamIdentity {
            id: "gone".to_string(),
            name: "Sara".to_string(),
            age: 24,
            country: "Austria".to_string(),
            rules_version: "1".to_string(),
            rules_at: 0,
            purge_at: Some(NOV_10 - DAY_MS),
            reviewer: false,
        })
        .await
        .unwrap();
        let row = |day: i32| TeamActivityDay {
            id: format!("u1:{day}"),
            user_id: "u1".to_string(),
            day,
            counts: vec![],
        };
        // 40 days back stays (at least 62 are kept); 100 days back goes
        db.upsert_team_activity_day(&row(add_days(20261110, -40))).await.unwrap();
        db.upsert_team_activity_day(&row(add_days(20261110, -100))).await.unwrap();
        let recorder = Recorder::default();
        let run = run_daily(&db, &settings, &recorder, NOV_10 + 3_600_000)
            .await
            .unwrap();
        assert!(!run.ran);
        assert!(db.fetch_team_identity("gone").await.unwrap().is_none());
        let rows = db.fetch_team_activity("u1", 20250101, 20271231).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.day).collect::<Vec<_>>(), vec![add_days(20261110, -40)]);
        assert!(recorder.0.lock().await.is_empty());
    }

    #[tokio::test]
    async fn away_ends_by_itself() {
        let db = db();
        let until = NOV_10 - 3_600_000;
        let mut from_inactive = member("m1", "u1", TeamMemberStatus::Away);
        from_inactive.history = vec![entry(NOV_10 - 20 * DAY_MS, "away_from_inactive")];
        from_inactive.away_until = Some(until);
        let mut in_trial = member("m2", "u2", TeamMemberStatus::Away);
        in_trial.history = vec![entry(NOV_10 - 5 * DAY_MS, "away")];
        in_trial.away_until = Some(until);
        in_trial.trial_ends_at = NOV_10 + 10 * DAY_MS;
        let mut active = member("m3", "u3", TeamMemberStatus::Away);
        active.history = vec![entry(NOV_10 - 5 * DAY_MS, "away")];
        active.away_until = Some(until);
        active.confirmed_at = Some(day_start(20260201));
        // still away
        let mut later = member("m4", "u4", TeamMemberStatus::Away);
        later.history = vec![entry(NOV_10 - 5 * DAY_MS, "away")];
        later.away_until = Some(NOV_10 + 5 * DAY_MS);
        later.confirmed_at = Some(day_start(20260201));
        for (user, m) in [("u1", from_inactive), ("u2", in_trial), ("u3", active), ("u4", later)] {
            holder(&db, user, vec![MOD_ROLE.to_string()]).await;
            db.insert_team_member(&m).await.unwrap();
        }
        let run = run_daily(&db, &settings(), &Recorder::default(), NOV_10 + 3_600_000)
            .await
            .unwrap();
        assert_eq!(run.away_ended, 3);
        for (id, status) in [
            ("m1", TeamMemberStatus::Inactive),
            ("m2", TeamMemberStatus::Trial),
            ("m3", TeamMemberStatus::Active),
            ("m4", TeamMemberStatus::Away),
        ] {
            let m = db.fetch_team_member(id).await.unwrap();
            assert_eq!(m.status, status, "{id}");
            if id != "m4" {
                assert_eq!(m.away_until, None);
                let last = m.history.last().unwrap();
                assert_eq!((last.actor.as_str(), last.action.as_str(), last.at), (SYSTEM, "back", until));
            }
        }
    }

    #[tokio::test]
    async fn trial_end_notice_also_when_away() {
        let db = db();
        holder(&db, "u1", vec![MOD_ROLE.to_string()]).await;
        save_owner(&db, "boss").await;
        let mut away = member("m1", "u1", TeamMemberStatus::Away);
        away.trial_ends_at = NOV_10 - DAY_MS;
        away.history = vec![entry(NOV_10 - 10 * DAY_MS, "away")];
        away.away_until = Some(NOV_10 + 10 * DAY_MS);
        db.insert_team_member(&away).await.unwrap();
        let recorder = Recorder::default();
        for _ in 0..2 {
            run_daily(&db, &settings(), &recorder, NOV_10 + 3_600_000)
                .await
                .unwrap();
        }
        assert_eq!(recorder.to("boss").await.len(), 1);
        let m = db.fetch_team_member("m1").await.unwrap();
        assert_eq!(m.status, TeamMemberStatus::Away);
        assert!(has_action(&m, "trial_end_notified"));
    }
}
