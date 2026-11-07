use std::{collections::HashSet, net::SocketAddr, num::NonZeroUsize, sync::Arc};

use async_tungstenite::WebSocketStream;
use fred::{
    error::RedisErrorKind,
    interfaces::{ClientLike, EventInterface, PubsubInterface},
    types::{ReconnectPolicy, RedisConfig},
};
use futures::{
    channel::oneshot,
    join, pin_mut, select,
    stream::{SplitSink, SplitStream},
    FutureExt, SinkExt, StreamExt, TryStreamExt,
};
use redis_kiss::{get_connection, AsyncCommands, PayloadType, REDIS_PAYLOAD_TYPE, REDIS_URI};
use revolt_config::report_internal_error;
use revolt_database::{
    events::{client::EventV1, server::ClientMessage},
    iso8601_timestamp::Timestamp,
    Database, Session, User, UserHint,
};
use revolt_presence::{create_session, delete_session};

use revolt_result::create_error;
use sentry::Level;
use tokio::{
    net::TcpStream,
    sync::{Mutex, RwLock},
    task::spawn,
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

use crate::config::{ProtocolConfiguration, WebsocketHandshakeCallback};
use crate::events::state::{State, SubscriptionStateChange};
use revolt_models::v0;

/// Time to finish the WebSocket handshake
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Time to send the session token after connecting; idle unauthenticated
/// sockets would otherwise stay open forever
const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// How often an open socket checks that its session still exists
const SESSION_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);
/// Largest message a client may send: everything a client sends (auth,
/// ping, typing, subscribe) is tiny, the library default is 64 MiB
const MAX_CLIENT_MESSAGE: usize = 1024 * 1024;

type WsReader = SplitStream<WebSocketStream<Compat<TcpStream>>>;
type WsWriter =
    SplitSink<WebSocketStream<Compat<TcpStream>>, async_tungstenite::tungstenite::Message>;

/// Start a new WebSocket client worker given access to the database,
/// the relevant TCP stream and the remote address of the client.
pub async fn client(db: &'static Database, stream: TcpStream, addr: SocketAddr) {
    client_with_check(db, stream, addr, SESSION_CHECK_EVERY).await
}

