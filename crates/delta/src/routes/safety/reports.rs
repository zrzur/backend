use revolt_database::{safety_now, Database, User, SAFETY_DAILY_REPORT_LIMIT};
use revolt_models::v0::{
    safety_categories, safety_needs_description, safety_priority, safety_reason_options,
    safety_reference, safety_subcategories, DataCreateSafetyReport, DataSafetyReporterInfo,
    SafetyCase, SafetyCategory, SafetyPriority, SafetyReasonOption, SafetyReceipt, SafetyRoute,
    SafetyStatus, SafetyTarget,
    SafetyTargetKind, SAFETY_POLICY_VERSION,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use ulid::Ulid;

use super::{clean_text, log, save_case, targets, validate, SYSTEM_ACTOR};

/// Distinct open reporters that raise a case to High
const REPORTERS_FOR_HIGH: usize = 3;

pub(crate) fn receipt(case: &SafetyCase, existing: bool) -> SafetyReceipt {
    SafetyReceipt {
        id: case.id.clone(),
        reference: case.reference.clone(),
        target_kind: case.target.kind,
        category: case.category,
        status: case.status.receipt(),
        info_request: case
            .info_request
            .clone()
            .filter(|_| case.status == SafetyStatus::MoreInfoRequired),
        public_note: case.resolution.as_ref().and_then(|r| r.public_note.clone()),
        created_at: case.created_at,
        updated_at: case.updated_at,
        existing,
    }
}

fn parse_kind(kind: &str) -> Result<SafetyTargetKind> {
    serde_json::from_value(serde_json::Value::String(kind.to_string()))
        .map_err(|_| create_error!(InvalidOperation))
}

/// # Report reasons
///
/// Reason categories (and their sub-reasons) offered for a target kind.
#[openapi(tag = "User Safety")]
#[get("/reasons/<kind>")]
pub async fn reasons(_user: User, kind: &str) -> Result<Json<Vec<SafetyReasonOption>>> {
    Ok(Json(safety_reason_options(parse_kind(kind)?)))
}

/// File a report
pub(crate) async fn file_report(
    db: &Database,
    user: &User,
    data: DataCreateSafetyReport,
) -> Result<SafetyReceipt> {
    // Bots can't file reports, on their own or for a user
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    validate(&data)?;

    let kind = data.target.kind;
    if !safety_categories(kind).contains(&data.category) {
        return Err(create_error!(FailedValidation {
            error: "category is not offered for this target".to_string()
        }));
    }
    let subcategory = data
        .subcategory
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(sub) = subcategory
        && !safety_subcategories(data.category).contains(&sub)
    {
        return Err(create_error!(FailedValidation {
            error: "unknown subcategory".to_string()
        }));
    }
    let description = clean_text(data.description.as_deref().unwrap_or_default());
    if safety_needs_description(data.category)
        && subcategory != Some("flashing")
        && description.chars().count() < 10
    {
        return Err(revolt_result::failed_validation(vec![revolt_result::ValidationProblem {
            min: Some(10),
            ..revolt_result::ValidationProblem::new("description", "length")
        }]));
    }

    let now = safety_now();
    let mut priority = safety_priority(data.category, subcategory);

    // A daily cap stops report spam; urgent reports are never blocked
    if priority != SafetyPriority::Urgent
        && db
            .count_safety_cases_since(&user.id, now - 24 * 60 * 60 * 1000)
            .await?
            >= SAFETY_DAILY_REPORT_LIMIT
    {
        return Err(create_error!(TooManyReports {
            max: SAFETY_DAILY_REPORT_LIMIT
        }));
    }

    let resolved = targets::resolve(db, user, &data.target, now).await?;

    // Official accounts are not people: questions about what they sent go to
    // support, and the public form stays open to anyone
    if let Some(owner) = &resolved.owner_id {
        if revolt_database::is_official_id(db, owner).await {
            return Err(create_error!(OfficialAccount));
        }
    }

    // One open report per reporter and target: return the existing one
    if let Some(existing) = db
        .find_open_safety_case(&user.id, kind, &resolved.target.id)
        .await?
    {
        return Ok(receipt(&existing, true));
    }

    let mut evidence = resolved.evidence;
    evidence.extend(
        targets::extra_message_evidence(
            db,
            user,
            resolved.channel.as_ref(),
            &data.evidence_message_ids,
            now,
        )
        .await?,
    );

    // Many people reporting the same thing raises its priority, nothing more
    let reporters = db.count_safety_reporters(kind, &resolved.target.id).await? + 1;
    if reporters >= REPORTERS_FOR_HIGH && priority < SafetyPriority::High {
        priority = SafetyPriority::High;
    }

    // Community designs keep their own report counter for the gallery
    if kind == SafetyTargetKind::CommunityItem {
        super::super::community::report(
            rocket::State::from(db),
            user.clone(),
            &resolved.target.id,
        )
        .await?;
    }

    let id = Ulid::new().to_string();
    let mut evidence_ids = vec![];
    for mut item in evidence {
        item.case_id = id.clone();
        for file in &item.files {
            db.mark_attachment_as_reported(&file.id).await.ok();
        }
        evidence_ids.push(item.id.clone());
        db.insert_safety_evidence(&item).await?;
    }

    let case = SafetyCase {
        reference: safety_reference(&id),
        id,
        reporter_id: user.id.clone(),
        public_reporter: None,
        target: resolved.target,
        target_owner_id: resolved.owner_id,
        server_id: resolved.server_id,
        channel_id: resolved.channel_id,
        channel_type: resolved.channel_type,
        route: SafetyRoute::Platform,
        category: data.category,
        subcategory: subcategory.map(str::to_string),
        description,
        evidence_ids,
        status: SafetyStatus::Queued,
        priority,
        assignee_id: None,
        resolution: None,
        duplicate_of: None,
        info_request: None,
        policy_version: SAFETY_POLICY_VERSION.to_string(),
        created_at: now,
        updated_at: now,
        closed_at: None,
        version: 0,
        legal_hold: false,
    };
    db.insert_safety_case(&case).await?;
    log(db, &case.id, SYSTEM_ACTOR, "Created", None, Some("Queued".into()), None).await?;

    Ok(receipt(&case, false))
}

/// # Create report
///
/// Report something to Zrzur's Trust & Safety team. The target must be
/// something you can see. Reporting never changes the target, and the
/// reported user is not told who reported them.
#[openapi(tag = "User Safety")]
#[post("/reports", data = "<data>")]
pub async fn create(
    db: &State<Database>,
    user: User,
    data: Json<DataCreateSafetyReport>,
) -> Result<Json<SafetyReceipt>> {
    file_report(db, &user, data.into_inner()).await.map(Json)
}

/// # My reports
///
/// Your reports, newest first.
#[openapi(tag = "User Safety")]
#[get("/reports")]
pub async fn list_mine(db: &State<Database>, user: User) -> Result<Json<Vec<SafetyReceipt>>> {
    Ok(Json(
        db.fetch_safety_cases_by_reporter(&user.id, 50)
            .await?
            .iter()
            .map(|case| receipt(case, false))
            .collect(),
    ))
}

/// # My report
#[openapi(tag = "User Safety")]
#[get("/reports/<id>")]
pub async fn fetch_mine(db: &State<Database>, user: User, id: &str) -> Result<Json<SafetyReceipt>> {
    let case = db.fetch_safety_case(id).await?;
    // someone else's report looks like no report at all
    if case.reporter_id != user.id {
        return Err(create_error!(NotFound));
    }
    Ok(Json(receipt(&case, false)))
}

/// # Answer a request for information
///
/// Add the details a reviewer asked for. Only while the report needs more
/// information.
#[openapi(tag = "User Safety")]
#[post("/reports/<id>/info", data = "<data>")]
pub async fn reply(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataSafetyReporterInfo>,
) -> Result<Json<SafetyReceipt>> {
    let data = data.into_inner();
    validate(&data)?;
    let before = db.fetch_safety_case(id).await?;
    if before.reporter_id != user.id {
        return Err(create_error!(NotFound));
    }
    if before.status != SafetyStatus::MoreInfoRequired {
        return Err(create_error!(InvalidOperation));
    }
    let text = clean_text(&data.text);
    if text.is_empty() {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("text", "required"),
        ]));
    }

    let mut after = before.clone();
    after.status = SafetyStatus::UnderReview;
    after.info_request = None;
    let after = save_case(db, &before, after).await?;
    log(db, id, &user.id, "ReporterInfo", None, None, Some(text)).await?;
    log(
        db,
        id,
        SYSTEM_ACTOR,
        "Status",
        Some("MoreInfoRequired".into()),
        Some("UnderReview".into()),
        None,
    )
    .await?;
    Ok(Json(receipt(&after, false)))
}

