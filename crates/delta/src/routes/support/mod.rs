//! Help from the Zrzur team: support tickets and notices.
//!
//! Signed-in people open tickets about their account, billing, safety, a
//! bug, a feature idea or anything else, and follow the answers here. The
//! support team answers from the admin console (`/staff/support`). Staff
//! notes marked internal never leave the console.
//!
//! Notices are short messages from the team (to one person or everyone),
//! shown in the app until dismissed.
//!
//! People who can't sign in at all write to support@zrzur.com.

use std::collections::HashMap;

use revolt_database::{
    Database, PlatformNotice, SupportTicket, SupportTicketMessage, SupportTicketQuery,
    SupportTicketStatus, User, SUPPORT_BODY_MAX, SUPPORT_CATEGORIES, SUPPORT_MESSAGES_MAX,
    SUPPORT_OPEN_MAX, SUPPORT_REOPEN_MS, SUPPORT_SUBJECT_MAX,
};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::{serde::json::Json, Route, State};
use rocket_empty::EmptyResponse;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        my_tickets,
        open_ticket,
        ticket,
        reply,
        close,
        reopen,
        notices,
        dismiss,
        set_locale
    ]
}

/// # Language
#[derive(Deserialize, JsonSchema, Debug)]
pub struct DataLocale {
    /// e.g. `ar`, `de` or `pt-BR`
    pub locale: String,
}

/// # Set the language you use the app in
///
/// Messages from Zrzur (support answers, decisions about your account)
/// are written in it when we have it, in English otherwise. Never shown to
/// other people.
#[openapi(tag = "Support")]
#[put("/locale", data = "<data>")]
pub async fn set_locale(db: &State<Database>, user: User, data: Json<DataLocale>) -> Result<EmptyResponse> {
    no_bots(&user)?;
    let locale = data.locale.trim();
    let valid = (2..=16).contains(&locale.len())
        && locale.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && locale.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    if !valid {
        return Err(invalid("unknown language"));
    }
    if user.locale.as_deref() != Some(locale) {
        db.update_user(
            &user.id,
            &revolt_database::PartialUser {
                locale: Some(locale.to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await?;
    }
    Ok(EmptyResponse)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn invalid(error: &str) -> revolt_result::Error {
    create_error!(FailedValidation {
        error: error.to_string()
    })
}

/// Trimmed text of 1 to `max` characters
pub(crate) fn text(value: &str, max: usize, what: &str) -> Result<String> {
    let value = value.trim();
    let length = value.chars().count();
    if length == 0 || length > max {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length(what, 1, max),
        ]));
    }
    Ok(value.to_string())
}

pub(crate) fn status_name(status: SupportTicketStatus) -> &'static str {
    match status {
        SupportTicketStatus::Open => "Open",
        SupportTicketStatus::Answered => "Answered",
        SupportTicketStatus::Closed => "Closed",
    }
}

// ---------------------------------------------------------------- tickets

/// # A message in your ticket
#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct TicketMessageView {
    pub id: String,
    /// From the support team (their name is not shown)
    pub staff: bool,
    pub body: String,
    pub created_at: i64,
}

/// # Your support ticket
#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct TicketView {
    #[serde(rename = "_id")]
    pub id: String,
    pub category: String,
    pub subject: String,
    /// Open (waiting for the team), Answered (waiting for you) or Closed
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related: Option<String>,
    pub messages: Vec<TicketMessageView>,
    pub created_at: i64,
    pub updated_at: i64,
    /// You can still write or reopen it
    pub can_reply: bool,
}

pub(crate) fn can_reply(ticket: &SupportTicket, now: i64) -> bool {
    ticket.messages.len() < SUPPORT_MESSAGES_MAX
        && (ticket.status != SupportTicketStatus::Closed
            || ticket.closed_at.is_some_and(|c| now - c < SUPPORT_REOPEN_MS))
}

fn view(ticket: &SupportTicket, now: i64) -> TicketView {
    TicketView {
        id: ticket.id.clone(),
        category: ticket.category.clone(),
        subject: ticket.subject.clone(),
        status: status_name(ticket.status).to_string(),
        related: ticket.related.clone(),
        // staff notes never leave the console
        messages: ticket
            .messages
            .iter()
            .filter(|m| !m.internal)
            .map(|m| TicketMessageView {
                id: m.id.clone(),
                staff: m.staff,
                body: m.body.clone(),
                created_at: m.created_at,
            })
            .collect(),
        created_at: ticket.created_at,
        updated_at: ticket.updated_at,
        can_reply: can_reply(ticket, now),
    }
}

