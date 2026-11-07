//! Support tickets and notices in the admin console.
//!
//! Tickets:
//! - `platform.support.read`: the queue and every ticket, staff notes
//!   included
//! - `platform.support.manage`: answer, write a staff note, take or hand
//!   over a ticket, close or reopen it
//!
//! Notices (messages from the team shown in the app until dismissed):
//! - `platform.support.read`: see sent notices
//! - to one person: `platform.support.manage` + a fresh step-up; arrives in
//!   their private messages from Zrzur's official account, optionally by email
//! - to everyone: `platform.settings.manage` + a fresh step-up; in the app
//!   only (no mass email or push from here)
//!
//! Every change is written to the platform audit log first.

use std::collections::HashMap;

use revolt_config::config;
use revolt_database::{
    record_platform_audit,
    util::email::{email_templates, send_email},
    AuditOutcome, AuditRecord, Database, PlatformNotice, StaffPermission, SupportTicket,
    SupportTicketMessage, SupportTicketQuery, SupportTicketStatus, NOTICE_BODY_MAX,
    NOTICE_TITLE_MAX, SUPPORT_BODY_MAX, SUPPORT_CATEGORIES, SUPPORT_MESSAGES_MAX,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use super::StaffContext;
use crate::routes::support::{save, status_name, text};

const LANGUAGES: [&str; 3] = ["ar", "en", "de"];

fn invalid(error: &str) -> revolt_result::Error {
    create_error!(FailedValidation {
        error: error.to_string()
    })
}

async fn audited<T>(
    db: &Database,
    record: AuditRecord,
    change: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    match change.await {
        Ok(value) => Ok(value),
        Err(error) => {
            let mut failed = record;
            failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
            let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
            Err(error)
        }
    }
}

// ---------------------------------------------------------------- tickets

#[derive(Serialize)]
pub struct StaffTicketMessage {
    pub id: String,
    pub author_id: String,
    pub author: Option<String>,
    pub staff: bool,
    pub internal: bool,
    pub body: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct StaffTicket {
    pub id: String,
    pub user_id: String,
    pub user: Option<String>,
    pub category: String,
    pub subject: String,
    pub status: String,
    pub assigned_to: Option<String>,
    pub assignee: Option<String>,
    pub related: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
    /// Messages people can see (staff notes not counted)
    pub message_count: usize,
    /// The last message came from the person (needs an answer)
    pub waiting: bool,
    /// Only in the single-ticket view
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<StaffTicketMessage>>,
    /// The official message this ticket answers (single-ticket view)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
}

async fn names(db: &Database, mut ids: Vec<String>) -> Result<HashMap<String, String>> {
    ids.sort();
    ids.dedup();
    Ok(db
        .fetch_users(&ids)
        .await?
        .into_iter()
        .map(|u| (u.id, format!("{}#{}", u.username, u.discriminator)))
        .collect())
}

fn staff_view(t: &SupportTicket, names: &HashMap<String, String>, full: bool) -> StaffTicket {
    let visible: Vec<_> = t.messages.iter().filter(|m| !m.internal).collect();
    StaffTicket {
        id: t.id.clone(),
        user_id: t.user_id.clone(),
        user: names.get(&t.user_id).cloned(),
        category: t.category.clone(),
        subject: t.subject.clone(),
        status: status_name(t.status).to_string(),
        assigned_to: t.assigned_to.clone(),
        assignee: t.assigned_to.as_ref().and_then(|a| names.get(a).cloned()),
        related: t.related.clone(),
        created_at: t.created_at,
        updated_at: t.updated_at,
        closed_at: t.closed_at,
        message_count: visible.len(),
        waiting: t.status == SupportTicketStatus::Open && visible.last().is_some_and(|m| !m.staff),
        messages: full.then(|| {
            t.messages
                .iter()
                .map(|m| StaffTicketMessage {
                    id: m.id.clone(),
                    author_id: m.author_id.clone(),
                    author: names.get(&m.author_id).cloned(),
                    staff: m.staff,
                    internal: m.internal,
                    body: m.body.clone(),
                    created_at: m.created_at,
                })
                .collect()
        }),
        in_reply_to: None,
    }
}

async fn full_view(db: &Database, t: &SupportTicket) -> Result<StaffTicket> {
    let mut ids: Vec<String> = t.messages.iter().map(|m| m.author_id.clone()).collect();
    ids.push(t.user_id.clone());
    ids.extend(t.assigned_to.clone());
    let mut view = staff_view(t, &names(db, ids).await?, true);
    // only messages from official accounts are shown here; any other id
    // the person typed stays hidden
    if let Some(related) = &t.related {
        if let Ok(message) = db.fetch_message(related).await {
            if revolt_database::is_official_id(db, &message.author).await {
                view.in_reply_to = message.content;
            }
        }
    }
    Ok(view)
}

fn parse_status(name: &str) -> Result<SupportTicketStatus> {
    match name {
        "Open" => Ok(SupportTicketStatus::Open),
        "Answered" => Ok(SupportTicketStatus::Answered),
        "Closed" => Ok(SupportTicketStatus::Closed),
        _ => Err(create_error!(InvalidOperation)),
    }
}

/// # Support tickets
///
/// Most recently updated first. `assigned` is `me`, `none` or a staff
/// member's id; `before` is an `updated_at` for paging.
#[get("/support/tickets?<status>&<assigned>&<category>&<user>&<before>")]
pub async fn tickets(
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
    assigned: Option<&str>,
    category: Option<&str>,
    user: Option<&str>,
    before: Option<i64>,
) -> Result<Json<Vec<StaffTicket>>> {
    staff.require(db, StaffPermission::SupportRead).await?;
    if category.is_some_and(|c| !SUPPORT_CATEGORIES.contains(&c)) || user.is_some_and(|u| u.len() > 64) || assigned.is_some_and(|a| a.len() > 64) {
        return Err(create_error!(InvalidOperation));
    }
    let status = status.map(parse_status).transpose()?;
    let me = staff.user.id.clone();
    let list = db
        .fetch_support_tickets(SupportTicketQuery {
            status,
            assigned_to: match assigned {
                Some("me") => Some(me.as_str()),
                Some("none") | None => None,
                Some(other) => Some(other),
            },
            unassigned: assigned == Some("none"),
            user_id: user,
            category,
            before,
            limit: 50,
        })
        .await?;
    let mut ids: Vec<String> = list.iter().map(|t| t.user_id.clone()).collect();
    ids.extend(list.iter().filter_map(|t| t.assigned_to.clone()));
    let names = names(db, ids).await?;
    Ok(Json(list.iter().map(|t| staff_view(t, &names, false)).collect()))
}

/// # A support ticket, with staff notes
#[get("/support/tickets/<id>")]
pub async fn ticket(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<StaffTicket>> {
    staff.require(db, StaffPermission::SupportRead).await?;
    let t = db.fetch_support_ticket(id).await?;
    Ok(Json(full_view(db, &t).await?))
}

fn ticket_record(staff: &StaffContext, action: &str, t: &SupportTicket) -> AuditRecord {
    let mut record = staff.record(action, Some(StaffPermission::SupportManage));
    record.target_kind = Some("support_ticket".to_string());
    record.target_id = Some(t.id.clone());
    record
}

#[derive(Deserialize)]
pub struct DataStaffReply {
    pub body: String,
    /// A note for the team only
    #[serde(default)]
    pub internal: bool,
    /// Close the ticket with this answer
    #[serde(default)]
    pub close: bool,
}

/// # Answer a ticket, or add a staff note
///
/// An answer marks the ticket Answered (or Closed with `close`) and tells
/// the person by push; a note changes nothing they can see.
#[post("/support/tickets/<id>/messages", data = "<data>")]
pub async fn reply(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataStaffReply>,
) -> Result<Json<StaffTicket>> {
    staff.require(db, StaffPermission::SupportManage).await?;
    let data = data.into_inner();
    let body = text(&data.body, SUPPORT_BODY_MAX, "the message")?;
    let mut t = db.fetch_support_ticket(id).await?;
    if t.user_id == staff.user.id {
        // nobody answers their own ticket
        return Err(create_error!(NotPrivileged));
    }
    if t.messages.len() >= SUPPORT_MESSAGES_MAX {
        return Err(invalid("this ticket is full; ask the person to open a new one"));
    }
    let before = t.updated_at;
    let now = staff.now.max(before + 1);
    t.messages.push(SupportTicketMessage {
        id: Ulid::new().to_string(),
        author_id: staff.user.id.clone(),
        staff: true,
        internal: data.internal,
        body,
        created_at: now,
    });
    if !data.internal {
        if data.close {
            t.status = SupportTicketStatus::Closed;
            t.closed_at = Some(now);
        } else {
            t.status = SupportTicketStatus::Answered;
            t.closed_at = None;
        }
        // whoever answers first looks after it
        if t.assigned_to.is_none() {
            t.assigned_to = Some(staff.user.id.clone());
        }
    }
    t.updated_at = now;
    let action = if data.internal { "staff.support.note" } else { "staff.support.reply" };
    let mut record = ticket_record(&staff, action, &t);
    record.summary = Some(status_name(t.status).to_string());
    audited(db, record, save(db, &t, before)).await?;
    if !data.internal {
        // the answer arrives in their private messages from Zrzur's official account
        let answer = t.messages.last().map(|m| m.body.clone()).unwrap_or_default();
        crate::util::official::ticket_answered(db, &t.user_id, &t.subject, &answer, data.close).await;
    }
    Ok(Json(full_view(db, &t).await?))
}

#[derive(Deserialize)]
pub struct DataTicketChange {
    /// assign, unassign, close or reopen
    pub action: String,
    /// assign: a staff member's id (yourself when absent)
    #[serde(default)]
    pub to: Option<String>,
}

/// # Take, hand over, close or reopen a ticket
#[post("/support/tickets/<id>", data = "<data>")]
pub async fn change(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataTicketChange>,
) -> Result<Json<StaffTicket>> {
    staff.require(db, StaffPermission::SupportManage).await?;
    let data = data.into_inner();
    let mut t = db.fetch_support_ticket(id).await?;
    let before = t.updated_at;
    let now = staff.now.max(before + 1);
    match data.action.as_str() {
        "assign" => {
            let to = data.to.unwrap_or_else(|| staff.user.id.clone());
            // only to someone who can work on tickets
            let grant = db.fetch_staff_grant(&to).await?.ok_or_else(|| create_error!(NotFound))?;
            if !grant.is_active(now) || !grant.effective(now).contains(&StaffPermission::SupportManage) {
                return Err(invalid("they can't work on support tickets"));
            }
            if to == t.user_id {
                return Err(create_error!(NotPrivileged));
            }
            t.assigned_to = Some(to);
        }
        "unassign" => {
            if t.assigned_to.is_none() {
                return Err(create_error!(InvalidOperation));
            }
            t.assigned_to = None;
        }
        "close" => {
            if t.status == SupportTicketStatus::Closed {
                return Err(create_error!(InvalidOperation));
            }
            t.status = SupportTicketStatus::Closed;
            t.closed_at = Some(now);
        }
        "reopen" => {
            if t.status != SupportTicketStatus::Closed {
                return Err(create_error!(InvalidOperation));
            }
            t.status = SupportTicketStatus::Open;
            t.closed_at = None;
        }
        _ => return Err(create_error!(InvalidOperation)),
    }
    t.updated_at = now;
    let mut record = ticket_record(&staff, "staff.support.change", &t);
    record.summary = Some(match &t.assigned_to {
        Some(to) if data.action == "assign" => format!("assign {to}"),
        _ => data.action.clone(),
    });
    audited(db, record, save(db, &t, before)).await?;
    Ok(Json(full_view(db, &t).await?))
}

// ---------------------------------------------------------------- notices

#[derive(Serialize)]
pub struct StaffNotice {
    #[serde(flatten)]
    pub notice: PlatformNotice,
    pub user: Option<String>,
    pub live: bool,
}

/// # Sent notices
///
/// Newest first; `user` limits it to one person's notices.
#[get("/notices?<user>")]
pub async fn notices(db: &State<Database>, staff: StaffContext, user: Option<&str>) -> Result<Json<Vec<StaffNotice>>> {
    staff.require(db, StaffPermission::SupportRead).await?;
    if user.is_some_and(|u| u.len() > 64) {
        return Err(create_error!(InvalidOperation));
    }
    let list = db.fetch_platform_notices(user, 100).await?;
    let names = names(db, list.iter().filter_map(|n| n.user_id.clone()).collect()).await?;
    Ok(Json(
        list.into_iter()
            .map(|n| StaffNotice {
                user: n.user_id.as_ref().and_then(|u| names.get(u).cloned()),
                live: n.is_live(staff.now),
                notice: n,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct DataNotice {
    /// One person's id; everyone when absent
    #[serde(default)]
    pub user: Option<String>,
    /// By language: ar, en and de
    pub title: HashMap<String, String>,
    pub body: HashMap<String, String>,
    #[serde(default)]
    pub link: Option<String>,
    /// info, warning or important
    pub tone: String,
    /// Stop showing it after this many days (1 to 365)
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    /// One person only: also send it by email
    #[serde(default)]
    pub email: bool,
}

fn languages(values: &HashMap<String, String>, max: usize, what: &str) -> Result<HashMap<String, String>> {
    if values.keys().any(|k| !LANGUAGES.contains(&k.as_str())) {
        return Err(invalid("unknown language"));
    }
    let mut out = HashMap::new();
    for language in LANGUAGES {
        let value = values.get(language).map(String::as_str).unwrap_or_default();
        out.insert(language.to_string(), text(value, max, &format!("{what} ({language})"))?);
    }
    Ok(out)
}

/// # Send a notice
#[post("/notices", data = "<data>")]
pub async fn send(
    db: &State<Database>,
    staff: StaffContext,
    data: Json<DataNotice>,
) -> Result<Json<StaffNotice>> {
    let data = data.into_inner();
    let everyone = data.user.is_none();
    let permission = if everyone { StaffPermission::SettingsManage } else { StaffPermission::SupportManage };
    staff.require_step_up(db, permission).await?;
    let title = languages(&data.title, NOTICE_TITLE_MAX, "the title")?;
    let body = languages(&data.body, NOTICE_BODY_MAX, "the text")?;
    if !matches!(data.tone.as_str(), "info" | "warning" | "important") {
        return Err(invalid("unknown tone"));
    }
    let link = match data.link.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) if l.starts_with("https://") && l.len() <= 300 && !l.chars().any(char::is_whitespace) => Some(l.to_string()),
        Some(_) => return Err(invalid("the link must be an https address")),
        None => None,
    };
    if data.expires_in_days.is_some_and(|d| !(1..=365).contains(&d)) {
        return Err(invalid("expires in 1 to 365 days"));
    }
    if everyone && data.email {
        return Err(invalid("notices to everyone are shown in the app only"));
    }
    let subject = match &data.user {
        Some(id) => {
            let user = db.fetch_user(id).await?;
            if user.bot.is_some() {
                return Err(create_error!(IsBot));
            }
            Some(user)
        }
        None => None,
    };
    let smtp = &config().await.api.smtp;
    if data.email && smtp.host.is_empty() {
        return Err(invalid("email is not set up on this server"));
    }

    let notice = PlatformNotice {
        id: Ulid::new().to_string(),
        user_id: data.user.clone(),
        title,
        body,
        link,
        tone: data.tone.clone(),
        created_by: staff.user.id.clone(),
        created_at: staff.now,
        expires_at: data.expires_in_days.map(|d| staff.now + d as i64 * 24 * 60 * 60 * 1000),
        withdrawn_at: None,
        emailed: false,
        pushed: false,
    };
    let mut record = staff.record("staff.notice.send", Some(permission));
    record.target_kind = Some(if everyone { "platform" } else { "user" }.to_string());
    record.target_id = data.user.clone();
    record.summary = Some(format!(
        "{} {}{}{}",
        notice.id,
        notice.tone,
        if data.email { " +email" } else { "" },
        if everyone { "" } else { " +dm" }
    ));
    audited(db, record, db.insert_platform_notice(&notice)).await?;

    // copies outside the app are best effort; the notice is in the app
    let mut emailed = false;
    let mut pushed = false;
    if let Some(user) = &subject {
        if data.email {
            if let Ok(account) = db.fetch_account(&user.id).await {
                let template = email_templates().await.notice;
                emailed = send_email(
                    smtp,
                    account.email.clone(),
                    &template,
                    serde_json::json!({
                        "title_ar": notice.title["ar"], "title_en": notice.title["en"], "title_de": notice.title["de"],
                        "body_ar": notice.body["ar"], "body_en": notice.body["en"], "body_de": notice.body["de"],
                        "link": notice.link,
                    }),
                )
                .is_ok();
            }
        }
        // a notice to one person arrives in their private messages from
        // Zrzur's official account (with its own push notification); the
        // in-app copy is then not shown again
        crate::util::official::notice(db, &user.id, &notice.title, &notice.body, notice.link.as_deref()).await;
        db.dismiss_platform_notice(&notice.id, &user.id).await?;
        pushed = true;
        db.set_platform_notice_delivery(&notice.id, emailed, pushed).await?;
    }
    Ok(Json(StaffNotice {
        user: subject.map(|u| format!("{}#{}", u.username, u.discriminator)),
        live: true,
        notice: PlatformNotice { emailed, pushed, ..notice },
    }))
}

#[derive(Deserialize)]
pub struct DataWithdrawNotice {
    pub reason: String,
}

/// # Withdraw a notice
///
/// It stops showing in the app (an email already sent stays sent).
#[post("/notices/<id>/withdraw", data = "<data>")]
pub async fn withdraw(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataWithdrawNotice>,
) -> Result<()> {
    let notice = db.fetch_platform_notice(id).await?;
    let permission = if notice.user_id.is_none() { StaffPermission::SettingsManage } else { StaffPermission::SupportManage };
    staff.require_step_up(db, permission).await?;
    let why = super::reason(&data.reason)?;
    if notice.withdrawn_at.is_some() {
        return Err(create_error!(InvalidOperation));
    }
    let mut record = staff.record("staff.notice.withdraw", Some(permission));
    record.target_kind = Some("notice".to_string());
    record.target_id = Some(id.to_string());
    record.reason = Some(why);
    audited(db, record, db.withdraw_platform_notice(id, staff.now)).await
}
