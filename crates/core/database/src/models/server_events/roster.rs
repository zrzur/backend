//! The seat list of an event: who has a seat and who waits for one, in the
//! order they came.
//!
//! It is one document per event and every change is one write, so two
//! people can never both take the last seat, nobody loses their place, and a
//! freed seat goes to the first waiting person in the same step. The rules
//! are written once here (`ServerEventRoster::apply`); the reference
//! database runs them as they are, and the MongoDB driver runs the same
//! rules as an update pipeline (tested against this code).

auto_derived!(
    /// Seats and waiting list of one event
    pub struct ServerEventRoster {
        /// Event id
        #[serde(rename = "_id")]
        pub id: String,
        /// Server of the event (to clean up with it)
        pub server: String,
        /// Seats; absent: no limit (up to `MAX_SEATS`)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub capacity: Option<u32>,
        /// Whether people can wait when every seat is taken
        #[serde(default)]
        pub waitlist: bool,
        /// People with a seat, first come first
        #[serde(default)]
        pub going: Vec<String>,
        /// People waiting for a seat, next one first
        #[serde(default)]
        pub waiting: Vec<String>,
        /// People the last change moved from the waiting list to a seat
        #[serde(default)]
        pub promoted: Vec<String>,
        /// Counts every change, so the counts on the event are never
        /// replaced by older ones
        #[serde(default)]
        pub version: i64,
    }

    /// One change to a seat list
    pub enum RosterChange {
        /// A person wants a seat: they get one, or wait, or (no waiting
        /// list, or it is full) nothing changes
        Join(String),
        /// A person gives up their seat or their place in the line; the
        /// first waiting people move up into free seats
        Leave(String),
        /// An organiser gives a waiting person a seat. With `over_capacity`
        /// also when every seat is taken; without it only into a free seat.
        /// The check and the seating are one step.
        Seat {
            user: String,
            over_capacity: bool,
        },
        /// A person is taken off both lists and nobody moves up (an event
        /// that is over: its seats are not handed on any more)
        Drop(String),
        /// New limit and waiting-list switch; waiting people move up into
        /// seats that became free
        Settings {
            capacity: Option<u32>,
            waitlist: bool,
        },
    }
);

/// Most seats an event can have, and most people "going" without a limit
pub const MAX_SEATS: u32 = 10_000;
/// Most people who can wait for a seat
pub const MAX_WAITING: usize = 10_000;

impl ServerEventRoster {
    /// An empty seat list
    pub fn new(event: &str, server: &str, capacity: Option<u32>, waitlist: bool) -> Self {
        ServerEventRoster {
            id: event.to_string(),
            server: server.to_string(),
            capacity,
            waitlist,
            going: vec![],
            waiting: vec![],
            promoted: vec![],
            version: 0,
        }
    }

    /// Seats in all (the limit, never above `MAX_SEATS`)
    pub fn seats(&self) -> usize {
        self.capacity.unwrap_or(MAX_SEATS).min(MAX_SEATS) as usize
    }

    /// Whether this person has a seat
    pub fn is_seated(&self, user: &str) -> bool {
        self.going.iter().any(|id| id == user)
    }

    /// Whether this person waits for a seat
    pub fn is_waiting(&self, user: &str) -> bool {
        self.waiting.iter().any(|id| id == user)
    }

    /// Place on the waiting list, 1 for the next person
    pub fn position(&self, user: &str) -> Option<u32> {
        self.waiting
            .iter()
            .position(|id| id == user)
            .map(|index| index as u32 + 1)
    }

    /// Move waiting people into free seats
    fn settle(&mut self) {
        let free = self.seats().saturating_sub(self.going.len());
        let n = free.min(self.waiting.len());
        self.promoted = self.waiting.drain(..n).collect();
        self.going.extend(self.promoted.iter().cloned());
    }