fn no_bots(user: &User) -> Result<()> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    Ok(())
}

/// The ticket, if it is yours (anyone else gets a 404)
async fn own_ticket(db: &Database, user: &User, id: &str) -> Result<SupportTicket> {
    let ticket = db.fetch_support_ticket(id).await?;
    if ticket.user_id != user.id {
        return Err(create_error!(NotFound));
    }
    Ok(ticket)
}

/// Save, or tell the caller someone changed it first
pub(crate) async fn save(db: &Database, ticket: &SupportTicket, before: i64) -> Result<()> {
    if !db.update_support_ticket(ticket, before).await? {
        return Err(create_error!(CommunityItemConflict));
    }
    Ok(())
}

/// # Your tickets
///
/// Most recently updated first.
#[openapi(tag = "Support")]
#[get("/tickets")]
pub async fn my_tickets(db: &State<Database>, user: User) -> Result<Json<Vec<TicketView>>> {
    no_bots(&user)?;
    let now = now_ms();
    Ok(Json(
        db.fetch_support_tickets(SupportTicketQuery {
            user_id: Some(&user.id),
            limit: 50,
            ..Default::default()
        })
        .await?
        .iter()
        .map(|t| view(t, now))
        .collect(),
    ))
}

/// # New ticket
#[derive(Deserialize, JsonSchema, Debug)]
pub struct DataOpenTicket {
    /// account, billing, safety, bug, feature or other
    pub category: String,
    pub subject: String,
    pub body: String,
    /// What it is about, e.g. an order or server id
    #[serde(default)]
    pub related: Option<String>,
}

/// # Open a ticket
///
/// Up to three open tickets at a time.
#[openapi(tag = "Support")]
#[post("/tickets", data = "<data>")]
pub async fn open_ticket(db: &State<Database>, user: User, data: Json<DataOpenTicket>) -> Result<Json<TicketView>> {
    no_bots(&user)?;
    let data = data.into_inner();
    if !SUPPORT_CATEGORIES.contains(&data.category.as_str()) {
        return Err(invalid("unknown category"));
    }
    let subject = text(&data.subject, SUPPORT_SUBJECT_MAX, "subject")?;
    let body = text(&data.body, SUPPORT_BODY_MAX, "body")?;
    let related = match data.related.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) if r.len() <= 64 && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => Some(r.to_string()),
        Some(_) => return Err(invalid("related must be an id")),
        None => None,
    };
    let open = db
        .fetch_support_tickets(SupportTicketQuery {
            user_id: Some(&user.id),
            limit: 50,
            ..Default::default()
        })
        .await?
        .iter()
        .filter(|t| t.status != SupportTicketStatus::Closed)
        .count();
    if open >= SUPPORT_OPEN_MAX {
        return Err(invalid("you have three open tickets; wait for an answer or close one"));
    }
    let now = now_ms();
    let ticket = SupportTicket {
        id: Ulid::new().to_string(),
        user_id: user.id.clone(),
        category: data.category,
        subject,
        status: SupportTicketStatus::Open,
        assigned_to: None,
        related,
        messages: vec![SupportTicketMessage {
            id: Ulid::new().to_string(),
            author_id: user.id.clone(),
            staff: false,
            internal: false,
            body,
            created_at: now,
        }],
        created_at: now,
        updated_at: now,
        closed_at: None,
    };
    db.insert_support_ticket(&ticket).await?;
    Ok(Json(view(&ticket, now)))
}

/// # One of your tickets
#[openapi(tag = "Support")]
#[get("/tickets/<id>")]
pub async fn ticket(db: &State<Database>, user: User, id: &str) -> Result<Json<TicketView>> {
    no_bots(&user)?;
    Ok(Json(view(&own_ticket(db, &user, id).await?, now_ms())))
}

/// # Message
#[derive(Deserialize, JsonSchema, Debug)]
pub struct DataTicketMessage {
    pub body: String,
}

