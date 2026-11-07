//! Server verification levels and rules screening: what a member needs before
//! they can send messages, react or speak. Bots and the owner are exempt;
//! members with any role skip the verification level, not the rules.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_models::v0::ServerVerificationLevel;
use revolt_result::{create_error, Result};

use crate::{Channel, Database, EmailVerification, Server, User};

/// Medium: the account is at least this old
const ACCOUNT_AGE: Duration = Duration::minutes(5);
/// High: a member of the server for at least this long
const MEMBER_AGE: Duration = Duration::minutes(10);

fn rank(level: ServerVerificationLevel) -> u8 {
    match level {
        ServerVerificationLevel::None => 0,
        ServerVerificationLevel::Low => 1,
        ServerVerificationLevel::Medium => 2,
        ServerVerificationLevel::High => 3,
    }
}

/// Seconds left until `at`, at least 1
fn seconds_until(at: Timestamp, now: Timestamp) -> u64 {
    at.duration_since(now).whole_seconds().max(1) as u64
}

/// Refuse to talk in a server channel when the server's level isn't met
pub async fn assert_can_talk(db: &Database, user: &User, channel: &Channel) -> Result<()> {
    match channel.server() {
        Some(server) => assert_can_talk_in(db, user, &db.fetch_server(server).await?).await,
        None => Ok(()),
    }
}

/// Refuse to talk in `server` when its verification level isn't met
pub async fn assert_can_talk_in(db: &Database, user: &User, server: &Server) -> Result<()> {
    let level = server.verification_level.unwrap_or_default();
    let screening = server
        .rules_screening_since
        .filter(|_| server.rules_channel.is_some());
    if (rank(level) == 0 && screening.is_none()) || user.bot.is_some() || user.id == server.owner {
        return Ok(());
    }

    // not a member: the permission checks already decided
    let Ok(member) = db.fetch_member(&server.id, &user.id).await else {
        return Ok(());
    };

    // Rules screening: who joined since it was switched on accepts first
    // (roles don't exempt: Lobby questions hand out roles to newcomers)
    if let Some(since) = screening {
        if member.joined_at >= since && member.accepted_rules_at.is_none() {
            return Err(create_error!(RulesNotAccepted));
        }
    }

    if rank(level) == 0 || !member.roles.is_empty() {
        return Ok(());
    }

    // Low: a verified email address (an email being changed was verified)
    let account = db.fetch_account(&user.id).await?;
    if matches!(account.verification, EmailVerification::Pending { .. }) {
        return Err(create_error!(VerificationRequired {
            requirement: "email".to_string(),
            wait_seconds: None,
        }));
    }

    let now = Timestamp::now_utc();

    // Medium: an account older than 5 minutes
    if rank(level) >= 2 {
        let created = ulid::Ulid::from_string(&user.id)
            .map(|id| Timestamp::UNIX_EPOCH + Duration::milliseconds(id.timestamp_ms() as i64))
            .unwrap_or(now);
        let ready = created + ACCOUNT_AGE;
        if ready > now {
            return Err(create_error!(VerificationRequired {
                requirement: "account_age".to_string(),
                wait_seconds: Some(seconds_until(ready, now)),
            }));
        }
    }

    // High: a member for 10 minutes
    if rank(level) >= 3 {
        let ready = member.joined_at + MEMBER_AGE;
        if ready > now {
            return Err(create_error!(VerificationRequired {
                requirement: "member_age".to_string(),
                wait_seconds: Some(seconds_until(ready, now)),
            }));
        }
    }

    Ok(())
}
