//! Scheduled server events.
//!
//! An event belongs to a server and takes place in one of its voice or text
//! channels or somewhere outside Zrzur. crond starts it at `starts_at` and
//! completes it at `ends_at` (or `DEFAULT_LENGTH_HOURS` after the start when
//! no end is set); managers can do both by hand.
//!
//! Repeating events form a series. Every occurrence is computed from the
//! series' first start (`series_start`) and its number (`occurrence`), in
//! the creator's time zone, so "every Thursday at 19:00 in Vienna" stays at
//! 19:00 across daylight saving changes and "monthly on the 31st" goes
//! 31 Jan, 28/29 Feb, 31 Mar. The next occurrence has an id derived from
//! the one before, and is written before the one before is marked over, so
//! a crash or two writers at once never make two (or none).
//!
//! Who sees what: members of the server see events at external places and
//! events in channels they may view. An event can narrow that to chosen
//! roles or to invited people (`visibility`); its organisers always see it,
//! and a draft is seen by organisers only. Gateway events follow the same
//! rule: an event open to everyone who sees the place goes to the place's
//! channel topic (only members who can view the channel are subscribed to
//! it) or to the server topic for external places; a restricted event goes
//! to the private topic of each person who may see it, and a draft to
//! nobody.
//!
//! Answers (going, maybe, not going), seats and the waiting list are in
//! `rsvp.rs` and `roster.rs`.

use std::collections::{HashMap, HashSet};

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_models::v0::{
    self, ServerEventAnswer, ServerEventAttendeeList, ServerEventLink, ServerEventPlace,
    ServerEventRepeat, ServerEventStatus, ServerEventVisibility,
};
use revolt_permissions::{
    calculate_channel_permissions, calculate_server_permissions, ChannelPermission,
};
use revolt_result::{ErrorType, Result};
use ulid::Ulid;

use crate::events::client::EventV1;
use crate::util::permissions::DatabasePermissionQuery;
use crate::util::tz::{civil_from_days, days_from_civil, days_in_month, Zone};
use crate::{Channel, Database, EventNotice, File, Member, Server, SystemMessage, User};

auto_derived!(
    /// A scheduled server event
    pub struct ServerEvent {
        /// Unique id
        #[serde(rename = "_id")]
        pub id: String,
        /// Server it belongs to
        pub server: String,
        /// Who created it
        pub creator: String,
        /// Title (1 to 100 characters)
        pub title: String,
        /// Description in Markdown (up to 1000 characters)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        /// When it starts
        pub starts_at: Timestamp,
        /// When it ends, if set
        #[serde(skip_serializing_if = "Option::is_none")]
        pub ends_at: Option<Timestamp>,
        /// Where it takes place
        pub place: ServerEventPlace,
        /// Cover image (each occurrence has its own use of the file)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub cover: Option<File>,
        /// Where it stands
        pub status: ServerEventStatus,
        /// How it repeats
        #[serde(skip_serializing_if = "Option::is_none")]
        pub repeat: Option<ServerEventRepeat>,
        /// How many members answered "maybe" (the count of the older
        /// "Interested" mark; kept by the database with each answer, never
        /// written from a stale copy)
        #[serde(default)]
        pub interested_count: u32,
        /// How many members are going and have a seat
        #[serde(default)]
        pub going_count: u32,
        /// How many members answered "not going"
        #[serde(default)]
        pub not_going_count: u32,
        /// How many members wait for a seat
        #[serde(default)]
        pub waitlist_count: u32,
        /// Version of the seat list the two seat counts were taken from
        #[serde(default)]
        pub roster_version: i64,
        /// Seats; absent: no limit
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub capacity: Option<u32>,
        /// Whether people can wait for a seat when all are taken
        #[serde(default = "revolt_models::if_true_default")]
        pub waitlist: bool,
        /// Who may see it (absent, as on older events: everyone who can
        /// see the place)
        #[serde(default)]
        pub visibility: ServerEventVisibility,
        /// Roles that see it (visibility `Roles`)
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub visible_roles: Vec<String>,
        /// Invited people (visibility `Invited`)
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub invited: Vec<String>,
        /// Who may read the list of people taking part (absent, as on
        /// older events: everyone who sees the event)
        #[serde(default)]
        pub attendee_list: ServerEventAttendeeList,
        /// Members who manage this event beside the people with Manage
        /// Events
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub co_organisers: Vec<String>,
        /// How many people were in the voice channel at the start (a
        /// number only; who they were is not kept)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub present_at_start: Option<u32>,
        /// When the retention time of the event ran out and the answers,
        /// the seat list and the invited list were removed. The counts
        /// stay.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub people_removed_at: Option<Timestamp>,
        /// The event this one follows (repeating events)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub previous: Option<String>,
        /// When it really started
        #[serde(skip_serializing_if = "Option::is_none")]
        pub started_at: Option<Timestamp>,
        /// When it really ended
        #[serde(skip_serializing_if = "Option::is_none")]
        pub ended_at: Option<Timestamp>,
        /// The creator's IANA time zone: repeats keep its wall-clock time
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub time_zone: Option<String>,
        /// Start of the first occurrence of the series (absent: this is it)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub series_start: Option<Timestamp>,
        /// Number of this occurrence in its series (0 for the first)
        #[serde(skip_serializing_if = "is_zero", default)]
        pub occurrence: u32,
        /// The occurrence after this one, once it exists
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub next: Option<String>,
        /// Reminders, in minutes before the start
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub reminders: Vec<u32>,
        /// The reminders that went out already (each goes out once)
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub reminders_sent: Vec<u32>,
        /// Its share link
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub link: Option<ServerEventLink>,
        /// People outside the server see a little of it through the link
        #[serde(default)]
        pub link_public: bool,
        /// Its invitation links (their use counts change on their own)
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub invitations: Vec<ServerEventInvitation>,
    }

    /// An invitation link of an event
    pub struct ServerEventInvitation {
        pub code: String,
        pub creator: String,
        #[serde(default)]
        pub uses: u32,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub max_uses: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub expires_at: Option<Timestamp>,
        pub created_at: Timestamp,
    }

    /// Event and member of an answer
    #[derive(Hash, Default)]
    pub struct ServerEventInterestKey {
        pub event: String,
        pub user: String,
    }

    /// A member's answer to an event (one per event and member). It keeps
    /// the name it had when the only answer was "Interested"; a row from
    /// then has no `answer` and reads as "maybe".
    pub struct ServerEventInterest {
        #[serde(rename = "_id")]
        pub id: ServerEventInterestKey,
        /// Server of the event (to clean up with it)
        pub server: String,
        /// When they first answered
        pub created_at: Timestamp,
        /// Going, maybe or not going
        #[serde(default)]
        pub answer: ServerEventAnswer,
        /// Going, but waiting for a seat (the seat list decides; this
        /// mirrors it for lists)
        #[serde(default)]
        pub waiting: bool,
        /// When they last changed their answer
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub updated_at: Option<Timestamp>,
        /// They want no notices of this event
        #[serde(default)]
        pub muted: bool,
    }
);

/// An active event without an end time completes this long after its start
pub const DEFAULT_LENGTH_HOURS: i64 = 12;
/// Scheduled and active events one server can have at once
pub const MAX_UPCOMING_EVENTS: usize = 100;
/// How far ahead an event can be scheduled
pub const MAX_DAYS_AHEAD: i64 = 366;
/// Longest event
pub const MAX_LENGTH_DAYS: i64 = 31;
/// How long after an event ended (or was called off) the answers, the seat
/// list and the invited list are kept: 12 calendar months (owner,
/// 2026-11-18). The counts stay.
pub const PEOPLE_RETENTION_MONTHS: u32 = 12;

