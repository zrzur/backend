//! Server safety actions: paused DMs between members, and the raid alert
//! that offers to pause invites when many people join at once.

use redis_kiss::{get_connection, AsyncCommands};
use revolt_config::config;
use revolt_result::Result;

use crate::{Database, RelationshipStatus, Server, SystemMessage, User};

/// Whether a server both people are in paused DMs between members who
/// aren't friends (bots and friends are never stopped)
pub async fn dms_paused_between(db: &Database, a: &User, b: &User) -> Result<bool> {
    if a.bot.is_some()
        || b.bot.is_some()
        || matches!(a.relationship_with(&b.id), RelationshipStatus::Friend)
    {
        return Ok(false);
    }

    let theirs: Vec<String> = db
        .fetch_all_memberships(&b.id)
        .await?
        .into_iter()
        .map(|member| member.id.server)
        .collect();
    let shared: Vec<String> = db
        .fetch_all_memberships(&a.id)
        .await?
        .into_iter()
        .map(|member| member.id.server)
        .filter(|server| theirs.contains(server))
        .collect();
    if shared.is_empty() {
        return Ok(false);
    }

    Ok(db
        .fetch_servers(&shared)
        .await?
        .iter()
        .any(Server::dms_paused))
}

/// Count a join. When more than `raid_alert_joins` people join within
/// `raid_alert_minutes`, post one alert in the server's join message
/// channel (members with Manage Server see "Pause invites" and "Ignore").
pub async fn record_join(db: &Database, server: &Server) {
    let limits = config().await.features.limits.global;
    let (joins, minutes) = (limits.raid_alert_joins, limits.raid_alert_minutes);
    if joins == 0 || minutes == 0 {
        return;
    }
    let Some(channel) = server
        .system_messages
        .as_ref()
        .and_then(|messages| messages.user_joined.clone())
    else {
        return;
    };

    // one counter per server; its window starts with the first join, so a
    // burst is never split in two by a clock boundary
    let window = minutes as i64 * 60;
    let key = format!("raid:{}", server.id);
    let Ok(mut redis) = get_connection().await else {
        return;
    };
    let Ok(count): std::result::Result<usize, _> = redis.incr(&key, 1).await else {
        return;
    };
    if count == 1 {
        let ttl = window.try_into().unwrap_or(60);
        let _: std::result::Result<(), _> = redis.expire(&key, ttl).await;
    }

    // exactly once per window, when the count goes past the limit
    if count == joins + 1 {
        SystemMessage::RaidAlert {
            joins: count as u32,
            minutes: minutes as u32,
        }
        .into_message(channel)
        .send_without_notifications(db, None, None, false, false, false)
        .await
        .ok();
    }
}
