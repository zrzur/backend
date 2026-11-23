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
    record_platform_audit,
    staff_deletion::{self, DeleteOwnedResult},
    voice::VoiceClient,
    AuditOutcome, AuditRecord, Bot, Database, Server, StaffPermission,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::Deserialize;

use super::{reason, StaffContext};

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
    require_delete_gate(db, &staff, "staff.user.delete_owned", "user", id).await?;
    let why = reason(&data.reason)?;
    let user = db.fetch_user(id).await?;
    staff_deletion::staff_delete_owned(db, voice_client, &actor(&staff), &user, &why, forget, None)
        .await
        .map(Json)
}
