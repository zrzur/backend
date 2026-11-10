//! The global commands of a public bot, as its Discover page and profile read
//! them.
//!
//! A popular bot's page is opened far more often than its commands change, so
//! the list of a **public** bot is kept in memory for [`TTL`]: a view of a
//! cached bot costs no database query. Private bots (read only by their owner
//! or by people with a DM with them) are never kept, so nobody can fill the
//! cache with them. The cache holds at most [`CAPACITY`] bots and about
//! [`MAX_BYTES`] of commands (measured as JSON); the least recently read go
//! first.
//!
//! Every route that changes global commands, the bot's public flag or the bot
//! itself calls [`forget`]. A read that started before a `forget` does not
//! store its (possibly stale) answer: `forget` moves an epoch, and the read
//! stores only if the epoch is still the one it saw before reading.
//!
//! Other API instances do not hear about a `forget`: their copy can be up to
//! [`TTL`] old (a bot made private on one instance may still list its
//! commands on another for that long; account deletion by crond relies on it).

use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use lru::LruCache;
use revolt_database::Database;
use revolt_models::v0::ApplicationCommand;
use revolt_result::{create_error, Result};

/// How long a list is kept
pub const TTL: Duration = Duration::from_secs(60);

/// Bots kept at once
pub const CAPACITY: usize = 4096;

/// Commands kept at once, as JSON bytes
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

type Commands = Arc<Vec<ApplicationCommand>>;

/// The kept lists, their total size and the epoch `forget` moves
struct Store {
    entries: LruCache<String, (Instant, Commands, usize)>,
    bytes: usize,
    epoch: u64,
    capacity: usize,
    max_bytes: usize,
}

impl Store {
    fn new(capacity: usize, max_bytes: usize) -> Self {
        Store {
            entries: LruCache::unbounded(),
            bytes: 0,
            epoch: 0,
            capacity,
            max_bytes,
        }
    }

    fn get(&mut self, id: &str, now: Instant) -> Option<Commands> {
        match self.entries.get(id) {
            Some((stored, commands, _)) if now.duration_since(*stored) < TTL => Some(commands.clone()),
            Some(_) => {
                self.remove(id);
                None
            }
            None => None,
        }
    }

    fn remove(&mut self, id: &str) {
        if let Some((_, _, size)) = self.entries.pop(id) {
            self.bytes -= size;
        }
    }

    /// Keep a list read when the epoch was `seen`, unless a `forget` came
    /// since; the least recently read go while over either limit
    fn put_if_current(&mut self, seen: u64, id: &str, commands: Commands, size: usize, now: Instant) -> bool {
        if self.epoch != seen || size > self.max_bytes {
            return false;
        }
        self.remove(id);
        self.entries.put(id.to_string(), (now, commands, size));
        self.bytes += size;
        while self.entries.len() > self.capacity || self.bytes > self.max_bytes {
            match self.entries.pop_lru() {
                Some((_, (_, _, size))) => self.bytes -= size,
                None => break,
            }
        }
        true
    }

    fn forget(&mut self, id: &str) {
        self.epoch = self.epoch.wrapping_add(1);
        self.remove(id);
    }
}

static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(Store::new(CAPACITY, MAX_BYTES)));

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the route needs to decide who may read the list
pub struct Listing {
    pub public: bool,
    /// Only known when the bot was read from the database (a cached bot is
    /// public, so nobody needs it)
    pub owner: Option<String>,
    /// Global commands, sorted by name
    pub commands: Commands,
}

/// The bot's listing: from memory for a fresh public bot, else from the
/// database (and then kept if the bot is public). An unknown bot is
/// `NotFound` and is not kept.
pub async fn listing(db: &Database, bot_id: &str) -> Result<Listing> {
    let seen = {
        let mut store = store();
        if let Some(commands) = store.get(bot_id, Instant::now()) {
            return Ok(Listing {
                public: true,
                owner: None,
                commands,
            });
        }
        store.epoch
    };

    let bot = db.fetch_bot(bot_id).await.map_err(|_| create_error!(NotFound))?;
    let mut commands = db.fetch_application_commands(&bot.id, None).await?;
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    let commands = Arc::new(commands);
    #[cfg(test)]
    after_read(bot_id);

    if bot.public {
        let size = serde_json::to_vec(commands.as_ref()).map(|json| json.len()).unwrap_or(usize::MAX);
        store().put_if_current(seen, bot_id, commands.clone(), size, Instant::now());
    }
    Ok(Listing {
        public: bot.public,
        owner: Some(bot.owner),
        commands,
    })
}

/// Drop a bot's cached listing: its global commands, public flag or owner
/// changed, or it was deleted. A read already under way will not store.
pub fn forget(bot_id: &str) {
    store().forget(bot_id);
}

#[cfg(test)]
static AFTER_READ: Mutex<Option<fn(&str)>> = Mutex::new(None);

/// Tests: run something between the database read and the put
#[cfg(test)]
pub fn set_after_read(hook: Option<fn(&str)>) {
    *AFTER_READ.lock().unwrap() = hook;
}

#[cfg(test)]
fn after_read(bot_id: &str) {
    let hook = *AFTER_READ.lock().unwrap();
    if let Some(hook) = hook {
        hook(bot_id);
    }
}

/// Tests: whether a bot's list is kept
#[cfg(test)]
pub fn is_cached(bot_id: &str) -> bool {
    store().entries.peek(bot_id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Commands {
        Arc::new(vec![])
    }

    #[test]
    fn a_listing_expires_after_its_lifetime() {
        let mut store = Store::new(10, 1000);
        let start = Instant::now();
        assert!(store.put_if_current(0, "a", list(), 10, start));

        assert!(store.get("a", start + TTL / 2).is_some());
        assert!(store.get("a", start + TTL).is_none());
        // an expired entry is dropped, not kept until it is pushed out
        assert!(store.entries.peek("a").is_none());
        assert_eq!(store.bytes, 0);
    }

    #[test]
    fn forget_drops_only_that_bot() {
        let mut store = Store::new(10, 1000);
        let now = Instant::now();
        store.put_if_current(0, "a", list(), 10, now);
        store.put_if_current(0, "b", list(), 10, now);

        store.forget("a");
        assert!(store.get("a", now).is_none());
        assert!(store.get("b", now).is_some());
    }

    #[test]
    fn a_read_from_before_a_forget_is_not_stored() {
        let mut store = Store::new(10, 1000);
        let seen = store.epoch;
        store.forget("a");
        assert!(!store.put_if_current(seen, "a", list(), 10, Instant::now()));
        assert!(store.entries.is_empty());
    }

    #[test]
    fn the_cache_is_bounded_by_size_and_count() {
        let now = Instant::now();
        let mut store = Store::new(3, 100);
        for (id, size) in [("a", 40), ("b", 40), ("c", 10)] {
            store.put_if_current(0, id, list(), size, now);
        }
        assert_eq!(store.bytes, 90);
        // "a" was read last: "b" is the least recently read
        store.get("a", now);
        store.put_if_current(0, "d", list(), 30, now);
        assert!(store.entries.peek("b").is_none());
        assert_eq!(store.bytes, 80);
        // over the count: the least recently read goes
        store.put_if_current(0, "e", list(), 1, now);
        assert_eq!(store.entries.len(), 3);
        assert!(store.entries.peek("c").is_none());
        // a single list larger than the whole budget is never kept
        assert!(!store.put_if_current(0, "huge", list(), 101, now));
        assert!(store.bytes <= 100);
    }
}
