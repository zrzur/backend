//! Server actions in the admin console, besides what Trust & Safety cases
//! do (removing a Discover listing):
//!
//! - `platform.servers.restrict` + a fresh step-up: give or take away the
//!   verified mark, mark a server 18+ or take that away
//! - Owner only, + a fresh step-up: hand a server to another member (for an
//!   owner who lost their account; the support team checks who asks)
//! - Owner only, + a fresh step-up: delete a server (never an official one;
//!   see `deletion.rs`)
//!
//! Every change needs a reason and is written to the platform audit log
//! first. Members see the change at once.

use revolt_database::{
    record_platform_audit, voice::VoiceClient, AuditOutcome, Database, PartialServer, StaffPermission,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};

use super::{reason, StaffContext};

/// `flags` values the app shows
const FLAG_OFFICIAL: i32 = 1;
const FLAG_VERIFIED: i32 = 2;

#[derive(Deserialize)]
pub struct DataServerAction {
    /// verify, unverify, mark_nsfw, unmark_nsfw, transfer or delete
    pub action: String,
    pub reason: String,
    /// transfer: the member who becomes the owner
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Serialize)]
pub struct ServerActionResult {
    pub id: String,
    pub owner: String,
    pub flags: Option<i32>,
    pub nsfw: bool,
    /// `delete`: the server is gone
    pub deleted: bool,
}

/// # Act on a server
#[post("/servers/<id>/actions", data = "<data>")]
pub async fn act(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    staff: StaffContext,
    id: &str,
    data: Json<DataServerAction>,
) -> Result<Json<ServerActionResult>> {
    let data = data.into_inner();
    if data.action == "delete" {
        return delete(db, voice_client, &staff, id, &data.reason).await.map(Json);
    }
    let transfer = data.action == "transfer";
    staff.require_step_up(db, StaffPermission::ServersRestrict).await?;
    if transfer && !staff.grant.is_owner(staff.now) {
        return Err(create_error!(NotPrivileged));
    }
    let why = reason(&data.reason)?;
    let mut server = db.fetch_server(id).await?;
    let mut partial = PartialServer::default();
    let mut summary = data.action.clone();
    match data.action.as_str() {
        "verify" => {
            if server.flags.is_some_and(|f| f == FLAG_OFFICIAL || f == FLAG_VERIFIED) {
                return Err(create_error!(InvalidOperation));
            }
            partial.flags = Some(FLAG_VERIFIED);
        }
        "unverify" => {
            if server.flags != Some(FLAG_VERIFIED) {
                return Err(create_error!(InvalidOperation));
            }
            partial.flags = Some(0);
        }
        "mark_nsfw" | "unmark_nsfw" => {
            let nsfw = data.action == "mark_nsfw";
            if server.nsfw == nsfw {
                return Err(create_error!(InvalidOperation));
            }
            partial.nsfw = Some(nsfw);
        }
        "transfer" => {
            let to = data.to.as_deref().ok_or_else(|| create_error!(InvalidOperation))?;
            if to == server.owner {
                return Err(create_error!(InvalidOperation));
            }
            let user = db.fetch_user(to).await?;
            if user.bot.is_some() {
                return Err(create_error!(IsBot));
            }
            // only someone already in the server
            db.fetch_member(&server.id, to).await?;
            partial.owner = Some(to.to_string());
            summary = format!("transfer {} -> {}", server.owner, to);
        }
        _ => return Err(create_error!(InvalidOperation)),
    }
    let mut record = staff.record(
        "staff.server.action",
        Some(StaffPermission::ServersRestrict),
    );
    record.target_kind = Some("server".to_string());
    record.target_id = Some(server.id.clone());
    record.reason = Some(why.clone());
    record.summary = Some(summary);
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    let previous_owner = server.owner.clone();
    // flags 0 means no mark in the app
    if let Err(error) = server.update(db, partial, vec![]).await {
        let mut failed = record;
        failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
        return Err(error);
    }

    // a failure here never undoes the action
    if matches!(data.action.as_str(), "verify" | "unverify" | "transfer") {
        for owner in [&previous_owner, &server.owner] {
            if let Err(error) = revolt_database::sync_verified_server_owner(db, owner).await {
                log::error!("verified server owner badge for {owner}: {error:?}");
            }
        }
    }

    // the owner (and a new owner) hear about it in their private messages
    let key = match data.action.as_str() {
        "verify" => "server.verified",
        "unverify" => "server.unverified",
        "mark_nsfw" => "server.nsfw",
        "unmark_nsfw" => "server.not_nsfw",
        _ => "server.transferred_from",
    };
    crate::util::official::server(db, &previous_owner, key, &server.name, &why).await;
    if transfer {
        crate::util::official::server(db, &server.owner, "server.transferred_to", &server.name, &why).await;
    }
    Ok(Json(ServerActionResult {
        id: server.id,
        owner: server.owner,
        flags: server.flags,
        nsfw: server.nsfw,
        deleted: false,
    }))
}

/// `delete`: owner role and step-up, a reason, never an official server.
/// A server that is already gone answers `NotFound` every time.
async fn delete(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    id: &str,
    why: &str,
) -> Result<ServerActionResult> {
    super::deletion::require_delete_gate(db, staff, "staff.server.delete", "server", id).await?;
    let why = reason(why)?;
    let server = db.fetch_server(id).await?;
    let result = ServerActionResult {
        id: server.id.clone(),
        owner: server.owner.clone(),
        flags: server.flags,
        nsfw: server.nsfw,
        deleted: true,
    };
    let (owner, name) = (server.owner.clone(), server.name.clone());
    super::deletion::delete_server(db, voice_client, staff, server, &why, None).await?;
    crate::util::official::server(db, &owner, "server.deleted", &name, &why).await;
    Ok(result)
}
