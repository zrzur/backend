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
        /// Keep this session for a short time after a drop, so it can be
        /// resumed with `Resume`; `Ready` then carries `session_id`
        #[serde(default)]
        resumable: bool,
    },
    /// Continue a dropped resumable session: the events after `seq` that
    /// the client may still see are sent again, then `Resumed`. Answered
    /// with `InvalidSession { resumable: false }` when that is not possible.
    Resume {
        token: String,
        session_id: String,
        seq: u64,
    },
    BeginTyping { channel: String },
    EndTyping { channel: String },
    Subscribe { server_id: String },
    Ping { data: Ping, responded: Option<()> },
    /// Set what you are doing right now, or clear it with `activity: null`.
    /// People only, from their own session; `source` is `Rpc` (an app over
    /// the desktop's local RPC) or `Desktop` (a program the desktop saw
    /// running). Refused with an `Error` event: `FeatureDisabled`,
    /// `IsBot`, `RateLimited`, `FailedValidation` or `ProfileTextFiltered`.
    /// Ignored while the person does not share their activity.
    SetActivity {
        source: revolt_models::v0::ActivitySource,
        #[serde(default)]
        activity: Option<revolt_models::v0::DataActivity>,
    },
}
