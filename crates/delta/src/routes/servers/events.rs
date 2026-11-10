//! Scheduled server events.
//!
//! Members see the events of their server whose place they can see (an
//! external place, or a channel where they have View Channel); everything
//! else answers NotFound, as if it were not there. Creating, editing,
//! starting, ending, cancelling and deleting need Manage Server; the creator
//! of an event may also edit, start, end and cancel it while still a member.
//! The whole feature is behind `features.server_events_enabled`.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::util::reference::Reference;
use revolt_database::{
    AuditLogEntryAction, Channel, Database, File, Member, Server, ServerEvent, User,
    MAX_DAYS_AHEAD, MAX_LENGTH_DAYS, MAX_UPCOMING_EVENTS,
};
use revolt_database::events::client::EventV1;
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
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

/// The server and your membership; NotFound for people outside it
async fn membership(db: &Database, user: &User, target: &Reference<'_>) -> Result<(Server, Member)> {
    require_enabled().await?;
    let server = target.as_server(db).await?;
    let member = db
        .fetch_member(&server.id, &user.id)
        .await
        .map_err(|_| create_error!(NotFound))?;
    Ok((server, member))
}

async fn has_manage_server(db: &Database, user: &User, server: &Server, member: &Member) -> bool {
    let mut query = DatabasePermissionQuery::new(db, user)
        .server(server)
        .member(member);
    calculate_server_permissions(&mut query)
        .await
        .has_channel_permission(ChannelPermission::ManageServer)
}

/// Manage Server, or the event's own creator
async fn require_manager(
    db: &Database,
    user: &User,
    server: &Server,
    member: &Member,
    event: Option<&ServerEvent>,
) -> Result<()> {
    if event.is_some_and(|event| event.creator == user.id)
        || has_manage_server(db, user, server, member).await
    {
        Ok(())
    } else {
        Err(create_error!(MissingPermission {
            permission: "ManageServer".to_string()
        }))
    }
}

