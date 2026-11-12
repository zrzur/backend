use crate::{AbstractSessions, ReferenceDb, Session, WebPushSubscription};
use iso8601_timestamp::Timestamp;
use revolt_result::Result;

#[async_trait]
impl AbstractSessions for ReferenceDb {
    /// Find session by id
    async fn fetch_session(&self, id: &str) -> Result<Session> {
        let sessions = self.sessions.lock().await;
        sessions
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(UnknownUser))
    }

    /// Find sessions by user id
    async fn fetch_sessions(&self, user_id: &str) -> Result<Vec<Session>> {
        let sessions = self.sessions.lock().await;
        Ok(sessions
            .values()
            .filter(|session| session.user_id == user_id)
            .cloned()
            .collect())
    }

    /// Find sessions by user ids
    async fn fetch_sessions_with_subscription(&self, user_ids: &[String]) -> Result<Vec<Session>> {
        let sessions = self.sessions.lock().await;
        Ok(sessions
            .values()
            .filter(|session| session.subscription.is_some() && user_ids.contains(&session.user_id))
            .cloned()
            .collect())
    }

    /// Find sessions last used at or before `before` whose grace, if any, is
    /// over at `now`
    async fn fetch_idle_sessions(
        &self,
        before: Timestamp,
        now: Timestamp,
        limit: i64,
    ) -> Result<Vec<Session>> {
        let sessions = self.sessions.lock().await;
        Ok(sessions
            .values()
            .filter(|session| {
                session.last_seen <= before
                    && session.idle_grace_until.is_some_and(|until| until <= now)
            })
            .take(limit as usize)
            .cloned()
            .collect())
    }

    /// Find session by token
    async fn fetch_session_by_token(&self, token: &str) -> Result<Session> {
        let sessions = self.sessions.lock().await;
        sessions
            .values()
            .find(|session| session.token_hash == crate::models::hash_secret(token))
            .cloned()
            .ok_or_else(|| create_error!(InvalidSession))
    }

    /// Insert a new session; fails if the id or token is taken
    async fn insert_session(&self, session: &Session) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if sessions
            .values()
            .any(|other| other.id == session.id || other.token_hash == session.token_hash)
        {
            return Err(create_database_error!("insert_one", "sessions"));
        }
        sessions.insert(session.id.to_string(), session.clone());
        Ok(())
    }

    /// Write a whole session, creating it if it is missing
    #[cfg(any(test, feature = "test-utils"))]
    async fn save_session(&self, session: &Session) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        sessions.insert(session.id.to_string(), session.clone());
        Ok(())
    }

    /// Set the name of a session; fails if the session is gone
    async fn update_session_name(&self, id: &str, name: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| create_error!(UnknownUser))?;
        session.name = name.to_string();
        Ok(())
    }

    /// Set the push subscription of a session; fails if the session is gone
    async fn update_session_subscription(
        &self,
        id: &str,
        subscription: &WebPushSubscription,
    ) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| create_error!(InvalidSession))?;
        session.subscription = Some(subscription.clone());
        Ok(())
    }

    /// Delete session
    async fn delete_session(&self, id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if sessions.remove(id).is_some() {
            Ok(())
        } else {
            Err(create_error!(InvalidSession))
        }
    }

    /// Delete session
    async fn delete_all_sessions(&self, user_id: &str, ignore: Option<String>) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| {
            if session.user_id == user_id {
                if let Some(ignore) = &ignore {
                    ignore == &session.id
                } else {
                    false
                }
            } else {
                true
            }
        });

        Ok(())
    }

    /// Remove push subscription for a session by session id
    async fn remove_push_subscription_by_session_id(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;

        if let Some(session) = sessions.get_mut(session_id) {
            session.subscription = None;
        };

        Ok(())
    }

    async fn update_session_last_seen(&self, session_id: &str, when: Timestamp) -> Result<()> {
        let mut sessions = self.sessions.lock().await;

        if let Some(session) = sessions.get_mut(session_id) {
            session.last_seen = when;
        };

        Ok(())
    }

    /// Set `last_seen` unless the session is gone or idle
    async fn touch_session(
        &self,
        session_id: &str,
        when: Timestamp,
        idle_before: Option<Timestamp>,
    ) -> Result<bool> {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return Ok(false);
        };

        let idle = idle_before.is_some_and(|before| {
            session.last_seen <= before
                && session.idle_grace_until.is_some_and(|until| until <= when)
        });
        if !idle {
            session.last_seen = when;
        }
        Ok(!idle)
    }
}
