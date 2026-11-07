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