/// The same day and time `months` calendar months later (UTC); a day the
/// later month does not have becomes its last day (31 August plus six
/// months is the last day of February).
pub fn months_after(time: Timestamp, months: u32) -> Timestamp {
    let seconds = unix_seconds(time);
    let fraction = time.duration_since(from_unix_seconds(seconds));
    let (day, clock) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, date) = civil_from_days(day);
    let count = year * 12 + (month as i64 - 1) + months as i64;
    let (year, month) = (count.div_euclid(12), count.rem_euclid(12) as u32 + 1);
    let day = days_from_civil(year, month, date.min(days_in_month(year, month)));
    let later = from_unix_seconds(day * 86_400 + clock);
    later.checked_add(fraction).unwrap_or(later)
}

/// Events whose people are removed in one retention pass
pub const RETENTION_BATCH: i64 = 50;
/// Who the audit entry of the retention names: nobody, the system did it
pub const RETENTION_ACTOR: &str = "00000000000000000000000000";

/// Draft events one server can have at once
pub const MAX_DRAFT_EVENTS: usize = 25;
/// Roles an event can be limited to
pub const MAX_VISIBLE_ROLES: usize = 20;
/// People who can be invited to one event
pub const MAX_INVITED: usize = 200;
/// Helpers one event can have
pub const MAX_CO_ORGANISERS: usize = 10;
/// A restricted event is sent live to at most this many people; the rest
/// see it when they next open the events of the server
pub const MAX_LIVE_AUDIENCE: usize = 5000;
/// Count changes of a restricted event are sent live only up to this many
/// people (each answer would otherwise make hundreds of sends)
pub const MAX_LIVE_COUNT_AUDIENCE: usize = 100;
/// Up to this many people are told before the request answers
#[cfg(not(any(test, feature = "test-utils")))]
const SEND_IN_LINE: usize = 50;

/// Redis set of the user ids in a voice channel (written by the voice code)
pub const VOICE_MEMBERS_KEY: &str = "vc_members:";

/// Who gets the gateway events of an event
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventAudience {
    /// A draft: nobody
    Nobody,
    /// Everyone subscribed to a topic (the place's channel, or the server)
    Topic(String),
    /// These people, each on their private topic
    People(Vec<String>),
}

