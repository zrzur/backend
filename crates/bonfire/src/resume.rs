//! Resumable gateway sessions.
//!
//! Every event a connection gets carries `seq`, counted per session from 1.
//! A client that asked for a resumable session (`Authenticate { resumable:
//! true }`) keeps it for [ResumeLimits::window] after its socket drops: the
//! session's listener keeps following its topics with the same state, and
//! keeps the last [ResumeLimits::max_events] events it would have sent. A
//! `Resume { token, session_id, seq }` on a new socket hands that socket to
//! the session, which sends the events after `seq` again (only those the
//! session may still see) and then `Resumed`.
//!
//! The buffer lives in this process, next to the session's state, and not in
//! Redis: events are filtered per connection (permissions, intents, age
//! rules) by a state that only exists here, so a buffer elsewhere would have
//! to hold unfiltered events and filter them again without that state.
//! A resume that reaches another bonfire process (or one after a restart)
//! finds no session and is answered `InvalidSession { resumable: false }`;
//! the client then authenticates again, as it would without resume.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use async_tungstenite::WebSocketStream;
use futures::{channel::oneshot, stream::SplitSink};
use once_cell::sync::Lazy;
use revolt_database::events::client::EventV1;
use serde::Serialize;
use tokio::{
    net::TcpStream,
    sync::{Mutex, RwLock},
    time::Instant,
};
use tokio_util::compat::Compat;

use crate::config::ProtocolConfiguration;

pub type WsWriter =
    SplitSink<WebSocketStream<Compat<TcpStream>>, async_tungstenite::tungstenite::Message>;

/// How long and how much a dropped session is kept
#[derive(Clone, Copy, Debug)]
pub struct ResumeLimits {
    /// A dropped session ends this long after the drop; buffered events
    /// older than this are forgotten
    pub window: Duration,
    /// Events kept for a resume
    pub max_events: usize,
    /// Bytes (as JSON) of the events kept for a resume
    pub max_bytes: usize,
    /// Dropped sessions one user may have waiting; a new one ends the oldest
    pub detached_per_user: usize,
    /// Dropped sessions this process keeps waiting; a new one ends the oldest
    pub detached_total: usize,
}

impl Default for ResumeLimits {
    fn default() -> Self {
        ResumeLimits {
            window: Duration::from_secs(120),
            max_events: 1000,
            max_bytes: 4 * 1024 * 1024,
            detached_per_user: 3,
            detached_total: 10_000,
        }
    }
}

/// An event as it goes out: the event's own fields and its `seq`
#[derive(Serialize)]
pub struct Sequenced<'a> {
    #[serde(flatten)]
    pub event: &'a EventV1,
    pub seq: u64,
}

struct Entry {
    seq: u64,
    at: Instant,
    /// Size as JSON, for the byte budget
    bytes: usize,
    event: EventV1,
}

/// Whether an event is kept for a resume. Typing goes stale in seconds, so
/// it gets a `seq` but is not sent again.
fn kept_for_resume(event: &EventV1) -> bool {
    !matches!(
        event,
        EventV1::ChannelStartTyping { .. } | EventV1::ChannelStopTyping { .. }
    )
}

/// Sequence numbers of a session, and the events kept for a resume
pub struct ReplayBuffer {
    limits: ResumeLimits,
    /// Whether events are kept at all (only resumable sessions keep them)
    keep: bool,
    /// Last sequence number given out
    seq: u64,
    /// Highest sequence number that is no longer kept
    forgotten: u64,
    /// Bytes of the kept events
    bytes: usize,
    entries: VecDeque<Entry>,
}

impl ReplayBuffer {
    pub fn new(limits: ResumeLimits, keep: bool) -> Self {
        ReplayBuffer {
            limits,
            keep,
            seq: 0,
            forgotten: 0,
            bytes: 0,
            entries: VecDeque::new(),
        }
    }

    /// Last sequence number given out (0 before the first event)
    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Highest sequence number no longer kept: a resume needs a `seq` at
    /// least this high
    pub fn forgotten(&self) -> u64 {
        self.forgotten
    }

