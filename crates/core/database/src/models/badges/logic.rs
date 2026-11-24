//! Badge catalogue and the only code that changes who holds a badge.
//!
//! `users.badges` stays the bitfield clients read (no extra query per
//! profile or message); it is a cache of the user's held `badge_grants`
//! and is only ever written by [`sync_user_badges`]. Plus, Early Adopter and
//! the time-on-Zrzur badges stay computed in [`User::get_badges`] from
//! trusted state.

use std::collections::HashSet;

use iso8601_timestamp::Timestamp;
use revolt_models::v0::UserBadges;
use revolt_result::{create_error, Result};
use ulid::Ulid;

use crate::{
    epoch_ms, BadgeAction, BadgeGrant, BadgeLogEntry, BadgeSource, BetaMember, BetaStatus,
    BugKind, BugReportQuery, BugStatus, CommunityItemStatus, CreatorAction, CreatorStanding, Database,
    PartialUser, User,
};

/// Actor id used when the system acts
pub const BADGE_SYSTEM_ACTOR: &str = "00000000000000000000000000";

/// How a badge is given out
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeGrantRule {
    /// Computed from trusted state on every read (Plus, Early Adopter);
    /// nobody can grant it
    Dynamic,
    /// Only the system gives it, from its own record (published designs,
    /// beta membership); staff change the record, not the badge
    Automatic,
    /// Staff with `badges.grant`, after the eligibility check
    Staff,
    /// A staff Owner with a fresh step-up (the `privileged` database flag
    /// no longer gives any badge right)
    Privileged,
    /// Nobody changes it on any path: Founder follows `[api.users]
    /// founder_id` (a host-level setting) and nothing else
    Locked,
    /// Follows an official appointment (`staff_appointments`, written by
    /// `zrzur-staff appoint` with two people); no badge permission gives it
    Appointment,
    /// Retired for good: no path gives it again. People who hold it keep it
    /// (shown as a retired badge); taking one away is an Owner change with a
    /// step-up and a second person, and the bit is never used again.
    Historical,
}

/// One badge in the catalogue
#[derive(Debug, Clone, Copy)]
pub struct BadgeDefinition {
    pub id: &'static str,
    pub bit: u32,
    pub rule: BadgeGrantRule,
    /// official, community, contribution, program, subscription, history
    pub category: &'static str,
    /// 0 regular, 1 rare, 2 top tier
    pub tier: u8,
    /// Lower is shown first
    pub priority: u16,
    /// The system may also grant it when a record proves it (Bug Hunter
    /// on a confirmed report)
    pub auto: bool,
    /// No longer given out; shown for people who have it
    pub retired: bool,
}

const fn def(
    id: &'static str,
    bit: u32,
    rule: BadgeGrantRule,
    category: &'static str,
    tier: u8,
    priority: u16,
) -> BadgeDefinition {
    BadgeDefinition {
        id,
        bit,
        rule,
        category,
        tier,
        priority,
        auto: false,
        retired: false,
    }
}

use BadgeGrantRule::*;

