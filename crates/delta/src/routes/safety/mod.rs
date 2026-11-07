//! Trust & Safety: reporting, review, moderation actions, standing, appeals.
//!
//! - Anyone signed in (not bots) files reports and sees their own receipts.
//! - `safety.review` reads the queue and triages; `safety.action` takes and
//!   reverses actions and decides appeals; `safety.manage` handles
//!   escalations, reopening and legal holds. Server roles never count.
//! - Reporters never see reviewer identities, notes, evidence or other
//!   reports; reported users never learn who reported them.

use revolt_database::{
    platform_permissions, safety_now, staff_now, Database, PlatformPermission, PlatformPermissions,
    StaffPermission, User,
};
use revolt_models::v0::{SafetyCase, SafetyLogEntry};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;
use ulid::Ulid;

pub(crate) mod actions;
pub(crate) mod appeals;
mod public_report;
mod report_content;
pub(crate) mod reports;
pub use public_report::PublicReportGuard;
pub(crate) mod review;
pub(crate) mod targets;

#[cfg(test)]
mod test;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        // Reporting
        report_content::report_content,
        public_report::create_public,
        reports::reasons,
        reports::create,
        reports::list_mine,
        reports::fetch_mine,
        reports::reply,
        // Account standing and appeals
        appeals::standing,
        appeals::appeal,
        // Review
        review::access,
        review::queue,
        review::case,
        review::set_status,
        review::edit,
        review::note,
        review::bulk,
        review::user_history,
        actions::take,
        actions::revoke,
        appeals::queue,
        appeals::decide,
        // Bot verification
    ]
}

/// Actor id used for things the system does
pub(crate) const SYSTEM_ACTOR: &str = "system";

/// Trust & Safety permissions of a user (bots never have any)
pub(crate) async fn safety_permissions(db: &Database, user: &User) -> Result<PlatformPermissions> {
    if user.bot.is_some() {
        return Ok(PlatformPermissions::default());
    }
    platform_permissions(db, user).await
}

/// Whether a user may review cases: a Trust & Safety reviewer, or platform
/// staff who may read reports (used when assigning cases)
pub(crate) async fn may_review(db: &Database, user: &User) -> Result<bool> {
    if user.bot.is_some() {
        return Ok(false);
    }
    if safety_permissions(db, user).await?.has(PlatformPermission::SafetyReview) {
        return Ok(true);
    }
    Ok(db.fetch_staff_grant(&user.id).await?.is_some_and(|grant| {
        grant
            .effective(staff_now())
            .contains(&StaffPermission::ReportsRead)
    }))
}

pub(crate) fn require(perms: &PlatformPermissions, permission: PlatformPermission) -> Result<()> {
    if perms.has(permission) {
        Ok(())
    } else {
        Err(create_error!(MissingPlatformPermission {
            permission: permission.name().to_string()
        }))
    }
}

/// Append to a case's audit trail
pub(crate) async fn log(
    db: &Database,
    case_id: &str,
    actor_id: &str,
    event: &str,
    from: Option<String>,
    to: Option<String>,
    text: Option<String>,
) -> Result<()> {
    db.insert_safety_log(&SafetyLogEntry {
        id: Ulid::new().to_string(),
        case_id: case_id.to_string(),
        actor_id: actor_id.to_string(),
        event: event.to_string(),
        from,
        to,
        text,
        created_at: safety_now(),
    })
    .await
}

/// Save a changed case if nobody changed it since `before`
pub(crate) async fn save_case(db: &Database, before: &SafetyCase, mut after: SafetyCase) -> Result<SafetyCase> {
    after.version = before.version + 1;
    after.updated_at = safety_now();
    if db.update_safety_case(&after, before.version).await? {
        Ok(after)
    } else {
        Err(create_error!(SafetyCaseConflict))
    }
}

/// Reject invalid request bodies
pub(crate) fn validate<T: validator::Validate>(data: &T) -> Result<()> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))
}

/// Trim and drop control characters from free text. Text is stored as
/// typed and always rendered as plain text by clients, never as HTML.
pub(crate) fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect::<String>()
        .trim()
        .to_string()
}
