//! Plugin review and bug reports in the admin console. Same functions as
//! `/plugins/review/*` and `/badges/bugs/admin/*` (which stay limited to
//! the app permissions `plugins.review` and `bugs.*`), through a staff
//! session:
//!
//! - plugins: `platform.reports.read` sees the queue;
//!   `platform.reports.resolve` + a fresh step-up approves, rejects,
//!   revokes, suspends, reinstates or removes, and verifies, suspends or
//!   reinstates a publisher (approved code runs on people's devices)
//! - bugs: `platform.support.read` / `platform.support.manage` for bug and
//!   feedback reports; `platform.security.read` /
//!   `platform.security.respond` for private security reports
//!
//! - designs: `platform.reports.read` sees what waits; `platform.reports.resolve`
//!   + a fresh step-up approves, refuses or takes down. Staff tools stay out
//!   of the app, so this is where every design is decided.
//!
//! Every change is written to the platform audit log first.

use revolt_database::{
    record_platform_audit, AuditOutcome, AuditRecord, BugReport, CommunityItemQuery,
    CommunityItemStatus, CommunityReviewAction, Database, Session, StaffPermission,
};
use revolt_models::v0::{self, BugReportReview, DataBugReview};
use revolt_result::{create_error, Result};
use rocket::{
    http::{ContentType, Header},
    serde::json::Json,
    State,
};
use serde::Deserialize;

use super::{reason, StaffContext};
use crate::routes::badges::bugs::{is_security, queue_of, report_for, review_as};
use crate::routes::plugins::{
    approve_as, publisher_as, queue_items, reinstate_as, reject_as, remove_as, revoke_as,
    suspend_as, DataPluginReview, PluginsOpen, ReviewItem,
};

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

// ---- community designs ----

/// # Community designs waiting for a decision
///
/// New designs, new versions of published ones and reported ones, of every
/// kind, oldest waiting first.
#[get("/designs/review")]
pub async fn design_queue(
    db: &State<Database>,
    staff: StaffContext,
) -> Result<Json<v0::CommunityReviewQueue>> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    let fetch = |statuses: Vec<CommunityItemStatus>, reported: bool, pending_revision: bool| {
        db.fetch_community_items(CommunityItemQuery {
            statuses,
            reported,
            pending_revision,
            limit: 100,
            ..Default::default()
        })
    };
    let mut pending = fetch(vec![CommunityItemStatus::PendingReview], false, false).await?;
    pending.reverse();
    let revisions = fetch(vec![CommunityItemStatus::Published], false, true).await?;
    let reported = fetch(
        vec![CommunityItemStatus::Published, CommunityItemStatus::Suspended],
        true,
        false,
    )
    .await?;
    let creators = crate::routes::community::creators_for(
        db,
        pending
            .iter()
            .chain(&revisions)
            .chain(&reported)
            .map(|item| item.creator_id.clone()),
    )
    .await?;
    let api = |list: Vec<revolt_database::CommunityItem>| {
        list.into_iter().map(|item| item.into_v0(true, false)).collect()
    };
    Ok(Json(v0::CommunityReviewQueue {
        pending: api(pending),
        revisions: api(revisions),
        reported: api(reported),
        creators,
    }))
}

/// How long the console waits for the file service
const FILE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A design's file for the console: never stored anywhere on the way
#[derive(rocket::Responder)]
pub struct DesignFile {
    body: Vec<u8>,
    content_type: ContentType,
    cache: Header<'static>,
    sniff: Header<'static>,
}

/// # The file of a design
///
/// For the console's preview, also while the design is not public. It comes
/// from the file service, which checks this session; the console itself only
/// talks to this API. `pending` asks for the waiting new version.
#[get("/designs/review/<id>/file?<pending>")]
pub async fn design_file(
    db: &State<Database>,
    staff: StaffContext,
    session: Session,
    id: &str,
    pending: Option<bool>,
) -> Result<DesignFile> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    let item = db.fetch_community_item(id).await?;
    let file = match (&item.pending, pending.unwrap_or(false)) {
        (Some(revision), true) => revision.file.clone(),
        _ => item.file.clone(),
    };
    let base = revolt_config::config().await.hosts.autumn;
    let client = reqwest::Client::builder()
        .timeout(FILE_TIMEOUT)
        .build()
        .map_err(|_| create_error!(ProxyError))?;
    let response = client
        .get(format!(
            "{}/{}/{}/original",
            base.trim_end_matches('/'),
            file.tag,
            file.id
        ))
        .header("x-session-token", session.token)
        .send()
        .await
        .map_err(|_| create_error!(ProxyError))?;
    if !response.status().is_success() {
        return Err(create_error!(NotFound));
    }
    let content_type = match file.content_type.as_str() {
        "image/gif" => ContentType::GIF,
        "image/webp" => ContentType::WEBP,
        _ => ContentType::PNG,
    };
    let body = response.bytes().await.map_err(|_| create_error!(ProxyError))?;
    Ok(DesignFile {
        body: body.to_vec(),
        content_type,
        cache: Header::new("Cache-Control", "private, no-store"),
        sniff: Header::new("X-Content-Type-Options", "nosniff"),
    })
}