/// What the place of an event is right now
pub enum PlaceState {
    /// Somewhere outside Zrzur
    External,
    /// A channel of the server
    Channel(Box<Channel>),
    /// A channel that no longer exists in the server
    Gone,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn epoch() -> Timestamp {
    Timestamp::parse("1970-01-01T00:00:00Z").expect("epoch")
}

/// Seconds since 1970 (UTC)
pub fn unix_seconds(time: Timestamp) -> i64 {
    time.duration_since(epoch()).whole_seconds()
}

/// A UTC moment from seconds since 1970
pub fn from_unix_seconds(seconds: i64) -> Timestamp {
    epoch()
        .checked_add(Duration::seconds(seconds))
        .unwrap_or_else(epoch)
}

/// Start of occurrence `n` of a series that first started at `first`, in
/// the wall-clock time of `zone` (UTC when unknown). Monthly keeps the day
/// of the first occurrence, or the last day of a shorter month.
pub fn occurrence_start(
    first: Timestamp,
    n: u32,
    repeat: &ServerEventRepeat,
    zone: Option<&str>,
) -> Timestamp {
    let zone = zone.and_then(Zone::load).unwrap_or_else(Zone::utc);
    let seconds = unix_seconds(first);
    // the part under a second stays as it was
    let fraction = first.duration_since(from_unix_seconds(seconds));
    let local = zone.to_local(seconds);
    let (day, time) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let day = match repeat {
        ServerEventRepeat::Daily => day + n as i64,
        ServerEventRepeat::Weekly => day + 7 * n as i64,
        ServerEventRepeat::EveryTwoWeeks => day + 14 * n as i64,
        ServerEventRepeat::Monthly => {
            let (year, month, date) = civil_from_days(day);
            let months = year * 12 + (month as i64 - 1) + n as i64;
            let (year, month) = (months.div_euclid(12), months.rem_euclid(12) as u32 + 1);
            days_from_civil(year, month, date.min(days_in_month(year, month)))
        }
    };
    let start = from_unix_seconds(zone.to_utc(day * 86_400 + time));
    start.checked_add(fraction).unwrap_or(start)
}

/// The start one step after `from` (a series that starts at `from`)
pub fn next_start(from: Timestamp, repeat: &ServerEventRepeat) -> Timestamp {
    occurrence_start(from, 1, repeat, None)
}

fn fnv(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The id of the occurrence after `previous`: always the same for the same
/// event, so writing it twice fails instead of making two
pub fn next_occurrence_id(previous: &str) -> String {
    let time = Ulid::from_string(previous)
        .map(|id| id.timestamp_ms())
        .unwrap_or_else(|_| fnv(previous.as_bytes(), 7) >> 16);
    let random = ((fnv(previous.as_bytes(), 1) as u128) << 16 | (fnv(previous.as_bytes(), 2) & 0xffff) as u128)
        & ((1u128 << 80) - 1);
    Ulid::from_parts(time, random).to_string()
}

impl ServerEvent {
    /// A new scheduled event
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        server: &str,
        creator: &str,
        title: String,
        description: Option<String>,
        starts_at: Timestamp,
        ends_at: Option<Timestamp>,
        place: ServerEventPlace,
        repeat: Option<ServerEventRepeat>,
    ) -> ServerEvent {
        ServerEvent {
            id: Ulid::new().to_string(),
            server: server.to_string(),
            creator: creator.to_string(),
            title,
            description: description.filter(|text| !text.trim().is_empty()),
            starts_at,
            ends_at,
            place,
            cover: None,
            status: ServerEventStatus::Scheduled,
            repeat,
            interested_count: 0,
            going_count: 0,
            not_going_count: 0,
            waitlist_count: 0,
            roster_version: 0,
            capacity: None,
            waitlist: true,
            visibility: ServerEventVisibility::Place,
            visible_roles: vec![],
            invited: vec![],
            // a new event shows its people to its organisers only, until
            // an organiser chooses otherwise
            attendee_list: ServerEventAttendeeList::Organisers,
            co_organisers: vec![],
            present_at_start: None,
            people_removed_at: None,
            previous: None,
            started_at: None,
            ended_at: None,
            time_zone: None,
            series_start: None,
            occurrence: 0,
            next: None,
            reminders: vec![],
            reminders_sent: vec![],
            link: None,
            link_public: false,
            invitations: vec![],
        }
    }

    /// Scheduled or active
    pub fn is_upcoming(&self) -> bool {
        matches!(
            self.status,
            ServerEventStatus::Scheduled | ServerEventStatus::Active
        )
    }

    /// When an active event completes by itself
    pub fn completes_at(&self) -> Timestamp {
        self.ends_at.unwrap_or_else(|| {
            let start = self.started_at.unwrap_or(self.starts_at).max(self.starts_at);
            start
                .checked_add(Duration::hours(DEFAULT_LENGTH_HOURS))
                .unwrap_or(start)
        })
    }

    /// Gateway topic: the place's channel, or the server for external places
    pub fn topic(&self) -> String {
        self.place
            .channel()
            .map(str::to_string)
            .unwrap_or_else(|| self.server.clone())
    }

    /// The next occurrence of a repeating event, after `now`
    pub fn next_occurrence(&self, now: Timestamp) -> Option<ServerEvent> {
        let repeat = self.repeat.as_ref()?;
        let first = self.series_start.unwrap_or(self.starts_at);
        let length = self.ends_at.map(|end| end.duration_since(self.starts_at));
        let mut n = self.occurrence + 1;
        let mut starts_at = occurrence_start(first, n, repeat, self.time_zone.as_deref());
        // crond may have been down: never schedule in the past
        while starts_at <= now && n < self.occurrence + 2000 {
            n += 1;
            starts_at = occurrence_start(first, n, repeat, self.time_zone.as_deref());
        }
        Some(ServerEvent {
            id: next_occurrence_id(&self.id),
            starts_at,
            ends_at: length.and_then(|length| starts_at.checked_add(length)),
            status: ServerEventStatus::Scheduled,
            interested_count: 0,
            going_count: 0,
            not_going_count: 0,
            waitlist_count: 0,
            roster_version: 0,
            present_at_start: None,
            people_removed_at: None,
            previous: Some(self.id.clone()),
            started_at: None,
            ended_at: None,
            series_start: Some(first),
            occurrence: n,
            next: None,
            cover: None,
            reminders_sent: vec![],
            link: None,
            link_public: false,
            invitations: vec![],
            ..self.clone()
        })
    }

    /// The place's channel, when it is a channel place that still exists
    pub async fn place_channel(&self, db: &Database) -> Option<Channel> {
        let id = self.place.channel()?;
        db.fetch_channel(id)
            .await
            .ok()
            .filter(|channel| channel.server() == Some(self.server.as_str()))
    }

    /// Whether the place is a channel that no longer exists in the server
    pub async fn place_is_gone(&self, db: &Database) -> bool {
        self.place.channel().is_some() && self.place_channel(db).await.is_none()
    }

    /// What the place is right now (one read of the channel)
    pub async fn place_state(&self, db: &Database) -> PlaceState {
        if self.place.channel().is_none() {
            return PlaceState::External;
        }
        match self.place_channel(db).await {
            Some(channel) => PlaceState::Channel(Box::new(channel)),
            None => PlaceState::Gone,
        }
    }

    /// Whether this member holds Manage Events (Manage Server implies it)
    pub async fn can_manage_events(
        db: &Database,
        user: &User,
        server: &Server,
        member: &Member,
    ) -> bool {
        let mut query = DatabasePermissionQuery::new(db, user)
            .server(server)
            .member(member);
        let permissions = calculate_server_permissions(&mut query).await;
        permissions.has_channel_permission(ChannelPermission::ManageEvents)
            || permissions.has_channel_permission(ChannelPermission::ManageServer)
    }

    /// Whether this person is one of the event's named helpers
    pub fn is_co_organiser(&self, user: &str) -> bool {
        self.co_organisers.iter().any(|id| id == user)
    }

    /// Whether the event is limited to roles or invited people
    pub fn is_restricted(&self) -> bool {
        self.visibility != ServerEventVisibility::Place
    }

    /// Whether this user (a member of the server) can see the event.
    ///
    /// A draft: its organisers only. Otherwise the place must be visible
    /// (always for external places, for channels only with View Channel
    /// there; a channel that is gone or of another server hides the event),
    /// and a restricted event is also only for its roles or invited people,
    /// its creator and its organisers.
    pub async fn visible_to(
        &self,
        db: &Database,
        user: &User,
        server: &Server,
        member: &Member,
    ) -> bool {
        let manager = Self::can_manage_events(db, user, server, member).await;
        let place = self.place_state(db).await;
        self.visible_given(db, user, server, member, manager, &place)
            .await
    }

    /// `visible_to` with what the caller already knows: whether the member
    /// holds Manage Events, and the state of the place
    pub async fn visible_given(
        &self,
        db: &Database,
        user: &User,
        server: &Server,
        member: &Member,
        manager: bool,
        place: &PlaceState,
    ) -> bool {
        // a mature server's events are for declared adults only, whatever
        // the place and whoever asks
        if server.nsfw
            && !crate::util::age::may_open_mature(db, user)
                .await
                .unwrap_or(false)
        {
            return false;
        }
        let organiser = manager || self.is_co_organiser(&user.id);
        if self.status == ServerEventStatus::Draft {
            return organiser;
        }
        let place_visible = match place {
            PlaceState::External => true,
            PlaceState::Channel(channel) => {
                Self::channel_visible(db, user, server, member, channel).await
            }
            PlaceState::Gone => false,
        };
        if !place_visible {
            return false;
        }
        match self.visibility {
            ServerEventVisibility::Place => true,
            _ if organiser || self.creator == user.id => true,
            ServerEventVisibility::Roles => member
                .roles
                .iter()
                .any(|role| self.visible_roles.contains(role)),
            ServerEventVisibility::Invited => self.invited.contains(&user.id),
        }
    }

    /// The people who may see an event right now, each checked as
    /// `visible_to` checks them: for a restricted event its roles or invited
    /// people, its creator, its helpers and everyone with Manage Events; for
    /// an event open to everyone who sees the place, the members. At most
    /// `MAX_LIVE_AUDIENCE`.
    pub async fn people_who_see(&self, db: &Database) -> Result<Vec<String>> {
        let server = db.fetch_server(&self.server).await?;
        let manage = ChannelPermission::ManageEvents as u64
            | ChannelPermission::ManageServer as u64
            | ChannelPermission::Administrator as u64;

        let mut ids: HashSet<String> = HashSet::new();
        ids.insert(self.creator.clone());
        ids.insert(server.owner.clone());
        ids.extend(self.co_organisers.iter().cloned());
        // roles that may manage events (checked exactly per member below)
        let mut roles: Vec<String> = server
            .roles
            .iter()
            .filter(|(_, role)| role.permissions.a as u64 & manage != 0)
            .map(|(id, _)| id.clone())
            .collect();
        match self.visibility {
            ServerEventVisibility::Roles => roles.extend(self.visible_roles.iter().cloned()),
            ServerEventVisibility::Invited => ids.extend(self.invited.iter().cloned()),
            ServerEventVisibility::Place => {}
        }

        let mut members: Vec<Member> = if !self.is_restricted()
            || server.default_permissions as u64 & manage != 0
        {
            // open to the members, or everyone manages events here
            db.fetch_all_members(&server.id).await?
        } else {
            let ids: Vec<String> = ids.into_iter().collect();
            let mut members = db.fetch_members(&server.id, &ids).await?;
            if !roles.is_empty() {
                members.extend(db.fetch_all_members_with_roles(&server.id, &roles).await?);
            }
            members
        };
        let mut seen = HashSet::new();
        members.retain(|member| seen.insert(member.id.user.clone()));
        members.truncate(MAX_LIVE_AUDIENCE);

        let ids: Vec<String> = members.iter().map(|member| member.id.user.clone()).collect();
        let users = db.fetch_users(&ids).await?;
        let place = self.place_state(db).await;
        let mut people = Vec::with_capacity(users.len());
        for member in &members {
            let Some(user) = users.iter().find(|user| user.id == member.id.user) else {
                continue;
            };
            let manager = Self::can_manage_events(db, user, &server, member).await;
            if self
                .visible_given(db, user, &server, member, manager, &place)
                .await
            {
                people.push(user.id.clone());
            }
        }
        Ok(people)
    }

    /// Whether the event's server is marked mature (unknown: yes)
    async fn is_mature_server(&self, db: &Database) -> bool {
        match db.fetch_server(&self.server).await {
            Ok(server) => server.nsfw,
            Err(_) => true,
        }
    }

    /// Whether this person is named on the event: its creator or one of
    /// its helpers
    pub fn names(&self, user: &str) -> bool {
        self.creator == user || self.is_co_organiser(user)
    }

    /// Whether an organiser sees who takes part without more ado: always in
    /// an event open to everyone who sees the place; in an event limited to
    /// roles or invited people only when named on it. Someone else who
    /// manages events (the server's owner too) has to give a reason, and
    /// that access is noted.
    pub fn shows_people_to_organiser(&self, user: &str) -> bool {
        !self.is_restricted() || self.names(user)
    }

    /// What a reader of the audit log may see of entries about people of
    /// events. The ids are stored; they are shown only to a reader who
    /// manages the event and sees its people (`shows_people_to_organiser`).
    /// For everyone else the person is blanked. With `by_target` (the
    /// reader asked for entries about one person) such entries are left out
    /// altogether, so that asking cannot answer "was this person in it?".
    pub async fn redact_audit_entries(
        db: &Database,
        reader: &User,
        server: &Server,
        entries: &mut Vec<crate::AuditLogEntry>,
        by_target: bool,
    ) {
        use crate::AuditLogEntryAction as Action;
        let mut manager: Option<bool> = None;
        let mut allowed: HashMap<String, bool> = HashMap::new();
        let mut hidden: HashSet<String> = HashSet::new();
        for entry in entries.iter_mut() {
            let event = match &entry.action {
                Action::ServerEventAttendeeRemove { event, .. }
                | Action::ServerEventAttendeeSeat { event, .. } => event.clone(),
                _ => continue,
            };
            let sees = match allowed.get(&event) {
                Some(sees) => *sees,
                None => {
                    let manages = match manager {
                        Some(manages) => manages,
                        None => {
                            let manages = match db.fetch_member(&server.id, &reader.id).await {
                                Ok(member) => {
                                    Self::can_manage_events(db, reader, server, &member).await
                                }
                                Err(_) => false,
                            };
                            manager = Some(manages);
                            manages
                        }
                    };
                    // an event that is gone: nobody is shown its people
                    let sees = match db.fetch_server_event(&event).await {
                        Ok(stored) => {
                            stored.server == server.id
                                && (manages || stored.is_co_organiser(&reader.id))
                                && stored.shows_people_to_organiser(&reader.id)
                        }
                        Err(_) => false,
                    };
                    allowed.insert(event.clone(), sees);
                    sees
                }
            };
            if sees {
                continue;
            }
            hidden.insert(entry.id.clone());
            entry.target = None;
            match &mut entry.action {
                Action::ServerEventAttendeeRemove { user, .. }
                | Action::ServerEventAttendeeSeat { user, .. } => user.clear(),
                _ => {}
            }
        }
        if by_target {
            entries.retain(|entry| !hidden.contains(&entry.id));
        }
    }

    /// The title an audit entry may carry: none for a draft and for an
    /// event limited to roles or invited people, since people who read the
    /// audit log need not be among those who may see the event
    pub fn audit_title(&self) -> String {
        if self.status == ServerEventStatus::Draft || self.is_restricted() {
            String::new()
        } else {
            self.title.clone()
        }
    }

    /// Who gets this event's gateway events. A failure to work out the
    /// people of a restricted event sends to nobody, never to a topic.
    pub async fn audience(&self, db: &Database) -> EventAudience {
        if self.status == ServerEventStatus::Draft {
            return EventAudience::Nobody;
        }
        if !self.is_restricted() {
            // the channel topic reaches only people who may view the
            // channel (mature ones included in that check); the server
            // topic reaches every member, so an outside place of a mature
            // server is sent person by person instead
            let mature = self.place.channel().is_none()
                && match db.fetch_server(&self.server).await {
                    Ok(server) => server.nsfw,
                    // unknown: the careful way
                    Err(_) => true,
                };
            if !mature {
                return EventAudience::Topic(self.topic());
            }
        }
        match self.people_who_see(db).await {
            Ok(people) => EventAudience::People(people),
            Err(error) => {
                log::warn!("Could not work out who sees event {}: {error:?}", self.id);
                EventAudience::People(vec![])
            }
        }
    }

    async fn send(event: EventV1, audience: &EventAudience) {
        match audience {
            EventAudience::Nobody => {}
            EventAudience::Topic(topic) => event.p(topic.clone()).await,
            EventAudience::People(people) => {
                // many people: sent beside the request, not inside it, so
                // that whoever caused it does not wait for everyone to be
                // told. (Tests send in line, to see what was sent.)
                #[cfg(not(any(test, feature = "test-utils")))]
                if people.len() > SEND_IN_LINE {
                    let people = people.clone();
                    tokio::task::spawn(async move { event.private_broadcast(people).await });
                    return;
                }
                event.private_broadcast(people.clone()).await;
            }
        }
    }

    /// View Channel on a channel of this server
    pub async fn channel_visible(
        db: &Database,
        user: &User,
        server: &Server,
        member: &Member,
        channel: &Channel,
    ) -> bool {
        if channel.server() != Some(server.id.as_str()) {
            return false;
        }
        // mature channels are for declared adults only
        if (channel.is_nsfw() || server.nsfw)
            && !crate::util::age::may_open_mature(db, user)
                .await
                .unwrap_or(false)
        {
            return false;
        }
        let mut query = DatabasePermissionQuery::new(db, user)
            .server(server)
            .member(member)
            .channel(channel);
        calculate_channel_permissions(&mut query)
            .await
            .has_channel_permission(ChannelPermission::ViewChannel)
    }

    /// Whether the start may be announced in the system channel: only when
    /// nobody could be kept from the place. Cautious on purpose: no mature
    /// channel or server, the server's default has View Channel, no role of
    /// the server denies it, and the channel (or the category it follows)
    /// denies it to no one (default, role or member).
    pub fn place_open_to_everyone(server: &Server, channel: Option<&Channel>) -> bool {
        let view = ChannelPermission::ViewChannel as i64;
        if server.nsfw || server.default_permissions & view == 0 {
            return false;
        }
        if server.roles.values().any(|role| role.permissions.d & view != 0) {
            return false;
        }
        let Some(channel) = channel else {
            return true;
        };
        match channel {
            Channel::TextChannel {
                id,
                nsfw,
                default_permissions,
                role_permissions,
                member_permissions,
                permissions_synced,
                ..
            } => {
                if *nsfw {
                    return false;
                }
                let denies = |field: &revolt_permissions::OverrideField| field.d & view != 0;
                if member_permissions.values().any(denies) {
                    return false;
                }
                if *permissions_synced {
                    let category = server.categories.as_ref().and_then(|categories| {
                        categories
                            .iter()
                            .find(|category| category.channels.contains(id))
                    });
                    match category {
                        Some(category) => {
                            !category.default_permissions.as_ref().is_some_and(denies)
                                && !category.role_permissions.values().any(denies)
                        }
                        // synced with a category that is not there: unknown
                        None => false,
                    }
                } else {
                    !default_permissions.as_ref().is_some_and(denies)
                        && !role_permissions.values().any(denies)
                }
            }
            // anything that is not a server channel is never announced
            _ => false,
        }
    }

    /// Tell the members who can see it that it is new
    pub async fn publish_create(&self, db: &Database) {
        let audience = self.audience(db).await;
        Self::send(EventV1::ServerEventCreate(self.clone().into()), &audience).await;
    }

    /// Tell the members who can see it that it changed. `old` is who could
    /// see it before the change: those who no longer can hear that it is
    /// gone first.
    pub async fn publish_update(&self, db: &Database, old: Option<EventAudience>) {
        let audience = self.audience(db).await;
        let gone = EventV1::ServerEventDelete {
            id: self.id.clone(),
            server: self.server.clone(),
        };
        match (old, &audience) {
            (None, _) | (Some(EventAudience::Nobody), _) => {}
            (Some(EventAudience::Topic(old)), EventAudience::Topic(new)) if &old == new => {}
            (Some(EventAudience::People(old)), EventAudience::People(new)) => {
                let lost: Vec<String> =
                    old.into_iter().filter(|user| !new.contains(user)).collect();
                Self::send(gone, &EventAudience::People(lost)).await;
            }
            (Some(old), _) => Self::send(gone, &old).await,
        }
        Self::send(
            EventV1::ServerEventUpdate {
                event: self.clone().into(),
            },
            &audience,
        )
        .await;
    }

    /// Tell the members who could see it that it is gone
    pub async fn publish_delete(&self, db: &Database) {
        let audience = self.audience(db).await;
        Self::send(
            EventV1::ServerEventDelete {
                id: self.id.clone(),
                server: self.server.clone(),
            },
            &audience,
        )
        .await;
    }

    /// Tell who sees the event that its counts changed. Nobody learns from
    /// this who answered: the older `ServerEventInterest` goes out with an
    /// empty `user`.
    pub async fn publish_counts(&self, db: &Database) {
        // an event sent person by person in a big server: the people are
        // not worked out for every answer, they see the counts on opening
        if self.is_restricted() || self.place.channel().is_none() {
            let invited_only = self.visibility == ServerEventVisibility::Invited;
            let big = !invited_only
                && db
                    .fetch_member_count(&self.server)
                    .await
                    .is_ok_and(|members| members > MAX_LIVE_COUNT_AUDIENCE);
            if big && (self.is_restricted() || self.is_mature_server(db).await) {
                return;
            }
        }
        let audience = match self.audience(db).await {
            // a big restricted event: people see the counts when they open it
            EventAudience::People(people) if people.len() > MAX_LIVE_COUNT_AUDIENCE => return,
            audience => audience,
        };
        let channel = self.place.channel().map(str::to_string);
        let event: v0::ServerEvent = self.clone().into();
        Self::send(
            EventV1::ServerEventCounts {
                id: self.id.clone(),
                server: self.server.clone(),
                channel: channel.clone(),
                counts: event.counts,
            },
            &audience,
        )
        .await;
        Self::send(
            EventV1::ServerEventInterest {
                id: self.id.clone(),
                server: self.server.clone(),
                channel,
                user: String::new(),
                interested: true,
                count: event.interested_count,
            },
            &audience,
        )
        .await;
    }

    /// Tell a person's own devices what their answer is now
    pub async fn publish_own_answer(&self, user: &str, rsvp: Option<v0::ServerEventRsvp>) {
        let event: v0::ServerEvent = self.clone().into();
        let channel = self.place.channel().map(str::to_string);
        // the older event, for apps from before answers
        EventV1::ServerEventInterest {
            id: self.id.clone(),
            server: self.server.clone(),
            channel,
            user: user.to_string(),
            interested: rsvp
                .as_ref()
                .is_some_and(|rsvp| rsvp.answer != ServerEventAnswer::NotGoing),
            count: event.interested_count,
        }
        .private(user.to_string())
        .await;
        EventV1::ServerEventRsvpUpdate {
            id: self.id.clone(),
            server: self.server.clone(),
            rsvp,
        }
        .private(user.to_string())
        .await;
    }

    /// Start it now (crond at `starts_at`, or a manager by hand). Returns
    /// false when it was not scheduled any more (someone else was first).
    pub async fn start(&mut self, db: &Database) -> Result<bool> {
        let now = Timestamp::now_utc();
        if !db
            .set_server_event_status(
                &self.id,
                ServerEventStatus::Scheduled,
                ServerEventStatus::Active,
                now,
            )
            .await?
        {
            return Ok(false);
        }
        self.status = ServerEventStatus::Active;
        self.started_at = Some(now);
        self.note_presence(db).await;
        self.publish_update(db, None).await;
        if let Err(error) = self.announce_start(db).await {
            log::warn!("Could not announce the start of event {}: {error:?}", self.id);
        }
        Ok(true)
    }

    /// Complete it now (crond at the end, or a manager by hand). The next
    /// occurrence of a repeating event is written first. Returns false when
    /// it was not active any more.
    pub async fn complete(&mut self, db: &Database) -> Result<bool> {
        let now = Timestamp::now_utc();
        if self.status != ServerEventStatus::Active {
            return Ok(false);
        }
        self.schedule_next(db, now).await?;
        if !db
            .set_server_event_status(
                &self.id,
                ServerEventStatus::Active,
                ServerEventStatus::Completed,
                now,
            )
            .await?
        {
            return Ok(false);
        }
        self.status = ServerEventStatus::Completed;
        self.ended_at = Some(now);
        self.publish_update(db, None).await;
        Ok(true)
    }

    /// Call it off (scheduled or active). One occurrence: the series goes
    /// on with the next one. The whole series: no more occurrences. Returns
    /// false when it was over already.
    pub async fn cancel(&mut self, db: &Database, whole_series: bool) -> Result<bool> {
        let now = Timestamp::now_utc();
        let from = self.status.clone();
        if !self.is_upcoming() {
            return Ok(false);
        }
        if whole_series {
            if self.repeat.take().is_some() {
                db.update_server_event(self).await?;
            }
        } else {
            self.schedule_next(db, now).await?;
        }
        if !db
            .set_server_event_status(&self.id, from.clone(), ServerEventStatus::Cancelled, now)
            .await?
        {
            return Ok(false);
        }
        self.status = ServerEventStatus::Cancelled;
        self.ended_at = Some(now);
        self.publish_update(db, None).await;
        if from != ServerEventStatus::Draft {
            if let Err(error) = self.notify(db, EventNotice::Cancelled, None).await {
                log::warn!("Could not tell people event {} is off: {error:?}", self.id);
            }
        }
        Ok(true)
    }

    /// Delete it for good. A repeating event's series goes on with the next
    /// occurrence, unless `whole_series`: then nothing comes after it and the
    /// series ends. Its cover file is released.
    pub async fn delete(&self, db: &Database, whole_series: bool) -> Result<()> {
        // one occurrence of a series: the next one is written first; the
        // whole series: nothing comes after this one
        if self.is_upcoming() && !whole_series {
            self.schedule_next(db, Timestamp::now_utc()).await?;
        }
        // who could see it, read while its people are still known
        let audience = self.audience(db).await;
        db.delete_server_event(&self.id).await?;
        if let Some(cover) = &self.cover {
            db.mark_attachments_as_deleted(&[cover.id.clone()]).await.ok();
        }
        Self::send(
            EventV1::ServerEventDelete {
                id: self.id.clone(),
                server: self.server.clone(),
            },
            &audience,
        )
        .await;
        Ok(())
    }

    /// A server is deleted. Its events are not deleted with it: an event
    /// that was still to come, running or a draft is called off at this
    /// moment, without a word to anyone (nobody can reach it any more), and
    /// all of them then fall under the retention time like any other ended
    /// event; when that time is up an event of a server that no longer
    /// exists is deleted as a whole (`run_retention_pass`). Only an event
    /// whose people were already removed by the retention goes at once:
    /// its time was up before. Doing this twice changes nothing.
    pub async fn close_all_of_server(db: &Database, server: &str) -> Result<()> {
        let now = Timestamp::now_utc();
        let mut open = db.fetch_upcoming_server_events(server).await?;
        open.extend(db.fetch_server_event_drafts(server).await?);
        for event in open {
            db.set_server_event_status(
                &event.id,
                event.status.clone(),
                ServerEventStatus::Cancelled,
                now,
            )
            .await?;
        }
        // events whose retention time was up before: nothing of them is
        // kept for anyone now, and no later pass would find them
        let servers = [server.to_string()];
        let far = now
            .checked_add(Duration::days(100 * 366))
            .unwrap_or(now);
        for event in db
            .fetch_server_events_between(&servers, epoch(), far, i32::MAX as i64)
            .await?
        {
            if event.people_removed_at.is_some() {
                event.delete_orphan(db, now).await?;
            }
        }
        Ok(())
    }

    /// Delete an event of a server that no longer exists as a whole, with
    /// its cover file. Returns how many answers went with it.
    async fn delete_orphan(&self, db: &Database, now: Timestamp) -> Result<u64> {
        let answers = if self.people_removed_at.is_some() {
            0
        } else {
            db.remove_server_event_people(&self.id, now).await?
        };
        db.delete_server_event(&self.id).await?;
        if let Some(cover) = &self.cover {
            db.mark_attachments_as_deleted(&[cover.id.clone()]).await.ok();
        }
        Ok(answers)
    }

    /// For a voice place: note how many people are in the channel as the
    /// event starts. Only the number is kept, not who they are. The number
    /// is read from the set the voice code keeps per channel
    /// (`voice::get_voice_channel_members`), by its name: crond starts most
    /// events and is built without the voice code.
    async fn note_presence(&mut self, db: &Database) {
        use redis_kiss::AsyncCommands;

        let ServerEventPlace::Voice { channel } = &self.place else {
            return;
        };
        let Ok(mut redis) = redis_kiss::get_connection().await else {
            return;
        };
        let Ok(count) = redis
            .scard::<_, u32>(format!("{VOICE_MEMBERS_KEY}{channel}"))
            .await
        else {
            return;
        };
        if db.set_server_event_presence(&self.id, count).await.is_ok() {
            self.present_at_start = Some(count);
        }
    }

    /// Write the next occurrence of a repeating event (once: its id comes
    /// from this one), with the people who are going or maybe going and
    /// are still members, in the order they hold now, and its own use of
    /// the cover file. "Not going" is an answer to one occurrence only.
    pub async fn schedule_next(&self, db: &Database, now: Timestamp) -> Result<Option<ServerEvent>> {
        let Some(mut next) = self.next_occurrence(now) else {
            return Ok(None);
        };
        if let Ok(existing) = db.fetch_server_event(&next.id).await {
            db.set_server_event_next(&self.id, &existing.id).await?;
            return Ok(Some(existing));
        }
        if let Some(cover) = &self.cover {
            next.cover = match File::copy_unused(db, cover, &self.creator).await {
                Ok(copy) => File::use_server_event_cover(db, &copy.id, &next.id, &self.creator)
                    .await
                    .ok(),
                Err(_) => None,
            };
        }
        if let Err(error) = db.insert_server_event(&next).await {
            // someone wrote it a moment ago
            return match db.fetch_server_event(&next.id).await {
                Ok(existing) => {
                    db.set_server_event_next(&self.id, &existing.id).await?;
                    Ok(Some(existing))
                }
                Err(_) => Err(error),
            };
        }
        next.carry_answers_from(self, db).await?;
        db.set_server_event_next(&self.id, &next.id).await?;
        next.publish_create(db).await;
        Ok(Some(next))
    }

    /// Withdraw the answers of everyone who may no longer see the event
    /// (after an edit narrowed who it is for, or moved it to a channel they
    /// cannot view): nobody stays counted or listed in an event they cannot
    /// open. A freed seat goes to the next waiting person. Returns whether
    /// anything changed.
    pub async fn drop_answers_of_people_shut_out(&mut self, db: &Database) -> Result<bool> {
        let server = db.fetch_server(&self.server).await?;
        let place = self.place_state(db).await;
        let mut shut_out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let rows = db
                .fetch_server_event_answers(
                    &self.id,
                    &[
                        ServerEventAnswer::Going,
                        ServerEventAnswer::Maybe,
                        ServerEventAnswer::NotGoing,
                    ],
                    None,
                    after.as_deref(),
                    ANNOUNCE_PAGE,
                )
                .await?;
            let Some(last) = rows.last().map(|row| row.id.user.clone()) else {
                break;
            };
            let ids: Vec<String> = rows.iter().map(|row| row.id.user.clone()).collect();
            let members = db.fetch_members(&server.id, &ids).await?;
            let users = db.fetch_users(&ids).await?;
            for id in &ids {
                let seen = match (
                    users.iter().find(|user| &user.id == id),
                    members.iter().find(|member| &member.id.user == id),
                ) {
                    (Some(user), Some(member)) => {
                        let manager = Self::can_manage_events(db, user, &server, member).await;
                        self.visible_given(db, user, &server, member, manager, &place)
                            .await
                    }
                    // no longer a member, or no such account
                    _ => false,
                };
                if !seen {
                    shut_out.push(id.clone());
                }
            }
            if (rows.len() as i64) < ANNOUNCE_PAGE {
                break;
            }
            after = Some(last);
        }
        for user in &shut_out {
            if self.withdraw(db, user).await? {
                self.publish_own_answer(user, None).await;
            }
        }
        Ok(!shut_out.is_empty())
    }

