use revolt_result::Result;

use crate::{ReferenceDb, SupportTicket, SupportTicketStatus};

use super::{AbstractSupportTicket, SupportTicketQuery};

#[async_trait]
impl AbstractSupportTicket for ReferenceDb {
    async fn insert_support_ticket(&self, ticket: &SupportTicket) -> Result<()> {
        self.support_tickets
            .lock()
            .await
            .insert(ticket.id.clone(), ticket.clone());
        Ok(())
    }

    async fn fetch_support_ticket(&self, id: &str) -> Result<SupportTicket> {
        self.support_tickets
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn update_support_ticket(&self, ticket: &SupportTicket, updated_at: i64) -> Result<bool> {
        let mut store = self.support_tickets.lock().await;
        match store.get(&ticket.id) {
            Some(current) if current.updated_at == updated_at => {
                store.insert(ticket.id.clone(), ticket.clone());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete_support_tickets_closed_before(&self, before: i64) -> Result<u64> {
        let mut store = self.support_tickets.lock().await;
        let count = store.len();
        store.retain(|_, t| {
            !(t.status == SupportTicketStatus::Closed && t.closed_at.is_some_and(|c| c < before))
        });
        Ok((count - store.len()) as u64)
    }

    async fn fetch_support_tickets(&self, query: SupportTicketQuery<'_>) -> Result<Vec<SupportTicket>> {
        let mut list: Vec<_> = self
            .support_tickets
            .lock()
            .await
            .values()
            .filter(|t| query.status.is_none_or(|s| t.status == s))
            .filter(|t| query.assigned_to.is_none_or(|a| t.assigned_to.as_deref() == Some(a)))
            .filter(|t| !query.unassigned || t.assigned_to.is_none())
            .filter(|t| query.user_id.is_none_or(|u| t.user_id == u))
            .filter(|t| query.category.is_none_or(|c| t.category == c))
            .filter(|t| query.before.is_none_or(|b| t.updated_at < b))
            .cloned()
            .collect();
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        list.truncate(query.limit);
        Ok(list)
    }
}
