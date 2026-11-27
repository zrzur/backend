#[cfg(feature = "test")]
use std::sync::OnceLock;
use std::{collections::HashMap, path::Path, sync::LazyLock};

use cached::proc_macro::cached;
use config::{Config, Environment, File, FileFormat};
use futures_locks::RwLock;
use serde::Deserialize;

/// Paths to search for configuration
static CONFIG_SEARCH_PATHS: [&str; 3] = [
    // current working directory
    "Revolt.toml",
    // current working directory - overrides file
    "Revolt.overrides.toml",
    // root directory, for Docker containers
    "/Revolt.toml",
];

/// Path to search for test overrides
static TEST_OVERRIDE_PATH: &str = "Revolt.test-overrides.toml";

/// Configuration builder
static CONFIG_BUILDER: LazyLock<RwLock<Config>> = LazyLock::new(|| {
    RwLock::new({
        let mut builder = Config::builder().add_source(File::from_str(
            include_str!("../Revolt.toml"),
            FileFormat::Toml,
        ));

        let cwd = std::env::current_dir().unwrap();
        let mut cwd: Option<&Path> = Some(&cwd);

        while let Some(path) = cwd {
            for config_path in CONFIG_SEARCH_PATHS {
                let config_path = path.join(config_path);
                if config_path.exists() {
                    builder = builder
                        .add_source(File::new(config_path.to_str().unwrap(), FileFormat::Toml));
                }
            }

            cwd = path.parent();
        }

        if std::env::var("TEST_DB").is_ok() {
            builder = builder.add_source(File::from_str(
                include_str!("../Revolt.test.toml"),
                FileFormat::Toml,
            ));

            // recursively search upwards for an overrides file (if there is one)
            if let Ok(cwd) = std::env::current_dir() {
                let mut path = Some(cwd.as_path());
                while let Some(current_path) = path {
                    let target_path = current_path.join(TEST_OVERRIDE_PATH);
                    if target_path.exists() {
                        builder = builder
                            .add_source(File::new(target_path.to_str().unwrap(), FileFormat::Toml));
                    }

                    path = current_path.parent();
                }
            }
        }

        builder = builder.add_source(Environment::with_prefix("REVOLT").separator("__"));

        builder.build().unwrap()
    })
});

