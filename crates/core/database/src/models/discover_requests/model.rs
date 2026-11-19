auto_derived!(
    #[derive(Hash)]
    pub enum DiscoverRequestType {
        Bot,
        Server,
    }

    pub enum DiscoverRequestStatus {
        Pending,
        UnderReview,
        Removed(Option<String>),
        Denied(Option<String>),   // reason
        Approved(Option<String>), // reason
    }

    /// Discover request
    pub struct DiscoverRequest {
        /// The type of request.
        pub request_type: DiscoverRequestType,
        /// The ID of the bot/server
        pub request_id: String,
        /// status of the request
        pub status: DiscoverRequestStatus,
        /// Listing tags chosen by the owner
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub tags: Vec<String>,
        /// Listing category chosen by the owner
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub category: Option<String>,
        /// Main language (ISO 639-1) chosen by the owner
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub language: Option<String>,
        /// Whether moderators feature this listing
        #[serde(default, skip_serializing_if = "crate::if_false")]
        pub featured: bool,
        /// When the owner filed (or filed again) this request, in
        /// milliseconds since the epoch. Missing on requests filed before
        /// this was recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub submitted_at: Option<i64>,
        /// Reviewer who last acted on this filing (under review, approved,
        /// denied) from the review desk. Never shown to the owner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub acted_by: Option<String>,
        /// When `acted_by` acted
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub acted_at: Option<i64>,
    }

    pub struct DiscoverBan {
        /// Ban Id
        #[serde(rename = "_id")]
        pub id: String,
        /// The type of item.
        #[serde(rename = "type")]
        pub item_type: DiscoverRequestType,
        /// The ID of the bot/server
        pub item_id: String,
    }
);