    /// The moment the retention time of this event runs out: 12 calendar
    /// months after it really ended or was called off. Each occurrence of a
    /// series has its own. None while it has not ended: its people are
    /// never removed by time.
    pub fn people_kept_until(&self) -> Option<Timestamp> {
        if !matches!(
            self.status,
            ServerEventStatus::Completed | ServerEventStatus::Cancelled
        ) {
            return None;
        }
        Some(months_after(self.ended_at?, PEOPLE_RETENTION_MONTHS))
    }

    /// One retention pass (crond). For an event that ended 12 calendar
    /// months ago or more: who answered, the seat list and the invited list
    /// are removed; the event, its counts and the number present at the
    /// start stay. An event of a server that no longer exists is deleted as
    /// a whole at that moment. Nothing that has not ended is ever touched.
    /// Each event done leaves an audit entry with its id, a number and the
    /// outcome, nothing else. One event failing never stops the others.
    /// Returns how many events were done.
    pub async fn run_retention_pass(db: &Database, now: Timestamp) -> Result<usize> {
        // candidates: ended at least 365 days ago, the shortest 12 months
        // can be; whether the time is really up is decided per event below
        let Some(before) = now.checked_sub(Duration::days(365)) else {
            return Ok(0);
        };
        let mut done = 0;
        for event in db
            .fetch_server_events_past_retention(before, RETENTION_BATCH)
            .await?
        {
            if !event.people_kept_until().is_some_and(|until| until <= now) {
                continue;
            }
            let outcome = async {
                let orphan = match db.fetch_server(&event.server).await {
                    Ok(_) => false,
                    Err(error) if matches!(error.error_type, ErrorType::NotFound) => true,
                    Err(error) => return Err(error),
                };
                if orphan {
                    Ok((event.delete_orphan(db, now).await?, "EventDeleted"))
                } else {
                    Ok((
                        db.remove_server_event_people(&event.id, now).await?,
                        "PeopleRemoved",
                    ))
                }
            }
            .await;
            match outcome {
                Ok((records, outcome)) => {
                    done += 1;
                    crate::AuditLogEntryAction::ServerEventRetention {
                        event: event.id.clone(),
                        records,
                        outcome: outcome.to_string(),
                    }
                    .insert(
                        db,
                        event.server.clone(),
                        None,
                        RETENTION_ACTOR.to_string(),
                        None,
                    )
                    .await;
                }
                Err(error) => {
                    log::error!("Retention of event {} failed: {error:?}", event.id);
                }
            }
        }
        Ok(done)
    }

