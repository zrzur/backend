use revolt_config::config;
use revolt_database::events::{client::EventV1, server::ClientMessage};
use revolt_models::v0::GatewayIntents;
use revolt_result::Result;
use rocket::serde::json::{Json, Value};
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
    /// Whether webhooks can be edited and deleted (their routes are mounted)
    pub webhooks: bool,
    /// Whether profile effects are open
    pub community_effects: bool,
    /// Whether plugins are open on this server. Off, every plugin route
    /// answers `PluginsDisabled` and clients load no plugin
    pub plugins: bool,
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
    /// Zrzur product version (authoritative; from `backend/ZRZUR_VERSION`).
    /// Year-line scheme: 26.x = calendar year 2026 (Apple-style major year).
    pub zrzur: String,
    /// Git commit this server was built from (omitted when unknown); the
    /// source code of this exact build is at `https://github.com/zrzur/backend/tree/<commit>`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Features enabled on this Zrzur node
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
/// Fetch the server configuration for this Zrzur instance.
#[openapi(tag = "Core")]
#[get("/")]
pub async fn root() -> Result<Json<RevoltConfig>> {
    let config = config().await;

    Ok(Json(RevoltConfig {
        zrzur: env!("ZRZUR_PKG_VERSION").to_string(),
        commit: Some(env!("ZRZUR_GIT_COMMIT").to_string()).filter(|c| !c.is_empty()),
        features: RevoltFeatures {
            captcha: CaptchaFeature {
                enabled: !config.api.security.captcha.hcaptcha_key.is_empty(),
                key: config.api.security.captcha.hcaptcha_sitekey.clone(),
            },
            email: !config.api.smtp.host.is_empty(),
            invite_only: config.api.registration.invite_only,
            webhooks: config.features.webhooks_enabled,
            community_effects: config.features.community.effects_enabled,
            plugins: config.features.plugins_enabled,
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
            body.get("zrzur").and_then(|v| v.as_str()),
            Some(env!("ZRZUR_PKG_VERSION"))
        );
        assert!(
            body.get("zarzour").is_none(),
            "legacy product key zarzour must not appear in API root"
        );
        assert!(
            body.get("stoat").is_none(),
            "upstream product key stoat must not appear in API root"
        );
        assert!(
            body.get("revolt").is_none(),
            "upstream product key revolt must not appear in API root"
        );
        // the commit is additive: absent or a non-empty string, never null
        match body.get("commit") {
            None => {}
            Some(c) => assert!(c.as_str().is_some_and(|c| !c.is_empty())),
        }
        let ver = body.get("zrzur").and_then(|v| v.as_str()).unwrap();
        assert!(
            ver.starts_with("26."),
            "Zrzur product version must use year-line 26.x (got {ver})"
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

/// The gateway protocol as JSON Schema, generated from the types the
/// gateway itself uses, so it cannot drift from what is sent.
fn gateway_schema_document() -> Value {
    json!({
        "server": schemars::schema_for!(EventV1),
        "client": schemars::schema_for!(ClientMessage),
        "intents": {
            "SERVERS": GatewayIntents::SERVERS,
            "SERVER_MESSAGES": GatewayIntents::SERVER_MESSAGES,
            "DIRECT_MESSAGES": GatewayIntents::DIRECT_MESSAGES,
            "REACTIONS": GatewayIntents::REACTIONS,
            "VOICE_STATES": GatewayIntents::VOICE_STATES,
            "INTERACTIONS": GatewayIntents::INTERACTIONS,
            "MESSAGE_CONTENT": GatewayIntents::MESSAGE_CONTENT,
            "SERVER_MEMBERS": GatewayIntents::SERVER_MEMBERS,
            "PRESENCE": GatewayIntents::PRESENCE,
        },
        "privileged_intents": GatewayIntents::PRIVILEGED,
        "default_intents": GatewayIntents::DEFAULT,
        // How the gateway closes a socket. Errors come first as an `Error`
        // event; only 4014 has its own code.
        "close_codes": {
            "1005": "Closed without a code: logged out, or the session ended or expired (`Logout` came first). Authenticate again with a valid token.",
            "1013": "Try again later: the bot could not be read (`Error` `InternalError` came first). Reconnect with backoff.",
            "4014": "Disallowed intents: a privileged intent is not switched on for this bot (`Error` `MissingIntent` came first). Do not retry with the same intents.",
        },
        // Every event after `Ready` (except `Pong`) carries `seq`, counted
        // per session from 1. Sessions asked for with
        // `Authenticate { resumable: true }` can be resumed after a drop.
        "resume": {
            "seq": "Every event from the session's stream carries seq, from 1, one per packet (a Bulk has one; its items have none). Authenticated, Ready, UserSlowmodes, Pong, Error, InvalidSession and Resumed have none. Typing events get a seq but are not kept for a resume, so a replay has gaps there and where access was lost.",
            "window_seconds": 120,
            "max_events": 1000,
            "max_bytes": 4194304,
            "dropped_sessions_per_user": 3,
            "flow": "Authenticate { resumable: true } -> Ready { session_id }. After a drop, on a new socket: Resume { token, session_id, seq } -> the missed events the session may still see, in order, then Resumed; or InvalidSession { resumable: false } -> send Authenticate on the same socket and catch up over the API. Events dropped since (lost access) leave gaps in seq.",
        },
    })
}

/// # Gateway schema
///
/// Every gateway event and client message as JSON Schema, with the intents.
/// Not part of the REST contract, so it stays out of the OpenAPI document.
#[openapi(skip)]
#[get("/gateway/schema")]
pub async fn gateway_schema() -> Json<Value> {
    Json(gateway_schema_document())
}

#[cfg(test)]
mod gateway_schema_tests {
    use crate::{rocket, util::test::TestHarness};
    use rocket::http::Status;
    use serde_json::Value;

    #[rocket::async_test]
    async fn gateway_schema_lists_public_events_and_nothing_internal() {
        let harness = TestHarness::new().await;
        let response = harness.client.get("/gateway/schema").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let document: Value = response.into_json().await.unwrap();

        let events: Vec<&str> = document["server"]["oneOf"]
            .as_array()
            .expect("one schema per event")
            .iter()
            .filter_map(|event| event["properties"]["type"]["enum"][0].as_str())
            .collect();
        for public in ["Ready", "Message", "ServerMemberJoin", "InteractionCreate"] {
            assert!(events.contains(&public), "{public} missing from {events:?}");
        }
        for internal in ["CreateAccount", "CreateSession", "DeleteSession", "DeleteAllSessions"] {
            assert!(!events.contains(&internal), "{internal} must stay internal");
        }
        assert_eq!(document["intents"]["PRESENCE"], 1 << 12);
        assert!(document["client"]["oneOf"].as_array().unwrap().len() >= 6);

        // resuming is described: its messages and events, and the close codes
        for public in ["Resumed", "InvalidSession"] {
            assert!(events.contains(&public), "{public} missing from {events:?}");
        }
        let client: Vec<&str> = document["client"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["properties"]["type"]["enum"][0].as_str())
            .collect();
        assert!(client.contains(&"Resume"), "{client:?}");
        assert!(document["close_codes"]["4014"].is_string());
        assert!(document["close_codes"]["1013"].is_string(), "a bot that can't be read");
        assert!(document["resume"]["seq"].is_string(), "which events carry seq");
        assert_eq!(document["resume"]["window_seconds"], 120);
    }
}
