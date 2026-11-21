//! Deleting servers and bots from the admin console, for accounts that
//! were created to abuse the platform (for example to grab custom links).
//!
//! - `POST /staff/servers/<id>/actions` with `"action": "delete"`
//! - `POST /staff/bots/<id>/delete`
//! - `POST /staff/users/<id>/delete-owned`: every server and bot of an
//!   account that is already banned or disabled, in one step
//!
//! Gate, the strictest one staff actions have (the same as handing a
//! server over): `platform.servers.restrict`, a fresh step-up, and the
//! Owner role. Every deletion needs a reason and writes one audit record
//! per server or bot before anything is removed. Official servers and bots
//! are never deleted here. Deletion goes through the same code the owner's
//! own delete uses, so members, roles, channels, invites (the custom link
//! included), emojis, webhooks and events go with the server, and a bot
//! leaves its servers and groups and its token stops working.

use revolt_database::{
    record_platform_audit, voice::VoiceClient, AuditOutcome, Bot, Database, Server,
    StaffPermission,
};
use revolt_models::v0::{BotFlags, UserFlags};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};

use super::{reason, StaffContext};
use crate::routes::{bots::delete::remove_bot, servers::server_delete::remove_server};

/// `flags` value of a server run by the Zrzur team
const SERVER_OFFICIAL: i32 = 1;
const BOT_OFFICIAL: i32 = BotFlags::Official as i32;
const BANNED: i32 = UserFlags::Banned as i32;
const SUSPENDED: i32 = UserFlags::SuspendedUntil as i32;

pub(super) const PERMISSION: StaffPermission = StaffPermission::ServersRestrict;

pub(super) fn is_official_server(server: &Server) -> bool {
    server.flags.is_some_and(|f| f & SERVER_OFFICIAL != 0)
}

pub(super) fn is_official_bot(bot: &Bot) -> bool {
    bot.flags.is_some_and(|f| f & BOT_OFFICIAL != 0)
}

/// Permission, step-up and the Owner role. A staff member without the
/// Owner role is refused and the attempt is written to the audit log.
pub(super) async fn require_delete_gate(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    target_kind: &str,
    target_id: &str,
) -> Result<()> {
    staff.require_step_up(db, PERMISSION).await?;
    if staff.grant.is_owner(staff.now) {
        return Ok(());
    }
    let mut record = staff.record(action, Some(PERMISSION));
    record.target_kind = Some(target_kind.to_string());
    record.target_id = Some(target_id.to_string());
    record.summary = Some("owner role required".to_string());
    record_platform_audit(db, record, AuditOutcome::Denied).await?;
    Err(create_error!(NotPrivileged))
}