    /// A member left the server: their answers there go (a seat they held
    /// goes to the next waiting person), and they come off the invited and
    /// helper lists of its events
    pub async fn forget_member(db: &Database, server: &str, user: &str) -> Result<()> {
        for row in db
            .fetch_server_event_answers_by_user(user, Some(server))
            .await?
        {
            if let Ok(mut event) = db.fetch_server_event(&row.id.event).await {
                if !event.is_upcoming() {
                    // over: the answer goes, nothing else moves or is said
                    event.forget_answer(db, user).await?;
                } else if event.withdraw(db, user).await? {
                    event.publish_counts(db).await;
                }
            }
        }
        db.pull_server_event_person(Some(server), user).await
    }

    /// An account is deleted: every answer of the person goes, in every
    /// server, and they come off every invited and helper list
    pub async fn forget_user(db: &Database, user: &str) -> Result<()> {
        for row in db.fetch_server_event_answers_by_user(user, None).await? {
            match db.fetch_server_event(&row.id.event).await {
                Ok(mut event) => {
                    if !event.is_upcoming() {
                        // over (or of a server that is gone): the answer
                        // goes, nothing else moves or is said
                        event.forget_answer(db, user).await?;
                    } else if event.withdraw(db, user).await? {
                        event.publish_counts(db).await;
                    }
                }
                // an answer to an event that is gone
                Err(_) => {
                    db.delete_server_event_answer(&row.id.event, user, None)
                        .await?;
                }
            }
        }
        db.pull_server_event_person(None, user).await
    }