/// An event of this server you can see; NotFound otherwise
async fn visible_event(
    db: &Database,
    user: &User,
    server: &Server,
    member: &Member,
    id: &str,
) -> Result<ServerEvent> {
    let event = db.fetch_server_event(id).await?;
    if event.server != server.id || !event.visible_to(db, user, server, member).await {
        return Err(create_error!(NotFound));
    }
    Ok(event)
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

fn clean_description(description: Option<String>) -> Option<String> {
    description
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// # Fetch Server Events
///
/// Scheduled and active events of this server whose place you can see,
/// soonest first, with the ones you marked "Interested".
#[openapi(tag = "Server Events")]
#[get("/<target>/events")]
pub async fn list(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::ServerEventList>> {
    let (server, member) = membership(db, &user, &target).await?;
    let mut events = Vec::new();
    for event in db.fetch_upcoming_server_events(&server.id).await? {
        if event.visible_to(db, &user, &server, &member).await {
            events.push(event);
        }
    }
    let ids: Vec<String> = events.iter().map(|event| event.id.clone()).collect();
    let interested = db.fetch_server_event_interests_of(&user.id, &ids).await?;
    Ok(Json(v0::ServerEventList {
        events: events.into_iter().map(Into::into).collect(),
        interested,
    }))
}

/// # Create Server Event
///
/// Schedule an event. Needs Manage Server.
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
    require_manager(db, &user, &server, &member, None).await?;

    let title = data.title.trim().to_string();
    if title.is_empty() {
        return Err(invalid("title: empty"));
    }
    check_times(data.starts_at, data.ends_at, Timestamp::now_utc(), true)?;
    if db.fetch_upcoming_server_events(&server.id).await?.len() >= MAX_UPCOMING_EVENTS {
        return Err(create_error!(TooManyServerEvents {
            max: MAX_UPCOMING_EVENTS
        }));
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
    if let Some(cover) = data.cover {
        event.cover = Some(File::use_server_event_cover(db, &cover, &event.id, &user.id).await?);
    }
    db.insert_server_event(&event).await?;
    event.publish_create().await;

    AuditLogEntryAction::ServerEventCreate {
        event: event.id.clone(),
        title: event.title.clone(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(event.into()))
}

/// # Fetch Server Event
///
/// One event (also a past one) and whether you marked it "Interested".
#[openapi(tag = "Server Events")]
#[get("/<target>/events/<event>")]
pub async fn fetch(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEventResponse>> {
    let (server, member) = membership(db, &user, &target).await?;
    let event = visible_event(db, &user, &server, &member, event).await?;
    let interested = !db
        .fetch_server_event_interests_of(&user.id, std::slice::from_ref(&event.id))
        .await?
        .is_empty();
    Ok(Json(v0::ServerEventResponse {
        event: event.into(),
        interested,
    }))
}

/// # Edit Server Event
///
/// Change a scheduled or active event. The start time can only change
/// while it is scheduled. Needs Manage Server, or being its creator.
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
    let mut event = visible_event(db, &user, &server, &member, event).await?;
    require_manager(db, &user, &server, &member, Some(&event)).await?;
    if !event.is_upcoming() {
        return Err(create_error!(InvalidOperation));
    }
    let old_topic = event.topic();

    for field in &data.remove {
        match field {
            v0::FieldsServerEvent::Description => event.description = None,
            v0::FieldsServerEvent::EndsAt => event.ends_at = None,
            v0::FieldsServerEvent::Cover => event.cover = None,
            v0::FieldsServerEvent::Repeat => event.repeat = None,
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
            if event.status != v0::ServerEventStatus::Scheduled {
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
    check_times(event.starts_at, event.ends_at, Timestamp::now_utc(), start_changed)?;
    if let Some(place) = data.place {
        event.place = check_place(db, &user, &server, &member, place).await?;
    }
    if let Some(repeat) = data.repeat {
        event.repeat = Some(repeat);
    }
    if let Some(cover) = data.cover {
        event.cover = Some(File::use_server_event_cover(db, &cover, &event.id, &user.id).await?);
    }

    db.update_server_event(&event).await?;
    // status and count as stored now
    let event = db.fetch_server_event(&event.id).await?;
    event.publish_update(Some(old_topic)).await;

    AuditLogEntryAction::ServerEventEdit {
        event: event.id.clone(),
        title: event.title.clone(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(event.into()))
}

/// # Start Server Event
///
/// Start a scheduled event now (also before its time). Interested members
/// are notified. Needs Manage Server, or being its creator.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/start")]
pub async fn start(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let mut event = visible_event(db, &user, &server, &member, event).await?;
    require_manager(db, &user, &server, &member, Some(&event)).await?;
    if !event.start(db).await? {
        return Err(create_error!(InvalidOperation));
    }
    Ok(Json(event.into()))
}

/// # End Server Event
///
/// End an active event now. A repeating event gets its next occurrence.
/// Needs Manage Server, or being its creator.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/end")]
pub async fn end(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let mut event = visible_event(db, &user, &server, &member, event).await?;
    require_manager(db, &user, &server, &member, Some(&event)).await?;
    if !event.complete(db).await? {
        return Err(create_error!(InvalidOperation));
    }
    Ok(Json(event.into()))
}

/// # Cancel Server Event
///
/// Call off a scheduled or active event. It stays readable by its link.
/// Needs Manage Server, or being its creator.
#[openapi(tag = "Server Events")]
#[post("/<target>/events/<event>/cancel")]
pub async fn cancel(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    reason: AuditLogReason,
) -> Result<Json<v0::ServerEvent>> {
    let (server, member) = membership(db, &user, &target).await?;
    let mut event = visible_event(db, &user, &server, &member, event).await?;
    require_manager(db, &user, &server, &member, Some(&event)).await?;
    if !event.cancel(db).await? {
        return Err(create_error!(InvalidOperation));
    }

    AuditLogEntryAction::ServerEventCancel {
        event: event.id.clone(),
        title: event.title.clone(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(Json(event.into()))
}

/// # Delete Server Event
///
/// Remove an event and its interested list for good. Needs Manage Server.
#[openapi(tag = "Server Events")]
#[delete("/<target>/events/<event>")]
pub async fn delete(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    event: &str,
    reason: AuditLogReason,
) -> Result<EmptyResponse> {
    let (server, member) = membership(db, &user, &target).await?;
    let event = visible_event(db, &user, &server, &member, event).await?;
    require_manager(db, &user, &server, &member, None).await?;
    db.delete_server_event(&event.id).await?;
    event.publish_delete().await;

    AuditLogEntryAction::ServerEventDelete {
        event: event.id.clone(),
        title: event.title.clone(),
    }
    .insert(db, server.id.clone(), reason.0, user.id.clone(), None)
    .await;

    Ok(EmptyResponse)
}

async fn set_interest(
    db: &Database,
    user: &User,
    target: &Reference<'_>,
    event: &str,
    interested: bool,
) -> Result<Json<v0::ServerEventResponse>> {
    let (server, member) = membership(db, user, target).await?;
    let mut event = visible_event(db, user, &server, &member, event).await?;
    if interested && !event.is_upcoming() {
        return Err(create_error!(InvalidOperation));
    }
    let count = if interested {
        db.add_server_event_interest(&event.id, &server.id, &user.id)
            .await?
    } else {
        db.remove_server_event_interest(&event.id, &user.id).await?
    };
    // a change: tell everyone who sees the event (a repeat changes nothing)
    if let Some(count) = count {
        event.interested_count = count;
        EventV1::ServerEventInterest {
            id: event.id.clone(),
            server: server.id.clone(),
            user: user.id.clone(),
            interested,
            count,
        }
        .p(event.topic())
        .await;
    }
    Ok(Json(v0::ServerEventResponse {
        event: event.into(),
        interested,
    }))
}

/// # Mark Server Event Interested
///
/// Hear when the event starts. Doing it twice changes nothing.
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

/// # Fetch Interested Members
///
/// Members who marked the event "Interested", by user id, 50 per page
/// (`limit` up to 100). Pass `next` of a page as `after` for the next one.
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
    let event = visible_event(db, &user, &server, &member, event).await?;
    let limit = options.limit.unwrap_or(50).clamp(1, 100);
    // one more than asked tells whether a next page exists
    let mut ids = db
        .fetch_server_event_interested(&event.id, options.after.as_deref(), limit + 1)
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
