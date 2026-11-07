//! Account standing and appeals. A user sees the actions against their
//! account and content with the public reason only: never who reported
//! them, the evidence, or reviewer notes. Appealing never lifts anything
//! by itself; a reviewer who did not take the action decides.

use revolt_database::{safety_now, Database, PlatformPermission, PlatformPermissions, User};
use revolt_models::v0::{
    DataSafetyAppeal, DataSafetyAppealDecision, SafetyAction, SafetyActionKind, SafetyAppeal,
    SafetyAppealStatus, SafetyStanding, SafetyStandingEntry, SafetyStandingLevel, SafetyStatus,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use ulid::Ulid;

use super::{actions::revoke_action, clean_text, log, require, safety_permissions, save_case, validate, SYSTEM_ACTOR};

const DAY: i64 = 24 * 60 * 60 * 1000;

/// How long an action counts against standing
fn weight(kind: SafetyActionKind) -> (u32, Option<i64>) {
    match kind {
        SafetyActionKind::Warning | SafetyActionKind::RemoveMessage => (1, Some(90 * DAY)),
        SafetyActionKind::DisableBot
        | SafetyActionKind::RemoveDiscoverListing
        | SafetyActionKind::SuspendCommunityItem
        | SafetyActionKind::SuspendCreator => (2, Some(180 * DAY)),
        SafetyActionKind::SuspendAccount => (3, Some(365 * DAY)),
        SafetyActionKind::BanAccount => (4, None),
    }
}

/// Lifted because the time ran out, not because it was a mistake
fn expired_by_system(action: &SafetyAction) -> bool {
    action
        .revoked
        .as_ref()
        .is_some_and(|r| r.actor_id == SYSTEM_ACTOR && r.reason == "expired")
}

pub(crate) fn standing_of(
    actions: &[SafetyAction],
    appeals: &[SafetyAppeal],
    now: i64,
) -> SafetyStanding {
    let mut level = SafetyStandingLevel::Good;
    let mut points = 0;
    let entries = actions
        .iter()
        .map(|action| {
            let revoked = action.revoked.is_some() && !expired_by_system(action);
            let (weight, counts_for) = weight(action.kind);
            let in_force = action.revoked.is_none()
                && action.expires_at.is_none_or(|at| at > now);
            let counts = !revoked && counts_for.is_none_or(|span| action.created_at + span > now);
            if counts {
                points += weight;
            }
            if in_force
                && matches!(
                    action.kind,
                    SafetyActionKind::SuspendAccount | SafetyActionKind::BanAccount
                )
            {
                level = SafetyStandingLevel::Suspended;
            }
            let appeal = appeals.iter().find(|a| a.action_id == action.id);
            SafetyStandingEntry {
                action_id: action.id.clone(),
                kind: action.kind,
                category: action.category,
                public_reason: action.public_reason.clone(),
                target_kind: action.target.kind,
                created_at: action.created_at,
                expires_at: action.expires_at,
                active: counts,
                revoked,
                appealable: action.appealable && !revoked && appeal.is_none(),
                appeal_status: appeal.map(|a| a.status),
                appeal_note: appeal.and_then(|a| a.decision_note.clone()),
                facts: action.facts.clone(),
                ground: action.ground.clone(),
                automated_detection: action.automated_detection,
                automated_decision: action.automated_decision,
                territorial_scope: action.territorial_scope.clone(),
                basis: action.basis,
            }
        })
        .collect();
    if level != SafetyStandingLevel::Suspended {
        level = match points {
            0 => SafetyStandingLevel::Good,
            1 => SafetyStandingLevel::Limited,
            2..=3 => SafetyStandingLevel::VeryLimited,
            _ => SafetyStandingLevel::AtRisk,
        };
    }
    SafetyStanding { level, entries, out_of_court_body: None }
}

/// # Account standing
///
/// Actions taken on your account or content, and their appeals.
#[openapi(tag = "User Safety")]
#[get("/standing")]
pub async fn standing(db: &State<Database>, user: User) -> Result<Json<SafetyStanding>> {
    let actions = db.fetch_safety_actions_for_user(&user.id).await?;
    let appeals = db.fetch_safety_appeals_for_user(&user.id).await?;
    let mut standing = standing_of(&actions, &appeals, safety_now());
    standing.out_of_court_body = crate::util::official::out_of_court_body();
    Ok(Json(standing))
}

/// # Appeal an action
///
/// One appeal per action. The action stays in force until a reviewer
/// decides.
#[openapi(tag = "User Safety")]
#[post("/standing/<action_id>/appeal", data = "<data>")]
pub async fn appeal(
    db: &State<Database>,
    user: User,
    action_id: &str,
    data: Json<DataSafetyAppeal>,
) -> Result<Json<SafetyAppeal>> {
    let data = data.into_inner();
    validate(&data)?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let action = db.fetch_safety_action(action_id).await?;
    if action.subject_user_id.as_deref() != Some(user.id.as_str()) {
        return Err(create_error!(NotFound));
    }
    if !action.appealable || (action.revoked.is_some() && !expired_by_system(&action)) {
        return Err(create_error!(InvalidOperation));
    }

    let appeal = SafetyAppeal {
        id: Ulid::new().to_string(),
        action_id: action.id.clone(),
        case_id: action.case_id.clone(),
        user_id: user.id.clone(),
        statement: clean_text(&data.statement),
        status: SafetyAppealStatus::Pending,
        reviewer_id: None,
        decision_note: None,
        created_at: safety_now(),
        decided_at: None,
    };
    db.insert_safety_appeal(&appeal).await?;

    if let Some(case_id) = &action.case_id
        && let Ok(before) = db.fetch_safety_case(case_id).await
    {
        if matches!(before.status, SafetyStatus::ActionTaken | SafetyStatus::Closed) {
            let mut after = before.clone();
            after.status = SafetyStatus::Appealed;
            save_case(db, &before, after).await.ok();
        }
        log(
            db,
            case_id,
            SYSTEM_ACTOR,
            "Appeal",
            Some(format!("{:?}", before.status)),
            Some(appeal.id.clone()),
            None,
        )
        .await?;
    }
    Ok(Json(appeal))
}

/// # Appeal queue
#[openapi(tag = "Trust & Safety")]
#[get("/admin/appeals?<status>&<before>&<limit>")]
pub async fn queue(
    db: &State<Database>,
    user: User,
    status: Option<&str>,
    before: Option<&str>,
    limit: Option<usize>,
) -> Result<Json<Vec<SafetyAppeal>>> {
    let perms = safety_permissions(db, &user).await?;
    require(&perms, PlatformPermission::SafetyReview)?;
    appeals_of(db, status, before, limit).await.map(Json)
}

pub(crate) async fn appeals_of(
    db: &Database,
    status: Option<&str>,
    before: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<SafetyAppeal>> {
    let status = match status {
        Some("Pending") | None => Some(SafetyAppealStatus::Pending),
        Some("Upheld") => Some(SafetyAppealStatus::Upheld),
        Some("Overturned") => Some(SafetyAppealStatus::Overturned),
        Some("all") => None,
        _ => return Err(create_error!(InvalidOperation)),
    };
    db.fetch_safety_appeals(
        status,
        before.map(str::to_string),
        limit.unwrap_or(50).clamp(1, 100),
    )
    .await
}

/// # Decide appeal
///
/// Uphold or overturn. Overturning reverses the action where possible. A
/// reviewer can't decide an appeal against their own action unless they
/// are a senior reviewer.
#[openapi(tag = "Trust & Safety")]
#[post("/admin/appeals/<id>", data = "<data>")]
pub async fn decide(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataSafetyAppealDecision>,
) -> Result<Json<SafetyAppeal>> {
    let perms = safety_permissions(db, &user).await?;
    decide_as(db, &user, &perms, id, data.into_inner()).await.map(Json)
}

pub(crate) async fn decide_as(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    id: &str,
    data: DataSafetyAppealDecision,
) -> Result<SafetyAppeal> {
    validate(&data)?;
    require(perms, PlatformPermission::SafetyAction)?;

    let mut appeal = db.fetch_safety_appeal(id).await?;
    if appeal.status != SafetyAppealStatus::Pending {
        return Err(create_error!(InvalidOperation));
    }
    let action = db.fetch_safety_action(&appeal.action_id).await?;
    if action.actor_id == user.id {
        require(perms, PlatformPermission::SafetyManage)?;
    }
    if action.kind == SafetyActionKind::BanAccount {
        require(perms, PlatformPermission::SafetyManage)?;
    }

    let note = clean_text(&data.note);
    appeal.status = if data.overturn {
        SafetyAppealStatus::Overturned
    } else {
        SafetyAppealStatus::Upheld
    };
    appeal.reviewer_id = Some(user.id.clone());
    appeal.decision_note = Some(note.clone());
    appeal.decided_at = Some(safety_now());
    // two reviewers deciding at once: only the first counts
    if !db.decide_safety_appeal(&appeal).await? {
        return Err(create_error!(SafetyCaseConflict));
    }
    if data.overturn && action.revoked.is_none() {
        revoke_action(db, user, action, format!("Appeal overturned: {note}")).await?;
    }

    if let Some(case_id) = &appeal.case_id {
        if let Ok(before) = db.fetch_safety_case(case_id).await
            && before.status == SafetyStatus::Appealed
        {
            let mut after = before.clone();
            after.status = SafetyStatus::AppealResolved;
            save_case(db, &before, after).await.ok();
        }
        log(
            db,
            case_id,
            &user.id,
            "AppealDecision",
            Some(appeal.id.clone()),
            Some(format!("{:?}", appeal.status)),
            Some(note.clone()),
        )
        .await?;
    }
    crate::util::official::appeal_decided(db, &appeal.user_id, data.overturn, &note).await;
    // the person who reported the content learns that the decision was reversed
    if data.overturn
        && let Some(case_id) = &appeal.case_id
        && let Ok(case) = db.fetch_safety_case(case_id).await
    {
        crate::util::official::report_outcome(db, &case, crate::util::official::ReportOutcome::Overturned, None).await;
    }
    Ok(appeal)
}

#[cfg(test)]
mod tests {
    use revolt_models::v0::{SafetyCategory, SafetyRevocation, SafetyTarget, SafetyTargetKind};

    use super::*;

    fn action(kind: SafetyActionKind, created_at: i64, expires_at: Option<i64>) -> SafetyAction {
        SafetyAction {
            id: Ulid::new().to_string(),
            case_id: None,
            actor_id: "reviewer".into(),
            kind,
            target: SafetyTarget {
                kind: SafetyTargetKind::Message,
                id: "m".into(),
                facet: None,
                message_id: None,
            },
            subject_user_id: Some("u".into()),
            category: SafetyCategory::Spam,
            public_reason: "spam".into(),
            internal_note: Some("secret".into()),
            created_at,
            expires_at,
            revoked: None,
            appealable: true,
            automated_detection: false,
            automated_decision: false,
            ground: Default::default(),
            facts: String::new(),
            territorial_scope: "worldwide".into(),
            basis: Default::default(),
        }
    }

    #[test]
    fn standing_levels() {
        let now = 1_000 * DAY;
        assert_eq!(standing_of(&[], &[], now).level, SafetyStandingLevel::Good);
        let warning = action(SafetyActionKind::Warning, now - DAY, None);
        assert_eq!(
            standing_of(&[warning.clone()], &[], now).level,
            SafetyStandingLevel::Limited
        );
        // old warnings stop counting
        let old = action(SafetyActionKind::Warning, now - 200 * DAY, None);
        assert_eq!(standing_of(&[old], &[], now).level, SafetyStandingLevel::Good);
        let suspended = action(SafetyActionKind::SuspendAccount, now - DAY, Some(now + DAY));
        assert_eq!(
            standing_of(&[suspended.clone()], &[], now).level,
            SafetyStandingLevel::Suspended
        );
        // a revoked action does not count
        let mut revoked = suspended;
        revoked.revoked = Some(SafetyRevocation {
            actor_id: "r".into(),
            reason: "mistake".into(),
            at: now,
        });
        let standing = standing_of(&[revoked], &[], now);
        assert_eq!(standing.level, SafetyStandingLevel::Good);
        assert!(standing.entries[0].revoked);
        assert!(!standing.entries[0].appealable);
    }

    #[test]
    fn standing_never_exposes_notes_or_actor() {
        let now = 1_000 * DAY;
        let json = serde_json::to_string(&standing_of(
            &[action(SafetyActionKind::Warning, now, None)],
            &[],
            now,
        ))
        .unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("reviewer"));
    }
}
