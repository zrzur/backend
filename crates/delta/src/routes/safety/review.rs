use std::collections::BTreeMap;

use revolt_database::{
    safety_now, Database, PlatformPermission, PlatformPermissions, User,
    SAFETY_EVIDENCE_RETENTION_MS,
};
use revolt_models::v0::{
    safety_transition_allowed, DataSafetyBulk, DataSafetyCaseEdit, DataSafetyCaseStatus,
    DataSafetyNote, SafetyAccess, SafetyAction, SafetyAppeal, SafetyBulkResult, SafetyCase,
    SafetyCaseDetails, SafetyCasePage, SafetyCategory, SafetyPriority, SafetyResolution,
    SafetyStatus, SafetyTargetKind,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::Serialize;

use super::{clean_text, log, may_review, require, safety_permissions, save_case, validate};

fn parse_list<T: serde::de::DeserializeOwned>(value: Option<&str>) -> Result<Vec<T>> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            serde_json::from_value(serde_json::Value::String(s.to_string()))
                .map_err(|_| create_error!(InvalidOperation))
        })
        .collect()
}

fn decided(status: SafetyStatus) -> bool {
    matches!(
        status,
        SafetyStatus::NoViolation | SafetyStatus::Duplicate | SafetyStatus::Closed
    )
}

/// Check one reviewer state change, including who may make it
fn check_transition(
    perms: &revolt_database::PlatformPermissions,
    case: &SafetyCase,
    to: SafetyStatus,
) -> Result<()> {
    if !safety_transition_allowed(case.status, to) {
        return Err(create_error!(InvalidOperation));
    }
    // escalated cases and reopening decided ones are for senior reviewers
    if case.status == SafetyStatus::Escalated || (decided(case.status) && to == SafetyStatus::UnderReview)
    {
        require(perms, PlatformPermission::SafetyManage)?;
    }
    // deciding a case needs the right to act
    if matches!(to, SafetyStatus::NoViolation | SafetyStatus::Closed) {
        require(perms, PlatformPermission::SafetyAction)?;
    }
    if to == SafetyStatus::Duplicate && case.duplicate_of.is_none() {
        return Err(create_error!(InvalidOperation));
    }
    Ok(())
}

fn apply_status(case: &mut SafetyCase, to: SafetyStatus, actor: &str, public_note: Option<String>) {
    let now = safety_now();
    case.status = to;
    match to {
        SafetyStatus::MoreInfoRequired => case.info_request = public_note,
        SafetyStatus::NoViolation | SafetyStatus::Duplicate => {
            case.closed_at = Some(now);
            case.resolution = Some(SafetyResolution {
                decided_by: actor.to_string(),
                decided_at: now,
                public_note,
                action_ids: case
                    .resolution
                    .as_ref()
                    .map(|r| r.action_ids.clone())
                    .unwrap_or_default(),
            });
        }
        SafetyStatus::Closed => {
            case.closed_at = Some(now);
            if let (Some(resolution), Some(note)) = (case.resolution.as_mut(), public_note) {
                resolution.public_note = Some(note);
            }
        }
        SafetyStatus::UnderReview if case.closed_at.is_some() => {
            // reopened: the old decision stays in the audit trail
            case.closed_at = None;
            case.resolution = None;
        }
        _ => {}
    }
}

/// # Trust & Safety access
///
/// What the signed-in user may do in Trust & Safety. Everyone may call
/// this; it is how clients decide whether to show the review dashboard.
#[openapi(tag = "Trust & Safety")]
#[get("/admin/access")]
pub async fn access(db: &State<Database>, user: User) -> Result<Json<SafetyAccess>> {
    let perms = safety_permissions(db, &user).await?;
    Ok(Json(SafetyAccess {
        review: perms.has(PlatformPermission::SafetyReview),
        action: perms.has(PlatformPermission::SafetyAction),
        manage: perms.has(PlatformPermission::SafetyManage),
    }))
}

/// # Review queue
///
/// Cases, newest first (or oldest first with `oldest=true`). Filters take
/// comma separated values; `assignee` is `me`, `none` or a user id.
#[openapi(tag = "Trust & Safety")]
#[get("/admin/reports?<status>&<kind>&<category>&<priority>&<assignee>&<search>&<before>&<oldest>&<limit>")]
#[allow(clippy::too_many_arguments)]
pub async fn queue(
    db: &State<Database>,
    user: User,
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
    let perms = safety_permissions(db, &user).await?;
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
    queue_as(db, &user, &perms, filter).await.map(Json)
}

