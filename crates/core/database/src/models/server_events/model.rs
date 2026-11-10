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
//! events in channels they may view. Gateway events follow the same rule:
//! they go to the place's channel topic (only members who can view the
//! channel are subscribed to it) or to the server topic for external places.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_models::v0::{self, ServerEventPlace, ServerEventRepeat, ServerEventStatus};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{ErrorType, Result};
use ulid::Ulid;

use crate::events::client::EventV1;
use crate::util::permissions::DatabasePermissionQuery;
use crate::util::tz::{civil_from_days, days_from_civil, days_in_month, Zone};
use crate::{Channel, Database, File, Member, Server, SystemMessage, User};

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
        /// How many members marked it "Interested" (kept by the database
        /// with each mark, never written from a stale copy)
        #[serde(default)]
        pub interested_count: u32,
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
    }

    /// Event and member of an "Interested" mark
    #[derive(Hash, Default)]
    pub struct ServerEventInterestKey {
        pub event: String,
        pub user: String,
    }

    /// A member marked an event "Interested"
    pub struct ServerEventInterest {
        #[serde(rename = "_id")]
        pub id: ServerEventInterestKey,
        /// Server of the event (to clean up with it)
        pub server: String,
        /// When it was marked
        pub created_at: Timestamp,
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
            previous: None,
            started_at: None,
            ended_at: None,
            time_zone: None,
            series_start: None,
            occurrence: 0,
            next: None,
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
            previous: Some(self.id.clone()),
            started_at: None,
            ended_at: None,
            series_start: Some(first),
            occurrence: n,
            next: None,
            cover: None,
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

    /// Whether this user (a member of the server) can see the event: always
    /// for external places, for channels only with View Channel there. A
    /// channel that is gone or of another server hides the event.
    pub async fn visible_to(
        &self,
        db: &Database,
        user: &User,
        server: &Server,
        member: &Member,
    ) -> bool {
        if self.place.channel().is_none() {
            return true;
        }
        match self.place_channel(db).await {
            Some(channel) => Self::channel_visible(db, user, server, member, &channel).await,
            None => false,
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
    pub async fn publish_create(&self) {
        EventV1::ServerEventCreate(self.clone().into())
            .p(self.topic())
            .await;
    }

    /// Tell the members who can see it that it changed. When the place moved
    /// to another topic, the old topic hears that it is gone first.
    pub async fn publish_update(&self, old_topic: Option<String>) {
        if let Some(old) = old_topic.filter(|old| old != &self.topic()) {
            EventV1::ServerEventDelete {
                id: self.id.clone(),
                server: self.server.clone(),
            }
            .p(old)
            .await;
        }
        EventV1::ServerEventUpdate {
            event: self.clone().into(),
        }
        .p(self.topic())
        .await;
    }

    /// Tell the members who could see it that it is gone
    pub async fn publish_delete(&self) {
        EventV1::ServerEventDelete {
            id: self.id.clone(),
            server: self.server.clone(),
        }
        .p(self.topic())
        .await;
    }

    /// Tell who sees the event that its count changed
    pub async fn publish_interest(&self, user: &str, interested: bool) {
        EventV1::ServerEventInterest {
            id: self.id.clone(),
            server: self.server.clone(),
            channel: self.place.channel().map(str::to_string),
            user: user.to_string(),
            interested,
            count: self.interested_count,
        }
        .p(self.topic())
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
        self.publish_update(None).await;
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
        self.publish_update(None).await;
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
            .set_server_event_status(&self.id, from, ServerEventStatus::Cancelled, now)
            .await?
        {
            return Ok(false);
        }
        self.status = ServerEventStatus::Cancelled;
        self.ended_at = Some(now);
        self.publish_update(None).await;
        Ok(true)
    }

    /// Delete it for good. A repeating event's series goes on with the next
    /// occurrence. Its cover file is released.
    pub async fn delete(&self, db: &Database) -> Result<()> {
        if self.is_upcoming() {
            self.schedule_next(db, Timestamp::now_utc()).await?;
        }
        db.delete_server_event(&self.id).await?;
        if let Some(cover) = &self.cover {
            db.mark_attachments_as_deleted(&[cover.id.clone()]).await.ok();
        }
        self.publish_delete().await;
        Ok(())
    }

    /// Write the next occurrence of a repeating event (once: its id comes
    /// from this one), with the interested members who are still members
    /// and its own use of the cover file
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
        let interested = db
            .fetch_server_event_interested(&self.id, None, u32::MAX as i64)
            .await?;
        let members = db.fetch_members(&self.server, &interested).await?;
        for member in &members {
            if let Some(count) = db
                .add_server_event_interest(&next.id, &next.server, &member.id.user)
                .await?
            {
                next.interested_count = count;
            }
        }
        db.set_server_event_next(&self.id, &next.id).await?;
        next.publish_create().await;
        Ok(Some(next))
    }

    /// A member left: their marks go, and the counts with them
    pub async fn forget_member(db: &Database, server: &str, user: &str) -> Result<()> {
        for id in db.remove_server_event_interests_of(server, user).await? {
            if let Ok(event) = db.fetch_server_event(&id).await {
                event.publish_interest(user, false).await;
            }
        }
        Ok(())
    }

    /// One crond pass: cancel what lost its channel, start what is due,
    /// complete what has ended, and give every repeating event that lacks
    /// one its next occurrence. One event failing never stops the others.
    pub async fn run_pass(db: &Database, now: Timestamp) -> Result<PassReport> {
        let mut report = PassReport::default();
        for mut event in db.fetch_server_events_to_start(now).await? {
            let outcome = async {
                match db.fetch_server(&event.server).await {
                    Err(error) if matches!(error.error_type, ErrorType::NotFound) => {
                        db.delete_server_event(&event.id).await?;
                        return Ok(false);
                    }
                    Err(error) => return Err(error),
                    Ok(_) => {}
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

    /// When it starts: a system message in the server's join message channel
    /// (only when everyone can see the place), and a push notification to
    /// the interested members who can still see the place and the channel
    /// the notification names
    async fn announce_start(&self, db: &Database) -> Result<()> {
        let server = db.fetch_server(&self.server).await?;
        let place_channel = self.place_channel(db).await;
        if self.place.channel().is_some() && place_channel.is_none() {
            return Ok(());
        }

        let system_channel = match server
            .system_messages
            .as_ref()
            .and_then(|channels| channels.user_joined.clone())
        {
            Some(id) => db.fetch_channel(&id).await.ok(),
            None => None,
        };
        let open = Self::place_open_to_everyone(&server, place_channel.as_ref());
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

        // interested members who are still members and can see both
        let interested = db
            .fetch_server_event_interested(&self.id, None, u32::MAX as i64)
            .await?;
        if interested.is_empty() {
            return Ok(());
        }
        let members = db.fetch_members(&server.id, &interested).await?;
        let users = db.fetch_users(&interested).await?;
        let mut recipients = Vec::with_capacity(users.len());
        for user in &users {
            let Some(member) = members.iter().find(|member| member.id.user == user.id) else {
                continue;
            };
            if Self::channel_visible(db, user, &server, member, &push_channel).await {
                recipients.push(user.id.clone());
            }
        }
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

/// What one crond pass did
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PassReport {
    pub started: usize,
    pub completed: usize,
    pub cancelled: usize,
    pub scheduled: usize,
    pub failed: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

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
