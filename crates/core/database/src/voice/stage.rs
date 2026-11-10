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

/// What adding a speaker did
#[derive(Debug, PartialEq, Eq)]
pub enum SpeakerAdded {
    /// Now a speaker; whether a raised hand went down with it
    Added { hand_lowered: bool },
    /// Already a speaker
    Already,
    /// The stage has the most speakers it may have
    Full,
}

/// Check and add in one step, so two invites at once can't pass the limit
const ADD_SPEAKER_SCRIPT: &str = r"
if redis.call('SISMEMBER', KEYS[1], ARGV[1]) == 1 then return -1 end
if redis.call('SCARD', KEYS[1]) >= tonumber(ARGV[2]) then return -2 end
redis.call('SADD', KEYS[1], ARGV[1])
return redis.call('ZREM', KEYS[2], ARGV[1])
";

/// Make a user a speaker (their raised hand goes down)
pub async fn add_stage_speaker(channel_id: &str, user_id: &str) -> Result<SpeakerAdded> {
    let result: i64 = redis_kiss::redis::cmd("EVAL")
        .arg(ADD_SPEAKER_SCRIPT)
        .arg(2)
        .arg(speakers_key(channel_id))
        .arg(hands_key(channel_id))
        .arg(user_id)
        .arg(MAX_STAGE_SPEAKERS)
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()?;
    Ok(match result {
        -1 => SpeakerAdded::Already,
        -2 => SpeakerAdded::Full,
        lowered => SpeakerAdded::Added {
            hand_lowered: lowered > 0,
        },
    })
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

/// Seconds before the same person may raise a hand again
pub const STAGE_HAND_COOLDOWN_SECS: u64 = 20;

/// What raising a hand did
#[derive(Debug, PartialEq, Eq)]
pub enum HandRaised {
    Raised,
    /// It was up already (keeps its place)
    Already,
    /// Too soon after the last time; milliseconds to wait
    TooSoon(u64),
    /// The queue is full
    Full,
}

/// Cooldown, limit and order in one step
const RAISE_HAND_SCRIPT: &str = r"
if redis.call('ZSCORE', KEYS[1], ARGV[1]) then return -1 end
local wait = redis.call('PTTL', KEYS[2])
if wait > 0 then return -1000 - wait end
if redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[3]) then return -2 end
redis.call('ZADD', KEYS[1], ARGV[2], ARGV[1])
redis.call('SET', KEYS[2], '1', 'EX', ARGV[4])
return 1
";

fn hand_cooldown_key(channel_id: &str, user_id: &str) -> String {
    format!("stage_hand_cd:{channel_id}:{user_id}")
}

/// Raise a hand (kept in the order hands went up), at most once every
/// 20 seconds per person
pub async fn raise_stage_hand(channel_id: &str, user_id: &str, at_millis: i64) -> Result<HandRaised> {
    let result: i64 = redis_kiss::redis::cmd("EVAL")
        .arg(RAISE_HAND_SCRIPT)
        .arg(2)
        .arg(hands_key(channel_id))
        .arg(hand_cooldown_key(channel_id, user_id))
        .arg(user_id)
        .arg(at_millis)
        .arg(MAX_STAGE_HANDS)
        .arg(STAGE_HAND_COOLDOWN_SECS)
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()?;
    Ok(match result {
        1 => HandRaised::Raised,
        -1 => HandRaised::Already,
        -2 => HandRaised::Full,
        wait => HandRaised::TooSoon((-wait - 1000).max(1) as u64),
    })
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