/// Review queue filters, as given in the query string
#[derive(Default)]
pub(crate) struct SafetyQueueFilter {
    pub status: Option<String>,
    pub kind: Option<String>,
    pub category: Option<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub search: Option<String>,
    pub before: Option<String>,
    pub oldest: bool,
    pub limit: Option<usize>,
}

pub(crate) async fn queue_as(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    f: SafetyQueueFilter,
) -> Result<SafetyCasePage> {
    require(perms, PlatformPermission::SafetyReview)?;
    let (status, kind, category, priority, assignee, search, before, oldest, limit) = (
        f.status.as_deref(),
        f.kind.as_deref(),
        f.category.as_deref(),
        f.priority.as_deref(),
        f.assignee.as_deref(),
        f.search.as_deref(),
        f.before.as_deref(),
        f.oldest,
        f.limit,
    );

    let limit = limit.unwrap_or(50).clamp(1, 100);
    let priority: Vec<SafetyPriority> = parse_list(priority)?;
    let (assignee_id, unassigned) = match assignee {
        Some("me") => (Some(user.id.clone()), false),
        Some("none") => (None, true),
        Some(id) if !id.is_empty() => (Some(id.to_string()), false),
        _ => (None, false),
    };
    let cases = db
        .fetch_safety_cases(revolt_database::SafetyCaseQuery {
            routes: vec![],
            statuses: parse_list::<SafetyStatus>(status)?,
            kinds: parse_list::<SafetyTargetKind>(kind)?,
            categories: parse_list::<SafetyCategory>(category)?,
            priority: priority.first().copied(),
            assignee_id,
            unassigned,
            reporter_id: None,
            search: search.map(|s| s.chars().take(64).collect()),
            before: before.map(str::to_string),
            oldest_first: oldest,
            limit,
        })
        .await?;
    let next = if cases.len() == limit {
        cases.last().map(|c| c.id.clone())
    } else {
        None
    };
    Ok(SafetyCasePage { cases, next })
}

/// # Case
///
/// Everything about one case. Opening a case is recorded in its audit
/// trail, because it shows evidence.
#[openapi(tag = "Trust & Safety")]
#[get("/admin/reports/<id>")]
pub async fn case(db: &State<Database>, user: User, id: &str) -> Result<Json<SafetyCaseDetails>> {
    let perms = safety_permissions(db, &user).await?;
    case_as(db, &user, &perms, id).await.map(Json)
}

pub(crate) async fn case_as(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    id: &str,
) -> Result<SafetyCaseDetails> {
    require(perms, PlatformPermission::SafetyReview)?;

    let case = db.fetch_safety_case(id).await?;
    log(db, id, &user.id, "EvidenceViewed", None, None, None).await?;

    let related = db
        .fetch_safety_cases_for_target(case.target.kind, &case.target.id, 26)
        .await?
        .into_iter()
        .filter(|c| c.id != case.id)
        .take(25)
        .collect();
    Ok(SafetyCaseDetails {
        evidence: db.fetch_safety_evidence(id).await?,
        log: db.fetch_safety_log(id).await?,
        actions: db.fetch_safety_actions_for_case(id).await?,
        distinct_reporters: db
            .count_safety_reporters(case.target.kind, &case.target.id)
            .await?,
        appeals: db.fetch_safety_appeals_for_case(id).await?,
        related,
        case,
    })
}

/// # Change case status
///
/// Move a case through review. Decisions with consequences
/// (`ActionTaken`) only come from taking an action.
#[openapi(tag = "Trust & Safety")]
#[post("/admin/reports/<id>/status", data = "<data>")]
pub async fn set_status(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataSafetyCaseStatus>,
) -> Result<Json<SafetyCase>> {
    let perms = safety_permissions(db, &user).await?;
    set_status_as(db, &user, &perms, id, data.into_inner()).await.map(Json)
}

