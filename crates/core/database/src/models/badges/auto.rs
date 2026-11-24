//! Badges the system gives from its own records (a verified server or bot;
//! the size of a server is counted by crond, see `community.rs`), and the ones computed from the
//! account's age (Early Adopter, One Year … Ten Years). Each rule is safe to
//! run again: a badge already held is left alone.

use revolt_models::v0::{BotFlags, UserBadges, UserFlags};
use revolt_result::{ErrorType, Result};
use ulid::Ulid;

use crate::{
    grant_badge, held_badges, record_platform_audit, revoke_badge, AuditOutcome, AuditRecord,
    BadgeSource, Database, PartialUser, User, BADGE_SYSTEM_ACTOR,
};

/// Grants read in one go
const BATCH: usize = 500;

/// Server `flags` values
const SERVER_OFFICIAL: i32 = 1;
const SERVER_VERIFIED: i32 = 2;

/// Give a badge unless it is held already
async fn grant_once(db: &Database, user_id: &str, badge: &str, source_ref: &str) -> Result<()> {
    match grant_badge(
        db,
        user_id,
        badge,
        BadgeSource::System,
        Some(source_ref.to_string()),
        BADGE_SYSTEM_ACTOR,
        None,
        None,
    )
    .await
    {
        Err(error) if !matches!(error.error_type, ErrorType::NoEffect) => Err(error),
        _ => Ok(()),
    }
}

async fn follow(db: &Database, user_id: &str, badge: &str, earned: bool, source_ref: &str) -> Result<()> {
    let held = held_badges(db, user_id).await?.contains(badge);
    if earned && !held {
        grant_once(db, user_id, badge, source_ref).await?;
    } else if !earned && held {
        revoke_badge(db, user_id, badge, BADGE_SYSTEM_ACTOR, Some("no longer eligible".into())).await?;
    }
    Ok(())
}

/// The badge follows owning a verified server: after verifying, unverifying
/// or handing a server over
pub async fn sync_verified_server_owner(db: &Database, user_id: &str) -> Result<()> {
    let earned = db
        .fetch_owned_servers(user_id)
        .await?
        .iter()
        .any(|server| server.flags.unwrap_or_default() & (SERVER_OFFICIAL | SERVER_VERIFIED) != 0);
    follow(db, user_id, "verified_server_owner", earned, "server").await
}

/// The badge follows owning a verified bot that isn't suspended
pub async fn sync_verified_bot_developer(db: &Database, user_id: &str) -> Result<()> {
    let verified = BotFlags::Verified as i32;
    let suspended = BotFlags::Suspended as i32;
    let earned = db.fetch_bots_by_user(user_id).await?.iter().any(|bot| {
        let flags = bot.flags.unwrap_or_default();
        flags & verified != 0 && flags & suspended == 0
    });
    follow(db, user_id, "verified_bot_developer", earned, "bot").await
}

