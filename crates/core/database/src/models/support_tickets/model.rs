auto_derived!(
    /// Where a support ticket is
    #[derive(Copy)]
    pub enum SupportTicketStatus {
        /// Waiting for the support team
        Open,
        /// The team answered; waiting for the person
        Answered,
        /// Done; the person can reopen it within a week
        Closed,
    }

    /// One message in a ticket
    pub struct SupportTicketMessage {
        pub id: String,
        pub author_id: String,
        /// Written by the support team
        pub staff: bool,
        /// Staff-only note, never shown to the person
        #[serde(default)]
        pub internal: bool,
        pub body: String,
        /// Milliseconds since the epoch
        pub created_at: i64,
    }

    /// A question or problem someone sent to the support team
    /// (`support_tickets`)
    pub struct SupportTicket {
        #[serde(rename = "_id")]
        pub id: String,
        pub user_id: String,
        /// `account`, `billing`, `safety`, `bug`, `feature` or `other`
        pub category: String,
        pub subject: String,
        pub status: SupportTicketStatus,
        /// Staff member looking after it
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub assigned_to: Option<String>,
        /// What it is about, e.g. an order or server id
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub related: Option<String>,
        pub messages: Vec<SupportTicketMessage>,
        pub created_at: i64,
        /// Changes with every message or status change (used for
        /// optimistic locking)
        pub updated_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub closed_at: Option<i64>,
    }
);

pub const SUPPORT_CATEGORIES: [&str; 6] = ["account", "billing", "safety", "bug", "feature", "other"];
/// Most open tickets one person may have
pub const SUPPORT_OPEN_MAX: usize = 3;
/// Most messages in one ticket
pub const SUPPORT_MESSAGES_MAX: usize = 100;
pub const SUPPORT_SUBJECT_MAX: usize = 120;
pub const SUPPORT_BODY_MAX: usize = 4000;
/// A closed ticket can be reopened for this long
pub const SUPPORT_REOPEN_MS: i64 = 7 * 24 * 60 * 60 * 1000;
