//! Custom invite links (zrzur.cc/<name>): which channel they open, and what
//! happens when a server falls below the Wings level that unlocks them.
//! The link keeps working for 7 days, then
//! pauses; the name stays reserved for the server for 90 days after the
//! drop and is released after that. Regaining the level restores it.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_permissions::ChannelPermission;
use revolt_result::{create_error, Result};

use crate::{server_supercharge_level, Channel, Database, FieldsServer, Invite, PartialServer, Server};

/// Days a link keeps working after the server falls below the level
pub const VANITY_GRACE_DAYS: i64 = 7;
/// Days the name stays reserved for the server after the drop
pub const VANITY_RESERVE_DAYS: i64 = 90;

/// Where a custom link stands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VanityState {
    /// The server holds the level
    Active,
    /// Below the level, link still works until `ends`
    Grace { ends: Timestamp },
    /// Below the level, link paused, name kept until `released_at`
    Paused { released_at: Timestamp },
    /// Reservation over: the name goes back to everyone
    Release,
}

/// State of a link from when its server fell below the level (None = holds it)
pub fn vanity_state(below_level_since: Option<Timestamp>, now: Timestamp) -> VanityState {
    let Some(since) = below_level_since else {
        return VanityState::Active;
    };
    let ends = since + Duration::days(VANITY_GRACE_DAYS);
    if now < ends {
        return VanityState::Grace { ends };
    }
    let released_at = since + Duration::days(VANITY_RESERVE_DAYS);
    if now < released_at {
        VanityState::Paused { released_at }
    } else {
        VanityState::Release
    }
}

/// Whether the server holds the level that unlocks a custom link
pub async fn vanity_eligible(server: &Server) -> bool {
    let billing = &config().await.features.billing;
    // with billing off nothing is paid for, so every server may have one
    if !billing.available() {
        return true;
    }
    match billing.vanity_level() {
        Some(required) => server_supercharge_level(server).await >= required,
        None => false,
    }
}

/// Refuse a custom link that is paused (fetch and join); other invites pass
pub async fn check_vanity_open(db: &Database, invite: &Invite) -> Result<()> {
    let Invite::Server { vanity: true, server, channel, .. } = invite else {
        return Ok(());
    };
    let server = db.fetch_server(server).await?;
    // no readable channel left to land in: paused until the owner picks one
    if !server.channels.iter().any(|c| c == channel) {
        return Err(create_error!(VanityPaused));
    }
    if vanity_eligible(&server).await {
        return Ok(());
    }
    let since = server.vanity_invite.as_ref().and_then(|v| v.below_level_since);
    match vanity_state(since, Timestamp::now_utc()) {
        VanityState::Active | VanityState::Grace { .. } => Ok(()),
        _ => Err(create_error!(VanityPaused)),
    }
}

/// Keep every custom link in step with its server's level; release names
/// whose reservation ran out
pub async fn sync_vanity_invites(db: &Database, now: Timestamp) -> Result<()> {
    for mut server in db.fetch_servers_with_vanity().await? {
        let Some(mut vanity) = server.vanity_invite.clone() else {
            continue;
        };
        let next = match (vanity_eligible(&server).await, vanity.below_level_since) {
            (true, _) => None,
            (false, None) => Some(now),
            (false, since) => since,
        };
        if vanity_state(next, now) == VanityState::Release {
            db.delete_invite(&vanity.code).await.ok();
            server
                .update(db, PartialServer::default(), vec![FieldsServer::VanityInvite])
                .await?;
            continue;
        }
        if next != vanity.below_level_since {
            vanity.below_level_since = next;
            server
                .update(
                    db,
                    PartialServer {
                        vanity_invite: Some(vanity),
                        ..Default::default()
                    },
                    vec![],
                )
                .await?;
        }
    }
    Ok(())
}

/// A channel was deleted (its invites with it): move the server's custom
/// link to a channel everyone can read, or keep the name reserved on the old
/// channel id, which answers "paused" until the owner picks a channel
pub async fn repoint_vanity_after_channel_delete(db: &Database, server_id: &str, deleted: &str) -> Result<()> {
    let mut server = db.fetch_server(server_id).await?;
    let Some(mut vanity) = server.vanity_invite.clone() else {
        return Ok(());
    };
    if vanity.channel != deleted {
        return Ok(());
    }
    let target = landing_channel(db, &server, None)
        .await
        .unwrap_or_else(|_| deleted.to_string());
    db.insert_invite(&Invite::Server {
        code: vanity.code.clone(),
        server: server.id.clone(),
        creator: server.owner.clone(),
        channel: target.clone(),
        max_uses: None,
        uses: 0,
        expires: None,
        vanity: true,
    })
    .await
    .ok();
    vanity.channel = target;
    server
        .update(
            db,
            PartialServer {
                vanity_invite: Some(vanity),
                ..Default::default()
            },
            vec![],
        )
        .await
}

/// The chosen channel, or the first one, that new members (@everyone) can read
pub async fn landing_channel(db: &Database, server: &Server, wanted: Option<&str>) -> Result<String> {
    let candidates = match wanted {
        Some(id) if server.channels.iter().any(|c| c == id) => vec![id.to_string()],
        Some(_) => return Err(create_error!(NotFound)),
        None => server.channels.clone(),
    };
    for id in candidates {
        if let Ok(channel) = db.fetch_channel(&id).await {
            if everyone_can_read(server, &channel) {
                return Ok(id);
            }
        }
    }
    Err(create_error!(InvalidOperation))
}

/// A text channel the server's default role may view
pub fn everyone_can_read(server: &Server, channel: &Channel) -> bool {
    let Channel::TextChannel { voice, default_permissions, .. } = channel else {
        return false;
    };
    if voice.is_some() {
        return false;
    }
    let view = ChannelPermission::ViewChannel as i64;
    let mut allowed = server.default_permissions & view != 0;
    if let Some(over) = default_permissions {
        if over.d & view != 0 {
            allowed = false;
        }
        if over.a & view != 0 {
            allowed = true;
        }
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanity_state_timeline() {
        let now = Timestamp::now_utc();
        let ago = |days: i64| Some(now - Duration::days(days));
        assert_eq!(vanity_state(None, now), VanityState::Active);
        assert!(matches!(vanity_state(ago(3), now), VanityState::Grace { .. }));
        assert_eq!(
            vanity_state(ago(3), now),
            VanityState::Grace { ends: now - Duration::days(3) + Duration::days(VANITY_GRACE_DAYS) }
        );
        assert!(matches!(vanity_state(ago(8), now), VanityState::Paused { .. }));
        assert!(matches!(vanity_state(ago(89), now), VanityState::Paused { .. }));
        assert_eq!(vanity_state(ago(91), now), VanityState::Release);
    }
}
