use std::collections::HashMap;

auto_derived!(
    /// A message from the Zrzur team, shown in the app until dismissed
    /// (`platform_notices`)
    pub struct PlatformNotice {
        #[serde(rename = "_id")]
        pub id: String,
        /// One person; everyone when `None`
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub user_id: Option<String>,
        /// Title by language code (`ar`, `en`, `de`)
        pub title: HashMap<String, String>,
        /// Text by language code
        pub body: HashMap<String, String>,
        /// Optional https link for more
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub link: Option<String>,
        /// `info`, `warning` or `important`
        pub tone: String,
        pub created_by: String,
        /// Milliseconds since the epoch
        pub created_at: i64,
        /// No longer shown after this
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expires_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub withdrawn_at: Option<i64>,
        /// The email copy was sent (one person only)
        #[serde(default)]
        pub emailed: bool,
        /// A push copy was queued (one person only)
        #[serde(default)]
        pub pushed: bool,
    }
);

impl PlatformNotice {
    /// Still shown at `now`
    pub fn is_live(&self, now: i64) -> bool {
        self.withdrawn_at.is_none() && self.expires_at.is_none_or(|e| now < e)
    }
}

/// Longest notice title
pub const NOTICE_TITLE_MAX: usize = 100;
/// Longest notice text
pub const NOTICE_BODY_MAX: usize = 1000;