pub static BADGE_CATALOGUE: &[BadgeDefinition] = &[
    def("staff", UserBadges::Staff as u32, Appointment, "official", 2, 10),
    def("founder", UserBadges::Founder as u32, Locked, "official", 2, 5),
    def("developer", UserBadges::Developer as u32, Privileged, "official", 1, 20),
    def("platform_moderator", UserBadges::PlatformModeration as u32, Appointment, "official", 1, 25),
    def("partner", UserBadges::Partner as u32, Staff, "official", 1, 30),
    def("super_creator", UserBadges::SuperCreator as u32, Staff, "community", 2, 40),
    def("creator", UserBadges::Creator as u32, Automatic, "community", 1, 45),
    // creators of original content elsewhere (any platform): an application,
    // a staff content-quality review and a second person (Phase 5)
    def("content_creator", UserBadges::ContentCreator as u32, Staff, "program", 1, 44),
    def("elite_bug_hunter", UserBadges::EliteBugHunter as u32, Staff, "contribution", 2, 50),
    BadgeDefinition {
        auto: true,
        ..def("bug_hunter", UserBadges::BugHunter as u32, Staff, "contribution", 1, 55)
    },
    def("beta_tester", UserBadges::BetaTester as u32, Automatic, "program", 0, 60),
    // the team approves it after a completed cycle (owner, 2026-11-24);
    // people who earned it under the old automatic rule keep it
    def("beta_veteran", UserBadges::BetaVeteran as u32, Staff, "program", 1, 65),
    def("plus", UserBadges::Plus as u32, Dynamic, "subscription", 1, 70),
    def("early_supporter", UserBadges::Supporter as u32, Staff, "history", 1, 75),
    def("early_adopter", UserBadges::EarlyAdopter as u32, Dynamic, "history", 0, 80),
    def("verified_server_owner", UserBadges::VerifiedServerOwner as u32, Automatic, "community", 1, 46),
    def("verified_bot_developer", UserBadges::VerifiedBotDeveloper as u32, Automatic, "community", 1, 47),
    def("community_helper", UserBadges::CommunityHelper as u32, Staff, "contribution", 1, 52),
    def("big_community", UserBadges::BigCommunity as u32, Automatic, "community", 1, 57),
    def("community_builder", UserBadges::CommunityBuilder as u32, Automatic, "community", 0, 58),
    // computed from the account's creation time in full calendar years (UTC);
    // apps show the highest year reached (2 and 4 to 10 have no bit of their
    // own: they are drawn from the same `one_year` bit and the account id)
    def("three_years", UserBadges::ThreeYears as u32, Dynamic, "history", 1, 82),
    def("one_year", UserBadges::OneYear as u32, Dynamic, "history", 0, 83),
    // older Revolt bits: kept so nobody loses them, not given out any more
    BadgeDefinition {
        retired: true,
        ..def("translator", UserBadges::Translator as u32, Staff, "contribution", 0, 90)
    },
    BadgeDefinition {
        retired: true,
        ..def("responsible_disclosure", UserBadges::ResponsibleDisclosure as u32, Staff, "contribution", 1, 91)
    },
    BadgeDefinition {
        retired: true,
        ..def("supporter", UserBadges::ActiveSupporter as u32, Staff, "history", 0, 92)
    },
    // retired for good (owner, 2026-11-24): never given on any path, the bit
    // stays reserved; nobody held Saha Champion when it was retired
    BadgeDefinition {
        retired: true,
        ..def("saha_champion", UserBadges::SahaChampion as u32, Historical, "community", 1, 48)
    },
    BadgeDefinition {
        retired: true,
        ..def("paw", UserBadges::Paw as u32, Historical, "history", 0, 93)
    },
    BadgeDefinition {
        retired: true,
        ..def("reserved_1", UserBadges::ReservedRelevantJokeBadge1 as u32, Privileged, "history", 0, 94)
    },
    BadgeDefinition {
        retired: true,
        ..def("reserved_2", UserBadges::ReservedRelevantJokeBadge2 as u32, Privileged, "history", 0, 95)
    },
];

/// Look up a badge by id
pub fn badge_definition(id: &str) -> Option<&'static BadgeDefinition> {
    BADGE_CATALOGUE.iter().find(|b| b.id == id)
}

/// Bits that only [`sync_user_badges`] may write
pub fn managed_badge_bits() -> u32 {
    BADGE_CATALOGUE
        .iter()
        .filter(|b| b.rule != Dynamic)
        .fold(0, |bits, b| bits | b.bit)
}

/// The Founder bit as `[api.users] founder_id` says: once it is set, only
/// that account shows it (an old grant record of anyone else is history);
/// unset, the stored bit is left as it is.
pub fn founder_bit(badges: u32, founder: Option<&str>, user_id: &str) -> u32 {
    let bit = UserBadges::Founder as u32;
    match founder {
        Some(id) if id == user_id => badges | bit,
        Some(_) => badges & !bit,
        None => badges,
    }
}

/// Id of a grant record
pub fn badge_grant_id(user_id: &str, badge: &str) -> String {
    format!("{user_id}:{badge}")
}

fn now_ms() -> i64 {
    epoch_ms(Timestamp::now_utc())
}

impl BadgeGrant {
    /// Held right now (not revoked, not expired)
    pub fn held(&self, now: i64) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|at| at > now)
    }
}

async fn write_log(
    db: &Database,
    user_id: &str,
    badge: &str,
    actor: &str,
    action: BadgeAction,
    reason: Option<String>,
) -> Result<()> {
    db.insert_badge_log(&BadgeLogEntry {
        id: format!("{user_id}:{}", Ulid::new()),
        user_id: user_id.to_string(),
        badge: badge.to_string(),
        actor_id: actor.to_string(),
        action,
        reason,
        created_at: now_ms(),
    })
    .await
}