/// # Write in your ticket
///
/// A closed ticket opens again (within a week of closing).
#[openapi(tag = "Support")]
#[post("/tickets/<id>/messages", data = "<data>")]
pub async fn reply(db: &State<Database>, user: User, id: &str, data: Json<DataTicketMessage>) -> Result<Json<TicketView>> {
    no_bots(&user)?;
    let body = text(&data.body, SUPPORT_BODY_MAX, "body")?;
    let mut ticket = own_ticket(db, &user, id).await?;
    let now = now_ms().max(ticket.updated_at + 1);
    if !can_reply(&ticket, now) {
        return Err(create_error!(InvalidOperation));
    }
    let before = ticket.updated_at;
    ticket.messages.push(SupportTicketMessage {
        id: Ulid::new().to_string(),
        author_id: user.id.clone(),
        staff: false,
        internal: false,
        body,
        created_at: now,
    });
    ticket.status = SupportTicketStatus::Open;
    ticket.closed_at = None;
    ticket.updated_at = now;
    save(db, &ticket, before).await?;
    Ok(Json(view(&ticket, now)))
}

/// # Close your ticket
#[openapi(tag = "Support")]
#[post("/tickets/<id>/close")]
pub async fn close(db: &State<Database>, user: User, id: &str) -> Result<Json<TicketView>> {
    no_bots(&user)?;
    let mut ticket = own_ticket(db, &user, id).await?;
    if ticket.status == SupportTicketStatus::Closed {
        return Err(create_error!(InvalidOperation));
    }
    let before = ticket.updated_at;
    let now = now_ms().max(before + 1);
    ticket.status = SupportTicketStatus::Closed;
    ticket.closed_at = Some(now);
    ticket.updated_at = now;
    save(db, &ticket, before).await?;
    Ok(Json(view(&ticket, now)))
}

/// # Reopen your ticket
///
/// Within a week of closing.
#[openapi(tag = "Support")]
#[post("/tickets/<id>/reopen")]
pub async fn reopen(db: &State<Database>, user: User, id: &str) -> Result<Json<TicketView>> {
    no_bots(&user)?;
    let mut ticket = own_ticket(db, &user, id).await?;
    let before = ticket.updated_at;
    let now = now_ms().max(before + 1);
    if ticket.status != SupportTicketStatus::Closed || !can_reply(&ticket, now) {
        return Err(create_error!(InvalidOperation));
    }
    ticket.status = SupportTicketStatus::Open;
    ticket.closed_at = None;
    ticket.updated_at = now;
    save(db, &ticket, before).await?;
    Ok(Json(view(&ticket, now)))
}

// ---------------------------------------------------------------- notices

/// # A message from the Zrzur team
#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct NoticeView {
    #[serde(rename = "_id")]
    pub id: String,
    /// By language code (`ar`, `en`, `de`)
    pub title: HashMap<String, String>,
    pub body: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// info, warning or important
    pub tone: String,
    /// Sent to you only (not to everyone)
    pub personal: bool,
    pub created_at: i64,
}

impl From<PlatformNotice> for NoticeView {
    fn from(n: PlatformNotice) -> Self {
        NoticeView {
            personal: n.user_id.is_some(),
            id: n.id,
            title: n.title,
            body: n.body,
            link: n.link,
            tone: n.tone,
            created_at: n.created_at,
        }
    }
}

/// # Notices for you
///
/// Newest first, without the ones you dismissed.
#[openapi(tag = "Support")]
#[get("/notices")]
pub async fn notices(db: &State<Database>, user: User) -> Result<Json<Vec<NoticeView>>> {
    no_bots(&user)?;
    Ok(Json(
        db.fetch_platform_notices_for(&user.id, now_ms())
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// # Dismiss a notice
#[openapi(tag = "Support")]
#[post("/notices/<id>/dismiss")]
pub async fn dismiss(db: &State<Database>, user: User, id: &str) -> Result<EmptyResponse> {
    no_bots(&user)?;
    let notice = db.fetch_platform_notice(id).await?;
    if notice.user_id.as_deref().is_some_and(|u| u != user.id) {
        return Err(create_error!(NotFound));
    }
    db.dismiss_platform_notice(id, &user.id).await?;
    Ok(EmptyResponse)
}
