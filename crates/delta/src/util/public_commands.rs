//! The global commands of a bot as its Discover page and profile read them.
//!
//! A popular bot's page is opened far more often than its commands change, so
//! the list (with whether the bot is public and who owns it, which decide who
//! may read it) is kept in memory for [`TTL`]. A view of a cached bot costs no
//! database query. Every route that changes global commands, the bot's public
//! flag or the bot itself calls [`forget`]; the short lifetime covers changes
//! made through another API instance.

use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use lru::LruCache;
use revolt_database::Database;
use revolt_models::v0::ApplicationCommand;
use revolt_result::{create_error, Result};

/// How long a list is kept
pub const TTL: Duration = Duration::from_secs(60);

/// Bots kept at once (a few kilobytes each)
const CAPACITY: usize = 4096;

/// What the route needs to answer without the database
#[derive(Clone)]
pub struct Listing {
    pub public: bool,
    pub owner: String,
    /// Global commands, sorted by name
    pub commands: Arc<Vec<ApplicationCommand>>,
}

static CACHE: LazyLock<Mutex<LruCache<String, (Instant, Listing)>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(NonZeroUsize::new(CAPACITY).expect("capacity")))
});

fn cached(bot_id: &str, now: Instant) -> Option<Listing> {
    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match cache.get(bot_id) {
        Some((stored, listing)) if now.duration_since(*stored) < TTL => Some(listing.clone()),
        Some(_) => {
            cache.pop(bot_id);
            None
        }
        None => None,
    }
}

/// The bot's listing, from memory when it is fresh. An unknown bot is
/// `NotFound` and is not kept.
pub async fn listing(db: &Database, bot_id: &str) -> Result<Listing> {
    if let Some(hit) = cached(bot_id, Instant::now()) {
        return Ok(hit);
    }

    let bot = db.fetch_bot(bot_id).await.map_err(|_| create_error!(NotFound))?;
    let mut commands = db.fetch_application_commands(&bot.id, None).await?;
    commands.sort_by(|a, b| a.name.cmp(&b.name));

    let listing = Listing {
        public: bot.public,
        owner: bot.owner,
        commands: Arc::new(commands),
    };
    CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .put(bot_id.to_string(), (Instant::now(), listing.clone()));
    Ok(listing)
}

/// Drop a bot's cached listing: its global commands, public flag or owner
/// changed, or it was deleted
pub fn forget(bot_id: &str) {
    CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .pop(bot_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(owner: &str) -> Listing {
        Listing {
            public: true,
            owner: owner.into(),
            commands: Arc::new(vec![]),
        }
    }

    #[test]
    fn a_listing_expires_after_its_lifetime() {
        let id = "01TESTCACHEEXPIRY0000000000";
        let start = Instant::now();
        CACHE.lock().unwrap().put(id.into(), (start, listing("a")));

        assert_eq!(cached(id, start + TTL / 2).map(|l| l.owner), Some("a".into()));
        assert!(cached(id, start + TTL).is_none());
        // an expired entry is dropped, not kept until it is pushed out
        assert!(CACHE.lock().unwrap().peek(id).is_none());
    }

    #[test]
    fn forget_drops_only_that_bot() {
        let (a, b) = ("01TESTCACHEFORGETA000000000", "01TESTCACHEFORGETB000000000");
        let now = Instant::now();
        CACHE.lock().unwrap().put(a.into(), (now, listing("a")));
        CACHE.lock().unwrap().put(b.into(), (now, listing("b")));

        forget(a);
        assert!(cached(a, now).is_none());
        assert!(cached(b, now).is_some());
    }
}