async fn client_with_check(
    db: &'static Database,
    stream: TcpStream,
    addr: SocketAddr,
    check_every: std::time::Duration,
) {
    // Upgrade the TCP connection to a WebSocket connection.
    // In this process, we also parse any additional parameters given.
    // e.g. wss://example.com?format=json&version=1
    let (sender, receiver) = oneshot::channel();
    let ws_config = async_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(MAX_CLIENT_MESSAGE),
        max_frame_size: Some(MAX_CLIENT_MESSAGE),
        ..Default::default()
    };
    let Ok(Ok(ws)) = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        async_tungstenite::accept_hdr_async_with_config(
            stream.compat(),
            WebsocketHandshakeCallback::from(sender),
            Some(ws_config),
        ),
    )
    .await
    else {
        return;
    };

    // Verify we've received a valid config, otherwise we should just drop the connection.
    let Ok(mut config) = receiver.await else {
        return;
    };

    info!(
        "User {addr:?} provided protocol configuration (version = {}, format = {:?})",
        config.get_protocol_version(),
        config.get_protocol_format()
    );

    // Split the socket for simultaneously read and write.
    let (mut write, mut read) = ws.split();

    // If the user has not provided authentication, request information.
    if config.get_session_token().is_none() {
        let wait_for_token = async {
            while let Ok(Some(message)) = read.try_next().await {
                if let Ok(ClientMessage::Authenticate { token, intents }) = config.decode(&message)
                {
                    config.set_session_token(token);
                    config.set_intents(intents);
                    break;
                }
            }
        };
        // no token in time: fall through to the InvalidSession error below
        let _ = tokio::time::timeout(AUTH_TIMEOUT, wait_for_token).await;
    }

    // Try to authenticate the user.
    let Some(token) = config.get_session_token().as_ref() else {
        write
            .send(config.encode(&EventV1::Error {
                data: create_error!(InvalidSession),
            }))
            .await
            .ok();
        return;
    };

    let (user, session_id) = match User::from_token(db, token, UserHint::Any).await {
        Ok(user) => user,
        Err(err) => {
            write
                .send(config.encode(&EventV1::Error { data: err }))
                .await
                .ok();
            return;
        }
    };

    // Bots receive only what their intents allow; asking for a privileged
    // intent that is not on for this bot closes the connection (4014). The
    // API's rule, so past the server limit only approved intents count.
    let intents = if user.bot.is_some() {
        match revolt_database::connection_intents(db, &user, config.get_intents()).await {
            Ok(intents) => intents,
            Err(missing) => {
                let intent = missing.join(", ");
                write
                    .send(config.encode(&EventV1::Error {
                        data: create_error!(MissingIntent {
                            intent: intent.clone()
                        }),
                    }))
                    .await
                    .ok();
                write
                    .send(async_tungstenite::tungstenite::Message::Close(Some(
                        async_tungstenite::tungstenite::protocol::CloseFrame {
                        code: async_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Library(4014),
                        reason: format!("Disallowed intents: {intent}").into(),
                    })))
                    .await
                    .ok();
                return;
            }
        }
    } else {
        // as the API: nothing until an age is declared
        if !revolt_database::util::age::has_declared_age(db, &user).await {
            write
                .send(config.encode(&EventV1::Error {
                    data: create_error!(AgeNotDeclared),
                }))
                .await
                .ok();
            return;
        }
        v0::GatewayIntents::ALL
    };

    info!("User {addr:?} authenticated as @{}", user.username);

    db.update_session_last_seen(&session_id, Timestamp::now_utc())
        .await
        .ok();

    let backend_config = revolt_config::config().await;

    // Create local state.
    let mut state = State::from(
        user,
        session_id,
        NonZeroUsize::new(backend_config.features.advanced.seen_events_cache_size as usize)
            .expect("config.features.advanced.seen_events_cache_size cannot be 0!"),
    );
    state.cache.intents = intents;
    let user_id = state.cache.user_id.clone();
    let session_id_for_worker = state.session_id.clone();

    // Notify socket we have authenticated.
    if report_internal_error!(write.send(config.encode(&EventV1::Authenticated)).await).is_err() {
        return;
    }

    // Download required data to local cache and send Ready payload.
    let ready_payload = match report_internal_error!(
        state
            .generate_ready_payload(db, config.get_ready_payload_fields())
            .await
    ) {
        Ok(ready_payload) => ready_payload,
        Err(_) => return,
    };

    if report_internal_error!(write.send(config.encode(&ready_payload)).await).is_err() {
        return;
    }

    let slowmodes = fetch_user_slowmodes(&user_id).await.unwrap_or_default();
    if !slowmodes.is_empty() {
        let event = EventV1::UserSlowmodes { slowmodes };
        if report_internal_error!(write.send(config.encode(&event)).await).is_err() {
            return;
        }
    }

    // Create presence session.
    let (first_session, session_id) = create_session(&user_id, 0).await;

    // If this was the first session, notify other users that we just went online.
    if first_session {
        state.broadcast_presence_change(true).await;
    }

    {
        // Setup channels and mutexes
        let write = Mutex::new(write);
        let subscribed = state.subscribed.clone();
        let active_servers = state.active_servers.clone();
        let (topic_signal_s, topic_signal_r) = async_channel::unbounded();

        // TODO: this needs to be rewritten
        // Create channels through which the tasks can signal to each other they need to clean up
        let (kill_signal_1_s, kill_signal_1_r) = async_channel::bounded(1);
        let (kill_signal_2_s, kill_signal_2_r) = async_channel::bounded(1);

        // Create a PubSub connection to poll on.
        let listener = listener_with_kill_signal(
            db,
            &mut state,
            addr,
            &config,
            topic_signal_r,
            kill_signal_1_r,
            &write,
            kill_signal_2_s,
        );

        // Read from WebSocket stream.
        let worker = worker_with_kill_signal(
            addr,
            subscribed,
            active_servers,
            user_id.clone(),
            &config,
            topic_signal_s,
            kill_signal_2_r,
            read,
            &write,
            kill_signal_1_s,
            db,
            session_id_for_worker,
            check_every,
        );

        join!(listener, worker);
    }
    // Clean up presence session.
    let last_session = delete_session(&user_id, session_id).await;

    // If this was the last session, notify other users that we just went offline.
    if last_session {
        state.broadcast_presence_change(false).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn listener_with_kill_signal(
    db: &'static Database,
    state: &mut State,
    addr: SocketAddr,
    config: &ProtocolConfiguration,
    topic_signal_r: async_channel::Receiver<()>,
    kill_signal_r: async_channel::Receiver<()>,
    write: &Mutex<WsWriter>,
    kill_signal_s: async_channel::Sender<()>,
) {
    listener(
        db,
        state,
        addr,
        config,
        topic_signal_r,
        kill_signal_r,
        write,
    )
    .await;
    kill_signal_s.send(()).await.ok();
}

async fn listener(
    db: &'static Database,
    state: &mut State,
    addr: SocketAddr,
    config: &ProtocolConfiguration,
    topic_signal_r: async_channel::Receiver<()>,
    kill_signal_r: async_channel::Receiver<()>,
    write: &Mutex<WsWriter>,
) {
    let stoat_config = revolt_config::config().await;
    let url = stoat_config
        .database
        .redis_pubsub
        .unwrap_or(REDIS_URI.to_string());

    let redis_config = RedisConfig::from_url(&url).unwrap();
    let mut builder = fred::types::Builder::from_config(redis_config);
    builder.set_policy(ReconnectPolicy::new_exponential(8, 100, 30_000, 2));
    let subscriber = match report_internal_error!(builder.build_subscriber_client()) {
        Ok(subscriber) => subscriber,
        Err(_) => return,
    };

    if report_internal_error!(subscriber.init().await).is_err() {
        return;
    }

    // Let Fred automatically re-subscribe to tracked channels on reconnect.
    subscriber.manage_subscriptions();

    // Handle Redis connection dropping
    let (clean_up_s, clean_up_r) = async_channel::bounded(1);
    let clean_up_s = Arc::new(Mutex::new(clean_up_s));
    subscriber.on_error(move |err| {
        warn!("Redis subscriber error: {:?}", err);
        if let RedisErrorKind::Canceled = err.kind() {
            let clean_up_s = clean_up_s.clone();
            spawn(async move {
                clean_up_s.lock().await.send(()).await.ok();
            });
        }
        // Transient errors (IO, timeout) are handled by the reconnect policy.

        Ok(())
    });

    let mut message_rx = subscriber.message_rx();
    let mut session_rechecked = false;
    'out: loop {
        // Check for state changes for subscriptions.
        match state.apply_state().await {
            SubscriptionStateChange::Reset => {
                if report_internal_error!(subscriber.unsubscribe_all().await).is_err() {
                    break 'out;
                }

                let subscribed = state.subscribed.read().await;
                for id in subscribed.iter() {
                    if report_internal_error!(subscriber.subscribe(id).await).is_err() {
                        break 'out;
                    }
                }

                #[cfg(debug_assertions)]
                info!("{addr:?} has reset their subscriptions");
            }
            SubscriptionStateChange::Change { add, remove } => {
                for id in remove {
                    #[cfg(debug_assertions)]
                    info!("{addr:?} unsubscribing from {id}");

                    if report_internal_error!(subscriber.unsubscribe(id).await).is_err() {
                        break 'out;
                    }
                }

                for id in add {
                    #[cfg(debug_assertions)]
                    info!("{addr:?} subscribing to {id}");

                    if report_internal_error!(subscriber.subscribe(id).await).is_err() {
                        break 'out;
                    }
                }
            }
            SubscriptionStateChange::None => {}
        }

        // A DeleteSession between the token check and this subscribe was
        // published to nobody, so look once now that nothing can be missed
        if !session_rechecked {
            session_rechecked = true;
            if !session_still_open(db, &state.session_id, false, Timestamp::now_utc()).await {
                end_session_socket(addr, config, write).await;
                break 'out;
            }
        }

        let t1 = message_rx.recv().fuse();
        let t2 = topic_signal_r.recv().fuse();
        let t3 = kill_signal_r.recv().fuse();
        let t4 = clean_up_r.recv().fuse();

        pin_mut!(t1, t2, t3, t4);

        select! {
            _ = t4 => {
                break 'out;
            },
            _ = t3 => {
                break 'out;
            },
            _ = t2 => {},
            message = t1 => {
                // Handle incoming events.
                let message = match report_internal_error!(message) {
                    Ok(message) => message,
                    Err(_) => break 'out
                };

                let event = match *REDIS_PAYLOAD_TYPE {
                    PayloadType::Json => message
                        .value
                        .as_str()
                        .and_then(|s| report_internal_error!(serde_json::from_str::<EventV1>(s.as_ref())).ok()),
                    PayloadType::Msgpack => message
                        .value
                        .as_bytes()
                        .and_then(|b| report_internal_error!(rmp_serde::from_slice::<EventV1>(b)).ok()),
                    PayloadType::Bincode => message
                        .value
                        .as_bytes()
                        .and_then(|b| report_internal_error!(bincode::deserialize::<EventV1>(b)).ok()),
                };

                let Some(mut event) = event else {
                    let err = format!(
                        "Failed to deserialise event for {}: `{:?}`",
                        message.channel,
                        message
                            .value
                    );

                    error!("{}", err);
                    sentry::capture_message(&err, Level::Error);
                    break 'out;
                };

                if let EventV1::DeleteSession { session_id, .. } = &event {
                    if &state.session_id == session_id {
                        event = EventV1::Logout;
                    }
                } else if let EventV1::DeleteAllSessions {
                    exclude_session_id, ..
                } = &event
                {
                    if let Some(excluded) = exclude_session_id {
                        if &state.session_id != excluded {
                            event = EventV1::Logout;
                        }
                    } else {
                        event = EventV1::Logout;
                    }
                } else {
                    let should_send = state
                        .handle_incoming_event_v1(db, &mut event, &message.channel)
                        .await;
                    if !should_send {
                        continue;
                    }
                }

                let result = write.lock().await.send(config.encode(&event)).await;
                if let Err(e) = result {
                    use async_tungstenite::tungstenite::Error;
                    if !matches!(e, Error::AlreadyClosed | Error::ConnectionClosed) {
                        let err = format!("Error while sending an event to {addr:?}: {e:?}");
                        warn!("{}", err);
                        sentry::capture_message(&err, Level::Warning);
                    }

                    break 'out;
                }

                if let EventV1::Logout = event {
                    info!("User {addr:?} received log out event!");
                    break 'out;
                }
            }
        }
    }

    report_internal_error!(subscriber.quit().await).ok();
}

