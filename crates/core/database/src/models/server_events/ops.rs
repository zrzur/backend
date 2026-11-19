use iso8601_timestamp::Timestamp;
use revolt_models::v0::{ServerEventAnswer, ServerEventStatus};
use revolt_result::Result;

use crate::{
    RosterChange, ServerEvent, ServerEventInterest, ServerEventInvitation, ServerEventRoster,
};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractServerEvents: Sync + Send {
    /// Insert a new event
    async fn insert_server_event(&self, event: &ServerEvent) -> Result<()>;

    /// Fetch an event by its id
    async fn fetch_server_event(&self, id: &str) -> Result<ServerEvent>;

    /// Scheduled and active events of a server, soonest first
    async fn fetch_upcoming_server_events(&self, server: &str) -> Result<Vec<ServerEvent>>;

    /// Draft events of a server, soonest first
    async fn fetch_server_event_drafts(&self, server: &str) -> Result<Vec<ServerEvent>>;

    /// Events of these servers that start in `from..to`, whatever their
    /// status except drafts, soonest first
    async fn fetch_server_events_between(
        &self,
        servers: &[String],
        from: Timestamp,
        to: Timestamp,
        limit: i64,
    ) -> Result<Vec<ServerEvent>>;

    /// Events a person created (for the copy of their data)
    async fn fetch_server_events_created_by(&self, user: &str) -> Result<Vec<ServerEvent>>;

    /// Events that name a person as invited or as a helper (for the copy
    /// of their data)
    async fn fetch_server_events_naming(&self, user: &str) -> Result<Vec<ServerEvent>>;

    /// Write the fields an organiser edits (never the status or the counts,
    /// which change on their own)
    async fn update_server_event(&self, event: &ServerEvent) -> Result<()>;

    /// Move an event from one status to another in one step, noting when it
    /// started or ended. Returns false when it was not in `from` any more.
    async fn set_server_event_status(
        &self,
        id: &str,
        from: ServerEventStatus,
        to: ServerEventStatus,
        at: Timestamp,
    ) -> Result<bool>;

    /// Delete an event, its answers and its seat list
    async fn delete_server_event(&self, id: &str) -> Result<()>;

    /// Delete every event of a server with its answers and seat lists.
    /// Returns the events (their cover files are released by the caller).
    async fn delete_server_events_of(&self, server: &str) -> Result<Vec<ServerEvent>>;

    /// Scheduled events whose start time has come
    async fn fetch_server_events_to_start(&self, now: Timestamp) -> Result<Vec<ServerEvent>>;

    /// Active events (crond checks which of them have ended)
    async fn fetch_active_server_events(&self) -> Result<Vec<ServerEvent>>;

    /// Write a person's answer (one row per event and person, so a second
    /// answer replaces the first). Returns the row as it was before.
    async fn set_server_event_answer(
        &self,
        event: &str,
        server: &str,
        user: &str,
        answer: ServerEventAnswer,
        waiting: bool,
    ) -> Result<Option<ServerEventInterest>>;

    /// Remove a person's answer. Returns the row that was removed. With
    /// `only`, the row goes only while it holds that answer.
    async fn delete_server_event_answer(
        &self,
        event: &str,
        user: &str,
        only: Option<ServerEventAnswer>,
    ) -> Result<Option<ServerEventInterest>>;

    /// Note whether a person who answered "going" waits for a seat. Returns
    /// false when they have no "going" answer.
    async fn set_server_event_answer_waiting(
        &self,
        event: &str,
        user: &str,
        waiting: bool,
    ) -> Result<bool>;

    /// A person's answer to an event
    async fn fetch_server_event_answer(
        &self,
        event: &str,
        user: &str,
    ) -> Result<Option<ServerEventInterest>>;

    /// A person's answers to these events
    async fn fetch_server_event_answers_of(
        &self,
        user: &str,
        events: &[String],
    ) -> Result<Vec<ServerEventInterest>>;

    /// Every answer of a person; with `server`, only in that server
    async fn fetch_server_event_answers_by_user(
        &self,
        user: &str,
        server: Option<&str>,
    ) -> Result<Vec<ServerEventInterest>>;

    /// Answers to an event, by user id, after `after`. `waiting` narrows
    /// "going" answers to the seated (false) or the waiting (true).
    async fn fetch_server_event_answers(
        &self,
        event: &str,
        answers: &[ServerEventAnswer],
        waiting: Option<bool>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ServerEventInterest>>;

    /// User ids that take part in an event (going with a seat, or maybe),
    /// by id, after `after`: the people `interested_count` counts
    async fn fetch_server_event_interested(
        &self,
        event: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>> {
        Ok(self
            .fetch_server_event_answers(
                event,
                &[ServerEventAnswer::Going, ServerEventAnswer::Maybe],
                Some(false),
                after,
                limit,
            )
            .await?
            .into_iter()
            .map(|row| row.id.user)
            .collect())
    }

    /// Change the "maybe" and "not going" counts of an event by these steps
    async fn add_server_event_counts(&self, event: &str, maybe: i32, not_going: i32)
        -> Result<()>;

    /// Write the seated and waiting counts that a seat list of `version`
    /// has. An older version never replaces a newer one.
    async fn set_server_event_seat_counts(
        &self,
        event: &str,
        going: u32,
        waiting: u32,
        version: i64,
    ) -> Result<()>;

    /// Set every count of an event from its answers and its seat list
    async fn recount_server_event(&self, event: &str) -> Result<()>;

    /// Write the seat list of a new event. An event that has one keeps it.
    async fn insert_server_event_roster(&self, roster: &ServerEventRoster) -> Result<()>;

    /// The seat list of an event
    async fn fetch_server_event_roster(&self, event: &str) -> Result<Option<ServerEventRoster>>;

    /// Change a seat list in one step and return it as it is after. Two
    /// changes at once never both take the last seat, and a freed seat goes
    /// to the first waiting person in the same step. None: no seat list.
    async fn change_server_event_roster(
        &self,
        event: &str,
        change: &RosterChange,
    ) -> Result<Option<ServerEventRoster>>;

    /// Note how many people were in the voice channel at the start
    async fn set_server_event_presence(&self, id: &str, count: u32) -> Result<()>;

    /// Take a person off the invited and co-organiser lists of every event
    /// (with `server`: of that server's events)
    async fn pull_server_event_person(&self, server: Option<&str>, user: &str) -> Result<()>;

    /// Completed and cancelled events that ended at or before `before` and
    /// still have their people, oldest first
    async fn fetch_server_events_past_retention(
        &self,
        before: Timestamp,
        limit: i64,
    ) -> Result<Vec<ServerEvent>>;

    /// Remove who answered an event, its seat list, its invited list and
    /// its helpers, and note when. The counts stay. Returns how many
    /// answers were removed.
    async fn remove_server_event_people(&self, id: &str, at: Timestamp) -> Result<u64>;

    /// Note the occurrence that follows an event
    async fn set_server_event_next(&self, id: &str, next: &str) -> Result<()>;

    /// Repeating events that are over (completed or cancelled on their own)
    /// and have no next occurrence yet
    async fn fetch_server_events_missing_next(&self) -> Result<Vec<ServerEvent>>;

    /// Scheduled events that have reminders and start after `now`, up to
    /// `until`
    async fn fetch_server_events_with_reminders(
        &self,
        now: Timestamp,
        until: Timestamp,
    ) -> Result<Vec<ServerEvent>>;

    /// Note that a reminder goes out. False when it was noted before (it
    /// went out already) or the event is no longer scheduled.
    async fn mark_server_event_reminder(&self, id: &str, minutes: u32) -> Result<bool>;

    /// The start moved: every reminder can go out again
    async fn clear_server_event_reminders_sent(&self, id: &str) -> Result<()>;

    /// Switch a person's notices of an event off or on. False when they
    /// have no answer to it.
    async fn set_server_event_answer_muted(
        &self,
        event: &str,
        user: &str,
        muted: bool,
    ) -> Result<bool>;

    /// Who switched the notices of an event off
    async fn fetch_server_event_muted(&self, event: &str) -> Result<Vec<String>>;

    /// The event a share link code belongs to
    async fn fetch_server_event_by_link(&self, code: &str) -> Result<Option<ServerEvent>>;

    /// The event an invitation link belongs to
    async fn fetch_server_event_by_invitation(&self, code: &str) -> Result<Option<ServerEvent>>;

    /// Add an invitation link to an event (at most `max` of them). False
    /// when it has that many.
    async fn add_server_event_invitation(
        &self,
        id: &str,
        invitation: &ServerEventInvitation,
        max: usize,
    ) -> Result<bool>;

    /// Take an invitation link away. False when there was none.
    async fn remove_server_event_invitation(&self, id: &str, code: &str) -> Result<bool>;

    /// Use an invitation link once: the person joins the invited people (at
    /// most `max_invited`) and the use is counted, in one write. False when
    /// the link is used up (`max_uses`, which never changes for a link), the
    /// list is full or the person is on it already.
    async fn use_server_event_invitation(
        &self,
        id: &str,
        code: &str,
        max_uses: Option<u32>,
        user: &str,
        max_invited: usize,
    ) -> Result<bool>;
}
