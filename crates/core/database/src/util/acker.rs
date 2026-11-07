use redis_kiss::{get_connection, redis::cmd, AsyncCommands, Conn};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{Result, ToRevoltError};

use crate::{events::client::EventV1, Channel, Database, Server, User, AMQP};

/// Seconds the "queue message is out" marker lives. It is the only thing that
/// stops an ack from being queued, so a lost queue message blocks a channel
/// for this long at most. Later acks never extend it.
pub const ACK_KEY_TTL_SECONDS: usize = 120;

/// Seconds the newest acked message waits for crond, longer than any backlog
const ACK_VALUE_TTL_SECONDS: usize = 86_400;

/// Holds the newest acked message of a channel
fn ack_key(user: &str, channel: &str) -> String {
    format!("acker:{user}+{channel}")
}

/// Set while a queue message for this channel is on its way to crond
fn ack_queued_key(user: &str, channel: &str) -> String {
    format!("acker-queued:{user}+{channel}")
}

/// Record the newest acked message; true when the caller has to queue it.
///
/// A leftover `acker:` key (no marker beside it) is simply overwritten and
/// queued, so old keys in Redis repair themselves on the next ack.
async fn mark_ack(redis: &mut Conn, user: &str, channel: &str, message: &str) -> Result<bool> {
    let connection = &mut **redis;

    // the value first: see the order in `take_ack`
    let _: () = cmd("SET")
        .arg(ack_key(user, channel))
        .arg(message)
        .arg("EX")
        .arg(ACK_VALUE_TTL_SECONDS)
        .query_async(connection)
        .await
        .to_internal_error()?;

    let claimed: Option<String> = cmd("SET")
        .arg(ack_queued_key(user, channel))
        .arg(1)
        .arg("NX")
        .arg("EX")
        .arg(ACK_KEY_TTL_SECONDS)
        .query_async(connection)
        .await
        .to_internal_error()?;

    Ok(claimed.is_some())
}

/// Nothing was queued after all: the marker must not claim otherwise
async fn release_ack(redis: &mut Conn, user: &str, channel: &str) {
    let _: Result<(), _> = redis.del(ack_queued_key(user, channel)).await;
}

/// Crond: take the newest acked message of a channel.
///
/// The marker goes first. An ack landing between the two steps then either
/// is read here, or finds no marker and queues a message of its own; the
/// other order could leave it with neither.
#[allow(clippy::disallowed_methods)]
pub async fn take_ack(redis: &mut Conn, user: &str, channel: &str) -> Result<Option<String>> {
    let _: () = redis
        .del(ack_queued_key(user, channel))
        .await
        .to_internal_error()?;
    redis
        .get_del(ack_key(user, channel))
        .await
        .to_internal_error()
}

pub async fn ack_channel(user: &str, channel: &str, message: &str, amqp: &AMQP) -> Result<()> {
    let mut redis = get_connection()
        .await
        .map_err(|_| create_error!(InternalError))?;

    if mark_ack(&mut redis, user, channel, message).await? {
        if let Err(_) = amqp.process_ack(user, Some(channel), None).await {
            release_ack(&mut redis, user, channel).await;
            return Err(create_error!(InternalError));
        }
    }

    Ok(())
}

