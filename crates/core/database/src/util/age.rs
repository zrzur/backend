//! Age safety
//!
//! Zrzur keeps only the year and month of birth, as stated by the user.
//! Ages are counted as if the person was born on the last day of that month,
//! so a rounding error always makes someone younger, never older.
//!
//! A declared age is never proof of age: "adult" here always means
//! "declared adult". Every age-based decision is made on the server.
use iso8601_timestamp::Timestamp;
use revolt_config::{config, MatureContentAccess};
use revolt_models::v0::{AccountAgeInfo, AgeBand, AgeSource, MatureAccess};
use revolt_result::{create_error, Result};

use crate::{Account, AccountAge, AgeChange, Channel, Database, Server, User};

/// Links and interests of a minor's profile only go to friends
pub fn may_see_personal_sections(owner_band: AgeBand, is_self: bool, is_friend: bool) -> bool {
    is_self || is_friend || owner_band == AgeBand::AdultDeclared
}

/// Age group of a profile's owner. Bots have none; a missing account counts
/// as unknown, which is the safe side.
pub async fn owner_band(db: &Database, user: &User) -> AgeBand {
    if user.bot.is_some() {
        return AgeBand::AdultDeclared;
    }
    db.fetch_account(&user.id)
        .await
        .map(|account| account.age_band())
        .unwrap_or(AgeBand::Unknown)
}

/// Minimum age for an account (GDPR Art. 8 default, the highest national age in the EU)
pub const MINIMUM_AGE: i32 = 16;

/// Age from which content marked as mature may be opened
pub const ADULT_AGE: i32 = 18;

/// Oldest accepted age, anything above is treated as a typo
pub const OLDEST_AGE: i32 = 120;

/// Current year and month (UTC)
fn current_month() -> (i32, u8) {
    let now = Timestamp::now_utc();
    (now.year(), u8::from(now.month()))
}

/// Parse a `YYYY-MM` year and month of birth
pub fn parse_birth_month(value: &str) -> Result<(i32, u8)> {
    parse_birth_month_at(value, current_month())
}

fn parse_birth_month_at(value: &str, now: (i32, u8)) -> Result<(i32, u8)> {
    let invalid = || {
        create_error!(IncorrectData {
            with: "birth_month".to_string()
        })
    };

    let (year, month) = value.split_once('-').ok_or_else(invalid)?;
    if year.len() != 4 || month.len() != 2 {
        return Err(invalid());
    }

    let year: i32 = year.parse().map_err(|_| invalid())?;
    let month: u8 = month.parse().map_err(|_| invalid())?;
    if !(1..=12).contains(&month) || (year, month) > now || now.0 - year > OLDEST_AGE {
        return Err(invalid());
    }

    Ok((year, month))
}

/// Age in full years for someone born in the given month
pub fn age_at(birth: (i32, u8), now: (i32, u8)) -> i32 {
    let months = (now.0 - birth.0) * 12 + now.1 as i32 - birth.1 as i32;

    // A birthday only counts once its month is over
    if months <= 0 {
        0
    } else {
        (months - 1) / 12
    }
}

/// Age group for an age in years
pub fn band_for_age(age: i32) -> AgeBand {
    if age < MINIMUM_AGE {
        AgeBand::UnderMinimum
    } else if age < ADULT_AGE {
        AgeBand::Teen
    } else {
        AgeBand::AdultDeclared
    }
}

/// Age group for a year and month of birth, today
pub fn band_for_birth(birth: (i32, u8)) -> AgeBand {
    band_for_age(age_at(birth, current_month()))
}

/// Whether a person was under 18 when this account (a ULID) was created
pub fn under_adult_when_created(account_id: &str, birth_month: &str) -> bool {
    let (Ok(id), Ok(birth)) = (ulid::Ulid::from_string(account_id), parse_birth_month(birth_month))
    else {
        return false;
    };
    let created = Timestamp::UNIX_EPOCH
        .checked_add(iso8601_timestamp::Duration::milliseconds(id.timestamp_ms() as i64));
    created.is_some_and(|at| age_at(birth, (at.year(), u8::from(at.month()))) < ADULT_AGE)
}