/// Open a case about an account on a staff member's own initiative (from
/// the admin console's user page), so actions taken on it show in the
/// account's standing and can be appealed like any other.
pub(crate) async fn open_staff_case(
    db: &Database,
    staff: &User,
    target_kind: SafetyTargetKind,
    target: &User,
    owner_id: &str,
    category: SafetyCategory,
    description: &str,
) -> Result<SafetyCase> {
    let now = safety_now();
    let id = Ulid::new().to_string();
    let mut snap = targets::user_evidence(target, None, now);
    snap.kind = target_kind;
    snap.case_id = id.clone();
    db.insert_safety_evidence(&snap).await?;
    let case = SafetyCase {
        reference: safety_reference(&id),
        id,
        reporter_id: staff.id.clone(),
        public_reporter: None,
        target: SafetyTarget {
            kind: target_kind,
            id: target.id.clone(),
            facet: None,
            message_id: None,
        },
        target_owner_id: Some(owner_id.to_string()),
        server_id: None,
        channel_id: None,
        channel_type: None,
        route: SafetyRoute::Platform,
        category,
        subcategory: Some("staff".to_string()),
        description: clean_text(description),
        evidence_ids: vec![snap.id],
        status: SafetyStatus::UnderReview,
        priority: SafetyPriority::Normal,
        assignee_id: Some(staff.id.clone()),
        resolution: None,
        duplicate_of: None,
        info_request: None,
        policy_version: SAFETY_POLICY_VERSION.to_string(),
        created_at: now,
        updated_at: now,
        closed_at: None,
        version: 0,
        legal_hold: false,
    };
    db.insert_safety_case(&case).await?;
    log(db, &case.id, &staff.id, "Created", None, Some("UnderReview".into()), Some("Opened by staff".into())).await?;
    Ok(case)
}
