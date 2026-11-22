//! Deleting servers and bots on behalf of the platform team, shared by the
//! staff API (`/staff/servers/<id>/actions` `delete`, `/staff/bots/<id>/delete`,
//! `/staff/users/<id>/delete-owned`) and the `zrzur-staff delete-owned`
//! command, so both run the same code and write the same audit records.
//!
//! The deletion itself is the owner's own: [`remove_server`] closes the
//! server's voice rooms and runs `Server::delete` (members, roles,
//! channels, invites and the custom link, emojis, events, `ServerDelete`);
//! [`remove_bot`] runs `Bot::delete` (servers and groups left, commands and
//! installs removed, token refused, `Logout`) and takes the bot out of
//! voice. Who may call these is decided by the caller: the API checks the
//! Owner role and a step-up, the command runs on the server host.

use revolt_models::v0::{BotFlags, UserFlags};
use revolt_result::{create_error, Result};
use serde::Serialize;

use crate::{
    record_platform_audit,
    voice::{
        delete_voice_channel, remove_user_from_voice_channels, UserVoiceChannel, VoiceClient,
    },
    AuditOutcome, AuditRecord, Bot, Database, Server, User,
};

/// `flags` bit of a server run by the Zrzur team
const SERVER_OFFICIAL: i32 = 1;
const BOT_OFFICIAL: i32 = BotFlags::Official as i32;
const BANNED: i32 = UserFlags::Banned as i32;

/// Called with a bot's id once it is deleted (the API drops its cached
/// command list); also after a partial failure
pub type BotForget<'a> = &'a (dyn Fn(&str) + Send + Sync);

pub fn is_official_server(server: &Server) -> bool {
    server.flags.is_some_and(|f| f & SERVER_OFFICIAL != 0)
}

pub fn is_official_bot(bot: &Bot) -> bool {
    bot.flags.is_some_and(|f| f & BOT_OFFICIAL != 0)
}

/// Banned by staff. A disabled account alone is not enough: people
/// disable their own account, and an account waiting for its deletion is
/// disabled too; a staff ban sets the flag and disables the account.
pub fn is_banned(user: &User) -> bool {
    user.flags.unwrap_or_default() & BANNED != 0
}

/// Delete a server the way its owner does
pub async fn remove_server(db: &Database, voice_client: &VoiceClient, server: Server) -> Result<()> {
    for channel_id in &server.channels {
        delete_voice_channel(
            voice_client,
            &UserVoiceChannel {
                id: channel_id.clone(),
                server_id: Some(server.id.clone()),
            },
        )
        .await?;
    }
    server.delete(db).await
}

/// Delete a bot the way its owner does
pub async fn remove_bot(db: &Database, voice_client: &VoiceClient, bot: &Bot, forget: BotForget<'_>) -> Result<()> {
    let deleted = bot.delete(db).await;
    // also after a partial failure: the list must not outlive the bot
    forget(&bot.id);
    deleted?;
    remove_user_from_voice_channels(voice_client, &bot.id).await
}

/// Who acts: the staff member and session, or the command line. `action`,
/// target, reason and summary are filled in here.
fn record_for(actor: &AuditRecord, action: &str, kind: &str, id: &str, why: &str, summary: String) -> AuditRecord {
    AuditRecord {
        actor_id: actor.actor_id.clone(),
        permission: actor.permission,
        staff_session_id: actor.staff_session_id.clone(),
        action: action.to_string(),
        target_kind: Some(kind.to_string()),
        target_id: Some(id.to_string()),
        reason: Some(why.to_string()),
        summary: Some(summary.chars().take(300).collect()),
    }
}

async fn run_audited(db: &Database, record: AuditRecord, work: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    // fail closed: the record comes first
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    if let Err(error) = work.await {
        let mut failed = record;
        failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
        return Err(error);
    }
    Ok(())
}