    /// Give the next sequence number to an event, and keep it if this
    /// session keeps events (within the count, byte and time budgets)
    pub fn record(&mut self, event: &EventV1, now: Instant) -> u64 {
        self.seq += 1;
        if !self.keep {
            self.forgotten = self.seq;
            return self.seq;
        }
        if kept_for_resume(event) {
            // what it costs as it goes out; an event that can't be measured
            // can't be kept
            let bytes = serde_json::to_vec(event)
                .map(|json| json.len())
                .unwrap_or(usize::MAX);
            if bytes > self.limits.max_bytes {
                self.entries.clear();
                self.bytes = 0;
                self.forgotten = self.seq;
            } else {
                self.entries.push_back(Entry {
                    seq: self.seq,
                    at: now,
                    bytes,
                    event: event.clone(),
                });
                self.bytes += bytes;
                while self.entries.len() > self.limits.max_events
                    || self.bytes > self.limits.max_bytes
                {
                    self.forget_oldest();
                }
            }
        }
        self.expire(now);
        self.seq
    }

    fn forget_oldest(&mut self) {
        if let Some(entry) = self.entries.pop_front() {
            self.forgotten = entry.seq;
            self.bytes -= entry.bytes;
        }
    }

    /// Forget events older than the window
    pub fn expire(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|entry| entry.at + self.limits.window <= now)
        {
            self.forget_oldest();
        }
    }

    /// Whether every event after `seq` is still kept (and `seq` was given out)
    pub fn covers(&mut self, seq: u64, now: Instant) -> bool {
        self.expire(now);
        seq <= self.seq && seq >= self.forgotten
    }

    /// The client has everything up to `seq`: forget it
    pub fn acknowledge(&mut self, seq: u64) {
        while self.entries.front().is_some_and(|entry| entry.seq <= seq) {
            self.forget_oldest();
        }
        self.forgotten = self.forgotten.max(seq.min(self.seq));
    }

    /// The kept events, in order
    pub fn kept(&self) -> impl Iterator<Item = (u64, &EventV1)> {
        self.entries.iter().map(|entry| (entry.seq, &entry.event))
    }

    /// The events after `seq`, in order; `None` when some of them are no
    /// longer kept or `seq` was never given out
    #[cfg(test)]
    pub fn after(&mut self, seq: u64, now: Instant) -> Option<Vec<(u64, EventV1)>> {
        if !self.covers(seq, now) {
            return None;
        }
        Some(
            self.kept()
                .filter(|(kept, _)| *kept > seq)
                .map(|(kept, event)| (kept, event.clone()))
                .collect(),
        )
    }
}

/// A new socket asks to take over a session
pub struct Attach {
    pub writer: Arc<Mutex<WsWriter>>,
    pub config: ProtocolConfiguration,
    /// Last `seq` the client received
    pub seq: u64,
    /// Ends the new socket's reader when the session ends
    pub kill_worker: async_channel::Sender<()>,
    /// The attachment's number once the events were sent again, or `None`
    /// when the session can't be resumed from `seq`
    pub reply: oneshot::Sender<Option<u64>>,
}

/// Messages to a session's listener
pub enum Control {
    /// The reader of attachment `generation` ended (its socket is gone)
    WorkerEnded { generation: u64 },
    /// A resume
    Attach(Attach),
    /// The session is over (closed or expired login, or ended to stay
    /// within the caps): end it now, no resume
    SessionOver,
}

/// What a resume needs to reach a dropped session
#[derive(Clone)]
pub struct SessionHandle {
    pub user_id: String,
    /// Login session behind the token (empty for bots), so only the same
    /// login can resume
    pub auth_session_id: String,
    /// Intents the session was made with
    pub intents: u64,
    pub subscribed: Arc<RwLock<HashSet<String>>>,
    pub active_servers: Arc<Mutex<lru_time_cache::LruCache<String, ()>>>,
    pub topic_signal: async_channel::Sender<()>,
    pub control: async_channel::Sender<Control>,
}

struct Registered {
    handle: SessionHandle,
    /// Since when the session has no socket
    detached_at: Option<Instant>,
}

static SESSIONS: Lazy<StdMutex<HashMap<String, Registered>>> =
    Lazy::new(|| StdMutex::new(HashMap::new()));

/// Keeps a session reachable for resumes; dropping it (also on a panic)
/// makes it unreachable
pub struct Registration(String);

impl Drop for Registration {
    fn drop(&mut self) {
        unregister(&self.0);
    }
}

