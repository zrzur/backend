//! Deleting servers and bots from the admin console, for accounts that
//! were created to abuse the platform (for example to grab custom links).
//!
//! - `POST /staff/servers/<id>/actions` with `"action": "delete"`
//! - `POST /staff/bots/<id>/delete`
//! - `POST /staff/users/<id>/delete-owned`: every server and bot of an
//!   account that staff already banned, in one step
//!
//! Gate, the strictest one staff actions have (the same as handing a
//! server over): the Owner role, `platform.servers.restrict` and a fresh
//! step-up. Every deletion needs a reason. The deletion, the official
//! refusal and the audit records are `revolt_database::staff_deletion`,
//! shared with `zrzur-staff delete-owned`.

use revolt_database::{
    AuditOutcome, AuditRecord, Bot, Database, DeletionInfo, Server, StaffPermission,
    record_platform_audit,
    staff_deletion::{self, DeleteOwnedResult, PlanIds},
    voice::VoiceClient,
};
use revolt_result::{Result, create_error};
use rocket::{State, serde::json::Json};
use serde::{Deserialize, Serialize};

use super::{StaffContext, reason};

pub(super) const PERMISSION: StaffPermission = StaffPermission::ServersRestrict;

/// The API's own command-list cache goes with a deleted bot
fn forget(bot_id: &str) {
    crate::util::public_commands::forget(bot_id);
}

/// The staff member and session, for the audit records
fn actor(staff: &StaffContext) -> AuditRecord {
    staff.record("", Some(PERMISSION))
}

