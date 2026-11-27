//! The checklist of each program: what the server can measure, what it
//! found, and what a reviewer checks by hand (`met: None`). Meeting every
//! measurable line only lets someone apply; nothing is ever given from the
//! numbers.

use std::collections::HashMap;

use revolt_config::ApiBadgePrograms;
use revolt_models::v0::{AgeBand, BotFlags};
use revolt_result::{ErrorType, Result};
use ulid::Ulid;

use crate::{
    count_genuine_members, genuine_person, partner_activity, week_of, BadgeApplication, BadgeProgram,
    CommunityCheckStatus, Database, EligibilityItem, User, DAY_MS,
};

/// Content Creator: the seven published quality criteria a reviewer checks
/// on the first approval (owner's brief, section 6)
pub const CREATOR_QUALITY: [&str; 7] = [
    "originality",
    "effort",
    "consistency",
    "authenticity",
    "safety",
    "honesty",
    "rights",
];

/// The checklist for one program and target
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramEligibility {
    pub items: Vec<EligibilityItem>,
    /// Current owner of the server or bot, when it exists
    pub owner_id: Option<String>,
}

impl ProgramEligibility {
    /// Codes of the measurable lines that are not met
    pub fn missing(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|item| item.met == Some(false))
            .map(|item| item.code.clone())
            .collect()
    }

    /// Codes of every line that is not met yet, the ones a person checks
    /// included (an approval that needs every line settled)
    pub fn unsettled(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|item| item.met != Some(true))
            .map(|item| item.code.clone())
            .collect()
    }

    pub fn met(&self, code: &str) -> Option<bool> {
        self.items.iter().find(|item| item.code == code).and_then(|item| item.met)
    }
}

fn line(code: &str, met: Option<bool>, value: Option<i64>, needed: Option<i64>) -> EligibilityItem {
    EligibilityItem {
        code: code.to_string(),
        met,
        value,
        needed,
    }
}

/// A count against its minimum
pub fn threshold_line(code: &str, value: i64, needed: i64) -> EligibilityItem {
    line(code, Some(value >= needed), Some(value), Some(needed))
}

/// When something was made, from its ULID (0 when it is not one)
pub fn made_at(id: &str) -> i64 {
    Ulid::from_string(id).map(|id| id.timestamp_ms() as i64).unwrap_or_default()
}

/// Full days since a ULID was made
pub fn days_old(id: &str, now: i64) -> i64 {
    let made = made_at(id);
    if made == 0 {
        0
    } else {
        (now - made).div_euclid(DAY_MS)
    }
}

/// The lines about the person applying: a person, a confirmed e-mail, 18 or
/// older by their own statement (legal BV-07: these programmes represent a
/// server, an entity or a bot)
pub async fn applicant_lines(db: &Database, applicant: &User) -> Result<Vec<EligibilityItem>> {
    let person = applicant.bot.is_none();
    let confirmed = person
        && db
            .fetch_verified_account_ids(&[applicant.id.clone()])
            .await?
            .contains(&applicant.id);
    let adult = person
        && match db.fetch_account(&applicant.id).await {
            Ok(account) => account.age_band() == AgeBand::AdultDeclared,
            Err(error) if matches!(error.error_type, ErrorType::NotFound | ErrorType::UnknownUser) => false,
            Err(error) => return Err(error),
        };
    Ok(vec![
        line("email", Some(confirmed), None, None),
        line("adult", Some(adult), None, None),
    ])
}

/// Servers a bot is in that count for Verified Bot Developer: each has at
/// least `bot_server_min_humans` genuine human members and no burst (the
/// Phase 3 checks), an owner who is a genuine person with a confirmed
/// e-mail and is not the bot's owner, and no owner counts for more than
/// `bot_servers_per_owner` (so one person's servers, or their other
/// accounts', never add up). Stops once `bot_servers` are found.
pub async fn legit_bot_servers(db: &Database, bot_id: &str, bot_owner: &str, programs: &ApiBadgePrograms, now: i64) -> Result<usize> {
    let mut per_owner: HashMap<String, usize> = HashMap::new();
    let mut found = 0;
    for membership in db.fetch_all_memberships(bot_id).await? {
        if found >= programs.bot_servers {
            break;
        }
        let server = match db.fetch_server(&membership.id.server).await {
            Ok(server) => server,
            Err(error) if matches!(error.error_type, ErrorType::NotFound | ErrorType::UnknownServer) => continue,
            Err(error) => return Err(error),
        };
        if server.owner == bot_owner || per_owner.get(&server.owner).copied().unwrap_or(0) >= programs.bot_servers_per_owner {
            continue;
        }
        let owner = match db.fetch_user(&server.owner).await {
            Ok(owner) => owner,
            Err(error) if matches!(error.error_type, ErrorType::NotFound | ErrorType::UnknownUser) => continue,
            Err(error) => return Err(error),
        };
        if !genuine_person(&owner, now) || !db.fetch_verified_account_ids(&[owner.id.clone()]).await?.contains(&owner.id) {
            continue;
        }
        if db
            .fetch_community_check(&server.id)
            .await?
            .is_some_and(|check| matches!(check.status, CommunityCheckStatus::Held | CommunityCheckStatus::Refused))
        {
            continue;
        }
        let count = count_genuine_members(db, &server.id, now).await?;
        if count.counted < programs.bot_server_min_humans || count.flagged() {
            continue;
        }
        *per_owner.entry(server.owner.clone()).or_default() += 1;
        found += 1;
    }
    Ok(found)
}

