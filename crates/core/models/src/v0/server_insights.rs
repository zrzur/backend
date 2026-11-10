//! Server insights: how a server is doing, as daily counts for its managers.
//!
//! Only aggregated numbers leave the server. Counts that could point at one
//! person are sent as `null` ("fewer than 5") when they are 1 to 4: per
//! channel messages, active members and voice minutes. Messages count only
//! in channels the viewer can see, daily totals included.

#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// Query of the insights page
    #[cfg_attr(feature = "validator", derive(Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsServerInsights {
        /// How many days to show: 7, 30 or 90 (default 30)
        pub days: Option<u32>,
        /// The viewer's IANA time zone (`Europe/Vienna`); days are counted in
        /// it, daylight saving included. Unknown names fall back to `utc_offset`.
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        pub time_zone: Option<String>,
        /// The viewer's offset from UTC in minutes (east positive, e.g. 120
        /// for UTC+2), used without a known `time_zone`. Rounded to whole
        /// hours, since counts are kept per hour.
        #[cfg_attr(feature = "validator", validate(range(min = -720, max = 840)))]
        pub utc_offset: Option<i32>,
    }

    /// One day of a server's insights
    pub struct ServerInsightsDay {
        /// The day in the viewer's time (YYYY-MM-DD)
        pub day: String,
        /// People who joined
        pub joins: u32,
        /// People who left, were kicked or banned
        pub leaves: u32,
        /// Members at the end of the day (today's count, less the joins and
        /// plus the leaves of the days after)
        pub members: u32,
        /// Messages sent in channels the viewer can see (system messages
        /// not counted)
        pub messages: u32,
        /// Distinct people who sent a message that day (counted per UTC day,
        /// bots left out; an estimate); `null` for 1 to 4
        pub active_members: Option<u32>,
        /// Minutes spent in the server's voice channels; `null` for 1 to 4
        pub voice_minutes: Option<u32>,
    }

    /// The whole range at once
    pub struct ServerInsightsTotals {
        /// People who joined
        pub joins: u32,
        /// People who left, were kicked or banned
        pub leaves: u32,
        /// Messages sent in channels the viewer can see
        pub messages: u32,
        /// Minutes in voice; `null` for 1 to 4
        pub voice_minutes: Option<u32>,
        /// Active members on an average day, rounded; `null` for 1 to 4
        pub active_members: Option<u32>,
    }

    /// Where the joins of the range came from
    #[derive(Default)]
    pub struct ServerInsightsJoinSources {
        /// An invite link
        pub invite: u32,
        /// Discover
        pub discover: u32,
        /// The server's custom link
        pub vanity: u32,
        /// Anything else: approved requests, bots added, creating the server
        pub other: u32,
    }

    /// One of the most active channels of the range
    pub struct ServerInsightsChannel {
        /// Channel id
        pub channel: String,
        /// Messages sent; `null` when there were 1 to 4 ("fewer than 5")
        pub messages: Option<u32>,
    }

    /// A server's insights for a range of days
    pub struct ServerInsights {
        /// Days shown (7, 30 or 90)
        pub days: u32,
        /// The time the days were counted in: an IANA name, or `UTC+02:00`
        pub time_zone: String,
        /// One row per day, oldest first, the last one is today
        pub rows: Vec<ServerInsightsDay>,
        /// The whole range
        pub totals: ServerInsightsTotals,
        /// Where the joins of the range came from
        pub join_sources: ServerInsightsJoinSources,
        /// Up to 10 channels with the most messages that the viewer can see
        pub top_channels: Vec<ServerInsightsChannel>,
        /// The day counting started on this Zrzur (YYYY-MM-DD, the viewer's
        /// time), when known: nothing before it was counted
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub counted_since: Option<String>,
    }
);
