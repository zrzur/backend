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
}

impl Default for ResumeLimits {
    fn default() -> Self {
        ResumeLimits {
            window: Duration::from_secs(120),
            max_events: 1000,
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
    event: EventV1,
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
    entries: VecDeque<Entry>,
}

impl ReplayBuffer {
    pub fn new(limits: ResumeLimits, keep: bool) -> Self {
        ReplayBuffer {
            limits,
            keep,
            seq: 0,
            forgotten: 0,
            entries: VecDeque::new(),
        }
    }

    /// Last sequence number given out (0 before the first event)
    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Give the next sequence number to an event, and keep it if this
    /// session keeps events
    pub fn record(&mut self, event: &EventV1, now: Instant) -> u64 {
        self.seq += 1;
        if self.keep {
            self.entries.push_back(Entry {
                seq: self.seq,
                at: now,
                event: event.clone(),
            });
            while self.entries.len() > self.limits.max_events {
                self.forget_oldest();
            }
            self.expire(now);
        } else {
            self.forgotten = self.seq;
        }
        self.seq
    }

    fn forget_oldest(&mut self) {
        if let Some(entry) = self.entries.pop_front() {
            self.forgotten = entry.seq;
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

    /// The events after `seq`, in order; `None` when some of them are no
    /// longer kept or `seq` was never given out
    pub fn after(&mut self, seq: u64, now: Instant) -> Option<Vec<(u64, EventV1)>> {
        self.expire(now);
        if seq > self.seq || seq < self.forgotten {
            return None;
        }
        Some(
            self.entries
                .iter()
                .filter(|entry| entry.seq > seq)
                .map(|entry| (entry.seq, entry.event.clone()))
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

static SESSIONS: Lazy<StdMutex<HashMap<String, SessionHandle>>> =
    Lazy::new(|| StdMutex::new(HashMap::new()));

/// Make a session reachable for resumes
pub fn register(session_id: &str, handle: SessionHandle) {
    if let Ok(mut sessions) = SESSIONS.lock() {
        sessions.insert(session_id.to_owned(), handle);
    }
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
        .and_then(|sessions| sessions.get(session_id).cloned())
}

/// What a kept event is about, checked again before it is sent again
pub enum Scope<'a> {
    /// Content of a channel: only while the session still follows it
    Channel(&'a str),
    /// A channel's webhooks: only while the session may still manage them
    Webhooks(&'a str),
    /// A server's own events: only while the session is still in it
    Server(&'a str),
    /// Events about the session itself, or ones that take something away
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
        }
    }

    fn users(events: &[(u64, EventV1)]) -> Vec<(u64, String)> {
        events
            .iter()
            .map(|(seq, event)| match event {
                EventV1::ChannelStartTyping { user, .. } => (*seq, user.clone()),
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
            assert_eq!(buffer.record(&typing(n), now), n as u64);
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
            buffer.record(&typing(n), now);
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
        buffer.record(&typing(1), start);
        buffer.record(&typing(2), start + Duration::from_secs(60));

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
        buffer.record(&typing(1), now);
        assert_eq!(buffer.record(&typing(2), now), 2);
        assert!(buffer.after(1, now).is_none());
        assert_eq!(buffer.after(2, now).unwrap().len(), 0);
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