#[derive(Deserialize, Debug, Clone)]
pub struct Database {
    pub mongodb: String,
    pub redis: String,
    pub redis_pubsub: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RabbitQueues {
    pub acks: String,
    pub events: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Rabbit {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub default_exchange: String,
    pub queues: RabbitQueues,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Hosts {
    pub app: String,
    pub api: String,
    pub events: String,
    pub autumn: String,
    /// Internal January URL (API/workers). May be a Docker service hostname.
    pub january: String,
    /// Browser-facing January URL advertised on `GET /` as `features.january.url`.
    /// When empty, `january` is advertised (local/non-Docker default).
    #[serde(default)]
    pub january_public: String,
    pub livekit: HashMap<String, String>,
    pub assets: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiRegistration {
    pub invite_only: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiSmtp {
    pub host: String,
    pub username: String,
    pub password: String,
    pub from_address: String,
    pub reply_to: Option<String>,
    pub port: Option<i32>,
    pub use_tls: Option<bool>,
    pub use_starttls: Option<bool>,
    pub expiry: EmailExpiry,
}

/// Email expiration config
#[derive(Deserialize, Debug, Clone)]
pub struct EmailExpiry {
    /// How long email verification codes should last for (in seconds)
    pub expire_verification: i64,
    /// How long password reset codes should last for (in seconds)
    pub expire_password_reset: i64,
    /// How long account deletion codes should last for (in seconds)
    pub expire_account_deletion: i64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct PushVapid {
    pub queue: String,
    pub private_key: String,
    pub public_key: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct PushFcm {
    pub queue: String,
    pub key_type: String,
    pub project_id: String,
    pub private_key_id: String,
    pub private_key: String,
    pub client_email: String,
    pub client_id: String,
    pub auth_uri: String,
    pub token_uri: String,
    pub auth_provider_x509_cert_url: String,
    pub client_x509_cert_url: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct PushApn {
    pub queue: String,
    pub sandbox: bool,
    pub pkcs8: String,
    pub key_id: String,
    pub team_id: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiSecurityCaptcha {
    /// Cloudflare Turnstile secret key (server side; never sent to clients)
    pub turnstile_secret: String,
    /// Cloudflare Turnstile site key (public; the web app renders the widget with it)
    pub turnstile_sitekey: String,
    /// New accounts one source address may make a day, on top of the captcha
    #[serde(default = "default_signups_per_source_per_day")]
    pub signups_per_source_per_day: i64,
}

fn default_signups_per_source_per_day() -> i64 {
    3
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiSecurityShield {
    pub host: String,
    pub key: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiSecurityRatelimits {
    /// Requests to `/auth/*` one address may make in 10 seconds
    pub auth: u32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiSecurity {
    pub shield: ApiSecurityShield,
    pub captcha: ApiSecurityCaptcha,
    pub trust_cloudflare: bool,
    pub easypwned: String,
    pub tenor_key: String,
    pub admin_keys: Vec<String>,
    /// Browser origins allowed to call `/staff` (the admin console).
    /// Empty: no browser may call it.
    #[serde(default)]
    pub staff_origins: Vec<String>,
    pub ratelimits: ApiSecurityRatelimits,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiWorkers {
    pub max_concurrent_connections: usize,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiLiveKit {
    pub call_ring_duration: usize,
    pub nodes: HashMap<String, LiveKitNode>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LiveKitNode {
    pub url: String,
    pub lat: f64,
    pub lon: f64,
    pub key: String,
    pub secret: String,

    // whether to hide the node in the nodes list
    #[serde(default)]
    pub private: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiUsers {
    /// Early Adopter: accounts made strictly before this moment (UTC
    /// milliseconds) show the badge. Unset: nobody does. Zrzur's value
    /// (2027-03-01T00:00:00Z = 1803859200000) is a deploy-time setting.
    pub early_adopter_cutoff: Option<u64>,
    pub min_username_length: usize,
    /// The one account that shows the Zrzur Founder badge. No API, bot,
    /// console or command line writes it: changing it is a host-level
    /// configuration change (two people, see Revolt.toml).
    #[serde(default)]
    pub founder_id: Option<String>,
}

impl ApiUsers {
    /// `founder_id`, trimmed; `None` when unset or empty
    pub fn founder(&self) -> Option<&str> {
        self.founder_id.as_deref().map(str::trim).filter(|id| !id.is_empty())
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiSessions {
    /// Most sessions one account keeps; a sign-in over this closes the least
    /// recently used one
    pub limit: usize,
    /// Days without use after which a session expires, 0 for never
    pub idle_days: u32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiAuditLogs {
    /// How long audit log entries last before being removed, in seconds
    pub expires_after: u64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Api {
    pub registration: ApiRegistration,
    pub smtp: ApiSmtp,
    pub security: ApiSecurity,
    pub workers: ApiWorkers,
    pub livekit: ApiLiveKit,
    pub users: ApiUsers,
    pub sessions: ApiSessions,
    pub audit_logs: ApiAuditLogs,
    pub interactions: ApiInteractions,
    /// Accounts on other services a person can link to their profile
    #[serde(default)]
    pub connections: ApiConnections,
    /// Partner, Verified Server and Verified Bot Developer: the numbers that
    /// let someone apply, and how long application data is kept
    #[serde(default)]
    pub badge_programs: ApiBadgePrograms,
}

/// The badge programs people apply for (Partner, Verified Server, Verified
/// Bot Developer, Content Creator). The numbers only let someone apply: a person on the team
/// always decides. Defaults are the owner's numbers (2026-11-23 brief);
/// `Revolt.test.toml` uses small ones.
#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct ApiBadgePrograms {
    /// Partner: genuine human members
    pub partner_members: usize,
    /// Partner: server age in days
    pub partner_age_days: i64,
    /// Partner: average unique weekly active members over `partner_weeks`
    pub partner_weekly_active: u32,
    /// Partner: average unique weekly contributors over `partner_weeks`
    pub partner_weekly_contributors: u32,
    /// Partner: per cent of new members still there after their first week
    pub partner_retention_percent: u32,
    /// Partner: weeks the averages cover
    pub partner_weeks: u32,
    /// Partner: fewer joins than this in the window is "not enough data"
    /// (the retention item then goes to the reviewer)
    pub partner_retention_min_joins: u32,
    /// Partner: a partnership is checked again after this many days
    pub partner_review_days: i64,
    /// Verified Server: genuine human members (standard route)
    pub verified_server_members: usize,
    /// Verified Server: server age in days (standard route)
    pub verified_server_age_days: i64,
    /// Verified Bot Developer: legitimate servers the bot is in
    pub bot_servers: usize,
    /// Verified Bot Developer: bot age in days
    pub bot_age_days: i64,
    /// A server counts for a bot only with this many genuine human members
    pub bot_server_min_humans: usize,
    /// Servers of one owner that count for a bot at most (one person's
    /// servers, or their other accounts', never add up to the number)
    pub bot_servers_per_owner: usize,
    /// crond measures weekly activity of servers with at least this many
    /// members of any kind
    pub metrics_min_members: usize,
    /// After a refusal, a new application waits this many days
    pub reapply_days: i64,
    /// A refusal may be appealed within this many days
    pub appeal_days: i64,
    /// Evidence and ownership codes are deleted this many days after the
    /// final decision (or the end of the appeal window)
    pub evidence_days: i64,
    /// The application and decision record is kept this many days after a
    /// refusal, a withdrawal of a submitted application, or the end of the
    /// badge
    pub record_days: i64,
    /// Drafts nobody touched for this many days are deleted
    pub draft_days: i64,
    /// Content Creator: followers or subscribers on ONE platform (never
    /// added up across platforms), as a reviewer saw them
    pub creator_followers: i64,
    /// Content Creator: days of active publishing
    pub creator_active_days: i64,
    /// Content Creator: substantial original works in the last
    /// `creator_window_days` (or an equivalent livestream schedule)
    pub creator_works: i64,
    pub creator_window_days: i64,
    /// Content Creator: a follower figure older than this many days must be
    /// looked at again before an approval
    pub creator_figures_fresh_days: i64,
    /// Content Creator: the profile code is accepted this many days (a
    /// reviewer checks it by hand, so it waits longer than a DNS code)
    pub creator_code_days: i64,
}

impl Default for ApiBadgePrograms {
    fn default() -> Self {
        ApiBadgePrograms {
            partner_members: 5000,
            partner_age_days: 180,
            partner_weekly_active: 500,
            partner_weekly_contributors: 200,
            partner_retention_percent: 20,
            partner_weeks: 8,
            partner_retention_min_joins: 50,
            partner_review_days: 180,
            verified_server_members: 10000,
            verified_server_age_days: 180,
            bot_servers: 25,
            bot_age_days: 30,
            bot_server_min_humans: 10,
            bot_servers_per_owner: 2,
            metrics_min_members: 5000,
            reapply_days: 90,
            appeal_days: 30,
            evidence_days: 90,
            record_days: 365,
            draft_days: 30,
            creator_followers: 100_000,
            creator_active_days: 90,
            creator_works: 5,
            creator_window_days: 90,
            creator_figures_fresh_days: 30,
            creator_code_days: 90,
        }
    }
}

/// Accounts on other services a person can link to their profile
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ApiConnections {
    #[serde(default)]
    pub steam: ApiConnectionSteam,
    #[serde(default)]
    pub twitch: ApiConnectionOAuth,
    #[serde(default)]
    pub github: ApiConnectionOAuth,
    #[serde(default)]
    pub battlenet: ApiConnectionOAuth,
}

/// A service a person links by signing in on its own OAuth page (Twitch,
/// GitHub, Battle.net). Off while the client id, secret or return origins
/// are missing. The client id and secret live only in the server's config,
/// never in git.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ApiConnectionOAuth {
    /// Client id from the service's developer console
    pub client_id: String,
    /// Client secret from the service's developer console
    pub client_secret: String,
    /// Web app origins the service may send people back to, for example
    /// `https://app.zrzur.com`. Each one's `/connections/<service>/return`
    /// must also be registered at the service.
    pub return_origins: Vec<String>,
    /// Address of the service's sign-in pages. Empty: the service's own.
    /// Changed only for a test stack's mock.
    pub auth_base: String,
    /// Address of the service's API. Empty: the service's own. Changed only
    /// for a test stack's mock.
    pub api_base: String,
    /// Twitch only: secret Twitch signs its live/offline messages with
    /// (10 to 100 characters, made by the server tool). Empty: no live status.
    pub eventsub_secret: String,
    /// Twitch only: public HTTPS address of `/connections/twitch/events` on
    /// this API, port 443. Empty: no live status.
    pub eventsub_callback: String,
}

impl ApiConnectionOAuth {
    /// The service can be linked on this server
    pub fn enabled(&self) -> bool {
        !self.client_id.trim().is_empty()
            && !self.client_secret.trim().is_empty()
            && !self.return_origins.is_empty()
    }

    /// Twitch live status can be received on this server
    pub fn live_enabled(&self) -> bool {
        self.enabled()
            && (10..=100).contains(&self.eventsub_secret.len())
            && self.eventsub_callback.starts_with("https://")
    }

    /// The configured base, or the service's own when empty
    pub fn auth_base_or<'a>(&'a self, official: &'a str) -> &'a str {
        let base = self.auth_base.trim_end_matches('/');
        if base.is_empty() { official } else { base }
    }

    /// The configured API base, or the service's own when empty
    pub fn api_base_or<'a>(&'a self, official: &'a str) -> &'a str {
        let base = self.api_base.trim_end_matches('/');
        if base.is_empty() { official } else { base }
    }
}

/// Steam: sign-in through Steam's OpenID 2.0, then the Steam Web API
#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct ApiConnectionSteam {
    /// Steam Web API key (steamcommunity.com/dev/apikey). Empty switches
    /// Steam off. Lives only in the server's config, never in git.
    pub web_api_key: String,
    /// Web app origins Steam may send people back to, for example
    /// `https://app.zrzur.com`. The client names its own; anything else is
    /// refused.
    pub return_origins: Vec<String>,
    /// Steam's OpenID endpoint. Changed only for a test stack's mock.
    pub openid_endpoint: String,
    /// Base address of the Steam Web API. Changed only for a test stack's mock.
    pub api_base: String,
    /// Steam app ids whose pictures are never shown (a maker asked for it);
    /// their names and hours still show
    pub no_picture_appids: Vec<u32>,
}

impl Default for ApiConnectionSteam {
    fn default() -> Self {
        ApiConnectionSteam {
            web_api_key: String::new(),
            return_origins: vec![],
            openid_endpoint: "https://steamcommunity.com/openid/login".into(),
            api_base: "https://api.steampowered.com".into(),
            no_picture_appids: vec![],
        }
    }
}

impl ApiConnectionSteam {
    /// Steam can be linked on this server
    pub fn enabled(&self) -> bool {
        !self.web_api_key.trim().is_empty() && !self.return_origins.is_empty()
    }
}

/// Bots that receive interactions over HTTP
#[derive(Deserialize, Debug, Clone)]
pub struct ApiInteractions {
    /// Every bot's signing secret is derived from this key; nothing per bot
    /// is stored. Empty, or under 32 bytes, switches the feature off.
    pub signing_key: String,
    /// Self-hosting on a private network only: allow plain http and private
    /// addresses. Never on a public instance: it opens the door to SSRF.
    pub allow_private_urls: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Pushd {
    pub production: bool,
    pub exchange: String,
    pub mass_mention_chunk_size: usize,
    pub render_cache_time: usize,

    // Queues
    pub message_queue: String,
    pub mass_mention_queue: String,
    pub dm_call_queue: String,
    pub fr_accepted_queue: String,
    pub fr_received_queue: String,
    pub generic_queue: String,
    pub ack_queue: String,

    pub vapid: PushVapid,
    pub fcm: PushFcm,
    pub apn: PushApn,
}

impl Pushd {
    fn get_routing_key(&self, key: String) -> String {
        match self.production {
            true => key + "-prd",
            false => key + "-tst",
        }
    }

    pub fn get_ack_routing_key(&self) -> String {
        self.get_routing_key(self.ack_queue.clone())
    }

    pub fn get_message_routing_key(&self) -> String {
        self.get_routing_key(self.message_queue.clone())
    }

    pub fn get_mass_mention_routing_key(&self) -> String {
        self.get_routing_key(self.mass_mention_queue.clone())
    }

    pub fn get_dm_call_routing_key(&self) -> String {
        self.get_routing_key(self.dm_call_queue.clone())
    }

    pub fn get_fr_accepted_routing_key(&self) -> String {
        self.get_routing_key(self.fr_accepted_queue.clone())
    }

    pub fn get_fr_received_routing_key(&self) -> String {
        self.get_routing_key(self.fr_received_queue.clone())
    }

    pub fn get_generic_routing_key(&self) -> String {
        self.get_routing_key(self.generic_queue.clone())
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct January {
    pub blocked_domains: Vec<String>,
    /// Shared with the API workers: when set, `/embed` answers only calls
    /// carrying it. Browsers never call `/embed`.
    #[serde(default)]
    pub embed_key: String,
    /// Signs the tickets `/proxy` checks. Shared by the API and January.
    #[serde(default)]
    pub proxy_key: String,
    /// `/proxy` refuses requests without a valid ticket
    #[serde(default)]
    pub require_proxy_ticket: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct FilesLimit {
    pub min_file_size: usize,
    pub min_resolution: [usize; 2],
    pub max_mega_pixels: usize,
    pub max_pixel_side: usize,
}

#[derive(Deserialize, Debug, Clone)]
pub struct FilesS3 {
    pub endpoint: String,
    pub path_style_buckets: bool,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub default_bucket: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Files {
    pub encryption_key: String,
    pub webp_quality: f32,
    pub blocked_mime_types: Vec<String>,
    pub clamd_host: String,
    pub scan_mime_types: Vec<String>,

    pub limit: FilesLimit,
    pub preview: HashMap<String, [usize; 2]>,
    pub s3: FilesS3,
    #[serde(default)]
    pub purge: FilesPurge,
}

/// Purge by URL at the CDN. Empty means no purge: nothing fails, and files
/// stay at the CDN for one hour at most.
#[derive(Deserialize, Clone, Default)]
pub struct FilesPurge {
    /// Cloudflare zone of the files host
    #[serde(default)]
    pub zone_id: String,
    /// Token limited to Cache Purge. Goes in `Revolt.overrides.toml` or
    /// `REVOLT__FILES__PURGE__API_TOKEN`, never in the repository.
    #[serde(default)]
    pub api_token: String,
    /// Cloudflare's API; only tests change it
    #[serde(default)]
    pub api_url: String,
}

impl FilesPurge {
    /// Purge works, so files may stay at the CDN for longer
    pub fn configured(&self) -> bool {
        !self.zone_id.trim().is_empty() && !self.api_token.trim().is_empty()
    }
}

impl std::fmt::Debug for FilesPurge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesPurge")
            .field("zone_id", &self.zone_id)
            .field("configured", &self.configured())
            .finish()
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct GlobalLimits {
    pub group_size: usize,
    pub message_embeds: usize,
    pub message_replies: usize,
    pub message_reactions: usize,
    pub server_emoji: usize,
    pub server_roles: usize,
    pub server_channels: usize,

    pub new_user_hours: usize,

    pub body_limit_size: usize,

    pub restrict_server_creation: Vec<String>,

    pub max_invite_duration_days: usize,

    /// Days until an invite expires when its creator chose no time
    #[serde(default = "default_invite_duration_days")]
    pub default_invite_duration_days: usize,

    /// Raid alert: more than this many joins within raid_alert_minutes
    /// posts an alert in the join message channel (0 = off)
    #[serde(default = "default_raid_alert_joins")]
    pub raid_alert_joins: usize,
    #[serde(default = "default_raid_alert_minutes")]
    pub raid_alert_minutes: usize,

    /// Longest member timeout in days
    #[serde(default = "default_max_timeout_days")]
    pub max_timeout_days: usize,

    /// Bots a user may make per 7 days (0 = no pacing)
    #[serde(default = "default_bot_creations_per_week")]
    pub bot_creations_per_week: usize,
}

fn default_bot_creations_per_week() -> usize {
    7
}

fn default_raid_alert_joins() -> usize {
    10
}

fn default_raid_alert_minutes() -> usize {
    1
}

fn default_max_timeout_days() -> usize {
    28
}

fn default_invite_duration_days() -> usize {
    7
}

#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesLimits {
    pub outgoing_friend_requests: usize,

    pub bots: usize,
    pub message_length: usize,
    pub message_attachments: usize,
    pub servers: usize,
    pub voice_quality: u32,
    pub video: bool,
    pub video_resolution: [u32; 2],
    pub video_aspect_ratio: [f32; 2],
    /// Highest frame rate for camera and screen share (honoured by clients)
    #[serde(default = "default_video_framerate")]
    pub video_framerate: u32,

    pub file_upload_size_limit: HashMap<String, usize>,
}

fn default_video_framerate() -> u32 {
    30
}

/// One supercharge level: how many supercharges a server needs and what
/// everyone in it gets
#[derive(Deserialize, Debug, Clone)]
pub struct SuperchargeLevel {
    pub supercharges: usize,
    pub server_emoji: usize,
    /// Attachment upload size for everyone in the server (bytes)
    pub attachments: usize,
    /// Voice bitrate in bits per second (honoured by clients)
    pub voice_bitrate: u32,
    pub video_resolution: [u32; 2],
    pub video_framerate: u32,
    /// The server may claim a custom invite link (zrzur.cc/<name>)
    #[serde(default)]
    pub vanity_invite: bool,
}

/// Lowest level (1-based) whose perks include a custom invite link
pub fn vanity_level_of(levels: &[SuperchargeLevel]) -> Option<usize> {
    levels
        .iter()
        .position(|level| level.vanity_invite)
        .map(|index| index + 1)
}

/// How billing runs. Worked out once from the config, the provider secrets
/// in the environment and how the server was built (see
/// `FeaturesBilling::resolve_mode`); anything unclear resolves to the
/// safest mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BillingMode {
    /// No billing: routes refuse, no perks apply
    #[default]
    Disabled,
    /// Built-in simulated provider (the app confirms its own payments).
    /// Only on local development hosts, never in production.
    InternalTest,
    /// Real provider with test keys: no real money, perks marked test
    Sandbox,
    /// Live keys configured, but nobody can start a checkout
    LiveReady,
    /// Real payments. Needs a `billing-live` build, live keys, the owner's
    /// activation value and `checkout_enabled`.
    LiveEnabled,
}

impl BillingMode {
    /// Billing is on (store shown, perks of this mode apply)
    pub fn active(self) -> bool {
        self != BillingMode::Disabled
    }

    /// Real money: only perks made with live payments count
    pub fn live(self) -> bool {
        matches!(self, BillingMode::LiveReady | BillingMode::LiveEnabled)
    }

    /// Users may start a purchase
    pub fn checkout_open(self) -> bool {
        matches!(
            self,
            BillingMode::InternalTest | BillingMode::Sandbox | BillingMode::LiveEnabled
        )
    }

    /// A real payment provider handles payments
    pub fn uses_provider(self) -> bool {
        matches!(
            self,
            BillingMode::Sandbox | BillingMode::LiveReady | BillingMode::LiveEnabled
        )
    }

    /// Whether something bought with this test flag counts in this mode
    pub fn counts(self, test: bool) -> bool {
        self.active() && test != self.live()
    }

    pub fn name(self) -> &'static str {
        match self {
            BillingMode::Disabled => "disabled",
            BillingMode::InternalTest => "internal_test",
            BillingMode::Sandbox => "sandbox",
            BillingMode::LiveReady => "live_ready",
            BillingMode::LiveEnabled => "live_enabled",
        }
    }
}

/// The payment provider. Secrets are never kept in the config: it only
/// names the environment variables that hold them.
#[derive(Deserialize, Debug, Clone)]
pub struct BillingProvider {
    /// `` (none: only the built-in test provider), `stripe` or `paypal`
    #[serde(default)]
    pub name: String,
    /// Environment variable with the provider's public client id (PayPal;
    /// empty: `ZRZUR_PAYPAL_CLIENT_ID`)
    #[serde(default)]
    pub client_id_env: String,
    /// `sandbox` or `live`
    #[serde(default = "default_provider_environment")]
    pub environment: String,
    /// Environment variable with the provider's secret API key
    #[serde(default = "default_secret_key_env")]
    pub secret_key_env: String,
    /// Environment variable with the webhook signing secret
    #[serde(default = "default_webhook_secret_env")]
    pub webhook_secret_env: String,
    /// Environment variable with the previous webhook signing secret,
    /// accepted too while rotating
    #[serde(default = "default_webhook_secret_previous_env")]
    pub webhook_secret_previous_env: String,
    /// Provider price of Plus (monthly). Empty: the price is sent inline.
    #[serde(default)]
    pub plus_price_id: String,
    /// Provider price of one feather (monthly). Empty: sent inline.
    #[serde(default)]
    pub supercharge_price_id: String,
    /// Owner switch for live checkout (needs the rest of the live gate too)
    #[serde(default)]
    pub checkout_enabled: bool,
    /// Accounts that may buy while a production server runs the sandbox
    /// (staging testers). Without any, the sandbox stays off in production.
    #[serde(default)]
    pub sandbox_testers: Vec<String>,
    /// Provider API address for local tests (http://127.0.0.1 only, never
    /// used with live keys)
    #[serde(default)]
    pub api_base: String,
    /// Unpaid checkouts expire after this many minutes (30 to 1440)
    #[serde(default = "default_checkout_minutes")]
    pub checkout_minutes: u32,
    /// Days a Plus member keeps perks while a renewal payment is retried
    #[serde(default = "default_past_due_grace_days")]
    pub past_due_grace_days: u32,
}

fn default_provider_environment() -> String {
    "sandbox".to_string()
}

fn default_secret_key_env() -> String {
    "ZRZUR_STRIPE_SECRET_KEY".to_string()
}

fn default_webhook_secret_env() -> String {
    "ZRZUR_STRIPE_WEBHOOK_SECRET".to_string()
}

fn default_webhook_secret_previous_env() -> String {
    "ZRZUR_STRIPE_WEBHOOK_SECRET_PREVIOUS".to_string()
}

fn default_checkout_minutes() -> u32 {
    30
}

fn default_past_due_grace_days() -> u32 {
    3
}

impl BillingProvider {
    /// Variable names actually used: PayPal entries left on the Stripe
    /// defaults read PayPal's own variables instead
    fn env_names(&self) -> (String, String, String, String) {
        let paypal = self.name == "paypal";
        let pick = |configured: &str, stripe_default: String, paypal_default: &str| {
            if paypal && (configured.is_empty() || configured == stripe_default) {
                paypal_default.to_string()
            } else {
                configured.to_string()
            }
        };
        (
            if self.client_id_env.is_empty() && paypal {
                "ZRZUR_PAYPAL_CLIENT_ID".to_string()
            } else {
                self.client_id_env.clone()
            },
            pick(&self.secret_key_env, default_secret_key_env(), "ZRZUR_PAYPAL_CLIENT_SECRET"),
            pick(&self.webhook_secret_env, default_webhook_secret_env(), "ZRZUR_PAYPAL_WEBHOOK_ID"),
            pick(
                &self.webhook_secret_previous_env,
                default_webhook_secret_previous_env(),
                "",
            ),
        )
    }

    /// Public client id (PayPal)
    pub fn client_id(&self) -> Option<String> {
        env_secret(&self.env_names().0)
    }

    /// Secret API key (Stripe) or client secret (PayPal)
    pub fn secret_key(&self) -> Option<String> {
        env_secret(&self.env_names().1)
    }

    /// Webhook signing secrets (Stripe, current first) or the webhook id
    /// (PayPal)
    pub fn webhook_secrets(&self) -> Vec<String> {
        let (_, _, current, previous) = self.env_names();
        [env_secret(&current), env_secret(&previous)]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// Whether a provider entry can take payments: Some(live) when its keys are
/// all there and fit its environment, None otherwise (it is then left out)
pub fn provider_ready(provider: &BillingProvider) -> Option<bool> {
    let live = match provider.environment.as_str() {
        "sandbox" => false,
        "live" => true,
        _ => return None,
    };
    // a test address is for local tests: sandbox and loopback only
    if !provider.api_base.is_empty()
        && (live || !(provider.api_base.starts_with("http://127.0.0.1:")
            || provider.api_base == "http://127.0.0.1"))
    {
        return None;
    }
    if provider.webhook_secrets().is_empty() {
        return None;
    }
    let key = provider.secret_key()?;
    match provider.name.as_str() {
        "stripe" => {
            let test_key = key.starts_with("sk_test_") || key.starts_with("rk_test_");
            let live_key = key.starts_with("sk_live_") || key.starts_with("rk_live_");
            (if live { live_key } else { test_key }).then_some(live)
        }
        // PayPal keys carry no environment: it comes from the config
        "paypal" => provider.client_id().map(|_| live),
        _ => None,
    }
}

impl Default for BillingProvider {
    fn default() -> Self {
        Self {
            name: String::new(),
            client_id_env: String::new(),
            environment: default_provider_environment(),
            secret_key_env: default_secret_key_env(),
            webhook_secret_env: default_webhook_secret_env(),
            webhook_secret_previous_env: default_webhook_secret_previous_env(),
            plus_price_id: String::new(),
            supercharge_price_id: String::new(),
            checkout_enabled: false,
            sandbox_testers: vec![],
            api_base: String::new(),
            checkout_minutes: default_checkout_minutes(),
            past_due_grace_days: default_past_due_grace_days(),
        }
    }
}

/// Environment variable the owner sets to activate live payments. Its value
/// must be `LIVE_ACTIVATION_PREFIX` followed by `hosts.api`, so a value
/// copied from another deployment does nothing.
pub const LIVE_ACTIVATION_ENV: &str = "ZRZUR_BILLING_LIVE_ACTIVATION";
pub const LIVE_ACTIVATION_PREFIX: &str = "activate-live-payments:";

fn env_secret(name: &str) -> Option<String> {
    env_secret_from(name, |key| std::env::var(key).ok())
}

/// Read a trimmed, non-empty secret through `lookup` (the process environment in production)
fn env_secret_from(name: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    lookup(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Whether the API URL is the public Zrzur API (staging and look-alike hosts are not)
fn is_production_api(api: &str) -> bool {
    api.trim_end_matches('/') == "https://api.zrzur.com"
}

/// Whether a URL points at this machine or a development name
pub fn is_local_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .or_else(|| url.strip_prefix("ws://"))
        .or_else(|| url.strip_prefix("wss://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next().unwrap_or_default()
    } else {
        authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host.ends_with(".localhost")
        || host.ends_with(".test")
}

/// Plus, feathers (supercharges), gifts and Discover promotion.
///
/// See `BillingMode` for the modes. With `enabled` off, or anything about
/// the provider unclear, every billing route refuses and nobody gets perks
/// from it.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct FeaturesBilling {
    #[serde(default)]
    pub enabled: bool,
    /// Built-in simulated provider (local development only)
    #[serde(default)]
    pub test_mode: bool,
    /// Real payment provider
    #[serde(default)]
    pub provider: BillingProvider,
    /// More providers next to `provider` (a buyer picks one)
    #[serde(default)]
    pub providers: Vec<BillingProvider>,
    /// Mode worked out when the config was loaded
    #[serde(skip)]
    pub resolved_mode: BillingMode,
    /// ISO currency code shown with prices
    #[serde(default)]
    pub currency: String,
    /// Monthly Plus price in minor units (cents)
    #[serde(default)]
    pub plus_price: u32,
    /// Monthly price of one supercharge in minor units
    #[serde(default)]
    pub supercharge_price: u32,
    /// Supercharges that come with Plus
    #[serde(default)]
    pub plus_free_supercharges: usize,
    /// Emoji slots of a server without supercharges while billing is
    /// available (otherwise `limits.global.server_emoji` applies)
    #[serde(default)]
    pub free_server_emoji: usize,
    /// Official server whose Plus members get `plus_role`
    #[serde(default)]
    pub hq_server: String,
    #[serde(default)]
    pub plus_role: String,
    /// Levels, lowest first
    #[serde(default)]
    pub levels: Vec<SuperchargeLevel>,
    /// Most supercharges one member may hold (stops runaway purchases)
    #[serde(default = "default_max_supercharges")]
    pub max_supercharges_per_user: usize,

    /// Discover promotion
    #[serde(default)]
    pub discover: DiscoverPromotionConfig,
}

fn default_max_supercharges() -> usize {
    30
}

/// One Discover boost tier
#[derive(Deserialize, Debug, Clone)]
pub struct BoostTier {
    /// Boost, SuperBoost or MegaBoost
    pub kind: String,
    /// Price in minor units for `promotion_days`
    pub price: u32,
    /// Guaranteed position: the server shows at this place or higher
    pub guarantee: usize,
    /// Places in this tier at once (0: worked out from the guarantees, so
    /// every guarantee can be kept)
    #[serde(default)]
    pub capacity: usize,
}

/// Paid placement on Discover
#[derive(Deserialize, Debug, Clone)]
pub struct DiscoverPromotionConfig {
    /// Members a server needs to be listed without a boost
    #[serde(default)]
    pub min_members: usize,
    /// Length of one purchase
    #[serde(default = "default_promotion_days")]
    pub promotion_days: u32,
    /// Boost tiers, lowest first
    #[serde(default)]
    pub boosts: Vec<BoostTier>,
    /// Price of the top row for `promotion_days`
    #[serde(default)]
    pub promoted_price: u32,
    /// Places in the top row
    #[serde(default)]
    pub promoted_slots: usize,
}

fn default_promotion_days() -> u32 {
    7
}

impl Default for DiscoverPromotionConfig {
    fn default() -> Self {
        Self {
            min_members: 0,
            promotion_days: default_promotion_days(),
            boosts: vec![],
            promoted_price: 0,
            promoted_slots: 0,
        }
    }
}

impl DiscoverPromotionConfig {
    /// Rank of a boost kind (higher is bigger), if it exists
    pub fn boost_rank(&self, kind: &str) -> Option<usize> {
        self.boosts.iter().position(|tier| tier.kind == kind)
    }

    /// Places of a boost tier, per kind of listing (servers or bots). By
    /// default each tier gets the places between its guarantee and the next
    /// bigger tier's, e.g. top 20 / top 50 / top 100 → 20, 30 and 50.
    pub fn boost_capacity(&self, kind: &str) -> usize {
        let Some(index) = self.boost_rank(kind) else {
            return 0;
        };
        let tier = &self.boosts[index];
        if tier.capacity > 0 {
            return tier.capacity;
        }
        let bigger = self
            .boosts
            .get(index + 1)
            .map(|next| next.guarantee)
            .unwrap_or(0);
        tier.guarantee.saturating_sub(bigger).max(1)
    }
}

impl FeaturesBilling {
    /// Whether billing is on (store shown, perks apply). Purchases need
    /// `mode().checkout_open()` as well.
    pub fn available(&self) -> bool {
        self.resolved_mode.active()
    }

    pub fn mode(&self) -> BillingMode {
        self.resolved_mode
    }

    /// Whether something bought with this test flag counts right now:
    /// sandbox and simulated purchases never count on a live server, live
    /// ones never on a test server
    pub fn counts(&self, test: bool) -> bool {
        self.resolved_mode.counts(test)
    }

    /// Test flag of things bought or granted right now
    pub fn new_test_flag(&self) -> bool {
        !self.resolved_mode.live()
    }

    /// Secret API key of the provider (read from the environment each time)
    pub fn provider_secret_key(&self) -> Option<String> {
        self.provider.secret_key()
    }

    /// Webhook signing secrets: current first, then the one being rotated out
    pub fn provider_webhook_secrets(&self) -> Vec<String> {
        self.provider.webhook_secrets()
    }

    /// Work out the mode. Fails closed: every unclear or half-configured
    /// case is `Disabled`, and live payments additionally need a
    /// `billing-live` build, `checkout_enabled`, the owner's activation
    /// value, legal links and https hosts, else they stay `LiveReady`.
    pub fn resolve_mode(&self, hosts: &Hosts, production: bool, legal: &LegalLinks) -> BillingMode {
        if !self.enabled {
            return BillingMode::Disabled;
        }

        match self.all_providers().next() {
            None => {
                // simulated payments: local development only (zrzur-u5-01)
                if self.test_mode
                    && !production
                    && is_local_url(&hosts.api)
                    && is_local_url(&hosts.app)
                {
                    BillingMode::InternalTest
                } else {
                    if self.test_mode {
                        log::error!(
                            "billing: built-in test provider refused outside local development; billing stays off"
                        );
                    }
                    BillingMode::Disabled
                }
            }
            Some(_) => {
                if self.test_mode {
                    log::error!(
                        "billing: test_mode is for the built-in provider only; billing stays off"
                    );
                    return BillingMode::Disabled;
                }
                // every configured provider is checked on its own: a
                // half-configured one is left out and never stops the rest
                let Some(live) = self.ready_environment() else {
                    log::warn!("billing: no payment provider is ready; billing stays off");
                    return BillingMode::Disabled;
                };
                if live {
                    if self.live_activation_complete(hosts, legal) {
                        BillingMode::LiveEnabled
                    } else {
                        BillingMode::LiveReady
                    }
                } else if production && self.provider.sandbox_testers.is_empty() {
                    log::error!(
                        "billing: sandbox on a production server needs sandbox_testers; billing stays off"
                    );
                    BillingMode::Disabled
                } else {
                    BillingMode::Sandbox
                }
            }
        }
    }

    /// Every configured provider: `provider` first, then `providers`
    pub fn all_providers(&self) -> impl Iterator<Item = &BillingProvider> {
        std::iter::once(&self.provider)
            .chain(self.providers.iter())
            .filter(|provider| !provider.name.is_empty())
    }

    /// Environment of the first ready provider (Some(live)); providers in
    /// the other environment are left out
    fn ready_environment(&self) -> Option<bool> {
        let mut environment = None;
        for provider in self.all_providers() {
            match provider_ready(provider) {
                Some(live) if environment.is_none() => environment = Some(live),
                Some(live) if environment != Some(live) => log::error!(
                    "billing: {} is in another environment than the first provider; left out",
                    provider.name
                ),
                Some(_) => {}
                None => log::warn!(
                    "billing: {} is not fully configured (keys, webhook or environment); left out",
                    provider.name
                ),
            }
        }
        environment
    }

    /// Providers that take payments right now, in order of preference
    pub fn ready_providers(&self) -> Vec<&BillingProvider> {
        let Some(live) = self.ready_environment() else {
            return vec![];
        };
        let mut seen = std::collections::HashSet::new();
        self.all_providers()
            .filter(|provider| provider_ready(provider) == Some(live))
            // the same provider listed twice counts once
            .filter(|provider| seen.insert(provider.name.clone()))
            .collect()
    }

    /// Names of `ready_providers`
    pub fn ready_provider_names(&self) -> Vec<&str> {
        self.ready_providers()
            .into_iter()
            .map(|provider| provider.name.as_str())
            .collect()
    }

    /// Everything live checkout needs besides live keys
    pub fn live_activation_complete(&self, hosts: &Hosts, legal: &LegalLinks) -> bool {
        self.live_gate_report(hosts, legal)
            .iter()
            .all(|(_, passed)| *passed)
    }

    /// Each live gate check and whether it passes (for the readiness report;
    /// never contains secrets)
    pub fn live_gate_report(&self, hosts: &Hosts, legal: &LegalLinks) -> Vec<(&'static str, bool)> {
        let expected = format!("{LIVE_ACTIVATION_PREFIX}{}", hosts.api);
        vec![
            ("build_has_billing_live_feature", cfg!(feature = "billing-live")),
            ("provider_checkout_enabled", self.provider.checkout_enabled),
            (
                "owner_activation_value",
                env_secret(LIVE_ACTIVATION_ENV).is_some_and(|value| value == expected),
            ),
            (
                "https_hosts",
                hosts.api.starts_with("https://") && hosts.app.starts_with("https://"),
            ),
            ("terms_of_service_link", !legal.terms_of_service.is_empty()),
            ("privacy_policy_link", !legal.privacy_policy.is_empty()),
            (
                "no_test_api_base",
                self.all_providers().all(|provider| provider.api_base.is_empty()),
            ),
        ]
    }

    /// Level reached with this many supercharges (0 = none)
    pub fn level_for(&self, supercharges: usize) -> usize {
        self.levels
            .iter()
            .take_while(|level| supercharges >= level.supercharges)
            .count()
    }

    /// Level that unlocks a custom invite link, if any level does
    pub fn vanity_level(&self) -> Option<usize> {
        vanity_level_of(&self.levels)
    }

    /// Perks of a level, if it is above 0
    pub fn perks(&self, level: usize) -> Option<&SuperchargeLevel> {
        level.checked_sub(1).and_then(|i| self.levels.get(i))
    }

    /// Emoji slots of a server at this level
    pub fn server_emoji(&self, level: usize, global: usize) -> usize {
        if !self.available() {
            return global;
        }
        match self.perks(level) {
            Some(perks) => perks.server_emoji,
            None if self.free_server_emoji > 0 => self.free_server_emoji,
            None => global,
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesLimitsCollection {
    pub global: GlobalLimits,

    pub new_user: FeaturesLimits,
    pub default: FeaturesLimits,

    #[serde(flatten)]
    pub roles: HashMap<String, FeaturesLimits>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LegalLinks {
    /// Terms of Service URL
    pub terms_of_service: String,
    /// Privacy Policy URL
    pub privacy_policy: String,
    /// Guidelines URL
    pub guidelines: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesAdvanced {
    #[serde(default)]
    pub process_message_delay_limit: u16,
    #[serde(default)]
    pub seen_events_cache_size: u32,
}

impl Default for FeaturesAdvanced {
    fn default() -> Self {
        Self {
            process_message_delay_limit: 5,
            seen_events_cache_size: 20,
        }
    }
}

fn default_join_confirm_hours() -> u64 {
    72
}

#[derive(Deserialize, Debug, Clone)]
pub struct Features {
    pub limits: FeaturesLimitsCollection,
    pub legal_links: LegalLinks,
    pub webhooks_enabled: bool,
    pub mass_mentions_send_notifications: bool,
    pub mass_mentions_enabled: bool,
    /// Plugins are open: the store, publishing, review and installs, staff
    /// routes included. Off, every one of them answers `PluginsDisabled` and
    /// the app loads no plugin; stored plugins and installs stay untouched.
    /// Unset means off.
    #[serde(default)]
    pub plugins_enabled: bool,
    /// Messages can be forwarded to another channel
    /// (`POST /channels/{target}/messages/forward`). Unset means off.
    #[serde(default)]
    pub message_forwarding_enabled: bool,
    /// Messages can carry polls (votes, voter lists, early end; crond
    /// closes them). Unset means off.
    #[serde(default)]
    pub polls_enabled: bool,
    /// Scheduled server events (`/servers/{target}/events`). Unset means off.
    #[serde(default)]
    pub server_events_enabled: bool,
    /// The short path from an invite link: the invite page says what is
    /// inside the server, and `POST /auth/account/join` makes an account
    /// that enters the invited server before its e-mail is confirmed.
    /// Unset means off.
    #[serde(default)]
    pub join_by_link_enabled: bool,
    /// Bots in direct messages: anyone may write to a public bot without a
    /// shared server, and a person can press Start (with the payload of a
    /// bot link). Unset means off.
    #[serde(default)]
    pub bot_dms_enabled: bool,
    /// Hours an account made by `POST /auth/account/join` has to confirm its
    /// e-mail before crond removes it. Unset means 72.
    #[serde(default = "default_join_confirm_hours")]
    pub join_confirm_hours: u64,
    /// Organisers can open an event's share link to people outside the
    /// server (title, time, cover and server name only). Unset means off:
    /// a share link then works for members only.
    #[serde(default)]
    pub server_event_public_links: bool,

    #[serde(default)]
    pub advanced: FeaturesAdvanced,

    #[serde(default)]
    pub billing: FeaturesBilling,

    #[serde(default)]
    pub community: FeaturesCommunity,

    #[serde(default)]
    pub age_safety: FeaturesAgeSafety,

    #[serde(default)]
    pub discover_review: FeaturesDiscoverReview,

    #[serde(default)]
    pub server_import: FeaturesServerImport,

    #[serde(default)]
    pub badge_applications: FeaturesBadgeApplications,
    /// Boxed: the settings are copied onto the stack of every request, and
    /// some long test walks have no room left for another wide field
    #[serde(default)]
    pub team_applications: Box<FeaturesTeamApplications>,
    #[serde(default)]
    pub saved_reminders: FeaturesSavedReminders,
    /// Rich presence: what someone is doing right now ("Playing ..."),
    /// set by the desktop app, local apps over its RPC, and Steam. Boxed
    /// for the same reason as `team_applications`.
    #[serde(default)]
    pub rich_presence: Box<FeaturesRichPresence>,
    /// The owner's `/badge` command (off unless both ids are set)
    #[serde(default)]
    pub badge_command: FeaturesBadgeCommand,
    /// Who may hold a custom invite link (zrzur.cc/<name>)
    #[serde(default)]
    pub vanity: FeaturesVanity,

    /// Zrzur Desk: staff manage the platform from a bot, each with their own
    /// grant (off unless enabled, the bot is set and a place is allowed)
    #[serde(default)]
    pub staff_desk: FeaturesStaffDesk,

    /// Voice rooms step down on a weak connection (lighter voice, video
    /// lowered, then video paused with audio kept) and show a sign. The
    /// steps are taken by each person's app; this switch and the numbers in
    /// `voice_weak_network` reach it through `GET /`. Unset means off.
    #[serde(default)]
    pub voice_weak_network_enabled: bool,
    /// The gateway may send large JSON frames compressed (raw DEFLATE in a
    /// binary frame) to a client that connects with `?compression=deflate`.
    /// Clients that do not ask get text frames as before. Unset means off.
    #[serde(default)]
    pub gateway_compression_enabled: bool,
    /// Voice clips: the last seconds of a voice room, saved by a person in
    /// it and shared as a message (`/channels/{target}/capture/...`). Unset
    /// means off.
    #[serde(default)]
    pub voice_clips_enabled: bool,
    /// Recorded rooms: an episode of a voice room that members can hear
    /// later. Unset means off.
    #[serde(default)]
    pub voice_recordings_enabled: bool,

    #[serde(default)]
    pub voice_weak_network: FeaturesVoiceWeakNetwork,

    #[serde(default)]
    pub voice_capture: FeaturesVoiceCapture,
}

/// Applying for badge programs (Partner, Verified Server, Verified Bot
/// Developer, Content Creator) from the app. Off, every applicant route
/// (`/badges/programs/*`, `/badges/applications*`) answers
/// `FeatureDisabled` and the app shows no way in; the team still reviews
/// and decides the applications already made in the admin console.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct FeaturesBadgeApplications {
    /// Unset means off
    #[serde(default)]
    pub enabled: bool,
}

/// Building a server from a layout read out of a Discord server template
#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesServerImport {
    /// The whole feature: the preview and the build. Off, both routes answer
    /// `FeatureDisabled` and the app shows no way in.
    #[serde(default)]
    pub enabled: bool,
    /// Reading a template by its link, which makes this server call
    /// Discord. Off, people paste the template's text instead; nothing else
    /// changes. This is the switch to turn if Discord refuses the calls.
    #[serde(default)]
    pub link_fetch: bool,
    /// Where a template is read from; its code is put at the end. Only for
    /// operators and tests: a person never chooses the address.
    #[serde(default = "server_import_endpoint")]
    pub template_endpoint: String,
    /// Link reads one account may ask for in an hour
    #[serde(default = "server_import_per_hour")]
    pub reads_per_hour: i64,
}

fn server_import_endpoint() -> String {
    "https://discord.com/api/v10/guilds/templates/".to_string()
}
fn server_import_per_hour() -> i64 {
    5
}

impl Default for FeaturesServerImport {
    fn default() -> Self {
        Self {
            enabled: false,
            link_fetch: false,
            template_endpoint: server_import_endpoint(),
            reads_per_hour: server_import_per_hour(),
        }
    }
}

/// `/badge` in one bot: the owner's account gives and takes platform badges
/// (routes under `/badges/command`). The bot only hands back the press; the
/// server checks it came from `owner`. Either value empty switches it off.
#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct FeaturesBadgeCommand {
    /// The only account that may use the command
    #[serde(default)]
    pub owner: String,
    /// The bot that offers the command
    #[serde(default)]
    pub bot: String,
}

impl FeaturesBadgeCommand {
    pub fn enabled(&self) -> bool {
        !self.owner.trim().is_empty() && !self.bot.trim().is_empty()
    }
}

/// Rich presence (`SetActivity` on the gateway, `activities` on users).
/// Activities live only in Redis while the person is online.
#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesRichPresence {
    /// The whole feature. Off, `SetActivity` is refused with
    /// `FeatureDisabled`, users carry no activities and Steam is not read.
    /// Unset means off.
    #[serde(default)]
    pub enabled: bool,
    /// Activity changes one person may make in `window_seconds`
    #[serde(default = "rich_presence_updates")]
    pub updates_per_window: i64,
    #[serde(default = "rich_presence_window")]
    pub window_seconds: u64,
    /// Read "now playing" from linked Steam accounts of people who are
    /// online and chose to share it. Needs the Steam connection's key.
    #[serde(default)]
    pub steam_enabled: bool,
    /// Seconds between two rounds of Steam reads, at the least
    #[serde(default = "rich_presence_steam_interval")]
    pub steam_interval_seconds: u64,
    /// Steam Web API calls a day this feature may make (each call reads up
    /// to 100 accounts). Steam allows 100,000 a day for one key, shared with
    /// the Steam connection, so stay well below. Rounds stretch to keep
    /// within it; when it is used up, Steam is not read until the next day.
    #[serde(default = "rich_presence_steam_budget")]
    pub steam_daily_calls: i64,
    /// A change is sent at once only to servers with at most this many
    /// members; in larger ones people see it when they open the profile or
    /// the member list (friends and direct messages are told as usual)
    #[serde(default = "rich_presence_broadcast_max")]
    pub broadcast_max_members: usize,
}

fn rich_presence_broadcast_max() -> usize {
    1000
}

fn rich_presence_updates() -> i64 {
    5
}
fn rich_presence_window() -> u64 {
    20
}
fn rich_presence_steam_interval() -> u64 {
    60
}
fn rich_presence_steam_budget() -> i64 {
    20_000
}

impl Default for FeaturesRichPresence {
    fn default() -> Self {
        Self {
            enabled: false,
            updates_per_window: rich_presence_updates(),
            window_seconds: rich_presence_window(),
            steam_enabled: false,
            steam_interval_seconds: rich_presence_steam_interval(),
            steam_daily_calls: rich_presence_steam_budget(),
            broadcast_max_members: rich_presence_broadcast_max(),
        }
    }
}

/// Zrzur Desk: the staff bot. The bot decides nothing; the server checks
/// every press against the presser's own staff grant.
#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct FeaturesStaffDesk {
    /// The whole desk
    #[serde(default)]
    pub enabled: bool,
    /// The Zrzur Desk bot id
    #[serde(default)]
    pub bot: String,
    /// Channel ids where desk interactions are accepted
    #[serde(default)]
    pub places: Vec<String>,
    /// Also accept DMs with the bot
    #[serde(default)]
    pub allow_dms: bool,
    /// Areas the desk offers; Phase 0 knows only `overview`
    #[serde(default)]
    pub areas: Vec<String>,
    /// Channel id of the staff inbox: one line there for every new report,
    /// support ticket, bug and feedback (ids and kinds only). Empty: none.
    #[serde(default)]
    pub inbox: String,
}

impl FeaturesStaffDesk {
    /// On only when enabled, the bot is set and a place is allowed
    pub fn on(&self) -> bool {
        self.enabled
            && !self.bot.trim().is_empty()
            && (self.allow_dms || self.places.iter().any(|p| !p.trim().is_empty()))
    }
}

/// Saving a message to a private list and "remind me" on a message
/// (`/users/@me/saved`, `/users/@me/reminders`; crond delivers reminders)
#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesSavedReminders {
    /// Both parts are open. Unset means off.
    #[serde(default)]
    pub enabled: bool,
    /// Messages one person may keep saved. Saving costs one small record,
    /// so this only stops runaway scripts.
    #[serde(default = "saved_reminders_max_saved")]
    pub max_saved: usize,
    /// Reminders one person may have waiting at the same time
    #[serde(default = "saved_reminders_max_active")]
    pub max_active: usize,
    /// The same with Feather
    #[serde(default = "saved_reminders_max_active_plus")]
    pub max_active_plus: usize,
    /// How far ahead a reminder may be set, in days
    #[serde(default = "saved_reminders_max_days_ahead")]
    pub max_days_ahead: u32,
    /// Days a delivered reminder stays in the list before crond removes it
    #[serde(default = "saved_reminders_keep_delivered_days")]
    pub keep_delivered_days: u32,
}

fn saved_reminders_max_saved() -> usize {
    1000
}
fn saved_reminders_max_active() -> usize {
    10
}
fn saved_reminders_max_active_plus() -> usize {
    100
}
fn saved_reminders_max_days_ahead() -> u32 {
    366
}
fn saved_reminders_keep_delivered_days() -> u32 {
    30
}

/// Who may hold a custom invite link (zrzur.cc/<name>), owner rule of
/// 2026-11-21: official and partner servers always; any other server once it
/// has `min_members` members and is active (`active_messages` messages from
/// `active_authors` different members in the last `active_days` days, bots,
/// webhooks and system messages not counted). With billing on, the Wings
/// level that unlocks the link is a second way in.
#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesVanity {
    #[serde(default = "vanity_min_members")]
    pub min_members: usize,
    #[serde(default = "vanity_active_messages")]
    pub active_messages: usize,
    #[serde(default = "vanity_active_authors")]
    pub active_authors: usize,
    #[serde(default = "vanity_active_days")]
    pub active_days: u32,
    /// Days a link keeps working after its server stops meeting the rule
    /// (the owner gets a notice); then the name is released
    #[serde(default = "vanity_notice_days")]
    pub notice_days: u32,
    /// Hours an activity count is reused before it is counted again
    #[serde(default = "vanity_activity_cache_hours")]
    pub activity_cache_hours: u32,
}

fn vanity_min_members() -> usize {
    500
}
fn vanity_active_messages() -> usize {
    100
}
fn vanity_active_authors() -> usize {
    20
}
fn vanity_active_days() -> u32 {
    30
}
fn vanity_notice_days() -> u32 {
    30
}
fn vanity_activity_cache_hours() -> u32 {
    6
}

impl Default for FeaturesVanity {
    fn default() -> Self {
        Self {
            min_members: vanity_min_members(),
            active_messages: vanity_active_messages(),
            active_authors: vanity_active_authors(),
            active_days: vanity_active_days(),
            notice_days: vanity_notice_days(),
            activity_cache_hours: vanity_activity_cache_hours(),
        }
    }
}

impl Default for FeaturesSavedReminders {
    fn default() -> Self {
        Self {
            enabled: false,
            max_saved: saved_reminders_max_saved(),
            max_active: saved_reminders_max_active(),
            max_active_plus: saved_reminders_max_active_plus(),
            max_days_ahead: saved_reminders_max_days_ahead(),
            keep_delivered_days: saved_reminders_keep_delivered_days(),
        }
    }
}


/// When a call counts as weak or very weak, and what the app sends then.
/// Every number is a setting; the app reads them from `GET /`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct FeaturesVoiceWeakNetwork {
    /// Packet loss (percent) from which a call is weak
    pub weak_loss_percent: u32,
    /// Packet loss (percent) from which a call is very weak
    pub bad_loss_percent: u32,
    /// Round trip time (ms) from which a call is weak
    pub weak_rtt_ms: u32,
    /// Round trip time (ms) from which a call is very weak
    pub bad_rtt_ms: u32,
    /// Microphone bitrate (bits per second) on a weak connection. Low-data
    /// mode's lighter voice uses the same number.
    pub weak_audio_bitrate: u32,
    /// Microphone bitrate on a very weak connection
    pub bad_audio_bitrate: u32,
    /// Seconds a call has to stay good before it steps back up one level
    pub recover_seconds: u32,
}

impl Default for FeaturesVoiceWeakNetwork {
    fn default() -> Self {
        Self {
            weak_loss_percent: 5,
            bad_loss_percent: 15,
            weak_rtt_ms: 400,
            bad_rtt_ms: 800,
            weak_audio_bitrate: 24_000,
            bad_audio_bitrate: 16_000,
            recover_seconds: 10,
        }
    }
}

/// Who agrees how before their voice is in a clip or an episode
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VoiceCaptureConsent {
    /// A voice is included only after its owner chose to be included
    #[default]
    OptIn,
    /// A voice is included until its owner chooses to be left out. Check
    /// the law where you run the server before using this.
    OptOut,
}

/// Limits of voice clips and recorded rooms. All of them are settings.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct FeaturesVoiceCapture {
    /// How people agree to be in a clip or an episode
    pub consent: VoiceCaptureConsent,
    /// Length of a clip in seconds
    pub clip_seconds: u32,
    /// Largest clip file in bytes
    pub clip_max_bytes: usize,
    /// Clips a person may share per calendar month
    pub clips_per_month: u32,
    /// Clips a person with Feather may share per calendar month
    pub clips_per_month_plus: u32,
    /// Minutes a server without Wings may record per calendar month (a
    /// trial; 0 turns recording off for such servers)
    pub trial_minutes: u32,
    /// Hours a server may record per calendar month, by Wings level (first
    /// entry: level 1). A level past the end uses the last entry.
    pub recording_hours_by_level: Vec<u32>,
    /// Longest single episode in minutes
    pub episode_max_minutes: u32,
    /// Days an episode is kept (0: until someone deletes it)
    pub episode_retention_days: u32,
}

impl Default for FeaturesVoiceCapture {
    fn default() -> Self {
        Self {
            consent: VoiceCaptureConsent::OptIn,
            clip_seconds: 30,
            clip_max_bytes: 600_000,
            clips_per_month: 10,
            clips_per_month_plus: 100,
            trial_minutes: 10,
            recording_hours_by_level: vec![5, 20, 60],
            episode_max_minutes: 60,
            episode_retention_days: 90,
        }
    }
}

impl FeaturesVoiceCapture {
    /// Seconds a server of this Wings level may record per month
    pub fn recording_seconds(&self, wings_level: usize) -> u64 {
        if wings_level == 0 {
            return u64::from(self.trial_minutes) * 60;
        }
        self.recording_hours_by_level
            .get(wings_level - 1)
            .or(self.recording_hours_by_level.last())
            .map(|hours| u64::from(*hours) * 3600)
            .unwrap_or_default()
    }

    /// Clips a person may share per month
    pub fn clips_allowed(&self, plus: bool) -> u32 {
        if plus {
            self.clips_per_month_plus.max(self.clips_per_month)
        } else {
            self.clips_per_month
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Sentry {
    pub api: String,
    pub events: String,
    pub voice_ingress: String,
    pub files: String,
    pub proxy: String,
    pub pushd: String,
    pub crond: String,
    pub gifbox: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Settings {
    pub database: Database,
    pub rabbit: Rabbit,
    pub hosts: Hosts,
    pub api: Api,
    pub pushd: Pushd,
    pub january: January,
    pub files: Files,
    pub features: Features,
    pub sentry: Sentry,
    pub production: bool,
    pub environment: String,
    pub disable_events_dont_use: bool,
}

/// The shipped (committed) default secrets, read from the embedded
/// `Revolt.toml`. Never printed or logged.
fn shipped_default(key: &str) -> Option<String> {
    Config::builder()
        .add_source(File::from_str(
            include_str!("../Revolt.toml"),
            FileFormat::Toml,
        ))
        .build()
        .ok()?
        .get_string(key)
        .ok()
}

/// A release build on a local API host may use the example values only with
/// this variable set to `local`. It is ignored with `production = true` or a
/// public API host, so a real deploy can't get past the check with it.
const ALLOW_EXAMPLE_SECRETS: &str = "ZRZUR_ALLOW_EXAMPLE_SECRETS";

/// LiveKit secrets from `Revolt.docker.toml.example`, `livekit.example.yml`
/// and the test config. `api.livekit.nodes` has no shipped default to compare.
const EXAMPLE_LIVEKIT_SECRETS: [&str; 2] = ["ZjCofRlfm6GGtjlifmNpCDkcQbEIIVC0", "secret"];

/// A database URL with no password in it
fn has_no_password(url: &str) -> bool {
    !url.contains('@')
}

impl Settings {
    /// Whether example values must be refused. Always with `production = true`
    /// or an API host that is not local, whatever the build. On a local host,
    /// in every release build unless a person set [`ALLOW_EXAMPLE_SECRETS`].
    /// So only debug builds (the dev stack, tests) pass without asking.
    fn refuses_examples(&self, release_build: bool, allowed_locally: bool) -> bool {
        self.production || !is_local_url(&self.hosts.api) || (release_build && !allowed_locally)
    }

    /// Settings still holding an example value, for this kind of run.
    /// Names only, never their values.
    pub fn example_secrets_for(&self, release_build: bool, allowed_locally: bool) -> Vec<&'static str> {
        if !self.refuses_examples(release_build, allowed_locally) {
            return Vec::new();
        }
        let mut bad = Vec::new();
        for (name, value) in [
            ("pushd.vapid.private_key", &self.pushd.vapid.private_key),
            ("files.encryption_key", &self.files.encryption_key),
            ("files.s3.access_key_id", &self.files.s3.access_key_id),
            ("files.s3.secret_access_key", &self.files.s3.secret_access_key),
            ("rabbit.password", &self.rabbit.password),
        ] {
            if shipped_default(name).as_deref() == Some(value.as_str()) {
                bad.push(name);
            }
        }
        if has_no_password(&self.database.mongodb) {
            bad.push("database.mongodb");
        }
        if has_no_password(&self.database.redis) {
            bad.push("database.redis");
        }
        if self
            .api
            .livekit
            .nodes
            .values()
            .any(|node| EXAMPLE_LIVEKIT_SECRETS.contains(&node.secret.as_str()))
        {
            bad.push("api.livekit.nodes.<node>.secret");
        }
        bad
    }

    /// The same for this very process
    pub fn default_secrets_in_use(&self) -> Vec<&'static str> {
        let allowed = std::env::var(ALLOW_EXAMPLE_SECRETS).is_ok_and(|value| value == "local");
        self.example_secrets_for(!cfg!(debug_assertions), allowed)
    }

    /// The refusal text: what is wrong and how to fix each setting safely
    pub fn default_secrets_message(bad: &[&str]) -> String {
        let mut msg = format!(
            concat!(
                "refusing to start: {} still hold(s) an example value shipped with the source code. ",
                "Anyone can read those values, so a release run must use its own. ",
                "Set your own in Revolt.overrides.toml or as environment variables ",
                "(REVOLT__SECTION__KEY, for example REVOLT__PUSHD__VAPID__PRIVATE_KEY)."
            ),
            bad.join(" and ")
        );
        if bad.contains(&"pushd.vapid.private_key") {
            msg.push_str(concat!(
                " VAPID: generate a pair with the commands in self-hosted/generate_config.sh: ",
                "`openssl ecparam -name prime256v1 -genkey -noout -out vapid_private.pem`, then ",
                r"private_key = `base64 -i vapid_private.pem | tr -d '\n' | tr -d '='` and ",
                r"public_key = `openssl ec -in vapid_private.pem -outform DER | tail --bytes 65 | base64 | tr '/+' '_-' | tr -d '\n' | tr -d '='`. ",
                "Set both together (the public key must match the private key); ",
                "browsers that subscribed with the old public key must subscribe again."
            ));
        }
        if bad.contains(&"files.encryption_key") {
            msg.push_str(concat!(
                " Files: generate a key with `openssl rand -base64 32`. ",
                "WARNING: stored files are encrypted with the current key, so changing the key ",
                "without re-encrypting them makes those files unreadable. ",
                "On a server that already holds files, do not rotate the key until you have a backup and a re-encryption plan."
            ));
        }
        if bad.contains(&"database.mongodb") || bad.contains(&"database.redis") {
            msg.push_str(" Databases: the connection URL must carry a password.");
        }
        msg
    }

    /// Stop the process when an example secret reaches a release run
    /// Settings a release run must set to a value of its own (not empty):
    /// the captcha that protects sign-up, and the key that keeps the link
    /// preview service (January) from serving the whole internet.
    pub fn missing_for_release(&self, release_build: bool, allowed_locally: bool) -> Vec<&'static str> {
        if !self.refuses_examples(release_build, allowed_locally) {
            return Vec::new();
        }
        let mut missing = Vec::new();
        if self.api.security.captcha.turnstile_secret.is_empty() {
            missing.push("api.security.captcha.turnstile_secret");
        }
        if self.api.security.captcha.turnstile_sitekey.is_empty() {
            missing.push("api.security.captcha.turnstile_sitekey");
        }
        if self.january.embed_key.is_empty() {
            missing.push("january.embed_key");
        }
        missing
    }

    pub fn refuse_default_secrets(&self) {
        let bad = self.default_secrets_in_use();
        if !bad.is_empty() {
            panic!("{}", Self::default_secrets_message(&bad));
        }

        let allowed = std::env::var(ALLOW_EXAMPLE_SECRETS).is_ok_and(|value| value == "local");
        let missing = self.missing_for_release(!cfg!(debug_assertions), allowed);
        if !missing.is_empty() {
            panic!(
                "refusing to start: {} must be set to your own value. \
                 Captcha: create a Cloudflare Turnstile widget and set its secret and site key \
                 (api.security.captcha.turnstile_secret and turnstile_sitekey). \
                 Embed key: `openssl rand -hex 32` for january.embed_key. \
                 Set them in Revolt.overrides.toml or as environment variables.",
                missing.join(" and ")
            );
        }
    }

    pub fn preflight_checks(&self) {
        self.refuse_default_secrets();

        if self.api.smtp.host.is_empty() {
            log::warn!("No SMTP settings specified! Remember to configure email.");
        }
    }
}

pub async fn init() {
    println!(
        ":: Revolt Configuration ::\n\x1b[32m{:?}\x1b[0m",
        config().await
    );
}

pub async fn read() -> Config {
    CONFIG_BUILDER.read().await.clone()
}

pub async fn config_no_cache() -> Settings {
    let mut config = read().await.try_deserialize::<Settings>().unwrap();

    // inject REDIS_URI for redis-kiss library
    if std::env::var("REDIS_URI").is_err() {
        unsafe {
            std::env::set_var("REDIS_URI", config.database.redis.clone());
        }
    }

    // auto-detect production nodes
    if is_production_api(&config.hosts.api)
        || (config.hosts.api.contains("https")
            && (config.hosts.api.contains("revolt.chat")
                || config.hosts.api.contains("stoat.chat")))
    {
        // Prefer explicit [production] in Revolt.toml for Zrzur.
        // Upstream host matches retained only so legacy forks don't break; do not
        // treat stoat.chat as a Zrzur production destination.
        config.production = true;
    }

    resolve_billing(&mut config);
    config.refuse_default_secrets();
    config
}

/// Work out the billing mode once the rest of the config is known
pub fn resolve_billing(config: &mut Settings) {
    config.features.billing.resolved_mode = config.features.billing.resolve_mode(
        &config.hosts,
        config.production,
        &config.features.legal_links,
    );
}

#[cached(time = 30)]
pub async fn config() -> Settings {
    #[cfg(feature = "test")]
    if let Some(overwrites) = CONFIG_OVERWRITES.get() {
        return overwrites.clone();
    }

    config_no_cache().await
}

#[cfg(feature = "test")]
static CONFIG_OVERWRITES: OnceLock<Settings> = OnceLock::new();

/// Modify the config values for a test, this can only be called once
///
/// This will also fail if two or more tests are running in the same process and both try to modify the config,
/// This could happen if tests where run under `cargo test` instead of `nextest`.
#[cfg(feature = "test")]
pub async fn overwrite_config(f: impl FnOnce(&mut Settings)) {
    let mut config = config_no_cache().await;

    f(&mut config);
    resolve_billing(&mut config);

    CONFIG_OVERWRITES.set(config).expect(
        "Cannot overwrite config multiple times, make sure you are running tests through nextest.",
    );
}

#[cfg(feature = "test")]
#[cfg(test)]
mod tests {
    use super::init;

    #[tokio::test]
    async fn it_works() {
        init().await;
    }
}

#[cfg(test)]
mod vanity_level_tests {
    use super::{vanity_level_of, SuperchargeLevel};

    fn level(vanity_invite: bool) -> SuperchargeLevel {
        SuperchargeLevel {
            supercharges: 1,
            server_emoji: 0,
            attachments: 0,
            voice_bitrate: 0,
            video_resolution: [0, 0],
            video_framerate: 30,
            vanity_invite,
        }
    }

    #[test]
    fn vanity_level_is_lowest_flagged() {
        assert_eq!(vanity_level_of(&[level(false), level(false), level(true)]), Some(3));
        assert_eq!(vanity_level_of(&[level(false), level(true), level(true)]), Some(2));
        assert_eq!(vanity_level_of(&[level(false), level(false)]), None);
        assert_eq!(vanity_level_of(&[]), None);
    }
}

#[cfg(test)]
mod env_secret_tests {
    use super::{env_secret_from, is_production_api};

    #[test]
    fn env_secret_reads_only_zrzur_names() {
        let env = |key: &str| match key {
            "ZRZUR_A" => Some(" current ".to_string()),
            "OLD_B" => Some("legacy".to_string()),
            "ZRZUR_EMPTY" => Some("  ".to_string()),
            _ => None,
        };
        assert_eq!(env_secret_from("ZRZUR_A", env).as_deref(), Some("current"));
        assert_eq!(env_secret_from("ZRZUR_B", env), None);
        assert_eq!(env_secret_from("ZRZUR_EMPTY", env), None);
        assert_eq!(env_secret_from("", env), None);
    }

    #[test]
    fn production_is_detected_only_for_the_zrzur_api_host() {
        assert!(is_production_api("https://api.zrzur.com"));
        assert!(is_production_api("https://api.zrzur.com/"));
        assert!(!is_production_api("https://api.staging.zrzur.com"));
        assert!(!is_production_api("https://api.zrzur.community"));
        assert!(!is_production_api("https://api.zrzur.com.example.test"));
        assert!(!is_production_api("http://api.zrzur.com"));
        assert!(!is_production_api("http://127.0.0.1:14702"));
    }
}

/// Community designs (avatar decorations, nameplates and effects by creators)
#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesCommunity {
    /// Designs one creator may have that are not archived or rejected
    #[serde(default = "community_max_items")]
    pub max_items_per_creator: usize,
    /// New designs one creator may make in 24 hours (0: no daily limit).
    /// Holders of the project's top ranks have neither limit
    /// (`UNLIMITED_RANKS` in delta's community routes)
    #[serde(default = "community_max_items_per_day")]
    pub max_items_per_day: usize,
    /// Largest width or height of an image in pixels
    #[serde(default = "community_max_dimension")]
    pub max_dimension: u32,
    /// Most frames in an animation
    #[serde(default = "community_max_frames")]
    pub max_frames: u32,
    /// Longest loop of an animation in milliseconds
    #[serde(default = "community_max_duration_ms")]
    pub max_duration_ms: u32,
    /// Creator effects are open: uploads, the gallery, wearing them and their
    /// files. Off is the switch that stops every creator effect at once; it
    /// takes a restart of delta and autumn. Zrzur's own effects are not touched.
    #[serde(default = "community_effects_enabled")]
    pub effects_enabled: bool,
    /// Creator profile frames are open: uploads, the gallery and wearing them.
    /// Off (the default until they are approved) refuses new community frames
    /// and wearing one; Zrzur's built-in frames are not touched. Takes a
    /// restart of delta.
    #[serde(default = "community_frames_enabled")]
    pub frames_enabled: bool,
    /// Largest width or height of an effect in pixels
    #[serde(default = "community_max_dimension")]
    pub effect_max_dimension: u32,
    /// Most frames in an effect
    #[serde(default = "community_effect_max_frames")]
    pub effect_max_frames: u32,
    /// Longest play of an effect in milliseconds
    #[serde(default = "community_effect_max_duration_ms")]
    pub effect_max_duration_ms: u32,
    /// Shortest time one frame of an effect may show, in milliseconds
    #[serde(default = "community_effect_min_frame_ms")]
    pub effect_min_frame_ms: u32,
    /// Largest WebM taken in for a design; the image made from it has the
    /// tag's own limit
    #[serde(default = "community_webm_max_bytes")]
    pub webm_max_bytes: usize,
    /// ffmpeg is killed after this long
    #[serde(default = "community_webm_timeout_ms")]
    pub webm_timeout_ms: u64,
}

fn community_max_items() -> usize {
    50
}
fn community_max_items_per_day() -> usize {
    20
}
fn community_max_dimension() -> u32 {
    1024
}
fn community_max_frames() -> u32 {
    240
}
fn community_max_duration_ms() -> u32 {
    12_000
}
fn community_webm_max_bytes() -> usize {
    8_000_000
}
fn community_webm_timeout_ms() -> u64 {
    20_000
}
fn community_effects_enabled() -> bool {
    true
}
fn community_frames_enabled() -> bool {
    false
}
fn community_effect_max_frames() -> u32 {
    120
}
fn community_effect_max_duration_ms() -> u32 {
    4_000
}
fn community_effect_min_frame_ms() -> u32 {
    33
}

impl Default for FeaturesCommunity {
    fn default() -> Self {
        Self {
            max_items_per_creator: community_max_items(),
            max_items_per_day: community_max_items_per_day(),
            max_dimension: community_max_dimension(),
            max_frames: community_max_frames(),
            max_duration_ms: community_max_duration_ms(),
            effects_enabled: community_effects_enabled(),
            frames_enabled: community_frames_enabled(),
            effect_max_dimension: community_max_dimension(),
            effect_max_frames: community_effect_max_frames(),
            effect_max_duration_ms: community_effect_max_duration_ms(),
            effect_min_frame_ms: community_effect_min_frame_ms(),
            webm_max_bytes: community_webm_max_bytes(),
            webm_timeout_ms: community_webm_timeout_ms(),
        }
    }
}

#[cfg(test)]
mod billing_mode_tests {
    use super::*;

    fn hosts(api: &str, app: &str) -> Hosts {
        Hosts {
            app: app.to_string(),
            api: api.to_string(),
            events: String::new(),
            autumn: String::new(),
            january: String::new(),
            january_public: String::new(),
            livekit: HashMap::new(),
            assets: String::new(),
        }
    }

    fn legal(filled: bool) -> LegalLinks {
        let value = if filled { "https://zrzur.com/legal" } else { "" };
        LegalLinks {
            terms_of_service: value.to_string(),
            privacy_policy: value.to_string(),
            guidelines: String::new(),
        }
    }

    /// Billing settings whose secrets live in variables only this test uses
    fn stripe(test: &str, environment: &str, key: Option<&str>, webhook: bool) -> FeaturesBilling {
        let key_env = format!("ZBILL_TEST_KEY_{test}");
        let hook_env = format!("ZBILL_TEST_HOOK_{test}");
        unsafe {
            match key {
                Some(key) => std::env::set_var(&key_env, key),
                None => std::env::remove_var(&key_env),
            }
            if webhook {
                std::env::set_var(&hook_env, "whsec_test_value");
            } else {
                std::env::remove_var(&hook_env);
            }
        }
        FeaturesBilling {
            enabled: true,
            provider: BillingProvider {
                name: "stripe".to_string(),
                environment: environment.to_string(),
                secret_key_env: key_env,
                webhook_secret_env: hook_env,
                webhook_secret_previous_env: String::new(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    const LOCAL: (&str, &str) = ("http://127.0.0.1:14702", "http://127.0.0.1:14701");
    const PROD: (&str, &str) = ("https://api.zrzur.com", "https://app.zrzur.com");

    #[test]
    fn off_unless_enabled() {
        let billing = FeaturesBilling {
            test_mode: true,
            ..Default::default()
        };
        let h = hosts(LOCAL.0, LOCAL.1);
        assert_eq!(billing.resolve_mode(&h, false, &legal(true)), BillingMode::Disabled);
    }

    #[test]
    fn internal_test_only_on_local_hosts() {
        let billing = FeaturesBilling {
            enabled: true,
            test_mode: true,
            ..Default::default()
        };
        assert_eq!(
            billing.resolve_mode(&hosts(LOCAL.0, LOCAL.1), false, &legal(false)),
            BillingMode::InternalTest
        );
        // u5-01: production hosts or the production flag never get free perks
        assert_eq!(
            billing.resolve_mode(&hosts(PROD.0, PROD.1), false, &legal(true)),
            BillingMode::Disabled
        );
        assert_eq!(
            billing.resolve_mode(&hosts(LOCAL.0, LOCAL.1), true, &legal(true)),
            BillingMode::Disabled
        );
        assert_eq!(
            billing.resolve_mode(&hosts("http://127.0.0.1.evil.example", LOCAL.1), false, &legal(true)),
            BillingMode::Disabled
        );
        // enabled alone (no test_mode, no provider) is off
        let enabled_only = FeaturesBilling {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            enabled_only.resolve_mode(&hosts(LOCAL.0, LOCAL.1), false, &legal(true)),
            BillingMode::Disabled
        );
    }

    #[test]
    fn sandbox_needs_test_key_and_webhook_secret() {
        let h = hosts(LOCAL.0, LOCAL.1);
        let ok = stripe("sb1", "sandbox", Some("sk_test_abc"), true);
        assert_eq!(ok.resolve_mode(&h, false, &legal(false)), BillingMode::Sandbox);
        let no_hook = stripe("sb2", "sandbox", Some("sk_test_abc"), false);
        assert_eq!(no_hook.resolve_mode(&h, false, &legal(false)), BillingMode::Disabled);
        let no_key = stripe("sb3", "sandbox", None, true);
        assert_eq!(no_key.resolve_mode(&h, false, &legal(false)), BillingMode::Disabled);
        // live key in sandbox environment: refused
        let live_key = stripe("sb4", "sandbox", Some("sk_live_abc"), true);
        assert_eq!(live_key.resolve_mode(&h, false, &legal(false)), BillingMode::Disabled);
        // test_mode together with a provider is ambiguous: refused
        let mut both = stripe("sb5", "sandbox", Some("sk_test_abc"), true);
        both.test_mode = true;
        assert_eq!(both.resolve_mode(&h, false, &legal(false)), BillingMode::Disabled);
    }

    #[test]
    fn sandbox_in_production_needs_testers() {
        let h = hosts(PROD.0, PROD.1);
        let mut billing = stripe("sp1", "sandbox", Some("sk_test_abc"), true);
        assert_eq!(billing.resolve_mode(&h, true, &legal(true)), BillingMode::Disabled);
        billing.provider.sandbox_testers = vec!["01TESTER".to_string()];
        assert_eq!(billing.resolve_mode(&h, true, &legal(true)), BillingMode::Sandbox);
    }

    #[test]
    fn live_stays_locked_without_every_gate() {
        let h = hosts(PROD.0, PROD.1);
        let mut billing = stripe("lv1", "live", Some("sk_live_abc"), true);
        billing.provider.checkout_enabled = true;
        // test key with live environment: refused outright
        let wrong = stripe("lv2", "live", Some("sk_test_abc"), true);
        assert_eq!(wrong.resolve_mode(&h, true, &legal(true)), BillingMode::Disabled);

        unsafe {
            std::env::set_var(
                LIVE_ACTIVATION_ENV,
                format!("{LIVE_ACTIVATION_PREFIX}{}", PROD.0),
            );
        }
        let mode = billing.resolve_mode(&h, true, &legal(true));
        if cfg!(feature = "billing-live") {
            assert_eq!(mode, BillingMode::LiveEnabled);
        } else {
            // the default build can never take real payments
            assert_eq!(mode, BillingMode::LiveReady);
        }
        // missing legal links keep it locked
        assert_eq!(billing.resolve_mode(&h, true, &legal(false)), BillingMode::LiveReady);
        // activation copied from another deployment does nothing
        let other = hosts("https://api.other.example", PROD.1);
        assert_eq!(billing.resolve_mode(&other, true, &legal(true)), BillingMode::LiveReady);
        unsafe {
            std::env::remove_var(LIVE_ACTIVATION_ENV);
        }
        assert_eq!(billing.resolve_mode(&h, true, &legal(true)), BillingMode::LiveReady);
    }

    #[test]
    fn perks_only_count_in_their_own_world() {
        assert!(BillingMode::Sandbox.counts(true));
        assert!(!BillingMode::Sandbox.counts(false));
        assert!(BillingMode::LiveEnabled.counts(false));
        assert!(!BillingMode::LiveEnabled.counts(true));
        assert!(!BillingMode::LiveReady.counts(true));
        assert!(!BillingMode::Disabled.counts(true));
        assert!(!BillingMode::Disabled.counts(false));
        assert!(!BillingMode::LiveReady.checkout_open());
    }

    #[test]
    fn local_url_detection() {
        assert!(is_local_url("http://localhost:3000"));
        assert!(is_local_url("http://[::1]:80/x"));
        assert!(is_local_url("http://zrzur.test"));
        assert!(!is_local_url("https://app.zrzur.com"));
        assert!(!is_local_url("http://localhost.evil.com"));
        assert!(!is_local_url("localhost"));
    }

    /// A PayPal entry whose secrets live in variables only this test uses
    fn paypal(test: &str, environment: &str, id: bool, secret: bool, webhook: bool) -> BillingProvider {
        let set = |name: String, on: bool, value: &str| {
            unsafe {
                if on {
                    std::env::set_var(&name, value);
                } else {
                    std::env::remove_var(&name);
                }
            }
            name
        };
        BillingProvider {
            name: "paypal".to_string(),
            environment: environment.to_string(),
            client_id_env: set(format!("ZBILL_PP_ID_{test}"), id, "client-id"),
            secret_key_env: set(format!("ZBILL_PP_SECRET_{test}"), secret, "client-secret"),
            webhook_secret_env: set(format!("ZBILL_PP_HOOK_{test}"), webhook, "WH-123"),
            webhook_secret_previous_env: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn second_provider_joins_when_ready() {
        let h = hosts(LOCAL.0, LOCAL.1);
        let mut both = stripe("mp1", "sandbox", Some("sk_test_abc"), true);
        both.providers = vec![paypal("mp1", "sandbox", true, true, true)];
        assert_eq!(both.resolve_mode(&h, false, &legal(false)), BillingMode::Sandbox);
        assert_eq!(both.ready_provider_names(), vec!["stripe", "paypal"]);
    }

    #[test]
    fn half_configured_provider_is_skipped_not_fatal() {
        let h = hosts(LOCAL.0, LOCAL.1);
        for (n, (id, secret, hook)) in [(false, true, true), (true, false, true), (true, true, false)]
            .into_iter()
            .enumerate()
        {
            let mut billing = stripe(&format!("mp2{n}"), "sandbox", Some("sk_test_abc"), true);
            billing.providers = vec![paypal(&format!("mp2{n}"), "sandbox", id, secret, hook)];
            assert_eq!(billing.resolve_mode(&h, false, &legal(false)), BillingMode::Sandbox);
            assert_eq!(billing.ready_provider_names(), vec!["stripe"]);
        }
    }

    #[test]
    fn paypal_alone_is_enough() {
        let h = hosts(LOCAL.0, LOCAL.1);
        let billing = FeaturesBilling {
            enabled: true,
            provider: paypal("mp3", "sandbox", true, true, true),
            ..Default::default()
        };
        assert_eq!(billing.resolve_mode(&h, false, &legal(false)), BillingMode::Sandbox);
        assert_eq!(billing.ready_provider_names(), vec!["paypal"]);
    }

    #[test]
    fn providers_must_share_one_environment() {
        let h = hosts(LOCAL.0, LOCAL.1);
        // the first ready provider sets the environment; a live PayPal next
        // to a sandbox Stripe is left out, never mixed in
        let mut billing = stripe("mp4", "sandbox", Some("sk_test_abc"), true);
        billing.providers = vec![paypal("mp4", "live", true, true, true)];
        assert_eq!(billing.resolve_mode(&h, false, &legal(false)), BillingMode::Sandbox);
        assert_eq!(billing.ready_provider_names(), vec!["stripe"]);
    }

    #[test]
    fn nothing_ready_is_off_and_unknown_names_are_ignored() {
        let h = hosts(LOCAL.0, LOCAL.1);
        let mut billing = stripe("mp5", "sandbox", None, true);
        billing.providers = vec![
            paypal("mp5", "sandbox", false, false, false),
            BillingProvider {
                name: "mystery".to_string(),
                ..Default::default()
            },
        ];
        assert_eq!(billing.resolve_mode(&h, false, &legal(false)), BillingMode::Disabled);
        assert!(billing.ready_provider_names().is_empty());
    }

    #[test]
    fn paypal_test_address_only_in_sandbox_on_loopback() {
        let mut sandbox = paypal("mp6", "sandbox", true, true, true);
        sandbox.api_base = "http://127.0.0.1:9999".to_string();
        assert!(provider_ready(&sandbox).is_some());
        let mut live = paypal("mp7", "live", true, true, true);
        live.api_base = "http://127.0.0.1:9999".to_string();
        assert!(provider_ready(&live).is_none());
        let mut remote = paypal("mp8", "sandbox", true, true, true);
        remote.api_base = "http://evil.example".to_string();
        assert!(provider_ready(&remote).is_none());
    }

    #[test]
    fn any_test_address_blocks_live() {
        let mut billing = FeaturesBilling {
            enabled: true,
            provider: paypal("mp9", "live", true, true, true),
            ..Default::default()
        };
        assert!(billing
            .live_gate_report(&hosts(PROD.0, PROD.1), &legal(true))
            .contains(&("no_test_api_base", true)));
        let mut extra = paypal("mp9b", "live", true, true, true);
        extra.api_base = "http://127.0.0.1:1".to_string();
        billing.providers = vec![extra];
        assert!(billing
            .live_gate_report(&hosts(PROD.0, PROD.1), &legal(true))
            .contains(&("no_test_api_base", false)));
    }
}

/// Discover review desk: one bot may show open Discover requests in one
/// staff channel, and hand back the clicks of people holding one role there.
/// The bot has no power of its own: every decision is checked against the
/// person who clicked (`/discover/review/*`). Any value left empty switches
/// the routes off.
#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct FeaturesDiscoverReview {
    /// Id of the server the reviewers work in
    #[serde(default)]
    pub server: String,
    /// Id of the staff channel in that server the notices are posted to
    #[serde(default)]
    pub channel: String,
    /// Id of a second staff channel for requests of bots. Empty: bots share
    /// `channel` with servers.
    #[serde(default)]
    pub channel_bots: String,
    /// Id of the role in that server whose holders may decide
    #[serde(default)]
    pub role: String,
    /// Id of the role whose holders may take a listed server or bot off
    /// Discover from the desk. Empty: nobody can, the admin console stays
    /// the only way.
    #[serde(default)]
    pub remove_role: String,
    /// Id of the one bot allowed to use the review routes
    #[serde(default)]
    pub bot: String,
}

impl FeaturesDiscoverReview {
    /// The staff channel requests of this kind are decided in
    pub fn channel_for(&self, bot_request: bool) -> &str {
        if bot_request && !self.channel_bots.trim().is_empty() {
            &self.channel_bots
        } else {
            &self.channel
        }
    }

    pub fn enabled(&self) -> bool {
        ![&self.server, &self.channel, &self.role, &self.bot]
            .iter()
            .any(|value| value.trim().is_empty())
    }
}

/// "Join the Zrzur Team": every team role is a volunteer role (owner,
/// 2026-11-19: never paid, never employment), and the texts say so.
/// People apply for a team role through one bot in
/// one server, and holders of one role there review the applications
/// (`/team/desk/*`). The bot has no power of its own: who applied, who
/// pressed and what they may do is checked by the server, and the server,
/// not the bot, hands out the role. `server`, `channel_apply`,
/// `channel_review`, `reviewer_role` or `bot` left empty switches the
/// routes off.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FeaturesTeamApplications {
    /// Id of the server people apply in and reviewers work in
    #[serde(default)]
    pub server: String,
    /// Id of the channel with the Apply message
    #[serde(default)]
    pub channel_apply: String,
    /// Id of the private staff channel the applications are shown in
    #[serde(default)]
    pub channel_review: String,
    /// Id of the role in `server` whose holders may decide
    #[serde(default)]
    pub reviewer_role: String,
    /// Id of the one bot allowed to use the routes
    #[serde(default)]
    pub bot: String,
    /// Days a person waits after a decision before applying again
    #[serde(default = "default_team_cooldown_days")]
    pub cooldown_days: i64,
    /// Minutes a person waits after withdrawing before applying again
    #[serde(default = "default_team_withdraw_wait_minutes")]
    pub withdraw_wait_minutes: i64,
    /// Days a rejected application is kept (never less than the cooldown)
    #[serde(default = "default_team_retention_days")]
    pub retention_days: i64,
    /// Days an accepted application is kept
    #[serde(default = "default_team_accepted_retention_days")]
    pub accepted_retention_days: i64,
    /// Where the form sends people to read how their data is used
    #[serde(default = "default_team_privacy_url")]
    pub privacy_url: String,
    /// The team roles people can apply for, in the order shown
    #[serde(default)]
    pub roles: Vec<TeamRole>,
    /// Looking after the volunteers once they are accepted
    #[serde(default)]
    pub volunteers: FeaturesTeamVolunteers,
}

/// Volunteer care after acceptance: a first month of getting to know each
/// other, a gentle monthly look at who has had time, and a weekly report.
/// Counts only; no message content and no channel names are stored.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FeaturesTeamVolunteers {
    /// Off by default: nothing here runs until the owner switches it on
    #[serde(default)]
    pub enabled: bool,
    /// Days of the first period after acceptance
    #[serde(default = "default_volunteers_trial_days")]
    pub trial_days: i64,
    /// Helpful actions a month below which a volunteer gets a kind check-in
    #[serde(default = "default_volunteers_monthly_minimum")]
    pub monthly_minimum: i64,
    /// Longest "away" a volunteer may set, in days
    #[serde(default = "default_volunteers_away_max_days")]
    pub away_max_days: i64,
    /// Days name, age and country, and an ended membership with its
    /// history and notes, are kept after a volunteer leaves
    #[serde(default = "default_volunteers_identity_keep_days")]
    pub identity_keep_days: i64,
    /// Days the daily counts are kept (at least 62: the monthly rule reads
    /// the whole month before)
    #[serde(default = "default_volunteers_activity_keep_days")]
    pub activity_keep_days: i64,
    /// Version of the volunteer rules people confirm
    #[serde(default = "default_volunteers_rules_version")]
    pub rules_version: String,
    /// Id of the server the "Zrzur Team" overview lives in
    #[serde(default)]
    pub team_server: String,
    /// Id of the private channel the weekly report is posted in
    #[serde(default)]
    pub report_channel: String,
}

fn default_volunteers_trial_days() -> i64 {
    30
}
fn default_volunteers_monthly_minimum() -> i64 {
    2
}
fn default_volunteers_away_max_days() -> i64 {
    90
}
fn default_volunteers_identity_keep_days() -> i64 {
    90
}
fn default_volunteers_activity_keep_days() -> i64 {
    190
}
fn default_volunteers_rules_version() -> String {
    "1".to_string()
}

impl Default for FeaturesTeamVolunteers {
    fn default() -> Self {
        FeaturesTeamVolunteers {
            enabled: false,
            trial_days: default_volunteers_trial_days(),
            monthly_minimum: default_volunteers_monthly_minimum(),
            away_max_days: default_volunteers_away_max_days(),
            identity_keep_days: default_volunteers_identity_keep_days(),
            activity_keep_days: default_volunteers_activity_keep_days(),
            rules_version: default_volunteers_rules_version(),
            team_server: String::new(),
            report_channel: String::new(),
        }
    }
}

fn default_team_cooldown_days() -> i64 {
    30
}
fn default_team_withdraw_wait_minutes() -> i64 {
    60
}
fn default_team_retention_days() -> i64 {
    30
}
fn default_team_accepted_retention_days() -> i64 {
    90
}
fn default_team_privacy_url() -> String {
    "https://zrzur.com/legal/privacy".to_string()
}

impl Default for FeaturesTeamApplications {
    fn default() -> Self {
        FeaturesTeamApplications {
            server: String::new(),
            channel_apply: String::new(),
            channel_review: String::new(),
            reviewer_role: String::new(),
            bot: String::new(),
            cooldown_days: default_team_cooldown_days(),
            withdraw_wait_minutes: default_team_withdraw_wait_minutes(),
            retention_days: default_team_retention_days(),
            accepted_retention_days: default_team_accepted_retention_days(),
            privacy_url: default_team_privacy_url(),
            roles: Vec::new(),
            volunteers: FeaturesTeamVolunteers::default(),
        }
    }
}

/// One team role people can apply for
#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamRole {
    /// Short name used in button ids: lowercase letters, digits and `-`
    pub key: String,
    /// English name shown to people. Empty: the name of the role's kind,
    /// which exists in every language of the app.
    #[serde(default)]
    pub name: String,
    /// One English line on what the role does. Empty: that of its kind.
    #[serde(default)]
    pub description: String,
    /// Id of the server the role belongs to
    #[serde(default)]
    pub server: String,
    /// Id of the role in that server an accepted person receives
    #[serde(default)]
    pub role: String,
    /// Id of a second role in that server every accepted person receives
    /// with it: the server's "Zrzur Team" role. Empty: none.
    #[serde(default)]
    pub team_role: String,
    /// Id of the staff channel this role's applications are shown and
    /// decided in (it may be in the role's own server). Empty: the common
    /// `channel_review`.
    #[serde(default)]
    pub channel_review: String,
    /// Id of a role in `server` whose holders may decide this role's
    /// applications, besides the holders of the common `reviewer_role`.
    /// Empty: only those.
    #[serde(default)]
    pub reviewer_role: String,
    /// Only accounts whose stated age is 18 or over may apply. Off by
    /// default (owner, 2026-11-19): anyone with an account, so 16 or over,
    /// may volunteer for any role.
    #[serde(default)]
    pub adult: bool,
    /// Never handed out by the system: accepting only records it and tells
    /// the owner of `server`, who gives the role by hand. Always the case
    /// for the key `administrator` and for any role that can manage the
    /// server, its roles or its permissions.
    #[serde(default)]
    pub manual: bool,
    /// Whether applications are taken right now
    #[serde(default = "default_true")]
    pub open: bool,
    /// English text sent to an accepted person: what the role may and may
    /// not do. Empty: the rules of its kind.
    #[serde(default)]
    pub rules: String,
    /// Up to two questions of the form, besides languages and time. Empty:
    /// "experience" and "why".
    #[serde(default)]
    pub questions: Vec<TeamQuestion>,
    /// Ids of the channels in `server` where this role's volunteers do
    /// their work; only counts of actions there are kept. Empty: none.
    #[serde(default)]
    pub work_channels: Vec<String>,
}

/// One free-text question of the form
#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamQuestion {
    /// Short name: lowercase letters, digits and `-`
    pub id: String,
    /// The question, at most 45 characters
    pub label: String,
    /// A line under the question, at most 100 characters
    #[serde(default)]
    pub hint: String,
}

impl TeamRole {
    /// A role with this key is never handed out by the system
    pub const ALWAYS_MANUAL: &'static str = "administrator";

    /// The kinds of role whose name, description and rules exist in every
    /// language of the app. A role is of a kind when its key is the kind or
    /// starts with it and `-` (`moderator-lounge`).
    pub const KINDS: &'static [&'static str] = &[
        "support",
        "moderator",
        "discover-reviewer",
        "community-manager",
        "designer",
        "developer",
        "administrator",
    ];

    pub fn has_kind(&self) -> bool {
        Self::KINDS
            .iter()
            .any(|kind| self.key == *kind || self.key.starts_with(&format!("{kind}-")))
    }

    pub fn is_manual(&self) -> bool {
        self.manual || self.key == Self::ALWAYS_MANUAL
    }

    /// Whether the role is set up well enough to be offered
    pub fn ready(&self) -> bool {
        let word = |s: &str| {
            !s.is_empty()
                && s.len() <= 32
                && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        };
        word(&self.key)
            // `me` is the button of "Add my details", not a role
            && self.key != "me"
            && (self.has_kind() || !self.name.trim().is_empty())
            && !self.server.trim().is_empty()
            && (self.is_manual() || !self.role.trim().is_empty())
            && self.questions.len() <= 2
            && self.questions.iter().all(|q| word(&q.id) && !q.label.trim().is_empty())
    }

    /// The free-text questions of this role's form
    pub fn form_questions(&self) -> Vec<TeamQuestion> {
        if !self.questions.is_empty() {
            return self.questions.clone();
        }
        vec![
            TeamQuestion {
                id: "experience".to_string(),
                label: "Your experience".to_string(),
                hint: "What have you done that fits this role? Links are welcome.".to_string(),
            },
            TeamQuestion {
                id: "why".to_string(),
                label: "Why do you want this role?".to_string(),
                hint: "A few honest lines are enough.".to_string(),
            },
        ]
    }
}

impl FeaturesTeamApplications {
    pub fn enabled(&self) -> bool {
        ![
            &self.server,
            &self.channel_apply,
            &self.channel_review,
            &self.reviewer_role,
            &self.bot,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    }

    /// The staff channel the applications for `key` are decided in
    pub fn review_channel(&self, key: &str) -> &str {
        match self.role(key) {
            Some(role) if !role.channel_review.trim().is_empty() => &role.channel_review,
            _ => &self.channel_review,
        }
    }

    /// Every staff channel in use, the common one first, without repeats
    pub fn review_channels(&self) -> Vec<String> {
        let mut channels = vec![self.channel_review.clone()];
        for role in &self.roles {
            let channel = role.channel_review.trim();
            if !channel.is_empty() && !channels.iter().any(|c| c == channel) {
                channels.push(channel.to_string());
            }
        }
        channels
    }

    pub fn role(&self, key: &str) -> Option<&TeamRole> {
        self.roles.iter().find(|role| role.key == key)
    }

    /// Every configured team role that belongs to `server`, in the order shown
    pub fn member_roles(&self, server: &str) -> Vec<&TeamRole> {
        self.roles.iter().filter(|role| role.server == server).collect()
    }

    /// Days a rejected application is kept: the cooldown needs its date
    pub fn rejected_days(&self) -> i64 {
        self.retention_days.max(self.cooldown_days).max(0)
    }
}

/// Age safety: the declared month and year of birth
#[derive(Deserialize, Debug, Clone)]
pub struct FeaturesAgeSafety {
    /// Sign-up needs a month and year of birth (at least the minimum age).
    /// Off by default: the app asks at the first entry instead.
    #[serde(default)]
    pub require_birth_month: bool,
    /// Every request of a person waits until the account has a declared
    /// month and year of birth (PATCH /auth/account/age is always open)
    #[serde(default = "default_true")]
    pub require_declared_age: bool,
    /// Who may open channels and Servers marked as mature
    #[serde(default)]
    pub mature_content: MatureContentAccess,
}

impl Default for FeaturesAgeSafety {
    fn default() -> Self {
        Self {
            require_birth_month: false,
            require_declared_age: true,
            mature_content: MatureContentAccess::default(),
        }
    }
}

/// Access to content marked as mature
#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatureContentAccess {
    /// Accounts whose declared age is 18 or over. Pornography stays forbidden
    /// by the rules, because a declared age is not a verified one.
    SelfDeclaredAdults,
    /// Nobody: mature content needs an age check Zrzur does not offer yet
    /// (the default)
    #[default]
    Blocked,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod release_integrity_tests {
    use super::*;

    fn shipped() -> Settings {
        Config::builder()
            .add_source(File::from_str(
                include_str!("../Revolt.toml"),
                FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize::<Settings>()
            .unwrap()
    }

    /// The privacy policy keeps server audit log entries for 90 days (PR-15)
    #[test]
    fn shipped_audit_log_lasts_ninety_days() {
        let s = shipped();
        assert_eq!(s.api.audit_logs.expires_after, 90 * 24 * 60 * 60);
    }

    /// New features ship off, and the address is Discord's documented one
    #[test]
    fn shipped_server_import_is_off() {
        let s = shipped();
        assert!(!s.features.server_import.enabled);
        assert!(!s.features.server_import.link_fetch);
        assert_eq!(
            s.features.server_import.template_endpoint,
            "https://discord.com/api/v10/guilds/templates/"
        );
        assert_eq!(s.features.server_import.reads_per_hour, 5);
    }

    #[test]
    fn shipped_defaults_limit_sessions() {
        let s = shipped();
        assert_eq!(s.api.sessions.limit, 10);
        assert_eq!(s.api.sessions.idle_days, 30);
    }

    #[test]
    fn shipped_defaults_are_private_by_default() {
        let s = shipped();
        assert!(s.api.security.shield.host.is_empty());
        assert!(s.api.security.shield.key.is_empty());
        assert!(s.api.security.easypwned.is_empty());
        for dsn in [
            &s.sentry.api,
            &s.sentry.events,
            &s.sentry.voice_ingress,
            &s.sentry.files,
            &s.sentry.proxy,
            &s.sentry.pushd,
            &s.sentry.crond,
            &s.sentry.gifbox,
        ] {
            assert!(dsn.is_empty());
        }
        assert_eq!(
            s.features.age_safety.mature_content,
            MatureContentAccess::Blocked
        );
        assert_eq!(MatureContentAccess::default(), MatureContentAccess::Blocked);
    }

    #[test]
    fn production_refuses_the_shipped_secrets() {
        let mut s = shipped();
        assert!(s.default_secrets_in_use().is_empty(), "not in production");
        s.production = true;
        assert_eq!(
            s.default_secrets_in_use(),
            vec![
                "pushd.vapid.private_key",
                "files.encryption_key",
                "files.s3.access_key_id",
                "files.s3.secret_access_key",
                "rabbit.password",
                "database.mongodb",
                "database.redis",
            ]
        );
        let text = Settings::default_secrets_message(&s.default_secrets_in_use());
        assert!(text.contains("generate_config.sh"), "VAPID advice");
        assert!(text.contains("prime256v1"));
        assert!(text.contains("unreadable"), "files rotation warning");
        assert!(text.contains("REVOLT__"));
        assert!(!text.contains("  "), "no stray spaces");
        assert!(!text.contains(&s.pushd.vapid.private_key));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.clone().refuse_default_secrets()));
        let msg = panic
            .unwrap_err()
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(msg.contains("refusing to start"));
        for value in [
            &s.files.encryption_key,
            &s.files.s3.secret_access_key,
            &s.rabbit.password,
            &s.database.mongodb,
        ] {
            assert!(!msg.contains(value.as_str()), "never print secrets");
        }
    }

    #[test]
    fn a_release_build_refuses_examples_even_on_a_local_host() {
        let s = shipped();
        assert!(s.example_secrets_for(false, false).is_empty(), "the dev stack and tests");
        assert_eq!(s.example_secrets_for(true, false).len(), 7);
        assert!(s.example_secrets_for(true, true).is_empty(), "a person asked for it");
    }

    #[test]
    fn the_local_exception_never_reaches_a_real_deploy() {
        let mut s = shipped();
        s.hosts.api = "https://api.example.org".into();
        assert_eq!(s.example_secrets_for(false, true).len(), 7);
        assert_eq!(s.example_secrets_for(true, true).len(), 7);
        let mut s = shipped();
        s.production = true;
        assert_eq!(s.example_secrets_for(true, true).len(), 7);
    }

    #[test]
    fn example_livekit_secrets_are_refused() {
        let mut s = own_secrets();
        s.api.livekit.nodes.insert(
            "worldwide".to_string(),
            super::LiveKitNode {
                url: "wss://voice.example.org".into(),
                lat: 0.0,
                lon: 0.0,
                key: "worldwide".into(),
                secret: super::EXAMPLE_LIVEKIT_SECRETS[0].into(),
                private: false,
            },
        );
        assert_eq!(s.example_secrets_for(true, true), vec!["api.livekit.nodes.<node>.secret"]);
    }

    /// A deploy where every secret was filled in
    fn own_secrets() -> Settings {
        let mut s = shipped();
        s.production = true;
        s.pushd.vapid.private_key = "own-vapid".into();
        s.files.encryption_key = "own-files".into();
        s.files.s3.access_key_id = "own-id".into();
        s.files.s3.secret_access_key = "own-key".into();
        s.rabbit.password = "own-rabbit".into();
        s.database.mongodb = "mongodb://zrzur:own@database:27017/".into();
        s.database.redis = "redis://:own@redis:6379/".into();
        s.api.security.captcha.turnstile_secret = "own-turnstile".into();
        s.api.security.captcha.turnstile_sitekey = "own-turnstile-site".into();
        s.january.embed_key = "own-embed".into();
        s
    }

    #[test]
    fn a_public_api_host_is_treated_as_production() {
        let mut s = shipped();
        s.hosts.api = "https://api.example.org".into();
        assert_eq!(s.default_secrets_in_use().len(), 7);
        s.hosts.api = "http://localhost:14702".into();
        assert!(s.default_secrets_in_use().is_empty());
    }

    #[test]
    fn production_accepts_own_secrets() {
        let mut s = own_secrets();
        assert!(s.default_secrets_in_use().is_empty());
        s.refuse_default_secrets();
        s.files.encryption_key = shipped().files.encryption_key;
        assert_eq!(s.default_secrets_in_use(), vec!["files.encryption_key"]);
    }
}

#[cfg(test)]
mod team_volunteers_tests {
    use config::{Config, File, FileFormat};

    use super::{FeaturesTeamApplications, FeaturesTeamVolunteers, TeamRole};

    /// The block prepared for the live server on 2026-11-19, as it stands
    /// (no `volunteers` table, no `work_channels`)
    const OLD_BLOCK: &str = r#"
[features.team_applications]
server = "01M7AD5WEYHG4GVN2BP4PBQRN6"
channel_apply = "01M7T4WAQZ0RHCWQEV5XRF1E02"
channel_review = "01M7T4VQXS5VNXS4K8W2GJ4M21"
reviewer_role = "01M7T36TKR42AB8KAP32ZSR6HA"
bot = "01M7T4PQEXD73ADF561K99QAEZ"
cooldown_days = 30
withdraw_wait_minutes = 60
retention_days = 30
accepted_retention_days = 90
privacy_url = "https://zrzur.com/legal/privacy"

[[features.team_applications.roles]]
key = "support"
server = "01M7AD5WEYHG4GVN2BP4PBQRN6"
role = "01M7T35Q0BG7RSXHJ8CKW0K14J"
team_role = "01M7AF4JRCWC7YJWYDRJN4M2Q1"

[[features.team_applications.roles]]
key = "moderator-lounge"
server = "01M7AD5WEYHG4GVN2BP4PBQRN6"
role = "01M7AD5WEYBHQDF7KG5HY8TKCX"
team_role = "01M7AF4JRCWC7YJWYDRJN4M2Q1"

[[features.team_applications.roles]]
key = "community-manager"
server = "01M7AD5WEYHG4GVN2BP4PBQRN6"
role = "01M7T366Z9W8C4QATWPWZZ8ZQG"
team_role = "01M7AF4JRCWC7YJWYDRJN4M2Q1"

[[features.team_applications.roles]]
key = "discover-reviewer"
server = "01M7R6QRNYYA4K72SB43GX5GAA"
role = "01M7R8RK30YE83QB31KXWC4RWN"
team_role = "01M7R8RA5WQVHXYQVDV4V087PJ"

[[features.team_applications.roles]]
key = "moderator-discover"
server = "01M7R6QRNYYA4K72SB43GX5GAA"
role = "01M7R6QRNYAQ5QQKP4TED529JH"
team_role = "01M7R8RA5WQVHXYQVDV4V087PJ"

[[features.team_applications.roles]]
key = "designer"
server = "01M7R6Q1RBM0Y8SV0FXHYYVW19"
role = "01M7T38NC82M5W0KNTKM8X1VY3"
team_role = "01M7T3846PC0A8RF4RB7PDHZVG"

[[features.team_applications.roles]]
key = "developer"
server = "01M7R6RHBHYW6RM8RSADVK6H9X"
role = "01M7T3A203H84H8K9392MB6G3N"
team_role = "01M7T39HNGVFPSEFRDCKTYWVTD"

[[features.team_applications.roles]]
key = "administrator"
server = "01M7AD5WEYHG4GVN2BP4PBQRN6"
manual = true
"#;

    fn parse(toml: &str) -> FeaturesTeamApplications {
        Config::builder()
            .add_source(File::from_str(toml, FileFormat::Toml))
            .build()
            .unwrap()
            .get("features.team_applications")
            .unwrap()
    }

    #[test]
    fn volunteers_default_off_with_spec_values() {
        let v = FeaturesTeamApplications::default().volunteers;
        assert!(!v.enabled);
        assert_eq!(v.trial_days, 30);
        assert_eq!(v.monthly_minimum, 2);
        assert_eq!(v.away_max_days, 90);
        assert_eq!(v.identity_keep_days, 90);
        assert_eq!(v.activity_keep_days, 190);
        assert_eq!(v.rules_version, "1");
        assert_eq!(v.team_server, "");
        assert_eq!(v.report_channel, "");
        assert_eq!(v, FeaturesTeamVolunteers::default());
    }

    #[test]
    fn work_channels_default_empty() {
        assert!(TeamRole::default().work_channels.is_empty());
        let parsed = parse(
            r#"
[features.team_applications]
[[features.team_applications.roles]]
key = "support"
"#,
        );
        assert!(parsed.roles[0].work_channels.is_empty());
    }

    #[test]
    fn old_block_without_volunteers_still_parses() {
        let parsed = parse(OLD_BLOCK);
        assert!(parsed.enabled());
        assert_eq!(parsed.roles.len(), 8);
        assert_eq!(parsed.cooldown_days, 30);
        assert_eq!(parsed.volunteers, FeaturesTeamVolunteers::default());
        assert!(parsed.roles.iter().all(|role| role.work_channels.is_empty()));
    }

    #[test]
    fn volunteers_table_overrides_and_member_roles_filter_by_server() {
        let parsed = parse(
            r#"
[features.team_applications.volunteers]
enabled = true
trial_days = 14

[[features.team_applications.roles]]
key = "support"
server = "A"
work_channels = ["c1", "c2"]

[[features.team_applications.roles]]
key = "designer"
server = "B"
"#,
        );
        assert!(parsed.volunteers.enabled);
        assert_eq!(parsed.volunteers.trial_days, 14);
        assert_eq!(parsed.volunteers.monthly_minimum, 2);
        let in_a = parsed.member_roles("A");
        assert_eq!(in_a.len(), 1);
        assert_eq!(in_a[0].work_channels, vec!["c1", "c2"]);
        assert!(parsed.member_roles("Z").is_empty());
    }
}

#[cfg(test)]
mod upload_limit_tests {
    use config::{Config, File, FileFormat};

    use super::Settings;

    /// Cloudflare stops uploads at 100 MB on its free plan; our limit is 95 MB
    const FILE_LIMIT: usize = 95_000_000;
    const CLOUDFLARE_LIMIT: usize = 100_000_000;

    fn shipped() -> Settings {
        Config::builder()
            .add_source(File::from_str(include_str!("../Revolt.toml"), FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }

    #[test]
    fn every_file_limit_fits_through_cloudflare() {
        let settings = shipped();
        let limits = &settings.features.limits;
        let mut largest = 0;
        for tier in limits.roles.values().chain([&limits.new_user, &limits.default]) {
            for (tag, size) in &tier.file_upload_size_limit {
                assert!(*size <= FILE_LIMIT, "{tag}: {size}");
                largest = largest.max(*size);
            }
        }
        for level in &settings.features.billing.levels {
            assert!(level.attachments <= FILE_LIMIT, "{}", level.attachments);
            largest = largest.max(level.attachments);
        }
        assert_eq!(largest, FILE_LIMIT);
        let body = limits.global.body_limit_size;
        assert!(body > largest && body < CLOUDFLARE_LIMIT, "{body}");
    }

    #[test]
    fn plugins_ship_switched_off() {
        assert!(!shipped().features.plugins_enabled);
    }

    #[test]
    fn purge_is_off_until_it_is_filled_in() {
        assert!(!shipped().files.purge.configured());
    }
}

#[cfg(test)]
mod badge_command_settings_tests {
    use config::{Config, File, FileFormat};

    use super::{FeaturesBadgeCommand, Settings};

    fn settings(toml: &str) -> Settings {
        Config::builder()
            .add_source(File::from_str(include_str!("../Revolt.toml"), FileFormat::Toml))
            .add_source(File::from_str(toml, FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }

    #[test]
    fn badge_command_ships_switched_off() {
        let shipped = settings("");
        assert_eq!(shipped.features.badge_command, FeaturesBadgeCommand::default());
        assert!(!shipped.features.badge_command.enabled());
    }

    #[test]
    fn old_config_without_the_block_still_parses() {
        // the shipped file as it was before the command existed: no `badge_command` block
        let shipped = include_str!("../Revolt.toml");
        let start = shipped.find("[features.badge_command]").unwrap();
        let end = start + shipped[start..].find("[features.team_applications]").unwrap();
        let old = format!("{}{}", &shipped[..start], &shipped[end..]);
        assert!(!old.contains("badge_command"));
        let parsed: Settings = Config::builder()
            .add_source(File::from_str(&old, FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(parsed.features.badge_command, FeaturesBadgeCommand::default());
        assert!(!parsed.features.badge_command.enabled());
    }

    #[test]
    fn badge_command_needs_both_ids() {
        let both = settings("[features.badge_command]\nowner = \"01OWNER\"\nbot = \"01BOT\"\n");
        assert!(both.features.badge_command.enabled());
        assert_eq!(both.features.badge_command.owner, "01OWNER");
        let owner_only = settings("[features.badge_command]\nowner = \"01OWNER\"\n");
        assert!(!owner_only.features.badge_command.enabled());
        let blank = settings("[features.badge_command]\nowner = \" \"\nbot = \"01BOT\"\n");
        assert!(!blank.features.badge_command.enabled());
    }
}

#[cfg(test)]
mod staff_desk_settings_tests {
    use config::{Config, File, FileFormat};

    use super::{FeaturesStaffDesk, Settings};

    fn settings(toml: &str) -> Settings {
        Config::builder()
            .add_source(File::from_str(include_str!("../Revolt.toml"), FileFormat::Toml))
            .add_source(File::from_str(toml, FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }

    #[test]
    fn staff_desk_ships_switched_off() {
        let shipped = settings("");
        assert_eq!(shipped.features.staff_desk, FeaturesStaffDesk::default());
        assert!(!shipped.features.staff_desk.on());
    }

    #[test]
    fn staff_desk_needs_enabled_bot_and_a_place() {
        let on = settings(
            "[features.staff_desk]\nenabled = true\nbot = \"01BOT\"\nplaces = [\"01CH\"]\n",
        );
        assert!(on.features.staff_desk.on());
        let dms = settings(
            "[features.staff_desk]\nenabled = true\nbot = \"01BOT\"\nallow_dms = true\n",
        );
        assert!(dms.features.staff_desk.on());
        let no_place = settings("[features.staff_desk]\nenabled = true\nbot = \"01BOT\"\n");
        assert!(!no_place.features.staff_desk.on());
        let no_bot = settings("[features.staff_desk]\nenabled = true\nplaces = [\"01CH\"]\n");
        assert!(!no_bot.features.staff_desk.on());
        let off = settings("[features.staff_desk]\nbot = \"01BOT\"\nplaces = [\"01CH\"]\n");
        assert!(!off.features.staff_desk.on());
    }
}