pub async fn ack_server(user: &User, server: &Server, db: &Database, amqp: &AMQP) -> Result<()> {
    let mut redis = get_connection()
        .await
        .map_err(|_| create_error!(InternalError))?;

    let channels = db.fetch_channels(&server.channels).await?;
    let query = crate::util::permissions::DatabasePermissionQuery::new(db, user).server(server);

    for channel in channels {
        let channel_id = channel.id();
        let mut q = query.clone().channel(&channel);

        if calculate_channel_permissions(&mut q)
            .await
            .has_channel_permission(ChannelPermission::ViewChannel)
        {
            let channel_last_msg = match &channel {
                Channel::TextChannel {
                    last_message_id, ..
                } => last_message_id,
                _ => unreachable!(),
            }
            .clone();

            if let Some(channel_last_msg) = channel_last_msg {
                if mark_ack(&mut redis, &user.id, channel_id, &channel_last_msg).await? {
                    if let Err(_) = amqp
                        .process_ack(&user.id, Some(channel_id), Some(&server.id))
                        .await
                    {
                        release_ack(&mut redis, &user.id, channel_id).await;
                        return Err(create_error!(InternalError));
                    }

                    EventV1::ChannelAck {
                        id: channel_id.to_string(),
                        user: user.id.clone(),
                        message_id: channel_last_msg,
                    }
                    .p(user.id.clone())
                    .await;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn connect() -> Conn {
        // reading the config points redis-kiss at the test Redis
        revolt_config::config().await;
        get_connection().await.expect("test Redis")
    }

    fn fresh_ids() -> (String, String) {
        (ulid::Ulid::new().to_string(), ulid::Ulid::new().to_string())
    }

    async fn seconds_left(redis: &mut Conn, key: &str) -> i64 {
        cmd("TTL").arg(key).query_async(&mut **redis).await.unwrap()
    }

    #[test]
    fn keys_are_the_ones_crond_reads() {
        assert_eq!(ack_key("u", "c"), "acker:u+c");
        assert_eq!(ack_queued_key("u", "c"), "acker-queued:u+c");
    }

    #[tokio::test]
    async fn only_the_first_ack_is_queued_and_crond_reads_the_newest() {
        let mut redis = connect().await;
        let (user, channel) = fresh_ids();

        assert!(mark_ack(&mut redis, &user, &channel, "m1").await.unwrap());
        assert!(!mark_ack(&mut redis, &user, &channel, "m2").await.unwrap());
        assert_eq!(
            take_ack(&mut redis, &user, &channel).await.unwrap().as_deref(),
            Some("m2")
        );

        // crond is done: the next ack needs a queue message of its own
        assert!(mark_ack(&mut redis, &user, &channel, "m3").await.unwrap());
        assert_eq!(
            take_ack(&mut redis, &user, &channel).await.unwrap().as_deref(),
            Some("m3")
        );
        assert_eq!(take_ack(&mut redis, &user, &channel).await.unwrap(), None);
    }

    /// What the dev Redis held on 2026-11-04: a marker with no expiry and no
    /// queue message behind it. Every ack of the channel was dropped.
    #[tokio::test]
    async fn a_leftover_key_does_not_block_the_channel() {
        let mut redis = connect().await;
        let (user, channel) = fresh_ids();
        let _: () = redis.set(ack_key(&user, &channel), "old").await.unwrap();

        assert!(mark_ack(&mut redis, &user, &channel, "new").await.unwrap());
        assert_eq!(
            take_ack(&mut redis, &user, &channel).await.unwrap().as_deref(),
            Some("new")
        );
    }

    #[tokio::test]
    async fn markers_expire_and_later_acks_do_not_postpone_it() {
        let mut redis = connect().await;
        let (user, channel) = fresh_ids();
        let queued = ack_queued_key(&user, &channel);

        assert!(mark_ack(&mut redis, &user, &channel, "m1").await.unwrap());
        let left = seconds_left(&mut redis, &queued).await;
        assert!(left > 0 && left <= ACK_KEY_TTL_SECONDS as i64, "{left}");
        assert!(seconds_left(&mut redis, &ack_key(&user, &channel)).await > 0);

        // someone keeps reading the channel while the queue message is lost
        let _: () = redis.expire(&queued, 30).await.unwrap();
        assert!(!mark_ack(&mut redis, &user, &channel, "m2").await.unwrap());
        assert!(seconds_left(&mut redis, &queued).await <= 30);
    }

    #[tokio::test]
    async fn a_lost_queue_message_blocks_only_until_the_marker_expires() {
        let mut redis = connect().await;
        let (user, channel) = fresh_ids();

        assert!(mark_ack(&mut redis, &user, &channel, "m1").await.unwrap());
        assert!(!mark_ack(&mut redis, &user, &channel, "m2").await.unwrap());

        // the marker runs out; the newest ack still gets saved by the next one
        let _: () = redis.del(ack_queued_key(&user, &channel)).await.unwrap();
        assert!(mark_ack(&mut redis, &user, &channel, "m3").await.unwrap());
        assert_eq!(
            take_ack(&mut redis, &user, &channel).await.unwrap().as_deref(),
            Some("m3")
        );
    }

    #[tokio::test]
    async fn a_failed_publish_leaves_nothing_in_the_way() {
        let mut redis = connect().await;
        let (user, channel) = fresh_ids();

        assert!(mark_ack(&mut redis, &user, &channel, "m1").await.unwrap());
        release_ack(&mut redis, &user, &channel).await;
        assert!(mark_ack(&mut redis, &user, &channel, "m1").await.unwrap());
    }
}
