//! Badge program applications in the admin console: the review queue, one
//! application with its evidence and live checklist, the decision with
//! reasons, appeals and Partner rechecks.
//!
//! - `platform.badges.manage`: see the queue and applications (opening the
//!   evidence is written to the audit log), take one
//! - deciding (approve, refuse, more information), appeals and rechecks:
//!   `platform.badges.manage` and a fresh step-up; Verified Server and
//!   Verified Bot also `platform.servers.restrict`
//! - nobody reviews an application they made or one for a server or bot
//!   they own
//! - Partner: two people. The first approval opens the Partner badge
//!   request through the badge gate; a different reviewer approves it here
//!   (or in the badge requests list) and the partnership starts
//! - Verified Server as an official entity below the numbers: a staff Owner
//!   approves first, a different reviewer second
//! - an appeal is decided by someone other than the reviewer who refused
//!
//! The applicant hears every decision in their language, each reason
//! written out (`util::official::program_decision`).

use revolt_config::config;
use revolt_database::{
    application_audit, approve_badge_request, approve_dates, checked_decision, end_application,
    mutate_badge, program_eligibility, record_review, refuse_badge_request, refuse_dates,
    reviewer_conflict, save_application, sync_verified_server_owner, target_owner, AppealStatus,
    ApplicationStatus, AuditOutcome, BadgeActor, BadgeApplication, BadgeChange, BadgeMutation,
    BadgeOutcome, BadgePath, BadgeProgram, BadgeRequestStatus, Database, PartialServer,
    StaffPermission, IMPROVE_DAYS, SERVER_FLAG_PARTNER, SERVER_FLAG_VERIFIED,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};

use super::StaffContext;
use crate::routes::badges::programs::{check_view, ownership_view, target_name};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The badge rights of this staff session (for the Partner badge request)
fn actor(staff: &StaffContext) -> BadgeActor {
    BadgeActor {
        id: staff.user.id.clone(),
        path: BadgePath::StaffConsole,
        staff: staff.has(StaffPermission::BadgesManage),
        owner: staff.grant.is_owner(staff.now),
        step_up: staff.session.step_up_until.is_some_and(|until| staff.now < until),
        session_id: Some(staff.session.id.clone()),
    }
}

fn invalid(code: &str) -> revolt_result::Error {
    create_error!(FailedValidation { error: code.to_string() })
}

/// One row of the queue
#[derive(Serialize, Debug)]
pub struct ApplicationRow {
    pub id: String,
    pub program: String,
    pub status: String,
    pub applicant_id: String,
    pub applicant_name: Option<String>,
    pub target_id: String,
    pub target_name: Option<String>,
    pub exception: bool,
    pub created_at: i64,
    pub submitted_at: Option<i64>,
    pub reviewer_id: Option<String>,
    /// Two-person approvals: who approved first (waits for someone else)
    pub first_approval_by: Option<String>,
    pub appeal_status: Option<String>,
    pub recheck_due: bool,
    /// You made it or own its server or bot: you can't review it
    pub conflict: bool,
}

#[derive(Serialize, Debug)]
pub struct EvidenceView {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub created_at: i64,
}

#[derive(Serialize, Debug)]
pub struct ReviewView {
    pub reviewer_id: String,
    pub action: String,
    pub reasons: Vec<String>,
    pub note: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, Debug)]
pub struct AppealView {
    pub text: String,
    pub status: String,
    pub created_at: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<i64>,
    pub reasons: Vec<String>,
    pub note: Option<String>,
}

