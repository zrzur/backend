//! Clears voice seats that LiveKit no longer knows about.
//!
//! Seats are added and removed by webhooks. When LiveKit restarts, or this
//! daemon is down while people leave, no `participant_left` arrives and the
//! user stays listed in the channel forever. This compares Redis with the
//! rooms each node really has and removes the leftovers.

use std::collections::{HashMap, HashSet};

use redis_kiss::{get_connection, redis::cmd};
use revolt_database::{
    events::client::EventV1,
    iso8601_timestamp::{Duration, Timestamp},
    voice::{
        delete_voice_state, get_channel_node, get_user_voice_channels, get_voice_channel_members,
        get_voice_state, UserVoiceChannel, VoiceClient,
    },
};
use revolt_result::{create_error, Result, ToRevoltError};

/// How often to compare Redis with LiveKit.
pub const INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Seats younger than this are left alone: the user may still be connecting.
const GRACE_SECONDS: i64 = 30;

/// Every channel that has at least one seat in Redis.
async fn channels_with_seats() -> Result<Vec<String>> {
    let mut conn = get_connection()
        .await
        .map_err(|_| create_error!(InternalError))?
        .into_inner();
    let mut cursor: u64 = 0;
    let mut ids = Vec::new();

    loop {
        let (next, keys): (u64, Vec<String>) = cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg("vc_members:*")
            .arg("COUNT")
            .arg(200)
            .query_async(&mut conn)
            .await
            .to_internal_error()?;

        ids.extend(
            keys.into_iter()
                .filter_map(|key| key.strip_prefix("vc_members:").map(str::to_string)),
        );

        if next == 0 {
            break;
        }
        cursor = next;
    }

    Ok(ids)
}

/// One pass: remove every seat whose user is not in the LiveKit room.
pub async fn reconcile(voice_client: &VoiceClient) -> Result<()> {
    // Rooms that exist on each reachable node. A node that does not answer
    // is skipped, so a short outage never empties its channels.
    let mut live_rooms: HashMap<&str, HashSet<String>> = HashMap::new();
    for (name, room) in &voice_client.rooms {
        match room.client.list_rooms(Vec::new()).await {
            Ok(rooms) => {
                live_rooms.insert(name, rooms.into_iter().map(|r| r.name).collect());
            }
            Err(error) => log::warn!("voice reconcile: node {name} unreachable: {error:?}"),
        }
    }

    let only_node = (voice_client.rooms.len() == 1)
        .then(|| voice_client.rooms.keys().next().cloned())
        .flatten();

    for channel_id in channels_with_seats().await? {
        let Some(node) = get_channel_node(&channel_id).await?.or_else(|| only_node.clone())
        else {
            continue;
        };
        let Some(rooms) = live_rooms.get(node.as_str()) else {
            continue;
        };

        let present: HashSet<String> = if rooms.contains(&channel_id) {
            match voice_client.get_room_participants(&node, &channel_id).await {
                Ok(participants) => participants.into_iter().map(|p| p.identity).collect(),
                Err(_) => continue,
            }
        } else {
            HashSet::new()
        };

        let seats = get_voice_channel_members(&UserVoiceChannel {
            id: channel_id.clone(),
            server_id: None,
        })
        .await?
        .unwrap_or_default();

        for user_id in seats {
            if present.contains(&user_id) {
                continue;
            }

            // The user's own record carries the server id the state keys use.
            let channel = get_user_voice_channels(&user_id)
                .await?
                .into_iter()
                .find(|channel| channel.id == channel_id)
                .unwrap_or_else(|| UserVoiceChannel {
                    id: channel_id.clone(),
                    server_id: None,
                });

            if let Some(state) = get_voice_state(&channel, &user_id).await? {
                if Timestamp::now_utc().duration_since(state.joined_at)
                    < Duration::seconds(GRACE_SECONDS)
                {
                    continue;
                }
            }

            log::info!("voice reconcile: removing stale seat of {user_id} in {channel_id}");
            delete_voice_state(&channel, &user_id).await?;

            EventV1::VoiceChannelLeave {
                id: channel_id.clone(),
                user: user_id,
            }
            .p(channel_id.clone())
            .await;
        }
    }

    Ok(())
}
