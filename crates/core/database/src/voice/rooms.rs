//! Temporary voice rooms.
//!
//! A voice channel marked as a room maker (`voice.maker`) hands every member
//! who asks a voice channel of their own (`voice.temporary` holds that
//! member's id). The room is removed when the last person leaves it; a room
//! nobody ever joined is removed after a short wait.

use std::time::{Duration, SystemTime};

use revolt_result::Result;
use ulid::Ulid;

use crate::{Channel, Database};

use super::{get_voice_channel_members, UserVoiceChannel};

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
