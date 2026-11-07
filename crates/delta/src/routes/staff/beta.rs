//! The beta program in the admin console: feature flags and testers. Same
//! functions as `/badges/beta/admin`, through a staff session:
//!
//! - `platform.settings.read`: see features and members
//! - `platform.settings.manage` + a fresh step-up: change a feature, invite,
//!   approve, remove or complete a tester, set their features
//!
//! Every change is written to the platform audit log first.

use revolt_database::{record_platform_audit, AuditOutcome, AuditRecord, Database, StaffPermission};
use revolt_models::v0::{BetaFeature, BetaMemberRecord, DataBetaFeature, DataBetaMember};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::StaffContext;
use crate::routes::badges::beta::{features_list, member_change, members_list, save_feature};

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

/// # Beta features
#[get("/beta/features")]
pub async fn features(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<BetaFeature>>> {
    staff.require(db, StaffPermission::SettingsRead).await?;
    features_list(db).await.map(Json)
}

/// # Create or change a beta feature
#[post("/beta/features", data = "<data>")]
pub async fn save(db: &State<Database>, staff: StaffContext, data: Json<DataBetaFeature>) -> Result<Json<BetaFeature>> {
    staff.require_step_up(db, StaffPermission::SettingsManage).await?;
    let data = data.into_inner();
    let mut record = staff.record("staff.beta.feature", Some(StaffPermission::SettingsManage));
    record.target_kind = Some("beta_feature".to_string());
    record.target_id = Some(data.id.chars().take(64).collect());
    record.summary = Some(format!("enabled={} all_testers={}", data.enabled, data.all_testers));
    audited(db, record, save_feature(db, &staff.user.id, data)).await.map(Json)
}

/// # Beta members
///
/// `status` is Requested, Invited, Active, Removed or Completed (all when
/// absent).
#[get("/beta/members?<status>")]
pub async fn members(db: &State<Database>, staff: StaffContext, status: Option<&str>) -> Result<Json<Vec<BetaMemberRecord>>> {
    staff.require(db, StaffPermission::SettingsRead).await?;
    members_list(db, status).await.map(Json)
}

/// # Change a beta membership
///
/// `invite`, `approve`, `remove`, `complete` (an active tester who sent
/// feedback or a bug report) or `features`.
#[post("/beta/members", data = "<data>")]
pub async fn member(db: &State<Database>, staff: StaffContext, data: Json<DataBetaMember>) -> Result<Json<BetaMemberRecord>> {
    staff.require_step_up(db, StaffPermission::SettingsManage).await?;
    let data = data.into_inner();
    if !matches!(data.action.as_str(), "invite" | "approve" | "remove" | "complete" | "features") {
        return Err(create_error!(InvalidOperation));
    }
    let mut record = staff.record("staff.beta.member", Some(StaffPermission::SettingsManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(data.user.chars().take(64).collect());
    record.summary = Some(data.action.clone());
    let (user, action) = (data.user.clone(), data.action.clone());
    let changed = audited(db, record, member_change(db, &staff.user.id, data)).await?;
    crate::util::official::beta(db, &user, &action).await;
    Ok(Json(changed))
}