#[derive(Deserialize)]
pub struct DataStaffDesignDecision {
    /// approve, reject or suspend (take down)
    pub action: String,
    /// Required to reject or take down; the creator reads it
    #[serde(default)]
    pub reason: Option<String>,
    /// The version the reviewer looked at: a design that changed since is
    /// not decided
    pub version: i64,
}

/// # Decide on a community design
///
/// Approve, refuse with a reason, or take down with a reason. A takedown of an
/// effect sends the creator a statement of reasons they can appeal.
#[post("/designs/review/<id>", data = "<data>")]
pub async fn design_decide(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataStaffDesignDecision>,
) -> Result<Json<v0::CommunityItem>> {
    staff.require_step_up(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    let action = match data.action.as_str() {
        "approve" => CommunityReviewAction::Approve,
        "reject" => CommunityReviewAction::Reject,
        "suspend" => CommunityReviewAction::Suspend,
        _ => return Err(create_error!(InvalidOperation)),
    };
    let why = match data.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(text) => Some(reason(text)?),
        None if action != CommunityReviewAction::Approve => {
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::new("reason", "required"),
            ]))
        }
        None => None,
    };
    // the target must exist before we log changing it
    let item = db.fetch_community_item(id).await?;
    // nobody decides on their own work
    if item.creator_id == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let mut record = staff.record("staff.design.review", Some(StaffPermission::ReportsResolve));
    record.target_kind = Some("community_item".to_string());
    record.target_id = Some(id.to_string());
    record.reason = why.clone();
    record.summary = Some(data.action.clone());
    let decision = v0::DataCommunityDecision {
        reason: why,
        version: Some(data.version),
    };
    audited(
        db,
        record,
        crate::routes::community::decide_as(db, &staff.user, id, action, decision),
    )
    .await
    .map(Json)
}

// ---- plugins ----

/// # Plugin releases waiting for review
#[get("/plugins/review")]
pub async fn plugin_queue(_open: PluginsOpen, db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<ReviewItem>>> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    queue_items(db).await.map(Json)
}

#[derive(Deserialize)]
pub struct DataStaffPluginDecision {
    /// approve, reject or revoke (a release); suspend, reinstate or remove
    /// (a plugin); verify, suspend_publisher or reinstate_publisher
    pub action: String,
    /// Required except for approve, reinstate and verify
    #[serde(default)]
    pub reason: Option<String>,
    /// approve: also mark the publisher verified
    #[serde(default)]
    pub verify_publisher: bool,
    /// approve: required for a high-risk release
    #[serde(default)]
    pub acknowledge_high_risk: bool,
}

/// # Decide on a plugin, a release or a publisher
///
/// `id` is the release for approve/reject/revoke, the plugin for
/// suspend/reinstate/remove and the publisher's user id for the publisher
/// actions.
#[post("/plugins/review/<id>", data = "<data>")]
pub async fn plugin_decide(
    _open: PluginsOpen,
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataStaffPluginDecision>,
) -> Result<Json<serde_json::Value>> {
    staff.require_step_up(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    let needs_reason = !matches!(data.action.as_str(), "approve" | "reinstate" | "verify" | "reinstate_publisher");
    let why = match data.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(text) => Some(reason(text)?),
        None if needs_reason => {
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::new("reason", "required"),
            ]))
        }
        None => None,
    };
    let target_kind = match data.action.as_str() {
        "approve" | "reject" | "revoke" => "plugin_release",
        "suspend" | "reinstate" | "remove" => "plugin",
        "verify" | "suspend_publisher" | "reinstate_publisher" => "user",
        _ => return Err(create_error!(InvalidOperation)),
    };
    // the target must exist before we log changing it
    match target_kind {
        "plugin_release" => drop(db.fetch_plugin_release(id).await?),
        "plugin" => drop(db.fetch_plugin(id).await?),
        _ => drop(db.fetch_user(id).await?),
    }
    let mut record = staff.record("staff.plugin.review", Some(StaffPermission::ReportsResolve));
    record.target_kind = Some(target_kind.to_string());
    record.target_id = Some(id.to_string());
    record.reason = why.clone();
    record.summary = Some(data.action.clone());
    let review = DataPluginReview {
        reason: why.clone(),
        verify_publisher: data.verify_publisher,
        acknowledge_high_risk: data.acknowledge_high_risk,
    };
    let user = &staff.user;
    let out = audited(db, record, async {
        Ok(match data.action.as_str() {
            "approve" => serde_json::to_value(approve_as(db, user, id, review).await?),
            "reject" => serde_json::to_value(reject_as(db, user, id, review).await?),
            "revoke" => serde_json::to_value(revoke_as(db, user, id, review).await?),
            "suspend" => serde_json::to_value(suspend_as(db, user, id, review).await?),
            "reinstate" => serde_json::to_value(reinstate_as(db, user, id, review).await?),
            "remove" => serde_json::to_value(remove_as(db, user, id, review).await?),
            "verify" => serde_json::to_value(publisher_as(db, user, id, "verify", None).await?),
            "suspend_publisher" => serde_json::to_value(publisher_as(db, user, id, "suspend", why.clone()).await?),
            _ => serde_json::to_value(publisher_as(db, user, id, "reinstate", None).await?),
        }
        .unwrap_or(serde_json::Value::Null))
    })
    .await?;

    // the publisher hears about it in their private messages
    let note = why.as_deref();
    match target_kind {
        "plugin_release" => {
            if let Ok(release) = db.fetch_plugin_release(id).await {
                if let Ok(plugin) = db.fetch_plugin(&release.plugin_id).await {
                    let key = match data.action.as_str() {
                        "approve" => "plugin.approved",
                        "reject" => "plugin.rejected",
                        _ => "plugin.revoked",
                    };
                    crate::util::official::plugin(db, &plugin.publisher_id, key, &plugin.name, &release.version, note).await;
                }
            }
        }
        "plugin" => {
            if let Ok(plugin) = db.fetch_plugin(id).await {
                let key = match data.action.as_str() {
                    "suspend" => "plugin.suspended",
                    "reinstate" => "plugin.reinstated",
                    _ => "plugin.removed",
                };
                crate::util::official::plugin(db, &plugin.publisher_id, key, &plugin.name, "", note).await;
            }
        }
        _ => {
            let key = match data.action.as_str() {
                "verify" => "publisher.verified",
                "suspend_publisher" => "publisher.suspended",
                _ => "publisher.reinstated",
            };
            crate::util::official::send(db, id, key, &[("note", note.unwrap_or_default())]).await;
        }
    }
    Ok(Json(out))
}

