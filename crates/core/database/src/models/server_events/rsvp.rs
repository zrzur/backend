//! Taking part in an event: a person's answer (going, maybe, not going),
//! seats and the waiting list.
//!
//! The seat list (`roster.rs`) decides who has a seat; each of its changes
//! is one write. A person's answer row mirrors it for lists and for "my
//! events", and the counts on the event are written from the seat list's
//! version, so an older write never replaces a newer one. After every
//! change of one person the row and the seat list are compared once more
//! (`reconcile`), so two requests of the same person that cross each other
//! still end in one consistent state.

use revolt_models::v0::{self, ServerEventAnswer};
use revolt_result::{create_error, Result};

use crate::{Database, RosterChange, ServerEvent, ServerEventInterest, ServerEventRoster};

impl ServerEventInterest {
    /// The answer as the API shows it to its owner
    pub fn into_rsvp(self, roster: Option<&ServerEventRoster>) -> v0::ServerEventRsvp {
        v0::ServerEventRsvp {
            position: if self.waiting {
                roster.and_then(|roster| roster.position(&self.id.user))
            } else {
                None
            },
            answer: self.answer,
            waiting: self.waiting,
            muted: self.muted,
        }
    }

    /// Whether the person takes part (going or maybe)
    pub fn takes_part(&self) -> bool {
        self.answer != ServerEventAnswer::NotGoing
    }
}