/// One application, as a reviewer sees it
#[derive(Serialize, Debug)]
pub struct ApplicationDetail {
    #[serde(flatten)]
    pub row: ApplicationRow,
    pub statement: String,
    pub links: Vec<String>,
    pub authorized: bool,
    pub ownership: Option<revolt_models::v0::ApplicationOwnership>,
    /// The checklist when it was sent
    pub checks: Vec<revolt_models::v0::ProgramCheck>,
    /// The checklist measured now
    pub live_checks: Vec<revolt_models::v0::ProgramCheck>,
    pub decided_at: Option<i64>,
    pub decided_by: Option<String>,
    pub reasons: Vec<String>,
    pub note: Option<String>,
    pub reapply_after: Option<i64>,
    pub appeal_until: Option<i64>,
    pub appeal: Option<AppealView>,
    pub next_review_at: Option<i64>,
    pub improve_until: Option<i64>,
    pub ended_at: Option<i64>,
    pub end_reason: Option<String>,
    pub badge_request_id: Option<String>,
    pub evidence: Vec<EvidenceView>,
    pub evidence_purged: bool,
    pub reviews: Vec<ReviewView>,
    /// Reason codes a reviewer may give for this program
    pub reason_codes: Vec<String>,
}

async fn row(db: &Database, application: &BadgeApplication, viewer: &str) -> Result<ApplicationRow> {
    let owner = target_owner(db, application.program, &application.target_id).await?;
    Ok(ApplicationRow {
        id: application.id.clone(),
        program: application.program.name().to_string(),
        status: application.status.name().to_string(),
        applicant_id: application.applicant_id.clone(),
        applicant_name: db.fetch_user(&application.applicant_id).await.ok().map(|u| u.username),
        target_id: application.target_id.clone(),
        target_name: target_name(db, application).await,
        exception: application.exception,
        created_at: application.created_at,
        submitted_at: application.submitted_at,
        reviewer_id: application.reviewer_id.clone(),
        first_approval_by: application.first_approval_by.clone(),
        appeal_status: application.appeal.as_ref().map(|a| a.status.name().to_string()),
        recheck_due: application.recheck_due,
        conflict: reviewer_conflict(application, owner.as_deref(), viewer),
    })
}

/// # Badge applications
///
/// The review queue: `status` (Submitted, InReview, MoreInfo, Approved,
/// Refused, Withdrawn, Ended; default: everything waiting for a person,
/// appeals and rechecks included) and `program` (partner, verified_server,
/// verified_bot). Drafts are never shown.
#[get("/badges/applications?<status>&<program>")]
pub async fn queue(
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
    program: Option<&str>,
) -> Result<Json<Vec<ApplicationRow>>> {
    staff.require(db, StaffPermission::BadgesManage).await?;
    let program = match program {
        None => None,
        Some(name) => Some(BadgeProgram::from_name(name).ok_or_else(|| invalid("program"))?),
    };
    let list: Vec<BadgeApplication> = match status {
        Some(name) => {
            let status = ApplicationStatus::from_name(name)
                .filter(|s| *s != ApplicationStatus::Draft)
                .ok_or_else(|| invalid("status"))?;
            db.fetch_badge_applications(Some(status), program, 200).await?
        }
        None => {
            let mut waiting = vec![];
            for status in [ApplicationStatus::Submitted, ApplicationStatus::InReview] {
                waiting.extend(db.fetch_badge_applications(Some(status), program, 200).await?);
            }
            // open appeals and due rechecks wait for a person too
            for status in [ApplicationStatus::Refused, ApplicationStatus::Approved] {
                waiting.extend(
                    db.fetch_badge_applications(Some(status), program, 200)
                        .await?
                        .into_iter()
                        .filter(|a| {
                            a.recheck_due || a.appeal.as_ref().is_some_and(|appeal| appeal.status == AppealStatus::Open)
                        }),
                );
            }
            waiting.sort_by(|a, b| a.id.cmp(&b.id));
            waiting
        }
    };
    let mut rows = vec![];
    for application in &list {
        rows.push(row(db, application, &staff.user.id).await?);
    }
    Ok(Json(rows))
}

