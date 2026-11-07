use serde::{Serialize, Deserialize};

use super::client::Ping;

#[derive(Serialize, Deserialize, Debug)]
#[cfg_attr(feature = "rocket-impl", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum ClientMessage {
    Authenticate {
        token: String,
        /// Bots: the gateway intents to receive (`v0::GatewayIntents`);
        /// the free ones when left out
        #[serde(default)]
        intents: Option<u64>,
    },
    BeginTyping { channel: String },
    EndTyping { channel: String },
    Subscribe { server_id: String },
    Ping { data: Ping, responded: Option<()> },
}
