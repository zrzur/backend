use revolt_config::config;
use revolt_result::Result;
use rocket::serde::json::Json;
use serde::Serialize;
use std::collections::HashMap;

/// # hCaptcha Configuration
#[derive(Serialize, JsonSchema, Debug)]
pub struct CaptchaFeature {
    /// Whether captcha is enabled
    pub enabled: bool,
    /// Client key used for solving captcha
    pub key: String,
}

/// # Generic Service Configuration
#[derive(Serialize, JsonSchema, Debug)]
pub struct Feature {
    /// Whether the service is enabled
    pub enabled: bool,
    /// URL pointing to the service
    pub url: String,
}

/// # Information about a livekit node
#[derive(Serialize, JsonSchema, Debug)]
pub struct VoiceNode {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub public_url: String,
}

/// # Voice Server Configuration
#[derive(Serialize, JsonSchema, Debug)]
pub struct VoiceFeature {
    /// Whether voice is enabled
    pub enabled: bool,
    /// All livekit nodes
    pub nodes: Vec<VoiceNode>,
}

/// # Feature Configuration
#[derive(Serialize, JsonSchema, Debug)]
pub struct RevoltFeatures {
    /// hCaptcha configuration
    pub captcha: CaptchaFeature,
    /// Whether email verification is enabled
    pub email: bool,
    /// Whether this server is invite only
    pub invite_only: bool,
    /// File server service configuration
    pub autumn: Feature,
    /// Proxy service configuration
    pub january: Feature,
    /// Voice server configuration
    pub livekit: VoiceFeature,
    /// Limits
    pub limits: LimitsConfig,
    /// Legal links
    pub legal_links: LegalLinks,
    /// Asset link
    pub assets: String,
}

/// # Limits For Users
#[derive(Serialize, JsonSchema, Debug)]
pub struct LimitsConfig {
    /// Global Limits
    pub global: GlobalLimits,
    /// New User Limits
    pub new_user: UserLimits,
    /// Default User Limits
    pub default: UserLimits,
}

/// # Legal links
#[derive(Serialize, JsonSchema, Debug)]
pub struct LegalLinks {
    /// Terms of Service URL
    pub terms_of_service: String,
    /// Privacy Policy URL
    pub privacy_policy: String,
    /// Guidelines URL
    pub guidelines: String,
}

/// # Global limits
#[derive(Serialize, JsonSchema, Debug)]
pub struct GlobalLimits {
    /// max group size
    group_size: i64,
    /// max message embeds
    message_embeds: i64,
    /// max replies
    message_replies: i64,
    /// max reactions per message
    message_reactions: i64,
    /// max server emoji
    server_emoji: i64,
    /// max server roles
    server_roles: i64,
    /// max server channels
    server_channels: i64,
    body_limit_size: i64,

    /// restrict server creation to these users.
    /// if blank, all users can create servers
    pub restrict_server_creation: Vec<String>,
    /// New user hours
    new_user_hours: i64,

    /// Number of days after creation before an invite expires
    max_invite_duration_days: i64,
}

/// # User Limits
#[derive(Serialize, JsonSchema, Debug)]
pub struct UserLimits {
    /// Max Outgoing Friend Requests
    pub outgoing_friend_requests: i64,
    /// Max Owned Bots
    pub bots: i64,
    /// Max message content length
    pub message_length: i64,
    /// max message attachments
    pub message_attachments: i64,
    /// max servers
    pub servers: i64,
    /// max audio quality
    pub voice_quality: i64,
    /// video streaming enabled
    pub video: bool,
    /// max video resolution (vertical, horizontal)
    pub video_resolution: [i64; 2],
    /// min/max aspect ratios
    pub video_aspect_ratio: [f64; 2],
    pub file_upload_size_limits: HashMap<String, usize>,
}

impl UserLimits {
    fn from_feature_limits(fl: revolt_config::FeaturesLimits) -> UserLimits {
        UserLimits {
            outgoing_friend_requests: fl.outgoing_friend_requests as i64,
            bots: fl.bots as i64,
            message_length: fl.message_length as i64,
            message_attachments: fl.message_attachments as i64,
            servers: fl.servers as i64,
            voice_quality: fl.voice_quality as i64,
            video: fl.video,
            video_resolution: [fl.video_resolution[0] as i64, fl.video_resolution[1] as i64],
            video_aspect_ratio: [
                fl.video_aspect_ratio[0] as f64,
                fl.video_aspect_ratio[1] as f64,
            ],
            file_upload_size_limits: fl.file_upload_size_limit,
        }
    }
}

/// # Server Configuration
#[derive(Serialize, JsonSchema, Debug)]
pub struct RevoltConfig {
    /// Zarzour product version (authoritative; from `backend/ZARZOUR_VERSION`)
    pub zarzour: String,
    /// Upstream Stoat protocol compatibility version (`CARGO_PKG_VERSION`)
    pub stoat: String,
    /// Legacy Revolt compatibility alias of `stoat` (temporary)
    pub revolt: String,
    /// Features enabled on this Zarzour node
    pub features: RevoltFeatures,
    /// WebSocket URL
    pub ws: String,
    /// URL pointing to the client serving this node
    pub app: String,
    /// Web Push VAPID public key
    pub vapid: String,
}

