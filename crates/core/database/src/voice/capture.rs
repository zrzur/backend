//! Clips and recording in a voice room: who turned clips on, the recording
//! that runs, and who agreed to be heard in them. Kept in Redis next to the
//! voice state, and only while the room is used.
//!
//! Nothing here records anything. The audio is put together by the app of
//! the person who clips or records, from the voices in `included` only; the
//! server keeps this state, tells the room about every change, and refuses
//! a clip or an episode that names someone who is not in `included`.

use redis_kiss::{redis::Pipeline, AsyncCommands};
use revolt_models::v0::{VoiceCaptureState, VoiceRecordingLive};
use revolt_result::{Result, ToRevoltError};

use super::{get_connection, get_voice_channel_members, UserVoiceChannel};
use crate::events::client::EventV1;

pub(crate) fn clips_key(channel_id: &str) -> String {
    format!("capture_clips:{channel_id}")
}

pub(crate) fn recording_key(channel_id: &str) -> String {
    format!("capture_rec:{channel_id}")
}

/// People who chose to be included
pub(crate) fn yes_key(channel_id: &str) -> String {
    format!("capture_yes:{channel_id}")
}

/// People who chose to be left out
pub(crate) fn no_key(channel_id: &str) -> String {
    format!("capture_no:{channel_id}")
}

/// Everyone who was included at some point of the running recording (people
/// who agreed and left later are still in the episode)
pub(crate) fn ever_key(channel_id: &str) -> String {
    format!("capture_ever:{channel_id}")
}

/// Who is included, from the people in the room and their choices. With
/// `opt_in` only those who said yes; otherwise everyone who did not say no.
/// Someone who is not in the room is never included.
pub fn included_of(members: &[String], yes: &[String], no: &[String], opt_in: bool) -> Vec<String> {
    let mut included: Vec<String> = members
        .iter()
        .filter(|member| {
            if opt_in {
                yes.contains(member)
            } else {
                !no.contains(member)
            }
        })
        .cloned()
        .collect();
    included.sort();
    included.dedup();
    included
}

/// The room's capture state right now
pub async fn get_capture_state(channel: &UserVoiceChannel, opt_in: bool) -> Result<VoiceCaptureState> {
    let (clips, recording, yes, no): (Option<String>, Option<String>, Vec<String>, Vec<String>) =
        Pipeline::new()
            .get(clips_key(&channel.id))
            .get(recording_key(&channel.id))
            .smembers(yes_key(&channel.id))
            .smembers(no_key(&channel.id))
            .query_async(&mut get_connection().await?.into_inner())
            .await
            .to_internal_error()?;

    let recording: Option<VoiceRecordingLive> =
        recording.and_then(|text| serde_json::from_str(&text).ok());

    // nothing is on: nobody is "included" in anything
    if clips.is_none() && recording.is_none() {
        return Ok(VoiceCaptureState::default());
    }

    let members = get_voice_channel_members(channel).await?.unwrap_or_default();
    Ok(VoiceCaptureState {
        clips,
        recording,
        included: included_of(&members, &yes, &no, opt_in),
    })
}

/// Turn clips on (by this person) or off
pub async fn set_capture_clips(channel_id: &str, by: Option<&str>) -> Result<()> {
    let mut conn = get_connection().await?;
    match by {
        Some(by) => conn.set(clips_key(channel_id), by).await,
        None => conn.del(clips_key(channel_id)).await,
    }
    .to_internal_error()
}