/// Bring `users.badges` in line with the user's grants: record bits set
/// before grants existed (never dropping one), end expired grants, then
/// write and publish the bitfield if it changed.
pub async fn sync_user_badges(db: &Database, user_id: &str) -> Result<User> {
    let mut user = db.fetch_user(user_id).await?;
    let mut grants = db.fetch_badge_grants(user_id).await?;
    let now = now_ms();
    let managed = managed_badge_bits();
    let stored = user.badges.unwrap_or_default() as u32;

    // legacy bits: a grant record for every set bit that has none
    let recorded: u32 = grants
        .iter()
        .filter_map(|g| badge_definition(&g.badge))
        .fold(0, |bits, b| bits | b.bit);
    for definition in BADGE_CATALOGUE
        .iter()
        .filter(|b| b.rule != Dynamic && stored & b.bit != 0 && recorded & b.bit == 0)
    {
        let grant = BadgeGrant {
            id: badge_grant_id(user_id, definition.id),
            user_id: user_id.to_string(),
            badge: definition.id.to_string(),
            source: BadgeSource::Legacy,
            source_ref: None,
            granted_by: BADGE_SYSTEM_ACTOR.to_string(),
            granted_at: now,
            reason: None,
            expires_at: None,
            revoked_at: None,
            revoked_by: None,
            revoke_reason: None,
            hidden: false,
        };
        if db.insert_badge_grant(&grant).await? {
            write_log(db, user_id, definition.id, BADGE_SYSTEM_ACTOR, BadgeAction::Grant, Some("legacy".into())).await?;
            grants.push(grant);
        }
    }

    // expired grants end here, with a log entry
    for grant in grants.iter_mut() {
        if grant.revoked_at.is_none() && grant.expires_at.is_some_and(|at| at <= now) {
            grant.revoked_at = Some(now);
            grant.revoked_by = Some(BADGE_SYSTEM_ACTOR.to_string());
            grant.revoke_reason = Some("expired".to_string());
            db.save_badge_grant(grant).await?;
            write_log(db, user_id, &grant.badge, BADGE_SYSTEM_ACTOR, BadgeAction::Expire, None).await?;
        }
    }

    let shown: u32 = grants
        .iter()
        .filter(|g| g.held(now) && !g.hidden)
        .filter_map(|g| badge_definition(&g.badge))
        .fold(0, |bits, b| bits | b.bit);
    let next = (stored & !managed) | shown;
    if next != stored {
        db.update_user(
            user_id,
            &PartialUser {
                badges: Some(next as i32),
                ..Default::default()
            },
            vec![],
        )
        .await?;
        user.badges = Some(next as i32);
        crate::publish_badges(&user).await;
    }
    Ok(user)
}

/// Give a badge. Fails with `NoEffect` if it is already held, so a badge is
/// never granted twice; a revoked or expired grant is renewed.
#[allow(clippy::too_many_arguments)]
pub async fn grant_badge(
    db: &Database,
    user_id: &str,
    badge: &str,
    source: BadgeSource,
    source_ref: Option<String>,
    actor: &str,
    reason: Option<String>,
    expires_at: Option<i64>,
) -> Result<BadgeGrant> {
    let definition = badge_definition(badge).ok_or_else(|| create_error!(NotFound))?;
    if definition.rule == Dynamic {
        return Err(create_error!(InvalidOperation));
    }
    let now = now_ms();
    if expires_at.is_some_and(|at| at <= now) {
        return Err(create_error!(FailedValidation {
            error: "expiry is in the past".to_string()
        }));
    }
    let id = badge_grant_id(user_id, badge);
    let existing = db
        .fetch_badge_grants(user_id)
        .await?
        .into_iter()
        .find(|g| g.id == id);
    if existing.as_ref().is_some_and(|g| g.held(now)) {
        return Err(create_error!(NoEffect));
    }
    let grant = BadgeGrant {
        id,
        user_id: user_id.to_string(),
        badge: badge.to_string(),
        source,
        source_ref,
        granted_by: actor.to_string(),
        granted_at: now,
        reason: reason.clone(),
        expires_at,
        revoked_at: None,
        revoked_by: None,
        revoke_reason: None,
        hidden: existing.as_ref().is_some_and(|g| g.hidden),
    };
    if existing.is_some() {
        db.save_badge_grant(&grant).await?;
    } else if !db.insert_badge_grant(&grant).await? {
        // someone granted it at the same moment
        return Err(create_error!(NoEffect));
    }
    write_log(db, user_id, badge, actor, BadgeAction::Grant, reason).await?;
    sync_user_badges(db, user_id).await?;
    Ok(grant)
}

