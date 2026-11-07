use iso8601_timestamp::Timestamp;

auto_derived!(
    /// Platform policy change
    pub struct PolicyChange {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,

        /// Time at which this policy was created
        pub created_time: Timestamp,
        /// Time at which this policy is effective
        pub effective_time: Timestamp,

        /// Message shown to users
        pub description: String,
        /// URL with details about changes
        pub url: String,
        /// The message by language code (`ar`, `en`, `de`)
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        pub translations: std::collections::HashMap<String, String>,
        /// Staff member who published it (none for older changes)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub created_by: Option<String>,
    }
);
