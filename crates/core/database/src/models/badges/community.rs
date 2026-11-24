//! Community Builder (100) and Big Community (1,000): genuine human
//! members, counted by crond, never on a request.
//!
//! A member counts when:
//! - the account is a person: not a bot, not deleted, banned or suspended,
//!   not an official or system account;
//! - the account is at least 7 days old and its e-mail is confirmed;
//! - they have been a member for at least 24 hours (so people who join
//!   and leave again within a day never count).
//!
//! When the counted members joined, or their accounts were made, in a
//! burst (more than a quarter of them within one hour), the server is held
//! for staff to look at before any badge is given. Staff release it or
//! refuse it; both are written to the platform audit log.
//!
//! Only metadata is read: member join times, account creation times (from
//! the id), account flags and whether the e-mail is confirmed. No message,
//! no DM, no address. Every grant goes through the badge gate
//! (`mutate_badge`, system path) with its audit record.
//!
//! Each server gives each badge once, to its owner when it first reaches
//! the count; it stays with them even if the server shrinks or changes
//! hands. People who earned it under the old rule (any member counted)
//! keep it.
//!
//! Limits (see the audit): aged, confirmed fake accounts added slowly by
//! one person, and real people paid to join, can't be told apart from this
//! data. These badges are achievements, never trust marks: they unlock
//! nothing.

use std::collections::HashSet;

use revolt_models::v0::UserFlags;
use revolt_result::{create_error, ErrorType, Result};
use ulid::Ulid;

use crate::{
    epoch_ms, held_badges, mutate_badge, record_platform_audit, revoked_by_a_person, AuditOutcome,
    AuditRecord, BadgeActor, BadgeChange, BadgeMutation, CommunityBadgeCheck, CommunityCheckStatus,
    Database, StaffPermission, User,
};

/// Genuine human members a server needs for Community Builder
pub const COMMUNITY_BUILDER_MEMBERS: usize = 100;
/// Genuine human members a server needs for Big Community
pub const BIG_COMMUNITY_MEMBERS: usize = 1000;

const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// A counted account is at least this old
pub const MIN_ACCOUNT_AGE_MS: i64 = 7 * DAY_MS;
/// A counted member joined at least this long ago
pub const MIN_MEMBERSHIP_MS: i64 = DAY_MS;
/// The window a burst is measured in
pub const BURST_WINDOW_MS: i64 = HOUR_MS;
/// More than this share (per cent) of the counted members inside one window
/// holds the server for staff
pub const BURST_SHARE_PERCENT: usize = 25;

/// Members read per page
const PAGE: i64 = 1000;

/// How often crond counts
pub const COMMUNITY_PASS_EVERY_SECS: u64 = 6 * 60 * 60;

/// Thresholds (tests use small ones)
#[derive(Debug, Clone, Copy)]
pub struct CommunityThresholds {
    pub builder: usize,
    pub big: usize,
}

impl Default for CommunityThresholds {
    fn default() -> Self {
        CommunityThresholds {
            builder: COMMUNITY_BUILDER_MEMBERS,
            big: BIG_COMMUNITY_MEMBERS,
        }
    }
}

/// Badges a count of genuine members earns
pub fn size_badges(counted: usize, thresholds: CommunityThresholds) -> &'static [&'static str] {
    if counted >= thresholds.big {
        &["community_builder", "big_community"]
    } else if counted >= thresholds.builder {
        &["community_builder"]
    } else {
        &[]
    }
}

/// When an account was made, from its id (0 when the id is not a ULID)
fn created_ms(id: &str) -> i64 {
    Ulid::from_string(id)
        .map(|id| id.timestamp_ms() as i64)
        .unwrap_or_default()
}

/// Whether this account counts as a genuine person at `now` (the e-mail
/// check is separate: it needs the account record)
pub fn genuine_person(user: &User, now: i64) -> bool {
    let created = created_ms(&user.id);
    created > 0
        && now - created >= MIN_ACCOUNT_AGE_MS
        && user.bot.is_none()
        && !user.official
        && user.flags.unwrap_or_default()
            & (UserFlags::Deleted as i32 | UserFlags::Banned as i32 | UserFlags::SuspendedUntil as i32)
            == 0
}

/// The most times inside any window of `window` milliseconds
pub fn densest_window(times: &mut [i64], window: i64) -> usize {
    times.sort_unstable();
    let mut best = 0;
    let mut start = 0;
    for end in 0..times.len() {
        while times[end] - times[start] >= window {
            start += 1;
        }
        best = best.max(end - start + 1);
    }
    best
}

