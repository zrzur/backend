use super::File;

/// Categories a server listing may use on Discover
pub const DISCOVER_SERVER_CATEGORIES: &[&str] = &[
    "gaming",
    "community",
    "education",
    "technology",
    "art",
    "music",
    "entertainment",
    "anime",
    "sports",
    "science",
    "languages",
    "other",
];

/// Categories a bot listing may use on Discover
pub const DISCOVER_BOT_CATEGORIES: &[&str] = &[
    "moderation",
    "utility",
    "fun",
    "music",
    "games",
    "social",
    "other",
];

/// Maximum number of tags on a listing
pub const DISCOVER_MAX_TAGS: usize = 5;

auto_derived!(
    pub enum DiscoverRequestType {
        Bot,
        Server,
    }

    pub enum DiscoverRequestStatus {
        Pending,
        UnderReview,
        Removed(Option<String>),
        Denied(Option<String>),   // reason
        Approved(Option<String>), // reason
    }

    /// Discover request
    pub struct DiscoverRequest {
        /// The type of request.
        #[serde(rename = "type")]
        pub request_type: DiscoverRequestType,
        /// The ID of the bot/server
        #[serde(rename = "id")]
        pub request_id: String,
        /// status of the request
        pub status: DiscoverRequestStatus,
        /// Listing tags
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub tags: Vec<String>,
        /// Listing category
        #[serde(skip_serializing_if = "Option::is_none")]
        pub category: Option<String>,
        /// Main language of the community (ISO 639-1)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub language: Option<String>,
        /// Whether moderators feature this listing
        #[serde(default)]
        pub featured: bool,
    }

    pub struct DiscoverBan {
        /// Ban Id
        #[serde(rename = "_id")]
        pub id: String,
        /// The type of item.
        #[serde(rename = "type")]
        pub item_type: DiscoverRequestType,
        /// The ID of the bot/server
        pub item_id: String,
    }

    /// Listing details an owner provides when submitting or editing
    #[derive(Default)]
    pub struct DataDiscoverListing {
        /// Up to five short tags
        #[serde(default)]
        pub tags: Option<Vec<String>>,
        /// Category id, see the Discover category lists
        #[serde(default)]
        pub category: Option<String>,
        /// ISO 639-1 language code
        #[serde(default)]
        pub language: Option<String>,
    }

    /// Discover listing query
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsDiscoverQuery {
        /// Case-insensitive search over name, description and tags
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        pub query: Option<String>,
        /// Only listings in this category
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub category: Option<String>,
        /// Only listings carrying this tag
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub tag: Option<String>,
        /// Only listings in this language
        #[cfg_attr(feature = "validator", validate(length(min = 2, max = 2)))]
        pub language: Option<String>,
        /// Only featured listings
        pub featured: Option<bool>,
        /// `popular` (default) or `new`
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 16)))]
        pub sort: Option<String>,
        /// Maximum number of results
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 100)))]
        pub limit: Option<i64>,
        /// Number of results to skip
        #[cfg_attr(feature = "validator", validate(range(min = 0, max = 10000)))]
        pub offset: Option<u64>,
    }

    /// Server listed on Discover
    pub struct DiscoverServer {
        /// Server Id
        #[serde(rename = "_id")]
        pub id: String,
        /// Server name
        pub name: String,
        /// Server description
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        /// Server icon
        #[serde(skip_serializing_if = "Option::is_none")]
        pub icon: Option<File>,
        /// Server banner
        #[serde(skip_serializing_if = "Option::is_none")]
        pub banner: Option<File>,
        /// Server flags
        #[serde(skip_serializing_if = "Option::is_none")]
        pub flags: Option<i32>,
        /// Whether the server is NSFW
        pub nsfw: bool,
        /// Approximate number of members
        pub member_count: i64,
        /// Approximate number of members online (cached; missing for very
        /// large servers)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub online_count: Option<i64>,
        /// The server's custom link name, used for its public page
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub link: Option<String>,
        /// Listing tags
        pub tags: Vec<String>,
        /// Listing category
        #[serde(skip_serializing_if = "Option::is_none")]
        pub category: Option<String>,
        /// Main language
        #[serde(skip_serializing_if = "Option::is_none")]
        pub language: Option<String>,
        /// Featured by moderators
        pub featured: bool,
        /// Biggest active paid boost (Boost, SuperBoost or MegaBoost)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub boost: Option<String>,
        /// In the paid top row
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub promoted: bool,
    }

    /// Bot listed on Discover
    pub struct DiscoverBot {
        /// Bot Id
        #[serde(rename = "_id")]
        pub id: String,
        /// Bot username
        pub username: String,
        /// Bot avatar
        #[serde(skip_serializing_if = "Option::is_none")]
        pub avatar: Option<File>,
        /// Bot profile background, used as banner
        #[serde(skip_serializing_if = "Option::is_none")]
        pub banner: Option<File>,
        /// Bot profile description
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        /// Number of servers the bot is in
        pub server_count: i64,
        /// Verified by Trust & Safety
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub verified: bool,
        /// Listing tags
        pub tags: Vec<String>,
        /// Listing category
        #[serde(skip_serializing_if = "Option::is_none")]
        pub category: Option<String>,
        /// Main language
        #[serde(skip_serializing_if = "Option::is_none")]
        pub language: Option<String>,
        /// Featured by moderators
        pub featured: bool,
        /// Biggest active paid boost (Boost, SuperBoost or MegaBoost)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub boost: Option<String>,
        /// In the paid top row
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub promoted: bool,
    }

    /// Discover request awaiting moderation, with a summary of its item
    pub struct DiscoverQueueEntry {
        /// The request itself
        pub request: DiscoverRequest,
        /// Name of the server or bot
        pub name: String,
        /// Server description or bot profile text
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        /// Server icon or bot avatar
        #[serde(skip_serializing_if = "Option::is_none")]
        pub icon: Option<File>,
        /// Owner of the server or bot
        pub owner: String,
        /// Approximate members (servers) or server count (bots)
        pub count: i64,
    }

    /// Moderation outcome for a Discover request
    pub enum DiscoverDecision {
        /// Mark as being reviewed
        Review,
        /// List the item on Discover
        Approve,
        /// Reject the request; the owner may apply again
        Deny,
        /// Remove the item from Discover; the owner must contact support
        Remove,
    }

    /// Moderation payload for a Discover request
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataDiscoverDecision {
        /// Outcome; omit to only change `featured`
        #[serde(default)]
        pub decision: Option<DiscoverDecision>,
        /// Reason shown to the owner
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 512)))]
        #[serde(default)]
        pub reason: Option<String>,
        /// Feature or unfeature an approved listing
        #[serde(default)]
        pub featured: Option<bool>,
    }
);
