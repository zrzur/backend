use iso8601_timestamp::Timestamp;
use revolt_models::v0::ServerEventStatus;
use revolt_result::Result;

use crate::ReferenceDb;
use crate::{ServerEvent, ServerEventInterest, ServerEventInterestKey};

use super::AbstractServerEvents;

#[async_trait]
impl AbstractServerEvents for ReferenceDb {
    async fn insert_server_event(&self, event: &ServerEvent) -> Result<()> {
        let mut events = self.server_events.lock().await;
        if events.contains_key(&event.id) {
            return Err(create_database_error!("insert", "server_events"));
        }
        events.insert(event.id.clone(), event.clone());
        Ok(())
    }

    async fn fetch_server_event(&self, id: &str) -> Result<ServerEvent> {
        let events = self.server_events.lock().await;
        events
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_upcoming_server_events(&self, server: &str) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        let mut list: Vec<ServerEvent> = events
            .values()
            .filter(|event| event.server == server && event.is_upcoming())
            .cloned()
            .collect();
        list.sort_by(|a, b| a.starts_at.cmp(&b.starts_at).then(a.id.cmp(&b.id)));
        Ok(list)
    }

    async fn update_server_event(&self, event: &ServerEvent) -> Result<()> {
        let mut events = self.server_events.lock().await;
        let stored = events
            .get_mut(&event.id)
            .ok_or_else(|| create_error!(NotFound))?;
        *stored = ServerEvent {
            status: stored.status.clone(),
            interested_count: stored.interested_count,
            started_at: stored.started_at,
            ended_at: stored.ended_at,
            ..event.clone()
        };
        Ok(())
    }

    async fn set_server_event_status(
        &self,
        id: &str,
        from: ServerEventStatus,
        to: ServerEventStatus,
        at: Timestamp,
    ) -> Result<bool> {
        let mut events = self.server_events.lock().await;
        match events.get_mut(id) {
            Some(event) if event.status == from => {
                if to == ServerEventStatus::Active {
                    event.started_at = Some(at);
                } else {
                    event.ended_at = Some(at);
                }
                event.status = to;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete_server_event(&self, id: &str) -> Result<()> {
        self.server_events.lock().await.remove(id);
        self.server_event_interests
            .lock()
            .await
            .retain(|key, _| key.event != id);
        Ok(())
    }

    async fn fetch_server_events_to_start(&self, now: Timestamp) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        Ok(events
            .values()
            .filter(|event| event.status == ServerEventStatus::Scheduled && event.starts_at <= now)
            .cloned()
            .collect())
    }

    async fn fetch_active_server_events(&self) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        Ok(events
            .values()
            .filter(|event| event.status == ServerEventStatus::Active)
            .cloned()
            .collect())
    }

    async fn add_server_event_interest(
        &self,
        event: &str,
        server: &str,
        user: &str,
    ) -> Result<Option<u32>> {
        let key = ServerEventInterestKey {
            event: event.to_string(),
            user: user.to_string(),
        };
        let mut interests = self.server_event_interests.lock().await;
        if interests.contains_key(&key) {
            return Ok(None);
        }
        interests.insert(
            key.clone(),
            ServerEventInterest {
                id: key,
                server: server.to_string(),
                created_at: Timestamp::now_utc(),
            },
        );
        let mut events = self.server_events.lock().await;
        let stored = events
            .get_mut(event)
            .ok_or_else(|| create_error!(NotFound))?;
        stored.interested_count += 1;
        Ok(Some(stored.interested_count))
    }

    async fn remove_server_event_interest(&self, event: &str, user: &str) -> Result<Option<u32>> {
        let key = ServerEventInterestKey {
            event: event.to_string(),
            user: user.to_string(),
        };
        if self
            .server_event_interests
            .lock()
            .await
            .remove(&key)
            .is_none()
        {
            return Ok(None);
        }
        let mut events = self.server_events.lock().await;
        let stored = events
            .get_mut(event)
            .ok_or_else(|| create_error!(NotFound))?;
        stored.interested_count = stored.interested_count.saturating_sub(1);
        Ok(Some(stored.interested_count))
    }

    async fn fetch_server_event_interested(
        &self,
        event: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>> {
        let interests = self.server_event_interests.lock().await;
        let mut users: Vec<String> = interests
            .keys()
            .filter(|key| key.event == event && after.is_none_or(|after| key.user.as_str() > after))
            .map(|key| key.user.clone())
            .collect();
        users.sort();
        users.truncate(limit.max(0) as usize);
        Ok(users)
    }

    async fn fetch_server_event_interests_of(
        &self,
        user: &str,
        events: &[String],
    ) -> Result<Vec<String>> {
        let interests = self.server_event_interests.lock().await;
        Ok(interests
            .keys()
            .filter(|key| key.user == user && events.contains(&key.event))
            .map(|key| key.event.clone())
            .collect())
    }
}
