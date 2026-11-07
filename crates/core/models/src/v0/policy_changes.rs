use iso8601_timestamp::Timestamp;

auto_derived!(
    /// Platform policy change
    pub struct PolicyChange {
        /// Time at which this policy was created
        pub created_time: Timestamp,
        /// Time at which this policy is effective
        pub effective_time: Timestamp,

        /// Message shown to users
        pub description: String,
        /// URL with details about changes
        pub url: String,
        /// The message in other languages, by language code (`ar`, `en`,
        /// `de`); `description` is the fallback
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "std::collections::HashMap::is_empty"))]
        pub translations: std::collections::HashMap<String, String>,
    }
);
