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
//! Owner role and a step-up, the command runs on the server host and asks
//! for the account's name ([`run_delete_owned_command`]).

use std::{
    collections::BTreeSet,
    future::Future,
    io::{BufRead, Write},
    panic::AssertUnwindSafe,
};

use futures::FutureExt;
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
pub type BotForget = fn(&str);

pub fn is_official_server(server: &Server) -> bool {
    server.flags.is_some_and(|f| f & SERVER_OFFICIAL != 0)
}

/// A bot Zrzur runs: the Official flag on the bot, or the official-bot mark
/// on its account (`zrzur-staff official-bot`). `bot_user` is the bot's own
/// account; without it only the flag counts.
pub fn is_official_bot(bot: &Bot, bot_user: Option<&User>) -> bool {
    bot.flags.is_some_and(|f| f & BOT_OFFICIAL != 0) || bot_user.is_some_and(|u| u.official_bot)
}

/// Banned by staff. A disabled account alone is not enough: people
/// disable their own account, and an account waiting for its deletion is
/// disabled too; a staff ban sets the flag and disables the account.
pub fn is_banned(user: &User) -> bool {
    user.flags.unwrap_or_default() & BANNED != 0
}

/// A reason of 3 to 500 characters, trimmed
pub fn check_reason(why: &str) -> Result<String> {
    let why = why.trim();
    if (3..=500).contains(&why.chars().count()) {
        Ok(why.to_string())
    } else {
        Err(create_error!(InvalidOperation))
    }
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
pub async fn remove_bot(db: &Database, voice_client: &VoiceClient, bot: &Bot, forget: BotForget) -> Result<()> {
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

async fn run_audited(db: &Database, record: AuditRecord, work: impl Future<Output = Result<()>>) -> Result<()> {
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
    let why = check_reason(why)?;
    let mut summary = format!("owner={} name={}", server.owner, server.name);
    if let Some(vanity) = &server.vanity_invite {
        summary.push_str(&format!(" link={}", vanity.code));
    }
    if let Some(context) = context {
        summary.push_str(&format!(" ({context})"));
    }
    let record = record_for(actor, "staff.server.delete", "server", &server.id, &why, summary);
    if is_official_server(&server) {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    run_audited(db, record, remove_server(db, voice_client, server)).await
}

/// Delete one bot, audited first (`staff.bot.delete`). An official bot
/// (flag or account mark) is refused (`NotPrivileged`) and the attempt
/// recorded.
pub async fn staff_delete_bot(
    db: &Database,
    voice_client: &VoiceClient,
    actor: &AuditRecord,
    bot: &Bot,
    why: &str,
    context: Option<&str>,
    forget: BotForget,
) -> Result<()> {
    let why = check_reason(why)?;
    let bot_user = db.fetch_user(&bot.id).await.ok();
    let name = bot_user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
    let mut summary = format!("owner={} name={name}", bot.owner);
    if let Some(context) = context {
        summary.push_str(&format!(" ({context})"));
    }
    let record = record_for(actor, "staff.bot.delete", "bot", &bot.id, &why, summary);
    if is_official_bot(bot, bot_user.as_ref()) {
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

/// The servers and bots a plan would delete, by id
#[derive(PartialEq, Eq, Debug, Clone)]
pub struct PlanIds {
    pub servers: BTreeSet<String>,
    pub bots: BTreeSet<String>,
}

impl DeleteOwnedPlan {
    pub fn ids(&self) -> PlanIds {
        PlanIds {
            servers: self.servers.iter().map(|s| s.server.id.clone()).collect(),
            bots: self.bots.iter().map(|b| b.bot.id.clone()).collect(),
        }
    }
}

pub async fn plan_delete_owned(db: &Database, user: &User) -> Result<DeleteOwnedPlan> {
    let mut plan = DeleteOwnedPlan {
        servers: vec![],
        bots: vec![],
        skipped: vec![],
    };
    for bot in db.fetch_bots_by_user(&user.id).await? {
        let bot_user = db.fetch_user(&bot.id).await.ok();
        let name = bot_user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
        if is_official_bot(&bot, bot_user.as_ref()) {
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

/// Record a refused `delete-owned` (an account that is not banned, or a
/// plan that changed after it was shown)
pub async fn record_delete_owned_refusal(db: &Database, actor: &AuditRecord, user_id: &str, why: &str, summary: &str) -> Result<()> {
    let record = record_for(actor, "staff.user.delete_owned", "user", user_id, why, summary.to_string());
    record_platform_audit(db, record, AuditOutcome::Denied).await.map(|_| ())
}

/// One item's deletion; a panic inside it (a lost Redis connection while an
/// event goes out) counts as that item failing, so the run goes on and the
/// summary is still given
async fn guarded(work: impl Future<Output = Result<()>>) -> std::result::Result<(), String> {
    match AssertUnwindSafe(work).catch_unwind().await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("{:?}", error.error_type)),
        Err(_) => Err("Panicked".to_string()),
    }
}

/// Delete every server and bot of an account staff banned, bots first so
/// they leave the servers before those go. A non-banned account is
/// refused (`InvalidOperation`) and the attempt recorded. With `expected`
/// (the command line: the plan the person confirmed), a plan that changed
/// since is refused the same way. Writes one `staff.user.delete_owned`
/// record, then one record per server and bot.
pub async fn staff_delete_owned(
    db: &Database,
    voice_client: &VoiceClient,
    actor: &AuditRecord,
    user: &User,
    why: &str,
    forget: BotForget,
    expected: Option<&PlanIds>,
) -> Result<DeleteOwnedResult> {
    let why = check_reason(why)?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if !is_banned(user) {
        record_delete_owned_refusal(db, actor, &user.id, &why, "account is not banned").await?;
        return Err(create_error!(InvalidOperation));
    }

    let plan = plan_delete_owned(db, user).await?;
    if expected.is_some_and(|ids| *ids != plan.ids()) {
        record_delete_owned_refusal(db, actor, &user.id, &why, "plan changed after it was shown").await?;
        return Err(create_error!(InvalidOperation));
    }
    let record = record_for(
        actor,
        "staff.user.delete_owned",
        "user",
        &user.id,
        &why,
        format!(
            "servers={} bots={} official={}",
            plan.servers.len(),
            plan.bots.len(),
            plan.skipped.len()
        ),
    );
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
        match guarded(staff_delete_bot(db, voice_client, actor, &bot, &why, Some(&context), forget)).await {
            Ok(()) => result.deleted_bots.push(bot.id.clone()),
            Err(error) => result.failed.push(SkippedItem {
                kind: "bot".into(),
                id: bot.id.clone(),
                name,
                why: error,
            }),
        }
    }
    for PlannedServer { server, .. } in plan.servers {
        let (id, name) = (server.id.clone(), server.name.clone());
        match guarded(staff_delete_server(db, voice_client, actor, server, &why, Some(&context))).await {
            Ok(()) => result.deleted_servers.push(id),
            Err(error) => result.failed.push(SkippedItem {
                kind: "server".into(),
                id,
                name,
                why: error,
            }),
        }
    }
    result.deleted_server_count = result.deleted_servers.len();
    result.deleted_bot_count = result.deleted_bots.len();
    Ok(result)
}

/// `zrzur-staff delete-owned <user-id> [--dry-run] <reason...>`, with its
/// input and output passed in so it can be tested. Returns the exit code:
/// 0 done (or a dry run of a banned account), 1 refused or something
/// failed, 2 wrong arguments.
///
/// Order: the plan is shown; an account that is not banned is refused
/// there and then (recorded, unless it is a dry run), before anything is
/// set up; a dry run stops; `setup` connects what the deletion needs (the
/// event queue, the voice service) and may fail; only then the account's
/// exact `username#discriminator` is asked for, and only that answer leads
/// to a deletion, of exactly the plan that was shown.
pub async fn run_delete_owned_command<S, F>(
    db: &Database,
    actor: &AuditRecord,
    user_id: &str,
    args: &[String],
    input: &mut impl BufRead,
    out: &mut impl Write,
    setup: S,
) -> i32
where
    S: FnOnce() -> F,
    F: Future<Output = std::result::Result<VoiceClient, String>>,
{
    macro_rules! say {
        ($($arg:tt)*) => {{
            let _ = writeln!(out, $($arg)*);
        }};
    }

    let mut dry_run = false;
    let mut words = vec![];
    for arg in args {
        if arg == "--dry-run" {
            dry_run = true;
        } else if arg.starts_with("--") {
            say!("error: unknown option {arg} (only --dry-run)");
            return 2;
        } else {
            words.push(arg.as_str());
        }
    }
    let Ok(why) = check_reason(&words.join(" ")) else {
        say!("error: give a reason (3 to 500 characters)");
        return 2;
    };
    let Ok(user) = db.fetch_user(user_id).await else {
        say!("error: no such user");
        return 1;
    };
    if user.bot.is_some() {
        say!("error: this is a bot; delete-owned takes the account that owns it");
        return 1;
    }
    let handle = format!("{}#{}", user.username, user.discriminator);
    say!("account: {handle} ({})", user.id);

    let plan = match plan_delete_owned(db, &user).await {
        Ok(plan) => plan,
        Err(error) => {
            say!("error: {:?}", error.error_type);
            return 1;
        }
    };
    say!("\nwould delete {} servers:", plan.servers.len());
    for s in &plan.servers {
        let link = s
            .server
            .vanity_invite
            .as_ref()
            .map(|v| format!("zrzur.cc/{}", v.code))
            .unwrap_or_else(|| "-".into());
        say!("  {}  {:?}  members={}  link={link}", s.server.id, s.server.name, s.members);
    }
    say!("would delete {} bots:", plan.bots.len());
    for b in &plan.bots {
        say!("  {}  {:?}", b.bot.id, b.name);
    }
    say!("kept (official): {}", plan.skipped.len());
    for k in &plan.skipped {
        say!("  {} {}  {:?}", k.kind, k.id, k.name);
    }

    if !is_banned(&user) {
        say!("\nrefused: the account is not banned. Ban it first (admin console, Trust & Safety); a disabled account is not enough. Nothing changed.");
        if !dry_run {
            if let Err(error) = record_delete_owned_refusal(db, actor, &user.id, &why, "account is not banned").await {
                say!("error: audit log write failed: {:?}", error.error_type);
            }
        }
        return 1;
    }
    if dry_run {
        say!("\ndry run: nothing changed");
        return 0;
    }
    if plan.servers.is_empty() && plan.bots.is_empty() {
        say!("\nnothing to delete");
        return 0;
    }

    let voice = match setup().await {
        Ok(voice) => voice,
        Err(error) => {
            say!("error: {error}; nothing changed");
            return 1;
        }
    };

    say!("\nThis cannot be undone. Type the account's name ({handle}) to delete all of the above:");
    let _ = out.flush();
    let mut typed = String::new();
    match input.read_line(&mut typed) {
        Ok(n) if n > 0 => {}
        _ => {
            say!("no answer; nothing changed");
            return 1;
        }
    }
    if typed.trim_end_matches(['\r', '\n']) != handle {
        say!("the name does not match; nothing changed");
        return 1;
    }

    // the account as it is now; the shared code checks the ban again and
    // that the servers and bots are still the ones shown
    let Ok(user) = db.fetch_user(user_id).await else {
        say!("error: the account is gone; nothing changed");
        return 1;
    };
    let shown = plan.ids();
    let result = match staff_delete_owned(db, &voice, actor, &user, &why, |_| {}, Some(&shown)).await {
        Ok(result) => result,
        Err(error) => {
            say!(
                "refused ({:?}): the account was unbanned, or its servers and bots changed after the plan was shown. Nothing changed; run it again.",
                error.error_type
            );
            return 1;
        }
    };
    say!("\ndeleted {} servers:", result.deleted_server_count);
    for id in &result.deleted_servers {
        say!("  {id}");
    }
    say!("deleted {} bots:", result.deleted_bot_count);
    for id in &result.deleted_bots {
        say!("  {id}");
    }
    say!("kept (official): {}", result.skipped.len());
    for k in &result.skipped {
        say!("  {} {}  {:?}", k.kind, k.id, k.name);
    }
    say!("failed: {}", result.failed.len());
    for f in &result.failed {
        say!("  {} {}  {:?}  {}", f.kind, f.id, f.name, f.why);
    }
    if result.failed.is_empty() {
        0
    } else {
        1
    }
}