    /// Whether a server has events switched on (None: the server is gone),
    /// read once per pass
    pub(super) async fn server_events_on(
        db: &Database,
        cache: &mut HashMap<String, Option<bool>>,
        server: &str,
    ) -> Result<Option<bool>> {
        if let Some(on) = cache.get(server) {
            return Ok(*on);
        }
        let on = match db.fetch_server(server).await {
            Ok(server) => Some(server.events_enabled),
            Err(error) if matches!(error.error_type, ErrorType::NotFound) => None,
            Err(error) => return Err(error),
        };
        cache.insert(server.to_string(), on);
        Ok(on)
    }

    /// One crond pass: cancel what lost its channel, start what is due,
    /// complete what has ended, and give every repeating event that lacks
    /// one its next occurrence. One event failing never stops the others.
    ///
    /// A server with events off is left alone: nothing starts, is announced
    /// or gets a next occurrence. Only an event whose whole time passed while
    /// off (scheduled or still active) is marked Completed, quietly (no
    /// gateway event, no message, no push); a repeating one gets its next
    /// occurrence once the server turns events on again, and an event still
    /// inside its time then starts as usual.
    pub async fn run_pass(db: &Database, now: Timestamp) -> Result<PassReport> {
        let mut report = PassReport::default();
        let mut servers = HashMap::new();
        for mut event in db.fetch_server_events_to_start(now).await? {
            let outcome = async {
                match Self::server_events_on(db, &mut servers, &event.server).await? {
                    None => {
                        // the server is gone: called off, kept for the
                        // retention time, never started
                        Self::close_all_of_server(db, &event.server).await?;
                        return Ok(false);
                    }
                    Some(false) => {
                        if event.completes_at() <= now {
                            report.missed += db
                                .set_server_event_status(
                                    &event.id,
                                    ServerEventStatus::Scheduled,
                                    ServerEventStatus::Completed,
                                    now,
                                )
                                .await? as usize;
                        }
                        return Ok(false);
                    }
                    Some(true) => {}
                }
                if event.place_is_gone(db).await {
                    report.cancelled += event.cancel(db, true).await? as usize;
                    return Ok(false);
                }
                event.start(db).await
            }
            .await;
            match outcome {
                Ok(started) => report.started += started as usize,
                Err(error) => {
                    report.failed += 1;
                    log::error!("Server event {} could not start: {error:?}", event.id);
                }
            }
        }
        for mut event in db.fetch_active_server_events().await? {
            let outcome = async {
                let on = Self::server_events_on(db, &mut servers, &event.server).await?;
                if on.is_none() {
                    // the server is gone: nothing is announced or scheduled
                    Self::close_all_of_server(db, &event.server).await?;
                    return Ok((false, false));
                }
                if on == Some(false) {
                    if event.completes_at() <= now {
                        report.missed += db
                            .set_server_event_status(
                                &event.id,
                                ServerEventStatus::Active,
                                ServerEventStatus::Completed,
                                now,
                            )
                            .await? as usize;
                    }
                    return Ok((false, false));
                }
                if event.place_is_gone(db).await {
                    return Ok((false, event.cancel(db, true).await?));
                }
                if event.completes_at() <= now {
                    return Ok((event.complete(db).await?, false));
                }
                Ok::<_, revolt_result::Error>((false, false))
            }
            .await;
            match outcome {
                Ok((completed, cancelled)) => {
                    report.completed += completed as usize;
                    report.cancelled += cancelled as usize;
                }
                Err(error) => {
                    report.failed += 1;
                    log::error!("Server event {} could not complete: {error:?}", event.id);
                }
            }
        }
        for event in db.fetch_server_events_missing_next().await? {
            match Self::server_events_on(db, &mut servers, &event.server).await {
                Ok(Some(true)) => {}
                // off, or the server is gone: no next occurrence
                Ok(Some(false)) | Ok(None) => continue,
                Err(error) => {
                    report.failed += 1;
                    log::error!("Server event {} has no next occurrence: {error:?}", event.id);
                    continue;
                }
            }
            match event.schedule_next(db, now).await {
                Ok(Some(_)) => report.scheduled += 1,
                Ok(None) => {}
                Err(error) => {
                    report.failed += 1;
                    log::error!("Server event {} has no next occurrence: {error:?}", event.id);
                }
            }
        }
        Ok(report)
    }