pub(crate) async fn set_status_as(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    id: &str,
    data: DataSafetyCaseStatus,
) -> Result<SafetyCase> {
    validate(&data)?;
    require(perms, PlatformPermission::SafetyReview)?;

    let before = db.fetch_safety_case(id).await?;
    if before.version != data.version {
        return Err(create_error!(SafetyCaseConflict));
    }
    check_transition(perms, &before, data.status)?;
    let public_note = data.public_note.as_deref().map(clean_text).filter(|s| !s.is_empty());
    if data.status == SafetyStatus::MoreInfoRequired && public_note.is_none() {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("public_note", "required"),
        ]));
    }

    let mut after = before.clone();
    apply_status(&mut after, data.status, &user.id, public_note.clone());
    if data.status == SafetyStatus::UnderReview && after.assignee_id.is_none() {
        after.assignee_id = Some(user.id.clone());
    }
    let after = save_case(db, &before, after).await?;
    log(
        db,
        id,
        &user.id,
        "Status",
        Some(format!("{:?}", before.status)),
        Some(format!("{:?}", after.status)),
        public_note.clone(),
    )
    .await?;
    if let Some(note) = data.note.as_deref().map(clean_text).filter(|s| !s.is_empty()) {
        log(db, id, &user.id, "Note", None, None, Some(note)).await?;
    }
    // every way a report can end, and every request for information, goes
    // to the person who reported it (DSA Art. 16(5)); an action is told in
    // `take_with_statement`, so closing a case that had one says nothing more
    use crate::util::official::{report_outcome, ReportOutcome};
    let had_action = before
        .resolution
        .as_ref()
        .is_some_and(|r| !r.action_ids.is_empty());
    match reporter_message(before.status, after.status, had_action, public_note.as_deref()) {
        Some(Told::NoViolation) => {
            report_outcome(db, &after, ReportOutcome::NoViolation, public_note.as_deref()).await
        }
        Some(Told::Duplicate) => {
            report_outcome(db, &after, ReportOutcome::Duplicate, public_note.as_deref()).await
        }
        Some(Told::Closed) => {
            report_outcome(db, &after, ReportOutcome::Closed, public_note.as_deref()).await
        }
        Some(Told::Note) => {
            report_outcome(db, &after, ReportOutcome::Note, public_note.as_deref()).await
        }
        Some(Told::Info) => {
            if let Some(question) = public_note.as_deref() {
                report_outcome(db, &after, ReportOutcome::Info(question), None).await
            }
        }
        None => {}
    }
    Ok(after)
}

/// # Edit case
///
/// Assign, change priority, mark as a duplicate, or place a legal hold
/// (senior reviewers) on the evidence.
#[openapi(tag = "Trust & Safety")]
#[patch("/admin/reports/<id>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataSafetyCaseEdit>,
) -> Result<Json<SafetyCase>> {
    let perms = safety_permissions(db, &user).await?;
    edit_as(db, &user, &perms, id, data.into_inner()).await.map(Json)
}

pub(crate) async fn edit_as(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    id: &str,
    data: DataSafetyCaseEdit,
) -> Result<SafetyCase> {
    require(perms, PlatformPermission::SafetyReview)?;

    let before = db.fetch_safety_case(id).await?;
    if before.version != data.version {
        return Err(create_error!(SafetyCaseConflict));
    }
    let mut after = before.clone();
    let mut entries: Vec<(&str, Option<String>, Option<String>)> = vec![];

    if let Some(assignee) = &data.assignee_id {
        if assignee.is_empty() {
            after.assignee_id = None;
        } else {
            // cases only go to people who may review them
            let reviewer = db.fetch_user(assignee).await.map_err(|_| create_error!(NotFound))?;
            if !may_review(db, &reviewer).await? {
                return Err(create_error!(MissingPlatformPermission {
                    permission: PlatformPermission::SafetyReview.name().to_string()
                }));
            }
            after.assignee_id = Some(assignee.clone());
        }
        entries.push(("Assign", before.assignee_id.clone(), after.assignee_id.clone()));
    }
    if let Some(priority) = data.priority {
        after.priority = priority;
        entries.push((
            "Priority",
            Some(format!("{:?}", before.priority)),
            Some(format!("{priority:?}")),
        ));
    }
    if let Some(of) = &data.duplicate_of {
        if of == id {
            return Err(create_error!(InvalidOperation));
        }
        let original = db.fetch_safety_case(of).await?;
        if original.duplicate_of.is_some() {
            // point at the original, never at another duplicate
            return Err(create_error!(InvalidOperation));
        }
        after.duplicate_of = Some(original.id.clone());
        entries.push(("Duplicate", None, Some(original.id)));
    }
    if let Some(hold) = data.legal_hold {
        require(perms, PlatformPermission::SafetyManage)?;
        let expires_at = (!hold).then(|| safety_now() + SAFETY_EVIDENCE_RETENTION_MS);
        after.legal_hold = hold;
        // a hold on a case with no copy yet (a report without an account)
        // must protect something: copy the content now, held
        if hold && after.public_reporter.is_some() && after.evidence_ids.is_empty() {
            super::actions::prepare_public_case(db, user, &mut after).await?;
            if after.evidence_ids.is_empty() {
                // the dashboard says so too: the case shows a hold with no evidence
                entries.push(("LegalHoldWithoutCopy", None, None));
            }
        }
        db.set_safety_evidence_hold(id, hold, expires_at).await?;
        entries.push(("LegalHold", None, Some(hold.to_string())));
    }

    let after = if after != before {
        save_case(db, &before, after).await?
    } else {
        before
    };
    for (event, from, to) in entries {
        log(db, id, &user.id, event, from, to, None).await?;
    }
    Ok(after)
}

