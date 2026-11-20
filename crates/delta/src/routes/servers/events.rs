//! Scheduled server events.
//!
//! Members see the events of their server that they may see: the place
//! must be visible (an external place, or a channel where they have View
//! Channel), and an event limited to roles or to invited people is only for
//! those, its creator and its organisers. Everything else answers NotFound,
//! as if it were not there, and says nothing about the event, not even a
//! count. People who manage events also see events whose channel is gone,
//! to clean them up, and drafts.
//!
//! Organisers of an event are the people with Manage Events (Manage Server
//! implies it) and the event's named helpers (`co_organisers`). Creating,
//! copying and deleting events and naming helpers need the permission;
//! editing, publishing, starting, ending, cancelling and managing the people
//! of one event are open to its helpers too. Being the creator gives no
//! rights, and every organiser action is refused while the person is timed
//! out and leaves an entry in the server's audit log.
//!
//! The whole feature is behind `features.server_events_enabled`, and each
//! server starts with events off: while its `events_enabled` switch is off
//! every route answers FeatureDisabled (`server_events_server`) and crond
//! leaves its events alone.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_database::util::reference::Reference;
use revolt_database::util::tz::{civil_from_days, Zone};
use revolt_database::{
    clean_link_name, clean_reminders, code_of_link_path, new_invitation_code, new_link_code,
    unix_seconds, AuditLogEntryAction, Channel, Database, EventNotice, File, Member, PlaceState,
    Server, ServerEvent, ServerEventInterest, ServerEventInvitation, ServerEventRoster, User,
    DEFAULT_LENGTH_HOURS, MAX_CO_ORGANISERS, MAX_DAYS_AHEAD, MAX_DRAFT_EVENTS,
    MAX_INVITATION_LINKS, MAX_INVITED, MAX_LENGTH_DAYS, MAX_SEATS, MAX_UPCOMING_EVENTS,
    MAX_VISIBLE_ROLES,
};
use revolt_models::v0::{
    self, ServerEventAnswer, ServerEventAttendeeList, ServerEventStatus, ServerEventVisibility,
};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use rocket_empty::EmptyResponse;
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// A start time may be this far in the past (clocks differ a little)
const START_GRACE_MINUTES: i64 = 5;

async fn require_enabled() -> Result<()> {
    if config().await.features.server_events_enabled {
        Ok(())
    } else {
        Err(create_error!(FeatureDisabled {
            feature: "server_events".to_string()
        }))
    }
}

/// The server and your membership; NotFound for people outside it, and
/// FeatureDisabled while the server has events off (off until a manager
/// turns them on: nothing is listed, fetched or created, as in the app)
async fn membership(db: &Database, user: &User, target: &Reference<'_>) -> Result<(Server, Member)> {
    require_enabled().await?;
    let server = target.as_server(db).await?;
    let member = db
        .fetch_member(&server.id, &user.id)
        .await
        .map_err(|_| create_error!(NotFound))?;
    if !server.events_enabled {
        return Err(create_error!(FeatureDisabled {
            feature: "server_events_server".to_string()
        }));
    }
    Ok((server, member))
}

/// Manage Events (or Manage Server), and not timed out
async fn require_manager(
    db: &Database,
    user: &User,
    server: &Server,
    member: &Member,
) -> Result<()> {
    if !member.in_timeout() && ServerEvent::can_manage_events(db, user, server, member).await {
        Ok(())
    } else {
        Err(missing_permission())
    }
}

fn missing_permission() -> revolt_result::Error {
    create_error!(MissingPermission {
        permission: "ManageEvents".to_string()
    })
}

/// An event of this server and what you may do with it
struct Seen {
    event: ServerEvent,
    /// You hold Manage Events (whether or not you are timed out)
    manager: bool,
}

impl Seen {
    /// You manage this event: by permission or as one of its helpers
    fn organiser(&self, user: &User) -> bool {
        self.manager || self.event.is_co_organiser(&user.id)
    }

    /// Refuse unless you manage this event and are not timed out
    fn require_organiser(&self, user: &User, member: &Member) -> Result<()> {
        if !member.in_timeout() && self.organiser(user) {
            Ok(())
        } else {
            Err(missing_permission())
        }
    }

    /// Refuse unless you hold Manage Events and are not timed out
    fn require_manager(&self, member: &Member) -> Result<()> {
        if !member.in_timeout() && self.manager {
            Ok(())
        } else {
            Err(missing_permission())
        }
    }
}

/// An event of this server you can see; NotFound otherwise
async fn visible_event(
    db: &Database,
    user: &User,
    server: &Server,
    member: &Member,
    id: &str,
) -> Result<Seen> {
    let event = db.fetch_server_event(id).await?;
    if event.server != server.id {
        return Err(create_error!(NotFound));
    }
    let manager = ServerEvent::can_manage_events(db, user, server, member).await;
    let place = event.place_state(db).await;
    if event
        .visible_given(db, user, server, member, manager, &place)
        .await
        || (matches!(place, PlaceState::Gone) && manager)
    {
        Ok(Seen { event, manager })
    } else {
        Err(create_error!(NotFound))
    }
}

/// What decides who sees an event (helpers always do)
fn seen_by(
    event: &ServerEvent,
) -> (
    v0::ServerEventPlace,
    ServerEventVisibility,
    Vec<String>,
    Vec<String>,
    Vec<String>,
) {
    (
        event.place.clone(),
        event.visibility.clone(),
        event.visible_roles.clone(),
        event.invited.clone(),
        event.co_organisers.clone(),
    )
}

/// Someone asks to take their answer back from an event they can no longer
/// see (they lost the role, were uninvited, or the channel closed to them):
/// the answer goes, and they are told nothing about the event
async fn withdraw_unseen(db: &Database, user: &User, server: &Server, id: &str) -> Result<()> {
    let Ok(mut event) = db.fetch_server_event(id).await else {
        return Ok(());
    };
    if event.server == server.id && event.withdraw(db, &user.id).await? {
        event.publish_counts(db).await;
        event.publish_own_answer(&user.id, None).await;
    }
    Ok(())
}

/// Tell people on their phones; a notice that fails is logged, the request
/// it came from still succeeds
async fn notice(db: &Database, event: &ServerEvent, what: EventNotice, only: Option<Vec<String>>) {
    if let Err(error) = event.notify(db, what, only).await {
        log::warn!("Could not send a notice of event {}: {error:?}", event.id);
    }
}

/// The invited people of a scheduled event who were not invited before,
/// without the person who invited them
fn newly_invited(event: &ServerEvent, before: &[String], actor: &str) -> Vec<String> {
    if event.visibility != ServerEventVisibility::Invited || !event.is_upcoming() {
        return vec![];
    }
    event
        .invited
        .iter()
        .filter(|id| !before.contains(id) && id.as_str() != actor)
        .cloned()
        .collect()
}

/// The event as the API shows it. The invited list is for the organisers
/// who see the event's people: in an event limited to roles or invited
/// people, those named on it.
fn shown(event: ServerEvent, organiser: bool, viewer: &str) -> v0::ServerEvent {
    let invited = (organiser && event.shows_people_to_organiser(viewer))
        .then(|| event.invited.clone());
    let mut shown: v0::ServerEvent = event.into();
    shown.invited = invited;
    shown
}

/// The event with your own answer to it
async fn respond(
    db: &Database,
    viewer: &str,
    event: ServerEvent,
    organiser: bool,
    row: Option<ServerEventInterest>,
) -> Result<Json<v0::ServerEventResponse>> {
    let roster: Option<ServerEventRoster> = match &row {
        Some(row) if row.waiting => db.fetch_server_event_roster(&event.id).await?,
        _ => None,
    };
    Ok(Json(v0::ServerEventResponse {
        interested: row.as_ref().is_some_and(ServerEventInterest::takes_part),
        rsvp: row.map(|row| row.into_rsvp(roster.as_ref())),
        organiser,
        event: shown(event, organiser, viewer),
    }))
}

