use async_tungstenite::tungstenite::{handshake, Message};
use futures::channel::oneshot::Sender;
use once_cell::sync::Lazy;
use regex::Regex;
use revolt_database::events::client::ReadyPayloadFields;
use revolt_result::{create_error, Result};
use serde::{Deserialize, Serialize};

/// matches either a single word ie "users" or a key and value ie "settings[notifications]"
static READY_PAYLOAD_FIELD_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^(\w+)(?:\[(\S+)\])?$"#).unwrap());

/// Enumeration of supported protocol formats
#[derive(Debug, Clone)]
pub enum ProtocolFormat {
    Json,
    Msgpack,
}

/// Compression a client asked for with `?compression=` (JSON only)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCompression {
    /// Every frame is a text frame, as before
    None,
    /// A JSON frame of [`COMPRESS_FROM`] bytes or more is sent as a binary
    /// frame holding its raw DEFLATE stream (RFC 1951, what a browser's
    /// `DecompressionStream("deflate-raw")` reads); smaller frames stay text
    Deflate,
}

/// Frames smaller than this stay text: compressing them saves little and
/// costs both sides time, and live events are serialised once per recipient.
/// The first answer (Ready) of anyone in more than a few servers is larger.
pub const COMPRESS_FROM: usize = 16 * 1024;

/// Raw DEFLATE of some bytes
fn deflate_raw(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    // the fast level: about a fifth of the size instead of a sixth, at a
    // fraction of the time (a reconnect storm deflates every Ready at once)
    let mut encoder = flate2::write::DeflateEncoder::new(
        Vec::with_capacity(bytes.len() / 4),
        flate2::Compression::fast(),
    );
    encoder
        .write_all(bytes)
        .expect("Writing into a Vec cannot fail.");
    encoder.finish().expect("Writing into a Vec cannot fail.")
}

/// User-provided protocol configuration
#[derive(Debug, Clone)]
pub struct ProtocolConfiguration {
    protocol_version: i32,
    format: ProtocolFormat,
    session_token: Option<String>,
    ready_payload_fields: ReadyPayloadFields,
    /// Gateway intents a bot asked for (`?intents=` or `Authenticate`)
    intents: Option<u64>,
    /// The client can resume this session after a drop (`?resumable=true`
    /// or `Authenticate { resumable: true }`)
    resumable: bool,
    /// Compression the client asked for, and the server allows
    compression: FrameCompression,
}

impl ProtocolConfiguration {
    /// Create a new protocol configuration object from provided data
    pub fn from(
        protocol_version: i32,
        format: ProtocolFormat,
        session_token: Option<String>,
        ready_payload_fields: ReadyPayloadFields,
    ) -> Self {
        Self {
            protocol_version,
            format,
            session_token,
            ready_payload_fields,
            intents: None,
            resumable: false,
            compression: FrameCompression::None,
        }
    }

    /// Ask for compressed frames (`?compression=deflate`)
    pub fn set_compression(&mut self, compression: FrameCompression) {
        self.compression = compression;
    }

    /// Keep the compression the client asked for only where the server
    /// allows it (`features.gateway_compression_enabled`) and the format is
    /// JSON: in msgpack every frame is already binary, so a compressed one
    /// could not be told apart
    pub fn allow_compression(&mut self, allowed: bool) {
        if !allowed || !matches!(self.format, ProtocolFormat::Json) {
            self.compression = FrameCompression::None;
        }
    }

    /// The compression frames are sent with
    pub fn get_compression(&self) -> FrameCompression {
        self.compression
    }

    /// Ask for a session that can be resumed
    pub fn set_resumable(&mut self, resumable: bool) {
        self.resumable |= resumable;
    }

    /// Whether the client asked for a session that can be resumed
    pub fn is_resumable(&self) -> bool {
        self.resumable
    }

    /// Decode some WebSocket message into a T: Deserialize using the client's specified protocol format
    pub fn decode<'a, T: Deserialize<'a>>(&self, msg: &'a Message) -> Result<T> {
        match self.format {
            ProtocolFormat::Json => {
                if let Message::Text(text) = msg {
                    serde_json::from_str(text).map_err(|_| create_error!(InternalError))
                } else {
                    Err(create_error!(InternalError))
                }
            }
            ProtocolFormat::Msgpack => {
                if let Message::Binary(buf) = msg {
                    rmp_serde::from_slice(buf).map_err(|_| create_error!(InternalError))
                } else {
                    Err(create_error!(InternalError))
                }
            }
        }
    }