/// # Add internal note
///
/// Notes are only ever shown to reviewers.
#[openapi(tag = "Trust & Safety")]
#[post("/admin/reports/<id>/notes", data = "<data>")]
pub async fn note(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataSafetyNote>,
) -> Result<Json<SafetyCase>> {
    let perms = safety_permissions(db, &user).await?;
    note_as(db, &user, &perms, id, data.into_inner()).await.map(Json)
}

pub(crate) async fn note_as(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    id: &str,
    data: DataSafetyNote,
) -> Result<SafetyCase> {
    validate(&data)?;
    require(perms, PlatformPermission::SafetyReview)?;
    let case = db.fetch_safety_case(id).await?;
    log(db, id, &user.id, "Note", None, None, Some(clean_text(&data.text))).await?;
    Ok(case)
}

/// # Bulk triage
///
/// Change status, priority or assignment of up to 50 cases. Never takes
/// moderation actions; each case is checked on its own.
#[openapi(tag = "Trust & Safety")]
#[post("/admin/reports/bulk", data = "<data>")]
pub async fn bulk(
    db: &State<Database>,
    user: User,
    data: Json<DataSafetyBulk>,
) -> Result<Json<SafetyBulkResult>> {
    let data = data.into_inner();
    validate(&data)?;
    let perms = safety_permissions(db, &user).await?;
    require(&perms, PlatformPermission::SafetyReview)?;
    if let Some(status) = data.status
        && !matches!(
            status,
            SafetyStatus::Queued
                | SafetyStatus::UnderReview
                | SafetyStatus::NoViolation
                | SafetyStatus::Closed
        )
    {
        return Err(create_error!(InvalidOperation));
    }

    let mut result = SafetyBulkResult {
        updated: vec![],
        failed: BTreeMap::new(),
    };
    let note = data.note.as_deref().map(clean_text).filter(|s| !s.is_empty());
    for id in data.ids.iter().take(50) {
        let outcome: Result<()> = async {
            let before = db.fetch_safety_case(id).await?;
            let mut after = before.clone();
            if let Some(status) = data.status {
                check_transition(&perms, &before, status)?;
                apply_status(&mut after, status, &user.id, None);
            }
            if let Some(priority) = data.priority {
                after.priority = priority;
            }
            if data.assign_self {
                after.assignee_id = Some(user.id.clone());
            }
            let after = save_case(db, &before, after).await?;
            if before.status != after.status {
                log(
                    db,
                    id,
                    &user.id,
                    "Status",
                    Some(format!("{:?}", before.status)),
                    Some(format!("{:?}", after.status)),
                    Some("bulk".into()),
                )
                .await?;
            }
            if before.priority != after.priority {
                log(
                    db,
                    id,
                    &user.id,
                    "Priority",
                    Some(format!("{:?}", before.priority)),
                    Some(format!("{:?}", after.priority)),
                    Some("bulk".into()),
                )
                .await?;
            }
            if before.assignee_id != after.assignee_id {
                log(db, id, &user.id, "Assign", before.assignee_id.clone(), after.assignee_id.clone(), Some("bulk".into())).await?;
            }
            if let Some(note) = &note {
                log(db, id, &user.id, "Note", None, None, Some(note.clone())).await?;
            }
            Ok(())
        }
        .await;
        match outcome {
            Ok(()) => result.updated.push(id.clone()),
            Err(error) => {
                result
                    .failed
                    .insert(id.clone(), format!("{:?}", error.error_type));
            }
        }
    }
    Ok(Json(result))
}