    /// Who gets the push when the event starts: the people who are going
    /// or maybe going, are still members, and can still see the event and
    /// the channel the notification names. Read a page at a time.
    pub async fn people_to_tell_at_start(
        &self,
        db: &Database,
        server: &Server,
        place: &PlaceState,
        push_channel: &Channel,
    ) -> Result<Vec<String>> {
        let mut recipients = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = db
                .fetch_server_event_interested(&self.id, after.as_deref(), ANNOUNCE_PAGE)
                .await?;
            let Some(last) = page.last().cloned() else {
                break;
            };
            recipients.extend(
                self.who_may_be_told(db, server, place, push_channel, &page)
                    .await?,
            );
            if (page.len() as i64) < ANNOUNCE_PAGE {
                break;
            }
            after = Some(last);
        }
        Ok(recipients)
    }

    /// Of these people: the ones who are still members and can still see
    /// the event and the channel a notification names
    pub async fn who_may_be_told(
        &self,
        db: &Database,
        server: &Server,
        place: &PlaceState,
        push_channel: &Channel,
        people: &[String],
    ) -> Result<Vec<String>> {
        let mut recipients = Vec::new();
        if people.is_empty() {
            return Ok(recipients);
        }
        let members = db.fetch_members(&server.id, people).await?;
        let users = db.fetch_users(people).await?;
        for user in &users {
            let Some(member) = members.iter().find(|member| member.id.user == user.id) else {
                continue;
            };
            let manager = Self::can_manage_events(db, user, server, member).await;
            if self
                .visible_given(db, user, server, member, manager, place)
                .await
                && Self::channel_visible(db, user, server, member, push_channel).await
            {
                recipients.push(user.id.clone());
            }
        }
        Ok(recipients)
    }

    /// When it starts: a system message in the server's join message channel
    /// (only for an event open to everyone who sees the place, and only when
    /// everyone can see that place), and a push notification to the people
    /// who are going or maybe going and can still see the event and the
    /// channel the notification names
    async fn announce_start(&self, db: &Database) -> Result<()> {
        let server = db.fetch_server(&self.server).await?;
        let place = self.place_state(db).await;
        let place_channel = match &place {
            PlaceState::Gone => return Ok(()),
            PlaceState::Channel(channel) => Some(channel.as_ref().clone()),
            PlaceState::External => None,
        };

        let system_channel = match server
            .system_messages
            .as_ref()
            .and_then(|channels| channels.user_joined.clone())
        {
            Some(id) => db.fetch_channel(&id).await.ok(),
            None => None,
        };
        let open = !self.is_restricted()
            && Self::place_open_to_everyone(&server, place_channel.as_ref());
        let mut message = SystemMessage::EventStarted {
            id: self.id.clone(),
            title: self.title.clone(),
        }
        .into_message(String::new());
        if let (Some(channel), true) = (&system_channel, open) {
            message.channel = channel.id().to_string();
            message
                .send_without_notifications(db, None, None, false, false, false)
                .await?;
        }

        // the channel the push names: the place, or the system channel
        let push_channel = match place_channel.clone().or(system_channel) {
            Some(channel) => channel,
            None => return Ok(()),
        };
        message.channel = push_channel.id().to_string();

        let recipients = self
            .people_to_tell_at_start(db, &server, &place, &push_channel)
            .await?;
        if recipients.is_empty() {
            return Ok(());
        }

        let mut notification = v0::PushNotification::from(
            message.into_model(None, None),
            None,
            push_channel.into(),
        )
        .await;
        notification.tag = format!("event-{}", self.id);
        notification.url = format!(
            "{}/server/{}/events/{}",
            config().await.hosts.app,
            self.server,
            self.id
        );
        if let Err(error) = crate::amqp::get_amqp()
            .message_sent(recipients, notification)
            .await
        {
            log::warn!("Could not queue event start pushes: {error:?}");
        }
        Ok(())
    }
}

