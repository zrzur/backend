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
    pub early_adopter_cutoff: Option<u64>,
    pub min_username_length: usize,
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
}

/// Accounts on other services a person can link to their profile
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ApiConnections {
    #[serde(default)]
    pub steam: ApiConnectionSteam,
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

    #[serde(default)]
    pub advanced: FeaturesAdvanced,

    #[serde(default)]
    pub billing: FeaturesBilling,

    #[serde(default)]
    pub community: FeaturesCommunity,

    #[serde(default)]
    pub age_safety: FeaturesAgeSafety,
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
            max_dimension: community_max_dimension(),
            max_frames: community_max_frames(),
            max_duration_ms: community_max_duration_ms(),
            effects_enabled: community_effects_enabled(),
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
