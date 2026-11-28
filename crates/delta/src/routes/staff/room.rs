//! The desk bot's review rooms: what is waiting, for its cards.
//!
//! The desk bot only (404 for anyone else, and when the desk is off).
//! Reading only, and only what the card shows: a press on a card is handed
//! back to `/staff/desk/act`, where the presser's own staff grant and desk
//! session decide, and the desk's confirmation card comes first.
use revolt_database::{
    CommunityItem, CommunityItemQuery, CommunityItemStatus, Database, SupportTicket,
    SupportTicketQuery, SupportTicketStatus,
};
use revolt_result::Result;
use rocket::{State, serde::json::Json};
use serde::Serialize;

/// Longest text of a card
const TEXT_MAX: usize = 1500;

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn name_of(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

async fn username(db: &Database, id: &str) -> String {
    db.fetch_user(id)
        .await
        .map(|u| u.display_name.unwrap_or(u.username))
        .unwrap_or_default()
}

/// A ticket waiting for the support team
#[derive(Serialize, Debug, PartialEq)]
pub struct RoomTicket {
    pub id: String,
    pub user_id: String,
    pub username: String,
    pub category: String,
    pub subject: String,
    /// The person's last message (staff-only notes never)
    pub last: String,
    /// How many messages the ticket has, notes left out
    pub messages: usize,
    pub assigned_to: Option<String>,
    pub related: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub(crate) fn ticket_card(ticket: &SupportTicket, username: String) -> RoomTicket {
    let shown: Vec<_> = ticket.messages.iter().filter(|m| !m.internal).collect();
    let last = shown
        .iter()
        .rev()
        .find(|m| !m.staff)
        .map(|m| cut(&m.body, TEXT_MAX))
        .unwrap_or_default();
    RoomTicket {
        id: ticket.id.clone(),
        user_id: ticket.user_id.clone(),
        username,
        category: ticket.category.clone(),
        subject: cut(&ticket.subject, 120),
        last,
        messages: shown.len(),
        assigned_to: ticket.assigned_to.clone(),
        related: ticket.related.clone(),
        created_at: ticket.created_at,
        updated_at: ticket.updated_at,
    }
}

/// # Zrzur Desk: support tickets for the support room
///
/// The open tickets (waiting for the team), oldest first, at most 50.
#[get("/desk/room/tickets")]
pub async fn room_tickets(
    db: &State<Database>,
    user: revolt_database::User,
) -> Result<Json<Vec<RoomTicket>>> {
    super::desk::settings(&user).await?;
    let mut tickets = db
        .fetch_support_tickets(SupportTicketQuery {
            status: Some(SupportTicketStatus::Open),
            limit: 50,
            ..Default::default()
        })
        .await?;
    tickets.sort_by_key(|t| t.updated_at);
    let mut out = Vec::with_capacity(tickets.len());
    for ticket in &tickets {
        let name = username(db, &ticket.user_id).await;
        out.push(ticket_card(ticket, name));
    }
    Ok(Json(out))
}

/// A design waiting for a reviewer: a new one, or a new version of one
/// in the gallery
#[derive(Serialize, Debug, PartialEq)]
pub struct RoomDesign {
    pub id: String,
    /// Decoration, Nameplate, Effect or Frame
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub creator_id: String,
    pub creator: String,
    /// The picture to review, on the file server
    pub file_id: String,
    pub file_tag: String,
    /// A new version of a published design
    pub revision: bool,
    /// Changes on every review step: a card for an older one is replaced
    pub version: i64,
}

pub(crate) fn design_card(item: &CommunityItem, creator: String) -> RoomDesign {
    let (name, description, tags, file, revision) = match &item.pending {
        Some(next) if item.status == CommunityItemStatus::Published => {
            (&next.name, &next.description, &next.tags, &next.file, true)
        }
        _ => (&item.name, &item.description, &item.tags, &item.file, false),
    };
    RoomDesign {
        id: item.id.clone(),
        kind: name_of(&item.kind),
        name: cut(name, 100),
        description: description.as_deref().map(|d| cut(d, 600)),
        tags: tags.clone(),
        creator_id: item.creator_id.clone(),
        creator,
        file_id: file.id.clone(),
        file_tag: file.tag.clone(),
        revision,
        version: item.version,
    }
}

/// # Zrzur Desk: designs for the review room
///
/// New designs waiting for review, then new versions of published ones;
/// oldest first, at most 50 of each.
#[get("/desk/room/designs")]
pub async fn room_designs(
    db: &State<Database>,
    user: revolt_database::User,
) -> Result<Json<Vec<RoomDesign>>> {
    super::desk::settings(&user).await?;
    let fetch = |statuses: Vec<CommunityItemStatus>, pending_revision: bool| {
        db.fetch_community_items(CommunityItemQuery {
            statuses,
            pending_revision,
            limit: 50,
            ..Default::default()
        })
    };
    let mut waiting = fetch(vec![CommunityItemStatus::PendingReview], false).await?;
    waiting.reverse();
    let revisions = fetch(vec![CommunityItemStatus::Published], true).await?;
    let mut out = Vec::new();
    for item in waiting.iter().chain(revisions.iter().rev()) {
        // a published design whose new version isn't sent for review yet
        if item.status == CommunityItemStatus::Published
            && item.pending.as_ref().map(|p| &p.status) != Some(&CommunityItemStatus::PendingReview)
        {
            continue;
        }
        let creator = username(db, &item.creator_id).await;
        out.push(design_card(item, creator));
    }
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_database::SupportTicketMessage;

    fn message(id: &str, staff: bool, internal: bool, body: &str) -> SupportTicketMessage {
        SupportTicketMessage {
            id: id.to_string(),
            author_id: "U".to_string(),
            staff,
            internal,
            body: body.to_string(),
            created_at: 1,
        }
    }

    #[test]
    fn a_ticket_card_shows_the_persons_last_words_and_never_a_staff_note() {
        let ticket = SupportTicket {
            id: "T1".to_string(),
            user_id: "U1".to_string(),
            category: "account".to_string(),
            subject: "Can't log in".to_string(),
            status: SupportTicketStatus::Open,
            assigned_to: None,
            related: None,
            messages: vec![
                message("1", false, false, "first"),
                message("2", true, false, "we answered"),
                message("3", false, false, "still broken"),
                message("4", true, true, "secret staff note"),
            ],
            created_at: 1,
            updated_at: 2,
            closed_at: None,
        };
        let card = ticket_card(&ticket, "maher".to_string());
        assert_eq!(card.last, "still broken");
        assert_eq!(card.messages, 3);
        assert!(!serde_json::to_string(&card).unwrap().contains("secret"));
    }

    #[test]
    fn long_text_is_cut() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
    }
}
