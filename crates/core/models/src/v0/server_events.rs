//! Scheduled server events: something happening in a server at a set time,
//! in one of its voice or text channels or at a place outside Zrzur.

use iso8601_timestamp::Timestamp;

#[cfg(feature = "validator")]
use validator::Validate;

use super::{File, User};

auto_derived!(
    /// Where an event takes place
    #[cfg_attr(feature = "serde", serde(tag = "type"))]
    pub enum ServerEventPlace {
        /// A voice channel of the server
        Voice { channel: String },
        /// A text channel of the server
        Text { channel: String },
        /// Somewhere outside Zrzur: an address or a link (1 to 100 characters)
        External { location: String },
    }

    /// Where an event stands
    pub enum ServerEventStatus {
        /// Waiting for its start time
        Scheduled,
        /// Happening now
        Active,
        /// Over
        Completed,
        /// Called off before or while it ran
        Cancelled,
        /// Not published yet: only its organisers see it and nothing starts it
        Draft,
    }

    /// How an event repeats. The next occurrence is made when one completes.
    pub enum ServerEventRepeat {
        Weekly,
        EveryTwoWeeks,
        Monthly,
        Daily,
    }

    /// Who may see an event, on top of being a member of the server and (for
    /// a channel place) seeing that channel
    pub enum ServerEventVisibility {
        /// Everyone who can see the place
        Place,
        /// Only members with one of `visible_roles`
        Roles,
        /// Only the invited people
        Invited,
    }

    /// Who may read the list of people taking part
    pub enum ServerEventAttendeeList {
        /// Only the organisers
        Organisers,
        /// People who answered "going" or "maybe", and the organisers
        Participants,
        /// Everyone who can see the event
        Everyone,
    }

    /// A person's answer to an event
    #[derive(Copy, Hash)]
    pub enum ServerEventAnswer {
        Going,
        Maybe,
        NotGoing,
    }

    /// How many people gave each answer
    #[derive(Default, Copy)]
    pub struct ServerEventCounts {
        /// Going, with a seat
        pub going: u32,
        /// Maybe
        pub maybe: u32,
        /// Not going
        pub not_going: u32,
        /// Going, on the waiting list
        pub waiting: u32,
    }

    /// Your own answer to an event
    pub struct ServerEventRsvp {
        pub answer: ServerEventAnswer,
        /// Going, but on the waiting list until a seat frees
        #[cfg_attr(feature = "serde", serde(default))]
        pub waiting: bool,
        /// Your place on the waiting list (1 is next)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub position: Option<u32>,
        /// You switched the notices of this event off (reminders, changes)
        #[cfg_attr(feature = "serde", serde(default))]
        pub muted: bool,
    }

    /// The share link of an event: `<invite host>/e/<name>-<code>`, or
    /// `/e/<code>` without a name. The code alone finds the event, so two
    /// events never share a link whatever their names are.
    pub struct ServerEventLink {
        /// Unique code
        pub code: String,
        /// The name the organiser chose (lower-case letters, digits, `-`)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub name: Option<String>,
    }

    /// Your answer to one of several listed events
    pub struct ServerEventOwnAnswer {
        /// Event id
        pub event: String,
        pub answer: ServerEventAnswer,
        /// Going, but on the waiting list
        #[cfg_attr(feature = "serde", serde(default))]
        pub waiting: bool,
    }

    /// A scheduled server event
    pub struct ServerEvent {
        /// Unique id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Server it belongs to
        pub server: String,
        /// Who created it
        pub creator: String,
        /// Title (1 to 100 characters)
        pub title: String,
        /// Description in Markdown (up to 1000 characters)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub description: Option<String>,
        /// When it starts
        pub starts_at: Timestamp,
        /// When it ends, if set
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub ends_at: Option<Timestamp>,
        /// Where it takes place
        pub place: ServerEventPlace,
        /// Cover image
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub cover: Option<File>,
        /// Where it stands
        pub status: ServerEventStatus,
        /// How it repeats
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub repeat: Option<ServerEventRepeat>,
        /// How many members take part: going (seated) plus maybe. The older
        /// "Interested" count; `counts` has each answer
        pub interested_count: u32,
        /// The event this one follows (repeating events)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub previous: Option<String>,
        /// The creator's IANA time zone; repeats keep its wall-clock time
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub time_zone: Option<String>,
        /// How many people gave each answer
        #[cfg_attr(feature = "serde", serde(default))]
        pub counts: ServerEventCounts,
        /// Seats; absent: no limit
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub capacity: Option<u32>,
        /// Whether people can wait for a seat when all are taken
        #[cfg_attr(feature = "serde", serde(default))]
        pub waitlist: bool,
        /// More people are seated than the event has seats (an organiser
        /// seated someone beyond the limit, or lowered the limit)
        #[cfg_attr(feature = "serde", serde(default))]
        pub over_capacity: bool,
        /// Who may see it
        #[cfg_attr(feature = "serde", serde(default))]
        pub visibility: ServerEventVisibility,
        /// Roles that see it (visibility `Roles`)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub visible_roles: Vec<String>,
        /// Who may read the list of people taking part
        #[cfg_attr(feature = "serde", serde(default))]
        pub attendee_list: ServerEventAttendeeList,
        /// Helpers who manage this event beside the people with Manage Events
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub co_organisers: Vec<String>,
        /// The invited people (visibility `Invited`). Only organisers get
        /// this, and only from the REST routes, never over the gateway
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub invited: Option<Vec<String>>,
        /// When it really started
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub started_at: Option<Timestamp>,
        /// When it really ended
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub ended_at: Option<Timestamp>,
        /// Set once the event's retention time ran out (12 months after
        /// it ended): who answered is no longer kept, only the counts
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub people_removed_at: Option<Timestamp>,
        /// Reminders, in minutes before the start (up to 3)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub reminders: Vec<u32>,
        /// Its share link, once an organiser made one
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub link: Option<ServerEventLink>,
        /// People who are not in the server see the title, the time, the
        /// cover and the server's name through the link. Off unless an
        /// organiser switches it on; nobody's answer is ever shown there.
        #[cfg_attr(feature = "serde", serde(default))]
        pub link_public: bool,
    }

    /// New event
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateServerEvent {
        /// Title
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 100)))]
        pub title: String,
        /// Description in Markdown
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1000)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub description: Option<String>,
        /// When it starts (not in the past)
        pub starts_at: Timestamp,
        /// When it ends (after the start)
        #[cfg_attr(feature = "serde", serde(default))]
        pub ends_at: Option<Timestamp>,
        /// Where it takes place
        pub place: ServerEventPlace,
        /// Id of an uploaded cover image (tag `banners`)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub cover: Option<String>,
        /// How it repeats
        #[cfg_attr(feature = "serde", serde(default))]
        pub repeat: Option<ServerEventRepeat>,
        /// Your IANA time zone (e.g. `Europe/Vienna`): repeats keep the
        /// same local time across daylight saving changes
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub time_zone: Option<String>,
        /// Save it as a draft: only organisers see it and nothing starts it
        /// until it is published
        #[cfg_attr(feature = "serde", serde(default))]
        pub draft: Option<bool>,
        /// Who may see it (default: everyone who can see the place)
        #[cfg_attr(feature = "serde", serde(default))]
        pub visibility: Option<ServerEventVisibility>,
        /// Roles that see it (visibility `Roles`, 1 to 20)
        #[cfg_attr(feature = "serde", serde(default))]
        pub visible_roles: Option<Vec<String>>,
        /// Members who are invited (visibility `Invited`, up to 200)
        #[cfg_attr(feature = "serde", serde(default))]
        pub invited: Option<Vec<String>>,
        /// Who may read the list of people taking part (default: organisers)
        /// An event stored before this setting existed keeps its open list.
        #[cfg_attr(feature = "serde", serde(default))]
        pub attendee_list: Option<ServerEventAttendeeList>,
        /// Seats (1 to 10000); absent: no limit
        #[cfg_attr(feature = "serde", serde(default))]
        pub capacity: Option<u32>,
        /// Let people wait for a seat when all are taken (default: yes)
        #[cfg_attr(feature = "serde", serde(default))]
        pub waitlist: Option<bool>,
        /// Members who help manage this event (up to 10)
        #[cfg_attr(feature = "serde", serde(default))]
        pub co_organisers: Option<Vec<String>>,
        /// Reminders, in minutes before the start (up to 3, each from 5
        /// minutes to a week). People who are going or maybe going get them
        #[cfg_attr(feature = "serde", serde(default))]
        pub reminders: Option<Vec<u32>>,
    }

    /// Calling off one occurrence or the whole series
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsCancelServerEvent {
        /// Stop the whole series (default: only this occurrence; the next
        /// one is scheduled)
        pub series: Option<bool>,
    }

    /// Deleting one occurrence or the whole series
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsDeleteServerEvent {
        /// Delete the whole series: this occurrence goes and nothing next is
        /// scheduled (default: only this occurrence; the next one is
        /// scheduled)
        pub series: Option<bool>,
    }

    /// Optional fields of an event that can be cleared
    pub enum FieldsServerEvent {
        Description,
        EndsAt,
        Cover,
        Repeat,
        /// No seat limit any more: everyone waiting gets a seat
        Capacity,
    }

    /// Changes to an event
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[derive(Default)]
    pub struct DataEditServerEvent {
        /// Title
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 100)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub title: Option<String>,
        /// Description in Markdown
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1000)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub description: Option<String>,
        /// When it starts (only while scheduled)
        #[cfg_attr(feature = "serde", serde(default))]
        pub starts_at: Option<Timestamp>,
        /// When it ends
        #[cfg_attr(feature = "serde", serde(default))]
        pub ends_at: Option<Timestamp>,
        /// Where it takes place
        #[cfg_attr(feature = "serde", serde(default))]
        pub place: Option<ServerEventPlace>,
        /// Id of an uploaded cover image (tag `banners`)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub cover: Option<String>,
        /// How it repeats
        #[cfg_attr(feature = "serde", serde(default))]
        pub repeat: Option<ServerEventRepeat>,
        /// Who may see it
        #[cfg_attr(feature = "serde", serde(default))]
        pub visibility: Option<ServerEventVisibility>,
        /// Roles that see it (visibility `Roles`, 1 to 20)
        #[cfg_attr(feature = "serde", serde(default))]
        pub visible_roles: Option<Vec<String>>,
        /// Members who are invited (visibility `Invited`, up to 200)
        #[cfg_attr(feature = "serde", serde(default))]
        pub invited: Option<Vec<String>>,
        /// Who may read the list of people taking part
        #[cfg_attr(feature = "serde", serde(default))]
        pub attendee_list: Option<ServerEventAttendeeList>,
        /// Seats (1 to 10000); absent: no limit
        #[cfg_attr(feature = "serde", serde(default))]
        pub capacity: Option<u32>,
        /// Let people wait for a seat when all are taken
        #[cfg_attr(feature = "serde", serde(default))]
        pub waitlist: Option<bool>,
        /// Members who help manage this event (up to 10)
        #[cfg_attr(feature = "serde", serde(default))]
        pub co_organisers: Option<Vec<String>>,
        /// Reminders, in minutes before the start (up to 3, each from 5
        /// minutes to a week). People who are going or maybe going get them
        #[cfg_attr(feature = "serde", serde(default))]
        pub reminders: Option<Vec<u32>>,
        /// Fields to clear
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 5)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsServerEvent>,
    }

    /// The events of a server you can see, with the ones you marked
    pub struct ServerEventList {
        /// Scheduled and active events, soonest first
        pub events: Vec<ServerEvent>,
        /// Ids of the listed events you take part in (going or maybe)
        pub interested: Vec<String>,
        /// Your answers to the listed events
        #[cfg_attr(feature = "serde", serde(default))]
        pub answers: Vec<ServerEventOwnAnswer>,
    }

    /// One event and your answer to it
    pub struct ServerEventResponse {
        pub event: ServerEvent,
        /// Whether you take part (going or maybe)
        pub interested: bool,
        /// Your answer, if you gave one
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub rsvp: Option<ServerEventRsvp>,
        /// Whether you may manage this event
        #[cfg_attr(feature = "serde", serde(default))]
        pub organiser: bool,
    }

    /// A page of the members who marked an event "Interested"
    pub struct ServerEventInterestedPage {
        /// Users, by id
        pub users: Vec<User>,
        /// Pass as `after` for the next page; absent on the last page
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub next: Option<String>,
    }

    /// Paging of the interested list
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsServerEventInterested {
        /// Start after this user id
        #[cfg_attr(feature = "validator", validate(length(min = 26, max = 26)))]
        pub after: Option<String>,
        /// Page size (1 to 100, default 50)
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 100)))]
        pub limit: Option<i64>,
    }
);