/// Take a badge away. `NoEffect` if it is not held.
pub async fn revoke_badge(
    db: &Database,
    user_id: &str,
    badge: &str,
    actor: &str,
    reason: Option<String>,
) -> Result<BadgeGrant> {
    let now = now_ms();
    let id = badge_grant_id(user_id, badge);
    // a bit set before grants were tracked becomes a grant first
    sync_user_badges(db, user_id).await?;
    let mut grant = db
        .fetch_badge_grants(user_id)
        .await?
        .into_iter()
        .find(|g| g.id == id && g.held(now))
        .ok_or_else(|| create_error!(NoEffect))?;
    grant.revoked_at = Some(now);
    grant.revoked_by = Some(actor.to_string());
    grant.revoke_reason = reason.clone();
    db.save_badge_grant(&grant).await?;
    write_log(db, user_id, badge, actor, BadgeAction::Revoke, reason).await?;
    sync_user_badges(db, user_id).await?;
    Ok(grant)
}

/// Show or hide one of your own badges on your profile. Hiding never adds
/// anything, so it can't be used to fake a badge.
pub async fn set_badge_hidden(db: &Database, user_id: &str, badge: &str, hidden: bool) -> Result<()> {
    let now = now_ms();
    let id = badge_grant_id(user_id, badge);
    sync_user_badges(db, user_id).await?;
    let mut grant = db
        .fetch_badge_grants(user_id)
        .await?
        .into_iter()
        .find(|g| g.id == id && g.held(now))
        .ok_or_else(|| create_error!(NotFound))?;
    if grant.hidden == hidden {
        return Err(create_error!(NoEffect));
    }
    grant.hidden = hidden;
    db.save_badge_grant(&grant).await?;
    let action = if hidden { BadgeAction::Hide } else { BadgeAction::Show };
    write_log(db, user_id, badge, user_id, action, None).await?;
    sync_user_badges(db, user_id).await?;
    Ok(())
}

/// Badges a user holds right now
pub async fn held_badges(db: &Database, user_id: &str) -> Result<HashSet<String>> {
    let now = now_ms();
    Ok(db
        .fetch_badge_grants(user_id)
        .await?
        .into_iter()
        .filter(|g| g.held(now))
        .map(|g| g.badge)
        .collect())
}

/// Designs a creator has published and whether that earns Creator: an
/// active creator record (set by staff) and at least one published design
pub async fn creator_eligibility(db: &Database, user_id: &str) -> Result<(bool, usize)> {
    let role = db.fetch_platform_role(user_id).await?;
    let active = role.and_then(|r| r.creator) == Some(CreatorStanding::Active);
    let published = db
        .count_community_items_by_creator(user_id, &[CommunityItemStatus::Published])
        .await?;
    Ok((active && published >= 1, published))
}

/// Approved, published designs needed before staff can make someone a
/// Super Creator (a minimum: the team still chooses)
pub const SUPER_CREATOR_MIN_DESIGNS: usize = 25;

/// Days in the creator program needed before staff can make someone a
/// Super Creator
pub const SUPER_CREATOR_MIN_DAYS: u32 = 180;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// When someone joined the creator program: the first time staff made them
/// a creator (creator log), or, for creators from before that log, when
/// they got the Creator badge. `None` when there is no record of either.
pub async fn creator_program_since(db: &Database, user_id: &str) -> Result<Option<i64>> {
    let joined = db
        .fetch_creator_logs(user_id)
        .await?
        .into_iter()
        .filter(|log| log.action == CreatorAction::Grant)
        .map(|log| log.created_at)
        .min();
    if joined.is_some() {
        return Ok(joined);
    }
    Ok(db
        .fetch_badge_grants(user_id)
        .await?
        .into_iter()
        .filter(|grant| grant.badge == "creator")
        .map(|grant| grant.granted_at)
        .min())
}

/// Full days from `since` to `now`
pub fn days_since(since: i64, now: i64) -> u32 {
    ((now - since).max(0) / DAY_MS) as u32
}