/// Whether a burst holds the server: more than the share of counted
/// members in one window
pub fn is_burst(burst: usize, counted: usize) -> bool {
    counted > 0 && burst * 100 > counted * BURST_SHARE_PERCENT
}

/// The count of one server
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberCount {
    /// Members of any kind
    pub members: usize,
    /// Genuine human members
    pub counted: usize,
    /// Most counted members who joined within one hour
    pub join_burst: usize,
    /// Most counted accounts made within one hour
    pub signup_burst: usize,
}

impl MemberCount {
    /// Whether the counted members look like a raid
    pub fn flagged(&self) -> bool {
        is_burst(self.join_burst, self.counted) || is_burst(self.signup_burst, self.counted)
    }
}

/// Count a server's genuine human members at `now`
pub async fn count_genuine_members(db: &Database, server_id: &str, now: i64) -> Result<MemberCount> {
    let mut count = MemberCount::default();
    let mut joined: Vec<i64> = vec![];
    let mut made: Vec<i64> = vec![];
    let mut after: Option<String> = None;
    loop {
        let page = db.fetch_members_page(server_id, after.as_deref(), PAGE).await?;
        let Some(last) = page.last() else { break };
        after = Some(last.id.user.clone());
        count.members += page.len();

        // members long enough to count, with their join time
        let settled: Vec<(String, i64)> = page
            .iter()
            .map(|member| (member.id.user.clone(), epoch_ms(member.joined_at)))
            .filter(|(_, at)| now - at >= MIN_MEMBERSHIP_MS)
            .collect();
        let ids: Vec<String> = settled.iter().map(|(id, _)| id.clone()).collect();
        let people: HashSet<String> = db
            .fetch_users(&ids)
            .await?
            .into_iter()
            .filter(|user| genuine_person(user, now))
            .map(|user| user.id)
            .collect();
        let people: Vec<String> = ids.into_iter().filter(|id| people.contains(id)).collect();
        let confirmed = db.fetch_verified_account_ids(&people).await?;
        for (id, at) in settled {
            if confirmed.contains(&id) {
                joined.push(at);
                made.push(created_ms(&id));
            }
        }
        if (page.len() as i64) < PAGE {
            break;
        }
    }
    count.counted = joined.len();
    count.join_burst = densest_window(&mut joined, BURST_WINDOW_MS);
    count.signup_burst = densest_window(&mut made, BURST_WINDOW_MS);
    Ok(count)
}

