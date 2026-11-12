use std::{collections::HashSet, net::SocketAddr, num::NonZeroUsize, sync::Arc};

use async_tungstenite::WebSocketStream;
use fred::{
    error::RedisErrorKind,
    interfaces::{ClientLike, EventInterface, PubsubInterface},
    types::{ReconnectPolicy, RedisConfig},
};
use futures::{
    channel::oneshot, join, pin_mut, select, stream::SplitStream, FutureExt, SinkExt, StreamExt,
    TryStreamExt,
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
    time::Instant,
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

use crate::config::{ProtocolConfiguration, WebsocketHandshakeCallback};
use crate::events::state::{State, SubscriptionStateChange};
use crate::resume::{
    self, Attach, Control, ReplayBuffer, ResumeLimits, Sequenced, SessionHandle, WsWriter,
};
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

/// Close code: a privileged intent was asked for that is not switched on
const CLOSE_DISALLOWED_INTENTS: u16 = 4014;
/// Longest a write to a socket may take; a client that reads nothing must
/// not hold its session's events
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Longest the missed events of a resume may take to send
const REPLAY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Refused resumes one socket may try before it is closed
const MAX_RESUME_TRIES: usize = 2;

type WsReader = SplitStream<WebSocketStream<Compat<TcpStream>>>;
type Writer = Arc<Mutex<WsWriter>>;

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
    client_with_limits(db, stream, addr, check_every, ResumeLimits::default()).await
}

/// What a new socket asked for first
enum Hello {
    /// A new session (`Authenticate`, or a token in the address)
    Identify,
    /// Continue a dropped session
    Resume {
        token: String,
        session_id: String,
        seq: u64,
    },
}

/// Wait for `Authenticate` or `Resume`; anything else is ignored
async fn hello(
    config: &mut ProtocolConfiguration,
    read: &mut WsReader,
    until: Instant,
) -> Option<Hello> {
    if config.get_session_token().is_some() {
        return Some(Hello::Identify);
    }

    let wait = async {
        while let Ok(Some(message)) = read.try_next().await {
            match config.decode(&message) {
                Ok(ClientMessage::Authenticate {
                    token,
                    intents,
                    resumable,
                }) => {
                    config.set_session_token(token);
                    config.set_intents(intents);
                    config.set_resumable(resumable);
                    return Some(Hello::Identify);
                }
                Ok(ClientMessage::Resume {
                    token,
                    session_id,
                    seq,
                }) => {
                    return Some(Hello::Resume {
                        token,
                        session_id,
                        seq,
                    })
                }
                _ => {}
            }
        }
        None
    };
    tokio::time::timeout_at(until, wait).await.ok().flatten()
}

/// Send one event, whatever happens to the socket
async fn send(write: &Mutex<WsWriter>, config: &ProtocolConfiguration, event: &EventV1) {
    write.lock().await.send(config.encode(event)).await.ok();
}

/// Refuse a bot's privileged intents that are not switched on: the error,
/// then close code 4014. The reason says what is switched on, as read for
/// this connection. A bot that could not be read is not refused for good:
/// the socket closes without 4014 so the bot tries again.
async fn refuse_intents(
    write: &Mutex<WsWriter>,
    config: &ProtocolConfiguration,
    refusal: revolt_database::IntentRefusal,
) {
    use async_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};
    let revolt_database::IntentRefusal::Missing {
        missing,
        switched_on,
    } = refusal
    else {
        send(
            write,
            config,
            &EventV1::Error {
                data: create_error!(InternalError),
            },
        )
        .await;
        write
            .lock()
            .await
            .send(async_tungstenite::tungstenite::Message::Close(Some(CloseFrame {
                code: CloseCode::Again,
                reason: "Could not read the bot, try again".into(),
            })))
            .await
            .ok();
        return;
    };

    let intent = missing.join(", ");
    send(
        write,
        config,
        &EventV1::Error {
            data: create_error!(MissingIntent {
                intent: intent.clone()
            }),
        },
    )
    .await;
    let switched_on = if switched_on.is_empty() {
        "none".to_owned()
    } else {
        switched_on.join(", ")
    };
    write
        .lock()
        .await
        .send(async_tungstenite::tungstenite::Message::Close(Some(CloseFrame {
            code: CloseCode::Library(CLOSE_DISALLOWED_INTENTS),
            reason: format!("Disallowed intents: {intent} (switched on: {switched_on})").into(),
        })))
        .await
        .ok();
}