//// Badges that follow the account's age: computed on every read, never
/// stored. They follow the "Member since" profile section: hidden there,
/// hidden here too.
pub const ACCOUNT_AGE_BADGES: u32 =
    UserBadges::EarlyAdopter as u32 | UserBadges::OneYear as u32 | UserBadges::ThreeYears as u32;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Year, month (1-12) and day of a day count since 1970-01-01 (proleptic
/// Gregorian calendar, UTC)
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Full calendar years from `created_ms` to `now_ms`, both UTC. An account
/// made on 29 February has its anniversary on 28 February in years without
/// that day, and on 29 February in leap years.
pub fn full_years(created_ms: i64, now_ms: i64) -> u32 {
    if now_ms <= created_ms {
        return 0;
    }
    let (cy, cm, cd) = civil_from_days(created_ms.div_euclid(DAY_MS));
    let (ny, nm, nd) = civil_from_days(now_ms.div_euclid(DAY_MS));
    let (c_time, n_time) = (created_ms.rem_euclid(DAY_MS), now_ms.rem_euclid(DAY_MS));
    let (am, ad) = if cm == 2 && cd == 29 && !is_leap(ny) {
        (2, 28)
    } else {
        (cm, cd)
    };
    let mut years = ny - cy;
    if (nm, nd, n_time) < (am, ad, c_time) {
        years -= 1;
    }
    years.max(0) as u32
}

/// When the account was made, from its id (0 when the id is not a ULID)
pub fn account_created_ms(user: &User) -> i64 {
    Ulid::from_string(&user.id)
        .map(|id| id.timestamp_ms() as i64)
        .unwrap_or_default()
}

/// Whether time on Zrzur counts for this account. Bots, official and
/// system accounts (made with a zero time), deleted, banned and suspended
/// accounts get nothing.
fn counts_for_time(user: &User, created_ms: i64) -> bool {
    created_ms > 0
        && user.bot.is_none()
        && !user.official
        && user.flags.unwrap_or_default()
            & (UserFlags::Deleted as i32 | UserFlags::Banned as i32 | UserFlags::SuspendedUntil as i32)
            == 0
}

/// The account-age badges this account has at `now_ms`: Early Adopter when
/// it was made strictly before `early_adopter_cutoff` (UTC milliseconds),
/// One Year after one full calendar year, Three Years after three. Apps draw
/// the highest year reached (up to ten) from the One Year bit and the id.
pub fn account_age_badges(user: &User, early_adopter_cutoff: Option<u64>, now_ms: i64) -> u32 {
    let created = account_created_ms(user);
    if !counts_for_time(user, created) {
        return 0;
    }
    let mut badges = 0;
    if early_adopter_cutoff.is_some_and(|cutoff| (created as u64) < cutoff) {
        badges |= UserBadges::EarlyAdopter as u32;
    }
    let years = full_years(created, now_ms);
    if years >= 1 {
        badges |= UserBadges::OneYear as u32;
    }
    if years >= 3 {
        badges |= UserBadges::ThreeYears as u32;
    }
    badges
}

/// Why the stored time badges were taken back
pub const TIME_BADGE_RETIRED_REASON: &str = "time badges are computed from the account age";

/// One Year and Three Years were grants until they became computed. This
/// ends every held grant (a platform audit record first, then the badge log
/// entry of the revoke) and clears the stored bits; the grant records stay
/// as history. Official and system accounts never had a right to them.
/// Safe to run again: with nothing held it changes nothing. Returns the
/// number of grants it ended.
pub async fn retire_stored_time_badges(db: &Database) -> Result<usize> {
    let mut ended = 0;
    for badge in ["one_year", "three_years"] {
        for grant in db.fetch_badge_holders(badge, 10 * BATCH).await? {
            let user = match db.fetch_user(&grant.user_id).await {
                Ok(user) => user,
                Err(error) if matches!(error.error_type, ErrorType::NotFound) => continue,
                Err(error) => return Err(error),
            };
            let why = if user.official || account_created_ms(&user) == 0 {
                "official or system account"
            } else {
                "now computed"
            };
            record_platform_audit(
                db,
                AuditRecord {
                    actor_id: BADGE_SYSTEM_ACTOR.to_string(),
                    action: "badge.system.revoke".to_string(),
                    target_kind: Some("user".to_string()),
                    target_id: Some(user.id.clone()),
                    reason: Some(TIME_BADGE_RETIRED_REASON.to_string()),
                    summary: Some(format!("{badge} stored grant ended ({why})")),
                    ..Default::default()
                },
                AuditOutcome::Success,
            )
            .await?;
            match revoke_badge(
                db,
                &user.id,
                badge,
                BADGE_SYSTEM_ACTOR,
                Some(TIME_BADGE_RETIRED_REASON.into()),
            )
            .await
            {
                Ok(_) => ended += 1,
                Err(error) if matches!(error.error_type, ErrorType::NoEffect) => {}
                Err(error) => return Err(error),
            }
            let stored = db.fetch_user(&user.id).await?.badges.unwrap_or_default() as u32;
            let cleared = stored & !ACCOUNT_AGE_BADGES;
            if cleared != stored {
                db.update_user(
                    &user.id,
                    &PartialUser {
                        badges: Some(cleared as i32),
                        ..Default::default()
                    },
                    vec![],
                )
                .await?;
            }
        }
    }
    Ok(ended)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTC milliseconds of a date and hour (the inverse of civil_from_days)
    fn at(year: i64, month: u32, day: u32, hour: i64) -> i64 {
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = (if month > 2 { month - 3 } else { month + 9 }) as i64;
        let doy = (153 * mp + 2) / 5 + day as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        (era * 146_097 + doe - 719_468) * DAY_MS + hour * 60 * 60 * 1000
    }

    #[test]
    fn the_calendar_is_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(at(1970, 1, 1, 0), 0);
        // 2027-03-01T00:00:00Z, the Early Adopter cutoff
        assert_eq!(at(2027, 3, 1, 0), 1_803_859_200_000);
        assert_eq!(civil_from_days(at(2028, 2, 29, 0) / DAY_MS), (2028, 2, 29));
        assert_eq!(civil_from_days(at(2100, 3, 1, 0) / DAY_MS), (2100, 3, 1));
        assert_eq!(civil_from_days(at(2026, 12, 31, 0) / DAY_MS), (2026, 12, 31));
    }

    #[test]
    fn years_are_full_calendar_years_in_utc() {
        let made = at(2026, 11, 24, 10);
        assert_eq!(full_years(made, made), 0);
        assert_eq!(full_years(made, at(2027, 11, 24, 10) - 1), 0);
        assert_eq!(full_years(made, at(2027, 11, 24, 10)), 1);
        // 365 days are not a year when a 29 February lies between
        let before_leap = at(2027, 3, 1, 0);
        assert_eq!(full_years(before_leap, before_leap + 365 * DAY_MS), 0);
        assert_eq!(full_years(before_leap, before_leap + 366 * DAY_MS), 1);
        assert_eq!(full_years(made, at(2036, 11, 24, 10)), 10);
        assert_eq!(full_years(made, at(2026, 1, 1, 0)), 0);
        // the last millisecond of a year, one calendar year later
        let last = at(2027, 1, 1, 0) - 1;
        assert_eq!(full_years(last, at(2028, 1, 1, 0) - 2), 0);
        assert_eq!(full_years(last, at(2028, 1, 1, 0) - 1), 1);
    }

    #[test]
    fn a_29_february_account_has_its_day_on_28_february() {
        let made = at(2028, 2, 29, 10);
        assert_eq!(full_years(made, at(2029, 2, 28, 10) - 1), 0);
        assert_eq!(full_years(made, at(2029, 2, 28, 10)), 1);
        assert_eq!(full_years(made, at(2031, 2, 28, 10)), 3);
        // in a leap year the day is 29 February again
        assert_eq!(full_years(made, at(2032, 2, 28, 23)), 3);
        assert_eq!(full_years(made, at(2032, 2, 29, 10) - 1), 3);
        assert_eq!(full_years(made, at(2032, 2, 29, 10)), 4);
        // 2100 is not a leap year
        let century = at(2096, 2, 29, 0);
        assert_eq!(full_years(century, at(2100, 2, 28, 0)), 4);
    }
}
