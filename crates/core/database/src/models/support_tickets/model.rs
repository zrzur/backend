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
/// A closed ticket is deleted this long after it was closed: 12 months, the
/// period the privacy policy gives for support tickets (PR-14)
pub const SUPPORT_KEEP_CLOSED_MS: i64 = 365 * 24 * 60 * 60 * 1000;

#[cfg(test)]
mod tests {
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

    use super::*;

    fn ticket(id: &str, status: SupportTicketStatus, closed_at: Option<i64>) -> SupportTicket {
        SupportTicket {
            id: id.to_string(),
            user_id: "user".to_string(),
            category: "other".to_string(),
            subject: "Help".to_string(),
            status,
            assigned_to: None,
            related: None,
            messages: vec![],
            created_at: 0,
            updated_at: closed_at.unwrap_or_default(),
            closed_at,
        }
    }

    /// Closed tickets go 12 months after closing; open, answered and newer
    /// closed tickets stay, and a second run deletes nothing more
    #[tokio::test]
    async fn closed_tickets_go_after_twelve_months() {
        database_test!(|db| async move {
            let now = 400 * 24 * 60 * 60 * 1000_i64;
            let cutoff = now - SUPPORT_KEEP_CLOSED_MS;
            let tickets = [
                ticket("old-closed", SupportTicketStatus::Closed, Some(cutoff - 1)),
                ticket("new-closed", SupportTicketStatus::Closed, Some(cutoff + 1)),
                ticket("old-open", SupportTicketStatus::Open, None),
                ticket("old-answered", SupportTicketStatus::Answered, None),
            ];
            for t in &tickets {
                db.insert_support_ticket(t).await.unwrap();
            }

            assert_eq!(db.delete_support_tickets_closed_before(cutoff).await.unwrap(), 1);
            assert!(db.fetch_support_ticket("old-closed").await.is_err());
            for id in ["new-closed", "old-open", "old-answered"] {
                assert!(db.fetch_support_ticket(id).await.is_ok(), "{id}");
            }
            assert_eq!(db.delete_support_tickets_closed_before(cutoff).await.unwrap(), 0);
        });
    }
}