/// Give Creator once someone's designs and creator record earn it. Creator
/// is permanent (owner, 2026-11-24): losing the creator record or the
/// designs never takes it away; only staff can, for fraud or infringement
/// (with a second person), and then the system never gives it back. The
/// grant goes through the badge gate (`mutate_badge`, system path) and is
/// written to the platform audit log.
pub async fn sync_creator_badge(db: &Database, user_id: &str) -> Result<()> {
    let (eligible, _) = creator_eligibility(db, user_id).await?;
    if eligible {
        crate::grant_by_system(db, user_id, "creator", "designs", "first approved design published").await?;
    }
    Ok(())
}

/// Feedback reports that count towards Beta Veteran
pub const BETA_VETERAN_FEEDBACK: usize = 3;
/// Confirmed bug reports that count towards Beta Veteran
pub const BETA_VETERAN_BUGS: usize = 1;

/// What a tester sent during their last beta cycle (from joining until it
/// ended): (substantive feedback, confirmed bug reports). Feedback counts
/// once a reviewer has looked at it and kept it (triaged, confirmed or
/// fixed); a bug or security report counts once confirmed or fixed.
/// Duplicates, "not a bug" and unread reports never count.
pub async fn beta_cycle_stats(db: &Database, member: &BetaMember) -> Result<(usize, usize)> {
    let Some(start) = member.joined_at else {
        return Ok((0, 0));
    };
    let end = member.ended_at.unwrap_or(i64::MAX);
    let reports = db
        .fetch_bug_reports(BugReportQuery {
            author_id: Some(&member.id),
            include_security: true,
            limit: 500,
            ..Default::default()
        })
        .await?;
    let in_cycle = reports.iter().filter(|r| r.created_at >= start && r.created_at <= end);
    let mut feedback = 0;
    let mut bugs = 0;
    for report in in_cycle {
        match report.kind {
            BugKind::Feedback
                if matches!(report.status, BugStatus::Triaged | BugStatus::Confirmed | BugStatus::Fixed) =>
            {
                feedback += 1
            }
            BugKind::Bug | BugKind::Security
                if matches!(report.status, BugStatus::Confirmed | BugStatus::Fixed) =>
            {
                bugs += 1
            }
            _ => {}
        }
    }
    Ok((feedback, bugs))
}

/// Verified bug reports of a user: (all verified, verified security
/// reports rated high or critical)
pub async fn bug_hunter_stats(db: &Database, user_id: &str) -> Result<(usize, usize)> {
    let verified = db
        .fetch_bug_reports(BugReportQuery {
            author_id: Some(user_id),
            statuses: vec![BugStatus::Confirmed, BugStatus::Fixed],
            include_security: true,
            limit: 200,
            ..Default::default()
        })
        .await?;
    let serious = verified
        .iter()
        .filter(|r| {
            r.kind == BugKind::Security
                && matches!(r.severity.as_deref(), Some("high" | "critical"))
        })
        .count();
    Ok((verified.len(), serious))
}

/// Verified reports that make someone a candidate for Elite Bug Hunter
/// (staff still decide; it is never automatic)
pub const ELITE_MIN_VERIFIED: usize = 10;

