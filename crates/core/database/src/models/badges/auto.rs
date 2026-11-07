//! Badges the system gives from its own records: time on Zrzur, the size of
//! a server someone owns, and a verified server or bot. Each rule is safe to
//! run again: a badge already held is left alone.

use std::time::Duration;

use revolt_models::v0::{BotFlags, UserFlags};
use revolt_result::{ErrorType, Result};
use ulid::Ulid;

use crate::{grant_badge, held_badges, revoke_badge, BadgeSource, Database, Server, User, BADGE_SYSTEM_ACTOR};

const YEAR_MS: i64 = 365 * 24 * 60 * 60 * 1000;

/// Members a server needs for each badge. Earned once: a server that shrinks
/// later keeps it.
pub const COMMUNITY_BUILDER_MEMBERS: usize = 100;
pub const BIG_COMMUNITY_MEMBERS: usize = 1000;

/// Users read in one go, and the rest between two reads
const BATCH: usize = 500;
const BATCH_PAUSE: Duration = Duration::from_secs(1);
const WALK_PAUSE: Duration = Duration::from_secs(60 * 60);

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

/// Badges a server of this size earns for its owner
pub fn size_badges(members: usize) -> &'static [&'static str] {
    if members >= BIG_COMMUNITY_MEMBERS {
        &["community_builder", "big_community"]
    } else if members >= COMMUNITY_BUILDER_MEMBERS {
        &["community_builder"]
    } else {
        &[]
    }
}

/// Give the owner what a server of `members` earns. Called on every join, so
/// small servers stop at once.
pub async fn sync_server_size_badges(db: &Database, server: &Server, members: usize) -> Result<()> {
    let earned = size_badges(members);
    if earned.is_empty() {
        return Ok(());
    }
    let held = held_badges(db, &server.owner).await?;
    for badge in earned.iter().filter(|badge| !held.contains(**badge)) {
        grant_once(db, &server.owner, badge, &format!("server:{}", server.id)).await?;
    }
    Ok(())
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
        .any(|server| matches!(server.flags, Some(SERVER_OFFICIAL | SERVER_VERIFIED)));
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

/// Whether time on Zrzur counts for this account. Deleted, banned and
/// suspended accounts get nothing, and neither do bots.
fn counts_for_time(user: &User) -> bool {
    user.bot.is_none()
        && user.flags.unwrap_or_default()
            & (UserFlags::Deleted as i32 | UserFlags::Banned as i32 | UserFlags::SuspendedUntil as i32)
            == 0
}

/// Id of the first account made at this moment
fn id_at(ms: i64) -> String {
    Ulid::from_parts(ms.max(0) as u64, 0).to_string()
}

/// One step of a walk: the next accounts old enough for `badge` that don't
/// have it. Returns where the next step starts, `None` when it has caught up.
pub async fn grant_time_badge(
    db: &Database,
    badge: &str,
    bit: u32,
    years: i64,
    now_ms: i64,
    after: Option<&str>,
) -> Result<Option<String>> {
    let before = id_at(now_ms - years * YEAR_MS);
    let users = db.fetch_users_without_badge(after, &before, bit, BATCH).await?;
    let next = (users.len() == BATCH).then(|| users.last().map(|user| user.id.clone())).flatten();
    for user in users.iter().filter(|user| counts_for_time(user)) {
        grant_once(db, &user.id, badge, "time").await?;
    }
    Ok(next)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Walk the accounts, paced, then look again every hour. The first walk
/// after a start also reaches everyone who earned a badge before it existed.
pub async fn time_badge_worker(db: Database) {
    use revolt_models::v0::UserBadges;
    loop {
        for (badge, bit, years) in [
            ("one_year", UserBadges::OneYear as u32, 1),
            ("three_years", UserBadges::ThreeYears as u32, 3),
        ] {
            let mut after = None;
            loop {
                match grant_time_badge(&db, badge, bit, years, now_ms(), after.as_deref()).await {
                    Ok(Some(next)) => after = Some(next),
                    Ok(None) => break,
                    Err(error) => {
                        log::error!("badges: {error:?}");
                        break;
                    }
                }
                tokio::time::sleep(BATCH_PAUSE).await;
            }
        }
        tokio::time::sleep(WALK_PAUSE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_size_thresholds() {
        assert!(size_badges(99).is_empty());
        assert_eq!(size_badges(100), ["community_builder"]);
        assert_eq!(size_badges(999), ["community_builder"]);
        assert_eq!(size_badges(1000), ["community_builder", "big_community"]);
    }

    #[test]
    fn a_later_moment_makes_a_later_id() {
        assert!(id_at(1_000) < id_at(2_000));
        assert_eq!(id_at(0), "00000000000000000000000000");
    }
}
