//! Who may see a custom emoji in someone's status.
//!
//! A custom emoji id leads to its server (`GET /custom/emoji/{id}` names
//! the parent), so a status emoji from a server would tell everyone who
//! sees the status which server its owner is in. Another person sees a
//! custom status emoji only while both the owner and the viewer are in the
//! emoji's server; otherwise the status shows without its emoji. Unicode
//! emoji are always shown.
//!
//! Cost: only statuses that carry a custom emoji are checked. The emoji's
//! server is kept for ten minutes (emoji rarely move), and "is the owner in
//! that server" for one minute, both in small process-wide LRU caches. The
//! viewer's side comes from what is already known: a gateway connection's
//! servers, or one membership lookup before a list (only when some status
//! in it has a custom emoji). So a member list or a Ready asks the
//! database at most once per emoji, once per (server, owner) a minute, and
//! once for the viewer; each ask is one indexed read. Joining or leaving a
//! server drops the cached owner answer in this process at once; other
//! processes follow within the minute.

use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use revolt_models::v0;
use ulid::Ulid;

use crate::{Database, EmojiParent};

const EMOJI_KEEP: Duration = Duration::from_secs(600);
const MEMBER_KEEP: Duration = Duration::from_secs(60);

static EMOJI_SERVER: Lazy<Mutex<lru::LruCache<String, (Option<String>, Instant)>>> =
    Lazy::new(|| Mutex::new(lru::LruCache::new(NonZeroUsize::new(4096).unwrap())));

static MEMBERSHIP: Lazy<Mutex<lru::LruCache<(String, String), (bool, Instant)>>> =
    Lazy::new(|| Mutex::new(lru::LruCache::new(NonZeroUsize::new(16384).unwrap())));

/// Whether a status emoji is a custom emoji id (unicode emoji are not)
pub fn is_custom(emoji: &str) -> bool {
    Ulid::from_str(emoji).is_ok()
}

/// The server a custom emoji belongs to; None for a detached or unknown one
async fn emoji_server(db: &Database, id: &str) -> Option<String> {
    if let Some((server, at)) = EMOJI_SERVER.lock().unwrap().get(id).cloned() {
        if at.elapsed() < EMOJI_KEEP {
            return server;
        }
    }
    let server = match db.fetch_emoji(id).await {
        Ok(emoji) => match emoji.parent {
            EmojiParent::Server { id } => Some(id),
            EmojiParent::Detached => None,
        },
        Err(_) => None,
    };
    EMOJI_SERVER
        .lock()
        .unwrap()
        .put(id.to_string(), (server.clone(), Instant::now()));
    server
}

/// Whether a person is a member of a server (cached for a minute)
async fn is_member(db: &Database, server: &str, user: &str) -> bool {
    let key = (server.to_string(), user.to_string());
    if let Some((member, at)) = MEMBERSHIP.lock().unwrap().get(&key).cloned() {
        if at.elapsed() < MEMBER_KEEP {
            return member;
        }
    }
    let member = db.fetch_member(server, user).await.is_ok();
    MEMBERSHIP.lock().unwrap().put(key, (member, Instant::now()));
    member
}

/// Forget a cached membership: call when someone joins or leaves
pub fn forget_membership(server: &str, user: &str) {
    MEMBERSHIP
        .lock()
        .unwrap()
        .pop(&(server.to_string(), user.to_string()));
}

/// Whether `viewer` may see `owner`'s status emoji. `viewer` None (no one
/// in particular, e.g. a message broadcast) sees no custom emoji. The
/// viewer's membership comes from the cache; for many users at once, or on
/// a gateway connection that knows its servers, use [`hide_for_member_of`].
pub async fn may_see(db: &Database, emoji: &str, owner: &str, viewer: Option<&str>) -> bool {
    if !is_custom(emoji) {
        return true;
    }
    let Some(viewer) = viewer else {
        return false;
    };
    if viewer == owner {
        return true;
    }
    let Some(server) = emoji_server(db, emoji).await else {
        return false;
    };
    is_member(db, &server, viewer).await && is_member(db, &server, owner).await
}

/// The same check with the viewer's servers already known (a gateway
/// connection's servers, or one membership lookup before a list): only the
/// owner side asks the cache
async fn may_see_member_of(
    db: &Database,
    emoji: &str,
    owner: &str,
    viewer: &str,
    viewer_in: &(dyn Fn(&str) -> bool + Sync),
) -> bool {
    if !is_custom(emoji) || viewer == owner {
        return true;
    }
    let Some(server) = emoji_server(db, emoji).await else {
        return false;
    };
    viewer_in(&server) && is_member(db, &server, owner).await
}

/// Drop a custom emoji from a status meant for no one in particular (a
/// message broadcast); a unicode emoji stays
pub fn drop_custom_emoji(status: &mut Option<v0::UserStatus>) {
    if status
        .as_ref()
        .and_then(|status| status.emoji.as_deref())
        .is_some_and(is_custom)
    {
        drop_emoji(status);
    }
}

/// Drop the emoji from a status shown to someone who may not see it
fn drop_emoji(status: &mut Option<v0::UserStatus>) {
    let Some(shown) = status.as_mut() else {
        return;
    };
    shown.emoji = None;
    if shown.text.is_none() {
        // nothing left to expire
        shown.expires_at = None;
        if shown.presence.is_none() {
            // an emoji-only status with its emoji hidden is no status
            *status = None;
        }
    }
}

/// Drop a custom status emoji `viewer` may not see from a status shown to them
pub async fn hide_for(
    db: &Database,
    status: &mut Option<v0::UserStatus>,
    owner: &str,
    viewer: Option<&str>,
) {
    let Some(emoji) = status.as_ref().and_then(|status| status.emoji.clone()) else {
        return;
    };
    if !may_see(db, &emoji, owner, viewer).await {
        drop_emoji(status);
    }
}

/// [`hide_for`] for a viewer whose servers are known: `viewer_in(server)`
/// says whether the viewer is in a server
pub async fn hide_for_member_of(
    db: &Database,
    status: &mut Option<v0::UserStatus>,
    owner: &str,
    viewer: &str,
    viewer_in: &(dyn Fn(&str) -> bool + Sync),
) {
    let Some(emoji) = status.as_ref().and_then(|status| status.emoji.clone()) else {
        return;
    };
    if !may_see_member_of(db, &emoji, owner, viewer, viewer_in).await {
        drop_emoji(status);
    }
}

/// Whether any of these statuses has a custom emoji (so a list need not
/// look up the viewer's servers when none does)
pub fn any_custom<'a>(statuses: impl IntoIterator<Item = Option<&'a v0::UserStatus>>) -> bool {
    statuses
        .into_iter()
        .flatten()
        .any(|status| status.emoji.as_deref().is_some_and(is_custom))
}