async fn client_with_limits(
    db: &'static Database,
    stream: TcpStream,
    addr: SocketAddr,
    check_every: std::time::Duration,
    limits: ResumeLimits,
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
    let (write, mut read) = ws.split();
    let write: Writer = Arc::new(Mutex::new(write));

    // Authenticate, or resume a dropped session. A resume that is refused
    // is answered `InvalidSession { resumable: false }` and the client may
    // authenticate on the same socket.
    let until = Instant::now() + AUTH_TIMEOUT;
    let mut resume_tries = 0;
    loop {
        match hello(&mut config, &mut read, until).await {
            Some(Hello::Identify) => break,
            Some(Hello::Resume {
                token,
                session_id,
                seq,
            }) => match tokio::time::timeout_at(
                until,
                resume_session(db, &config, &write, &token, &session_id, seq),
            )
            .await
            // too slow: the socket ends; a session that took it detaches again
            .unwrap_or(Resumption::Ended)
            {
                Resumption::Accepted {
                    handle,
                    generation,
                    kill_worker,
                    auth_session_id,
                } => {
                    info!("User {addr:?} resumed session {session_id}");
                    db.update_session_last_seen(&auth_session_id, Timestamp::now_utc())
                        .await
                        .ok();
                    let end = worker(
                        addr,
                        handle.subscribed.clone(),
                        handle.active_servers.clone(),
                        handle.user_id.clone(),
                        &config,
                        handle.topic_signal.clone(),
                        kill_worker,
                        read,
                        &write,
                        db,
                        auth_session_id,
                        check_every,
                    )
                    .await;
                    handle.control.send(end.control(generation)).await.ok();
                    return;
                }
                Resumption::Refused => {
                    send(&write, &config, &EventV1::InvalidSession { resumable: false }).await;
                    resume_tries += 1;
                    if resume_tries >= MAX_RESUME_TRIES {
                        write
                            .lock()
                            .await
                            .send(async_tungstenite::tungstenite::Message::Close(None))
                            .await
                            .ok();
                        return;
                    }
                }
                Resumption::Ended => return,
            },
            None => {
                // no token in time
                send(
                    &write,
                    &config,
                    &EventV1::Error {
                        data: create_error!(InvalidSession),
                    },
                )
                .await;
                return;
            }
        }
    }

    // Try to authenticate the user.
    let Some(token) = config.get_session_token().clone() else {
        send(
            &write,
            &config,
            &EventV1::Error {
                data: create_error!(InvalidSession),
            },
        )
        .await;
        return;
    };

    let (user, session_id) = match User::from_token(db, &token, UserHint::Any).await {
        Ok(user) => user,
        Err(err) => {
            send(&write, &config, &EventV1::Error { data: err }).await;
            return;
        }
    };

    // Bots receive only what their intents allow; asking for a privileged
    // intent that is not on for this bot closes the connection (4014). The
    // API's rule, so past the server limit only approved intents count.
    let intents = if user.bot.is_some() {
        match revolt_database::connection_intents(db, &user, config.get_intents()).await {
            Ok(intents) => intents,
            Err(refusal) => {
                refuse_intents(&write, &config, refusal).await;
                return;
            }
        }
    } else {
        // as the API: nothing until an age is declared
        if !revolt_database::util::age::has_declared_age(db, &user).await {
            send(
                &write,
                &config,
                &EventV1::Error {
                    data: create_error!(AgeNotDeclared),
                },
            )
            .await;
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
    // only a client that asked for it gets a session it can resume
    let gateway_session_id = config
        .is_resumable()
        .then(|| ulid::Ulid::new().to_string());

    // Notify socket we have authenticated.
    if report_internal_error!(write.lock().await.send(config.encode(&EventV1::Authenticated)).await)
        .is_err()
    {
        return;
    }

    // Download required data to local cache and send Ready payload.
    let mut ready_payload = match report_internal_error!(
        state
            .generate_ready_payload(db, config.get_ready_payload_fields())
            .await
    ) {
        Ok(ready_payload) => ready_payload,
        Err(_) => return,
    };
    if let EventV1::Ready { session_id, .. } = &mut ready_payload {
        *session_id = gateway_session_id.clone();
    }

    if report_internal_error!(write.lock().await.send(config.encode(&ready_payload)).await).is_err()
    {
        return;
    }

    let slowmodes = fetch_user_slowmodes(&user_id).await.unwrap_or_default();
    if !slowmodes.is_empty() {
        let event = EventV1::UserSlowmodes { slowmodes };
        if report_internal_error!(write.lock().await.send(config.encode(&event)).await).is_err() {
            return;
        }
    }

    {
        // Setup channels and mutexes
        let subscribed = state.subscribed.clone();
        let active_servers = state.active_servers.clone();
        let (topic_signal_s, topic_signal_r) = async_channel::unbounded();
        // the reader tells the listener when its socket is gone; the
        // listener ends the reader when the session ends
        let (control_s, control_r) = async_channel::unbounded();
        let (kill_worker_s, kill_worker_r) = async_channel::bounded(1);

        let registration = gateway_session_id.as_ref().map(|id| {
            resume::register(
                id,
                SessionHandle {
                    user_id: user_id.clone(),
                    auth_session_id: session_id_for_worker.clone(),
                    intents,
                    subscribed: subscribed.clone(),
                    active_servers: active_servers.clone(),
                    topic_signal: topic_signal_s.clone(),
                    control: control_s.clone(),
                },
            )
        });

        // Create a PubSub connection to poll on.
        let listener = listener(
            db,
            &mut state,
            addr,
            Outlet {
                write: write.clone(),
                config: config.clone(),
                kill_worker: kill_worker_s,
                generation: 0,
            },
            topic_signal_r,
            control_r,
            limits,
            gateway_session_id,
            registration,
        );

        // Read from WebSocket stream.
        let worker = async {
            let end = worker(
                addr,
                subscribed,
                active_servers,
                user_id.clone(),
                &config,
                topic_signal_s,
                kill_worker_r,
                read,
                &write,
                db,
                session_id_for_worker,
                check_every,
            )
            .await;
            control_s.send(end.control(0)).await.ok();
        };

        join!(listener, worker);
    }
}

/// The result of a `Resume`
enum Resumption {
    /// The missed events and `Resumed` were sent; this socket now reads for
    /// the session
    Accepted {
        handle: SessionHandle,
        generation: u64,
        kill_worker: async_channel::Receiver<()>,
        auth_session_id: String,
    },
    /// No such session for this login, or too much was missed
    Refused,
    /// The socket was answered with an error and ends
    Ended,
}

/// Check a resume and hand this socket to the dropped session. The token is
/// checked as on `Authenticate` (a revoked session is refused the same way);
/// a session of another user or login is answered as if it did not exist.
async fn resume_session(
    db: &'static Database,
    config: &ProtocolConfiguration,
    write: &Writer,
    token: &str,
    session_id: &str,
    seq: u64,
) -> Resumption {
    let (user, auth_session_id) = match User::from_token(db, token, UserHint::Any).await {
        Ok(found) => found,
        Err(err) => {
            send(write, config, &EventV1::Error { data: err }).await;
            return Resumption::Ended;
        }
    };

    let Some(handle) = resume::find(session_id) else {
        return Resumption::Refused;
    };
    if handle.user_id != user.id || handle.auth_session_id != auth_session_id {
        return Resumption::Refused;
    }

    // the intents are checked again: one switched off since closes as on
    // Authenticate, and a session whose intents would now differ starts anew
    if user.bot.is_some() {
        match revolt_database::connection_intents(db, &user, Some(handle.intents)).await {
            Err(refusal) => {
                refuse_intents(write, config, refusal).await;
                return Resumption::Ended;
            }
            Ok(intents) if intents != handle.intents => return Resumption::Refused,
            Ok(_) => {}
        }
    }

    let (kill_worker_s, kill_worker_r) = async_channel::bounded(1);
    let (reply_s, reply_r) = oneshot::channel();
    let attach = Attach {
        writer: write.clone(),
        config: config.clone(),
        seq,
        kill_worker: kill_worker_s,
        reply: reply_s,
    };
    if handle.control.send(Control::Attach(attach)).await.is_err() {
        return Resumption::Refused;
    }

    match reply_r.await {
        Ok(Some(generation)) => Resumption::Accepted {
            handle,
            generation,
            kill_worker: kill_worker_r,
            auth_session_id,
        },
        _ => Resumption::Refused,
    }
}

/// The socket a session currently sends to
struct Outlet {
    write: Writer,
    config: ProtocolConfiguration,
    /// Ends this socket's reader
    kill_worker: async_channel::Sender<()>,
    generation: u64,
}

/// Where a session's events go: its socket, and (if resumable) the buffer
/// kept for a resume while it has none
struct Delivery {
    outlet: Option<Outlet>,
    buffer: ReplayBuffer,
    resumable: bool,
    limits: ResumeLimits,
    user_id: String,
    /// Id for resumes; reachable while this is held
    gateway_session_id: Option<String>,
    _registration: Option<resume::Registration>,
    /// Presence session while the user counts as online (not while dropped)
    presence: Option<u32>,
    /// When the last socket dropped, while there is none
    detached_at: Option<Instant>,
    /// Last `seq` given out when the socket dropped: the most a resume can
    /// start from
    seq_at_detach: u64,
    next_generation: u64,
}

/// Send one message, but not for ever
async fn send_in_time(
    write: &Mutex<WsWriter>,
    message: async_tungstenite::tungstenite::Message,
    addr: SocketAddr,
) -> bool {
    match tokio::time::timeout(WRITE_TIMEOUT, async {
        write.lock().await.send(message).await
    })
    .await
    {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            use async_tungstenite::tungstenite::Error;
            if !matches!(e, Error::AlreadyClosed | Error::ConnectionClosed) {
                let err = format!("Error while sending an event to {addr:?}: {e:?}");
                warn!("{}", err);
                sentry::capture_message(&err, Level::Warning);
            }
            false
        }
        Err(_) => {
            info!("{addr:?} took too long to read its events");
            false
        }
    }
}

impl Delivery {
    /// Online: a presence session, and the first one tells the others
    async fn come_online(&mut self, state: &State) {
        if self.presence.is_none() {
            let (first, presence) = create_session(&self.user_id, 0).await;
            self.presence = Some(presence);
            if first {
                state.broadcast_presence_change(true).await;
            }
        }
    }

    /// Offline: the presence session goes, and the last one tells the others
    async fn go_offline(&mut self, state: &State) {
        if let Some(presence) = self.presence.take() {
            if delete_session(&self.user_id, presence).await {
                state.broadcast_presence_change(false).await;
            }
        }
    }

    /// The socket is gone: keep the session for a resume, offline (a bot
    /// gets no interactions meanwhile), within the caps
    async fn detach(&mut self, state: &State) {
        if let Some(outlet) = self.outlet.take() {
            let _ = outlet.kill_worker.try_send(());
        }
        if self.detached_at.is_some() {
            return;
        }
        let now = Instant::now();
        self.detached_at = Some(now);
        self.seq_at_detach = self.buffer.last_seq();
        self.go_offline(state).await;
        if let Some(id) = &self.gateway_session_id {
            for control in resume::detached(id, &self.limits, now) {
                let _ = control.try_send(Control::SessionOver);
            }
        }
    }

    /// Whether nothing could resume this dropped session any more: more was
    /// missed than is kept
    fn beyond_resume(&self) -> bool {
        self.detached_at.is_some() && self.buffer.forgotten() > self.seq_at_detach
    }

    /// Send an event with its sequence number. False when the socket failed
    /// or was too slow.
    async fn send(&mut self, event: &EventV1, seq: u64, addr: SocketAddr) -> bool {
        let Some(outlet) = &self.outlet else {
            return true;
        };
        send_in_time(&outlet.write, outlet.config.encode(&Sequenced { event, seq }), addr).await
    }

    /// A resume: send the kept events after its `seq` that the session may
    /// still see, then `Resumed`, and use its socket from now on
    async fn attach(&mut self, db: &Database, state: &State, attach: Attach, addr: SocketAddr) {
        let Attach {
            writer,
            config,
            seq,
            kill_worker,
            reply,
        } = attach;
        if !self.resumable || !self.buffer.covers(seq, Instant::now()) {
            let _ = reply.send(None);
            return;
        }
        // the client has everything up to its seq
        self.buffer.acknowledge(seq);

        // a socket the client gave up on, still open, is replaced; closing
        // it must not wait on that client
        if let Some(old) = self.outlet.take() {
            let _ = old.kill_worker.try_send(());
            tokio::spawn(async move {
                let _ = tokio::time::timeout(WRITE_TIMEOUT, async {
                    old.write
                        .lock()
                        .await
                        .send(async_tungstenite::tungstenite::Message::Close(None))
                        .await
                })
                .await;
            });
        }

        let generation = self.next_generation;
        self.next_generation += 1;
        let buffer = &self.buffer;
        let replay = tokio::time::timeout(REPLAY_TIMEOUT, async {
            let mut write = writer.lock().await;
            for (seq, event) in buffer.kept() {
                let Some(event) = state.may_replay(db, event).await else {
                    continue;
                };
                write
                    .send(config.encode(&Sequenced {
                        event: event.as_ref(),
                        seq,
                    }))
                    .await?;
            }
            write.send(config.encode(&EventV1::Resumed)).await
        })
        .await;
        let mut delivered = matches!(replay, Ok(Ok(())));
        if !delivered {
            info!("{addr:?}: the missed events could not be sent");
        }

        self.outlet = Some(Outlet {
            write: writer,
            config,
            kill_worker,
            generation,
        });
        // the resuming socket may have given up waiting
        if reply.send(Some(generation)).is_err() {
            delivered = false;
        }
        if delivered {
            self.detached_at = None;
            if let Some(id) = &self.gateway_session_id {
                resume::attached(id);
            }
            self.come_online(state).await;
        } else {
            self.detach(state).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn listener(
    db: &'static Database,
    state: &mut State,
    addr: SocketAddr,
    outlet: Outlet,
    topic_signal_r: async_channel::Receiver<()>,
    control_r: async_channel::Receiver<Control>,
    limits: ResumeLimits,
    gateway_session_id: Option<String>,
    registration: Option<resume::Registration>,
) {
    let resumable = gateway_session_id.is_some();
    let mut delivery = Delivery {
        outlet: Some(outlet),
        buffer: ReplayBuffer::new(limits, resumable),
        resumable,
        limits,
        user_id: state.cache.user_id.clone(),
        gateway_session_id,
        _registration: registration,
        presence: None,
        detached_at: None,
        seq_at_detach: 0,
        next_generation: 1,
    };

    // Create presence session; the first one tells the others we are online
    delivery.come_online(state).await;

    listen(db, state, addr, topic_signal_r, control_r, &mut delivery).await;

    // the session is over: no resume, and its reader stops
    drop(delivery._registration.take());
    if let Some(outlet) = &delivery.outlet {
        outlet.kill_worker.send(()).await.ok();
    }
    // the last session going tells the others we are offline
    delivery.go_offline(state).await;
}

async fn listen(
    db: &'static Database,
    state: &mut State,
    addr: SocketAddr,
    topic_signal_r: async_channel::Receiver<()>,
    control_r: async_channel::Receiver<Control>,
    delivery: &mut Delivery,
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
                if let Some(outlet) = &delivery.outlet {
                    end_session_socket(addr, &outlet.config, &outlet.write).await;
                }
                break 'out;
            }
        }

        // a dropped session waits for a resume this long
        let resume_until = delivery
            .detached_at
            .map(|at| at + delivery.limits.window);

        let t1 = message_rx.recv().fuse();
        let t2 = topic_signal_r.recv().fuse();
        let t3 = control_r.recv().fuse();
        let t4 = clean_up_r.recv().fuse();
        let t5 = async move {
            match resume_until {
                Some(until) => tokio::time::sleep_until(until).await,
                None => futures::future::pending::<()>().await,
            }
        }
        .fuse();

        pin_mut!(t1, t2, t3, t4, t5);

        select! {
            _ = t4 => {
                break 'out;
            },
            _ = t5 => {
                info!("{addr:?}: nobody resumed the session in time");
                break 'out;
            },
            _ = t2 => {},
            control = t3 => match control {
                Ok(Control::WorkerEnded { generation }) => {
                    let current = delivery
                        .outlet
                        .as_ref()
                        .is_some_and(|outlet| outlet.generation == generation);
                    if current {
                        if !delivery.resumable {
                            break 'out;
                        }
                        delivery.detach(state).await;
                    }
                }
                Ok(Control::Attach(attach)) => {
                    delivery.attach(db, state, attach, addr).await;
                }
                Ok(Control::SessionOver) | Err(_) => break 'out,
            },
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

                let seq = delivery.buffer.record(&event, Instant::now());
                if !delivery.send(&event, seq, addr).await {
                    if !delivery.resumable {
                        break 'out;
                    }
                    delivery.detach(state).await;
                }
                if delivery.beyond_resume() {
                    info!("{addr:?}: missed more than is kept, the session ends");
                    break 'out;
                }

                // a logged out session is over, also one waiting for a resume
                if let EventV1::Logout = event {
                    info!("User {addr:?} received log out event!");
                    break 'out;
                }
            }
        }
    }

    report_internal_error!(subscriber.quit().await).ok();
}

