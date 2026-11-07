auto_derived!(
    /// State of a bot verification request
    #[derive(Copy)]
    pub enum BotVerificationStatus {
        /// Waiting for a reviewer
        Pending,
        /// The bot shows the verified mark
        Approved,
        /// Not verified; the owner may apply again later
        Denied,
        /// Was verified, mark taken away
        Revoked,
    }

    /// A bot owner's request for the verified mark (`bot_verifications`)
    pub struct BotVerification {
        #[serde(rename = "_id")]
        pub id: String,
        pub bot_id: String,
        /// Owner at the time of the request
        pub owner_id: String,
        /// What the bot does and why it should be verified
        pub statement: String,
        /// Website or documentation
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub website: Option<String>,
        /// Where users get help
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub support: Option<String>,
        /// The bot's privacy policy (requests from before it was required
        /// have none)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub privacy_policy_url: Option<String>,
        /// The bot's terms of service, same as above
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub terms_of_service_url: Option<String>,
        /// Privileged gateway intents asked for; approving the request
        /// approves them (needed past 100 servers)
        #[serde(default)]
        pub intents: i64,
        pub status: BotVerificationStatus,
        /// Milliseconds since the epoch
        pub created_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decided_by: Option<String>,
        /// Shown to the owner
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub decision_note: Option<String>,
    }
);

/// Longest statement an owner may write
pub const BOT_VERIFICATION_STATEMENT_MAX: usize = 1000;
/// A denied bot may apply again after this long
pub const BOT_VERIFICATION_RETRY_MS: i64 = 7 * 24 * 60 * 60 * 1000;
