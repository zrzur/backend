//! Notices of an event that reach a person's phone: reminders before the
//! start, a changed time or place, a called-off event, an invitation, and a
//! seat that came free for someone on the waiting list.
//!
//! One change makes one notice. A reminder goes out once: crond notes it on
//! the event before sending (`mark_server_event_reminder`), so two crond
//! processes, or a crash in between, never send it twice. Every notice of
//! one event carries the same tag, so on a phone a newer one replaces the
//! older one instead of piling up.
//!
//! Who is told: people who are going or maybe going (an invitation: the
//! newly invited people; a seat: its new holder), who are still members and
//! can still see the event. A person can switch the notices of one event
//! off (`muted` on their answer), or a kind of notice off for every event
//! (the synced setting `event_notices`).

use std::collections::HashSet;

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_models::v0;
use revolt_result::Result;

use super::model::{PlaceState, ANNOUNCE_PAGE};
use crate::{Channel, Database, ServerEvent, SystemMessage};

/// Reminders one event can have
pub const MAX_REMINDERS: usize = 3;
/// The shortest reminder: 5 minutes before the start
pub const MIN_REMINDER_MINUTES: u32 = 5;
/// The longest reminder: a week before the start
pub const MAX_REMINDER_MINUTES: u32 = 7 * 24 * 60;
/// A reminder that is this late (crond was down, or the event was made
/// after the reminder's time) is not sent any more
pub const REMINDER_GRACE_MINUTES: i64 = 10;
/// One notice goes to at most this many people
pub const MAX_NOTICE_PEOPLE: usize = 5000;
/// The synced setting with a person's switches: a JSON object, a kind set
/// to `false` is off (`reminders`, `changes`, `invitations`, `seats`)
pub const NOTICE_SETTINGS_KEY: &str = "event_notices";

/// What a notice says
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventNotice {
    /// It starts in this many minutes
    Reminder(u32),
    /// Its time or place changed
    Changed,
    /// It was called off
    Cancelled,
    /// You were invited to it
    Invited,
    /// A seat came free and is yours
    Seated,
}

impl EventNotice {
    /// The switch in a person's settings that turns this kind off
    pub fn switch(&self) -> &'static str {
        match self {
            EventNotice::Reminder(_) => "reminders",
            EventNotice::Changed | EventNotice::Cancelled => "changes",
            EventNotice::Invited => "invitations",
            EventNotice::Seated => "seats",
        }
    }

    /// The line a phone shows
    pub fn body(&self, title: &str) -> String {
        match self {
            EventNotice::Reminder(minutes) => {
                format!("Starts in {}: {title}", reminder_span(*minutes))
            }
            EventNotice::Changed => format!("Time or place changed: {title}"),
            EventNotice::Cancelled => format!("Event called off: {title}"),
            EventNotice::Invited => format!("You are invited: {title}"),
            EventNotice::Seated => format!("A seat is yours: {title}"),
        }
    }
}

/// "15 minutes", "1 hour", "2 days"
fn reminder_span(minutes: u32) -> String {
    let (count, unit) = if minutes % 1440 == 0 {
        (minutes / 1440, "day")
    } else if minutes % 60 == 0 {
        (minutes / 60, "hour")
    } else {
        (minutes, "minute")
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

/// The reminders an organiser asked for, checked: at most `MAX_REMINDERS`,
/// each inside the allowed span, none twice, longest first
pub fn clean_reminders(mut reminders: Vec<u32>) -> std::result::Result<Vec<u32>, &'static str> {
    reminders.sort_unstable_by(|a, b| b.cmp(a));
    reminders.dedup();
    if reminders.len() > MAX_REMINDERS {
        return Err("reminders: at most 3");
    }
    if reminders
        .iter()
        .any(|minutes| !(MIN_REMINDER_MINUTES..=MAX_REMINDER_MINUTES).contains(minutes))
    {
        return Err("reminders: from 5 minutes to a week before the start");
    }
    Ok(reminders)
}

/// Whether a person's settings switch this kind of notice off
pub fn switched_off(settings: Option<&str>, notice: EventNotice) -> bool {
    settings
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| value.get(notice.switch()).and_then(|on| on.as_bool()))
        == Some(false)
}

/// What one reminder pass did
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReminderReport {
    /// Reminders that went out
    pub sent: usize,
    /// Reminders that were too late and were only noted
    pub late: usize,
    /// People told, over all reminders
    pub people: usize,
    pub failed: usize,
}