async fn detail_of(db: &Database, staff: &StaffContext, application: &BadgeApplication) -> Result<ApplicationDetail> {
    let programs = config().await.api.badge_programs;
    let live = match db.fetch_user(&application.applicant_id).await {
        Ok(applicant) => program_eligibility(
            db,
            &programs,
            application.program,
            &applicant,
            &application.target_id,
            application.exception,
            application.ownership.as_ref().is_some_and(|o| o.verified_at.is_some()),
            staff.now,
        )
        .await?
        .items
        .iter()
        .map(check_view)
        .collect(),
        Err(_) => vec![],
    };
    let evidence = if application.evidence_purged {
        vec![]
    } else {
        db.fetch_badge_evidence(&application.id).await?
    };
    Ok(ApplicationDetail {
        row: row(db, application, &staff.user.id).await?,
        statement: application.statement.clone(),
        links: application.links.clone(),
        authorized: application.authorized,
        ownership: ownership_view(application),
        checks: application.eligibility.iter().map(check_view).collect(),
        live_checks: live,
        decided_at: application.decided_at,
        decided_by: application.decided_by.clone(),
        reasons: application.reasons.clone(),
        note: application.note.clone(),
        reapply_after: application.reapply_after,
        appeal_until: application.appeal_until,
        appeal: application.appeal.as_ref().map(|a| AppealView {
            text: a.text.clone(),
            status: a.status.name().to_string(),
            created_at: a.created_at,
            decided_by: a.decided_by.clone(),
            decided_at: a.decided_at,
            reasons: a.reasons.clone(),
            note: a.note.clone(),
        }),
        next_review_at: application.next_review_at,
        improve_until: application.improve_until,
        ended_at: application.ended_at,
        end_reason: application.end_reason.clone(),
        badge_request_id: application.badge_request_id.clone(),
        evidence: evidence
            .into_iter()
            .map(|e| EvidenceView {
                id: e.id,
                kind: e.kind,
                text: e.text,
                created_at: e.created_at,
            })
            .collect(),
        evidence_purged: application.evidence_purged,
        reviews: db
            .fetch_badge_reviews(&application.id)
            .await?
            .into_iter()
            .map(|r| ReviewView {
                reviewer_id: r.reviewer_id,
                action: r.action,
                reasons: r.reasons,
                note: r.note,
                created_at: r.created_at,
            })
            .collect(),
        reason_codes: revolt_database::reason_codes(application.program).iter().map(|c| c.to_string()).collect(),
    })
}

/// Load an application a reviewer may act on: not a draft, and not theirs
/// or their server's or bot's (refusals are written to the audit log)
async fn reviewable(db: &Database, staff: &StaffContext, id: &str, action: &str) -> Result<BadgeApplication> {
    let application = db.fetch_badge_application(id).await?;
    if application.status == ApplicationStatus::Draft {
        return Err(create_error!(NotFound));
    }
    let owner = target_owner(db, application.program, &application.target_id).await?;
    if reviewer_conflict(&application, owner.as_deref(), &staff.user.id) {
        application_audit(
            db,
            &staff.user.id,
            Some(staff.session.id.clone()),
            &format!("staff.badge.application.{action}"),
            &application,
            AuditOutcome::Denied,
            "denied:own_application",
        )
        .await?;
        return Err(create_error!(NotPrivileged));
    }
    Ok(application)
}

