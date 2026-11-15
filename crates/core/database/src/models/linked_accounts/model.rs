auto_derived!(
    /// One game on a linked Steam account, from what Steam shows publicly
    pub struct LinkedGame {
        /// Steam app id
        pub appid: u32,
        pub name: String,
        /// Hours played in total, rounded down
        pub hours: u32,
        /// Achievements unlocked, when the game has any and Steam shows them
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub achievements_unlocked: Option<u32>,
        /// Achievements the game has
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub achievements_total: Option<u32>,
        /// Steam's icon file name for the game (40 hex characters)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub icon: Option<String>,
    }

    /// One unlocked achievement on a linked Steam account
    pub struct LinkedAchievement {
        /// Steam app id of its game
        pub appid: u32,
        /// Name, in English as Steam gives it
        pub name: String,
        #[serde(default)]
        pub description: String,
        /// Steam's icon file name (40 hex characters)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub icon: Option<String>,
        /// Day it was unlocked, `YYYY-MM-DD` in UTC. Never the time of day.
        pub unlocked_on: String,
    }

    /// One public repository on a linked GitHub account
    #[derive(Default)]
    pub struct LinkedRepo {
        /// GitHub's repository id
        pub id: u64,
        pub name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub language: Option<String>,
        pub stars: u32,
        /// `https://github.com/<owner>/<name>`
        pub url: String,
    }

    /// An account on another service that a person linked to their profile
    /// (`linked_accounts`). Only the fields below are ever stored: no token,
    /// no password, no real name, country, avatar, friends or times of day
    /// (an achievement keeps the day it was unlocked, nothing finer).
    #[derive(Default)]
    pub struct LinkedAccount {
        /// `<user id>/<service>`: one account per service per person
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// `steam`
        pub service: String,
        /// Id on that service (64-bit SteamID). One service account links to
        /// one Zrzur account only.
        pub external_id: String,
        /// Name on that service. Empty while the account there is private.
        #[serde(default)]
        pub display_name: String,
        /// Public profile address on that service
        #[serde(default)]
        pub profile_url: String,
        /// Shows on the profile. False until the person checked the preview.
        #[serde(default)]
        pub shown: bool,
        /// The account or its game details are private at the service
        #[serde(default)]
        pub private: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub game_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub total_hours: Option<u32>,
        /// Most played games, most hours first
        #[serde(default)]
        pub games: Vec<LinkedGame>,
        /// Latest unlocked achievements across those games, newest first
        #[serde(default)]
        pub achievements: Vec<LinkedAchievement>,
        /// Games the person chose not to show
        #[serde(default)]
        pub hidden_titles: Vec<u32>,
        /// Version of the consent text the person agreed to. A link made
        /// under an older text shows only what that text named.
        #[serde(default = "first_consent")]
        pub consent: u32,
        /// Milliseconds since the epoch
        pub linked_at: i64,
        pub refreshed_at: i64,
        /// Twitch: live now, as Twitch last told us
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub live: Option<bool>,
        /// Twitch: title of the stream while live
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub live_title: Option<String>,
        /// Twitch: when the live flag was last set, milliseconds since the epoch
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub live_at: Option<i64>,
        /// Twitch: ids of the live/offline subscriptions at Twitch, so
        /// Disconnect can end them
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub subscriptions: Vec<String>,
        /// GitHub: most starred public repositories, most stars first
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub repos: Vec<LinkedRepo>,
        /// GitHub: repositories the person chose to show
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub shown_repos: Vec<u64>,
        /// Battle.net: the copy may show until then; the service allows a
        /// copy for 30 days. Milliseconds since the epoch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expires_at: Option<i64>,
        /// Twitch, GitHub: profile picture on the service's own image host
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub avatar_url: Option<String>,
        /// GitHub: the person's own name there
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub real_name: Option<String>,
        /// Twitch description, GitHub bio
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub bio: Option<String>,
        /// GitHub followers
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub followers: Option<u32>,
        /// GitHub public repositories
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub public_repos: Option<u32>,
        /// Twitch: `partner` or `affiliate`
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub kind: Option<String>,
        /// Twitch: the category of the live stream, or of the last one
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub game: Option<String>,
    }
);

/// The consent text of the first version (name, counts, hours, top games)
fn first_consent() -> u32 {
    1
}

/// The consent text in use: adds pictures, latest achievements and the days
/// they were unlocked
pub const LINKED_CONSENT_VERSION: u32 = 2;

/// Services a person can link
pub const LINKED_SERVICES: &[&str] = &["steam", "twitch", "github", "battlenet"];

/// Most games kept per linked account
pub const LINKED_TOP_GAMES: usize = 10;

/// Most achievements kept per linked account
pub const LINKED_LATEST_ACHIEVEMENTS: usize = 60;

impl LinkedAccount {
    /// The stored id of one person's account on one service
    pub fn key(user_id: &str, service: &str) -> String {
        format!("{user_id}/{service}")
    }
}

/// Most repositories kept per linked GitHub account
pub const LINKED_TOP_REPOS: usize = 12;

/// Most repositories a person can show
pub const LINKED_MAX_SHOWN_REPOS: usize = 6;