// ---- bug reports ----

fn sees(staff: &StaffContext, report: &BugReport) -> bool {
    staff.has(if is_security(report) {
        StaffPermission::SecurityRead
    } else {
        StaffPermission::SupportRead
    })
}

/// # Bug reports
///
/// `security=true` lists private security reports (`security.read`);
/// otherwise bug and feedback reports (`support.read`). `status` is a
/// comma-separated list (default: Open, Triaged, NeedsInfo).
#[get("/bugs?<security>&<status>")]
pub async fn bugs(
    db: &State<Database>,
    staff: StaffContext,
    security: Option<bool>,
    status: Option<&str>,
) -> Result<Json<Vec<BugReportReview>>> {
    let security = security.unwrap_or(false);
    staff
        .require(db, if security { StaffPermission::SecurityRead } else { StaffPermission::SupportRead })
        .await?;
    if status.is_some_and(|s| s.len() > 128) {
        return Err(create_error!(InvalidOperation));
    }
    queue_of(db, security, status).await.map(Json)
}

/// # A bug report
#[get("/bugs/<id>")]
pub async fn bug(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<BugReportReview>> {
    // a report you may not see is a 404, like for anyone else
    if !staff.has(StaffPermission::SupportRead) && !staff.has(StaffPermission::SecurityRead) {
        staff.require(db, StaffPermission::SupportRead).await?;
    }
    report_for(db, id, |r| sees(&staff, r)).await.map(Json)
}

/// # Review a bug report
///
/// `triage`, `need_info`, `confirm`, `fix`, `duplicate` (with
/// `duplicate_of`), `not_a_bug` or `reopen`, with `updated_at` from the
/// report you read. Confirming or fixing gives the reporter Bug Hunter.
#[post("/bugs/<id>/review", data = "<data>")]
pub async fn bug_review(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataBugReview>,
) -> Result<Json<BugReportReview>> {
    let report = db.fetch_bug_report(id).await?;
    let (read, manage) = if is_security(&report) {
        (StaffPermission::SecurityRead, StaffPermission::SecurityRespond)
    } else {
        (StaffPermission::SupportRead, StaffPermission::SupportManage)
    };
    if !staff.has(read) {
        return Err(create_error!(NotFound));
    }
    staff.require(db, manage).await?;
    let data = data.into_inner();
    let mut record = staff.record("staff.bug.review", Some(manage));
    record.target_kind = Some("bug_report".to_string());
    record.target_id = Some(id.to_string());
    record.summary = Some(data.action.chars().take(32).collect());
    let can = |r: &BugReport| sees(&staff, r);
    let action = data.action.clone();
    let note = data.note.clone().unwrap_or_default();
    let reviewed = audited(db, record, review_as(db, &staff.user, can, id, data)).await?;
    crate::util::official::bug(db, &reviewed.report.author_id, &action, &reviewed.report.title, &note).await;
    Ok(Json(reviewed))
}