    /// Make one change
    pub fn apply(&mut self, change: &RosterChange) {
        self.promoted.clear();
        match change {
            RosterChange::Join(user) => {
                if !self.is_seated(user) && !self.is_waiting(user) {
                    if self.going.len() < self.seats() {
                        self.going.push(user.clone());
                    } else if self.waitlist && self.waiting.len() < MAX_WAITING {
                        self.waiting.push(user.clone());
                    }
                }
            }
            RosterChange::Leave(user) => {
                self.going.retain(|id| id != user);
                self.waiting.retain(|id| id != user);
                self.settle();
            }
            RosterChange::Drop(user) => {
                self.going.retain(|id| id != user);
                self.waiting.retain(|id| id != user);
            }
            RosterChange::Seat { user, over_capacity } => {
                if self.is_waiting(user) && (*over_capacity || self.going.len() < self.seats()) {
                    self.waiting.retain(|id| id != user);
                    self.going.push(user.clone());
                    self.promoted.push(user.clone());
                }
            }
            RosterChange::Settings { capacity, waitlist } => {
                self.capacity = *capacity;
                self.waitlist = *waitlist;
                self.settle();
            }
        }
        self.version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(capacity: Option<u32>, waitlist: bool) -> ServerEventRoster {
        ServerEventRoster::new("event", "server", capacity, waitlist)
    }

    fn join(roster: &mut ServerEventRoster, user: &str) {
        roster.apply(&RosterChange::Join(user.to_string()));
    }

    #[test]
    fn seats_fill_in_order_then_people_wait_in_order() {
        let mut r = roster(Some(2), true);
        for user in ["a", "b", "c", "d"] {
            join(&mut r, user);
        }
        assert_eq!(r.going, ["a", "b"]);
        assert_eq!(r.waiting, ["c", "d"]);
        assert_eq!(r.position("d"), Some(2));
        // joining twice changes nothing
        join(&mut r, "a");
        join(&mut r, "c");
        assert_eq!(r.going, ["a", "b"]);
        assert_eq!(r.waiting, ["c", "d"]);
        assert_eq!(r.version, 6);
    }

    #[test]
    fn a_freed_seat_goes_to_the_first_waiting_person() {
        let mut r = roster(Some(2), true);
        for user in ["a", "b", "c", "d"] {
            join(&mut r, user);
        }
        r.apply(&RosterChange::Leave("a".into()));
        assert_eq!(r.going, ["b", "c"]);
        assert_eq!(r.waiting, ["d"]);
        assert_eq!(r.promoted, ["c"]);
        // leaving the line moves nobody up
        r.apply(&RosterChange::Leave("d".into()));
        assert_eq!(r.going, ["b", "c"]);
        assert!(r.waiting.is_empty());
        assert!(r.promoted.is_empty());
    }

    #[test]
    fn without_a_waiting_list_a_full_event_takes_nobody() {
        let mut r = roster(Some(1), false);
        join(&mut r, "a");
        join(&mut r, "b");
        assert_eq!(r.going, ["a"]);
        assert!(r.waiting.is_empty());
        assert!(!r.is_seated("b") && !r.is_waiting("b"));
    }

    #[test]
    fn more_seats_move_people_up_and_fewer_seats_bump_nobody() {
        let mut r = roster(Some(1), true);
        for user in ["a", "b", "c", "d"] {
            join(&mut r, user);
        }
        r.apply(&RosterChange::Settings { capacity: Some(3), waitlist: true });
        assert_eq!(r.going, ["a", "b", "c"]);
        assert_eq!(r.waiting, ["d"]);
        assert_eq!(r.promoted, ["b", "c"]);
        // fewer seats than people seated: they stay, nobody new gets in
        r.apply(&RosterChange::Settings { capacity: Some(1), waitlist: true });
        assert_eq!(r.going, ["a", "b", "c"]);
        r.apply(&RosterChange::Leave("a".into()));
        assert_eq!(r.going, ["b", "c"]);
        assert_eq!(r.waiting, ["d"]);
        // no limit: everyone waiting is seated
        r.apply(&RosterChange::Settings { capacity: None, waitlist: true });
        assert_eq!(r.going, ["b", "c", "d"]);
        assert!(r.waiting.is_empty());
    }

    #[test]
    fn an_organiser_seats_a_waiting_person_beyond_the_limit() {
        let mut r = roster(Some(1), true);
        for user in ["a", "b", "c"] {
            join(&mut r, user);
        }
        // without the organiser's word a full event seats nobody
        r.apply(&RosterChange::Seat { user: "c".into(), over_capacity: false });
        assert_eq!(r.going, ["a"]);
        assert_eq!(r.waiting, ["b", "c"]);
        assert!(r.promoted.is_empty());
        r.apply(&RosterChange::Seat { user: "c".into(), over_capacity: true });
        assert_eq!(r.going, ["a", "c"]);
        assert_eq!(r.waiting, ["b"]);
        assert_eq!(r.promoted, ["c"]);
        // someone who is not waiting is not seated this way
        r.apply(&RosterChange::Seat { user: "z".into(), over_capacity: true });
        assert_eq!(r.going, ["a", "c"]);
        assert!(r.promoted.is_empty());
        // taken off without anyone moving up
        r.apply(&RosterChange::Drop("a".into()));
        assert_eq!(r.going, ["c"]);
        assert_eq!(r.waiting, ["b"]);
        assert!(r.promoted.is_empty());
    }
}
