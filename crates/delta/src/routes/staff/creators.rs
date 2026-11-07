//! Community design creators in the admin console. Only creators can
//! publish designs (they go live without review), so this list is who may
//! publish. Same rules as `/community/admin/creators`, through a staff
//! session:
//!
//! - `platform.users.read`: see creators and their history
//! - `platform.creators.manage`: grant, suspend, revoke, reinstate
//!
//! Suspended and revoked creators can't publish. A suspended creator's
//! designs are hidden until they are reinstated; a revoked creator's stay
//! (take one down through Trust & Safety). Every change is written to the
//! platform audit log first.

use revolt_database::{record_platform_audit, AuditOutcome, Database, StaffPermission};
use revolt_models::v0::{CreatorLogEntry, CreatorRecord, DataCreatorAction};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::Deserialize;

use super::{reason, StaffContext};
use crate::routes::community::admin::{creator_change, creator_list};

/// # Creators
///
/// Without `q`: everyone with a creator record. With `q`: users matching
/// it by name, with their standing (to find someone to make a creator).
#[get("/creators?<q>")]
pub async fn list(db: &State<Database>, staff: StaffContext, q: Option<&str>) -> Result<Json<Vec<CreatorRecord>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    if q.is_some_and(|q| q.chars().count() > 64) {
        return Err(create_error!(FailedValidation {
            error: "query too long".to_string()
        }));
    }
    let searching = q.is_some_and(|q| !q.trim().is_empty());
    let mut records = creator_list(db, q).await?;
    if !searching {
        // reviewers without a creator record aren't creators
        records.retain(|r| r.creator.is_some());
    }
    Ok(Json(records))
}

/// # A creator's history
#[get("/creators/<id>/history")]
pub async fn history(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<Vec<CreatorLogEntry>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    Ok(Json(
        db.fetch_creator_logs(id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct DataCreatorChange {
    /// grant, suspend, revoke or reinstate
    pub action: String,
    pub reason: String,
}

/// # Change a creator
#[post("/creators/<id>", data = "<data>")]
pub async fn change(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataCreatorChange>,
) -> Result<Json<CreatorRecord>> {
    staff.require(db, StaffPermission::CreatorsManage).await?;
    let data = data.into_inner();
    if !matches!(data.action.as_str(), "grant" | "suspend" | "revoke" | "reinstate") {
        return Err(create_error!(InvalidOperation));
    }
    let why = reason(&data.reason)?;
    // the user must exist before we log changing them
    db.fetch_user(id).await?;
    let mut record = staff.record("staff.creator.change", Some(StaffPermission::CreatorsManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    record.reason = Some(why.clone());
    record.summary = Some(data.action.clone());
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    let action = data.action.clone();
    let result = creator_change(
        db,
        &staff.user.id,
        DataCreatorAction {
            user: id.to_string(),
            action: data.action,
            reason: Some(why.clone()),
        },
    )
    .await;
    if let Err(error) = &result {
        let mut failed = record;
        failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
    } else {
        crate::util::official::creator(db, id, &action, &why).await;
    }
    result.map(Json)
}