/// Whether staff may give a badge to this user now. Returns a reason when
/// not.
pub async fn badge_eligibility(db: &Database, user_id: &str, badge: &str) -> Result<Option<String>> {
    Ok(match badge {
        // the system's grant: the record must back it at that moment too
        "creator" => {
            let (eligible, _) = creator_eligibility(db, user_id).await?;
            (!eligible).then(|| "needs an active creator record and a published design".to_string())
        }
        "super_creator" => {
            let (active, published) = creator_eligibility(db, user_id).await?;
            let since = creator_program_since(db, user_id).await?;
            if !held_badges(db, user_id).await?.contains("creator") {
                Some("needs the Creator badge".into())
            } else if !active {
                Some("needs an active creator record".into())
            } else if published < SUPER_CREATOR_MIN_DESIGNS {
                Some(format!(
                    "needs {SUPER_CREATOR_MIN_DESIGNS} approved, published designs (has {published})"
                ))
            } else if since.is_none_or(|since| days_since(since, now_ms()) < SUPER_CREATOR_MIN_DAYS) {
                Some(format!("needs {SUPER_CREATOR_MIN_DAYS} days in the creator program"))
            } else {
                None
            }
        }
        "beta_veteran" => match db.fetch_beta_member(user_id).await? {
            Some(member) if member.status == BetaStatus::Completed => {
                let (feedback, bugs) = beta_cycle_stats(db, &member).await?;
                (feedback < BETA_VETERAN_FEEDBACK && bugs < BETA_VETERAN_BUGS).then(|| {
                    format!(
                        "needs {BETA_VETERAN_FEEDBACK} reviewed feedback reports or {BETA_VETERAN_BUGS} confirmed bug report in the completed cycle (has {feedback} and {bugs})"
                    )
                })
            }
            _ => Some("needs a completed beta cycle".into()),
        },
        // only a partner application a reviewer approved backs it: no
        // console, command or line gives Partner on its own (Phase 4)
        "partner" => (!crate::program_application_backs(db, user_id, crate::BadgeProgram::Partner).await?)
            .then(|| "needs a partner application approved by a reviewer".to_string()),
        // the same for Content Creator: the application, its reviewed
        // figures and the quality review back it (Phase 5)
        "content_creator" => (!crate::program_application_backs(db, user_id, crate::BadgeProgram::ContentCreator).await?)
            .then(|| "needs a content creator application approved by a reviewer".to_string()),
        "verified_server_owner" => (!crate::owns_verified_server(db, user_id).await?)
            .then(|| "needs a verified server they own".to_string()),
        "verified_bot_developer" => (!crate::owns_verified_bot(db, user_id).await?)
            .then(|| "needs a verified bot they own that is not suspended".to_string()),
        "elite_bug_hunter" => {
            let (verified, serious) = bug_hunter_stats(db, user_id).await?;
            if !held_badges(db, user_id).await?.contains("bug_hunter") {
                Some("needs the Bug Hunter badge".into())
            } else if verified < ELITE_MIN_VERIFIED && serious == 0 {
                Some(format!(
                    "needs {ELITE_MIN_VERIFIED} verified reports or a verified high/critical security report"
                ))
            } else {
                None
            }
        }
        _ => None,
    })
}

/// Whether a user may use a beta feature. This is the only check; the
/// badge never grants anything.
pub async fn beta_access(db: &Database, user: &User, feature: &str) -> Result<bool> {
    if user.bot.is_some() {
        return Ok(false);
    }
    let Some(member) = db.fetch_beta_member(&user.id).await? else {
        return Ok(false);
    };
    if member.status != BetaStatus::Active {
        return Ok(false);
    }
    Ok(db
        .fetch_beta_features()
        .await?
        .into_iter()
        .any(|f| f.id == feature && f.enabled && (f.all_testers || member.features.contains(&f.id))))
}

/// Features a member can use right now
pub async fn beta_features_for(db: &Database, member: &BetaMember) -> Result<Vec<String>> {
    if member.status != BetaStatus::Active {
        return Ok(vec![]);
    }
    Ok(db
        .fetch_beta_features()
        .await?
        .into_iter()
        .filter(|f| f.enabled && (f.all_testers || member.features.contains(&f.id)))
        .map(|f| f.id)
        .collect())
}

/// Beta Tester follows active membership. Beta Veteran is no longer given
/// here: the team approves it after a completed cycle (`badge_eligibility`
/// checks the cycle's reports); leaving never takes it away.
pub async fn sync_beta_badges(db: &Database, member: &BetaMember, actor: &str) -> Result<()> {
    let held = held_badges(db, &member.id).await?;
    let active = member.status == BetaStatus::Active;
    if active && !held.contains("beta_tester") {
        grant_badge(db, &member.id, "beta_tester", BadgeSource::System, Some("beta".into()), actor, None, None).await?;
    }
    if !active && held.contains("beta_tester") {
        revoke_badge(db, &member.id, "beta_tester", actor, Some(format!("{:?}", member.status))).await?;
    }
    Ok(())
}

/// End expired grants every few minutes
pub async fn badge_worker(db: Database) {
    loop {
        match db.fetch_expired_badge_grants(now_ms()).await {
            Ok(grants) => {
                let users: HashSet<String> = grants.into_iter().map(|g| g.user_id).collect();
                for user in users {
                    if let Err(error) = sync_user_badges(&db, &user).await {
                        log::error!("badges: {error:?}");
                    }
                }
            }
            Err(error) => log::error!("badges: {error:?}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    }
}
