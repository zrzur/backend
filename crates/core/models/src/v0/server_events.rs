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
    }

    /// How an event repeats. The next occurrence is made when one completes.
    pub enum ServerEventRepeat {
        Weekly,
        EveryTwoWeeks,
        Monthly,
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
        /// How many members marked it "Interested"
        pub interested_count: u32,
        /// The event this one follows (repeating events)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub previous: Option<String>,
        /// The creator's IANA time zone; repeats keep its wall-clock time
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub time_zone: Option<String>,
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
    }

    /// Calling off one occurrence or the whole series
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsCancelServerEvent {
        /// Stop the whole series (default: only this occurrence; the next
        /// one is scheduled)
        pub series: Option<bool>,
    }

    /// Optional fields of an event that can be cleared
    pub enum FieldsServerEvent {
        Description,
        EndsAt,
        Cover,
        Repeat,
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
        /// Fields to clear
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 4)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsServerEvent>,
    }

    /// The events of a server you can see, with the ones you marked
    pub struct ServerEventList {
        /// Scheduled and active events, soonest first
        pub events: Vec<ServerEvent>,
        /// Ids of the listed events you marked "Interested"
        pub interested: Vec<String>,
    }

    /// One event and whether you marked it "Interested"
    pub struct ServerEventResponse {
        pub event: ServerEvent,
        pub interested: bool,
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