impl ServerEvent {
    /// The reminders of this event that are due at `now` and did not go
    /// out yet
    pub fn due_reminders(&self, now: Timestamp) -> Vec<u32> {
        self.reminders
            .iter()
            .copied()
            .filter(|minutes| !self.reminders_sent.contains(minutes))
            .filter(|minutes| {
                self.starts_at
                    .checked_sub(Duration::minutes(*minutes as i64))
                    .is_some_and(|due| due <= now)
            })
            .collect()
    }

    /// The channel a notice names: the place, else the channel of the
    /// server's join messages, else the server's first channel
    async fn notice_channel(&self, db: &Database, place: &PlaceState) -> Option<Channel> {
        if let PlaceState::Channel(channel) = place {
            return Some(channel.as_ref().clone());
        }
        let server = db.fetch_server(&self.server).await.ok()?;
        let system = server
            .system_messages
            .as_ref()
            .and_then(|channels| channels.user_joined.clone());
        for id in system.iter().chain(server.channels.iter()) {
            if let Ok(channel) = db.fetch_channel(id).await {
                return Some(channel);
            }
        }
        None
    }

    /// Of the people who may be told: the ones who did not switch this
    /// notice off, for this event or for every event
    async fn wants_notice(
        &self,
        db: &Database,
        people: Vec<String>,
        muted: &HashSet<String>,
        notice: EventNotice,
    ) -> Vec<String> {
        let key = [NOTICE_SETTINGS_KEY.to_string()];
        let mut wanted = Vec::with_capacity(people.len());
        for user in people {
            if muted.contains(&user) {
                continue;
            }
            // no settings at all: nothing is off
            let settings = db.fetch_user_settings(&user, &key).await.unwrap_or_default();
            let text = settings.get(NOTICE_SETTINGS_KEY).map(|(_, text)| text.as_str());
            if !switched_off(text, notice) {
                wanted.push(user);
            }
        }
        wanted
    }

    /// Send one notice. `only`: these people (an invitation, a seat);
    /// absent: everyone who is going or maybe going. Returns who was told.
    pub async fn notify(
        &self,
        db: &Database,
        notice: EventNotice,
        only: Option<Vec<String>>,
    ) -> Result<Vec<String>> {
        let server = db.fetch_server(&self.server).await?;
        if !server.events_enabled {
            return Ok(vec![]);
        }
        let place = self.place_state(db).await;
        let Some(push_channel) = self.notice_channel(db, &place).await else {
            return Ok(vec![]);
        };
        // a called-off event whose channel is gone is still told to the
        // people who were coming: its place reads as an outside one
        let place = match place {
            PlaceState::Gone => PlaceState::External,
            place => place,
        };
        let muted: HashSet<String> = db
            .fetch_server_event_muted(&self.id)
            .await?
            .into_iter()
            .collect();

        let mut recipients = Vec::new();
        match only {
            Some(people) => {
                let seen = self
                    .who_may_be_told(db, &server, &place, &push_channel, &people)
                    .await?;
                recipients = self.wants_notice(db, seen, &muted, notice).await;
            }
            None => {
                let mut after: Option<String> = None;
                loop {
                    let page = db
                        .fetch_server_event_interested(&self.id, after.as_deref(), ANNOUNCE_PAGE)
                        .await?;
                    let Some(last) = page.last().cloned() else {
                        break;
                    };
                    let seen = self
                        .who_may_be_told(db, &server, &place, &push_channel, &page)
                        .await?;
                    recipients.extend(self.wants_notice(db, seen, &muted, notice).await);
                    if (page.len() as i64) < ANNOUNCE_PAGE || recipients.len() >= MAX_NOTICE_PEOPLE
                    {
                        break;
                    }
                    after = Some(last);
                }
            }
        }
        recipients.truncate(MAX_NOTICE_PEOPLE);
        if recipients.is_empty() {
            return Ok(recipients);
        }

        // the payload of a push is a message: the one an event already
        // has, with the line of this notice
        let mut message = SystemMessage::EventStarted {
            id: self.id.clone(),
            title: self.title.clone(),
        }
        .into_message(push_channel.id().to_string());
        message.channel = push_channel.id().to_string();
        let mut notification =
            v0::PushNotification::from(message.into_model(None, None), None, push_channel.into())
                .await;
        notification.body = notice.body(&self.title);
        notification.tag = format!("event-{}", self.id);
        notification.url = format!(
            "{}/server/{}/events/{}",
            config().await.hosts.app,
            self.server,
            self.id
        );
        sent_notices::note(&self.id, notice, &recipients);
        if let Err(error) = crate::amqp::get_amqp()
            .message_sent(recipients.clone(), notification)
            .await
        {
            log::warn!("Could not queue notices of event {}: {error:?}", self.id);
        }
        Ok(recipients)
    }