/// Your answers to these events, as the lists show them
fn own_answers(rows: Vec<ServerEventInterest>) -> (Vec<String>, Vec<v0::ServerEventOwnAnswer>) {
    let interested = rows
        .iter()
        .filter(|row| row.takes_part())
        .map(|row| row.id.event.clone())
        .collect();
    let answers = rows
        .into_iter()
        .map(|row| v0::ServerEventOwnAnswer {
            event: row.id.event,
            answer: row.answer,
            waiting: row.waiting,
        })
        .collect();
    (interested, answers)
}

/// A refused field of an event, with a reason the app can show in plain
/// words
fn bad(field: &str, reason: &str) -> revolt_result::Error {
    create_error!(ServerEventInvalid {
        field: field.to_string(),
        reason: reason.to_string()
    })
}

fn parse_time(field: &str, text: &str) -> Result<Timestamp> {
    Timestamp::parse(text.trim()).ok_or_else(|| bad(field, "not_a_time"))
}

/// A list of members of this server, without repeats, at most `max`
async fn check_people(
    db: &Database,
    server: &Server,
    field: &str,
    mut ids: Vec<String>,
    max: usize,
) -> Result<Vec<String>> {
    ids.sort();
    ids.dedup();
    if ids.len() > max {
        return Err(bad(field, "too_many"));
    }
    if ids.iter().any(|id| id.len() != 26) {
        return Err(bad(field, "not_a_member"));
    }
    if db.fetch_members(&server.id, &ids).await?.len() != ids.len() {
        return Err(bad(field, "not_a_member"));
    }
    Ok(ids)
}

/// The settings of an event that came with answers, seats and visibility
#[derive(Default)]
struct Settings {
    visibility: Option<ServerEventVisibility>,
    visible_roles: Option<Vec<String>>,
    invited: Option<Vec<String>>,
    attendee_list: Option<ServerEventAttendeeList>,
    capacity: Option<u32>,
    waitlist: Option<bool>,
    co_organisers: Option<Vec<String>>,
}

impl Settings {
    /// Check them and write them onto the event. Helpers are named only by
    /// people with Manage Events.
    async fn apply(
        self,
        db: &Database,
        server: &Server,
        event: &mut ServerEvent,
        manager: bool,
    ) -> Result<()> {
        if let Some(visibility) = self.visibility {
            event.visibility = visibility;
        }
        if let Some(mut roles) = self.visible_roles {
            roles.sort();
            roles.dedup();
            if roles.len() > MAX_VISIBLE_ROLES {
                return Err(bad("visible_roles", "too_many"));
            }
            if roles.iter().any(|role| !server.roles.contains_key(role)) {
                return Err(bad("visible_roles", "unknown_role"));
            }
            event.visible_roles = roles;
        }
        if let Some(invited) = self.invited {
            event.invited = check_people(db, server, "invited", invited, MAX_INVITED).await?;
        }
        // lists of a kind the event no longer has are not kept
        match event.visibility {
            ServerEventVisibility::Place => {
                event.visible_roles.clear();
                event.invited.clear();
            }
            ServerEventVisibility::Roles => {
                event.invited.clear();
                if event.visible_roles.is_empty() {
                    return Err(bad("visible_roles", "empty"));
                }
            }
            ServerEventVisibility::Invited => event.visible_roles.clear(),
        }
        if let Some(attendee_list) = self.attendee_list {
            event.attendee_list = attendee_list;
        }
        if let Some(capacity) = self.capacity {
            if capacity == 0 || capacity > MAX_SEATS {
                return Err(bad("capacity", "out_of_range"));
            }
            event.capacity = Some(capacity);
        }
        if let Some(waitlist) = self.waitlist {
            event.waitlist = waitlist;
        }
        if let Some(helpers) = self.co_organisers {
            if !manager {
                return Err(missing_permission());
            }
            event.co_organisers =
                check_people(db, server, "co_organisers", helpers, MAX_CO_ORGANISERS).await?;
        }
        Ok(())
    }
}

/// A place must be a channel of this server you can see (a voice channel
/// for a voice place), or an external place of 1 to 100 characters
async fn check_place(
    db: &Database,
    user: &User,
    server: &Server,
    member: &Member,
    place: v0::ServerEventPlace,
) -> Result<v0::ServerEventPlace> {
    match place {
        v0::ServerEventPlace::External { location } => {
            let location = location.trim().to_string();
            let length = location.chars().count();
            if length == 0 || length > 100 {
                return Err(create_error!(FailedValidation {
                    error: "place.location: 1 to 100 characters".to_string()
                }));
            }
            if !location_is_safe(&location) {
                return Err(create_error!(FailedValidation {
                    error: "place.location: only http and https links".to_string()
                }));
            }
            Ok(v0::ServerEventPlace::External { location })
        }
        place => {
            let id = place.channel().unwrap_or_default().to_string();
            let channel: Channel = db
                .fetch_channel(&id)
                .await
                .map_err(|_| create_error!(UnknownChannel))?;
            if !ServerEvent::channel_visible(db, user, server, member, &channel).await {
                return Err(create_error!(UnknownChannel));
            }
            let is_voice = channel.voice().is_some();
            match (&place, is_voice) {
                (v0::ServerEventPlace::Voice { .. }, false) => Err(create_error!(FailedValidation {
                    error: "place.channel: not a voice channel".to_string()
                })),
                (v0::ServerEventPlace::Text { .. }, _)
                    if !matches!(channel, Channel::TextChannel { .. }) =>
                {
                    Err(create_error!(FailedValidation {
                        error: "place.channel: not a text channel".to_string()
                    }))
                }
                _ => Ok(place),
            }
        }
    }
}

/// A place may be any text, but a link is only http or https: no
/// `javascript:`, `data:`, `file:` or other scheme (`Room 5: second floor`
/// is text, not a scheme)
pub fn location_is_safe(location: &str) -> bool {
    let lower = location.trim().to_ascii_lowercase();
    let Some((scheme, rest)) = lower.split_once(':') else {
        return true;
    };
    let looks_like_scheme = !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !looks_like_scheme {
        return true;
    }
    if matches!(scheme, "http" | "https") {
        return true;
    }
    // text like "Room 5: second floor" goes on after a space
    let dangerous = matches!(
        scheme,
        "javascript" | "data" | "vbscript" | "file" | "blob" | "about" | "intent"
    );
    !(dangerous || rest.starts_with("//") || !rest.starts_with(' '))
}

fn invalid(error: &str) -> revolt_result::Error {
    create_error!(FailedValidation {
        error: error.to_string()
    })
}

/// Start not in the past (a few minutes of grace) and within a year; end
/// after the start and at most a month later
pub fn check_times(
    starts_at: Timestamp,
    ends_at: Option<Timestamp>,
    now: Timestamp,
    check_start: bool,
) -> Result<()> {
    if check_start {
        if starts_at < now - Duration::minutes(START_GRACE_MINUTES) {
            return Err(invalid("starts_at: in the past"));
        }
        if starts_at > now + Duration::days(MAX_DAYS_AHEAD) {
            return Err(invalid("starts_at: more than a year ahead"));
        }
    }
    if let Some(end) = ends_at {
        if end <= starts_at {
            return Err(invalid("ends_at: not after the start"));
        }
        if end > starts_at + Duration::days(MAX_LENGTH_DAYS) {
            return Err(invalid("ends_at: longer than 31 days"));
        }
        if end <= now {
            return Err(invalid("ends_at: in the past"));
        }
    }
    Ok(())
}

/// A draft may hold any start; only the end must fit the start
fn check_draft_times(starts_at: Timestamp, ends_at: Option<Timestamp>) -> Result<()> {
    if let Some(end) = ends_at {
        if end <= starts_at {
            return Err(invalid("ends_at: not after the start"));
        }
        if end > starts_at + Duration::days(MAX_LENGTH_DAYS) {
            return Err(invalid("ends_at: longer than 31 days"));
        }
    }
    Ok(())
}