/// Order of the age groups, youngest first (unknown counts as youngest)
fn rank(band: AgeBand) -> u8 {
    match band {
        AgeBand::Unknown | AgeBand::UnderMinimum => 0,
        AgeBand::Teen => 1,
        AgeBand::AdultDeclared => 2,
    }
}

fn format_birth_month(birth: (i32, u8)) -> String {
    format!("{:04}-{:02}", birth.0, birth.1)
}

/// Whether an age group may open mature content, and if not, why
pub fn mature_access(band: AgeBand, setting: MatureContentAccess) -> MatureAccess {
    match band {
        AgeBand::Unknown => MatureAccess::AgeUnknown,
        AgeBand::UnderMinimum | AgeBand::Teen => MatureAccess::Minor,
        AgeBand::AdultDeclared => match setting {
            MatureContentAccess::SelfDeclaredAdults => MatureAccess::Allowed,
            MatureContentAccess::Blocked => MatureAccess::VerificationRequired,
        },
    }
}

/// Accounts already known to have declared an age; a declared age is never
/// removed, so a hit never goes stale
static DECLARED: once_cell::sync::Lazy<std::sync::Mutex<lru::LruCache<String, ()>>> =
    once_cell::sync::Lazy::new(|| {
        std::sync::Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(100_000).unwrap(),
        ))
    });

/// Whether this person has declared a month and year of birth. Every API
/// request and every gateway connection of a person waits for it (bots have
/// no account age and are never asked; `require_declared_age` turns it off).
pub async fn has_declared_age(db: &Database, user: &User) -> bool {
    if user.bot.is_some()
        || !revolt_config::config()
            .await
            .features
            .age_safety
            .require_declared_age
    {
        return true;
    }
    if DECLARED.lock().unwrap().get(&user.id).is_some() {
        return true;
    }
    let declared = db
        .fetch_account(&user.id)
        .await
        .is_ok_and(|account| account.age.is_some());
    if declared {
        DECLARED.lock().unwrap().put(user.id.clone(), ());
    }
    declared
}

impl AccountAge {
    /// Age stated by the user, now
    pub fn declare(birth: (i32, u8)) -> AccountAge {
        AccountAge {
            birth_month: format_birth_month(birth),
            declared_at: Timestamp::now_utc(),
            source: AgeSource::SelfDeclared,
            raised: false,
            changes: vec![],
        }
    }

    /// Current age group
    pub fn band(&self) -> AgeBand {
        parse_birth_month(&self.birth_month)
            .map(band_for_birth)
            // A stored value that no longer parses is treated as the most restricted
            .unwrap_or(AgeBand::UnderMinimum)
    }
}

/// Decide whether a change of the declared age is allowed
///
/// Returns whether the change raises the age (uses up the one correction).
pub fn check_age_change(current: &AccountAge, new_birth: (i32, u8)) -> Result<bool> {
    let new_month = format_birth_month(new_birth);
    if new_month == current.birth_month {
        return Err(create_error!(NoEffect));
    }

    // An earlier month of birth makes the person older
    let raises = new_month < current.birth_month;
    if raises && (current.raised || band_for_birth(new_birth) != current.band()) {
        return Err(create_error!(AgeChangeNotAllowed));
    }

    Ok(raises)
}

impl Account {
    /// Current age group
    pub fn age_band(&self) -> AgeBand {
        self.age
            .as_ref()
            .map(AccountAge::band)
            .unwrap_or(AgeBand::Unknown)
    }

    /// Whether mature content may be opened, and if not, why
    pub async fn mature_access(&self) -> MatureAccess {
        mature_access(
            self.age_band(),
            config().await.features.age_safety.mature_content,
        )
    }

    /// Age information for the account owner
    pub async fn age_info(&self) -> AccountAgeInfo {
        AccountAgeInfo {
            birth_month: self.age.as_ref().map(|age| age.birth_month.clone()),
            band: self.age_band(),
            mature_access: self.mature_access().await,
            can_raise: self.age.as_ref().is_some_and(|age| !age.raised),
        }
    }

    /// Account information for its owner, with the age
    pub async fn into_info(self) -> revolt_models::v0::AccountInfo {
        let age = self.age_info().await;
        let mut info: revolt_models::v0::AccountInfo = self.into();
        info.age = Some(age);
        info
    }