/// Make a session reachable for resumes
pub fn register(session_id: &str, handle: SessionHandle) -> Registration {
    if let Ok(mut sessions) = SESSIONS.lock() {
        sessions.insert(
            session_id.to_owned(),
            Registered {
                handle,
                detached_at: None,
            },
        );
    }
    Registration(session_id.to_owned())
}

/// The session ended: nothing can resume it any more
pub fn unregister(session_id: &str) {
    if let Ok(mut sessions) = SESSIONS.lock() {
        sessions.remove(session_id);
    }
}

/// A session that may be resumed
pub fn find(session_id: &str) -> Option<SessionHandle> {
    SESSIONS
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(session_id).map(|entry| entry.handle.clone()))
}

/// The session has a socket again
pub fn attached(session_id: &str) {
    if let Ok(mut sessions) = SESSIONS.lock() {
        if let Some(entry) = sessions.get_mut(session_id) {
            entry.detached_at = None;
        }
    }
}

/// The session lost its socket. To stay within the caps the oldest dropped
/// sessions of the same user, then of everyone, are made unreachable; their
/// listeners are given back to be told `SessionOver`.
pub fn detached(
    session_id: &str,
    limits: &ResumeLimits,
    now: Instant,
) -> Vec<async_channel::Sender<Control>> {
    let Ok(mut sessions) = SESSIONS.lock() else {
        return vec![];
    };
    let Some(entry) = sessions.get_mut(session_id) else {
        return vec![];
    };
    entry.detached_at = Some(now);
    let user = entry.handle.user_id.clone();

    // oldest first; the session that just dropped is never the one to go
    let waiting = |sessions: &HashMap<String, Registered>, same_user: bool| {
        let mut waiting: Vec<(Instant, String)> = sessions
            .iter()
            .filter(|(id, entry)| {
                id.as_str() != session_id && (!same_user || entry.handle.user_id == user)
            })
            .filter_map(|(id, entry)| entry.detached_at.map(|at| (at, id.clone())))
            .collect();
        waiting.sort();
        waiting
    };

    let mut ended = vec![];
    let mine = waiting(&sessions, true);
    let over = (mine.len() + 1).saturating_sub(limits.detached_per_user.max(1));
    for (_, id) in mine.into_iter().take(over) {
        if let Some(entry) = sessions.remove(&id) {
            ended.push(entry.handle.control);
        }
    }
    let all = waiting(&sessions, false);
    let over = (all.len() + 1).saturating_sub(limits.detached_total.max(1));
    for (_, id) in all.into_iter().take(over) {
        if let Some(entry) = sessions.remove(&id) {
            ended.push(entry.handle.control);
        }
    }
    ended
}

