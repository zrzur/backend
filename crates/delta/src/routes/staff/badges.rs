//! Badges in the admin console: see a user's badges, grant and revoke the
//! hand-given ones. Same rules as `/badges/admin` (shared functions in
//! `routes::badges`), through a staff session:
//!
//! - `platform.users.read`: see the catalogue and a user's badge record
//! - `platform.badges.manage`: grant and revoke `Staff`-rule badges
//! - `Privileged`-rule badges (the Staff badge, old official badges): an
//!   Owner only, with a fresh step-up
//!
//! Plus, Creator and Beta badges follow their records and can't be given
//! by hand. Every change is written to the platform audit log first.

use revolt_database::{
    badge_definition, record_platform_audit, AuditOutcome, BadgeGrantRule, Database, StaffPermission,
};
use revolt_models::v0::{AdminBadgeGrant, AdminUserBadges, BadgeInfo, DataGrantBadge, DataRevokeBadge};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::Deserialize;

use super::StaffContext;
use crate::routes::badges::{catalogue, grant_with, revoke_with, user_record, BadgeRights};

fn rights(staff: &StaffContext) -> BadgeRights {
    BadgeRights {
        staff: staff.has(StaffPermission::BadgesManage),
        privileged: staff.grant.is_owner(staff.now),
    }
}

/// # Badge catalogue
#[get("/badges/catalogue")]
pub async fn badge_catalogue(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<BadgeInfo>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    Ok(catalogue().await)
}

/// # A user's badges
///
/// Grants, badge history and which badges you may give them now.
#[get("/users/<id>/badges")]
pub async fn user_badges(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<AdminUserBadges>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    user_record(db, &staff.user.id, rights(&staff), id).await.map(Json)
}

#[derive(Deserialize)]
pub struct DataBadgeChange {
    pub badge: String,
    pub reason: String,
    /// Milliseconds since the epoch (grants only)
    pub expires_at: Option<i64>,
}

/// Permission checks shared by grant and revoke; returns the audit record
async fn check(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    id: &str,
    data: &DataBadgeChange,
) -> Result<revolt_database::AuditRecord> {
    staff.require(db, StaffPermission::BadgesManage).await?;
    let definition = badge_definition(&data.badge).ok_or_else(|| create_error!(NotFound))?;
    // the Staff badge and old official badges: Owner only, freshly confirmed
    if definition.rule == BadgeGrantRule::Privileged {
        if !staff.grant.is_owner(staff.now) {
            let mut record = staff.record(action, Some(StaffPermission::BadgesManage));
            record.target_kind = Some("user".to_string());
            record.target_id = Some(id.to_string());
            record.summary = Some(format!("{} (owner only)", data.badge));
            record_platform_audit(db, record, AuditOutcome::Denied).await?;
            return Err(create_error!(NotPrivileged));
        }
        staff.require_step_up(db, StaffPermission::BadgesManage).await?;
    }
    let mut record = staff.record(action, Some(StaffPermission::BadgesManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    record.reason = Some(data.reason.trim().chars().take(500).collect());
    record.summary = Some(data.badge.clone());
    Ok(record)
}

async fn finish<T>(
    db: &Database,
    record: revolt_database::AuditRecord,
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

/// # Grant a badge
#[post("/users/<id>/badges/grant", data = "<data>")]
pub async fn grant(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataBadgeChange>,
) -> Result<Json<AdminBadgeGrant>> {
    let data = data.into_inner();
    let record = check(db, &staff, "staff.badge.grant", id, &data).await?;
    let (badge, note) = (data.badge.clone(), data.reason.clone());
    let change = grant_with(
        db,
        &staff.user.id,
        rights(&staff),
        DataGrantBadge {
            user: id.to_string(),
            badge: data.badge,
            reason: data.reason,
            expires_at: data.expires_at,
        },
    );
    let granted = finish(db, record, change).await?;
    crate::util::official::badge(db, id, true, &badge, &note).await;
    Ok(Json(granted))
}

/// # Revoke a badge
#[post("/users/<id>/badges/revoke", data = "<data>")]
pub async fn revoke(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataBadgeChange>,
) -> Result<Json<AdminBadgeGrant>> {
    let data = data.into_inner();
    let record = check(db, &staff, "staff.badge.revoke", id, &data).await?;
    let (badge, note) = (data.badge.clone(), data.reason.clone());
    let change = revoke_with(
        db,
        &staff.user.id,
        rights(&staff),
        DataRevokeBadge {
            user: id.to_string(),
            badge: data.badge,
            reason: data.reason,
        },
    );
    let revoked = finish(db, record, change).await?;
    crate::util::official::badge(db, id, false, &badge, &note).await;
    Ok(Json(revoked))
}
