//! Bug reports, security reports and beta feedback. Mounted at `/bugs`.
//!
//! Security reports are private: only their reporter and reviewers with
//! `bugs.security` ever see them, and nothing about them reaches a profile.
//! Bug Hunter is granted when a reviewer verifies a report, never for
//! sending one.

use std::collections::HashSet;

use revolt_database::{
    beta_access, grant_badge, held_badges, platform_permissions, BadgeSource, BugHistoryEntry,
    BugKind, BugReport, BugReportQuery, BugStatus, Database, File, PlatformPermission,
    PlatformPermissions, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::{serde::json::Json, Route, State};
use ulid::Ulid;

use super::{no_bots, now_ms, validated};
use crate::routes::community::accounts_for;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![submit, mine, fetch, queue, review]
}

/// Reports one user can send per day
pub const BUG_REPORTS_PER_DAY: usize = 10;

const PLATFORMS: [&str; 5] = ["web", "desktop-windows", "desktop-macos", "desktop-linux", "other"];

fn status_name(status: &BugStatus) -> String {
    format!("{status:?}")
}

fn is_verified(status: &BugStatus) -> bool {
    matches!(status, BugStatus::Confirmed | BugStatus::Fixed)
}

/// Lowercase words of three letters or more (any script), for duplicates
pub fn bug_keywords(title: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .filter(|w| seen.insert(w.to_string()))
        .map(str::to_string)
        .collect()
}

/// Share of words two titles have in common (0 to 1)
pub fn keyword_similarity(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a: HashSet<_> = a.iter().collect();
    let b: HashSet<_> = b.iter().collect();
    a.intersection(&b).count() as f32 / a.union(&b).count() as f32
}

fn view(report: BugReport) -> v0::BugReport {
    v0::BugReport {
        verified: is_verified(&report.status),
        id: report.id,
        author_id: report.author_id,
        kind: format!("{:?}", report.kind),
        title: report.title,
        description: report.description,
        steps: report.steps,
        expected: report.expected,
        actual: report.actual,
        platform: report.platform,
        device: report.device,
        app_version: report.app_version,
        feature: report.feature,
        attachments: report.attachments.into_iter().map(Into::into).collect(),
        status: status_name(&report.status),
        severity: report.severity,
        duplicate_of: report.duplicate_of,
        reviewer_note: report.reviewer_note,
        history: report
            .history
            .into_iter()
            .map(|h| v0::BugHistoryEntry {
                actor_id: h.actor_id,
                status: status_name(&h.status),
                note: h.note,
                created_at: h.created_at,
            })
            .collect(),
        created_at: report.created_at,
        updated_at: report.updated_at,
    }
}

/// May this user see this report as a reviewer?
fn reviews(perms: &PlatformPermissions, report: &BugReport) -> bool {
    if report.kind == BugKind::Security {
        perms.has(PlatformPermission::BugsSecurity)
    } else {
        perms.has(PlatformPermission::BugsReview)
    }
}

/// # Report a bug
///
/// `bug`, `security` (private: only you and security reviewers see it) or
/// `feedback` (beta testers, about a beta feature). Up to four screenshots
/// uploaded to `attachments`.
#[openapi(tag = "Bug Reports")]
#[post("/", data = "<data>")]
pub async fn submit(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataBugReport>,
) -> Result<Json<v0::BugReport>> {
    no_bots(&user)?;
    let data = validated(data.into_inner())?;
    let kind = match data.kind.as_str() {
        "bug" => BugKind::Bug,
        "security" => BugKind::Security,
        "feedback" => BugKind::Feedback,
        _ => return Err(create_error!(InvalidOperation)),
    };
    if !PLATFORMS.contains(&data.platform.as_str()) {
        return Err(create_error!(FailedValidation {
            error: "unknown platform".to_string()
        }));
    }
    let now = now_ms();
    if db
        .count_bug_reports_since(&user.id, now - 24 * 60 * 60 * 1000)
        .await?
        >= BUG_REPORTS_PER_DAY
    {
        return Err(revolt_result::failed_validation(vec![revolt_result::ValidationProblem {
            max: Some(BUG_REPORTS_PER_DAY as i64),
            ..revolt_result::ValidationProblem::new("reports", "per_day")
        }]));
    }

    let member = db.fetch_beta_member(&user.id).await?;
    let tester = member
        .as_ref()
        .is_some_and(|m| m.status == revolt_database::BetaStatus::Active);
    let feature = data
        .feature
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty());
    if kind == BugKind::Feedback {
        // feedback is for testers, about a feature they can actually use
        if !tester {
            return Err(create_error!(NotPrivileged));
        }
        if let Some(feature) = &feature {
            if !beta_access(db, &user, feature).await? {
                return Err(create_error!(NotPrivileged));
            }
        }
    }

    let id = Ulid::new().to_string();
    let mut attachments: Vec<File> = vec![];
    for file in data.attachments.unwrap_or_default() {
        attachments.push(File::use_attachment(db, &file, &id, &user.id).await?);
    }
    let clean = |text: Option<String>| text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let title = data.title.trim().to_string();
    let report = BugReport {
        id,
        author_id: user.id.clone(),
        kind,
        keywords: bug_keywords(&title),
        title,
        description: data.description.trim().to_string(),
        steps: clean(data.steps),
        expected: clean(data.expected),
        actual: clean(data.actual),
        platform: data.platform,
        device: clean(data.device),
        app_version: clean(data.app_version),
        feature,
        attachments,
        status: BugStatus::Open,
        severity: None,
        duplicate_of: None,
        reviewed_by: None,
        reviewer_note: None,
        history: vec![BugHistoryEntry {
            actor_id: user.id.clone(),
            status: BugStatus::Open,
            note: None,
            created_at: now,
        }],
        created_at: now,
        updated_at: now,
    };
    db.insert_bug_report(&report).await?;
    if tester {
        db.add_beta_contribution(&user.id).await?;
    }
    Ok(Json(view(report)))
}