impl ServerEvent {
    /// The seat list of the event; an event from before seat lists gets an
    /// empty one with the event's own settings
    pub async fn roster(&self, db: &Database) -> Result<ServerEventRoster> {
        if let Some(roster) = db.fetch_server_event_roster(&self.id).await? {
            return Ok(roster);
        }
        // the retention removed the seat list: it is not written again
        if self.people_removed_at.is_some() {
            return Ok(ServerEventRoster::new(
                &self.id,
                &self.server,
                self.capacity,
                self.waitlist,
            ));
        }
        db.insert_server_event_roster(&ServerEventRoster::new(
            &self.id,
            &self.server,
            self.capacity,
            self.waitlist,
        ))
        .await?;
        db.fetch_server_event_roster(&self.id)
            .await?
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Make one change to the seat list, then bring the rest in line: the
    /// counts on the event, and the rows and devices of people who moved up
    /// from the waiting list
    async fn change_roster(
        &self,
        db: &Database,
        change: RosterChange,
        tell: bool,
    ) -> Result<ServerEventRoster> {
        let roster = match db.change_server_event_roster(&self.id, &change).await? {
            Some(roster) => roster,
            None => {
                // an event from before seat lists
                self.roster(db).await?;
                db.change_server_event_roster(&self.id, &change)
                    .await?
                    .ok_or_else(|| create_error!(NotFound))?
            }
        };
        db.set_server_event_seat_counts(
            &self.id,
            roster.going.len() as u32,
            roster.waiting.len() as u32,
            roster.version,
        )
        .await?;
        for user in &roster.promoted {
            if db
                .set_server_event_answer_waiting(&self.id, user, false)
                .await?
                && tell
            {
                let muted = db
                    .fetch_server_event_answer(&self.id, user)
                    .await?
                    .is_some_and(|row| row.muted);
                self.publish_own_answer(
                    user,
                    Some(v0::ServerEventRsvp {
                        answer: ServerEventAnswer::Going,
                        waiting: false,
                        position: None,
                        muted,
                    }),
                )
                .await;
                // a seat came free for them: they hear it on their phone too
                if let Err(error) = self
                    .notify(db, crate::EventNotice::Seated, Some(vec![user.clone()]))
                    .await
                {
                    log::warn!("Could not tell {user} about their seat: {error:?}");
                }
            }
        }
        Ok(roster)
    }

    /// Read the counts again (they change on their own)
    async fn refresh_counts(&mut self, db: &Database) -> Result<()> {
        let stored = db.fetch_server_event(&self.id).await?;
        self.interested_count = stored.interested_count;
        self.going_count = stored.going_count;
        self.not_going_count = stored.not_going_count;
        self.waitlist_count = stored.waitlist_count;
        self.roster_version = stored.roster_version;
        Ok(())
    }

    /// Compare a person's row with the seat list and make them agree. The
    /// seat list wins for seats; an answer other than "going" wins over a
    /// seat. Returns the row as it is after.
    async fn reconcile(&self, db: &Database, user: &str) -> Result<Option<ServerEventInterest>> {
        // a few rounds at most: each round only fixes what it finds
        for _ in 0..3 {
            let row = db.fetch_server_event_answer(&self.id, user).await?;
            let roster = self.roster(db).await?;
            let (seated, waiting) = (roster.is_seated(user), roster.is_waiting(user));
            match &row {
                Some(row) if row.answer == ServerEventAnswer::Going => {
                    if !seated && !waiting {
                        // the seat went in a crossing request: no answer is
                        // left, the person can answer again
                        db.delete_server_event_answer(
                            &self.id,
                            user,
                            Some(ServerEventAnswer::Going),
                        )
                        .await?;
                    } else if row.waiting != waiting {
                        db.set_server_event_answer_waiting(&self.id, user, waiting)
                            .await?;
                    } else {
                        return Ok(Some(row.clone()));
                    }
                }
                _ if seated || waiting => {
                    self.change_roster(db, RosterChange::Leave(user.to_string()), true)
                        .await?;
                }
                _ => return Ok(row),
            }
        }
        db.fetch_server_event_answer(&self.id, user).await
    }

    /// Write a person's answer. "Going" takes a seat, or a place on the
    /// waiting list when every seat is taken; `ServerEventFull` when there
    /// is neither. Returns the person's row as it is after.
    pub async fn answer(
        &mut self,
        db: &Database,
        user: &str,
        answer: ServerEventAnswer,
    ) -> Result<Option<ServerEventInterest>> {
        self.set_answer(db, user, answer, true).await?;
        let row = self.reconcile(db, user).await?;
        self.refresh_counts(db).await?;
        Ok(row)
    }

    async fn set_answer(
        &self,
        db: &Database,
        user: &str,
        answer: ServerEventAnswer,
        tell: bool,
    ) -> Result<()> {
        let count = |answer: &ServerEventAnswer, by: i32| match answer {
            ServerEventAnswer::Maybe => (by, 0),
            ServerEventAnswer::NotGoing => (0, by),
            ServerEventAnswer::Going => (0, 0),
        };
        let (mut maybe, mut not_going) = count(&answer, 1);
        let before = if answer == ServerEventAnswer::Going {
            let roster = self
                .change_roster(db, RosterChange::Join(user.to_string()), tell)
                .await?;
            let waiting = roster.is_waiting(user);
            if !waiting && !roster.is_seated(user) {
                return Err(create_error!(ServerEventFull));
            }
            db.set_server_event_answer(&self.id, &self.server, user, answer, waiting)
                .await?
        } else {
            let before = db
                .set_server_event_answer(&self.id, &self.server, user, answer, false)
                .await?;
            if before
                .as_ref()
                .is_some_and(|row| row.answer == ServerEventAnswer::Going)
            {
                self.change_roster(db, RosterChange::Leave(user.to_string()), tell)
                    .await?;
            }
            before
        };
        if let Some(before) = before {
            let (m, n) = count(&before.answer, -1);
            maybe += m;
            not_going += n;
        }
        db.add_server_event_counts(&self.id, maybe, not_going).await
    }

    /// Remove a person's answer; a seat they held goes to the next waiting
    /// person. Returns false when they had no answer.
    pub async fn withdraw(&mut self, db: &Database, user: &str) -> Result<bool> {
        let Some(before) = db.delete_server_event_answer(&self.id, user, None).await? else {
            // no row, but make sure no seat is left behind either
            self.reconcile(db, user).await?;
            return Ok(false);
        };
        match before.answer {
            ServerEventAnswer::Going => {
                self.change_roster(db, RosterChange::Leave(user.to_string()), true)
                    .await?;
            }
            ServerEventAnswer::Maybe => db.add_server_event_counts(&self.id, -1, 0).await?,
            ServerEventAnswer::NotGoing => db.add_server_event_counts(&self.id, 0, -1).await?,
        }
        self.reconcile(db, user).await?;
        self.refresh_counts(db).await?;
        Ok(true)
    }

    /// Remove a person's answer from an event that is over (they left the
    /// server or deleted their account). Nobody moves up into their seat,
    /// nobody is told, and the counts of the past event stay as they were.
    /// Returns false when they had no answer.
    pub async fn forget_answer(&self, db: &Database, user: &str) -> Result<bool> {
        let had = db
            .delete_server_event_answer(&self.id, user, None)
            .await?
            .is_some();
        if self.people_removed_at.is_none() {
            // a seat list that is not there stays not there
            db.change_server_event_roster(&self.id, &RosterChange::Drop(user.to_string()))
                .await?;
        }
        Ok(had)
    }

    /// An organiser gives a waiting person a seat: into a free seat, or
    /// with `over_capacity` also beyond the limit. The check is part of the
    /// same write as the seating. Returns false when nothing changed (the
    /// person is not waiting, or every seat is taken and `over_capacity`
    /// was not given).
    pub async fn seat(&mut self, db: &Database, user: &str, over_capacity: bool) -> Result<bool> {
        let roster = self
            .change_roster(
                db,
                RosterChange::Seat {
                    user: user.to_string(),
                    over_capacity,
                },
                true,
            )
            .await?;
        let seated = roster.promoted.iter().any(|id| id == user);
        self.refresh_counts(db).await?;
        Ok(seated)
    }

    /// Apply the event's seat limit and waiting-list switch to its seat
    /// list. More seats (or no limit) move waiting people up at once.
    pub async fn apply_seat_settings(&mut self, db: &Database) -> Result<()> {
        self.change_roster(
            db,
            RosterChange::Settings {
                capacity: self.capacity,
                waitlist: self.waitlist,
            },
            true,
        )
        .await?;
        self.refresh_counts(db).await
    }

    /// A new occurrence of a series takes over the people of the one
    /// before who are still members: first those with a seat, then those
    /// waiting, each in their order, then the "maybe" answers
    pub(super) async fn carry_answers_from(
        &mut self,
        previous: &ServerEvent,
        db: &Database,
    ) -> Result<()> {
        db.insert_server_event_roster(&ServerEventRoster::new(
            &self.id,
            &self.server,
            self.capacity,
            self.waitlist,
        ))
        .await?;
        let roster = db.fetch_server_event_roster(&previous.id).await?;
        let going: Vec<String> = roster
            .map(|roster| roster.going.into_iter().chain(roster.waiting).collect())
            .unwrap_or_default();
        let mut maybe = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = db
                .fetch_server_event_answers(
                    &previous.id,
                    &[ServerEventAnswer::Maybe],
                    None,
                    after.as_deref(),
                    CARRY_PAGE,
                )
                .await?;
            let full = page.len() as i64 == CARRY_PAGE;
            after = page.last().map(|row| row.id.user.clone());
            maybe.extend(page.into_iter().map(|row| row.id.user));
            if !full {
                break;
            }
        }
        let server = db.fetch_server(&self.server).await?;
        let place = self.place_state(db).await;
        for (users, answer) in [
            (going, ServerEventAnswer::Going),
            (maybe, ServerEventAnswer::Maybe),
        ] {
            for chunk in users.chunks(CARRY_PAGE as usize) {
                let members = db.fetch_members(&self.server, chunk).await?;
                let accounts = db.fetch_users(chunk).await?;
                // in the order of the list, not of the members query
                for user in chunk {
                    // only people who are still members and may still see
                    // the event are taken along
                    let (Some(member), Some(account)) = (
                        members.iter().find(|member| &member.id.user == user),
                        accounts.iter().find(|account| &account.id == user),
                    ) else {
                        continue;
                    };
                    let manager =
                        ServerEvent::can_manage_events(db, account, &server, member).await;
                    if !self
                        .visible_given(db, account, &server, member, manager, &place)
                        .await
                    {
                        continue;
                    }
                    match self.set_answer(db, user, answer, false).await {
                        Ok(()) => {}
                        // no seat and no waiting list: not carried over
                        Err(error)
                            if matches!(
                                error.error_type,
                                revolt_result::ErrorType::ServerEventFull
                            ) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        self.refresh_counts(db).await
    }
}

/// People read at once when answers are carried to a new occurrence
const CARRY_PAGE: i64 = 500;