/// What a kept event is about, checked again before it is sent again
pub enum Scope<'a> {
    /// Content of a channel: only while the session still follows it
    Channel(&'a str),
    /// A channel's webhooks: only while the session may still manage them
    Webhooks(&'a str),
    /// Between two channels (a move): while either is still followed
    Channels(&'a str, &'a str),
    /// A server's own events: only while the session is still in it
    Server(&'a str),
    /// An emoji's change: while the session is still in the emoji's server
    Emoji(&'a str),
    /// Everything else: events about the session's own user (its settings,
    /// relationships, acks, account, the bot's interactions) and events that
    /// take something away (ChannelDelete, ServerMemberLeave, ServerDelete,
    /// EmojiDelete), which reveal nothing new
    Always,
}

pub fn scope(event: &EventV1) -> Scope<'_> {
    use EventV1::*;
    match event {
        Message(message) => Scope::Channel(&message.channel),
        MessageUpdate { channel, .. }
        | MessageAppend { channel, .. }
        | MessageDelete { channel, .. }
        | BulkMessageDelete { channel, .. } => Scope::Channel(channel),
        MessageReact { channel_id, .. }
        | MessageUnreact { channel_id, .. }
        | MessageRemoveReaction { channel_id, .. } => Scope::Channel(channel_id),
        ChannelStartTyping { id, .. } | ChannelStopTyping { id, .. } => Scope::Channel(id),
        ChannelCreate(channel) => Scope::Channel(channel.id()),
        ChannelUpdate { id, .. } => Scope::Channel(id),
        ChannelGroupJoin { id, .. } => Scope::Channel(id),
        VoiceChannelJoin { id, .. } | VoiceChannelLeave { id, .. } => Scope::Channel(id),
        UserVoiceStateUpdate { channel_id, .. } | VoiceCallUpdate { channel_id, .. } => {
            Scope::Channel(channel_id)
        }
        UserMoveVoiceChannel { to, .. } => Scope::Channel(to),
        VoiceChannelMove { from, to, .. } => Scope::Channels(from, to),
        EphemeralMessageCreate { message } | EphemeralMessageUpdate { message } => {
            Scope::Channel(&message.channel_id)
        }
        EphemeralMessageDelete { channel_id, .. }
        | InteractionModal { channel_id, .. }
        | InteractionStateUpdate { channel_id, .. } => Scope::Channel(channel_id),
        EmojiUpdate { id, .. } => Scope::Emoji(id),
        WebhookCreate(webhook) => Scope::Webhooks(&webhook.channel_id),
        WebhookUpdate { channel_id, .. } | WebhookDelete { channel_id, .. } => {
            Scope::Webhooks(channel_id)
        }
        ServerCreate { id, .. }
        | ServerUpdate { id, .. }
        | ServerMemberJoin { id, .. }
        | ServerRoleUpdate { id, .. }
        | ServerRoleDelete { id, .. }
        | ServerRoleRanksUpdate { id, .. } => Scope::Server(id),
        ServerMemberUpdate { id, .. } => Scope::Server(&id.server),
        ApplicationCommandsUpdate { server_id, .. } => Scope::Server(server_id),
        // an event in a channel only for those who still see the channel
        ServerEventCreate(event) | ServerEventUpdate { event } => match event.place.channel() {
            Some(channel) => Scope::Channel(channel),
            None => Scope::Server(&event.server),
        },
        ServerEventInterest { channel, server, .. } => match channel {
            Some(channel) => Scope::Channel(channel),
            None => Scope::Server(server),
        },
        EmojiCreate(emoji) => match &emoji.parent {
            revolt_models::v0::EmojiParent::Server { id } => Scope::Server(id),
            _ => Scope::Always,
        },
        _ => Scope::Always,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interest_in_a_channel_event_is_replayed_only_with_the_channel() {
        let event = EventV1::ServerEventInterest {
            id: "e".into(),
            server: "s".into(),
            channel: Some("c".into()),
            user: "u".into(),
            interested: true,
            count: 1,
        };
        assert!(matches!(scope(&event), Scope::Channel("c")));
        let external = EventV1::ServerEventInterest {
            id: "e".into(),
            server: "s".into(),
            channel: None,
            user: "u".into(),
            interested: true,
            count: 1,
        };
        assert!(matches!(scope(&external), Scope::Server("s")));
    }

    /// A kept event, told apart by `n`
    fn event(n: usize) -> EventV1 {
        EventV1::ChannelAck {
            id: "channel".to_owned(),
            user: "me".to_owned(),
            message_id: format!("user{n}"),
        }
    }

    fn typing(n: usize) -> EventV1 {
        EventV1::ChannelStartTyping {
            id: "channel".to_owned(),
            user: format!("user{n}"),
        }
    }

    fn limits(window_secs: u64, max_events: usize) -> ResumeLimits {
        ResumeLimits {
            window: Duration::from_secs(window_secs),
            max_events,
            ..Default::default()
        }
    }

    fn users(events: &[(u64, EventV1)]) -> Vec<(u64, String)> {
        events
            .iter()
            .map(|(seq, event)| match event {
                EventV1::ChannelAck { message_id, .. } => (*seq, message_id.clone()),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn sequence_numbers_start_at_one_and_replay_keeps_their_order() {
        let now = Instant::now();
        let mut buffer = ReplayBuffer::new(limits(120, 10), true);
        assert_eq!(buffer.last_seq(), 0);
        for n in 1..=5 {
            assert_eq!(buffer.record(&event(n), now), n as u64);
        }

        let replay = buffer.after(2, now).expect("all kept");
        assert_eq!(
            users(&replay),
            vec![(3, "user3".into()), (4, "user4".into()), (5, "user5".into())]
        );
        assert_eq!(buffer.after(5, now).unwrap().len(), 0, "nothing missed");
        assert_eq!(buffer.after(0, now).unwrap().len(), 5, "from the start");
        assert!(buffer.after(6, now).is_none(), "a seq never given out");
    }

    #[test]
    fn a_full_buffer_forgets_the_oldest_and_refuses_resumes_from_before() {
        let now = Instant::now();
        let mut buffer = ReplayBuffer::new(limits(120, 3), true);
        for n in 1..=5 {
            buffer.record(&event(n), now);
        }
        // 1 and 2 are gone: a client that saw only 1 missed 2
        assert!(buffer.after(1, now).is_none());
        assert!(buffer.after(0, now).is_none());
        assert_eq!(users(&buffer.after(2, now).unwrap()).len(), 3);
    }

    #[test]
    fn events_older_than_the_window_are_forgotten() {
        let start = Instant::now();
        let mut buffer = ReplayBuffer::new(limits(120, 100), true);
        buffer.record(&event(1), start);
        buffer.record(&event(2), start + Duration::from_secs(60));

        let later = start + Duration::from_secs(150);
        // event 1 has expired, so a client that saw nothing can't resume
        assert!(buffer.after(0, later).is_none());
        assert_eq!(users(&buffer.after(1, later).unwrap()), vec![(2, "user2".into())]);
        assert!(buffer.after(1, start + Duration::from_secs(200)).is_none());
    }

    #[test]
    fn a_session_that_is_not_resumable_only_counts() {
        let now = Instant::now();
        let mut buffer = ReplayBuffer::new(limits(120, 10), false);
        buffer.record(&event(1), now);
        assert_eq!(buffer.record(&event(2), now), 2);
        assert!(buffer.after(1, now).is_none());
        assert_eq!(buffer.after(2, now).unwrap().len(), 0);
    }

    fn ack(n: usize) -> EventV1 {
        EventV1::ChannelAck {
            id: "channel".to_owned(),
            user: "me".to_owned(),
            message_id: format!("message{n}"),
        }
    }

    #[test]
    fn typing_counts_but_is_not_kept() {
        let now = Instant::now();
        let mut buffer = ReplayBuffer::new(limits(120, 10), true);
        buffer.record(&ack(1), now);
        assert_eq!(buffer.record(&typing(2), now), 2);
        buffer.record(&ack(3), now);
        let kept: Vec<u64> = buffer.after(0, now).unwrap().iter().map(|(seq, _)| *seq).collect();
        assert_eq!(kept, vec![1, 3], "a gap where typing was");
    }

    #[test]
    fn the_byte_budget_forgets_the_oldest() {
        let now = Instant::now();
        let size = serde_json::to_vec(&ack(1)).unwrap().len();
        let mut buffer = ReplayBuffer::new(
            ResumeLimits {
                max_bytes: size * 2,
                ..limits(120, 100)
            },
            true,
        );
        for n in 1..=3 {
            buffer.record(&ack(n), now);
        }
        assert!(buffer.after(0, now).is_none(), "the first no longer fits");
        assert_eq!(buffer.after(1, now).unwrap().len(), 2);
    }

    #[test]
    fn seq_goes_next_to_the_event_fields() {
        let unit = serde_json::to_value(Sequenced {
            event: &EventV1::Authenticated,
            seq: 7,
        })
        .unwrap();
        assert_eq!(unit, serde_json::json!({ "type": "Authenticated", "seq": 7 }));

        let fields = serde_json::to_value(Sequenced {
            event: &typing(1),
            seq: 8,
        })
        .unwrap();
        assert_eq!(
            fields,
            serde_json::json!({ "type": "ChannelStartTyping", "id": "channel", "user": "user1", "seq": 8 })
        );

        // a newtype event (the message itself is the event)
        let message: revolt_models::v0::Message = serde_json::from_value(serde_json::json!({
            "_id": "message", "channel": "channel", "author": "user", "content": "hi"
        }))
        .unwrap();
        let event = EventV1::Message(message);
        let value = serde_json::to_value(Sequenced { event: &event, seq: 9 }).unwrap();
        assert_eq!(value["type"], "Message");
        assert_eq!(value["_id"], "message");
        assert_eq!(value["content"], "hi");
        assert_eq!(value["seq"], 9);

        // msgpack clients get the same map
        let packed = rmp_serde::to_vec_named(&Sequenced { event: &event, seq: 9 }).unwrap();
        let unpacked: serde_json::Value = rmp_serde::from_slice(&packed).unwrap();
        assert_eq!(unpacked, value);
    }
}