    /// Encode T: Serialize into a WebSocket message using the client's specified protocol format
    pub fn encode<T: Serialize>(&self, data: &T) -> Message {
        match self.format {
            ProtocolFormat::Json => {
                let text = serde_json::to_string(data).expect("Failed to serialise (as json).");
                if self.compression == FrameCompression::Deflate && text.len() >= COMPRESS_FROM {
                    Message::Binary(deflate_raw(text.as_bytes()))
                } else {
                    Message::Text(text)
                }
            }
            ProtocolFormat::Msgpack => Message::Binary(
                rmp_serde::to_vec_named(data).expect("Failed to serialise (as msgpack)."),
            ),
        }
    }

    /// Set the current session token
    pub fn set_session_token(&mut self, token: String) {
        self.session_token.replace(token);
    }

    /// Get the current session token
    pub fn get_session_token(&self) -> &Option<String> {
        &self.session_token
    }

    /// Set the gateway intents a bot asked for
    pub fn set_intents(&mut self, intents: Option<u64>) {
        if intents.is_some() {
            self.intents = intents;
        }
    }

    /// Gateway intents a bot asked for, if any
    pub fn get_intents(&self) -> Option<u64> {
        self.intents
    }

    /// Get the protocol version specified
    pub fn get_protocol_version(&self) -> i32 {
        self.protocol_version
    }

    /// Get the protocol format specified
    pub fn get_protocol_format(&self) -> &ProtocolFormat {
        &self.format
    }

    /// Get ready payload fields
    pub fn get_ready_payload_fields(&self) -> &ReadyPayloadFields {
        &self.ready_payload_fields
    }
}

/// Object holding one side of a channel for receiving the parsed information
pub struct WebsocketHandshakeCallback {
    sender: Sender<ProtocolConfiguration>,
}

impl WebsocketHandshakeCallback {
    /// Create a callback using a given sender
    pub fn from(sender: Sender<ProtocolConfiguration>) -> Self {
        Self { sender }
    }
}