/// # My reports
#[openapi(tag = "Bug Reports")]
#[get("/@me")]
pub async fn mine(db: &State<Database>, user: User) -> Result<Json<Vec<v0::BugReport>>> {
    no_bots(&user)?;
    Ok(Json(
        db.fetch_bug_reports(BugReportQuery {
            author_id: Some(&user.id),
            include_security: true,
            limit: 100,
            ..Default::default()
        })
        .await?
        .into_iter()
        .map(view)
        .collect(),
    ))
}

async fn possible_duplicates(db: &Database, report: &BugReport) -> Result<Vec<String>> {
    let open = db
        .fetch_bug_reports(BugReportQuery {
            statuses: vec![
                BugStatus::Open,
                BugStatus::Triaged,
                BugStatus::NeedsInfo,
                BugStatus::Confirmed,
            ],
            include_security: report.kind == BugKind::Security,
            only_security: report.kind == BugKind::Security,
            limit: 200,
            ..Default::default()
        })
        .await?;
    Ok(open
        .into_iter()
        .filter(|r| r.id != report.id && r.created_at <= report.created_at)
        .filter(|r| keyword_similarity(&r.keywords, &report.keywords) >= 0.5)
        .take(5)
        .map(|r| r.id)
        .collect())
}

async fn review_view(db: &Database, report: BugReport) -> Result<v0::BugReportReview> {
    let possible_duplicates = possible_duplicates(db, &report).await?;
    let author = accounts_for(db, [report.author_id.clone()]).await?.pop();
    Ok(v0::BugReportReview {
        report: view(report),
        author,
        possible_duplicates,
    })
}

/// # A report
///
/// Its reporter sees it; reviewers see it with the reporter and possible
/// duplicates. A security report is a 404 for everyone else.
#[openapi(tag = "Bug Reports")]
#[get("/<id>")]
pub async fn fetch(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::BugReportReview>> {
    no_bots(&user)?;
    let report = db.fetch_bug_report(id).await?;
    let perms = platform_permissions(db, &user).await?;
    if reviews(&perms, &report) {
        return Ok(Json(review_view(db, report).await?));
    }
    if report.author_id != user.id {
        return Err(create_error!(NotFound));
    }
    Ok(Json(v0::BugReportReview {
        report: view(report),
        author: None,
        possible_duplicates: vec![],
    }))
}

/// # Review queue
///
/// Reports in the given statuses (default: open, triaged, needs info).
/// Security reports only with `bugs.security` and `security=true`.
#[openapi(tag = "Bug Reports")]
#[get("/admin/queue?<options..>")]
pub async fn queue(
    db: &State<Database>,
    user: User,
    options: v0::OptionsBugQueue,
) -> Result<Json<Vec<v0::BugReportReview>>> {
    no_bots(&user)?;
    let perms = platform_permissions(db, &user).await?;
    let security = options.security.unwrap_or(false);
    let needed = if security {
        PlatformPermission::BugsSecurity
    } else {
        PlatformPermission::BugsReview
    };
    super::require(&perms, needed)?;
    queue_of(db, security, options.status.as_deref()).await.map(Json)
}

/// Reports in the given comma-separated statuses (default: open, triaged,
/// needs info); security reports only, or none of them
pub(crate) async fn queue_of(
    db: &Database,
    security: bool,
    status: Option<&str>,
) -> Result<Vec<v0::BugReportReview>> {
    let mut statuses = vec![];
    for name in status
        .unwrap_or("Open,Triaged,NeedsInfo")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        statuses.push(match name {
            "Open" => BugStatus::Open,
            "Triaged" => BugStatus::Triaged,
            "NeedsInfo" => BugStatus::NeedsInfo,
            "Confirmed" => BugStatus::Confirmed,
            "Fixed" => BugStatus::Fixed,
            "Duplicate" => BugStatus::Duplicate,
            "NotABug" => BugStatus::NotABug,
            _ => return Err(create_error!(InvalidOperation)),
        });
    }
    let reports = db
        .fetch_bug_reports(BugReportQuery {
            statuses,
            only_security: security,
            include_security: security,
            limit: 100,
            ..Default::default()
        })
        .await?;
    let mut out = vec![];
    for report in reports {
        out.push(review_view(db, report).await?);
    }
    Ok(out)
}

