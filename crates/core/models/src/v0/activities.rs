use iso8601_timestamp::Timestamp;

auto_derived!(
    /// What someone is doing right now, as the line under their name says it
    #[derive(Copy, Default, Hash)]
    pub enum ActivityType {
        /// "Playing <name>": a game
        #[default]
        Playing,
        /// "Listening to <name>": music or a podcast
        Listening,
        /// "Watching <name>": a video or a stream
        Watching,
        /// "Streaming <name>": the person is live
        Streaming,
        /// "Competing in <name>": a match or a tournament
        Competing,
        /// "Using <name>": an ordinary program (an editor, a browser)
        Using,
    }

    /// Where an activity came from. When several report at once the
    /// first one here is the main activity: the app itself knows most.
    #[derive(Copy, Hash, PartialOrd, Ord)]
    pub enum ActivitySource {
        /// An app or game that told the desktop app over the local RPC
        Rpc,
        /// The desktop app saw the program running
        Desktop,
        /// The server read "now playing" from the linked Steam account
        Steam,
    }

    /// A link button on an activity (opened through the link warning)
    pub struct ActivityButton {
        /// Text on the button, 1 to 32 characters
        pub label: String,
        /// `https://` address, at most 512 characters
        pub url: String,
    }

    /// One activity of a user. Never stored: it lives in the presence
    /// store while the user is online and is gone when they go offline.
    pub struct UserActivity {
        /// Where it came from
        pub source: ActivitySource,
        /// Kind of activity
        #[serde(rename = "type")]
        pub kind: ActivityType,
        /// Name of the game or program, 1 to 128 characters
        pub name: String,
        /// First line from the app, for example the level being played
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub details: Option<String>,
        /// Second line from the app, for example "In a group (2 of 4)"
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub state: Option<String>,
        /// When it began (the app shows the time since)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub started_at: Option<Timestamp>,
        /// When it ends (the app shows the time left)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub ends_at: Option<Timestamp>,
        /// Id of the app that set it: a Zrzur application id, or the id a
        /// local app gave the desktop app (used for platform-wide blocks)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub application_id: Option<String>,
        /// Large picture: an address on the Zrzur file or proxy server
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub large_image: Option<String>,
        /// Text shown over the large picture
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub large_text: Option<String>,
        /// Small picture: an address on the Zrzur file or proxy server
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub small_image: Option<String>,
        /// Text shown over the small picture
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub small_text: Option<String>,
        /// At most two link buttons
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub buttons: Vec<ActivityButton>,
    }

    /// An activity as an app or the desktop sends it (`SetActivity` on the
    /// gateway). The server checks every field, keeps pictures on Zrzur's
    /// own servers and runs the texts through the platform word list.
    #[derive(Default)]
    pub struct DataActivity {
        /// Kind of activity
        #[serde(rename = "type", default)]
        pub kind: ActivityType,
        /// Name of the game or program, 1 to 128 characters
        pub name: String,
        /// First line, at most 128 characters
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub details: Option<String>,
        /// Second line, at most 128 characters
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub state: Option<String>,
        /// When it began; at most a minute ahead of the server's clock
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub started_at: Option<Timestamp>,
        /// When it ends; after `started_at`, at most 7 days ahead
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub ends_at: Option<Timestamp>,
        /// Id of the app: a Zrzur application id, or the id a local app gave
        /// (letters, digits, `.`, `_`, `-`; at most 64)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub application_id: Option<String>,
        /// `https://` picture; anything not on Zrzur's servers is shown
        /// through the Zrzur proxy
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub large_image: Option<String>,
        /// At most 128 characters
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub large_text: Option<String>,
        /// `https://` picture, like `large_image`
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub small_image: Option<String>,
        /// At most 128 characters
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub small_text: Option<String>,
        /// At most two
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub buttons: Vec<ActivityButton>,
    }
);

auto_derived!(
    /// What a platform-wide activity block stops
    #[derive(Copy)]
    pub enum ActivityBlockKind {
        /// Every activity with this application id
        Application,
        /// Every activity that shows this picture address
        Picture,
    }

    /// A platform-wide block on what activities may show, taken by Trust &
    /// Safety with a statement of reasons (legal RP-07)
    pub struct ActivityBlock {
        #[serde(rename = "_id")]
        pub id: String,
        pub kind: ActivityBlockKind,
        /// The application id, or the picture address as the app sent it
        pub value: String,
        /// Case it was taken on
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub case_id: Option<String>,
        /// Staff member who took it
        pub actor_id: String,
        /// Milliseconds since the epoch
        pub created_at: i64,
    }
);

impl UserActivity {
    /// Order of the list: the main activity first
    pub fn sort(activities: &mut [UserActivity]) {
        activities.sort_by_key(|activity| activity.source);
    }
}
