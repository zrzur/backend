use revolt_result::Result;

use crate::{SupportTicket, SupportTicketStatus};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

/// Filter for the support queue
#[derive(Default)]
pub struct SupportTicketQuery<'a> {
    pub status: Option<SupportTicketStatus>,
    pub assigned_to: Option<&'a str>,
    /// Only tickets nobody looks after
    pub unassigned: bool,
    pub user_id: Option<&'a str>,
    pub category: Option<&'a str>,
    /// Updated before this (paging)
    pub before: Option<i64>,
    pub limit: usize,
}

#[async_trait]
pub trait AbstractSupportTicket: Sync + Send {
    async fn insert_support_ticket(&self, ticket: &SupportTicket) -> Result<()>;

    async fn fetch_support_ticket(&self, id: &str) -> Result<SupportTicket>;

    /// Replace a ticket if nobody changed it since `updated_at`; false
    /// otherwise
    async fn update_support_ticket(&self, ticket: &SupportTicket, updated_at: i64) -> Result<bool>;

    /// Most recently updated first
    async fn fetch_support_tickets(&self, query: SupportTicketQuery<'_>) -> Result<Vec<SupportTicket>>;
}