impl handshake::server::Callback for WebsocketHandshakeCallback {
    /// Handle request to create a new WebSocket connection
    fn on_request(
        self,
        request: &handshake::server::Request,
        response: handshake::server::Response,
    ) -> Result<handshake::server::Response, handshake::server::ErrorResponse> {
        // Take and parse query parameters from the URI.
        let query = request.uri().query().unwrap_or_default();
        let params = querystring::querify(query);

        // Set default values for the protocol.
        let mut protocol_version = 1;
        let mut format = ProtocolFormat::Json;
        let mut session_token = None;
        let mut intents = None;
        let mut resumable = false;
        let mut compression = FrameCompression::None;
        let mut ready_payload_fields = if params.iter().any(|(k, _)| *k == "ready") {
            // If they pass the ready field, set all fields to false

            ReadyPayloadFields {
                users: false,
                servers: false,
                channels: false,
                members: false,
                emojis: false,
                voice_states: false,
                user_settings: Vec::new(),
                channel_unreads: false,
                policy_changes: false,
            }
        } else {
            ReadyPayloadFields::default()
        };

        // Parse and map parameters from key-value to known variables.
        for (key, value) in params {
            match key {
                "version" => {
                    if let Ok(version) = value.parse() {
                        protocol_version = version;
                    }
                }
                "format" => match value {
                    "json" => format = ProtocolFormat::Json,
                    "msgpack" => format = ProtocolFormat::Msgpack,
                    _ => {}
                },
                "token" => session_token = Some(value.into()),
                "intents" => intents = value.parse().ok(),
                "resumable" => resumable = matches!(value, "true" | "1"),
                "compression" => {
                    if value == "deflate" {
                        compression = FrameCompression::Deflate;
                    }
                }
                "ready" => {
                    // Re-enable all the fields the client specifies
                    if let Some(captures) = READY_PAYLOAD_FIELD_REGEX.captures(value) {
                        if let Some(field) = captures.get(0) {
                            match field.as_str() {
                                "users" => ready_payload_fields.users = true,
                                "servers" => ready_payload_fields.servers = true,
                                "channels" => ready_payload_fields.channels = true,
                                "members" => ready_payload_fields.members = true,
                                "emojis" => ready_payload_fields.emojis = true,
                                "voice_states" => ready_payload_fields.voice_states = true,
                                "channel_unreads" => ready_payload_fields.channel_unreads = true,
                                "user_settings" => {
                                    if let Some(subkey) = captures.get(1) {
                                        ready_payload_fields
                                            .user_settings
                                            .push(subkey.as_str().to_string());
                                    }
                                }
                                "policy_changes" => ready_payload_fields.policy_changes = true,
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Send configuration information back from this callback.
        // We have to use a channel as this function does not borrow mutably.
        if self
            .sender
            .send(ProtocolConfiguration {
                protocol_version,
                format,
                session_token,
                ready_payload_fields,
                intents,
                resumable,
                compression,
            })
            .is_ok()
        {
            Ok(response)
        } else {
            Err(handshake::server::ErrorResponse::new(None))
        }
    }
}

#[cfg(test)]
mod compression_tests {
    use std::io::Read;

    use async_tungstenite::tungstenite::{handshake::server::Callback, Message};
    use revolt_database::events::client::ReadyPayloadFields;
    use serde_json::json;

    use super::{
        FrameCompression, ProtocolConfiguration, ProtocolFormat, WebsocketHandshakeCallback,
        COMPRESS_FROM,
    };

    fn config(format: ProtocolFormat, compression: FrameCompression) -> ProtocolConfiguration {
        let mut c = ProtocolConfiguration::from(1, format, None, ReadyPayloadFields::default());
        c.set_compression(compression);
        c
    }

    fn inflate(bytes: &[u8]) -> String {
        let mut out = String::new();
        flate2::read::DeflateDecoder::new(bytes)
            .read_to_string(&mut out)
            .expect("valid raw deflate");
        out
    }

    /// Like a Ready: many similar channels
    fn big() -> serde_json::Value {
        json!({ "type": "Ready", "channels": (0..400).map(|i| json!({
            "_id": format!("01ARZ3NDEKTSV4RRFFQ69G5F{i:03}"),
            "channel_type": "TextChannel",
            "name": format!("channel {i}"),
            "server": "01ARZ3NDEKTSV4RRFFQ69G5FSV",
        })).collect::<Vec<_>>() })
    }

    #[test]
    fn a_large_json_frame_is_sent_deflated_and_inflates_to_the_same_json() {
        let c = config(ProtocolFormat::Json, FrameCompression::Deflate);
        let value = big();
        let text = serde_json::to_string(&value).unwrap();
        assert!(text.len() >= COMPRESS_FROM);
        match c.encode(&value) {
            Message::Binary(bytes) => {
                assert!(
                    bytes.len() * 4 < text.len(),
                    "{} of {} bytes",
                    bytes.len(),
                    text.len()
                );
                assert_eq!(inflate(&bytes), text);
            }
            other => panic!("expected a binary frame, got {other:?}"),
        }
    }

    #[test]
    fn a_small_json_frame_stays_text() {
        let c = config(ProtocolFormat::Json, FrameCompression::Deflate);
        let value = json!({ "type": "Pong", "data": 1 });
        assert_eq!(
            c.encode(&value),
            Message::Text(serde_json::to_string(&value).unwrap())
        );
    }

    #[test]
    fn without_compression_every_frame_is_text_as_before() {
        let c = config(ProtocolFormat::Json, FrameCompression::None);
        let value = big();
        assert_eq!(
            c.encode(&value),
            Message::Text(serde_json::to_string(&value).unwrap())
        );
    }

    #[test]
    fn the_server_switch_and_msgpack_turn_compression_off() {
        let mut off = config(ProtocolFormat::Json, FrameCompression::Deflate);
        off.allow_compression(false);
        assert_eq!(off.get_compression(), FrameCompression::None);
        assert!(matches!(off.encode(&big()), Message::Text(_)));

        let mut on = config(ProtocolFormat::Json, FrameCompression::Deflate);
        on.allow_compression(true);
        assert_eq!(on.get_compression(), FrameCompression::Deflate);

        let mut msgpack = config(ProtocolFormat::Msgpack, FrameCompression::Deflate);
        msgpack.allow_compression(true);
        assert_eq!(msgpack.get_compression(), FrameCompression::None);
        // msgpack frames are what they always were
        let value = big();
        assert_eq!(
            msgpack.encode(&value),
            Message::Binary(rmp_serde::to_vec_named(&value).unwrap())
        );
    }

    fn handshake(query: &str) -> ProtocolConfiguration {
        let (sender, mut receiver) = futures::channel::oneshot::channel();
        let request = async_tungstenite::tungstenite::http::Request::builder()
            .uri(format!("/?{query}"))
            .body(())
            .unwrap();
        let response = async_tungstenite::tungstenite::http::Response::builder()
            .body(())
            .unwrap();
        WebsocketHandshakeCallback::from(sender)
            .on_request(&request, response)
            .expect("accepted");
        receiver.try_recv().unwrap().expect("a configuration")
    }

    #[test]
    fn the_connect_parameter_asks_for_deflate_and_nothing_else() {
        assert_eq!(
            handshake("version=1&format=json&compression=deflate").get_compression(),
            FrameCompression::Deflate
        );
        assert_eq!(
            handshake("version=1&format=json").get_compression(),
            FrameCompression::None
        );
        assert_eq!(
            handshake("version=1&format=json&compression=zstd").get_compression(),
            FrameCompression::None
        );
    }
}
