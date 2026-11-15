#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// One public repository on a linked GitHub account
    pub struct LinkedRepo {
        /// GitHub's repository id
        pub id: u64,
        pub name: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub description: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub language: Option<String>,
        pub stars: u32,
        /// `https://github.com/<owner>/<name>`
        pub url: String,
    }

    /// A Twitch, GitHub or Battle.net link as its owner sees it in settings
    pub struct ServiceAccount {
        /// `twitch`, `github` or `battlenet`
        pub service: String,
        /// Name on that service (Battle.net: the BattleTag)
        pub display_name: String,
        /// Public page on that service; empty for Battle.net
        pub profile_url: String,
        /// Shows on the profile (false until the preview was checked)
        pub shown: bool,
        /// Twitch: live now
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub live: Option<bool>,
        /// Twitch: stream title while live
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub live_title: Option<String>,
        /// GitHub: most starred public repositories, shown ones included
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub repos: Vec<LinkedRepo>,
        /// GitHub: ids of the repositories that show
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub shown_repos: Vec<u64>,
        /// Battle.net: the BattleTag stops showing at this time until the
        /// person confirms again. Milliseconds since the epoch.
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub expires_at: Option<i64>,
        /// Milliseconds since the epoch
        pub linked_at: i64,
        pub refreshed_at: i64,
        /// Earliest time Refresh works again (GitHub), milliseconds since the epoch
        pub refresh_after: i64,
        /// Twitch, GitHub: profile picture on the service's own image host
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub avatar_url: Option<String>,
        /// GitHub: the person's own name there
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub real_name: Option<String>,
        /// Twitch description, GitHub bio
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub bio: Option<String>,
        /// GitHub followers
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub followers: Option<u32>,
        /// GitHub public repositories
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub public_repos: Option<u32>,
        /// Twitch: `partner` or `affiliate`
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub kind: Option<String>,
        /// Twitch: the category of the live stream, or of the last one
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub game: Option<String>,
        /// The person agreed to the consent text in use. When false, Twitch
        /// does not show on the profile and is not read again, and GitHub
        /// Refresh waits, until they agree (`consent` in Edit).
        #[cfg_attr(feature = "serde", serde(default))]
        pub consent_current: bool,
    }

    /// One service on this server, and the person's link if any
    #[derive(Default)]
    pub struct ServiceConnection {
        /// The service can be linked on this server
        pub available: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub account: Option<ServiceAccount>,
    }

    /// A linked account on a profile, as another person sees it. Nothing
    /// here comes from the service at viewing time.
    pub struct ProfileConnection {
        /// `twitch`, `github` or `battlenet`
        pub service: String,
        pub display_name: String,
        /// Public page on that service; empty for Battle.net
        pub profile_url: String,
        /// Twitch: live now
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub live: Option<bool>,
        /// Twitch: stream title while live
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub live_title: Option<String>,
        /// GitHub: the repositories the person chose to show
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub repos: Vec<LinkedRepo>,
        /// Twitch, GitHub: profile picture on the service's own image host
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub avatar_url: Option<String>,
        /// GitHub: the person's own name there
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub real_name: Option<String>,
        /// Twitch description, GitHub bio
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub bio: Option<String>,
        /// GitHub followers
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub followers: Option<u32>,
        /// GitHub public repositories
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub public_repos: Option<u32>,
        /// Twitch: `partner` or `affiliate`
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub kind: Option<String>,
        /// Twitch: the category of the live stream, or of the last one
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub game: Option<String>,
    }

    /// Start linking a service
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataServiceStart {
        /// Origin of the web app the service should send the person back to,
        /// such as `https://app.zrzur.com`. Must be one the server allows.
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub origin: String,
    }

    /// Where to send the person to sign in on the service
    pub struct ServiceStartResponse {
        /// The service's own sign-in page
        pub url: String,
    }

    /// Finish linking with what the service sent back
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataServiceFinish {
        /// The `state` from the return address
        #[cfg_attr(feature = "validator", validate(length(min = 16, max = 64)))]
        pub state: String,
        /// The `code` from the return address
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 512)))]
        pub code: String,
    }

    /// Change what a linked Twitch, GitHub or Battle.net account shows
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataEditServiceAccount {
        /// Show it on the profile
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub shown: Option<bool>,
        /// GitHub: ids of the repositories to show (replaces the list, at most 6)
        #[cfg_attr(feature = "validator", validate(length(max = 6)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub shown_repos: Option<Vec<u64>>,
        /// Agree to the consent text in use; must be its version number
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub consent: Option<u32>,
    }
);