    /// Declare or correct the age on this account
    ///
    /// - The first statement needs no password. It is asked at the first entry
    ///   instead of at sign-up, so it follows the sign-up rule: below the
    ///   minimum age it is refused and nothing is stored.
    /// - Making yourself younger is always allowed and applies at once.
    /// - Making yourself older is allowed once, and only within the same age
    ///   group, so retries can't climb into another group. Anything else goes
    ///   through support.
    /// - Lowering an age already on record below the minimum restricts the
    ///   account like a minor's and is left for review; it never closes or
    ///   deletes the account by itself.
    pub async fn change_age(
        &mut self,
        db: &Database,
        birth_month: &str,
        current_password: Option<&str>,
    ) -> Result<AgeBand> {
        let birth = parse_birth_month(birth_month)?;
        let old_band = self.age_band();
        let new_band = band_for_birth(birth);

        let next = match &self.age {
            None => {
                if new_band == AgeBand::UnderMinimum {
                    // Policy: below the minimum age nothing is stored. That
                    // is only done for a genuinely new, empty account. An
                    // older account (created before the age was asked) may
                    // own servers and messages other people rely on, and a
                    // typo must not destroy them: it follows the review path
                    // below, like any account that lowers its age under the
                    // minimum.
                    if self.is_new_and_empty(db).await {
                        self.erase_completely(db).await?;
                        return Err(create_error!(AgeRequirementNotMet));
                    }
                }
                let mut next = AccountAge::declare(birth);
                next.changes.push(AgeChange {
                    at: next.declared_at,
                    from: AgeBand::Unknown,
                    to: new_band,
                });
                next
            }
            Some(age) => {
                self.verify_password(current_password.unwrap_or_default())?;
                let raises = check_age_change(age, birth)?;

                let mut next = AccountAge::declare(birth);
                next.raised = age.raised || raises;
                next.changes = age.changes.clone();
                if old_band != new_band {
                    next.changes.push(AgeChange {
                        at: next.declared_at,
                        from: old_band,
                        to: new_band,
                    });
                }
                next
            }
        };

        self.age = Some(next);
        self.save(db).await?;

        // Open connections drop mature channels straight away when the
        // group goes down (and pick them up when it is first declared)
        if rank(new_band) != rank(old_band) || old_band == AgeBand::Unknown {
            crate::events::client::EventV1::AccountAgeUpdate { band: new_band }
                .p(self.id.clone())
                .await;
        }

        Ok(new_band)
    }
}

impl Channel {
    /// Whether this channel itself is marked as mature
    pub fn is_nsfw(&self) -> bool {
        match self {
            Channel::Group { nsfw, .. } | Channel::TextChannel { nsfw, .. } => *nsfw,
            _ => false,
        }
    }
}

/// Whether a channel is mature, by itself or through its Server
pub async fn is_mature_channel(db: &Database, channel: &Channel) -> Result<bool> {
    if channel.is_nsfw() {
        return Ok(true);
    }

    Ok(match channel.server() {
        Some(server) => db.fetch_server(server).await?.nsfw,
        None => false,
    })
}

/// Whether a user may open mature content (bots are not checked)
pub async fn may_open_mature(db: &Database, user: &User) -> Result<bool> {
    if user.bot.is_some() {
        return Ok(true);
    }

    Ok(db.fetch_account(&user.id).await?.mature_access().await == MatureAccess::Allowed)
}

/// Make sure a user may see the content of a channel
///
/// Channels marked as mature, and every channel of a Server marked as
/// mature, are only open to declared adults (or nobody, depending on
/// `features.age_safety.mature_content`).
pub async fn assert_can_view_channel(db: &Database, user: &User, channel: &Channel) -> Result<()> {
    if user.bot.is_some() || !is_mature_channel(db, channel).await? {
        return Ok(());
    }

    assert_may_open_mature(db, user).await
}

/// Make sure a user may join or preview a Server
pub async fn assert_can_join_server(db: &Database, user: &User, server: &Server) -> Result<()> {
    if server.nsfw {
        assert_may_open_mature(db, user).await
    } else {
        Ok(())
    }
}