/// Delete one server, audited first. An official server is refused.
pub(super) async fn delete_server(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    server: Server,
    why: &str,
    context: Option<&str>,
) -> Result<()> {
    let mut record = staff.record("staff.server.delete", Some(PERMISSION));
    record.target_kind = Some("server".to_string());
    record.target_id = Some(server.id.clone());
    record.reason = Some(why.to_string());
    let mut summary = format!("owner={} name={}", server.owner, server.name);
    if let Some(vanity) = &server.vanity_invite {
        summary.push_str(&format!(" link={}", vanity.code));
    }
    if let Some(context) = context {
        summary.push_str(&format!(" ({context})"));
    }
    record.summary = Some(summary.chars().take(300).collect());
    if is_official_server(&server) {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    // fail closed: the record comes first
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    if let Err(error) = remove_server(db, voice_client, server).await {
        let mut failed = record;
        failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
        return Err(error);
    }
    Ok(())
}

/// Delete one bot, audited first. An official bot is refused.
pub(super) async fn delete_bot(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    bot: &Bot,
    why: &str,
    context: Option<&str>,
) -> Result<()> {
    let name = db.fetch_user(&bot.id).await.map(|u| u.username).unwrap_or_default();
    let mut record = staff.record("staff.bot.delete", Some(PERMISSION));
    record.target_kind = Some("bot".to_string());
    record.target_id = Some(bot.id.clone());
    record.reason = Some(why.to_string());
    let mut summary = format!("owner={} name={name}", bot.owner);
    if let Some(context) = context {
        summary.push_str(&format!(" ({context})"));
    }
    record.summary = Some(summary.chars().take(300).collect());
    if is_official_bot(bot) {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    if let Err(error) = remove_bot(db, voice_client, bot).await {
        let mut failed = record;
        failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
        return Err(error);
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct DataDeleteOwned {
    pub reason: String,
}

#[derive(Serialize)]
pub struct SkippedItem {
    /// `server` or `bot`
    pub kind: String,
    pub id: String,
    pub name: String,
    /// `official`, or the error type when the deletion failed
    pub why: String,
}

#[derive(Serialize)]
pub struct DeleteOwnedResult {
    pub user_id: String,
    pub deleted_servers: Vec<String>,
    pub deleted_bots: Vec<String>,
    pub deleted_server_count: usize,
    pub deleted_bot_count: usize,
    /// Official servers and bots, kept
    pub skipped: Vec<SkippedItem>,
    /// Deletions that failed; the rest went ahead
    pub failed: Vec<SkippedItem>,
}

/// Banned, or switched off by staff (a disabled account that is not on a
/// timed suspension)
pub(super) async fn is_banned_or_disabled(db: &Database, user: &revolt_database::User) -> Result<bool> {
    let flags = user.flags.unwrap_or_default();
    if flags & BANNED != 0 {
        return Ok(true);
    }
    if flags & SUSPENDED != 0 {
        return Ok(false);
    }
    Ok(db.fetch_account(&user.id).await.map(|a| a.disabled).unwrap_or(false))
}

/// # Delete every server and bot an account owns
///
/// Only for an account that is already banned or disabled. Official
/// servers and bots are kept and listed under `skipped`. Bots go first, so
/// they leave the servers before those are deleted.
#[post("/users/<id>/delete-owned", data = "<data>")]
pub async fn delete_owned(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    staff: StaffContext,
    id: &str,
    data: Json<DataDeleteOwned>,
) -> Result<Json<DeleteOwnedResult>> {
    require_delete_gate(db, &staff, "staff.user.delete_owned", "user", id).await?;
    let why = reason(&data.reason)?;
    let user = db.fetch_user(id).await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let mut record = staff.record("staff.user.delete_owned", Some(PERMISSION));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(user.id.clone());
    record.reason = Some(why.clone());
    if !is_banned_or_disabled(db, &user).await? {
        record.summary = Some("account is not banned or disabled".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    }

    let bots = db.fetch_bots_by_user(&user.id).await?;
    let servers = db.fetch_owned_servers(&user.id).await?;
    record.summary = Some(format!("servers={} bots={}", servers.len(), bots.len()));
    record_platform_audit(db, record, AuditOutcome::Success).await?;

    let context = format!("delete_owned {}", user.id);
    let mut result = DeleteOwnedResult {
        user_id: user.id.clone(),
        deleted_servers: vec![],
        deleted_bots: vec![],
        deleted_server_count: 0,
        deleted_bot_count: 0,
        skipped: vec![],
        failed: vec![],
    };
    for bot in bots {
        let name = db.fetch_user(&bot.id).await.map(|u| u.username).unwrap_or_default();
        let item = |why: String| SkippedItem {
            kind: "bot".to_string(),
            id: bot.id.clone(),
            name: name.clone(),
            why,
        };
        if is_official_bot(&bot) {
            result.skipped.push(item("official".to_string()));
            continue;
        }
        match delete_bot(db, voice_client, &staff, &bot, &why, Some(&context)).await {
            Ok(()) => result.deleted_bots.push(bot.id.clone()),
            Err(error) => result.failed.push(item(format!("{:?}", error.error_type))),
        }
    }
    for server in servers {
        let item = SkippedItem {
            kind: "server".to_string(),
            id: server.id.clone(),
            name: server.name.clone(),
            why: String::new(),
        };
        if is_official_server(&server) {
            result.skipped.push(SkippedItem { why: "official".to_string(), ..item });
            continue;
        }
        match delete_server(db, voice_client, &staff, server, &why, Some(&context)).await {
            Ok(()) => result.deleted_servers.push(item.id),
            Err(error) => result.failed.push(SkippedItem {
                why: format!("{:?}", error.error_type),
                ..item
            }),
        }
    }
    result.deleted_server_count = result.deleted_servers.len();
    result.deleted_bot_count = result.deleted_bots.len();
    Ok(Json(result))
}