/// One report with its reporter and possible duplicates, if `may_see` it
pub(crate) async fn report_for(
    db: &Database,
    id: &str,
    may_see: impl Fn(&BugReport) -> bool,
) -> Result<v0::BugReportReview> {
    let report = db.fetch_bug_report(id).await?;
    if !may_see(&report) {
        return Err(create_error!(NotFound));
    }
    review_view(db, report).await
}

/// Who may review a security report, and who an ordinary one
pub(crate) fn is_security(report: &BugReport) -> bool {
    report.kind == BugKind::Security
}

/// # Review a report
///
/// `triage`, `need_info`, `confirm`, `fix`, `duplicate` (with
/// `duplicate_of`), `not_a_bug` or `reopen`. Confirming or fixing a bug or
/// security report verifies it and gives its reporter Bug Hunter. You
/// can't review your own report.
#[openapi(tag = "Bug Reports")]
#[post("/admin/<id>/review", data = "<data>")]
pub async fn review(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataBugReview>,
) -> Result<Json<v0::BugReportReview>> {
    no_bots(&user)?;
    let perms = platform_permissions(db, &user).await?;
    review_as(db, &user, |r| reviews(&perms, r), id, data.into_inner())
        .await
        .map(Json)
}

/// Review a report as `user`, who `may_see` the reports they act on
pub(crate) async fn review_as(
    db: &Database,
    user: &User,
    may_see: impl Fn(&BugReport) -> bool,
    id: &str,
    data: v0::DataBugReview,
) -> Result<v0::BugReportReview> {
    let data = validated(data)?;
    let before = db.fetch_bug_report(id).await?;
    if !may_see(&before) {
        return Err(create_error!(NotFound));
    }
    if before.author_id == user.id {
        return Err(create_error!(NotPrivileged));
    }
    let mut report = before.clone();
    let status = match data.action.as_str() {
        "triage" => BugStatus::Triaged,
        "need_info" => BugStatus::NeedsInfo,
        "confirm" => BugStatus::Confirmed,
        "fix" => BugStatus::Fixed,
        "not_a_bug" => BugStatus::NotABug,
        "reopen" => BugStatus::Open,
        "duplicate" => {
            let original = data
                .duplicate_of
                .as_deref()
                .ok_or_else(|| create_error!(InvalidOperation))?;
            if original == report.id {
                return Err(create_error!(InvalidOperation));
            }
            let original = db.fetch_bug_report(original).await?;
            if !may_see(&original) {
                return Err(create_error!(NotFound));
            }
            report.duplicate_of = Some(original.id);
            BugStatus::Duplicate
        }
        _ => return Err(create_error!(InvalidOperation)),
    };
    if status != BugStatus::Duplicate {
        report.duplicate_of = None;
    }
    if let Some(severity) = data.severity.as_deref() {
        if !["low", "medium", "high", "critical"].contains(&severity) {
            return Err(create_error!(FailedValidation {
                error: "unknown severity".to_string()
            }));
        }
        report.severity = Some(severity.to_string());
    }
    let note = data.note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if note.is_some() {
        report.reviewer_note = note.clone();
    }
    let now = now_ms().max(before.updated_at + 1);
    report.status = status.clone();
    report.reviewed_by = Some(user.id.clone());
    report.updated_at = now;
    report.history.push(BugHistoryEntry {
        actor_id: user.id.clone(),
        status: status.clone(),
        note,
        created_at: now,
    });
    if before.updated_at != data.updated_at || !db.update_bug_report(&report, before.updated_at).await? {
        return Err(create_error!(CommunityItemConflict));
    }

    // a verified bug or security report earns Bug Hunter (feedback doesn't)
    if is_verified(&status)
        && report.kind != BugKind::Feedback
        && !held_badges(db, &report.author_id).await?.contains("bug_hunter")
    {
        if let Ok(author) = db.fetch_user(&report.author_id).await {
            if author.bot.is_none() {
                grant_badge(
                    db,
                    &author.id,
                    "bug_hunter",
                    BadgeSource::System,
                    Some(format!("bug:{}", report.id)),
                    &user.id,
                    None,
                    None,
                )
                .await?;
            }
        }
    }
    review_view(db, report).await
}