/// Moderation history of one account (reviewers)
#[derive(Serialize, JsonSchema)]
pub struct SafetyUserHistory {
    pub actions: Vec<SafetyAction>,
    pub appeals: Vec<SafetyAppeal>,
    /// Reports about content this account is responsible for
    pub reported: Vec<SafetyCase>,
}

/// # Account history
///
/// Actions, appeals and reports concerning one account.
#[openapi(tag = "Trust & Safety")]
#[get("/admin/users/<user_id>")]
pub async fn user_history(
    db: &State<Database>,
    user: User,
    user_id: &str,
) -> Result<Json<SafetyUserHistory>> {
    let perms = safety_permissions(db, &user).await?;
    require(&perms, PlatformPermission::SafetyReview)?;
    user_history_of(db, user_id).await.map(Json)
}

pub(crate) async fn user_history_of(db: &Database, user_id: &str) -> Result<SafetyUserHistory> {
    let reported = db
        .fetch_safety_cases(revolt_database::SafetyCaseQuery {
            search: Some(user_id.to_string()),
            limit: 50,
            ..Default::default()
        })
        .await?
        .into_iter()
        .filter(|c| c.target_owner_id.as_deref() == Some(user_id) || c.target.id == user_id)
        .collect();
    Ok(SafetyUserHistory {
        actions: db.fetch_safety_actions_for_user(user_id).await?,
        appeals: db.fetch_safety_appeals_for_user(user_id).await?,
        reported,
    })
}

/// What the reporter is told after a status change
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Told {
    NoViolation,
    Duplicate,
    Closed,
    /// Only the reviewer's note (the decision was told before)
    Note,
    Info,
}

/// Which message, if any, a status change sends to the reporter. A decision
/// is told once: closing a case that was already decided (or had an action)
/// adds nothing, except a note the reviewer wrote for the reporter.
pub(crate) fn reporter_message(
    before: SafetyStatus,
    after: SafetyStatus,
    had_action: bool,
    public_note: Option<&str>,
) -> Option<Told> {
    let told_already = had_action
        || matches!(
            before,
            SafetyStatus::ActionTaken
                | SafetyStatus::NoViolation
                | SafetyStatus::Duplicate
                | SafetyStatus::Closed
                | SafetyStatus::AppealResolved
        );
    match after {
        SafetyStatus::NoViolation => Some(Told::NoViolation),
        SafetyStatus::Duplicate => Some(Told::Duplicate),
        SafetyStatus::Closed if !told_already => Some(Told::Closed),
        SafetyStatus::Closed if public_note.is_some() && before != SafetyStatus::Closed => Some(Told::Note),
        SafetyStatus::MoreInfoRequired => Some(Told::Info),
        _ => None,
    }
}

#[cfg(test)]
mod reporter_message_tests {
    use super::*;

    #[test]
    fn each_decision_is_told_once() {
        use SafetyStatus::*;
        // a decision is told when made
        assert_eq!(reporter_message(UnderReview, NoViolation, false, None), Some(Told::NoViolation));
        assert_eq!(reporter_message(Queued, Duplicate, false, None), Some(Told::Duplicate));
        assert_eq!(reporter_message(UnderReview, Closed, false, None), Some(Told::Closed));
        // ... and closing it afterwards says nothing new
        assert_eq!(reporter_message(NoViolation, Closed, false, None), None);
        assert_eq!(reporter_message(Duplicate, Closed, false, None), None);
        assert_eq!(reporter_message(ActionTaken, Closed, true, None), None);
        assert_eq!(reporter_message(AppealResolved, Closed, true, None), None);
        // a note the reviewer wrote for the reporter is still sent
        assert_eq!(reporter_message(ActionTaken, Closed, true, Some("Thanks")), Some(Told::Note));
        assert_eq!(reporter_message(NoViolation, Closed, false, Some("Thanks")), Some(Told::Note));
        // requests for information are told, other moves are not
        assert_eq!(reporter_message(UnderReview, MoreInfoRequired, false, Some("Which?")), Some(Told::Info));
        assert_eq!(reporter_message(Queued, UnderReview, false, None), None);
    }
}