/// The checklist of one program for one person and target. `exception`:
/// Verified Server as an official entity below the numbers (the team checks
/// members and age by hand). `ownership_verified`: the DNS or website
/// challenge of the application passed.
#[allow(clippy::too_many_arguments)]
pub async fn program_eligibility(
    db: &Database,
    programs: &ApiBadgePrograms,
    program: BadgeProgram,
    applicant: &User,
    target_id: &str,
    exception: bool,
    ownership_verified: bool,
    now: i64,
) -> Result<ProgramEligibility> {
    let mut eligibility = ProgramEligibility::default();
    let items = &mut eligibility.items;
    if program.targets_person() {
        // Content Creator: about the applicant; nothing here can be
        // measured before an application (a reviewer looks at the channel)
        if target_id != applicant.id {
            items.push(line("target", Some(false), None, None));
            return Ok(eligibility);
        }
        eligibility.owner_id = Some(applicant.id.clone());
        items.extend(creator_lines(programs, None, now));
        items.extend(applicant_lines(db, applicant).await?);
        return Ok(eligibility);
    }
    if program.targets_bot() {
        let bot = match db.fetch_bot(target_id).await {
            Ok(bot) => bot,
            Err(error) if matches!(error.error_type, ErrorType::NotFound) => {
                items.push(line("target", Some(false), None, None));
                return Ok(eligibility);
            }
            Err(error) => return Err(error),
        };
        eligibility.owner_id = Some(bot.owner.clone());
        let flags = bot.flags.unwrap_or_default();
        items.push(line("owner", Some(bot.owner == applicant.id), None, None));
        items.push(line("bot_active", Some(flags & BotFlags::Suspended as i32 == 0), None, None));
        // the bot's age and servers never stop anyone from asking (owner,
        // 2026-11-27: a bot in 25 servers could not apply because fewer of
        // them counted): reached, they show as met; below, the team weighs
        // them in the review with the numbers in front of it
        let age = days_old(&bot.id, now);
        items.push(line("bot_age", (age >= programs.bot_age_days).then_some(true), Some(age), Some(programs.bot_age_days)));
        let servers = legit_bot_servers(db, &bot.id, &bot.owner, programs, now).await? as i64;
        let needed = programs.bot_servers as i64;
        items.push(line("legit_servers", (servers >= needed).then_some(true), Some(servers), Some(needed)));
    } else {
        let server = match db.fetch_server(target_id).await {
            Ok(server) => server,
            Err(error) if matches!(error.error_type, ErrorType::NotFound | ErrorType::UnknownServer) => {
                items.push(line("target", Some(false), None, None));
                return Ok(eligibility);
            }
            Err(error) => return Err(error),
        };
        eligibility.owner_id = Some(server.owner.clone());
        items.push(line("owner", Some(server.owner == applicant.id), None, None));
        let count = count_genuine_members(db, &server.id, now).await?;
        let held = db
            .fetch_community_check(&server.id)
            .await?
            .is_some_and(|check| matches!(check.status, CommunityCheckStatus::Held | CommunityCheckStatus::Refused));
        let (members, age_days) = match program {
            BadgeProgram::Partner => (programs.partner_members, programs.partner_age_days),
            _ => (programs.verified_server_members, programs.verified_server_age_days),
        };
        let age = days_old(&server.id, now);
        if program == BadgeProgram::VerifiedServer && exception {
            // official entity below the numbers: the team checks by hand
            items.push(line("members", None, Some(count.counted as i64), Some(members as i64)));
            items.push(line("age", None, Some(age), Some(age_days)));
        } else {
            items.push(threshold_line("members", count.counted as i64, members as i64));
            items.push(threshold_line("age", age, age_days));
        }
        items.push(line("no_burst", Some(!held && !count.flagged()), None, None));
        if program == BadgeProgram::Partner {
            let span = programs.partner_weeks.max(1) as i64;
            let rows = db.fetch_server_activity_weeks(&server.id, week_of(now) - span).await?;
            let activity = partner_activity(&rows, programs, now);
            items.push(threshold_line("activity_weeks", activity.weeks as i64, span));
            items.push(threshold_line("weekly_active", activity.avg_active as i64, programs.partner_weekly_active as i64));
            items.push(threshold_line(
                "weekly_contributors",
                activity.avg_contributors as i64,
                programs.partner_weekly_contributors as i64,
            ));
            // not enough joins to say: the reviewer looks at it
            items.push(match activity.retention_percent {
                Some(percent) => threshold_line("retention", percent as i64, programs.partner_retention_percent as i64),
                None => line("retention", None, None, Some(programs.partner_retention_percent as i64)),
            });
        } else {
            items.push(line("ownership", Some(ownership_verified), None, None));
        }
    }
    items.extend(applicant_lines(db, applicant).await?);
    Ok(eligibility)
}