fn january_public_url(hosts: &revolt_config::Hosts) -> String {
    if !hosts.january_public.is_empty() {
        hosts.january_public.clone()
    } else {
        hosts.january.clone()
    }
}

/// # Query Node
///
/// Fetch the server configuration for this Zarzour instance.
#[openapi(tag = "Core")]
#[get("/")]
pub async fn root() -> Result<Json<RevoltConfig>> {
    let config = config().await;

    Ok(Json(RevoltConfig {
        zarzour: env!("ZARZOUR_PKG_VERSION").to_string(),
        stoat: env!("CARGO_PKG_VERSION").to_string(),
        revolt: env!("CARGO_PKG_VERSION").to_string(),
        features: RevoltFeatures {
            captcha: CaptchaFeature {
                enabled: !config.api.security.captcha.hcaptcha_key.is_empty(),
                key: config.api.security.captcha.hcaptcha_sitekey.clone(),
            },
            email: !config.api.smtp.host.is_empty(),
            invite_only: config.api.registration.invite_only,
            autumn: Feature {
                enabled: !config.hosts.autumn.is_empty(),
                url: config.hosts.autumn.clone(),
            },
            january: Feature {
                enabled: !config.hosts.january.is_empty(),
                url: january_public_url(&config.hosts),
            },
            livekit: VoiceFeature {
                enabled: !config.hosts.livekit.is_empty(),
                nodes: config
                    .api
                    .livekit
                    .nodes
                    .iter()
                    .filter(|(_, node)| !node.private)
                    .map(|(name, value)| VoiceNode {
                        name: name.clone(),
                        lat: value.lat,
                        lon: value.lon,
                        public_url: config
                            .hosts
                            .livekit
                            .get(name)
                            .expect("Missing corresponding host for voice node")
                            .clone(),
                    })
                    .collect(),
            },
            assets: config.hosts.assets.clone(),
            limits: LimitsConfig {
                global: GlobalLimits {
                    group_size: config.features.limits.global.group_size as i64,
                    message_embeds: config.features.limits.global.message_embeds as i64,
                    message_replies: config.features.limits.global.message_replies as i64,
                    message_reactions: config.features.limits.global.message_reactions as i64,
                    server_emoji: config.features.limits.global.server_emoji as i64,
                    server_roles: config.features.limits.global.server_roles as i64,
                    server_channels: config.features.limits.global.server_channels as i64,
                    body_limit_size: config.features.limits.global.body_limit_size as i64,
                    restrict_server_creation: config
                        .features
                        .limits
                        .global
                        .restrict_server_creation,
                    new_user_hours: config.features.limits.global.new_user_hours as i64,
                    max_invite_duration_days: config.features.limits.global.max_invite_duration_days as i64,
                },
                new_user: UserLimits::from_feature_limits(config.features.limits.new_user),
                default: UserLimits::from_feature_limits(config.features.limits.default),
            },
            legal_links: LegalLinks {
                terms_of_service: config.features.legal_links.terms_of_service,
                privacy_policy: config.features.legal_links.privacy_policy,
                guidelines: config.features.legal_links.guidelines,
            },
        },
        ws: config.hosts.events,
        app: config.hosts.app,
        vapid: config.pushd.vapid.public_key,
    }))
}

#[cfg(test)]
mod test {
    use rocket::http::Status;
    use serde_json::Value;

    #[rocket::async_test]
    async fn hello_world() {
        let harness = crate::util::test::TestHarness::new().await;
        let response = harness.client.get("/").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
    }

    #[rocket::async_test]
    async fn hello_world_concurrent() {
        let harness = crate::util::test::TestHarness::new().await;
        let response = harness.client.get("/").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
    }

    #[rocket::async_test]
    async fn root_identity_fields() {
        let harness = crate::util::test::TestHarness::new().await;
        let response = harness.client.get("/").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let body: Value = response.into_json().await.expect("json body");
        assert_eq!(
            body.get("zarzour").and_then(|v| v.as_str()),
            Some(env!("ZARZOUR_PKG_VERSION"))
        );
        assert_eq!(
            body.get("stoat").and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            body.get("revolt").and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_ne!(
            body.get("zarzour").and_then(|v| v.as_str()),
            body.get("stoat").and_then(|v| v.as_str()),
            "product identity must not silently equal upstream protocol version"
        );
    }

    #[test]
    fn january_public_prefers_public_host() {
        let mut hosts = revolt_config::Hosts {
            app: String::new(),
            api: String::new(),
            events: String::new(),
            autumn: String::new(),
            january: "http://january:14705".into(),
            january_public: "http://127.0.0.1:14705".into(),
            livekit: Default::default(),
            assets: String::new(),
        };
        assert_eq!(
            super::january_public_url(&hosts),
            "http://127.0.0.1:14705"
        );
        hosts.january_public.clear();
        assert_eq!(
            super::january_public_url(&hosts),
            "http://january:14705"
        );
    }
}
