//! Temporary voice rooms.
//!
//! A voice channel marked as a room maker (`voice.maker`) hands every member
//! who asks a voice channel of their own (`voice.temporary` holds that
//! member's id). The room is removed when the last person leaves it; a room
//! nobody ever joined is removed after a short wait.

use std::time::{Duration, SystemTime};

use revolt_result::Result;
use ulid::Ulid;

use revolt_permissions::{ChannelPermission, OverrideField};

use crate::{Channel, Database, PartialChannel};

use super::{get_voice_channel_members, get_voice_state, UserVoiceChannel};

/// What the member a room was made for may do in it, on top of their own
/// permissions: rename it, set its user limit, remove it, move people out,
/// and always see and join it (also when it is locked or hidden)
pub const ROOM_OWNER_PERMISSIONS: i64 = ChannelPermission::ManageChannel as i64
    | ChannelPermission::MoveMembers as i64
    | ChannelPermission::ViewChannel as i64
    | ChannelPermission::Connect as i64;

/// What someone let into a locked or hidden room gets
pub const ROOM_GUEST_PERMISSIONS: i64 =
    ChannelPermission::ViewChannel as i64 | ChannelPermission::Connect as i64;

/// Give a temporary room to another member: they get the room's controls,
/// the member before keeps only a guest's access
pub async fn give_temporary_room(db: &Database, channel: &mut Channel, to: &str) -> Result<()> {
    let Channel::TextChannel {
        voice: Some(voice), ..
    } = &*channel
    else {
        return Err(revolt_result::create_error!(InvalidOperation));
    };
    let Some(previous) = voice.temporary.clone() else {
        return Err(revolt_result::create_error!(InvalidOperation));
    };
    if previous == to {
        return Ok(());
    }
    let mut voice = voice.clone();
    voice.temporary = Some(to.to_string());

    channel
        .set_member_permission(
            db,
            &previous,
            OverrideField {
                a: ROOM_GUEST_PERMISSIONS,
                d: 0,
            },
        )
        .await?;
    channel
        .set_member_permission(
            db,
            to,
            OverrideField {
                a: ROOM_OWNER_PERMISSIONS,
                d: 0,
            },
        )
        .await?;
    channel
        .update(
            db,
            PartialChannel {
                voice: Some(voice),
                ..Default::default()
            },
            vec![],
        )
        .await
}

/// The member a room was made for left it while others are still inside:
/// the room goes to whoever has been in it longest. Returns the new member,
/// or none when nothing changed.
pub async fn hand_over_temporary_room(
    db: &Database,
    channel: &Channel,
    leaving: &str,
) -> Result<Option<String>> {
    if temporary_room_owner(channel).as_deref() != Some(leaving) {
        return Ok(None);
    }
    let seat = UserVoiceChannel::from_channel(channel);
    let Some(members) = get_voice_channel_members(&seat).await? else {
        return Ok(None);
    };
    let mut next = None;
    for id in members {
        if id == leaving {
            continue;
        }
        if let Some(state) = get_voice_state(&seat, &id).await? {
            if next
                .as_ref()
                .is_none_or(|(joined_at, _)| state.joined_at < *joined_at)
            {
                next = Some((state.joined_at, id));
            }
        }
    }
    let Some((_, next)) = next else {
        return Ok(None);
    };
    let mut channel = channel.clone();
    give_temporary_room(db, &mut channel, &next).await?;
    Ok(Some(next))
}

/// How long a room nobody joined is kept
pub const UNUSED_ROOM_SECS: u64 = 60;

/// The member a temporary room was made for
pub fn temporary_room_owner(channel: &Channel) -> Option<String> {
    channel.voice().and_then(|voice| voice.temporary.clone())
}

/// Whether a channel was made at least `secs` ago (by its id)
pub fn channel_older_than(channel: &Channel, secs: u64) -> bool {
    Ulid::from_string(channel.id())
        .ok()
        .and_then(|id| SystemTime::now().duration_since(id.datetime()).ok())
        .is_some_and(|age| age >= Duration::from_secs(secs))
}

/// Remove a temporary room nobody is in. Anything else is left alone.
/// Returns whether the room was removed.
pub async fn remove_temporary_room_if_empty(db: &Database, channel: &Channel) -> Result<bool> {
    if temporary_room_owner(channel).is_none() {
        return Ok(false);
    }
    if get_voice_channel_members(&UserVoiceChannel::from_channel(channel))
        .await?
        .is_some()
    {
        return Ok(false);
    }

    channel.delete(db).await?;
    if let Some(server) = channel.server() {
        crate::repoint_vanity_after_channel_delete(db, server, channel.id()).await?;
    }
    Ok(true)
}