/// People read at once when a start is announced
pub(super) const ANNOUNCE_PAGE: i64 = 500;

/// What one crond pass did
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PassReport {
    pub started: usize,
    pub completed: usize,
    pub cancelled: usize,
    pub scheduled: usize,
    /// Ended while the server had events off: marked Completed, not announced
    pub missed: usize,
    pub failed: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_calendar_months_keep_the_day_or_take_the_months_last() {
        let at = |text: &str| Timestamp::parse(text).unwrap();
        let later = |text: &str| months_after(at(text), PEOPLE_RETENTION_MONTHS);
        assert_eq!(later("2027-08-31T21:30:15Z"), at("2028-08-31T21:30:15Z"));
        // 366 days in between, where a fixed 365 would be a day early
        assert_eq!(later("2027-03-01T00:00:00Z"), at("2028-03-01T00:00:00Z"));
        // ended on a leap day: the last day of February a year on
        assert_eq!(later("2028-02-29T10:00:00Z"), at("2029-02-28T10:00:00Z"));
        // the 28th stays the 28th, also into a leap year
        assert_eq!(later("2027-02-28T10:00:00Z"), at("2028-02-28T10:00:00Z"));
        // the last moment of a year, and of a month
        assert_eq!(later("2026-12-31T23:59:59Z"), at("2027-12-31T23:59:59Z"));
        assert_eq!(later("2027-01-31T23:59:59.500Z"), at("2028-01-31T23:59:59.500Z"));
        // months that are shorter than the day asked for
        assert_eq!(months_after(at("2027-08-31T12:00:00Z"), 6), at("2028-02-29T12:00:00Z"));
        assert_eq!(months_after(at("2027-10-31T12:00:00Z"), 1), at("2027-11-30T12:00:00Z"));

        let mut event = ServerEvent::new(
            "S",
            "U",
            "T".into(),
            None,
            at("2028-02-29T08:00:00Z"),
            None,
            ServerEventPlace::External { location: "x".into() },
            None,
        );
        // not ended: never by time
        assert_eq!(event.people_kept_until(), None);
        event.status = ServerEventStatus::Active;
        event.ended_at = Some(at("2028-02-29T10:00:00Z"));
        assert_eq!(event.people_kept_until(), None);
        // ended or called off: from that moment
        for status in [ServerEventStatus::Completed, ServerEventStatus::Cancelled] {
            event.status = status;
            assert_eq!(event.people_kept_until(), Some(at("2029-02-28T10:00:00Z")));
        }
        // ended without a known time: kept
        event.ended_at = None;
        assert_eq!(event.people_kept_until(), None);
    }

    #[test]
    fn daily_repeats_keep_the_local_time() {
        let first = Timestamp::parse("2027-01-30T18:00:00Z").unwrap();
        let daily = |n| occurrence_start(first, n, &ServerEventRepeat::Daily, None);
        assert_eq!(daily(1), Timestamp::parse("2027-01-31T18:00:00Z").unwrap());
        assert_eq!(daily(2), Timestamp::parse("2027-02-01T18:00:00Z").unwrap());
        assert_eq!(daily(30), Timestamp::parse("2027-03-01T18:00:00Z").unwrap());
    }

    #[test]
    fn the_next_id_is_the_same_every_time_and_differs_from_the_last() {
        let first = Ulid::new().to_string();
        let next = next_occurrence_id(&first);
        assert_eq!(next, next_occurrence_id(&first));
        assert_ne!(next, first);
        assert_ne!(next_occurrence_id(&next), next);
        assert!(Ulid::from_string(&next).is_ok());
    }

    #[test]
    fn occurrences_count_from_the_first_start() {
        let first = Timestamp::parse("2027-01-31T18:00:00Z").unwrap();
        let monthly = |n| occurrence_start(first, n, &ServerEventRepeat::Monthly, None);
        assert_eq!(monthly(1), Timestamp::parse("2027-02-28T18:00:00Z").unwrap());
        assert_eq!(monthly(2), Timestamp::parse("2027-03-31T18:00:00Z").unwrap());
        assert_eq!(monthly(13), Timestamp::parse("2028-02-29T18:00:00Z").unwrap());
        assert_eq!(monthly(11), Timestamp::parse("2027-12-31T18:00:00Z").unwrap());
        assert_eq!(
            occurrence_start(first, 3, &ServerEventRepeat::EveryTwoWeeks, None),
            Timestamp::parse("2027-03-14T18:00:00Z").unwrap()
        );
        assert_eq!(unix_seconds(from_unix_seconds(1_800_000_000)), 1_800_000_000);
    }
}
