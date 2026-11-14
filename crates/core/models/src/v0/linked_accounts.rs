use std::collections::BTreeMap;

#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// One game on a linked Steam account
    pub struct LinkedGame {
        /// Steam app id
        pub appid: u32,
        pub name: String,
        /// Hours played in total
        pub hours: u32,
        /// Achievements unlocked, when the game has any and Steam shows them
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub achievements_unlocked: Option<u32>,
        /// Achievements the game has
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub achievements_total: Option<u32>,
        /// The game's small icon on Steam's image server. Clients load it
        /// only through the media proxy.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub icon_url: Option<String>,
        /// The game's wide header picture on Steam's image server (proxy only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub header_url: Option<String>,
        /// The game's page in the Steam store
        pub store_url: String,
    }

    /// One unlocked achievement of a linked Steam account
    pub struct LinkedAchievement {
        /// Steam app id of its game
        pub appid: u32,
        /// Name of its game
        pub game: String,
        /// Name, in English as Steam gives it
        pub name: String,
        pub description: String,
        /// Its icon on Steam's image server (proxy only)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub icon_url: Option<String>,
        /// Day it was unlocked, `YYYY-MM-DD` (UTC). Never a time of day.
        pub unlocked_on: String,
    }

    /// A linked account as its owner sees it in settings
    pub struct LinkedAccount {
        /// `steam`
        pub service: String,
        /// Name on that service; empty while it is private there
        pub display_name: String,
        /// Public profile address on that service
        pub profile_url: String,
        /// Shows on the profile (false until the preview was checked)
        pub shown: bool,
        /// The account or its game details are private at the service
        pub private: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub game_count: Option<u32>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub total_hours: Option<u32>,
        /// Most played games, hidden ones included
        pub games: Vec<LinkedGame>,
        /// Latest achievements, newest first, hidden games included
        pub achievements: Vec<LinkedAchievement>,
        /// App ids of games that do not show
        pub hidden_titles: Vec<u32>,
        /// Milliseconds since the epoch
        pub linked_at: i64,
        pub refreshed_at: i64,
        /// Earliest time Refresh works again, milliseconds since the epoch
        pub refresh_after: i64,
        /// The person agreed to the consent text in use. When false, only
        /// what the earlier text named shows to others until they agree.
        pub consent_current: bool,
    }

    /// Linked accounts on a profile, as another person sees them. Nothing
    /// here comes from the service at viewing time.
    pub struct ProfileGames {
        /// `steam`
        pub service: String,
        /// Empty when private
        pub display_name: String,
        /// Empty when private: a link would reveal the account
        pub profile_url: String,
        /// The account or its game details are private at the service
        pub private: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub game_count: Option<u32>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub total_hours: Option<u32>,
        /// Most played games the person did not hide
        pub games: Vec<LinkedGame>,
        /// Latest achievements in games the person did not hide, newest first
        pub achievements: Vec<LinkedAchievement>,
    }

    /// Steam on this server, and the person's link if any
    pub struct SteamConnection {
        /// Steam can be linked on this server
        pub available: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub account: Option<LinkedAccount>,
    }

    /// The person's linked accounts
    pub struct LinkedAccounts {
        pub steam: SteamConnection,
    }

    /// Start linking Steam
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSteamStart {
        /// Origin of the web app Steam should send the person back to, such
        /// as `https://app.zrzur.com`. Must be one the server allows.
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub origin: String,
    }

    /// Where to send the person to sign in on Steam
    pub struct SteamStartResponse {
        /// Steam's own sign-in page
        pub url: String,
    }

    /// Finish linking Steam with what Steam sent back
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataSteamFinish {
        /// The `state` from the return address
        #[cfg_attr(feature = "validator", validate(length(min = 16, max = 64)))]
        pub state: String,
        /// Every `openid.*` field of the return address, unchanged
        pub params: BTreeMap<String, String>,
    }

    /// Change what a linked account shows
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataEditLinkedAccount {
        /// Show it on the profile
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub shown: Option<bool>,
        /// App ids of games not to show (replaces the list)
        #[cfg_attr(feature = "validator", validate(length(max = 50)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub hidden_titles: Option<Vec<u32>>,
        /// Agree to the consent text in use; must be its version number
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub consent: Option<u32>,
    }
);