/// Why a socket's reader stopped
enum WorkerEnd {
    /// The socket is gone (or was replaced)
    SocketGone,
    /// The login behind it is closed or expired: the session ends
    SessionOver,
}

impl WorkerEnd {
    fn control(self, generation: u64) -> Control {
        match self {
            WorkerEnd::SocketGone => Control::WorkerEnded { generation },
            WorkerEnd::SessionOver => Control::SessionOver,
        }
    }
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
) -> WorkerEnd {
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
                return WorkerEnd::SocketGone;
            },
            _ = t3 => {
                next_check = tokio::time::Instant::now() + check_every;
                let used = std::mem::take(&mut pinged);
                if !session_still_open(db, &session_id, used, Timestamp::now_utc()).await {
                    end_session_socket(addr, config, write).await;
                    return WorkerEnd::SessionOver;
                }
            },
            result = t1 => {
                let msg = match result {
                    Ok(Some(msg)) => msg,
                    Ok(None) => {
                        warn!("Received a None message!");
                        sentry::capture_message("Received a None message!", Level::Warning);
                        return WorkerEnd::SocketGone;
                    }
                    Err(e) => {
                        use async_tungstenite::tungstenite::Error;
                        if !matches!(e, Error::AlreadyClosed | Error::ConnectionClosed) {
                            let err = format!("Error while reading an event from {addr:?}: {e:?}");
                            warn!("{}", err);
                            sentry::capture_message(&err, Level::Warning);
                        }

                        return WorkerEnd::SocketGone;
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
            token_hash: revolt_database::hash_secret(&format!("token-{id}")),
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

/// Resuming a dropped session, on real sockets with Redis
#[cfg(test)]
mod resume_tests {
    use std::time::Duration as StdDuration;

    use async_tungstenite::tungstenite::Message;
    use futures::{SinkExt, StreamExt};
    use revolt_database::{
        events::client::EventV1, iso8601_timestamp::Timestamp, Account, AccountAge, Database,
        DatabaseInfo, Session, User, AMQP,
    };
    use revolt_models::v0;
    use serde_json::{json, Value};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::compat::TokioAsyncReadCompatExt;

    use super::client_with_limits;
    use crate::resume::ResumeLimits;

    const NEVER: StdDuration = StdDuration::from_secs(3600);
    const PATIENCE: StdDuration = StdDuration::from_secs(10);

    type Socket = async_tungstenite::WebSocketStream<tokio_util::compat::Compat<TcpStream>>;

    async fn database() -> &'static Database {
        AMQP::new_auto().await;
        let db = DatabaseInfo::Test(format!("bonfire_resume_{}", ulid::Ulid::new()))
            .connect()
            .await
            .expect("database");
        Box::leak(Box::new(db))
    }

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
        let name = format!("r{}", ulid::Ulid::new().to_string().to_lowercase());
        User::create(db, name[..20].to_owned(), account.id.clone(), None)
            .await
            .unwrap();
        account.create_session(db, "test".to_owned()).await.unwrap()
    }

    /// A socket to a fresh bonfire task; `query` is added to the address
    async fn connect(db: &'static Database, query: &str, limits: ResumeLimits) -> Socket {
        connect_checking(db, query, limits, NEVER).await
    }

    async fn connect_checking(
        db: &'static Database,
        query: &str,
        limits: ResumeLimits,
        check_every: StdDuration,
    ) -> Socket {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            client_with_limits(db, stream, peer, check_every, limits).await;
        });

        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let url = format!("ws://127.0.0.1:{port}/?format=json&version=1{query}");
        async_tungstenite::client_async(url, stream.compat())
            .await
            .unwrap()
            .0
    }

    async fn next_event(socket: &mut Socket) -> Option<Value> {
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

    async fn wait_for(socket: &mut Socket, kind: &str) -> Value {
        loop {
            let event = next_event(socket).await.expect("the socket ended");
            if event["type"] == kind {
                return event;
            }
        }
    }

    async fn send(socket: &mut Socket, message: Value) {
        socket.send(Message::Text(message.to_string())).await.unwrap();
    }

    /// An event only this user gets: an acknowledgement on their private topic
    async fn ack(user: &str, marker: &str) {
        EventV1::ChannelAck {
            id: "channel".to_owned(),
            user: user.to_owned(),
            message_id: marker.to_owned(),
        }
        .p(user.to_owned())
        .await;
    }

    /// Wait until the session follows its topics: acknowledge until one
    /// arrives, and give back the `seq` of the last event seen
    async fn until_following(socket: &mut Socket, user: &str) -> u64 {
        for n in 0..50 {
            ack(user, &format!("probe{n}")).await;
            if let Ok(Some(Ok(Message::Text(text)))) =
                tokio::time::timeout(StdDuration::from_millis(200), socket.next()).await
            {
                let event: Value = serde_json::from_str(&text).unwrap();
                if event["type"] == "ChannelAck" {
                    // later probes may still come: take them all
                    let mut seq = event["seq"].as_u64().expect("seq on the event");
                    while let Ok(Some(Ok(Message::Text(text)))) =
                        tokio::time::timeout(StdDuration::from_millis(300), socket.next()).await
                    {
                        let event: Value = serde_json::from_str(&text).unwrap();
                        if let Some(next) = event["seq"].as_u64() {
                            assert!(next > seq, "seq goes up");
                            seq = next;
                        }
                    }
                    return seq;
                }
            }
        }
        panic!("the session never followed its topics");
    }

    /// A resumable session: the socket, its id, and the last `seq` seen
    async fn resumable_session(
        db: &'static Database,
        session: &Session,
        limits: ResumeLimits,
    ) -> (Socket, String, u64) {
        let mut socket = connect(db, "", limits).await;
        send(
            &mut socket,
            json!({ "type": "Authenticate", "token": session.token, "resumable": true }),
        )
        .await;
        let ready = wait_for(&mut socket, "Ready").await;
        let id = ready["session_id"]
            .as_str()
            .expect("Ready carries the session id")
            .to_owned();
        let seq = until_following(&mut socket, &session.user_id).await;
        (socket, id, seq)
    }

    async fn resume(
        db: &'static Database,
        token: &str,
        id: &str,
        seq: u64,
        limits: ResumeLimits,
    ) -> Socket {
        let mut socket = connect(db, "", limits).await;
        send(
            &mut socket,
            json!({ "type": "Resume", "token": token, "session_id": id, "seq": seq }),
        )
        .await;
        socket
    }

    /// Close the socket and give the session time to notice
    async fn drop_socket(mut socket: Socket) {
        socket.close(None).await.ok();
        tokio::time::sleep(StdDuration::from_millis(300)).await;
    }

    #[tokio::test]
    async fn missed_events_come_again_in_order_then_resumed() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits::default();
        let (socket, id, seen) = resumable_session(db, &session, limits).await;
        drop_socket(socket).await;

        for marker in ["one", "two", "three"] {
            ack(&session.user_id, marker).await;
        }
        tokio::time::sleep(StdDuration::from_millis(300)).await;

        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        let mut replayed = vec![];
        loop {
            let event = next_event(&mut socket).await.expect("the socket ended");
            if event["type"] == "Resumed" {
                break;
            }
            let seq = event["seq"].as_u64().unwrap();
            assert!(seq > seen, "only the missed events: {event}");
            // the user's own presence change (offline while dropped) is
            // among them; the acknowledgements come in order
            if event["type"] == "ChannelAck" {
                replayed.push((seq, event["message_id"].as_str().unwrap().to_owned()));
            }
        }
        let markers: Vec<&str> = replayed.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(markers, vec!["one", "two", "three"]);
        assert!(replayed.windows(2).all(|pair| pair[0].0 < pair[1].0), "in seq order");
        let last = replayed.last().unwrap().0;

        // and the session goes on, on the new socket
        ack(&session.user_id, "four").await;
        let event = wait_for(&mut socket, "ChannelAck").await;
        assert_eq!(event["message_id"], "four");
        assert!(event["seq"].as_u64().unwrap() > last);
    }

    #[tokio::test]
    async fn after_the_window_the_session_is_gone_and_the_socket_can_authenticate() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits {
            window: StdDuration::from_millis(400),
            ..Default::default()
        };
        let (socket, id, seen) = resumable_session(db, &session, limits).await;
        drop_socket(socket).await;
        tokio::time::sleep(StdDuration::from_millis(1000)).await;

        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        let event = next_event(&mut socket).await.expect("an answer");
        assert_eq!(event, json!({ "type": "InvalidSession", "resumable": false }));

        // the same socket authenticates anew
        send(&mut socket, json!({ "type": "Authenticate", "token": session.token })).await;
        let ready = wait_for(&mut socket, "Ready").await;
        assert!(ready.get("session_id").is_none(), "not asked to be resumable");
    }

    #[tokio::test]
    async fn too_many_missed_events_cannot_be_resumed() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits {
            max_events: 2,
            ..Default::default()
        };
        let (socket, id, seen) = resumable_session(db, &session, limits).await;
        drop_socket(socket).await;
        for marker in ["one", "two", "three"] {
            ack(&session.user_id, marker).await;
        }
        tokio::time::sleep(StdDuration::from_millis(300)).await;

        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        let event = next_event(&mut socket).await.expect("an answer");
        assert_eq!(event, json!({ "type": "InvalidSession", "resumable": false }));
    }

    #[tokio::test]
    async fn another_users_token_cannot_resume_and_learns_nothing() {
        let db = database().await;
        let session = person(db).await;
        let stranger = person(db).await;
        let limits = ResumeLimits::default();
        let (socket, id, seen) = resumable_session(db, &session, limits).await;
        drop_socket(socket).await;
        ack(&session.user_id, "private").await;
        tokio::time::sleep(StdDuration::from_millis(300)).await;

        let mut socket = resume(db, &stranger.token, &id, seen, limits).await;
        let event = next_event(&mut socket).await.expect("an answer");
        assert_eq!(event, json!({ "type": "InvalidSession", "resumable": false }));

        // the same answer as for a session that never existed
        let mut socket = resume(db, &session.token, "01NOSUCHSESSION0000000000", seen, limits).await;
        let event = next_event(&mut socket).await.expect("an answer");
        assert_eq!(event, json!({ "type": "InvalidSession", "resumable": false }));

        // and the owner still can
        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        let event = wait_for(&mut socket, "ChannelAck").await;
        assert_eq!(event["message_id"], "private");
        wait_for(&mut socket, "Resumed").await;
    }

    #[tokio::test]
    async fn a_revoked_session_cannot_be_resumed() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits::default();
        let (socket, id, seen) = resumable_session(db, &session, limits).await;
        drop_socket(socket).await;
        db.delete_session(&session.id).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(300)).await;

        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        let event = next_event(&mut socket).await.expect("an answer");
        assert_eq!(event["type"], "Error");
        assert_eq!(event["data"]["type"], "InvalidSession");
        assert!(next_event(&mut socket).await.is_none(), "the socket ends");
    }

    #[tokio::test]
    async fn a_client_that_never_resumes_works_as_before() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits::default();
        let mut socket = connect(db, &format!("&token={}", session.token), limits).await;
        let ready = wait_for(&mut socket, "Ready").await;
        assert!(ready.get("session_id").is_none());
        // its events are counted too
        let seq = until_following(&mut socket, &session.user_id).await;
        assert!(seq >= 1);

        // and nothing is kept for it
        drop_socket(socket).await;
        let mut socket = resume(db, &session.token, "anything", seq, limits).await;
        let event = next_event(&mut socket).await.expect("an answer");
        assert_eq!(event, json!({ "type": "InvalidSession", "resumable": false }));
    }

    /// Gap 17: switching Message Content on counts from the next connection,
    /// and a refusal says what the gateway read as switched on
    #[tokio::test]
    async fn a_switched_on_intent_counts_from_the_next_connection() {
        use revolt_database::{Bot, PartialBot};
        let db = database().await;
        let session = person(db).await;
        let owner = db.fetch_user(&session.user_id).await.unwrap();
        let (mut bot, _) = Bot::create(db, format!("b{}", &ulid::Ulid::new().to_string()[..12]), &owner, None)
            .await
            .unwrap();
        let wanted = v0::GatewayIntents::DEFAULT | v0::GatewayIntents::MESSAGE_CONTENT;
        let limits = ResumeLimits::default();
        let query = format!("&token={}&intents={wanted}", bot.token);

        let mut socket = connect(db, &query, limits).await;
        let event = next_event(&mut socket).await.expect("an error first");
        assert_eq!(event["data"]["type"], "MissingIntent");
        let close = loop {
            match tokio::time::timeout(PATIENCE, socket.next()).await.expect("a close") {
                Some(Ok(Message::Close(frame))) => break frame.expect("a close frame"),
                Some(Ok(_)) => {}
                other => panic!("no close frame: {other:?}"),
            }
        };
        assert_eq!(u16::from(close.code), 4014);
        assert_eq!(&*close.reason, "Disallowed intents: MESSAGE_CONTENT (switched on: none)");

        // the owner switches it on (as the portal does)
        let token = bot.token.clone();
        bot.update(
            db,
            PartialBot {
                privileged_intents: Some(v0::GatewayIntents::MESSAGE_CONTENT as i64),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

        let mut socket = connect(db, &format!("&token={token}&intents={wanted}"), limits).await;
        wait_for(&mut socket, "Ready").await;
    }

    /// The session is no longer reachable for a resume, within a short wait
    async fn ends_soon(id: &str) -> bool {
        for _ in 0..30 {
            if crate::resume::find(id).is_none() {
                return true;
            }
            tokio::time::sleep(StdDuration::from_millis(100)).await;
        }
        false
    }

    async fn invalid(socket: &mut Socket) {
        let event = next_event(socket).await.expect("an answer");
        assert_eq!(event, json!({ "type": "InvalidSession", "resumable": false }));
    }

    /// Review B1: one user can't keep more dropped sessions than the cap;
    /// a new drop ends the oldest
    #[tokio::test]
    async fn a_user_keeps_only_a_few_dropped_sessions() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits {
            detached_per_user: 1,
            ..Default::default()
        };
        let (first, first_id, first_seen) = resumable_session(db, &session, limits).await;
        let (second, second_id, second_seen) = resumable_session(db, &session, limits).await;
        drop_socket(first).await;
        assert!(crate::resume::find(&first_id).is_some(), "within the cap");
        drop_socket(second).await;
        assert!(ends_soon(&first_id).await, "the oldest dropped session ends");

        let mut socket = resume(db, &session.token, &first_id, first_seen, limits).await;
        invalid(&mut socket).await;
        let mut socket = resume(db, &session.token, &second_id, second_seen, limits).await;
        wait_for(&mut socket, "Resumed").await;
    }

    #[tokio::test]
    async fn the_process_keeps_only_so_many_dropped_sessions() {
        let db = database().await;
        let (one, two) = (person(db).await, person(db).await);
        let limits = ResumeLimits {
            detached_total: 1,
            ..Default::default()
        };
        let (first, first_id, _) = resumable_session(db, &one, limits).await;
        let (second, second_id, second_seen) = resumable_session(db, &two, limits).await;
        drop_socket(first).await;
        drop_socket(second).await;
        assert!(ends_soon(&first_id).await, "the oldest dropped session ends");
        let mut socket = resume(db, &two.token, &second_id, second_seen, limits).await;
        wait_for(&mut socket, "Resumed").await;
    }

    /// Review B1: once more was missed than is kept, nothing can resume the
    /// session, so it ends at once instead of waiting out the window
    #[tokio::test]
    async fn a_dropped_session_that_missed_too_much_ends_at_once() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits {
            max_events: 2,
            ..Default::default()
        };
        let (socket, id, _) = resumable_session(db, &session, limits).await;
        drop_socket(socket).await;
        assert!(crate::resume::find(&id).is_some());
        for marker in ["one", "two", "three"] {
            ack(&session.user_id, marker).await;
        }
        assert!(ends_soon(&id).await);
    }

    /// Review S5: while dropped the user is not online (a bot gets
    /// ApplicationOffline); a resume brings them back
    #[tokio::test]
    async fn a_dropped_session_is_offline_until_resumed() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits::default();
        let (socket, id, seen) = resumable_session(db, &session, limits).await;
        assert!(revolt_presence::is_online(&session.user_id).await);
        drop_socket(socket).await;
        assert!(!revolt_presence::is_online(&session.user_id).await, "offline while dropped");

        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        wait_for(&mut socket, "Resumed").await;
        assert!(revolt_presence::is_online(&session.user_id).await, "online again");
    }

    /// Review S2: a session the timer finds closed ends; it does not wait for
    /// a resume
    #[tokio::test]
    async fn a_session_the_timer_finds_closed_ends_at_once() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits::default();
        let mut socket = connect_checking(db, "", limits, StdDuration::from_millis(200)).await;
        send(
            &mut socket,
            json!({ "type": "Authenticate", "token": session.token, "resumable": true }),
        )
        .await;
        let ready = wait_for(&mut socket, "Ready").await;
        let id = ready["session_id"].as_str().unwrap().to_owned();
        tokio::time::sleep(StdDuration::from_millis(1000)).await;
        db.delete_session(&session.id).await.unwrap();

        wait_for(&mut socket, "Logout").await;
        assert!(ends_soon(&id).await, "the session is over, not waiting for a resume");
    }

    /// Review S9: a resume while the old socket is still open takes over
    #[tokio::test]
    async fn a_resume_while_the_old_socket_is_open_takes_over() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits::default();
        let (mut old, id, seen) = resumable_session(db, &session, limits).await;

        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        wait_for(&mut socket, "Resumed").await;
        assert!(next_event(&mut old).await.is_none(), "the old socket is closed");

        ack(&session.user_id, "after").await;
        let event = wait_for(&mut socket, "ChannelAck").await;
        assert_eq!(event["message_id"], "after");
    }

    /// Review S9: a logout while dropped ends the session
    #[tokio::test]
    async fn a_logout_while_dropped_ends_the_session() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits::default();
        let (socket, id, seen) = resumable_session(db, &session, limits).await;
        drop_socket(socket).await;
        EventV1::Logout.p(session.user_id.clone()).await;
        assert!(ends_soon(&id).await);

        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        invalid(&mut socket).await;
    }

    /// Review S9: on a real socket, a channel lost while dropped takes its
    /// missed messages with it; its removal still comes
    #[tokio::test]
    async fn a_channel_lost_while_dropped_is_not_replayed() {
        use revolt_database::{Member, Server};
        let db = database().await;
        let owner_session = person(db).await;
        let owner = db.fetch_user(&owner_session.user_id).await.unwrap();
        let session = person(db).await;
        let member = db.fetch_user(&session.user_id).await.unwrap();
        let (server, channels) = Server::create(
            db,
            v0::DataCreateServer {
                name: "Resume".to_owned(),
                ..Default::default()
            },
            &owner,
            true,
        )
        .await
        .unwrap();
        Member::create(db, &server, &member, None).await.unwrap();
        let channel = channels[0].id().to_owned();

        let limits = ResumeLimits::default();
        let (socket, id, seen) = resumable_session(db, &session, limits).await;
        drop_socket(socket).await;

        let message: v0::Message = serde_json::from_value(json!({
            "_id": ulid::Ulid::new().to_string(), "channel": channel,
            "author": owner.id, "content": "only for those who can see it"
        }))
        .unwrap();
        EventV1::Message(message).p(channel.clone()).await;
        tokio::time::sleep(StdDuration::from_millis(200)).await;
        let view = revolt_permissions::ChannelPermission::ViewChannel as i64;
        EventV1::ChannelUpdate {
            id: channel.clone(),
            data: serde_json::from_value(json!({ "default_permissions": { "a": 0, "d": view } }))
                .unwrap(),
            clear: vec![],
        }
        .p(channel.clone())
        .await;
        tokio::time::sleep(StdDuration::from_millis(300)).await;

        let mut socket = resume(db, &session.token, &id, seen, limits).await;
        let mut kinds = vec![];
        loop {
            let event = next_event(&mut socket).await.expect("the socket ended");
            if event["type"] == "Resumed" {
                break;
            }
            kinds.push(event["type"].as_str().unwrap().to_owned());
        }
        assert!(!kinds.contains(&"Message".to_owned()), "{kinds:?}");
        assert!(kinds.contains(&"ChannelDelete".to_owned()), "{kinds:?}");
    }

    /// Review nit: a socket gets two tries at a resume, then it is closed
    #[tokio::test]
    async fn a_socket_gets_two_resume_tries() {
        let db = database().await;
        let session = person(db).await;
        let limits = ResumeLimits::default();
        let mut socket = connect(db, "", limits).await;
        for n in 0..2 {
            send(
                &mut socket,
                json!({ "type": "Resume", "token": session.token, "session_id": format!("none{n}"), "seq": 0 }),
            )
            .await;
            invalid(&mut socket).await;
        }
        assert!(next_event(&mut socket).await.is_none(), "closed after the second");
    }
}