auto_derived!(
    /// Which events to list
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsServerEventList {
        /// Also your drafts (organisers only)
        pub drafts: Option<bool>,
        /// Events that start at or after this time (ISO 8601). With `from`
        /// or `to` the list also holds past and cancelled events
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 40)))]
        pub from: Option<String>,
        /// Events that start before this time (ISO 8601)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 40)))]
        pub to: Option<String>,
    }

    /// Seating a waiting person
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsSeatServerEventAttendee {
        /// Seat them although every seat is taken. Without it the request
        /// is refused with `ServerEventOverCapacity` when the event is full.
        pub over_capacity: Option<bool>,
    }

    /// Your answer
    pub struct DataServerEventRsvp {
        pub answer: ServerEventAnswer,
    }

    /// Your notices of one event
    pub struct DataServerEventNotices {
        /// No reminders and no "changed" or "called off" notices of this
        /// event for you
        pub muted: bool,
    }

    /// The share link of an event
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataServerEventLink {
        /// A name in the link (1 to 40 characters; kept as lower-case
        /// letters, digits and `-`). Absent: the name stays as it is; empty:
        /// no name
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 40)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub name: Option<String>,
        /// Let people outside the server see the title, the time, the cover
        /// and the server's name through the link
        #[cfg_attr(feature = "serde", serde(default))]
        pub public: Option<bool>,
    }

    /// What a share link shows. Someone outside the server gets the first
    /// fields only, and only when the organiser opened the link; a member
    /// who can see the event also gets where to open it.
    pub struct ServerEventLinkPreview {
        pub title: String,
        pub starts_at: Timestamp,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub ends_at: Option<Timestamp>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub cover: Option<File>,
        pub status: ServerEventStatus,
        /// Name of the server
        pub server_name: String,
        /// Server id (members who can see the event)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub server: Option<String>,
        /// Event id (members who can see the event)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub event: Option<String>,
    }

    /// A new invitation link
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateServerEventInvitation {
        /// How many people can use it (1 to 200); absent: until the event
        /// has its 200 invited people
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 200)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub max_uses: Option<u32>,
        /// Seconds until it stops working (5 minutes to 30 days); absent:
        /// it works until the event is over
        #[cfg_attr(feature = "validator", validate(range(min = 300, max = 2592000)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub expires_in: Option<u64>,
    }

    /// An invitation link of an event: a member of the server who opens it
    /// is added to the invited people
    pub struct ServerEventInvitation {
        pub code: String,
        /// Who made it
        pub creator: String,
        /// How many people used it
        pub uses: u32,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub max_uses: Option<u32>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub expires_at: Option<Timestamp>,
        pub created_at: Timestamp,
    }

    /// The invitation links of an event
    pub struct ServerEventInvitationList {
        pub invitations: Vec<ServerEventInvitation>,
    }

    /// Where an accepted invitation leads
    pub struct ServerEventInvitationAccepted {
        pub server: String,
        pub event: String,
    }

    /// One person taking part in an event
    pub struct ServerEventAttendee {
        pub user: User,
        pub answer: ServerEventAnswer,
        /// Going, but on the waiting list
        #[cfg_attr(feature = "serde", serde(default))]
        pub waiting: bool,
        /// When they first answered (organisers only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub since: Option<Timestamp>,
    }

    /// A page of the people taking part in an event
    pub struct ServerEventAttendeesPage {
        /// People, by user id
        pub attendees: Vec<ServerEventAttendee>,
        /// Pass as `after` for the next page; absent on the last page
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub next: Option<String>,
    }

    /// Paging and filter of the attendee list
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsServerEventAttendees {
        /// `Going`, `Maybe`, `NotGoing` or `Waiting` (default: going, with
        /// the waiting list, and maybe)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 16)))]
        pub answer: Option<String>,
        /// Start after this user id
        #[cfg_attr(feature = "validator", validate(length(min = 26, max = 26)))]
        pub after: Option<String>,
        /// Page size (1 to 100, default 50)
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 100)))]
        pub limit: Option<i64>,
    }

    /// A time and place to check for clashes
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsServerEventClashes {
        /// Start (ISO 8601)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 40)))]
        pub starts_at: String,
        /// End (ISO 8601); absent: the default length of an event
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 40)))]
        pub ends_at: Option<String>,
        /// Only events in this channel
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 26)))]
        pub channel: Option<String>,
        /// Only events at this outside place
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 100)))]
        pub location: Option<String>,
        /// Leave this event out (the one being edited)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 26)))]
        pub exclude: Option<String>,
    }

    /// Events that overlap the asked time (and place). A warning for the
    /// editor: nothing stops two events at once
    pub struct ServerEventClashes {
        pub events: Vec<ServerEvent>,
    }

    /// Answers first given on one day
    pub struct ServerEventStatsDay {
        /// The day (`YYYY-MM-DD`, in the event's time zone)
        pub day: String,
        pub going: u32,
        pub maybe: u32,
        pub not_going: u32,
    }

    /// Numbers of one event, for its organisers. People who withdrew their
    /// answer are not counted anywhere: their answer is deleted
    pub struct ServerEventStats {
        /// The counts now (final once the event is over)
        pub counts: ServerEventCounts,
        /// Seats; absent: no limit
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub capacity: Option<u32>,
        /// Present answers by the day they were first given, oldest first
        pub registrations: Vec<ServerEventStatsDay>,
        /// How many people were in the voice channel when the event
        /// started (voice places only; a number, no names are kept)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub present_at_start: Option<u32>,
        /// When it really started
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub started_at: Option<Timestamp>,
        /// When it really ended
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub ended_at: Option<Timestamp>,
    }

    /// Which of your events to list
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsMyServerEvents {
        /// Events that start at or after this time (ISO 8601; default: a
        /// day ago, so an event that began earlier today is in the list)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 40)))]
        pub from: Option<String>,
        /// Events that start before this time (default: 90 days after
        /// `from`; at most 366 days after it)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 40)))]
        pub to: Option<String>,
        /// Only events you answered `Going`, `Maybe`, `NotGoing` or
        /// `Waiting`, or `Any` for every answer (default: all events you
        /// can see, answered or not)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 16)))]
        pub answer: Option<String>,
        /// How many (1 to 200, default 100)
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 200)))]
        pub limit: Option<i64>,
    }

    /// Events across your servers, soonest first, with your answers
    pub struct MyServerEvents {
        pub events: Vec<ServerEvent>,
        pub answers: Vec<ServerEventOwnAnswer>,
    }
);

impl Default for ServerEventVisibility {
    fn default() -> Self {
        ServerEventVisibility::Place
    }
}

impl Default for ServerEventAttendeeList {
    /// What an event from before this setting has: its list was open
    fn default() -> Self {
        ServerEventAttendeeList::Everyone
    }
}

impl Default for ServerEventAnswer {
    /// What an "Interested" mark from before answers means
    fn default() -> Self {
        ServerEventAnswer::Maybe
    }
}

impl ServerEventAnswer {
    /// The name stored in the database
    pub fn name(&self) -> &'static str {
        match self {
            ServerEventAnswer::Going => "Going",
            ServerEventAnswer::Maybe => "Maybe",
            ServerEventAnswer::NotGoing => "NotGoing",
        }
    }
}

impl ServerEventPlace {
    /// The channel of a channel place
    pub fn channel(&self) -> Option<&str> {
        match self {
            ServerEventPlace::Voice { channel } | ServerEventPlace::Text { channel } => {
                Some(channel)
            }
            ServerEventPlace::External { .. } => None,
        }
    }
}
