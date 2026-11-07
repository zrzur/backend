//! Trust & Safety in the admin console. The same case rules as
//! `/safety/admin` (shared functions in `routes::safety`), reached through a
//! staff session instead of a `platform_roles` entry:
//!
//! - `platform.reports.read`: queue, cases (with evidence), appeals, history
//! - `platform.reports.resolve`: triage, notes, actions, revocations, appeals
//! - account suspension and bans also need `platform.users.suspend` and a
//!   fresh step-up; so do the senior decisions (escalations, reopening,
//!   legal holds, bans)
//!
//! Opening a case and every change are written to the platform audit log
//! before they happen; a change that then fails gets a second record.

use std::collections::BTreeSet;

use revolt_database::{
    record_platform_audit, AuditOutcome, AuditRecord, Database, PlatformPermission,
    PlatformPermissions, StaffPermission,
};
use revolt_models::v0::{
    DataSafetyAction, DataSafetyAppealDecision, DataSafetyCaseEdit, DataSafetyCaseStatus,
    DataSafetyNote, DataSafetyRevoke, SafetyAction, SafetyActionKind, SafetyAppeal, SafetyCase,
    SafetyCaseDetails, SafetyCasePage, SafetyCategory, SafetyTargetKind,
};
use revolt_result::{create_error, Error, ErrorType, Result};
use rocket::{serde::json::Json, State};

use super::StaffContext;
use crate::routes::safety::{
    actions::{revoke_as, take_as},
    appeals::{appeals_of, decide_as},
    reports::open_staff_case,
    review::{
        case_as, edit_as, note_as, queue_as, set_status_as, user_history_of, SafetyQueueFilter,
        SafetyUserHistory,
    },
};

fn stepped_up(staff: &StaffContext) -> bool {
    staff.session.step_up_until.is_some_and(|until| staff.now < until)
}

/// Whether the staff member could make senior decisions after a step-up
fn can_be_senior(staff: &StaffContext) -> bool {
    staff.has(StaffPermission::ReportsResolve) && staff.has(StaffPermission::UsersSuspend)
}

/// Trust & Safety permissions a staff session carries
pub(crate) fn bridged(staff: &StaffContext) -> PlatformPermissions {
    let mut set = BTreeSet::new();
    if staff.has(StaffPermission::ReportsRead) || staff.has(StaffPermission::ReportsResolve) {
        set.insert(PlatformPermission::SafetyReview);
    }
    if staff.has(StaffPermission::ReportsResolve) {
        set.insert(PlatformPermission::SafetyAction);
        if can_be_senior(staff) && stepped_up(staff) {
            set.insert(PlatformPermission::SafetyManage);
        }
    }
    PlatformPermissions(set)
}

/// A refusal from the shared case rules, in staff terms: a missing senior
/// right the staff member could unlock becomes "step up first"
fn translate(staff: &StaffContext, error: Error) -> Error {
    let ErrorType::MissingPlatformPermission { permission } = &error.error_type else {
        return error;
    };
    let staff_permission = match permission.as_str() {
        "safety.review" => StaffPermission::ReportsRead,
        "safety.action" => StaffPermission::ReportsResolve,
        _ => {
            if can_be_senior(staff) && !stepped_up(staff) {
                return create_error!(NotElevated);
            }
            StaffPermission::UsersSuspend
        }
    };
    create_error!(MissingPlatformPermission {
        permission: staff_permission.name().to_string()
    })
}

fn is_denial(error: &Error) -> bool {
    matches!(
        error.error_type,
        ErrorType::MissingPlatformPermission { .. } | ErrorType::NotElevated | ErrorType::NotPrivileged
    )
}

/// Record, then run a change. A change that fails afterwards gets a second
/// record saying so (denied or failed), so the log never shows a change
/// that did not happen as the last word.
async fn audited<T>(
    db: &Database,
    staff: &StaffContext,
    record: AuditRecord,
    change: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    match change.await {
        Ok(value) => Ok(value),
        Err(error) => {
            let error = translate(staff, error);
            let mut failed = record;
            failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
            let outcome = if is_denial(&error) {
                AuditOutcome::Denied
            } else {
                AuditOutcome::Failed
            };
            // best effort: the first record already shows the attempt
            let _ = record_platform_audit(db, failed, outcome).await;
            Err(error)
        }
    }
}

fn case_record(staff: &StaffContext, action: &str, permission: StaffPermission, id: &str) -> AuditRecord {
    let mut record = staff.record(action, Some(permission));
    record.target_kind = Some("safety_case".to_string());
    record.target_id = Some(id.to_string());
    record
}

fn note_reason(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(500).collect())
}

