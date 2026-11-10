//! Scheduled server events.
//!
//! An event belongs to a server and takes place in one of its voice or text
//! channels or somewhere outside Zrzur. crond starts it at `starts_at` and
//! completes it at `ends_at` (or `DEFAULT_LENGTH_HOURS` after the start when
//! no end is set); managers can do both by hand. A repeating event gets its
//! next occurrence when it completes.
//!
//! Who sees what: members of the server see events at external places and
//! events in channels they may view. Gateway events follow the same rule:
//! they go to the place's channel topic (only members who can view the
//! channel are subscribed to it) or to the server topic for external places.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_models::v0::{self, ServerEventPlace, ServerEventRepeat, ServerEventStatus};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::Result;
use ulid::Ulid;

use crate::events::client::EventV1;
use crate::util::permissions::DatabasePermissionQuery;
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
        /// Cover image
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

fn days_in_month(year: i32, month: u8) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// The start after `from` for a repeat. Monthly keeps the day of the month,
/// or takes the last day of a shorter month (31 January, then 28 February).
pub fn next_start(from: Timestamp, repeat: &ServerEventRepeat) -> Timestamp {
    let days = match repeat {
        ServerEventRepeat::Weekly => 7,
        ServerEventRepeat::EveryTwoWeeks => 14,
        ServerEventRepeat::Monthly => {
            let (year, month, day) = (from.year(), from.month() as u8, from.day() as i64);
            let (next_year, next_month) = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            let target = day.min(days_in_month(next_year, next_month));
            days_in_month(year, month) - day + target
        }
    };
    from.checked_add(Duration::days(days)).unwrap_or(from)
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
        let length = self.ends_at.map(|end| end.duration_since(self.starts_at));
        let mut starts_at = next_start(self.starts_at, repeat);
        // crond may have been down: never schedule in the past
        let mut guard = 0;
        while starts_at <= now && guard < 1000 {
            starts_at = next_start(starts_at, repeat);
            guard += 1;
        }
        Some(ServerEvent {
            id: Ulid::new().to_string(),
            starts_at,
            ends_at: length.and_then(|length| starts_at.checked_add(length)),
            status: ServerEventStatus::Scheduled,
            interested_count: 0,
            previous: Some(self.id.clone()),
            started_at: None,
            ended_at: None,
            ..self.clone()
        })
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
        let Some(channel_id) = self.place.channel() else {
            return true;
        };
        let Ok(channel) = db.fetch_channel(channel_id).await else {
            return false;
        };
        Self::channel_visible(db, user, server, member, &channel).await
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

    /// Whether everyone in the server can view the place, judged from the
    /// server's and channel's defaults only (no role is assumed). Used to
    /// decide whether the start may be announced in the system channel.
    pub fn place_open_to_everyone(server: &Server, channel: Option<&Channel>) -> bool {
        let view = ChannelPermission::ViewChannel as i64;
        let Some(channel) = channel else {
            return true;
        };
        if server.default_permissions & view == 0 {
            return false;
        }
        match channel {
            Channel::TextChannel {
                id,
                default_permissions,
                permissions_synced,
                ..
            } => {
                let defaults = if *permissions_synced {
                    server
                        .categories
                        .as_ref()
                        .and_then(|categories| {
                            categories
                                .iter()
                                .find(|category| category.channels.contains(id))
                        })
                        .and_then(|category| category.default_permissions.clone())
                } else {
                    default_permissions.clone()
                };
                defaults.is_none_or(|field| field.d & view == 0)
            }
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
        // an event started early by hand keeps its planned length
        if let Some(end) = self.ends_at {
            if end <= now {
                self.ends_at = None;
            }
        }
        self.publish_update(None).await;
        if let Err(error) = self.announce_start(db).await {
            log::warn!("Could not announce the start of event {}: {error:?}", self.id);
        }
        Ok(true)
    }

    /// Complete it now (crond at the end, or a manager by hand), and make
    /// the next occurrence of a repeating event. Returns false when it was
    /// not active any more.
    pub async fn complete(&mut self, db: &Database) -> Result<bool> {
        let now = Timestamp::now_utc();
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
        self.schedule_next(db, now).await?;
        Ok(true)
    }

    /// Call it off (scheduled or active). Returns false when it was over.
    pub async fn cancel(&mut self, db: &Database) -> Result<bool> {
        let now = Timestamp::now_utc();
        let from = self.status.clone();
        if !matches!(
            from,
            ServerEventStatus::Scheduled | ServerEventStatus::Active
        ) || !db
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

    /// The next occurrence of a repeating event, with the same interested
    /// members (they asked to hear about this series)
    async fn schedule_next(&self, db: &Database, now: Timestamp) -> Result<Option<ServerEvent>> {
        let Some(mut next) = self.next_occurrence(now) else {
            return Ok(None);
        };
        db.insert_server_event(&next).await?;
        let interested = db
            .fetch_server_event_interested(&self.id, None, u32::MAX as i64)
            .await?;
        for user in &interested {
            if let Some(count) = db
                .add_server_event_interest(&next.id, &next.server, user)
                .await?
            {
                next.interested_count = count;
            }
        }
        next.publish_create().await;
        Ok(Some(next))
    }

    /// When it starts: a system message in the server's join message channel
    /// (only when everyone can see the place), and a push notification to
    /// the interested members who can still see it
    async fn announce_start(&self, db: &Database) -> Result<()> {
        let server = db.fetch_server(&self.server).await?;
        let place_channel = match self.place.channel() {
            Some(id) => Some(db.fetch_channel(id).await?),
            None => None,
        };

        let system_channel = server
            .system_messages
            .as_ref()
            .and_then(|channels| channels.user_joined.clone());
        let mut message = SystemMessage::EventStarted {
            id: self.id.clone(),
            title: self.title.clone(),
        }
        .into_message(
            system_channel
                .clone()
                .or_else(|| self.place.channel().map(str::to_string))
                .or_else(|| server.channels.first().cloned())
                .unwrap_or_default(),
        );
        if system_channel.is_some()
            && Self::place_open_to_everyone(&server, place_channel.as_ref())
        {
            message
                .send_without_notifications(db, None, None, false, false, false)
                .await?;
        }

        // push: interested members who are still members and can see the place
        let interested = db
            .fetch_server_event_interested(&self.id, None, u32::MAX as i64)
            .await?;
        if interested.is_empty() || message.channel.is_empty() {
            return Ok(());
        }
        let members = db.fetch_members(&server.id, &interested).await?;
        let users = db.fetch_users(&interested).await?;
        let mut recipients = Vec::with_capacity(users.len());
        for user in &users {
            let Some(member) = members.iter().find(|member| member.id.user == user.id) else {
                continue;
            };
            let visible = match &place_channel {
                Some(channel) => Self::channel_visible(db, user, &server, member, channel).await,
                None => true,
            };
            if visible {
                recipients.push(user.id.clone());
            }
        }
        if recipients.is_empty() {
            return Ok(());
        }

        let channel = match db.fetch_channel(&message.channel).await {
            Ok(channel) => channel,
            Err(_) => return Ok(()),
        };
        let mut notification = v0::PushNotification::from(
            message.into_model(None, None),
            None,
            channel.into(),
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