    /// One crond pass over the reminders: every reminder that is due goes
    /// out once. One event failing never stops the others.
    pub async fn run_reminder_pass(db: &Database, now: Timestamp) -> Result<ReminderReport> {
        let mut report = ReminderReport::default();
        let Some(until) = now.checked_add(Duration::minutes(MAX_REMINDER_MINUTES as i64)) else {
            return Ok(report);
        };
        for event in db.fetch_server_events_with_reminders(now, until).await? {
            for minutes in event.due_reminders(now) {
                let outcome = async {
                    if !db.mark_server_event_reminder(&event.id, minutes).await? {
                        return Ok::<_, revolt_result::Error>(None);
                    }
                    let late = event
                        .starts_at
                        .checked_sub(Duration::minutes(minutes as i64))
                        .and_then(|due| due.checked_add(Duration::minutes(REMINDER_GRACE_MINUTES)))
                        .is_some_and(|limit| limit < now);
                    if late {
                        return Ok(Some(None));
                    }
                    let told = event.notify(db, EventNotice::Reminder(minutes), None).await?;
                    Ok(Some(Some(told.len())))
                }
                .await;
                match outcome {
                    Ok(None) => {}
                    Ok(Some(None)) => report.late += 1,
                    Ok(Some(Some(people))) => {
                        report.sent += 1;
                        report.people += people;
                    }
                    Err(error) => {
                        report.failed += 1;
                        log::error!("Reminder of event {} failed: {error:?}", event.id);
                    }
                }
            }
        }
        Ok(report)
    }
}

/// The notices that went out, kept for tests (a push itself leaves the
/// process through the queue and cannot be read back)
pub mod sent_notices {
    use super::EventNotice;

    #[cfg(any(test, feature = "test-utils"))]
    static SENT: std::sync::Mutex<Vec<(String, EventNotice, Vec<String>)>> =
        std::sync::Mutex::new(Vec::new());

    #[allow(unused_variables)]
    pub(super) fn note(event: &str, notice: EventNotice, people: &[String]) {
        #[cfg(any(test, feature = "test-utils"))]
        SENT.lock()
            .expect("notices")
            .push((event.to_string(), notice, people.to_vec()));
    }

    /// The notices of one event so far, oldest first
    #[cfg(any(test, feature = "test-utils"))]
    pub fn of(event: &str) -> Vec<(EventNotice, Vec<String>)> {
        SENT.lock()
            .expect("notices")
            .iter()
            .filter(|(id, _, _)| id == event)
            .map(|(_, notice, people)| (*notice, people.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_are_checked_and_put_in_order() {
        assert_eq!(clean_reminders(vec![15, 1440, 15]), Ok(vec![1440, 15]));
        assert_eq!(clean_reminders(vec![]), Ok(vec![]));
        assert!(clean_reminders(vec![4]).is_err());
        assert!(clean_reminders(vec![MAX_REMINDER_MINUTES + 1]).is_err());
        assert!(clean_reminders(vec![5, 10, 15, 20]).is_err());
    }

    #[test]
    fn a_person_switches_one_kind_off() {
        let settings = Some(r#"{"reminders":false,"changes":true}"#);
        assert!(switched_off(settings, EventNotice::Reminder(15)));
        assert!(!switched_off(settings, EventNotice::Changed));
        assert!(!switched_off(settings, EventNotice::Invited));
        assert!(!switched_off(None, EventNotice::Reminder(15)));
        assert!(!switched_off(Some("not json"), EventNotice::Reminder(15)));
    }

    #[test]
    fn the_line_names_the_span() {
        assert_eq!(EventNotice::Reminder(15).body("Quiz"), "Starts in 15 minutes: Quiz");
        assert_eq!(EventNotice::Reminder(60).body("Quiz"), "Starts in 1 hour: Quiz");
        assert_eq!(EventNotice::Reminder(2880).body("Quiz"), "Starts in 2 days: Quiz");
    }
}