/// # Report queue
#[get("/safety/cases?<status>&<kind>&<category>&<priority>&<assignee>&<search>&<before>&<oldest>&<limit>")]
#[allow(clippy::too_many_arguments)]
pub async fn cases(
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
    kind: Option<&str>,
    category: Option<&str>,
    priority: Option<&str>,
    assignee: Option<&str>,
    search: Option<&str>,
    before: Option<&str>,
    oldest: Option<bool>,
    limit: Option<usize>,
) -> Result<Json<SafetyCasePage>> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    let filter = SafetyQueueFilter {
        status: status.map(str::to_string),
        kind: kind.map(str::to_string),
        category: category.map(str::to_string),
        priority: priority.map(str::to_string),
        assignee: assignee.map(str::to_string),
        search: search.map(str::to_string),
        before: before.map(str::to_string),
        oldest: oldest.unwrap_or(false),
        limit,
    };
    queue_as(db, &staff.user, &bridged(&staff), filter)
        .await
        .map(Json)
        .map_err(|e| translate(&staff, e))
}

/// # Case
///
/// Shows evidence (the reported content), so opening a case is recorded.
#[get("/safety/cases/<id>")]
pub async fn case(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<SafetyCaseDetails>> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    // the case must exist before we log looking at it
    db.fetch_safety_case(id).await?;
    let record = case_record(&staff, "staff.safety.case.view", StaffPermission::ReportsRead, id);
    audited(db, &staff, record, case_as(db, &staff.user, &bridged(&staff), id))
        .await
        .map(Json)
}

/// # Change case status
#[post("/safety/cases/<id>/status", data = "<data>")]
pub async fn set_status(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataSafetyCaseStatus>,
) -> Result<Json<SafetyCase>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    let mut record = case_record(&staff, "staff.safety.case.status", StaffPermission::ReportsResolve, id);
    record.summary = Some(format!("{:?}", data.status));
    record.reason = note_reason(data.note.as_deref().or(data.public_note.as_deref()));
    audited(db, &staff, record, set_status_as(db, &staff.user, &bridged(&staff), id, data))
        .await
        .map(Json)
}

/// # Edit case
///
/// Assign, prioritise, mark as duplicate, legal hold (senior).
#[patch("/safety/cases/<id>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataSafetyCaseEdit>,
) -> Result<Json<SafetyCase>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    let mut record = case_record(&staff, "staff.safety.case.edit", StaffPermission::ReportsResolve, id);
    let mut parts = vec![];
    if let Some(a) = &data.assignee_id {
        parts.push(format!("assignee={}", if a.is_empty() { "none" } else { a }));
    }
    if let Some(p) = data.priority {
        parts.push(format!("priority={p:?}"));
    }
    if let Some(d) = &data.duplicate_of {
        parts.push(format!("duplicate_of={d}"));
    }
    if let Some(h) = data.legal_hold {
        parts.push(format!("legal_hold={h}"));
    }
    record.summary = Some(parts.join(" "));
    audited(db, &staff, record, edit_as(db, &staff.user, &bridged(&staff), id, data))
        .await
        .map(Json)
}

/// # Internal note
#[post("/safety/cases/<id>/notes", data = "<data>")]
pub async fn note(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataSafetyNote>,
) -> Result<Json<SafetyCase>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    // the note itself stays in the case trail, shown to reviewers only
    let record = case_record(&staff, "staff.safety.case.note", StaffPermission::ReportsResolve, id);
    audited(db, &staff, record, note_as(db, &staff.user, &bridged(&staff), id, data.into_inner()))
        .await
        .map(Json)
}

fn affects_account(kind: SafetyActionKind) -> bool {
    matches!(kind, SafetyActionKind::SuspendAccount | SafetyActionKind::BanAccount)
}

/// # Take action
///
/// Warnings, message removal, bot and listing actions need
/// `platform.reports.resolve`; suspending or banning an account also needs
/// `platform.users.suspend` and a fresh step-up.
#[post("/safety/cases/<id>/actions", data = "<data>")]
pub async fn take(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataSafetyAction>,
) -> Result<Json<SafetyAction>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    if affects_account(data.kind) {
        staff.require_step_up(db, StaffPermission::UsersSuspend).await?;
    }
    let case = db.fetch_safety_case(id).await?;
    let mut record = case_record(&staff, "staff.safety.action.take", StaffPermission::ReportsResolve, id);
    record.reason = note_reason(Some(&data.public_reason));
    record.summary = Some(match data.duration_days {
        Some(days) if data.kind == SafetyActionKind::SuspendAccount => {
            format!("{:?} {days}d subject={}", data.kind, case.target_owner_id.as_deref().unwrap_or("-"))
        }
        _ => format!("{:?} subject={}", data.kind, case.target_owner_id.as_deref().unwrap_or("-")),
    });
    audited(db, &staff, record, take_as(db, &staff.user, &bridged(&staff), id, data))
        .await
        .map(Json)
}

