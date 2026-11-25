//! Server actions in the admin console, besides what Trust & Safety cases
//! do (removing a Discover listing):
//!
//! - `platform.servers.restrict` + a fresh step-up: give or take away the
//!   verified mark or the partner mark (flag bit 4: a partner may hold a
//!   custom invite link without the member and activity rule), mark a
//!   server 18+ or take that away
//! - Owner only, + a fresh step-up: hand a server to another member (for an
//!   owner who lost their account; the support team checks who asks)
//! - Owner only, + a fresh step-up: delete a server (never an official one;
//!   see `deletion.rs`)
//!
//! Every change needs a reason and is written to the platform audit log
//! first. Members see the change at once.

use revolt_database::{
    AuditOutcome, Database, PartialServer, StaffPermission, record_platform_audit,
    voice::VoiceClient,
};
use revolt_result::{Result, create_error};
use rocket::{State, serde::json::Json};
use serde::{Deserialize, Serialize};

use super::{StaffContext, reason};

/// `flags` bits: 1 official, 2 verified, 4 partner. Each action changes its
/// own bit and keeps the others.
use revolt_database::{
    SERVER_FLAG_OFFICIAL as FLAG_OFFICIAL, SERVER_FLAG_PARTNER as FLAG_PARTNER,
    SERVER_FLAG_VERIFIED as FLAG_VERIFIED,
};

/// What `verify` and `partner` answer: those marks come from applications
pub const APPLICATION_ONLY: &str =
    "Verified Server and Partner are given only by approving an application (Badge applications).";

#[derive(Deserialize)]
pub struct DataServerAction {
    /// verify, unverify, partner, unpartner, mark_nsfw, unmark_nsfw, transfer or delete
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
    act_core(db, voice_client, &staff, id, data.into_inner())
        .await
        .map(Json)
}

/// The console's `act`, for any caller holding a [`StaffContext`] (the
/// console route and Zrzur Desk): the same checks and records
pub(super) async fn act_core(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    id: &str,
    data: DataServerAction,
) -> Result<ServerActionResult> {
    if data.action == "delete" {
        return delete(db, voice_client, staff, id, &data.reason).await;
    }
    let transfer = data.action == "transfer";
    staff
        .require_step_up(db, StaffPermission::ServersRestrict)
        .await?;
    if transfer && !staff.grant.is_owner(staff.now) {
        return Err(create_error!(NotPrivileged));
    }
    let why = reason(&data.reason)?;
    let mut server = db.fetch_server(id).await?;
    let mut partial = PartialServer::default();
    let flags = server.flags.unwrap_or_default();
    let mut summary = data.action.clone();
    // Verified Server and Partner come only from an approved application
    // (Badge applications), with its checks, reasons and two-person rules
    if matches!(data.action.as_str(), "verify" | "partner") {
        let mut record = staff.record("staff.server.action", Some(StaffPermission::ServersRestrict));
        record.target_kind = Some("server".to_string());
        record.target_id = Some(server.id.clone());
        record.summary = Some(format!("{} denied:application_only", data.action));
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(FailedValidation { error: APPLICATION_ONLY.to_string() }));
    }
    match data.action.as_str() {
        "unverify" => {
            if flags & FLAG_VERIFIED == 0 {
                return Err(create_error!(InvalidOperation));
            }
            partial.flags = Some(flags & !FLAG_VERIFIED);
        }
        "unpartner" => {
            if flags & FLAG_PARTNER == 0 {
                return Err(create_error!(InvalidOperation));
            }
            partial.flags = Some(flags & !FLAG_PARTNER);
        }
        "mark_nsfw" | "unmark_nsfw" => {
            let nsfw = data.action == "mark_nsfw";
            if server.nsfw == nsfw {
                return Err(create_error!(InvalidOperation));
            }
            partial.nsfw = Some(nsfw);
        }
        "transfer" => {
            // an official server is not handed over until a two-person
            // rule for it exists
            if flags & FLAG_OFFICIAL != 0 {
                let mut record = staff.record(
                    "staff.server.action",
                    Some(StaffPermission::ServersRestrict),
                );
                record.target_kind = Some("server".to_string());
                record.target_id = Some(server.id.clone());
                record.summary = Some("transfer of an official server".to_string());
                revolt_database::record_platform_audit(
                    db,
                    record,
                    revolt_database::AuditOutcome::Denied,
                )
                .await?;
                return Err(create_error!(NotPrivileged));
            }
            let to = data
                .to
                .as_deref()
                .ok_or_else(|| create_error!(InvalidOperation))?;
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
        failed.summary = Some(
            format!("{:?}", error.error_type)
                .chars()
                .take(200)
                .collect(),
        );
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
        return Err(error);
    }

    // a failure here never undoes the action
    if transfer {
        server.flags = crate::routes::badges::programs::after_server_transfer(db, &server).await;
    } else if matches!(data.action.as_str(), "unverify" | "unpartner") {
        // the approved application ends with its mark (Partner: the badge too)
        let program = if data.action == "unverify" {
            revolt_database::BadgeProgram::VerifiedServer
        } else {
            revolt_database::BadgeProgram::Partner
        };
        let programs = revolt_config::config().await.api.badge_programs;
        if let Err(error) = revolt_database::end_programs_for_target(
            db,
            &programs,
            &server.id,
            Some(program),
            "mark_removed",
            crate::routes::badges::now_ms(),
        )
        .await
        {
            log::error!("programs of server {}: {:?}", server.id, error.error_type);
        }
    }

    // the owner (and a new owner) hear about it in their private messages
    let key = match data.action.as_str() {
        "unverify" => "server.unverified",
        "mark_nsfw" => "server.nsfw",
        "unmark_nsfw" => "server.not_nsfw",
        "unpartner" => "server.unpartnered",
        _ => "server.transferred_from",
    };
    crate::util::official::server(db, &previous_owner, key, &server.name, &why).await;
    if transfer {
        crate::util::official::server(
            db,
            &server.owner,
            "server.transferred_to",
            &server.name,
            &why,
        )
        .await;
    }
    Ok(ServerActionResult {
        id: server.id,
        owner: server.owner,
        flags: server.flags,
        nsfw: server.nsfw,
        deleted: false,
    })
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
    super::deletion::delete_server(db, voice_client, staff, server, &why).await?;
    crate::util::official::server(db, &owner, "server.deleted", &name, &why).await;
    Ok(result)
}