/// Make sure a user may open mature content
pub async fn assert_may_open_mature(db: &Database, user: &User) -> Result<()> {
    if may_open_mature(db, user).await? {
        Ok(())
    } else {
        Err(create_error!(AgeRestricted))
    }
}

/// Keep only the users who may open mature content (for notifications)
pub async fn filter_mature_recipients(db: &Database, users: &[String]) -> Vec<String> {
    let setting = config().await.features.age_safety.mature_content;
    let mut allowed = vec![];
    for id in users {
        let Ok(user) = db.fetch_user(id).await else {
            continue;
        };
        if user.bot.is_some() {
            allowed.push(id.clone());
            continue;
        }
        if let Ok(account) = db.fetch_account(id).await {
            if mature_access(account.age_band(), setting) == MatureAccess::Allowed {
                allowed.push(id.clone());
            }
        }
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_counts_full_months() {
        // Born October 2010: 15 during October 2026, 16 from November 2026
        assert_eq!(age_at((2010, 10), (2026, 10)), 15);
        assert_eq!(age_at((2010, 10), (2026, 11)), 16);
        assert_eq!(age_at((2010, 1), (2026, 10)), 16);
        assert_eq!(age_at((2026, 10), (2026, 10)), 0);
    }

    #[test]
    fn bands_at_the_boundaries() {
        assert_eq!(band_for_age(15), AgeBand::UnderMinimum);
        assert_eq!(band_for_age(16), AgeBand::Teen);
        assert_eq!(band_for_age(17), AgeBand::Teen);
        assert_eq!(band_for_age(18), AgeBand::AdultDeclared);
    }

    #[test]
    fn parse() {
        let now = (2026, 10);
        assert_eq!(parse_birth_month_at("2000-03", now).unwrap(), (2000, 3));
        assert_eq!(parse_birth_month_at("2026-10", now).unwrap(), (2026, 10));
        for bad in [
            "2000-3", "2000-13", "2000-00", "20-03", "2026-11", "2027-01", "1800-01", "abcd-ef",
            "2000-03-01", "",
        ] {
            assert!(parse_birth_month_at(bad, now).is_err(), "{bad}");
        }
    }

    #[test]
    fn mature_access_needs_a_declared_adult() {
        use MatureContentAccess::*;
        assert_eq!(mature_access(AgeBand::Unknown, SelfDeclaredAdults), MatureAccess::AgeUnknown);
        assert_eq!(mature_access(AgeBand::UnderMinimum, SelfDeclaredAdults), MatureAccess::Minor);
        assert_eq!(mature_access(AgeBand::Teen, SelfDeclaredAdults), MatureAccess::Minor);
        assert_eq!(mature_access(AgeBand::AdultDeclared, SelfDeclaredAdults), MatureAccess::Allowed);
        assert_eq!(
            mature_access(AgeBand::AdultDeclared, Blocked),
            MatureAccess::VerificationRequired
        );
    }

    fn declared(years_ago: i32) -> AccountAge {
        let (year, month) = current_month();
        AccountAge::declare((year - years_ago, month))
    }

    #[test]
    fn lowering_is_allowed() {
        let (year, month) = current_month();
        let age = declared(30);
        assert!(!check_age_change(&age, (year - 17, month)).unwrap());
        assert!(!check_age_change(&age, (year - 10, month)).unwrap());
    }

    #[test]
    fn raising_once_within_band() {
        let (year, month) = current_month();
        let mut age = declared(30);
        assert!(check_age_change(&age, (year - 31, month)).unwrap());

        age.raised = true;
        assert!(check_age_change(&age, (year - 32, month)).is_err());
    }

    #[test]
    fn raising_into_another_band_is_refused() {
        let (year, month) = current_month();
        let teen = declared(17);
        assert_eq!(teen.band(), AgeBand::Teen);
        assert!(check_age_change(&teen, (year - 25, month)).is_err());

        let child = declared(14);
        assert_eq!(child.band(), AgeBand::UnderMinimum);
        assert!(check_age_change(&child, (year - 17, month)).is_err());
    }

    #[test]
    fn same_value_has_no_effect() {
        let age = declared(30);
        let birth = parse_birth_month(&age.birth_month).unwrap();
        assert!(check_age_change(&age, birth).is_err());
    }
}