/// The Owner role first, then the permission and a fresh step-up. A staff
/// member without the Owner role gets one clear refusal, written to the
/// audit log on every attempt, whether or not they stepped up.
pub(super) async fn require_delete_gate(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    target_kind: &str,
    target_id: &str,
) -> Result<()> {
    if !staff.grant.is_owner(staff.now) {
        let mut record = staff.record(action, Some(PERMISSION));
        record.target_kind = Some(target_kind.to_string());
        record.target_id = Some(target_id.to_string());
        record.summary = Some("owner role required".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    staff.require_step_up(db, PERMISSION).await
}

/// Delete one server, audited first. An official server is refused.
pub(super) async fn delete_server(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    server: Server,
    why: &str,
) -> Result<()> {
    staff_deletion::staff_delete_server(db, voice_client, &actor(staff), server, why, None).await
}

/// Delete one bot, audited first. An official bot is refused.
pub(super) async fn delete_bot(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    bot: &Bot,
    why: &str,
) -> Result<()> {
    staff_deletion::staff_delete_bot(db, voice_client, &actor(staff), bot, why, None, forget).await
}

#[derive(Deserialize)]
pub struct DataDeleteOwned {
    pub reason: String,
}

/// # Delete every server and bot an account owns
///
/// Only for an account that staff already banned. Official servers and
/// bots are kept and listed under `skipped`. Bots go first, so they leave
/// the servers before those are deleted.
#[post("/users/<id>/delete-owned", data = "<data>")]
pub async fn delete_owned(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    staff: StaffContext,
    id: &str,
    data: Json<DataDeleteOwned>,
) -> Result<Json<DeleteOwnedResult>> {
    delete_owned_core(db, voice_client, &staff, id, data.into_inner(), None)
        .await
        .map(Json)
}

/// The console's `delete_owned`, for any caller holding a
/// [`StaffContext`]. `expected`: the servers and bots the person was shown
/// (Zrzur Desk's card); a plan that changed since is refused, as on the
/// command line.
pub(super) async fn delete_owned_core(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    id: &str,
    data: DataDeleteOwned,
    expected: Option<&PlanIds>,
) -> Result<DeleteOwnedResult> {
    require_delete_gate(db, staff, "staff.user.delete_owned", "user", id).await?;
    let why = reason(&data.reason)?;
    let user = db.fetch_user(id).await?;
    staff_deletion::staff_delete_owned(
        db,
        voice_client,
        &actor(staff),
        &user,
        &why,
        forget,
        expected,
    )
    .await
}

// ---- account deletion requests ----

/// What an account deletion request asks for
#[derive(Deserialize)]
pub struct DataDeletionRequest {
    pub reason: String,
}

/// An account waiting for its deletion
#[derive(Serialize)]
pub struct ScheduledDeletion {
    pub user_id: String,
    pub username: Option<String>,
    /// When the deletion runs, milliseconds since the epoch
    pub after: i64,
}

/// # Schedule an account's deletion
///
/// The same path as a person deleting their own account: the account is
/// disabled now (signed out everywhere) and deleted after the grace
/// period by the scheduled job. The Owner role, `platform.users.delete.
/// request` and a fresh step-up; never yourself, a bot, staff, a
/// privileged or an official account.
#[post("/users/<id>/delete-request", data = "<data>")]
pub async fn delete_request(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataDeletionRequest>,
) -> Result<Json<ScheduledDeletion>> {
    delete_request_core(db, &staff, id, data.into_inner())
        .await
        .map(Json)
}

/// See [`delete_request`]; shared with Zrzur Desk
pub(super) async fn delete_request_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataDeletionRequest,
) -> Result<ScheduledDeletion> {
    let permission = StaffPermission::UsersDeleteRequest;
    let mut record = staff.record("staff.user.delete_request", Some(permission));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    if !staff.grant.is_owner(staff.now) {
        record.summary = Some("owner role required".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    staff.require_step_up(db, permission).await?;
    let why = reason(&data.reason)?;
    let user = db.fetch_user(id).await?;
    if user.id == staff.user.id {
        return Err(create_error!(CannotReportYourself));
    }
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let staff_member = db
        .fetch_staff_grant(&user.id)
        .await?
        .is_some_and(|grant| grant.is_active(staff.now));
    if user.privileged || user.official || staff_member {
        record.summary = Some("protected account".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    let mut account = db.fetch_account(&user.id).await?;
    if matches!(
        account.deletion,
        Some(DeletionInfo::Scheduled { .. } | DeletionInfo::Deleted)
    ) {
        return Err(create_error!(NoEffect));
    }
    record.reason = Some(why);
    record.summary = Some("scheduled".to_string());
    super::audited::audited(db, record, account.schedule_deletion(db)).await?;
    let after = match &account.deletion {
        Some(DeletionInfo::Scheduled { after }) => millis(after),
        _ => 0,
    };
    Ok(ScheduledDeletion {
        user_id: user.id,
        username: Some(revolt_models::v0::user_handle(
            &user.username,
            &user.discriminator,
        )),
        after,
    })
}

fn millis(at: &iso8601_timestamp::Timestamp) -> i64 {
    at.duration_since(iso8601_timestamp::Timestamp::UNIX_EPOCH)
        .whole_milliseconds() as i64
}

/// # Accounts waiting for their deletion
///
/// Read only: who, and when the deletion runs. Newest first, 100 at most.
#[get("/deletions")]
pub async fn deletions(
    db: &State<Database>,
    staff: StaffContext,
) -> Result<Json<Vec<ScheduledDeletion>>> {
    deletions_core(db, &staff).await.map(Json)
}

/// See [`deletions`]; shared with Zrzur Desk
pub(super) async fn deletions_core(
    db: &Database,
    staff: &StaffContext,
) -> Result<Vec<ScheduledDeletion>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let accounts = db.fetch_accounts_scheduled_for_deletion(100).await?;
    let ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
    let users = db.fetch_users(&ids).await.unwrap_or_default();
    let mut out: Vec<ScheduledDeletion> = accounts
        .iter()
        .filter_map(|account| match &account.deletion {
            Some(DeletionInfo::Scheduled { after }) => Some(ScheduledDeletion {
                user_id: account.id.clone(),
                username: users
                    .iter()
                    .find(|u| u.id == account.id)
                    .map(|u| revolt_models::v0::user_handle(&u.username, &u.discriminator)),
                after: millis(after),
            }),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| b.after.cmp(&a.after));
    Ok(out)
}