fn too_many_events() -> revolt_result::Error {
    create_error!(TooManyServerEvents {
        max: MAX_UPCOMING_EVENTS
    })
}

fn too_many_drafts() -> revolt_result::Error {
    create_error!(TooManyServerEventDrafts {
        max: MAX_DRAFT_EVENTS
    })
}

/// How many drafts, or scheduled and active events, a server has
async fn count_events(db: &Database, server: &str, draft: bool) -> Result<usize> {
    Ok(if draft {
        db.fetch_server_event_drafts(server).await?.len()
    } else {
        db.fetch_upcoming_server_events(server).await?.len()
    })
}

/// When an event is over for the clash check
fn clash_end(event: &ServerEvent) -> Timestamp {
    event.ends_at.unwrap_or_else(|| {
        event
            .starts_at
            .checked_add(Duration::hours(DEFAULT_LENGTH_HOURS))
            .unwrap_or(event.starts_at)
    })
}

fn clean_description(description: Option<String>) -> Option<String> {
    description
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// # Fetch Server Events
///
/// Scheduled and active events of this server that you may see, soonest
/// first, with your answers. `drafts=true` adds the drafts you manage. With
/// `from` or `to` the list is instead every event that starts in that time
/// (also past and cancelled ones; at most 500).
#[openapi(tag = "Server Events")]
#[get("/<target>/events?<options..>")]
pub async fn list(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: v0::OptionsServerEventList,
) -> Result<Json<v0::ServerEventList>> {
    options
        .validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (server, member) = membership(db, &user, &target).await?;
    let manager = ServerEvent::can_manage_events(db, &user, &server, &member).await;

    let mut candidates = if options.from.is_some() || options.to.is_some() {
        let now = Timestamp::now_utc();
        let from = match &options.from {
            Some(from) => parse_time("from", from)?,
            None => now - Duration::days(MAX_DAYS_AHEAD),
        };
        let to = match &options.to {
            Some(to) => parse_time("to", to)?,
            None => from + Duration::days(MAX_DAYS_AHEAD),
        };
        if to <= from || to > from + Duration::days(2 * MAX_DAYS_AHEAD) {
            return Err(bad("to", "out_of_range"));
        }
        db.fetch_server_events_between(std::slice::from_ref(&server.id), from, to, 500)
            .await?
    } else {
        db.fetch_upcoming_server_events(&server.id).await?
    };
    if options.drafts == Some(true) {
        candidates.extend(db.fetch_server_event_drafts(&server.id).await?);
    }

    let mut events = Vec::new();
    for event in candidates {
        let place = event.place_state(db).await;
        if event
            .visible_given(db, &user, &server, &member, manager, &place)
            .await
        {
            events.push(event);
        }
    }
    let ids: Vec<String> = events.iter().map(|event| event.id.clone()).collect();
    let (interested, answers) =
        own_answers(db.fetch_server_event_answers_of(&user.id, &ids).await?);
    Ok(Json(v0::ServerEventList {
        events: events
            .into_iter()
            .map(|event| {
                let organiser = manager || event.is_co_organiser(&user.id);
                shown(event, organiser, &user.id)
            })
            .collect(),
        interested,
        answers,
    }))
}

/// # Create Server Event
///
/// Schedule an event, or save it as a draft (`draft: true`). Needs Manage
/// Events.
#[openapi(tag = "Server Events")]
#[post("/<target>/events", data = "<data>")]
pub async fn create(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataCreateServerEvent>,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (server, member) = membership(db, &user, &target).await?;
    require_manager(db, &user, &server, &member).await?;
    let draft = data.draft == Some(true);

    let title = data.title.trim().to_string();
    if title.is_empty() {
        return Err(invalid("title: empty"));
    }
    if draft {
        check_draft_times(data.starts_at, data.ends_at)?;
    } else {
        check_times(data.starts_at, data.ends_at, Timestamp::now_utc(), true)?;
    }
    if let Some(zone) = &data.time_zone {
        if Zone::load(zone).is_none() && zone != "UTC" {
            return Err(invalid("time_zone: not a known time zone"));
        }
    }
    let (max, too_many): (usize, fn() -> revolt_result::Error) = if draft {
        (MAX_DRAFT_EVENTS, too_many_drafts)
    } else {
        (MAX_UPCOMING_EVENTS, too_many_events)
    };
    if count_events(db, &server.id, draft).await? >= max {
        return Err(too_many());
    }
    let place = check_place(db, &user, &server, &member, data.place).await?;

    let mut event = ServerEvent::new(
        &server.id,
        &user.id,
        title,
        clean_description(data.description),
        data.starts_at,
        data.ends_at,
        place,
        data.repeat,
    );
    event.time_zone = data.time_zone;
    if draft {
        event.status = ServerEventStatus::Draft;
    }
    Settings {
        visibility: data.visibility,
        visible_roles: data.visible_roles,
        invited: data.invited,
        attendee_list: data.attendee_list,
        capacity: data.capacity,
        waitlist: data.waitlist,
        co_organisers: data.co_organisers,
    }
    .apply(db, &server, &mut event, true)
    .await?;
    if let Some(reminders) = data.reminders {
        event.reminders = clean_reminders(reminders).map_err(invalid)?;
    }
    if let Some(cover) = data.cover {
        event.cover = Some(File::use_server_event_cover(db, &cover, &event.id, &user.id).await?);
    }
    db.insert_server_event(&event).await?;
    // counted again after writing: of two made at once, one is taken back
    if count_events(db, &server.id, draft).await? > max {
        db.delete_server_event(&event.id).await?;
        return Err(too_many());
    }
    db.insert_server_event_roster(&ServerEventRoster::new(
        &event.id,
        &event.server,
        event.capacity,
        event.waitlist,
    ))
    .await?;
    event.publish_create(db).await;
    let invited = newly_invited(&event, &[], &user.id);
    if !invited.is_empty() {
        notice(db, &event, EventNotice::Invited, Some(invited)).await;
    }

    AuditLogEntryAction::ServerEventCreate {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # Fetch Server Event
///
/// One event (also a past one) and your answer to it.
#[openapi(tag = "Server Events")]
#[get("/<target>/events/<event>")]
pub async fn fetch(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEventResponse>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    let organiser = seen.organiser(&user);
    let row = db
        .fetch_server_event_answer(&seen.event.id, &user.id)
        .await?;
    respond(db, &user.id, seen.event, organiser, row).await
}

/// # Edit Server Event
///
/// Change a draft, scheduled or active event. The start time can only
/// change before it starts. More seats (or none) move waiting people up at
/// once. Needs Manage Events, or being a helper of the event; only people
/// with Manage Events name helpers.
#[openapi(tag = "Server Events")]
#[patch("/<target>/events/<event>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    data: Json<v0::DataEditServerEvent>,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let manager = seen.manager;
    let mut event = seen.event;
    let draft = event.status == ServerEventStatus::Draft;
    if !event.is_upcoming() && !draft {
        return Err(create_error!(InvalidOperation));
    }
    let old_audience = event.audience(db).await;
    let seats_before = (event.capacity, event.waitlist);
    let seen_before = seen_by(&event);
    let (place_before, invited_before) = (event.place.clone(), event.invited.clone());

    for field in &data.remove {
        match field {
            v0::FieldsServerEvent::Description => event.description = None,
            v0::FieldsServerEvent::EndsAt => event.ends_at = None,
            v0::FieldsServerEvent::Cover => event.cover = None,
            v0::FieldsServerEvent::Repeat => event.repeat = None,
            v0::FieldsServerEvent::Capacity => event.capacity = None,
        }
    }
    if let Some(title) = data.title {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(invalid("title: empty"));
        }
        event.title = title;
    }
    if data.description.is_some() {
        event.description = clean_description(data.description);
    }
    let start_changed = match data.starts_at {
        Some(starts_at) if starts_at != event.starts_at => {
            if event.status == ServerEventStatus::Active {
                return Err(invalid("starts_at: the event has started"));
            }
            event.starts_at = starts_at;
            true
        }
        _ => false,
    };
    if data.ends_at.is_some() {
        event.ends_at = data.ends_at;
    }
    if draft {
        check_draft_times(event.starts_at, event.ends_at)?;
    } else {
        check_times(event.starts_at, event.ends_at, Timestamp::now_utc(), start_changed)?;
    }
    if let Some(place) = data.place {
        event.place = check_place(db, &user, &server, &member, place).await?;
    }
    if let Some(repeat) = data.repeat {
        event.repeat = Some(repeat);
    }
    Settings {
        visibility: data.visibility,
        visible_roles: data.visible_roles,
        invited: data.invited,
        attendee_list: data.attendee_list,
        capacity: data.capacity,
        waitlist: data.waitlist,
        co_organisers: data.co_organisers,
    }
    .apply(db, &server, &mut event, manager)
    .await?;
    if let Some(reminders) = data.reminders {
        event.reminders = clean_reminders(reminders).map_err(invalid)?;
    }
    if let Some(cover) = data.cover {
        event.cover = Some(File::use_server_event_cover(db, &cover, &event.id, &user.id).await?);
    }

    db.update_server_event(&event).await?;
    if start_changed {
        // a new start: every reminder counts from it again
        db.clear_server_event_reminders_sent(&event.id).await?;
    }
    let seats_changed = seats_before != (event.capacity, event.waitlist);
    if seats_changed {
        event.apply_seat_settings(db).await?;
    }
    // who the event is for, or where it is, changed: people who can no
    // longer see it do not stay counted or listed in it
    let people_changed = if seen_before != seen_by(&event) {
        event.drop_answers_of_people_shut_out(db).await?
    } else {
        false
    };
    let seats_changed = seats_changed || people_changed;
    // status and counts as stored now
    let event = db.fetch_server_event(&event.id).await?;
    event.publish_update(db, Some(old_audience)).await;
    if seats_changed {
        event.publish_counts(db).await;
    }
    // one edit, one notice: the people who are coming hear of a new time
    // or place; people invited by this edit hear that they are invited
    if event.is_upcoming() {
        if start_changed || event.place != place_before {
            notice(db, &event, EventNotice::Changed, None).await;
        }
        let invited = newly_invited(&event, &invited_before, &user.id);
        if !invited.is_empty() {
            notice(db, &event, EventNotice::Invited, Some(invited)).await;
        }
    }

    AuditLogEntryAction::ServerEventEdit {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # Publish Server Event
///
/// Schedule a draft: from now on the people it is for see it and it starts
/// at its time. The start must not be in the past. Needs Manage Events, or
/// being a helper of the event.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/publish")]
pub async fn publish(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let mut event = seen.event;
    if event.status != ServerEventStatus::Draft {
        return Err(create_error!(InvalidOperation));
    }
    check_times(event.starts_at, event.ends_at, Timestamp::now_utc(), true)?;
    if db.fetch_upcoming_server_events(&server.id).await?.len() >= MAX_UPCOMING_EVENTS {
        return Err(too_many_events());
    }
    if !db
        .set_server_event_status(
            &event.id,
            ServerEventStatus::Draft,
            ServerEventStatus::Scheduled,
            Timestamp::now_utc(),
        )
        .await?
    {
        return Err(create_error!(InvalidOperation));
    }
    event.status = ServerEventStatus::Scheduled;
    // counted again after writing: of two published at once, one goes back
    if db.fetch_upcoming_server_events(&server.id).await?.len() > MAX_UPCOMING_EVENTS {
        db.set_server_event_status(
            &event.id,
            ServerEventStatus::Scheduled,
            ServerEventStatus::Draft,
            Timestamp::now_utc(),
        )
        .await?;
        return Err(too_many_events());
    }
    event.publish_create(db).await;
    let invited = newly_invited(&event, &[], &user.id);
    if !invited.is_empty() {
        notice(db, &event, EventNotice::Invited, Some(invited)).await;
    }

    AuditLogEntryAction::ServerEventPublish {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # Copy Server Event
///
/// Make a draft with the same title, text, times, place, cover, repeat,
/// seats and visibility as this event (also a past one). Nobody's answer is
/// copied. Needs Manage Events.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/copy")]
pub async fn copy(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_manager(&member)?;
    let source = seen.event;
    if db.fetch_server_event_drafts(&server.id).await?.len() >= MAX_DRAFT_EVENTS {
        return Err(too_many_drafts());
    }

    let mut copy = ServerEvent::new(
        &server.id,
        &user.id,
        source.title.clone(),
        source.description.clone(),
        source.starts_at,
        source.ends_at,
        source.place.clone(),
        source.repeat.clone(),
    );
    copy.status = ServerEventStatus::Draft;
    copy.time_zone = source.time_zone.clone();
    copy.capacity = source.capacity;
    copy.waitlist = source.waitlist;
    copy.visibility = source.visibility.clone();
    copy.visible_roles = source.visible_roles.clone();
    copy.invited = source.invited.clone();
    copy.attendee_list = source.attendee_list.clone();
    copy.co_organisers = source.co_organisers.clone();
    copy.reminders = source.reminders.clone();
    if let Some(cover) = &source.cover {
        // its own use of the file, as each occurrence of a series has
        copy.cover = match File::copy_unused(db, cover, &user.id).await {
            Ok(file) => File::use_server_event_cover(db, &file.id, &copy.id, &user.id)
                .await
                .ok(),
            Err(_) => None,
        };
    }
    db.insert_server_event(&copy).await?;
    if db.fetch_server_event_drafts(&server.id).await?.len() > MAX_DRAFT_EVENTS {
        db.delete_server_event(&copy.id).await?;
        return Err(too_many_drafts());
    }
    db.insert_server_event_roster(&ServerEventRoster::new(
        &copy.id,
        &copy.server,
        copy.capacity,
        copy.waitlist,
    ))
    .await?;

    AuditLogEntryAction::ServerEventCreate {
        event: copy.id.clone(),
        title: copy.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(copy, true, &user.id)))
}

/// # Start Server Event
///
/// Start a scheduled event now (also before its time). The people taking
/// part are notified. Needs Manage Events, or being a helper of the event.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/start")]
pub async fn start(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let mut event = seen.event;
    if !event.start(db).await? {
        return Err(create_error!(InvalidOperation));
    }

    AuditLogEntryAction::ServerEventStart {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # End Server Event
///
/// End an active event now. A repeating event gets its next occurrence.
/// Needs Manage Events, or being a helper of the event.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/end")]
pub async fn end(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let mut event = seen.event;
    if !event.complete(db).await? {
        return Err(create_error!(InvalidOperation));
    }

    AuditLogEntryAction::ServerEventEnd {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # Cancel Server Event
///
/// Call off a scheduled or active event. It stays readable by its link.
/// For a repeating event only this occurrence is off and the next one is
/// scheduled; `series=true` stops the whole series. Needs Manage Events, or
/// being a helper of the event.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/cancel?<options..>")]
pub async fn cancel(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    options: v0::OptionsCancelServerEvent,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let mut event = seen.event;
    if !event.cancel(db, options.series == Some(true)).await? {
        return Err(create_error!(InvalidOperation));
    }

    AuditLogEntryAction::ServerEventCancel {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # Delete Server Event
///
/// Remove an event (also a draft) with everyone's answers and its seat
/// list for good. For a repeating event the series goes on with the next
/// occurrence; `series=true` deletes this occurrence and schedules nothing
/// next, so the series ends. Needs Manage Events.
#[openapi(tag = "Server Events")]
#[delete("/<target>/events/<event>?<options..>")]
pub async fn delete(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    options: v0::OptionsDeleteServerEvent,
    reason: AuditLogReason,
) -> Result<EmptyResponse> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_manager(&member)?;
    let event = seen.event;
    event.delete(db, options.series == Some(true)).await?;

    AuditLogEntryAction::ServerEventDelete {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(EmptyResponse)
}

/// The older "Interested" mark: marking means "maybe" for someone who has
/// not said they take part, unmarking withdraws the answer of someone who
/// does. A "going" answer is never lowered by it.
async fn set_interest(
    db: &Database,
    user: &User,
    target: &Reference<'_>,
    event: &str,
    interested: bool,
) -> Result<Json<v0::ServerEventResponse>> {
    let (server, member) = membership(db, user, target).await?;
    let seen = match visible_event(db, user, &server, &member, event).await {
        Ok(seen) => seen,
        Err(error) => {
            if !interested {
                withdraw_unseen(db, user, &server, event).await?;
            }
            return Err(error);
        }
    };
    let organiser = seen.organiser(user);
    let mut event = seen.event;
    if interested && !event.is_upcoming() {
        return Err(create_error!(InvalidOperation));
    }
    let before = db.fetch_server_event_answer(&event.id, &user.id).await?;
    let takes_part = before
        .as_ref()
        .is_some_and(ServerEventInterest::takes_part);
    // a repeat changes nothing and tells nobody
    let row = if interested == takes_part {
        before
    } else {
        let row = if interested {
            event.answer(db, &user.id, ServerEventAnswer::Maybe).await?
        } else {
            event.withdraw(db, &user.id).await?;
            None
        };
        told(db, &event, &user.id, row.clone()).await?;
        row
    };
    respond(db, &user.id, event, organiser, row).await
}

/// Tell who sees the event its new counts, and the person's own devices
/// their answer
async fn told(
    db: &Database,
    event: &ServerEvent,
    user: &str,
    row: Option<ServerEventInterest>,
) -> Result<()> {
    event.publish_counts(db).await;
    let roster = match &row {
        Some(row) if row.waiting => db.fetch_server_event_roster(&event.id).await?,
        _ => None,
    };
    event
        .publish_own_answer(user, row.map(|row| row.into_rsvp(roster.as_ref())))
        .await;
    Ok(())
}

/// # Mark Server Event Interested
///
/// The older form of answering "maybe" (`PUT rsvp`). Doing it twice, or
/// when you already said you are going, changes nothing.
#[openapi(tag = "Server Events")]
#[put("/<target>/events/<event>/interested")]
pub async fn mark_interested(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEventResponse>> {
    set_interest(db, &user, &target, event, true).await
}

/// # Unmark Server Event Interested
///
/// The older form of withdrawing your answer (`DELETE rsvp`).
#[openapi(tag = "Server Events")]
#[delete("/<target>/events/<event>/interested")]
pub async fn unmark_interested(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEventResponse>> {
    set_interest(db, &user, &target, event, false).await
}

/// Whether you may read the list of people taking part
async fn may_read_attendees(
    db: &Database,
    user: &User,
    event: &ServerEvent,
    organiser: bool,
) -> Result<bool> {
    Ok(organiser
        || match event.attendee_list {
            ServerEventAttendeeList::Everyone => true,
            ServerEventAttendeeList::Organisers => false,
            ServerEventAttendeeList::Participants => db
                .fetch_server_event_answer(&event.id, &user.id)
                .await?
                .is_some_and(|row| row.takes_part()),
        })
}

/// # Fetch Interested Members
///
/// The older form of the attendee list: members who are going or maybe
/// going, by user id, 50 per page (`limit` up to 100). Pass `next` of a
/// page as `after` for the next one. Empty when the event's list is not
/// open to you.
#[openapi(tag = "Server Events")]
#[get("/<target>/events/<event>/interested?<options..>")]
pub async fn interested(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    options: v0::OptionsServerEventInterested,
) -> Result<Json<v0::ServerEventInterestedPage>> {
    options
        .validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    // this older route has no way to give a reason: an organiser who is
    // not named on a restricted event reads it as any other viewer does
    let organiser = seen.organiser(&user) && seen.event.shows_people_to_organiser(&user.id);
    if !may_read_attendees(db, &user, &seen.event, organiser).await? {
        return Ok(Json(v0::ServerEventInterestedPage {
            users: vec![],
            next: None,
        }));
    }
    let limit = options.limit.unwrap_or(50).clamp(1, 100);
    // one more than asked tells whether a next page exists
    let mut ids = db
        .fetch_server_event_interested(&seen.event.id, options.after.as_deref(), limit + 1)
        .await?;
    let next = if ids.len() as i64 > limit {
        ids.truncate(limit as usize);
        ids.last().cloned()
    } else {
        None
    };
    let mut users = User::fetch_many_ids_as_mutuals(db, &user, &ids).await?;
    users.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(v0::ServerEventInterestedPage { users, next }))
}

/// # Answer Server Event
///
/// Say whether you are going, maybe going or not going. A second answer
/// replaces the first. When the event has a seat limit, "going" takes a
/// seat, or a place on the waiting list once every seat is taken (`rsvp`
/// then says `waiting` and your `position`); `ServerEventFull` when there
/// is no seat and no waiting list.
#[openapi(tag = "Server Events")]
#[put("/<target>/events/<event>/rsvp", data = "<data>")]
pub async fn rsvp_set(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    data: Json<v0::DataServerEventRsvp>,
) -> Result<Json<v0::ServerEventResponse>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    let organiser = seen.organiser(&user);
    let mut event = seen.event;
    if !event.is_upcoming() {
        return Err(create_error!(InvalidOperation));
    }
    let row = event.answer(db, &user.id, data.into_inner().answer).await?;
    told(db, &event, &user.id, row.clone()).await?;
    respond(db, &user.id, event, organiser, row).await
}

/// # Withdraw Server Event Answer
///
/// Take your answer back. A seat you held goes to the first person on the
/// waiting list at once.
#[openapi(tag = "Server Events")]
#[delete("/<target>/events/<event>/rsvp")]
pub async fn rsvp_remove(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEventResponse>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = match visible_event(db, &user, &server, &member, event).await {
        Ok(seen) => seen,
        Err(error) => {
            withdraw_unseen(db, &user, &server, event).await?;
            return Err(error);
        }
    };
    let organiser = seen.organiser(&user);
    let mut event = seen.event;
    if event.withdraw(db, &user.id).await? {
        told(db, &event, &user.id, None).await?;
    }
    respond(db, &user.id, event, organiser, None).await
}

/// # Fetch Server Event Attendees
///
/// The people taking part, by user id, 50 per page (`limit` up to 100).
/// Who may read it is the event's `attendee_list` setting; organisers
/// always may. `answer` narrows it: `Going` (seated), `Waiting`, `Maybe`,
/// or `NotGoing` (organisers only).
///
/// In an event limited to roles or invited people, "organisers" means the
/// people named on it (its creator and helpers). Someone else who manages
/// events, the server's owner too, gets the list only with a written reason
/// in `X-Audit-Log-Reason` (`ServerEventReasonRequired` without one), and
/// the access is noted in the audit log.
#[openapi(tag = "Server Events")]
#[get("/<target>/events/<event>/attendees?<options..>")]
pub async fn attendees(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    options: v0::OptionsServerEventAttendees,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEventAttendeesPage>> {
    options
        .validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    let manages = seen.organiser(&user);
    // an organiser who sees the event's people as a matter of course
    let mut organiser = manages && seen.event.shows_people_to_organiser(&user.id);
    let organisers_only = matches!(options.answer.as_deref(), Some("NotGoing"));
    let by_setting = may_read_attendees(db, &user, &seen.event, false).await?;
    if manages && !organiser && (!by_setting || organisers_only) {
        // manages events but is not named on this restricted event: only
        // with a reason, and the access is noted for its organisers
        let given = reason.0.as_deref().map(str::trim).unwrap_or_default();
        if given.is_empty() || member.in_timeout() {
            return Err(create_error!(ServerEventReasonRequired));
        }
        AuditLogEntryAction::ServerEventPeopleAccess {
            event: seen.event.id.clone(),
        }
        .insert(
            db,
            server.id.clone(),
            Some(given.to_string()),
            user.id.clone(),
            None,
        )
        .await;
        organiser = true;
    }
    if !organiser && !by_setting {
        return Err(missing_permission());
    }
    let (answers, waiting): (&[ServerEventAnswer], Option<bool>) = match options.answer.as_deref()
    {
        None => (&[ServerEventAnswer::Going, ServerEventAnswer::Maybe], None),
        Some("Going") => (&[ServerEventAnswer::Going], Some(false)),
        Some("Waiting") => (&[ServerEventAnswer::Going], Some(true)),
        Some("Maybe") => (&[ServerEventAnswer::Maybe], None),
        // saying no is between the person and the organisers
        Some("NotGoing") if organiser => (&[ServerEventAnswer::NotGoing], None),
        Some("NotGoing") => return Err(missing_permission()),
        Some(_) => return Err(bad("answer", "unknown")),
    };
    let limit = options.limit.unwrap_or(50).clamp(1, 100);
    // one more than asked tells whether a next page exists
    let mut rows = db
        .fetch_server_event_answers(
            &seen.event.id,
            answers,
            waiting,
            options.after.as_deref(),
            limit + 1,
        )
        .await?;
    let next = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|row| row.id.user.clone())
    } else {
        None
    };
    let ids: Vec<String> = rows.iter().map(|row| row.id.user.clone()).collect();
    let users = User::fetch_many_ids_as_mutuals(db, &user, &ids).await?;
    let attendees = rows
        .into_iter()
        .filter_map(|row| {
            let user = users.iter().find(|user| user.id == row.id.user)?.clone();
            Some(v0::ServerEventAttendee {
                user,
                answer: row.answer,
                waiting: row.waiting,
                // when someone answered is for the organisers
                since: organiser.then_some(row.created_at),
            })
        })
        .collect();
    Ok(Json(v0::ServerEventAttendeesPage { attendees, next }))
}

/// # Remove Server Event Attendee
///
/// Take a person's answer away (they can answer again). A seat they held
/// goes to the first person on the waiting list. Needs Manage Events, or
/// being a helper of the event.
#[openapi(tag = "Server Events")]
#[delete("/<target>/events/<event>/attendees/<attendee>")]
pub async fn attendee_remove(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    attendee: &str,
    reason: AuditLogReason,
) -> Result<EmptyResponse> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let mut event = seen.event;
    if !event.withdraw(db, attendee).await? {
        return Err(create_error!(NotFound));
    }
    told(db, &event, attendee, None).await?;

    AuditLogEntryAction::ServerEventAttendeeRemove {
        event: event.id.clone(),
        title: event.audit_title(),
        user: attendee.to_string(),
    }
    .insert(
        db,
        server.id.clone(),
        reason.0,
        user.id.clone(),
        Some(attendee.to_string()),
    )
    .await;

    Ok(EmptyResponse)
}

/// # Seat Server Event Attendee
///
/// Give a person on the waiting list a seat now. When every seat is taken
/// this is refused with `ServerEventOverCapacity`; an organiser who means
/// it repeats the request with `over_capacity=true`. The event then says
/// `over_capacity`, and the audit entry says the limit was passed. Needs
/// Manage Events, or being a helper of the event.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/attendees/<attendee>/seat?<options..>")]
pub async fn attendee_seat(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    attendee: &str,
    options: v0::OptionsSeatServerEventAttendee,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let mut event = seen.event;
    if !event.is_upcoming() {
        return Err(create_error!(InvalidOperation));
    }
    // first into a free seat: the seat list itself refuses when there is
    // none, in the same write, so nothing can slip in between
    let mut over_capacity = false;
    if !event.seat(db, attendee, false).await? {
        let roster = event.roster(db).await?;
        if !roster.is_waiting(attendee) {
            return Err(create_error!(InvalidOperation));
        }
        // every seat is taken: only on the organiser's word
        if options.over_capacity != Some(true) {
            return Err(create_error!(ServerEventOverCapacity {
                capacity: roster.seats() as u32
            }));
        }
        if !event.seat(db, attendee, true).await? {
            return Err(create_error!(InvalidOperation));
        }
        over_capacity = true;
    }
    event.publish_counts(db).await;
    if over_capacity {
        // everyone who sees the event learns that it is over its limit
        event.publish_update(db, None).await;
    }

    AuditLogEntryAction::ServerEventAttendeeSeat {
        event: event.id.clone(),
        title: event.audit_title(),
        user: attendee.to_string(),
        over_capacity,
    }
    .insert(
        db,
        server.id.clone(),
        reason.0,
        user.id.clone(),
        Some(attendee.to_string()),
    )
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # Find Clashing Server Events
///
/// Events of this server (scheduled, active, and the drafts you manage)
/// that overlap a time, and a place when `channel` or `location` is given.
/// Only events you may see are named. A warning for the editor: two events
/// at once are allowed.
#[openapi(tag = "Server Events")]
#[get("/<target>/events/clashes?<options..>")]
pub async fn clashes(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: v0::OptionsServerEventClashes,
) -> Result<Json<v0::ServerEventClashes>> {
    options
        .validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (server, member) = membership(db, &user, &target).await?;
    let manager = ServerEvent::can_manage_events(db, &user, &server, &member).await;
    let starts_at = parse_time("starts_at", &options.starts_at)?;
    let ends_at = match &options.ends_at {
        Some(end) => parse_time("ends_at", end)?,
        None => starts_at + Duration::hours(DEFAULT_LENGTH_HOURS),
    };
    if ends_at <= starts_at {
        return Err(bad("ends_at", "out_of_range"));
    }
    let location = options
        .location
        .as_deref()
        .map(|location| location.trim().to_lowercase());

    let mut candidates = db.fetch_upcoming_server_events(&server.id).await?;
    candidates.extend(db.fetch_server_event_drafts(&server.id).await?);
    let mut events = Vec::new();
    for event in candidates {
        if options.exclude.as_deref() == Some(event.id.as_str())
            || event.starts_at >= ends_at
            || clash_end(&event) <= starts_at
        {
            continue;
        }
        let same_place = match (&options.channel, &location, &event.place) {
            (None, None, _) => true,
            (Some(channel), _, place) if place.channel() == Some(channel.as_str()) => true,
            (_, Some(location), v0::ServerEventPlace::External { location: other }) => {
                &other.trim().to_lowercase() == location
            }
            _ => false,
        };
        if !same_place {
            continue;
        }
        let place = event.place_state(db).await;
        if event
            .visible_given(db, &user, &server, &member, manager, &place)
            .await
        {
            let organiser = manager || event.is_co_organiser(&user.id);
            events.push(shown(event, organiser, &user.id));
        }
    }
    Ok(Json(v0::ServerEventClashes { events }))
}

/// Rows read at once for the statistics, and how many such pages at most
const STATS_PAGE: i64 = 1000;
const STATS_PAGES: usize = 50;

/// # Fetch Server Event Statistics
///
/// Numbers of one event for its organisers: the counts per answer, the
/// waiting list, the answers by the day they were first given, and for a
/// voice place how many people were in the channel at the start. Needs
/// Manage Events, or being a helper of the event.
#[openapi(tag = "Server Events")]
#[get("/<target>/events/<event>/stats")]
pub async fn stats(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEventStats>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let event = seen.event;

    let zone = event
        .time_zone
        .as_deref()
        .and_then(Zone::load)
        .unwrap_or_else(Zone::utc);
    let mut days: std::collections::BTreeMap<i64, (u32, u32, u32)> = Default::default();
    let mut after: Option<String> = None;
    for _ in 0..STATS_PAGES {
        let rows = db
            .fetch_server_event_answers(
                &event.id,
                &[
                    ServerEventAnswer::Going,
                    ServerEventAnswer::Maybe,
                    ServerEventAnswer::NotGoing,
                ],
                None,
                after.as_deref(),
                STATS_PAGE,
            )
            .await?;
        let full = rows.len() as i64 == STATS_PAGE;
        after = rows.last().map(|row| row.id.user.clone());
        for row in rows {
            let day = zone
                .to_local(unix_seconds(row.created_at))
                .div_euclid(86_400);
            let counts = days.entry(day).or_default();
            match row.answer {
                ServerEventAnswer::Going => counts.0 += 1,
                ServerEventAnswer::Maybe => counts.1 += 1,
                ServerEventAnswer::NotGoing => counts.2 += 1,
            }
        }
        if !full {
            break;
        }
    }
    let registrations = days
        .into_iter()
        .map(|(day, (going, maybe, not_going))| {
            let (year, month, date) = civil_from_days(day);
            v0::ServerEventStatsDay {
                day: format!("{year:04}-{month:02}-{date:02}"),
                going,
                maybe,
                not_going,
            }
        })
        .collect();

    let present_at_start = event.present_at_start;
    let shown: v0::ServerEvent = event.into();
    Ok(Json(v0::ServerEventStats {
        counts: shown.counts,
        capacity: shown.capacity,
        registrations,
        present_at_start,
        started_at: shown.started_at,
        ended_at: shown.ended_at,
    }))
}

/// How far back the list of your events starts when no `from` is given: an
/// event that began earlier today is still worth showing
const MINE_DEFAULT_BACK_HOURS: i64 = 24;
/// Events read at most before the ones you may not see are left out
const MINE_SCAN: i64 = 1000;

/// # Fetch My Server Events
///
/// Events across all your servers that you may see and that start in a
/// time range (default: from a day ago to 90 days ahead), soonest first,
/// with your answers. `answer` keeps only the events you answered that
/// way. Servers with events switched off are left out.
#[openapi(tag = "Server Events")]
#[get("/@me/events?<options..>")]
pub async fn mine(
    db: &State<Database>,
    user: User,
    options: v0::OptionsMyServerEvents,
) -> Result<Json<v0::MyServerEvents>> {
    options
        .validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    require_enabled().await?;
    let now = Timestamp::now_utc();
    let from = match &options.from {
        Some(from) => parse_time("from", from)?,
        None => now - Duration::hours(MINE_DEFAULT_BACK_HOURS),
    };
    let to = match &options.to {
        Some(to) => parse_time("to", to)?,
        None => from + Duration::days(90),
    };
    if to <= from || to > from + Duration::days(MAX_DAYS_AHEAD) {
        return Err(bad("to", "out_of_range"));
    }
    let wanted: Option<(Option<ServerEventAnswer>, Option<bool>)> = match options.answer.as_deref()
    {
        None => None,
        Some("Any") => Some((None, None)),
        Some("Going") => Some((Some(ServerEventAnswer::Going), Some(false))),
        Some("Waiting") => Some((Some(ServerEventAnswer::Going), Some(true))),
        Some("Maybe") => Some((Some(ServerEventAnswer::Maybe), None)),
        Some("NotGoing") => Some((Some(ServerEventAnswer::NotGoing), None)),
        Some(_) => return Err(bad("answer", "unknown")),
    };
    let limit = options.limit.unwrap_or(100).clamp(1, 200) as usize;

    let memberships = db.fetch_all_memberships(&user.id).await?;
    let server_ids: Vec<String> = memberships
        .iter()
        .map(|member| member.id.server.clone())
        .collect();
    let servers: Vec<Server> = db
        .fetch_servers(&server_ids)
        .await?
        .into_iter()
        .filter(|server| server.events_enabled)
        .collect();
    let server_ids: Vec<String> = servers.iter().map(|server| server.id.clone()).collect();

    let candidates = db
        .fetch_server_events_between(&server_ids, from, to, MINE_SCAN)
        .await?;
    let ids: Vec<String> = candidates.iter().map(|event| event.id.clone()).collect();
    let rows = db.fetch_server_event_answers_of(&user.id, &ids).await?;

    let mut managers: std::collections::HashMap<String, bool> = Default::default();
    let mut events = Vec::new();
    let mut kept_rows = Vec::new();
    for event in candidates {
        if events.len() >= limit {
            break;
        }
        let row = rows.iter().find(|row| row.id.event == event.id);
        if let Some((answer, waiting)) = &wanted {
            let matches = row.is_some_and(|row| {
                answer.is_none_or(|answer| row.answer == answer)
                    && waiting.is_none_or(|waiting| row.waiting == waiting)
            });
            if !matches {
                continue;
            }
        }
        let (Some(server), Some(member)) = (
            servers.iter().find(|server| server.id == event.server),
            memberships
                .iter()
                .find(|member| member.id.server == event.server),
        ) else {
            continue;
        };
        let manager = match managers.get(&server.id) {
            Some(manager) => *manager,
            None => {
                let manager = ServerEvent::can_manage_events(db, &user, server, member).await;
                managers.insert(server.id.clone(), manager);
                manager
            }
        };
        let place = event.place_state(db).await;
        if !event
            .visible_given(db, &user, server, member, manager, &place)
            .await
        {
            continue;
        }
        if let Some(row) = row {
            kept_rows.push(row.clone());
        }
        let organiser = manager || event.is_co_organiser(&user.id);
        events.push(shown(event, organiser, &user.id));
    }
    let (_, answers) = own_answers(kept_rows);
    Ok(Json(v0::MyServerEvents { events, answers }))
}

/// # Set Server Event Notices
///
/// Switch your notices of one event off or on: its reminders and the
/// notices that its time or place changed or that it was called off. You
/// need an answer to the event first (the notices go to people who are
/// going or maybe going). Kinds of notices are switched off for every event
/// in your synced setting `event_notices`.
#[openapi(tag = "Server Events")]
#[put("/<target>/events/<event>/notices", data = "<data>")]
pub async fn notices_set(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    data: Json<v0::DataServerEventNotices>,
) -> Result<Json<v0::ServerEventResponse>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    let organiser = seen.organiser(&user);
    let event = seen.event;
    if !db
        .set_server_event_answer_muted(&event.id, &user.id, data.muted)
        .await?
    {
        return Err(create_error!(InvalidOperation));
    }
    let row = db.fetch_server_event_answer(&event.id, &user.id).await?;
    let roster = match &row {
        Some(row) if row.waiting => db.fetch_server_event_roster(&event.id).await?,
        _ => None,
    };
    event
        .publish_own_answer(
            &user.id,
            row.clone().map(|row| row.into_rsvp(roster.as_ref())),
        )
        .await;
    respond(db, &user.id, event, organiser, row).await
}

/// # Set Server Event Link
///
/// Give the event a share link (`<invite host>/e/<name>-<code>`), choose
/// the name in it, and decide whether people outside the server see a
/// little of the event through it: the title, the time, the cover and the
/// server's name, never a count or a person. The code is made once and
/// stays until the link is removed. Needs Manage Events, or being a helper
/// of the event.
#[openapi(tag = "Server Events")]
#[put("/<target>/events/<event>/link", data = "<data>")]
pub async fn link_set(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    data: Json<v0::DataServerEventLink>,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let mut event = seen.event;
    if data.public == Some(true) && !config().await.features.server_event_public_links {
        return Err(create_error!(FeatureDisabled {
            feature: "server_event_public_links".to_string()
        }));
    }
    let mut link = event.link.clone().unwrap_or_else(|| v0::ServerEventLink {
        code: new_link_code(),
        name: None,
    });
    if let Some(name) = &data.name {
        link.name = clean_link_name(name);
    }
    event.link = Some(link);
    if let Some(public) = data.public {
        event.link_public = public;
    }
    db.update_server_event(&event).await?;
    let event = db.fetch_server_event(&event.id).await?;
    event.publish_update(db, None).await;

    AuditLogEntryAction::ServerEventEdit {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # Remove Server Event Link
///
/// Take the share link away: it stops working at once, also for people
/// outside the server. A link made later has a new code.
#[openapi(tag = "Server Events")]
#[delete("/<target>/events/<event>/link")]
pub async fn link_remove(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let mut event = seen.event;
    event.link = None;
    event.link_public = false;
    db.update_server_event(&event).await?;
    let event = db.fetch_server_event(&event.id).await?;
    event.publish_update(db, None).await;

    AuditLogEntryAction::ServerEventEdit {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(shown(event, true, &user.id)))
}

/// # Create Server Event Invitation
///
/// Make an invitation link for an event that is for invited people: a
/// member of the server who opens it joins the invited people. It can run
/// out after a number of uses or a time; an event has at most 10 of them.
/// Needs Manage Events, or being a helper of the event.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/invitations", data = "<data>")]
pub async fn invitation_create(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    data: Json<v0::DataCreateServerEventInvitation>,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEventInvitation>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    let event = seen.event;
    if event.visibility != ServerEventVisibility::Invited {
        return Err(bad("invitations", "not_invited_only"));
    }
    if !event.is_upcoming() && event.status != ServerEventStatus::Draft {
        return Err(create_error!(InvalidOperation));
    }
    let now = Timestamp::now_utc();
    let invitation = ServerEventInvitation {
        code: new_invitation_code(),
        creator: user.id.clone(),
        uses: 0,
        max_uses: data.max_uses,
        expires_at: data
            .expires_in
            .and_then(|seconds| now.checked_add(Duration::seconds(seconds as i64))),
        created_at: now,
    };
    if !db
        .add_server_event_invitation(&event.id, &invitation, MAX_INVITATION_LINKS)
        .await?
    {
        return Err(bad("invitations", "too_many"));
    }

    AuditLogEntryAction::ServerEventEdit {
        event: event.id.clone(),
        title: event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(invitation.into()))
}

/// # Fetch Server Event Invitations
///
/// The invitation links of an event, with how often each was used. Needs
/// Manage Events, or being a helper of the event.
#[openapi(tag = "Server Events")]
#[get("/<target>/events/<event>/invitations")]
pub async fn invitations(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEventInvitationList>> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    Ok(Json(v0::ServerEventInvitationList {
        invitations: seen.event.invitations.into_iter().map(Into::into).collect(),
    }))
}

/// # Delete Server Event Invitation
///
/// Take an invitation link away: it stops working at once. People who used
/// it stay invited.
#[openapi(tag = "Server Events")]
#[delete("/<target>/events/<event>/invitations/<code>")]
pub async fn invitation_delete(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    code: &str,
    reason: AuditLogReason,
) -> Result<EmptyResponse> {
    let (server, member) = membership(db, &user, &target).await?;
    let seen = visible_event(db, &user, &server, &member, event).await?;
    seen.require_organiser(&user, &member)?;
    if !db
        .remove_server_event_invitation(&seen.event.id, code)
        .await?
    {
        return Err(create_error!(NotFound));
    }

    AuditLogEntryAction::ServerEventEdit {
        event: seen.event.id.clone(),
        title: seen.event.audit_title(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(EmptyResponse)
}

/// # Fetch Server Event By Link
///
/// What a share link (`/e/<name>-<code>`; `path` is the part after `/e/`)
/// leads to. A member of the server who can see the event gets its title,
/// time, cover, the server's name and the ids to open it. Anyone else, also
/// without signing in, gets the title, the time, the cover and the server's
/// name, and only when an organiser opened the link to people outside the
/// server; otherwise NotFound, as if the link did not exist. Counts and
/// people are never part of this.
#[openapi(tag = "Server Events")]
#[get("/event/<path>")]
pub async fn link_fetch(
    db: &State<Database>,
    user: Option<User>,
    path: &str,
) -> Result<Json<v0::ServerEventLinkPreview>> {
    require_enabled().await?;
    let code = code_of_link_path(path).ok_or_else(|| create_error!(NotFound))?;
    let event = db
        .fetch_server_event_by_link(code)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    let server = db
        .fetch_server(&event.server)
        .await
        .map_err(|_| create_error!(NotFound))?;
    if !server.events_enabled {
        return Err(create_error!(NotFound));
    }
    let mut sees = false;
    if let Some(user) = &user {
        if let Ok(member) = db.fetch_member(&server.id, &user.id).await {
            let manager = ServerEvent::can_manage_events(db, user, &server, &member).await;
            let place = event.place_state(db).await;
            sees = event
                .visible_given(db, user, &server, &member, manager, &place)
                .await;
        }
    }
    // outside the server: only an opened link, never of a mature server
    let open = event.open_to_link_holders()
        && config().await.features.server_event_public_links
        && !server.nsfw;
    if !sees && !open {
        return Err(create_error!(NotFound));
    }
    Ok(Json(v0::ServerEventLinkPreview {
        title: event.title.clone(),
        starts_at: event.starts_at,
        ends_at: event.ends_at,
        cover: event.cover.clone().map(Into::into),
        status: event.status.clone(),
        server_name: server.name.clone(),
        server: sees.then(|| server.id.clone()),
        event: sees.then(|| event.id.clone()),
    }))
}

/// # Accept Server Event Invitation
///
/// Use an invitation link: you join the invited people of its event and
/// see it from now on. You must be a member of the event's server
/// (`InvalidOperation` otherwise: join the server first). A link that ran
/// out, was taken away or never existed answers NotFound. Using a link of
/// an event you can already see changes nothing and does not count.
#[openapi(tag = "Server Events")]
#[post("/event/<code>")]
pub async fn invitation_accept(
    db: &State<Database>,
    user: User,
    code: &str,
) -> Result<Json<v0::ServerEventInvitationAccepted>> {
    require_enabled().await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let event = db
        .fetch_server_event_by_invitation(code)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    // the link itself; whether it still works is asked below, after
    // someone who already sees the event got their answer (opening a used-up
    // link again must not say "not found" to the person who used it)
    let invitation = event
        .invitations
        .iter()
        .find(|link| link.code == code)
        .cloned()
        .ok_or_else(|| create_error!(NotFound))?;
    let upcoming = event.is_upcoming();
    if !upcoming || event.visibility != ServerEventVisibility::Invited {
        return Err(create_error!(NotFound));
    }
    let server = db
        .fetch_server(&event.server)
        .await
        .map_err(|_| create_error!(NotFound))?;
    if !server.events_enabled {
        return Err(create_error!(NotFound));
    }
    let Ok(member) = db.fetch_member(&server.id, &user.id).await else {
        // outside the server: "join first" only for a link that still works
        return Err(if invitation.works(Timestamp::now_utc()) {
            create_error!(InvalidOperation)
        } else {
            create_error!(NotFound)
        });
    };
    let accepted = v0::ServerEventInvitationAccepted {
        server: server.id.clone(),
        event: event.id.clone(),
    };
    let manager = ServerEvent::can_manage_events(db, &user, &server, &member).await;
    let place = event.place_state(db).await;
    if event
        .visible_given(db, &user, &server, &member, manager, &place)
        .await
    {
        return Ok(Json(accepted));
    }
    if !invitation.works(Timestamp::now_utc()) {
        return Err(create_error!(NotFound));
    }
    if !db
        .use_server_event_invitation(
            &event.id,
            code,
            invitation.max_uses,
            &user.id,
            MAX_INVITED,
        )
        .await?
    {
        return Err(create_error!(NotFound));
    }
    let event = db.fetch_server_event(&event.id).await?;
    // the place may still be closed to them: then they are invited, and
    // see the event once they can see its channel
    if event
        .visible_given(db, &user, &server, &member, manager, &place)
        .await
    {
        event.publish_to(&user.id).await;
    }
    Ok(Json(accepted))
}
