use iso8601_timestamp::Timestamp;
use revolt_models::v0::{ServerEventAnswer, ServerEventStatus};
use revolt_result::Result;

use crate::ReferenceDb;
use crate::{
    RosterChange, ServerEvent, ServerEventInterest, ServerEventInterestKey,
    ServerEventInvitation, ServerEventRoster,
};

use super::AbstractServerEvents;

fn key(event: &str, user: &str) -> ServerEventInterestKey {
    ServerEventInterestKey {
        event: event.to_string(),
        user: user.to_string(),
    }
}

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

    async fn fetch_server_event_drafts(&self, server: &str) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        let mut list: Vec<ServerEvent> = events
            .values()
            .filter(|event| event.server == server && event.status == ServerEventStatus::Draft)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.starts_at.cmp(&b.starts_at).then(a.id.cmp(&b.id)));
        Ok(list)
    }

    async fn fetch_server_events_between(
        &self,
        servers: &[String],
        from: Timestamp,
        to: Timestamp,
        limit: i64,
    ) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        let mut list: Vec<ServerEvent> = events
            .values()
            .filter(|event| {
                servers.contains(&event.server)
                    && event.status != ServerEventStatus::Draft
                    && event.starts_at >= from
                    && event.starts_at < to
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| a.starts_at.cmp(&b.starts_at).then(a.id.cmp(&b.id)));
        list.truncate(limit.max(0) as usize);
        Ok(list)
    }

    async fn fetch_server_events_created_by(&self, user: &str) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        let mut list: Vec<ServerEvent> = events
            .values()
            .filter(|event| event.creator == user)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    async fn fetch_server_events_naming(&self, user: &str) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        let mut list: Vec<ServerEvent> = events
            .values()
            .filter(|event| {
                event.invited.iter().any(|id| id == user)
                    || event.co_organisers.iter().any(|id| id == user)
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
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
            going_count: stored.going_count,
            not_going_count: stored.not_going_count,
            waitlist_count: stored.waitlist_count,
            roster_version: stored.roster_version,
            present_at_start: stored.present_at_start,
            people_removed_at: stored.people_removed_at,
            started_at: stored.started_at,
            ended_at: stored.ended_at,
            series_start: stored.series_start,
            occurrence: stored.occurrence,
            next: stored.next.clone(),
            server: stored.server.clone(),
            creator: stored.creator.clone(),
            time_zone: stored.time_zone.clone(),
            reminders_sent: stored.reminders_sent.clone(),
            invitations: stored.invitations.clone(),
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
                match to {
                    ServerEventStatus::Active => event.started_at = Some(at),
                    // a draft that is published has neither started nor ended
                    ServerEventStatus::Scheduled | ServerEventStatus::Draft => {}
                    _ => event.ended_at = Some(at),
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
        self.server_event_rosters.lock().await.remove(id);
        Ok(())
    }

    async fn delete_server_events_of(&self, server: &str) -> Result<Vec<ServerEvent>> {
        let mut events = self.server_events.lock().await;
        let ids: Vec<String> = events
            .values()
            .filter(|event| event.server == server)
            .map(|event| event.id.clone())
            .collect();
        let removed: Vec<ServerEvent> = ids.iter().filter_map(|id| events.remove(id)).collect();
        drop(events);
        self.server_event_interests
            .lock()
            .await
            .retain(|_, row| row.server != server);
        self.server_event_rosters
            .lock()
            .await
            .retain(|_, roster| roster.server != server);
        Ok(removed)
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

    async fn set_server_event_answer(
        &self,
        event: &str,
        server: &str,
        user: &str,
        answer: ServerEventAnswer,
        waiting: bool,
    ) -> Result<Option<ServerEventInterest>> {
        let now = Timestamp::now_utc();
        let mut rows = self.server_event_interests.lock().await;
        let key = key(event, user);
        let before = rows.get(&key).cloned();
        rows.insert(
            key.clone(),
            ServerEventInterest {
                id: key,
                server: server.to_string(),
                created_at: before.as_ref().map(|row| row.created_at).unwrap_or(now),
                answer,
                waiting,
                updated_at: Some(now),
                muted: before.as_ref().is_some_and(|row| row.muted),
            },
        );
        Ok(before)
    }

    async fn delete_server_event_answer(
        &self,
        event: &str,
        user: &str,
        only: Option<ServerEventAnswer>,
    ) -> Result<Option<ServerEventInterest>> {
        let mut rows = self.server_event_interests.lock().await;
        let key = key(event, user);
        match rows.get(&key) {
            Some(row) if only.is_none_or(|only| row.answer == only) => Ok(rows.remove(&key)),
            _ => Ok(None),
        }
    }

    async fn set_server_event_answer_waiting(
        &self,
        event: &str,
        user: &str,
        waiting: bool,
    ) -> Result<bool> {
        let mut rows = self.server_event_interests.lock().await;
        match rows.get_mut(&key(event, user)) {
            Some(row) if row.answer == ServerEventAnswer::Going => {
                row.waiting = waiting;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn fetch_server_event_answer(
        &self,
        event: &str,
        user: &str,
    ) -> Result<Option<ServerEventInterest>> {
        let rows = self.server_event_interests.lock().await;
        Ok(rows.get(&key(event, user)).cloned())
    }

    async fn fetch_server_event_answers_of(
        &self,
        user: &str,
        events: &[String],
    ) -> Result<Vec<ServerEventInterest>> {
        let rows = self.server_event_interests.lock().await;
        Ok(rows
            .values()
            .filter(|row| row.id.user == user && events.contains(&row.id.event))
            .cloned()
            .collect())
    }

    async fn fetch_server_event_answers_by_user(
        &self,
        user: &str,
        server: Option<&str>,
    ) -> Result<Vec<ServerEventInterest>> {
        let rows = self.server_event_interests.lock().await;
        let mut list: Vec<ServerEventInterest> = rows
            .values()
            .filter(|row| row.id.user == user && server.is_none_or(|server| row.server == server))
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.event.cmp(&b.id.event));
        Ok(list)
    }

    async fn fetch_server_event_answers(
        &self,
        event: &str,
        answers: &[ServerEventAnswer],
        waiting: Option<bool>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ServerEventInterest>> {
        let rows = self.server_event_interests.lock().await;
        let mut list: Vec<ServerEventInterest> = rows
            .values()
            .filter(|row| {
                row.id.event == event
                    && answers.contains(&row.answer)
                    && after.is_none_or(|after| row.id.user.as_str() > after)
                    && waiting.is_none_or(|waiting| {
                        row.answer != ServerEventAnswer::Going || row.waiting == waiting
                    })
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.user.cmp(&b.id.user));
        list.truncate(limit.max(0) as usize);
        Ok(list)
    }

    async fn add_server_event_counts(
        &self,
        event: &str,
        maybe: i32,
        not_going: i32,
    ) -> Result<()> {
        let mut events = self.server_events.lock().await;
        if let Some(stored) = events.get_mut(event) {
            stored.interested_count = stored.interested_count.saturating_add_signed(maybe);
            stored.not_going_count = stored.not_going_count.saturating_add_signed(not_going);
        }
        Ok(())
    }

    async fn set_server_event_seat_counts(
        &self,
        event: &str,
        going: u32,
        waiting: u32,
        version: i64,
    ) -> Result<()> {
        let mut events = self.server_events.lock().await;
        if let Some(stored) = events.get_mut(event) {
            if stored.roster_version < version {
                stored.going_count = going;
                stored.waitlist_count = waiting;
                stored.roster_version = version;
            }
        }
        Ok(())
    }

    async fn recount_server_event(&self, event: &str) -> Result<()> {
        let (mut going, mut waiting, mut maybe, mut not_going) = (0u32, 0u32, 0u32, 0u32);
        for row in self.server_event_interests.lock().await.values() {
            if row.id.event != event {
                continue;
            }
            match row.answer {
                ServerEventAnswer::Going if row.waiting => waiting += 1,
                ServerEventAnswer::Going => going += 1,
                ServerEventAnswer::Maybe => maybe += 1,
                ServerEventAnswer::NotGoing => not_going += 1,
            }
        }
        // the seat list is what decides seats
        let roster = self.server_event_rosters.lock().await.get(event).cloned();
        let mut events = self.server_events.lock().await;
        if let Some(stored) = events.get_mut(event) {
            if let Some(roster) = roster {
                going = roster.going.len() as u32;
                waiting = roster.waiting.len() as u32;
                stored.roster_version = roster.version;
            }
            stored.going_count = going;
            stored.waitlist_count = waiting;
            stored.interested_count = maybe;
            stored.not_going_count = not_going;
        }
        Ok(())
    }

    async fn insert_server_event_roster(&self, roster: &ServerEventRoster) -> Result<()> {
        self.server_event_rosters
            .lock()
            .await
            .entry(roster.id.clone())
            .or_insert_with(|| roster.clone());
        Ok(())
    }

    async fn fetch_server_event_roster(&self, event: &str) -> Result<Option<ServerEventRoster>> {
        Ok(self.server_event_rosters.lock().await.get(event).cloned())
    }

    async fn change_server_event_roster(
        &self,
        event: &str,
        change: &RosterChange,
    ) -> Result<Option<ServerEventRoster>> {
        let mut rosters = self.server_event_rosters.lock().await;
        Ok(rosters.get_mut(event).map(|roster| {
            roster.apply(change);
            roster.clone()
        }))
    }

    async fn set_server_event_presence(&self, id: &str, count: u32) -> Result<()> {
        if let Some(event) = self.server_events.lock().await.get_mut(id) {
            event.present_at_start = Some(count);
        }
        Ok(())
    }

    async fn pull_server_event_person(&self, server: Option<&str>, user: &str) -> Result<()> {
        for event in self.server_events.lock().await.values_mut() {
            if server.is_none_or(|server| event.server == server) {
                event.invited.retain(|id| id != user);
                event.co_organisers.retain(|id| id != user);
            }
        }
        Ok(())
    }

    async fn fetch_server_events_past_retention(
        &self,
        before: Timestamp,
        limit: i64,
    ) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        let mut list: Vec<ServerEvent> = events
            .values()
            .filter(|event| {
                matches!(
                    event.status,
                    ServerEventStatus::Completed | ServerEventStatus::Cancelled
                ) && event.people_removed_at.is_none()
                    && event.ended_at.is_some_and(|ended| ended <= before)
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| a.ended_at.cmp(&b.ended_at).then(a.id.cmp(&b.id)));
        list.truncate(limit.max(0) as usize);
        Ok(list)
    }

    async fn remove_server_event_people(&self, id: &str, at: Timestamp) -> Result<u64> {
        let mut rows = self.server_event_interests.lock().await;
        let before = rows.len();
        rows.retain(|key, _| key.event != id);
        let removed = (before - rows.len()) as u64;
        drop(rows);
        self.server_event_rosters.lock().await.remove(id);
        if let Some(event) = self.server_events.lock().await.get_mut(id) {
            event.invited.clear();
            event.co_organisers.clear();
            event.people_removed_at = Some(at);
        }
        Ok(removed)
    }

    async fn set_server_event_next(&self, id: &str, next: &str) -> Result<()> {
        if let Some(event) = self.server_events.lock().await.get_mut(id) {
            event.next = Some(next.to_string());
        }
        Ok(())
    }

    async fn fetch_server_events_missing_next(&self) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        Ok(events
            .values()
            .filter(|event| {
                matches!(
                    event.status,
                    ServerEventStatus::Completed | ServerEventStatus::Cancelled
                ) && event.repeat.is_some()
                    && event.next.is_none()
            })
            .cloned()
            .collect())
    }

    async fn fetch_server_events_with_reminders(
        &self,
        now: Timestamp,
        until: Timestamp,
    ) -> Result<Vec<ServerEvent>> {
        let events = self.server_events.lock().await;
        let mut list: Vec<ServerEvent> = events
            .values()
            .filter(|event| {
                event.status == ServerEventStatus::Scheduled
                    && event.starts_at > now
                    && event.starts_at <= until
                    && !event.reminders.is_empty()
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| a.starts_at.cmp(&b.starts_at));
        Ok(list)
    }

    async fn mark_server_event_reminder(&self, id: &str, minutes: u32) -> Result<bool> {
        let mut events = self.server_events.lock().await;
        match events.get_mut(id) {
            Some(event)
                if event.status == ServerEventStatus::Scheduled
                    && !event.reminders_sent.contains(&minutes) =>
            {
                event.reminders_sent.push(minutes);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn clear_server_event_reminders_sent(&self, id: &str) -> Result<()> {
        if let Some(event) = self.server_events.lock().await.get_mut(id) {
            event.reminders_sent.clear();
        }
        Ok(())
    }

    async fn set_server_event_answer_muted(
        &self,
        event: &str,
        user: &str,
        muted: bool,
    ) -> Result<bool> {
        let mut rows = self.server_event_interests.lock().await;
        match rows.get_mut(&key(event, user)) {
            Some(row) => {
                row.muted = muted;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn fetch_server_event_muted(&self, event: &str) -> Result<Vec<String>> {
        let rows = self.server_event_interests.lock().await;
        Ok(rows
            .values()
            .filter(|row| row.id.event == event && row.muted)
            .map(|row| row.id.user.clone())
            .collect())
    }

    async fn fetch_server_event_by_link(&self, code: &str) -> Result<Option<ServerEvent>> {
        let events = self.server_events.lock().await;
        Ok(events
            .values()
            .find(|event| event.link.as_ref().is_some_and(|link| link.code == code))
            .cloned())
    }

    async fn fetch_server_event_by_invitation(&self, code: &str) -> Result<Option<ServerEvent>> {
        let events = self.server_events.lock().await;
        Ok(events
            .values()
            .find(|event| event.invitations.iter().any(|link| link.code == code))
            .cloned())
    }

    async fn add_server_event_invitation(
        &self,
        id: &str,
        invitation: &ServerEventInvitation,
        max: usize,
    ) -> Result<bool> {
        let mut events = self.server_events.lock().await;
        match events.get_mut(id) {
            Some(event) if event.invitations.len() < max => {
                event.invitations.push(invitation.clone());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn remove_server_event_invitation(&self, id: &str, code: &str) -> Result<bool> {
        let mut events = self.server_events.lock().await;
        let Some(event) = events.get_mut(id) else {
            return Ok(false);
        };
        let before = event.invitations.len();
        event.invitations.retain(|link| link.code != code);
        Ok(event.invitations.len() != before)
    }

    async fn use_server_event_invitation(
        &self,
        id: &str,
        code: &str,
        max_uses: Option<u32>,
        user: &str,
        max_invited: usize,
    ) -> Result<bool> {
        let mut events = self.server_events.lock().await;
        let Some(event) = events.get_mut(id) else {
            return Ok(false);
        };
        if event.invited.iter().any(|id| id == user) || event.invited.len() >= max_invited {
            return Ok(false);
        }
        let Some(link) = event
            .invitations
            .iter_mut()
            .find(|link| link.code == code && max_uses.is_none_or(|max| link.uses < max))
        else {
            return Ok(false);
        };
        link.uses += 1;
        event.invited.push(user.to_string());
        Ok(true)
    }
}
