//! Ringing in DM and group calls.
//!
//! When someone starts a call in a DM or group, the other people are "rung"
//! until they join, decline, the caller stops ringing them, the call ends,
//! or `RING_TIMEOUT_SECS` pass. Who is still being rung is a Redis set per
//! channel; whether anyone answered is a flag read when the call ends (a call
//! nobody answered is a missed call).

use redis_kiss::{redis::Pipeline, AsyncCommands};
use revolt_result::{Result, ToRevoltError};

use super::get_connection;
use crate::{
    events::client::{EventV1, RingEndReason},
    AMQP,
};

/// How long people are rung before the call counts as unanswered for them
pub const RING_TIMEOUT_SECS: u64 = 45;

/// Ring state outlives the timeout a little, so a late join still finds it;
/// it never outlives a forgotten call
const RING_STATE_TTL_SECS: usize = 180;

/// Calls last; the answered flag lives as long as a long call
const ANSWERED_TTL_SECS: usize = 60 * 60 * 24;

fn ringing_key(channel_id: &str) -> String {
    format!("ringing:{channel_id}")
}

fn ring_by_key(channel_id: &str) -> String {
    format!("ring_by:{channel_id}")
}

fn ring_id_key(channel_id: &str) -> String {
    format!("ring_id:{channel_id}")
}

fn answered_key(channel_id: &str) -> String {
    format!("call_answered:{channel_id}")
}

/// Start ringing these users for a call `initiator` started in this channel
/// (replaces any earlier ring of the channel). `ring_id` names this ring, so
/// a timer of an earlier call never ends a newer one.
pub async fn start_ringing(
    channel_id: &str,
    ring_id: &str,
    initiator: &str,
    users: &[String],
) -> Result<()> {
    let key = ringing_key(channel_id);
    let mut pipeline = Pipeline::new();
    pipeline
        .atomic()
        .del(&key)
        .del(answered_key(channel_id))
        .set_ex(ring_by_key(channel_id), initiator, RING_STATE_TTL_SECS)
        .set_ex(ring_id_key(channel_id), ring_id, RING_STATE_TTL_SECS);
    if !users.is_empty() {
        pipeline
            .sadd(&key, users)
            .expire(&key, RING_STATE_TTL_SECS);
    }
    pipeline
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()
}

/// Stop ringing one user; true when they were still being rung
pub async fn stop_ringing(channel_id: &str, user_id: &str) -> Result<bool> {
    get_connection()
        .await?
        .srem::<_, _, u32>(ringing_key(channel_id), user_id)
        .await
        .map(|removed| removed > 0)
        .to_internal_error()
}

/// Everyone still being rung in this channel; ringing ends for all of them
pub async fn take_ringing(channel_id: &str) -> Result<Vec<String>> {
    let key = ringing_key(channel_id);
    let (users, _deleted): (Vec<String>, i64) = Pipeline::new()
        .atomic()
        .smembers(&key)
        .del(&key)
        .query_async(&mut get_connection().await?.into_inner())
        .await
        .to_internal_error()?;
    Ok(users)
}

/// Someone other than the caller joined: the call was answered
pub async fn mark_call_answered(channel_id: &str) -> Result<()> {
    get_connection()
        .await?
        .set_ex(answered_key(channel_id), 1, ANSWERED_TTL_SECS)
        .await
        .to_internal_error()
}

/// Whether the call that just ended was answered (reads and clears)
pub async fn take_call_answered(channel_id: &str) -> Result<bool> {
    get_connection()
        .await?
        .get_del::<_, Option<u8>>(answered_key(channel_id))
        .await
        .map(|value| value.is_some())
        .to_internal_error()
}

/// Is this ring still the channel's current one?
pub async fn is_current_ring(channel_id: &str, ring_id: &str) -> Result<bool> {
    get_connection()
        .await?
        .get::<_, Option<String>>(ring_id_key(channel_id))
        .await
        .map(|current| current.as_deref() == Some(ring_id))
        .to_internal_error()
}

/// Who started the call that is ringing in this channel
pub async fn ring_initiator(channel_id: &str) -> Result<Option<String>> {
    get_connection()
        .await?
        .get(ring_by_key(channel_id))
        .await
        .to_internal_error()
}

/// Tell the channel (the caller, and the other devices of these people) that
/// ringing ended for them, and replace their call notification with one that
/// says why. `users` must already be taken out of the ring set.
pub async fn announce_ring_end(
    amqp: Option<&AMQP>,
    channel_id: &str,
    initiator: Option<&str>,
    users: Vec<String>,
    reason: RingEndReason,
) {
    if users.is_empty() {
        return;
    }
    for user in &users {
        EventV1::VoiceRingEnd {
            channel_id: channel_id.to_string(),
            user: user.clone(),
            reason,
        }
        .p(channel_id.to_string())
        .await;
    }
    // the person who answered is in the call: their page shows it, and
    // a push to their other devices would only repeat it
    if reason == RingEndReason::Answered {
        return;
    }
    let initiator = match initiator {
        Some(initiator) => Some(initiator.to_string()),
        None => ring_initiator(channel_id).await.ok().flatten(),
    };
    if let (Some(amqp), Some(initiator)) = (amqp, initiator) {
        if let Err(error) = amqp
            .dm_call_ring_ended(&initiator, channel_id, reason, users)
            .await
        {
            log::error!("ring end push for {channel_id}: {error:?}");
        }
    }
}