/// # One badge application
///
/// With its evidence (the opening is written to the audit log), the
/// checklist then and now, and every review step.
#[get("/badges/applications/<id>")]
pub async fn detail(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<ApplicationDetail>> {
    staff.require(db, StaffPermission::BadgesManage).await?;
    let application = reviewable(db, &staff, id, "view").await?;
    application_audit(
        db,
        &staff.user.id,
        Some(staff.session.id.clone()),
        "staff.badge.application.view",
        &application,
        AuditOutcome::Success,
        "evidence opened",
    )
    .await?;
    detail_of(db, &staff, &application).await.map(Json)
}

/// # Take an application
///
/// It moves from the queue to "in review" with your name on it.
#[post("/badges/applications/<id>/take")]
pub async fn take(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<ApplicationDetail>> {
    staff.require(db, StaffPermission::BadgesManage).await?;
    let mut application = reviewable(db, &staff, id, "take").await?;
    if application.status != ApplicationStatus::Submitted {
        return Err(invalid("not_waiting"));
    }
    let before = application.updated_at;
    application.status = ApplicationStatus::InReview;
    application.reviewer_id = Some(staff.user.id.clone());
    save_application(db, &mut application, before, staff.now).await?;
    record_review(db, &application, &staff.user.id, "take", &[], None, staff.now).await?;
    application_audit(db, &staff.user.id, Some(staff.session.id.clone()), "staff.badge.application.take", &application, AuditOutcome::Success, "").await?;
    detail_of(db, &staff, &application).await.map(Json)
}

#[derive(Deserialize)]
pub struct DataDecision {
    /// `approve`, `refuse` or `more_info`
    pub decision: String,
    pub reasons: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Rights for a decision on this program: badges.manage with a step-up;
/// the marks also servers.restrict
async fn require_decision_rights(db: &Database, staff: &StaffContext, program: BadgeProgram) -> Result<()> {
    staff.require_step_up(db, StaffPermission::BadgesManage).await?;
    if program != BadgeProgram::Partner {
        staff.require(db, StaffPermission::ServersRestrict).await?;
    }
    Ok(())
}

/// The application is approved: the mark is set (Verified Server, Verified
/// Bot) and the record closed. Partner's badge was given by the gate already.
async fn finish_approval(
    db: &Database,
    staff: &StaffContext,
    mut application: BadgeApplication,
    reasons: Vec<String>,
    note: Option<String>,
) -> Result<BadgeApplication> {
    let programs = config().await.api.badge_programs;
    match application.program {
        BadgeProgram::Partner | BadgeProgram::VerifiedServer => {
            let mut server = db.fetch_server(&application.target_id).await?;
            let flag = if application.program == BadgeProgram::Partner { SERVER_FLAG_PARTNER } else { SERVER_FLAG_VERIFIED };
            let flags = server.flags.unwrap_or_default() | flag;
            server.update(db, PartialServer { flags: Some(flags), ..Default::default() }, vec![]).await?;
            if application.program == BadgeProgram::VerifiedServer {
                // the owner's badge follows the mark, through the gate
                sync_verified_server_owner(db, &server.owner).await?;
            }
        }
        BadgeProgram::VerifiedBot => {
            super::bots::set_verified(db, &application.target_id, true, 0).await?;
        }
    }
    let before = application.updated_at;
    application.status = ApplicationStatus::Approved;
    application.decided_at = Some(staff.now);
    application.decided_by = Some(staff.user.id.clone());
    application.reasons = reasons;
    application.note = note;
    approve_dates(&mut application, &programs, staff.now);
    save_application(db, &mut application, before, staff.now).await?;
    record_review(db, &application, &staff.user.id, "approve", &application.reasons, application.note.as_deref(), staff.now).await?;
    application_audit(
        db,
        &staff.user.id,
        Some(staff.session.id.clone()),
        "staff.badge.application.approve",
        &application,
        AuditOutcome::Success,
        &application.reasons.join(","),
    )
    .await?;
    crate::util::official::program_decision(
        db,
        &application.applicant_id,
        "program.approved",
        application.program.name(),
        &application.reasons,
        application.note.as_deref(),
        &[],
    )
    .await;
    Ok(application)
}

/// First of two approvals: Partner opens its badge request through the
/// gate; the official-entity exception notes who approved first
async fn first_approval(
    db: &Database,
    staff: &StaffContext,
    mut application: BadgeApplication,
    reasons: &[String],
    note: Option<&str>,
) -> Result<BadgeApplication> {
    let before = application.updated_at;
    application.status = ApplicationStatus::InReview;
    application.reviewer_id = Some(staff.user.id.clone());
    application.first_approval_by = Some(staff.user.id.clone());
    save_application(db, &mut application, before, staff.now).await?;
    if application.program == BadgeProgram::Partner {
        let outcome = mutate_badge(
            db,
            &actor(staff),
            BadgeMutation {
                user_id: application.applicant_id.clone(),
                badge: "partner".to_string(),
                change: BadgeChange::Grant,
                reason: format!("partner application {}", application.id),
                expires_at: None,
                source_ref: None,
            },
        )
        .await;
        let request = match outcome {
            Ok(BadgeOutcome::Pending(request)) => request,
            Ok(BadgeOutcome::Applied(_)) => return Err(create_error!(InternalError)),
            Err(error) => {
                // nothing was asked: the first approval does not stand
                let before = application.updated_at;
                application.first_approval_by = None;
                let _ = save_application(db, &mut application, before, staff.now).await;
                return Err(error);
            }
        };
        let before = application.updated_at;
        application.badge_request_id = Some(request.id);
        save_application(db, &mut application, before, staff.now).await?;
    }
    record_review(db, &application, &staff.user.id, "approve_first", reasons, note, staff.now).await?;
    application_audit(
        db,
        &staff.user.id,
        Some(staff.session.id.clone()),
        "staff.badge.application.approve_first",
        &application,
        AuditOutcome::Success,
        "waits for a second person",
    )
    .await?;
    Ok(application)
}

/// # Decide an application
///
/// `approve`, `refuse` or `more_info`, with reason codes (and a note,
/// required with `other`). Approving measures the checklist again.
#[post("/badges/applications/<id>/decide", data = "<data>")]
pub async fn decide(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataDecision>,
) -> Result<Json<ApplicationDetail>> {
    staff.require(db, StaffPermission::BadgesManage).await?;
    let application = reviewable(db, &staff, id, "decide").await?;
    require_decision_rights(db, &staff, application.program).await?;
    if !matches!(application.status, ApplicationStatus::Submitted | ApplicationStatus::InReview) {
        return Err(invalid("not_waiting"));
    }
    let (reasons, note) = checked_decision(application.program, &data.reasons, data.note.as_deref())?;
    let programs = config().await.api.badge_programs;
    let application = match data.decision.as_str() {
        "approve" => {
            // the numbers still hold now
            let applicant = db.fetch_user(&application.applicant_id).await?;
            let ownership = application.ownership.as_ref().is_some_and(|o| o.verified_at.is_some());
            let checklist = program_eligibility(
                db,
                &programs,
                application.program,
                &applicant,
                &application.target_id,
                application.exception,
                ownership,
                staff.now,
            )
            .await?;
            let missing = checklist.missing();
            if !missing.is_empty() {
                return Err(invalid(&format!("not_eligible:{}", missing.join(","))));
            }
            let two_people = application.program == BadgeProgram::Partner
                || (application.program == BadgeProgram::VerifiedServer && application.exception);
            if !two_people {
                finish_approval(db, &staff, application, reasons, note).await?
            } else if let Some(first) = application.first_approval_by.clone() {
                if first == staff.user.id {
                    application_audit(db, &staff.user.id, Some(staff.session.id.clone()), "staff.badge.application.approve", &application, AuditOutcome::Denied, "denied:same_person").await?;
                    return Err(create_error!(NotPrivileged));
                }
                if application.program == BadgeProgram::Partner {
                    let request_id = application.badge_request_id.clone().ok_or_else(|| invalid("no_request"))?;
                    let request = db.fetch_badge_request(&request_id).await?;
                    // approved meanwhile in the badge requests list: only the record is left
                    if request.status != BadgeRequestStatus::Approved {
                        approve_badge_request(db, &actor(&staff), &request_id).await?;
                    }
                }
                finish_approval(db, &staff, application, reasons, note).await?
            } else {
                // the official-entity exception starts with a staff Owner
                if application.exception && !staff.grant.is_owner(staff.now) {
                    application_audit(db, &staff.user.id, Some(staff.session.id.clone()), "staff.badge.application.approve_first", &application, AuditOutcome::Denied, "denied:owner_only").await?;
                    return Err(create_error!(NotPrivileged));
                }
                first_approval(db, &staff, application, &reasons, note.as_deref()).await?
            }
        }
        "refuse" | "more_info" => {
            let refuse = data.decision == "refuse";
            let mut application = application;
            // a first approval waiting for a second person no longer stands
            if let Some(request_id) = application.badge_request_id.take() {
                if let Ok(request) = db.fetch_badge_request(&request_id).await {
                    if request.status == BadgeRequestStatus::Pending {
                        let _ = refuse_badge_request(db, &actor(&staff), &request_id).await;
                    }
                }
            }
            let before = application.updated_at;
            application.status = if refuse { ApplicationStatus::Refused } else { ApplicationStatus::MoreInfo };
            application.first_approval_by = None;
            application.reviewer_id = Some(staff.user.id.clone());
            application.decided_at = Some(staff.now);
            application.decided_by = Some(staff.user.id.clone());
            application.reasons = reasons;
            application.note = note;
            if refuse {
                refuse_dates(&mut application, &programs, staff.now);
            }
            save_application(db, &mut application, before, staff.now).await?;
            let action = if refuse { "refuse" } else { "more_info" };
            record_review(db, &application, &staff.user.id, action, &application.reasons, application.note.as_deref(), staff.now).await?;
            application_audit(
                db,
                &staff.user.id,
                Some(staff.session.id.clone()),
                &format!("staff.badge.application.{action}"),
                &application,
                AuditOutcome::Success,
                &application.reasons.join(","),
            )
            .await?;
            let mut dates = vec![];
            if let (Some(until), Some(after)) = (application.appeal_until, application.reapply_after) {
                dates = vec![("appeal_until", until), ("reapply_after", after)];
            }
            crate::util::official::program_decision(
                db,
                &application.applicant_id,
                if refuse { "program.refused" } else { "program.more_info" },
                application.program.name(),
                &application.reasons,
                application.note.as_deref(),
                &dates,
            )
            .await;
            application
        }
        _ => return Err(invalid("decision")),
    };
    detail_of(db, &staff, &application).await.map(Json)
}

#[derive(Deserialize)]
pub struct DataAppealDecision {
    /// true: the refusal is set aside and the application is reviewed again
    pub uphold: bool,
    pub reasons: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// # Decide an appeal
///
/// By someone other than the reviewer who refused.
#[post("/badges/applications/<id>/appeal", data = "<data>")]
pub async fn decide_appeal(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataAppealDecision>,
) -> Result<Json<ApplicationDetail>> {
    staff.require(db, StaffPermission::BadgesManage).await?;
    let mut application = reviewable(db, &staff, id, "appeal").await?;
    require_decision_rights(db, &staff, application.program).await?;
    let open = application.appeal.as_ref().is_some_and(|a| a.status == AppealStatus::Open);
    if application.status != ApplicationStatus::Refused || !open {
        return Err(invalid("no_open_appeal"));
    }
    if application.decided_by.as_deref() == Some(staff.user.id.as_str()) {
        application_audit(db, &staff.user.id, Some(staff.session.id.clone()), "staff.badge.application.appeal", &application, AuditOutcome::Denied, "denied:same_reviewer").await?;
        return Err(create_error!(NotPrivileged));
    }
    let (reasons, note) = checked_decision(application.program, &data.reasons, data.note.as_deref())?;
    let programs = config().await.api.badge_programs;
    let before = application.updated_at;
    if let Some(appeal) = application.appeal.as_mut() {
        appeal.status = if data.uphold { AppealStatus::Upheld } else { AppealStatus::Rejected };
        appeal.decided_by = Some(staff.user.id.clone());
        appeal.decided_at = Some(staff.now);
        appeal.reasons = reasons.clone();
        appeal.note = note.clone();
    }
    if data.uphold {
        // back to the queue, as if just sent
        application.status = ApplicationStatus::Submitted;
        application.reviewer_id = None;
        application.reapply_after = None;
        application.appeal_until = None;
        application.evidence_purge_at = None;
        application.purge_at = None;
    } else {
        application.evidence_purge_at = Some(staff.now + programs.evidence_days * DAY_MS);
    }
    save_application(db, &mut application, before, staff.now).await?;
    let action = if data.uphold { "appeal_upheld" } else { "appeal_rejected" };
    record_review(db, &application, &staff.user.id, action, &reasons, note.as_deref(), staff.now).await?;
    application_audit(db, &staff.user.id, Some(staff.session.id.clone()), &format!("staff.badge.application.{action}"), &application, AuditOutcome::Success, &reasons.join(",")).await?;
    let dates: Vec<(&str, i64)> = application.reapply_after.map(|at| vec![("reapply_after", at)]).unwrap_or_default();
    crate::util::official::program_decision(
        db,
        &application.applicant_id,
        &format!("program.{action}"),
        application.program.name(),
        &reasons,
        note.as_deref(),
        &dates,
    )
    .await;
    detail_of(db, &staff, &application).await.map(Json)
}

#[derive(Deserialize)]
pub struct DataRecheck {
    /// `keep`, `warn` (30 days to improve) or `end`
    pub decision: String,
    pub reasons: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// # Recheck a partnership
///
/// crond flags each partnership after 180 days; a person decides. Ending it
/// here is for conditions no longer met; a removal for a rule breach goes
/// through Trust & Safety with a statement of reasons.
#[post("/badges/applications/<id>/recheck", data = "<data>")]
pub async fn recheck(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataRecheck>,
) -> Result<Json<ApplicationDetail>> {
    staff.require(db, StaffPermission::BadgesManage).await?;
    let mut application = reviewable(db, &staff, id, "recheck").await?;
    require_decision_rights(db, &staff, application.program).await?;
    if application.status != ApplicationStatus::Approved || application.program != BadgeProgram::Partner {
        return Err(invalid("not_a_partnership"));
    }
    let (reasons, note) = checked_decision(application.program, &data.reasons, data.note.as_deref())?;
    let programs = config().await.api.badge_programs;
    let action = match data.decision.as_str() {
        "keep" | "warn" => {
            let before = application.updated_at;
            application.recheck_due = false;
            if data.decision == "keep" {
                application.improve_until = None;
                application.next_review_at = Some(staff.now + programs.partner_review_days * DAY_MS);
            } else {
                let until = staff.now + IMPROVE_DAYS * DAY_MS;
                application.improve_until = Some(until);
                application.next_review_at = Some(until);
            }
            save_application(db, &mut application, before, staff.now).await?;
            if data.decision == "keep" { "recheck_keep" } else { "recheck_warning" }
        }
        "end" => {
            let server = db.fetch_server(&application.target_id).await;
            application = end_application(db, &programs, application, &staff.user.id, "recheck", staff.now).await?;
            if let Ok(mut server) = server {
                let flags = server.flags.unwrap_or_default() & !SERVER_FLAG_PARTNER;
                server.update(db, PartialServer { flags: Some(flags), ..Default::default() }, vec![]).await?;
            }
            "recheck_end"
        }
        _ => return Err(invalid("decision")),
    };
    record_review(db, &application, &staff.user.id, action, &reasons, note.as_deref(), staff.now).await?;
    application_audit(db, &staff.user.id, Some(staff.session.id.clone()), &format!("staff.badge.application.{action}"), &application, AuditOutcome::Success, &reasons.join(",")).await?;
    match action {
        "recheck_keep" => {
            crate::util::official::program_decision(db, &application.applicant_id, "program.recheck_kept", "partner", &reasons, note.as_deref(), &[("next_review", application.next_review_at.unwrap_or(staff.now))]).await
        }
        "recheck_warning" => {
            crate::util::official::program_decision(db, &application.applicant_id, "program.recheck_warning", "partner", &reasons, note.as_deref(), &[("improve_until", application.improve_until.unwrap_or(staff.now))]).await
        }
        _ => crate::util::official::program_ended(db, &application.applicant_id, "partner", "recheck", note.as_deref()).await,
    }
    detail_of(db, &staff, &application).await.map(Json)
}
