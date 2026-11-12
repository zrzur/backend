use mongodb::options::FindOptions;
use revolt_result::Result;

use crate::{MongoDb, SupportTicket, SupportTicketStatus};

use super::{AbstractSupportTicket, SupportTicketQuery};

static COL: &str = "support_tickets";

fn status_name(status: SupportTicketStatus) -> &'static str {
    match status {
        SupportTicketStatus::Open => "Open",
        SupportTicketStatus::Answered => "Answered",
        SupportTicketStatus::Closed => "Closed",
    }
}

#[async_trait]
impl AbstractSupportTicket for MongoDb {
    async fn insert_support_ticket(&self, ticket: &SupportTicket) -> Result<()> {
        query!(self, insert_one, COL, &ticket).map(|_| ())
    }

    async fn fetch_support_ticket(&self, id: &str) -> Result<SupportTicket> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn update_support_ticket(&self, ticket: &SupportTicket, updated_at: i64) -> Result<bool> {
        // two people answering at once: only the first counts
        self.col::<SupportTicket>(COL)
            .replace_one(doc! { "_id": &ticket.id, "updated_at": updated_at }, ticket)
            .await
            .map(|r| r.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", COL))
    }

    async fn delete_support_tickets_closed_before(&self, before: i64) -> Result<u64> {
        self.col::<SupportTicket>(COL)
            .delete_many(doc! {
                "status": status_name(SupportTicketStatus::Closed),
                "closed_at": { "$lt": before },
            })
            .await
            .map(|r| r.deleted_count)
            .map_err(|_| create_database_error!("delete_many", COL))
    }

    async fn fetch_support_tickets(&self, query: SupportTicketQuery<'_>) -> Result<Vec<SupportTicket>> {
        let mut filter = doc! {};
        if let Some(status) = query.status {
            filter.insert("status", status_name(status));
        }
        if let Some(assigned) = query.assigned_to {
            filter.insert("assigned_to", assigned);
        } else if query.unassigned {
            filter.insert("assigned_to", doc! { "$exists": false });
        }
        if let Some(user) = query.user_id {
            filter.insert("user_id", user);
        }
        if let Some(category) = query.category {
            filter.insert("category", category);
        }
        if let Some(before) = query.before {
            filter.insert("updated_at", doc! { "$lt": before });
        }
        self.find_with_options(
            COL,
            filter,
            FindOptions::builder()
                .sort(doc! { "updated_at": -1 })
                .limit(query.limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }
}
