//! Live state of stage channels, kept in Redis next to the voice state.
//!
//! A stage has a topic, a set of speakers and a queue of raised hands. Only
//! speakers may publish; everyone else joins as audience with a token that
//! cannot publish anything, so an audience seat costs LiveKit no tracks.
//! The state lives only while the stage is used: ending the stage, or the
//! room closing, removes it.

use redis_kiss::{redis::Pipeline, AsyncCommands};
use revolt_models::v0::StageState;
use revolt_result::{Result, ToRevoltError};

use super::get_connection;

/// Most speakers on one stage at a time
pub const MAX_STAGE_SPEAKERS: usize = 50;
/// Most raised hands kept (and sent) per stage
pub const MAX_STAGE_HANDS: usize = 200;

pub(crate) fn topic_key(channel_id: &str) -> String {
    format!("stage_topic:{channel_id}")
}

pub(crate) fn speakers_key(channel_id: &str) -> String {
    format!("stage_speakers:{channel_id}")
}

pub(crate) fn hands_key(channel_id: &str) -> String {
    format!("stage_hands:{channel_id}")
}

/// Current state of a stage (empty when nobody started it)
pub async fn get_stage_state(channel_id: &str) -> Result<StageState> {
    let (topic, speakers, hands): (Option<String>, Vec<String>, Vec<String>) = Pipeline::new()
        .get(topic_key(channel_id))
        .smembers(speakers_key(channel_id))
        .zrange(hands_key(channel_id), 0, MAX_STAGE_HANDS as isize - 1)
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()?;

    let mut speakers = speakers;
    speakers.sort();

    Ok(StageState {
        topic,
        speakers,
        hands,
    })
}

/// Whether this user may speak on the stage
pub async fn is_stage_speaker(channel_id: &str, user_id: &str) -> Result<bool> {
    get_connection()
        .await?
        .sismember(speakers_key(channel_id), user_id)
        .await
        .to_internal_error()
}

/// Set or clear the topic
pub async fn set_stage_topic(channel_id: &str, topic: Option<&str>) -> Result<()> {
    let mut conn = get_connection().await?;
    match topic {
        Some(topic) => conn.set(topic_key(channel_id), topic).await,
        None => conn.del(topic_key(channel_id)).await,
    }
    .to_internal_error()
}

/// Make a user a speaker (their raised hand goes down). Returns false when
/// the stage already has the most speakers it may have.
pub async fn add_stage_speaker(channel_id: &str, user_id: &str) -> Result<bool> {
    let mut conn = get_connection().await?;
    let already: bool = conn
        .sismember(speakers_key(channel_id), user_id)
        .await
        .to_internal_error()?;
    if !already {
        let count: usize = conn
            .scard(speakers_key(channel_id))
            .await
            .to_internal_error()?;
        if count >= MAX_STAGE_SPEAKERS {
            return Ok(false);
        }
    }

    Pipeline::new()
        .sadd(speakers_key(channel_id), user_id)
        .zrem(hands_key(channel_id), user_id)
        .query_async::<_, ()>(&mut conn.into_inner())
        .await
        .to_internal_error()?;
    Ok(true)
}

/// Move a speaker back to the audience. Returns whether they were a speaker.
pub async fn remove_stage_speaker(channel_id: &str, user_id: &str) -> Result<bool> {
    let removed: usize = get_connection()
        .await?
        .srem(speakers_key(channel_id), user_id)
        .await
        .to_internal_error()?;
    Ok(removed > 0)
}

/// Raise a hand (kept in the order hands went up). Returns false when the
/// queue is full; raising an already raised hand keeps its place.
pub async fn raise_stage_hand(channel_id: &str, user_id: &str, at_millis: i64) -> Result<bool> {
    let mut conn = get_connection().await?;
    let count: usize = conn.zcard(hands_key(channel_id)).await.to_internal_error()?;
    if count >= MAX_STAGE_HANDS {
        let present: Option<f64> = conn
            .zscore(hands_key(channel_id), user_id)
            .await
            .to_internal_error()?;
        return Ok(present.is_some());
    }

    redis_kiss::redis::cmd("ZADD")
        .arg(hands_key(channel_id))
        .arg("NX")
        .arg(at_millis)
        .arg(user_id)
        .query_async::<_, ()>(&mut conn.into_inner())
        .await
        .to_internal_error()?;
    Ok(true)
}

/// Lower a hand. Returns whether it was raised.
pub async fn lower_stage_hand(channel_id: &str, user_id: &str) -> Result<bool> {
    let removed: usize = get_connection()
        .await?
        .zrem(hands_key(channel_id), user_id)
        .await
        .to_internal_error()?;
    Ok(removed > 0)
}

/// Forget everything about the stage (topic, speakers, hands)
pub async fn clear_stage_state(channel_id: &str) -> Result<()> {
    get_connection()
        .await?
        .del::<_, ()>(vec![
            topic_key(channel_id),
            speakers_key(channel_id),
            hands_key(channel_id),
        ])
        .await
        .to_internal_error()
}

/// Publish sources a member keeps on a stage. Speakers keep what their
/// permissions give them; the audience publishes nothing.
pub fn stage_sources(sources: Vec<&'static str>, is_stage: bool, is_speaker: bool) -> Vec<&'static str> {
    if is_stage && !is_speaker {
        Vec::new()
    } else {
        sources
    }
}

#[cfg(test)]
mod tests {
    use super::stage_sources;

    #[test]
    fn audience_publishes_nothing() {
        let all = vec!["microphone", "camera", "screen_share", "screen_share_audio"];
        assert!(stage_sources(all.clone(), true, false).is_empty());
        assert_eq!(stage_sources(all.clone(), true, true), all);
        assert_eq!(stage_sources(all.clone(), false, false), all);
    }
}
