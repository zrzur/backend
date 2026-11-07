//! Discover approval in the admin console. Servers and bots appear on
//! Discover only after a reviewer approves their request. Same decision
//! logic as `/discover/requests` (which stays limited to `privileged`
//! accounts), reached through a staff session:
//!
//! - `platform.servers.read`: see the queue
//! - `platform.servers.restrict`: approve, deny, remove, feature
//!
//! Every decision is written to the platform audit log first.

use revolt_database::{
    record_platform_audit, AuditOutcome, Database, DiscoverRequestType, StaffPermission,
};
use revolt_models::v0::{DataDiscoverDecision, DiscoverDecision, DiscoverQueueEntry};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::StaffContext;
use crate::routes::discover::{decide::decide_request, queue::queue_entries};

/// # Discover queue
///
/// Requests waiting for a decision (pending or under review).
#[get("/discover/requests")]
pub async fn requests(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<DiscoverQueueEntry>>> {
    staff.require(db, StaffPermission::ServersRead).await?;
    queue_entries(db).await.map(Json)
}

/// # Decide a Discover request
///
/// Approve (lists it), deny (the owner may apply again), remove (unlists;
/// the owner must contact support) or mark as under review. Denying and
/// removing need a reason, which the owner sees.
#[post("/discover/requests/<kind>/<id>", data = "<data>")]
pub async fn decide(
    db: &State<Database>,
    staff: StaffContext,
    kind: &str,
    id: &str,
    data: Json<DataDiscoverDecision>,
) -> Result<()> {
    staff.require(db, StaffPermission::ServersRestrict).await?;
    let data = data.into_inner();
    let request_type = match kind {
        "server" => DiscoverRequestType::Server,
        "bot" => DiscoverRequestType::Bot,
        _ => return Err(create_error!(InvalidOperation)),
    };
    let reason = data.reason.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if matches!(data.decision, Some(DiscoverDecision::Deny | DiscoverDecision::Remove))
        && reason.is_none_or(|r| r.chars().count() < 3)
    {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length("reason", 3, 512),
        ]));
    }
    // the request must exist before we log deciding it
    db.fetch_discover_request_by_item_id(request_type, id).await?;

    let mut record = staff.record("staff.discover.decide", Some(StaffPermission::ServersRestrict));
    record.target_kind = Some(kind.to_string());
    record.target_id = Some(id.to_string());
    record.reason = reason.map(|r| r.chars().take(500).collect());
    record.summary = Some(format!(
        "{}{}",
        data.decision.as_ref().map(|d| format!("{d:?}")).unwrap_or_else(|| "-".to_string()),
        data.featured.map(|f| format!(" featured={f}")).unwrap_or_default()
    ));
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    if let Err(error) = decide_request(db, kind, id, data).await {
        let mut failed = record;
        failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
        return Err(error);
    }
    Ok(())
}