/// Delete one server, audited first (`staff.server.delete`). An official
/// server is refused (`NotPrivileged`) and the attempt recorded.
pub async fn staff_delete_server(
    db: &Database,
    voice_client: &VoiceClient,
    actor: &AuditRecord,
    server: Server,
    why: &str,
    context: Option<&str>,
) -> Result<()> {
    let mut summary = format!("owner={} name={}", server.owner, server.name);
    if let Some(vanity) = &server.vanity_invite {
        summary.push_str(&format!(" link={}", vanity.code));
    }
    if let Some(context) = context {
        summary.push_str(&format!(" ({context})"));
    }
    let record = record_for(actor, "staff.server.delete", "server", &server.id, why, summary);
    if is_official_server(&server) {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    run_audited(db, record, remove_server(db, voice_client, server)).await
}

/// Delete one bot, audited first (`staff.bot.delete`). An official bot is
/// refused (`NotPrivileged`) and the attempt recorded.
pub async fn staff_delete_bot(
    db: &Database,
    voice_client: &VoiceClient,
    actor: &AuditRecord,
    bot: &Bot,
    why: &str,
    context: Option<&str>,
    forget: BotForget<'_>,
) -> Result<()> {
    let name = db.fetch_user(&bot.id).await.map(|u| u.username).unwrap_or_default();
    let mut summary = format!("owner={} name={name}", bot.owner);
    if let Some(context) = context {
        summary.push_str(&format!(" ({context})"));
    }
    let record = record_for(actor, "staff.bot.delete", "bot", &bot.id, why, summary);
    if is_official_bot(bot) {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    run_audited(db, record, remove_bot(db, voice_client, bot, forget)).await
}

#[derive(Serialize, Debug, Clone)]
pub struct SkippedItem {
    /// `server` or `bot`
    pub kind: String,
    pub id: String,
    pub name: String,
    /// `official`, or the error type when the deletion failed
    pub why: String,
}

#[derive(Serialize, Debug)]
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

pub struct PlannedServer {
    pub server: Server,
    pub members: usize,
}

pub struct PlannedBot {
    pub bot: Bot,
    pub name: String,
}

/// What `staff_delete_owned` would do, read without changing anything
pub struct DeleteOwnedPlan {
    pub servers: Vec<PlannedServer>,
    pub bots: Vec<PlannedBot>,
    /// Official servers and bots, which stay
    pub skipped: Vec<SkippedItem>,
}

pub async fn plan_delete_owned(db: &Database, user: &User) -> Result<DeleteOwnedPlan> {
    let mut plan = DeleteOwnedPlan {
        servers: vec![],
        bots: vec![],
        skipped: vec![],
    };
    for bot in db.fetch_bots_by_user(&user.id).await? {
        let name = db.fetch_user(&bot.id).await.map(|u| u.username).unwrap_or_default();
        if is_official_bot(&bot) {
            plan.skipped.push(SkippedItem {
                kind: "bot".into(),
                id: bot.id.clone(),
                name,
                why: "official".into(),
            });
        } else {
            plan.bots.push(PlannedBot { bot, name });
        }
    }
    for server in db.fetch_owned_servers(&user.id).await? {
        if is_official_server(&server) {
            plan.skipped.push(SkippedItem {
                kind: "server".into(),
                id: server.id.clone(),
                name: server.name.clone(),
                why: "official".into(),
            });
        } else {
            let members = db.fetch_member_count(&server.id).await.unwrap_or(0);
            plan.servers.push(PlannedServer { server, members });
        }
    }
    Ok(plan)
}

/// Delete every server and bot of an account staff banned, bots first so
/// they leave the servers before those go. A non-banned account is
/// refused (`InvalidOperation`) and the attempt recorded. Writes one
/// `staff.user.delete_owned` record, then one record per server and bot.
pub async fn staff_delete_owned(
    db: &Database,
    voice_client: &VoiceClient,
    actor: &AuditRecord,
    user: &User,
    why: &str,
    forget: BotForget<'_>,
) -> Result<DeleteOwnedResult> {
    let mut record = record_for(actor, "staff.user.delete_owned", "user", &user.id, why, String::new());
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if !is_banned(user) {
        record.summary = Some("account is not banned".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    }

    let plan = plan_delete_owned(db, user).await?;
    record.summary = Some(format!(
        "servers={} bots={} official={}",
        plan.servers.len(),
        plan.bots.len(),
        plan.skipped.len()
    ));
    record_platform_audit(db, record, AuditOutcome::Success).await?;

    let context = format!("delete_owned {}", user.id);
    let mut result = DeleteOwnedResult {
        user_id: user.id.clone(),
        deleted_servers: vec![],
        deleted_bots: vec![],
        deleted_server_count: 0,
        deleted_bot_count: 0,
        skipped: plan.skipped,
        failed: vec![],
    };
    for PlannedBot { bot, name } in plan.bots {
        match staff_delete_bot(db, voice_client, actor, &bot, why, Some(&context), forget).await {
            Ok(()) => result.deleted_bots.push(bot.id.clone()),
            Err(error) => result.failed.push(SkippedItem {
                kind: "bot".into(),
                id: bot.id.clone(),
                name,
                why: format!("{:?}", error.error_type),
            }),
        }
    }
    for PlannedServer { server, .. } in plan.servers {
        let (id, name) = (server.id.clone(), server.name.clone());
        match staff_delete_server(db, voice_client, actor, server, why, Some(&context)).await {
            Ok(()) => result.deleted_servers.push(id),
            Err(error) => result.failed.push(SkippedItem {
                kind: "server".into(),
                id,
                name,
                why: format!("{:?}", error.error_type),
            }),
        }
    }
    result.deleted_server_count = result.deleted_servers.len();
    result.deleted_bot_count = result.deleted_bots.len();
    Ok(result)
}