/// # Revoke action
#[post("/safety/actions/<id>/revoke", data = "<data>")]
pub async fn revoke(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataSafetyRevoke>,
) -> Result<Json<SafetyAction>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let action = db.fetch_safety_action(id).await?;
    if affects_account(action.kind) {
        staff.require_step_up(db, StaffPermission::UsersSuspend).await?;
    }
    let data = data.into_inner();
    let mut record = staff.record("staff.safety.action.revoke", Some(StaffPermission::ReportsResolve));
    record.target_kind = Some("safety_action".to_string());
    record.target_id = Some(action.id.clone());
    record.reason = note_reason(Some(&data.reason));
    record.summary = Some(format!(
        "{:?} subject={}",
        action.kind,
        action.subject_user_id.as_deref().unwrap_or("-")
    ));
    let note = data.reason.clone();
    let revoked = audited(db, &staff, record, revoke_as(db, &staff.user, &bridged(&staff), id, data)).await?;
    if let Some(subject) = &revoked.subject_user_id {
        crate::util::official::action_revoked(db, subject, &note).await;
    }
    Ok(Json(revoked))
}

/// # Appeal queue
#[get("/safety/appeals?<status>&<before>&<limit>")]
pub async fn appeals(
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
    before: Option<&str>,
    limit: Option<usize>,
) -> Result<Json<Vec<SafetyAppeal>>> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    appeals_of(db, status, before, limit).await.map(Json)
}

/// # Decide appeal
#[post("/safety/appeals/<id>", data = "<data>")]
pub async fn decide(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataSafetyAppealDecision>,
) -> Result<Json<SafetyAppeal>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    let mut record = staff.record("staff.safety.appeal.decide", Some(StaffPermission::ReportsResolve));
    record.target_kind = Some("safety_appeal".to_string());
    record.target_id = Some(id.to_string());
    record.reason = note_reason(Some(&data.note));
    record.summary = Some(if data.overturn { "overturn" } else { "uphold" }.to_string());
    audited(db, &staff, record, decide_as(db, &staff.user, &bridged(&staff), id, data))
        .await
        .map(Json)
}

/// # Account history
///
/// Actions, appeals and reports concerning one account.
#[get("/safety/users/<id>")]
pub async fn user_history(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
) -> Result<Json<SafetyUserHistory>> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    db.fetch_user(id).await?;
    user_history_of(db, id).await.map(Json)
}

/// Action taken from the user page
#[derive(serde::Deserialize)]
pub struct DataUserAction {
    pub kind: SafetyActionKind,
    pub category: SafetyCategory,
    /// Shown to the user in their account standing
    pub public_reason: String,
    pub internal_note: Option<String>,
    pub duration_days: Option<u32>,
}

/// # Act on an account directly
///
/// Warn, suspend (days) or ban an account from the user page. A staff case
/// is opened for it, so the action shows in the account's standing, can be
/// appealed and reversed like any report action. Suspending and banning
/// need `platform.users.suspend` and a fresh step-up.
#[post("/users/<id>/actions", data = "<data>")]
pub async fn user_action(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataUserAction>,
) -> Result<Json<SafetyAction>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    if !matches!(
        data.kind,
        SafetyActionKind::Warning | SafetyActionKind::SuspendAccount | SafetyActionKind::BanAccount
    ) {
        return Err(create_error!(InvalidOperation));
    }
    if affects_account(data.kind) {
        staff.require_step_up(db, StaffPermission::UsersSuspend).await?;
    }
    let reason = data.public_reason.trim().to_string();
    if reason.chars().count() < 3 || reason.chars().count() > 500 {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length("public_reason", 3, 500),
        ]));
    }
    // refuse protected accounts before a case is opened
    let subject = db.fetch_user(id).await?;
    if subject.id == staff.user.id {
        return Err(create_error!(CannotReportYourself));
    }
    if subject.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let protected = subject.privileged
        || db
            .fetch_staff_grant(&subject.id)
            .await?
            .is_some_and(|grant| grant.is_active(staff.now));
    let mut record = staff.record("staff.user.action", Some(StaffPermission::ReportsResolve));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(subject.id.clone());
    record.reason = note_reason(Some(&reason));
    record.summary = Some(match data.duration_days {
        Some(days) if data.kind == SafetyActionKind::SuspendAccount => format!("{:?} {days}d", data.kind),
        _ => format!("{:?}", data.kind),
    });
    if protected {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }

    let perms = bridged(&staff);
    let user = staff.user.clone();
    audited(db, &staff, record, async {
        let case = open_staff_case(
            db,
            &user,
            SafetyTargetKind::User,
            &subject,
            &subject.id,
            data.category,
            data.internal_note.as_deref().unwrap_or(&reason),
        )
        .await?;
        take_as(
            db,
            &user,
            &perms,
            &case.id,
            DataSafetyAction {
                kind: data.kind,
                category: Some(data.category),
                public_reason: reason.clone(),
                internal_note: data.internal_note.clone(),
                duration_days: data.duration_days,
                reporter_note: None,
                version: case.version,
            },
        )
        .await
    })
    .await
    .map(Json)
}