/// What one check did
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommunityCheckOutcome {
    /// Nothing more this server can give (both given, or refused)
    Done,
    /// Below the count, or nothing new to give
    Counted,
    /// Held for staff
    Held,
    /// Gave these badges
    Granted(Vec<&'static str>),
    /// The owner can't receive a badge (bot, deleted, banned, suspended)
    OwnerSkipped,
}

fn new_check(server_id: &str, owner_id: &str, now: i64) -> CommunityBadgeCheck {
    CommunityBadgeCheck {
        id: server_id.to_string(),
        owner_id: owner_id.to_string(),
        members: 0,
        counted: 0,
        join_burst: 0,
        signup_burst: 0,
        status: CommunityCheckStatus::Clear,
        builder_to: None,
        big_to: None,
        checked_at: now,
        decided_by: None,
        decided_at: None,
    }
}

fn system_record(action: &str, server_id: &str, summary: String) -> AuditRecord {
    AuditRecord {
        actor_id: crate::BADGE_SYSTEM_ACTOR.to_string(),
        action: action.to_string(),
        target_kind: Some("server".to_string()),
        target_id: Some(server_id.to_string()),
        summary: Some(summary),
        ..Default::default()
    }
}

/// Give a badge from the system's own record, through the badge gate (with
/// its audit record). False when nothing was given: held already, or
/// taken away by staff (the system never gives that back).
pub async fn grant_by_system(db: &Database, user_id: &str, badge: &str, source_ref: &str, reason: &str) -> Result<bool> {
    if held_badges(db, user_id).await?.contains(badge) || revoked_by_a_person(db, user_id, badge).await? {
        return Ok(false);
    }
    match mutate_badge(
        db,
        &BadgeActor::system(),
        BadgeMutation {
            user_id: user_id.to_string(),
            badge: badge.to_string(),
            change: BadgeChange::Grant,
            reason: reason.to_string(),
            expires_at: None,
            source_ref: Some(source_ref.to_string()),
        },
    )
    .await
    {
        Ok(_) => Ok(true),
        // given at the same moment by another pass
        Err(error) if matches!(error.error_type, ErrorType::NoEffect) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Count one server and give its owner what it earned. Safe to run again:
/// a server gives each badge once.
pub async fn check_server_community(
    db: &Database,
    server_id: &str,
    thresholds: CommunityThresholds,
    now: i64,
) -> Result<CommunityCheckOutcome> {
    let server = match db.fetch_server(server_id).await {
        Ok(server) => server,
        Err(error) if matches!(error.error_type, ErrorType::NotFound | ErrorType::UnknownServer) => {
            return Ok(CommunityCheckOutcome::Done)
        }
        Err(error) => return Err(error),
    };
    let mut check = db
        .fetch_community_check(server_id)
        .await?
        .unwrap_or_else(|| new_check(server_id, &server.owner, now));
    if check.status == CommunityCheckStatus::Refused || (check.builder_to.is_some() && check.big_to.is_some()) {
        return Ok(CommunityCheckOutcome::Done);
    }

    let count = count_genuine_members(db, server_id, now).await?;
    check.owner_id = server.owner.clone();
    check.members = count.members as u32;
    check.counted = count.counted as u32;
    check.join_burst = count.join_burst as u32;
    check.signup_burst = count.signup_burst as u32;
    check.checked_at = now;

    let earned: Vec<&'static str> = size_badges(count.counted, thresholds)
        .iter()
        .copied()
        .filter(|badge| match *badge {
            "community_builder" => check.builder_to.is_none(),
            _ => check.big_to.is_none(),
        })
        .collect();
    if earned.is_empty() {
        db.save_community_check(&check).await?;
        return Ok(CommunityCheckOutcome::Counted);
    }

    if count.flagged() && check.status != CommunityCheckStatus::Released {
        let first = check.status != CommunityCheckStatus::Held;
        check.status = CommunityCheckStatus::Held;
        if first {
            record_platform_audit(
                db,
                system_record(
                    "badge.system.community.hold",
                    server_id,
                    format!(
                        "held for review: {} counted, {} joined within one hour, {} accounts made within one hour",
                        count.counted, count.join_burst, count.signup_burst
                    ),
                ),
                AuditOutcome::Success,
            )
            .await?;
        }
        db.save_community_check(&check).await?;
        return Ok(CommunityCheckOutcome::Held);
    }

    let owner = db.fetch_user(&server.owner).await?;
    if owner.bot.is_some()
        || owner.flags.unwrap_or_default()
            & (UserFlags::Deleted as i32 | UserFlags::Banned as i32 | UserFlags::SuspendedUntil as i32)
            != 0
    {
        db.save_community_check(&check).await?;
        return Ok(CommunityCheckOutcome::OwnerSkipped);
    }

    let mut granted = vec![];
    for badge in earned {
        let reason = format!("{} genuine human members on server {server_id}", count.counted);
        if grant_by_system(db, &owner.id, badge, &format!("server:{server_id}"), &reason).await? {
            granted.push(badge);
        }
        // given now, held already (old rule) or taken away by staff: this
        // server has given it either way
        match badge {
            "community_builder" => check.builder_to = Some(owner.id.clone()),
            _ => check.big_to = Some(owner.id.clone()),
        }
    }
    if check.status == CommunityCheckStatus::Held {
        check.status = CommunityCheckStatus::Clear;
    }
    db.save_community_check(&check).await?;
    Ok(if granted.is_empty() {
        CommunityCheckOutcome::Counted
    } else {
        CommunityCheckOutcome::Granted(granted)
    })
}

/// What one crond pass did
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommunityPassReport {
    pub servers: usize,
    pub granted: usize,
    pub held: usize,
    pub failures: usize,
}

/// Look at every server with enough members of any kind. Each server's
/// failure is logged and the pass goes on.
pub async fn community_badge_pass(db: &Database, thresholds: CommunityThresholds, now: i64) -> Result<CommunityPassReport> {
    let mut report = CommunityPassReport::default();
    for (server_id, _) in db.fetch_server_member_totals(thresholds.builder).await? {
        report.servers += 1;
        match check_server_community(db, &server_id, thresholds, now).await {
            Ok(CommunityCheckOutcome::Granted(badges)) => report.granted += badges.len(),
            Ok(CommunityCheckOutcome::Held) => report.held += 1,
            Ok(_) => {}
            Err(error) => {
                report.failures += 1;
                log::error!("community badges for {server_id}: {error:?}");
            }
        }
    }
    Ok(report)
}

/// Staff look at a held server: `release` lets the badges through (given
/// at once if the count still earns them), otherwise the server gives no
/// community badge until released. Needs `platform.badges.manage`; never
/// for a server the staff member owns. Written to the platform audit log,
/// refusals too.
pub async fn decide_community_check(
    db: &Database,
    actor: &BadgeActor,
    server_id: &str,
    release: bool,
    reason: &str,
    thresholds: CommunityThresholds,
    now: i64,
) -> Result<CommunityBadgeCheck> {
    let word = if release { "release" } else { "refuse" };
    let reason = reason.trim();
    let reason_ok = (3..=500).contains(&reason.chars().count());
    let mut record = AuditRecord {
        actor_id: actor.id.clone(),
        action: format!("staff.badge.community.{word}"),
        permission: Some(StaffPermission::BadgesManage),
        target_kind: Some("server".to_string()),
        target_id: Ulid::from_string(server_id).ok().map(|_| server_id.to_string()),
        reason: reason_ok.then(|| reason.to_string()),
        staff_session_id: actor.session_id.clone(),
        summary: None,
    };
    let refuse = |code: &str| format!("community {word} denied:{code}");

    if !actor.staff {
        record.summary = Some(refuse("no_permission"));
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(MissingPlatformPermission {
            permission: StaffPermission::BadgesManage.name().to_string()
        }));
    }
    let Some(mut check) = db.fetch_community_check(server_id).await? else {
        record.summary = Some(refuse("unknown_server"));
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotFound));
    };
    if check.owner_id == actor.id {
        record.summary = Some(refuse("self"));
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    let allowed = if release {
        matches!(check.status, CommunityCheckStatus::Held | CommunityCheckStatus::Refused)
    } else {
        check.status != CommunityCheckStatus::Refused && (check.builder_to.is_none() || check.big_to.is_none())
    };
    if !allowed {
        record.summary = Some(refuse("not_pending"));
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    }
    if !reason_ok {
        record.summary = Some(refuse("bad_reason"));
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length("reason", 3, 500),
        ]));
    }
    record.summary = Some(format!(
        "community {word}: {} counted, join burst {}, signup burst {}",
        check.counted, check.join_burst, check.signup_burst
    ));
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    check.status = if release {
        CommunityCheckStatus::Released
    } else {
        CommunityCheckStatus::Refused
    };
    check.decided_by = Some(actor.id.clone());
    check.decided_at = Some(now);
    db.save_community_check(&check).await?;
    if release {
        check_server_community(db, server_id, thresholds, now).await?;
        if let Some(after) = db.fetch_community_check(server_id).await? {
            return Ok(after);
        }
    }
    Ok(check)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_exact() {
        let t = CommunityThresholds::default();
        assert!(size_badges(99, t).is_empty());
        assert_eq!(size_badges(100, t), ["community_builder"]);
        assert_eq!(size_badges(999, t), ["community_builder"]);
        assert_eq!(size_badges(1000, t), ["community_builder", "big_community"]);
    }

    #[test]
    fn the_densest_hour_is_found() {
        let mut none: Vec<i64> = vec![];
        assert_eq!(densest_window(&mut none, HOUR_MS), 0);
        // three inside one hour, two far apart
        let mut times = vec![10 * HOUR_MS, 0, HOUR_MS / 2, HOUR_MS - 1, 5 * HOUR_MS];
        assert_eq!(densest_window(&mut times, HOUR_MS), 3);
        // exactly one hour apart is not inside the same hour
        let mut edge = vec![0, HOUR_MS];
        assert_eq!(densest_window(&mut edge, HOUR_MS), 1);
    }

    #[test]
    fn a_burst_is_more_than_a_quarter() {
        assert!(!is_burst(25, 100));
        assert!(is_burst(26, 100));
        assert!(!is_burst(0, 0));
        assert!(!MemberCount { members: 120, counted: 100, join_burst: 25, signup_burst: 10 }.flagged());
        assert!(MemberCount { members: 120, counted: 100, join_burst: 3, signup_burst: 26 }.flagged());
    }
}