/// Remember a person's choice for this room
pub async fn set_capture_consent(channel_id: &str, user_id: &str, include: bool) -> Result<()> {
    let (add, remove) = if include {
        (yes_key(channel_id), no_key(channel_id))
    } else {
        (no_key(channel_id), yes_key(channel_id))
    };
    Pipeline::new()
        .sadd(add, user_id)
        .srem(remove, user_id)
        .query_async::<_, ()>(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()
}

/// Start a recording unless one runs already. `ttl_secs` is a safety net:
/// a recording nobody finishes goes by itself. Returns whether it started.
pub async fn start_capture_recording(
    channel_id: &str,
    recording: &VoiceRecordingLive,
    ttl_secs: u64,
) -> Result<bool> {
    let text = serde_json::to_string(recording).to_internal_error()?;
    let set: Option<String> = redis_kiss::redis::cmd("SET")
        .arg(recording_key(channel_id))
        .arg(text)
        .arg("NX")
        .arg("EX")
        .arg(ttl_secs.max(60))
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()?;
    Ok(set.is_some())
}

/// The recording that runs in this room
pub async fn get_capture_recording(channel_id: &str) -> Result<Option<VoiceRecordingLive>> {
    let text: Option<String> = get_connection()
        .await?
        .get(recording_key(channel_id))
        .await
        .to_internal_error()?;
    Ok(text.and_then(|text| serde_json::from_str(&text).ok()))
}

/// End the recording. Returns whether one was running.
pub async fn clear_capture_recording(channel_id: &str) -> Result<bool> {
    let (removed, _): (usize, usize) = Pipeline::new()
        .del(recording_key(channel_id))
        .del(ever_key(channel_id))
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()?;
    Ok(removed > 0)
}

/// These people's voices are in the running recording from now on
pub async fn note_recording_voices(channel_id: &str, user_ids: &[String]) -> Result<()> {
    if user_ids.is_empty() {
        return Ok(());
    }
    get_connection()
        .await?
        .sadd::<_, _, ()>(ever_key(channel_id), user_ids)
        .await
        .to_internal_error()
}

/// Everyone whose voice may be in the running recording, and everyone who
/// chose to be left out of this room
pub async fn get_recording_voices(channel_id: &str) -> Result<(Vec<String>, Vec<String>)> {
    Pipeline::new()
        .smembers(ever_key(channel_id))
        .smembers(no_key(channel_id))
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()
}

/// Forget everything (the room is empty or gone)
pub async fn clear_capture_state(channel_id: &str) -> Result<()> {
    get_connection()
        .await?
        .del::<_, ()>(vec![
            clips_key(channel_id),
            recording_key(channel_id),
            ever_key(channel_id),
            yes_key(channel_id),
            no_key(channel_id),
        ])
        .await
        .to_internal_error()
}

/// Tell everyone who sees the channel
pub async fn publish_capture_state(channel_id: &str, state: VoiceCaptureState) {
    EventV1::VoiceCaptureUpdate {
        id: channel_id.to_string(),
        data: state,
    }
    .p(channel_id.to_string())
    .await;
}

/// Someone left the room (call after their seat is gone). Their choice is
/// forgotten, a recording they started ends, and an empty room forgets
/// everything. The room is told when anything was on.
pub async fn capture_after_leave(channel: &UserVoiceChannel, user_id: &str, opt_in: bool) -> Result<()> {
    let (clips, recording): (Option<String>, Option<String>) = Pipeline::new()
        .get(clips_key(&channel.id))
        .get(recording_key(&channel.id))
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()?;
    if clips.is_none() && recording.is_none() {
        return Ok(());
    }

    let members = get_voice_channel_members(channel).await?.unwrap_or_default();
    if members.is_empty() {
        clear_capture_state(&channel.id).await?;
        publish_capture_state(&channel.id, VoiceCaptureState::default()).await;
        return Ok(());
    }

    let recorder_left = recording
        .and_then(|text| serde_json::from_str::<VoiceRecordingLive>(&text).ok())
        .is_some_and(|recording| recording.by == user_id);

    let mut pipeline = Pipeline::new();
    pipeline
        .srem(yes_key(&channel.id), user_id)
        .srem(no_key(&channel.id), user_id);
    if recorder_left {
        // the audio was on their device: there is nothing left to save
        pipeline.del(recording_key(&channel.id));
        pipeline.del(ever_key(&channel.id));
    }
    pipeline
        .query_async::<_, ()>(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()?;

    publish_capture_state(&channel.id, get_capture_state(channel, opt_in).await?).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::included_of;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn opt_in_includes_only_people_who_said_yes() {
        let members = ids(&["a", "b", "c"]);
        assert_eq!(included_of(&members, &ids(&["b"]), &[], true), ids(&["b"]));
        assert!(included_of(&members, &[], &[], true).is_empty());
    }

    #[test]
    fn opt_out_includes_everyone_who_did_not_say_no() {
        let members = ids(&["a", "b", "c"]);
        assert_eq!(included_of(&members, &[], &ids(&["b"]), false), ids(&["a", "c"]));
    }

    #[test]
    fn someone_who_left_the_room_is_never_included() {
        let members = ids(&["a"]);
        assert_eq!(included_of(&members, &ids(&["a", "gone"]), &[], true), ids(&["a"]));
        assert_eq!(included_of(&members, &[], &[], false), ids(&["a"]));
    }
}