#[allow(clippy::too_many_arguments)]
async fn worker_with_kill_signal(
    addr: SocketAddr,
    subscribed: Arc<RwLock<HashSet<String>>>,
    active_servers: Arc<Mutex<lru_time_cache::LruCache<String, ()>>>,
    user_id: String,
    config: &ProtocolConfiguration,
    topic_signal_s: async_channel::Sender<()>,
    kill_signal_r: async_channel::Receiver<()>,
    read: WsReader,
    write: &Mutex<WsWriter>,
    kill_signal_s: async_channel::Sender<()>,
    db: &Database,
    session_id: String,
    check_every: std::time::Duration,
) {
    worker(
        addr,
        subscribed,
        active_servers,
        user_id,
        config,
        topic_signal_s,
        kill_signal_r,
        read,
        write,
        db,
        session_id,
        check_every,
    )
    .await;
    kill_signal_s.send(()).await.ok();
}

#[allow(clippy::too_many_arguments)]
async fn worker(
    addr: SocketAddr,
    subscribed: Arc<RwLock<HashSet<String>>>,
    active_servers: Arc<Mutex<lru_time_cache::LruCache<String, ()>>>,
    user_id: String,
    config: &ProtocolConfiguration,
    topic_signal_s: async_channel::Sender<()>,
    kill_signal_r: async_channel::Receiver<()>,
    mut read: WsReader,
    write: &Mutex<WsWriter>,
    db: &Database,
    session_id: String,
    check_every: std::time::Duration,
) {
    // The check runs on a timer: a client that never pings must not escape it.
    // Only pings count as use, so a silent socket lets its session expire.
    let mut next_check = tokio::time::Instant::now() + check_every;
    let mut pinged = false;
    let mut typing = crate::typing::TypingRights::default();

    loop {
        let t1 = read.try_next().fuse();
        let t2 = kill_signal_r.recv().fuse();
        let t3 = tokio::time::sleep_until(next_check).fuse();

        pin_mut!(t1, t2, t3);

        select! {
            _ = t2 => {
                return;
            },
            _ = t3 => {
                next_check = tokio::time::Instant::now() + check_every;
                let used = std::mem::take(&mut pinged);
                if !session_still_open(db, &session_id, used, Timestamp::now_utc()).await {
                    end_session_socket(addr, config, write).await;
                    return;
                }
            },
            result = t1 => {
                let msg = match result {
                    Ok(Some(msg)) => msg,
                    Ok(None) => {
                        warn!("Received a None message!");
                        sentry::capture_message("Received a None message!", Level::Warning);
                        return;
                    }
                    Err(e) => {
                        use async_tungstenite::tungstenite::Error;
                        if !matches!(e, Error::AlreadyClosed | Error::ConnectionClosed) {
                            let err = format!("Error while reading an event from {addr:?}: {e:?}");
                            warn!("{}", err);
                            sentry::capture_message(&err, Level::Warning);
                        }

                        return;
                    }
                };

                let Ok(payload) = config.decode(&msg) else {
                    continue;
                };

                match payload {
                    ClientMessage::BeginTyping { channel } => {
                        if !subscribed.read().await.contains(&channel) {
                            continue;
                        }

                        if !typing.allowed(db, &user_id, &channel).await {
                            continue;
                        }

                        EventV1::ChannelStartTyping {
                            id: channel.clone(),
                            user: user_id.clone(),
                        }
                        .p(channel.clone())
                        .await;
                    }
                    ClientMessage::EndTyping { channel } => {
                        if !subscribed.read().await.contains(&channel) {
                            continue;
                        }

                        EventV1::ChannelStopTyping {
                            id: channel.clone(),
                            user: user_id.clone(),
                        }
                        .p(channel.clone())
                        .await;
                    }
                    ClientMessage::Subscribe { server_id } => {
                        if db.fetch_member(&server_id, &user_id).await.is_ok() {
                            let mut servers = active_servers.lock().await;
                            let has_item = servers.contains_key(&server_id);
                            servers.insert(server_id, ());

                            if !has_item {
                                // Poke the listener to adjust subscriptions
                                topic_signal_s.send(()).await.ok();
                            }
                        }
                    }
                    ClientMessage::Ping { data, responded } => {
                        pinged = true;

                        if responded.is_none() {
                            write
                                .lock()
                                .await
                                .send(config.encode(&EventV1::Pong { data }))
                                .await
                                .ok();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Whether the session behind a socket is still there and not expired. With
/// `used`, pings have come in since the last look, and that is recorded,
/// though only on a live session. Bots have no session. A failing database
/// ends nothing: the next look tries again.
async fn session_still_open(
    db: &Database,
    session_id: &str,
    used: bool,
    now: Timestamp,
) -> bool {
    if session_id.is_empty() {
        return true;
    }

    let open = if used {
        Session::touch(db, session_id, now).await
    } else {
        Session::is_open(db, session_id, now).await
    };
    open.unwrap_or(true)
}

/// Tell the client its session is over and end the socket
async fn end_session_socket(
    addr: SocketAddr,
    config: &ProtocolConfiguration,
    write: &Mutex<WsWriter>,
) {
    info!("Session of {addr:?} is closed or expired, ending the socket");
    let mut write = write.lock().await;
    write.send(config.encode(&EventV1::Logout)).await.ok();
    write
        .send(async_tungstenite::tungstenite::Message::Close(None))
        .await
        .ok();
}

async fn fetch_user_slowmodes(user_id: &str) -> Option<Vec<v0::ChannelSlowmode>> {
    let mut conn = get_connection().await.ok()?.into_inner();
    let idx_key = format!("slowmode_idx:{}", user_id);

    let channel_ids: Vec<String> = conn.smembers(&idx_key).await.unwrap_or_default();
    if channel_ids.is_empty() {
        return Some(vec![]);
    }

    // Bulk fetch all TTLs in one round trip
    let mut pipe = redis_kiss::redis::pipe();
    for channel_id in &channel_ids {
        pipe.ttl(format!("slowmode:{}:{}", user_id, channel_id));
    }
    let ttls: Vec<i64> = pipe.query_async(&mut conn).await.unwrap_or_default();

    // Partition into alive/expired in one pass
    let mut slowmodes = vec![];
    let mut expired = vec![];
    for (channel_id, ttl) in channel_ids.iter().zip(ttls.iter()) {
        if *ttl > 0 {
            slowmodes.push(v0::ChannelSlowmode {
                channel_id: channel_id.clone(),
                duration: *ttl as u64,
                retry_after: *ttl as u64,
            });
        } else {
            expired.push(channel_id.as_str());
        }
    }

    // Bulk remove all expired members in one SREM call
    if !expired.is_empty() {
        conn.srem::<_, _, ()>(&idx_key, expired).await.ok();
    }

    Some(slowmodes)
}

#[cfg(test)]
mod session_check_tests {
    use revolt_database::{
        iso8601_timestamp::{Duration, Timestamp},
        Database, DatabaseInfo, Session,
    };

    use super::session_still_open;

    fn session(id: &str, last_seen: Timestamp) -> Session {
        Session {
            id: id.to_owned(),
            user_id: "user".to_owned(),
            token: format!("token-{id}"),
            name: id.to_owned(),
            last_seen,
            idle_grace_until: Some(Timestamp::UNIX_EPOCH),
            origin: None,
            subscription: None,
        }
    }

    async fn database(name: &str) -> Database {
        DatabaseInfo::Test(format!("bonfire_check_{name}"))
            .connect()
            .await
            .expect("database")
    }

    /// The test database is shared and kept
    fn unique(name: &str) -> String {
        format!("{name}-{}", ulid::Ulid::new())
    }

    #[tokio::test]
    async fn a_closed_session_is_not_open() {
        let db = database("closed").await;
        let now = Timestamp::now_utc();
        for used in [false, true] {
            assert!(!session_still_open(&db, &unique("gone"), used, now).await);
        }
    }

    #[tokio::test]
    async fn an_expired_session_is_not_open_and_a_ping_does_not_revive_it() {
        let db = database("expired").await;
        let now = Timestamp::now_utc();
        let id = unique("idle");
        let stale = now + Duration::days(-31);
        db.save_session(&session(&id, stale)).await.unwrap();

        for used in [false, true] {
            assert!(!session_still_open(&db, &id, used, now).await);
        }
        assert!(db.fetch_session(&id).await.unwrap().last_seen < now + Duration::days(-30));
    }

    #[tokio::test]
    async fn only_a_ping_counts_as_use() {
        let db = database("live").await;
        let now = Timestamp::now_utc();
        let id = unique("live");
        let start = now + Duration::days(-1);
        db.save_session(&session(&id, start)).await.unwrap();

        // a silent socket: open, and the session ages
        assert!(session_still_open(&db, &id, false, now).await);
        let silent = db.fetch_session(&id).await.unwrap().last_seen;
        assert!(silent < now + Duration::hours(-23));

        // pings since the last check
        assert!(session_still_open(&db, &id, true, now).await);
        let pinged = db.fetch_session(&id).await.unwrap().last_seen;
        assert!(pinged > now + Duration::minutes(-1));
    }

    #[tokio::test]
    async fn a_bot_has_no_session_to_check() {
        let db = database("bot").await;
        let now = Timestamp::now_utc();
        for used in [false, true] {
            assert!(session_still_open(&db, "", used, now).await);
        }
    }
}

/// bonfire itself on a real TCP socket, with Redis, against the test database
#[cfg(test)]
mod socket_tests {
    use std::time::Duration as StdDuration;

    use async_tungstenite::tungstenite::Message;
    use futures::{SinkExt, StreamExt};
    use revolt_database::{
        iso8601_timestamp::{Duration, Timestamp},
        Account, AccountAge, Database, DatabaseInfo, Session, User, AMQP,
    };
    use revolt_models::v0;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::compat::TokioAsyncReadCompatExt;

    use super::client_with_check;

    /// Never fires during a test
    const NEVER: StdDuration = StdDuration::from_secs(3600);
    /// How long a test waits for what it expects
    const PATIENCE: StdDuration = StdDuration::from_secs(10);

    async fn database() -> &'static Database {
        // as `main` does: creating a user announces it
        AMQP::new_auto().await;
        let db = DatabaseInfo::Test(format!("bonfire_socket_{}", ulid::Ulid::new()))
            .connect()
            .await
            .expect("database");
        Box::leak(Box::new(db))
    }

    /// A person who may connect: an account, a declared age, a user
    async fn person(db: &Database) -> Session {
        let age = AccountAge {
            birth_month: "1990-01".to_owned(),
            declared_at: Timestamp::now_utc(),
            source: v0::AgeSource::SelfDeclared,
            raised: false,
            changes: vec![],
        };
        let account = Account::new_with_age(
            db,
            format!("{}@zrzur.cc", ulid::Ulid::new()),
            "password_insecure".to_owned(),
            false,
            Some(age),
        )
        .await
        .unwrap();
        let name = format!("s{}", ulid::Ulid::new().to_string().to_lowercase());
        User::create(db, name[..20].to_owned(), account.id.clone(), None)
            .await
            .unwrap();
        account.create_session(db, "test".to_owned()).await.unwrap()
    }

    type Socket = async_tungstenite::WebSocketStream<tokio_util::compat::Compat<TcpStream>>;

    async fn connect(db: &'static Database, token: &str, check_every: StdDuration) -> Socket {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            client_with_check(db, stream, peer, check_every).await;
        });

        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let url = format!("ws://127.0.0.1:{port}/?format=json&version=1&token={token}");
        async_tungstenite::client_async(url, stream.compat())
            .await
            .unwrap()
            .0
    }

    /// The next event's JSON; None once the socket has ended
    async fn next_event(socket: &mut Socket) -> Option<serde_json::Value> {
        loop {
            let message = tokio::time::timeout(PATIENCE, socket.next())
                .await
                .expect("the socket stayed open and silent")?;
            match message {
                Ok(Message::Text(text)) => return serde_json::from_str(&text).ok(),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    async fn wait_for(socket: &mut Socket, kind: &str) {
        loop {
            let event = next_event(socket).await.expect("the socket ended");
            if event["type"] == kind {
                return;
            }
        }
    }

    async fn expect_logout_and_close(socket: &mut Socket) {
        wait_for(socket, "Logout").await;
        assert!(next_event(socket).await.is_none(), "the socket stayed open");
    }

    #[tokio::test]
    async fn a_session_closed_before_the_subscribe_ends_a_socket_that_never_pings() {
        let db = database().await;
        let session = person(db).await;
        let mut socket = connect(db, &session.token, NEVER).await;

        // the token was accepted. The delete goes out now, while this socket
        // has not subscribed yet, so its event reaches nobody.
        wait_for(&mut socket, "Authenticated").await;
        db.delete_session(&session.id).await.unwrap();

        expect_logout_and_close(&mut socket).await;
    }

    #[tokio::test]
    async fn the_timer_ends_a_socket_that_never_pings() {
        let db = database().await;
        let session = person(db).await;
        let mut socket = connect(db, &session.token, StdDuration::from_millis(200)).await;
        wait_for(&mut socket, "Ready").await;

        // past the check after the subscribe, then a delete nobody is told of
        tokio::time::sleep(StdDuration::from_millis(1000)).await;
        db.delete_session(&session.id).await.unwrap();

        expect_logout_and_close(&mut socket).await;
    }

    #[tokio::test]
    async fn the_timer_ends_a_socket_whose_session_expires() {
        let db = database().await;
        let session = person(db).await;
        let mut socket = connect(db, &session.token, StdDuration::from_millis(200)).await;
        wait_for(&mut socket, "Ready").await;

        let long_ago = Timestamp::now_utc() + Duration::days(-31);
        db.save_session(&Session {
            last_seen: long_ago,
            idle_grace_until: Some(long_ago),
            ..session
        })
        .await
        .unwrap();

        expect_logout_and_close(&mut socket).await;
    }

    #[tokio::test]
    async fn an_expired_token_is_refused_on_a_real_socket() {
        let db = database().await;
        let session = person(db).await;
        let long_ago = Timestamp::now_utc() + Duration::days(-31);
        let token = session.token.clone();
        db.save_session(&Session {
            last_seen: long_ago,
            idle_grace_until: Some(long_ago),
            ..session
        })
        .await
        .unwrap();

        let mut socket = connect(db, &token, NEVER).await;
        let event = next_event(&mut socket).await.expect("an error event");
        assert_eq!(event["type"], "Error");
        assert_eq!(event["data"]["type"], "InvalidSession");
    }

    #[tokio::test]
    async fn a_live_session_keeps_its_socket_and_a_ping_counts_as_use() {
        let db = database().await;
        let session = person(db).await;
        let mut socket = connect(db, &session.token, StdDuration::from_millis(200)).await;
        wait_for(&mut socket, "Ready").await;

        // aged, not expired: a ping is what must bring it forward
        let aged = Timestamp::now_utc() + Duration::days(-2);
        db.save_session(&Session {
            last_seen: aged,
            ..session.clone()
        })
        .await
        .unwrap();

        socket
            .send(Message::Text(r#"{"type":"Ping","data":1}"#.to_owned()))
            .await
            .unwrap();
        wait_for(&mut socket, "Pong").await;
        tokio::time::sleep(StdDuration::from_millis(600)).await;

        let stored = db.fetch_session(&session.id).await.unwrap();
        assert!(stored.last_seen > aged + Duration::days(1));
        assert!(session_is_open_on(&mut socket).await);
    }

    /// The socket still answers
    async fn session_is_open_on(socket: &mut Socket) -> bool {
        socket
            .send(Message::Text(r#"{"type":"Ping","data":2}"#.to_owned()))
            .await
            .unwrap();
        wait_for(socket, "Pong").await;
        true
    }
}