/// Content Creator's lines from an application (or before one: every line
/// waits for a person). `channel` and `ownership` (the code was asked for)
/// are the applicant's part; followers, activity, works and quality come
/// only from what a reviewer recorded, and stay "a person checks" until
/// then. A follower figure older than `creator_figures_fresh_days` counts
/// as not looked at.
pub fn creator_lines(programs: &ApiBadgePrograms, application: Option<&BadgeApplication>, now: i64) -> Vec<EligibilityItem> {
    let channel = application.and_then(|a| a.creator.as_ref());
    let figures = application.and_then(|a| a.creator_figures.as_ref());
    let ownership = application.and_then(|a| a.ownership.as_ref());
    let mut items = vec![];
    if application.is_some() {
        let complete = channel.is_some_and(|c| {
            !c.url.is_empty() && (c.works.len() as i64 >= programs.creator_works || c.schedule.as_deref().is_some_and(|s| !s.trim().is_empty()))
        });
        items.push(line("channel", Some(complete), None, None));
    }
    items.push(match ownership {
        Some(o) if o.verified_at.is_some() => line("ownership", Some(true), None, None),
        // the code was asked for: a reviewer looks for it
        Some(_) => line("ownership", None, None, None),
        None if application.is_some() => line("ownership", Some(false), None, None),
        None => line("ownership", None, None, None),
    });
    let fresh = figures.filter(|f| now - f.seen_at <= programs.creator_figures_fresh_days * DAY_MS);
    items.push(match fresh {
        Some(f) => threshold_line("followers", f.followers, programs.creator_followers),
        None => line("followers", None, None, Some(programs.creator_followers)),
    });
    items.push(match figures {
        Some(f) => threshold_line("active_days", f.active_days, programs.creator_active_days),
        None => line("active_days", None, None, Some(programs.creator_active_days)),
    });
    items.push(match figures {
        Some(f) => line(
            "works",
            Some(f.works >= programs.creator_works || f.schedule),
            Some(f.works),
            Some(programs.creator_works),
        ),
        None => line("works", None, None, Some(programs.creator_works)),
    });
    items.push(match figures {
        Some(f) => {
            let met = CREATOR_QUALITY.iter().filter(|c| f.quality.iter().any(|q| q == *c)).count() as i64;
            threshold_line("quality", met, CREATOR_QUALITY.len() as i64)
        }
        None => line("quality", None, None, Some(CREATOR_QUALITY.len() as i64)),
    });
    items
}

/// The checklist of an application as it stands: the program's lines for
/// its target, and for Content Creator the lines from the application
/// itself (the channel, the profile code, what the reviewer recorded)
pub async fn application_checklist(
    db: &Database,
    programs: &ApiBadgePrograms,
    applicant: &User,
    application: &BadgeApplication,
    now: i64,
) -> Result<ProgramEligibility> {
    if application.program.targets_person() {
        let mut eligibility = ProgramEligibility {
            items: vec![],
            owner_id: Some(application.applicant_id.clone()),
        };
        if application.target_id != applicant.id {
            eligibility.items.push(line("target", Some(false), None, None));
            return Ok(eligibility);
        }
        eligibility.items.extend(creator_lines(programs, Some(application), now));
        eligibility.items.extend(applicant_lines(db, applicant).await?);
        return Ok(eligibility);
    }
    program_eligibility(
        db,
        programs,
        application.program,
        applicant,
        &application.target_id,
        application.exception,
        application.ownership.as_ref().is_some_and(|o| o.verified_at.is_some()),
        now,
    )
    .await
}
