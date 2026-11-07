use iso8601_timestamp::Timestamp;
use revolt_result::Result;

use crate::{Session, WebPushSubscription};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractSessions: Sync + Send {
    /// Find session by id
    async fn fetch_session(&self, id: &str) -> Result<Session>;

    /// Find sessions by user id
    async fn fetch_sessions(&self, user_id: &str) -> Result<Vec<Session>>;

    /// Find sessions by user ids
    async fn fetch_sessions_with_subscription(&self, user_ids: &[String]) -> Result<Vec<Session>>;

    /// Find sessions last used at or before `before` whose grace, if any, is
    /// over at `now`
    async fn fetch_idle_sessions(
        &self,
        before: Timestamp,
        now: Timestamp,
        limit: i64,
    ) -> Result<Vec<Session>>;

    /// Find session by token
    async fn fetch_session_by_token(&self, token: &str) -> Result<Session>;

    /// Insert a new session; fails if the id or token is taken
    async fn insert_session(&self, session: &Session) -> Result<()>;

    /// Write a whole session, creating it if it is missing. Tests only, to set
    /// up stale data: in the server a write in flight during a logout would
    /// bring the session back with its token.
    #[cfg(any(test, feature = "test-utils"))]
    async fn save_session(&self, session: &Session) -> Result<()>;

    /// Set the name of a session; `UnknownUser` if it is gone, as when a
    /// lookup finds no session
    async fn update_session_name(&self, id: &str, name: &str) -> Result<()>;

    /// Set the push subscription of a session; `InvalidSession` if it is
    /// gone: the caller is that session
    async fn update_session_subscription(
        &self,
        id: &str,
        subscription: &WebPushSubscription,
    ) -> Result<()>;

    /// Delete session
    async fn delete_session(&self, id: &str) -> Result<()>;

    /// Delete session
    async fn delete_all_sessions(&self, user_id: &str, ignore: Option<String>) -> Result<()>;

    /// Remove push subscription for a session by session id
    async fn remove_push_subscription_by_session_id(&self, session_id: &str) -> Result<()>;

    async fn update_session_last_seen(&self, session_id: &str, when: Timestamp) -> Result<()>;

    /// Set `last_seen` unless the session is gone, or was last used at or
    /// before `idle_before` with its grace over. Says whether it was set.
    async fn touch_session(
        &self,
        session_id: &str,
        when: Timestamp,
        idle_before: Option<Timestamp>,
    ) -> Result<bool>;
}
