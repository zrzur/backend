//! Moderation actions. Only reviewers with `safety.action` take them, one
//! case at a time, never automatically. Nothing here deletes an account,
//! server, bot or purchase; a removed message stays in the case evidence.

use revolt_database::{
    community_transition, events::client::EventV1, safety_now, staff_now, CommunityItem,
    CommunityItemKind, CommunityItemStatus, CommunityReview, CommunityReviewAction, Database,
    DiscoverRequestStatus, DiscoverRequestType,
    PartialBot, PartialUser, PlatformPermission, PlatformPermissions, User, BOT_FLAG_SUSPENDED,
};
use revolt_models::v0::{
    default_territorial_scope, DataSafetyAction, DataSafetyRevoke, DataSafetyStatement,
    SafetyAction, SafetyActionBasis, SafetyActionKind, SafetyCase, SafetyCategory, SafetyGround,
    SafetyPriority, SafetyResolution, SafetyRevocation, SafetyRoute, SafetyStatus, SafetyTarget,
    SafetyTargetKind, UserFlags, SAFETY_POLICY_VERSION,
};
use revolt_result::{create_error, ErrorType, Result};
use rocket::{serde::json::Json, State};
use serde::Deserialize;
use ulid::Ulid;

use super::{clean_text, log, require, safety_permissions, save_case, targets, validate};
use crate::util::official::ReportOutcome;
use crate::routes::discover::listing::set_discoverable;

const BANNED: i32 = UserFlags::Banned as i32;

/// The bot an action is about, if any
fn bot_id(case: &SafetyCase) -> Option<String> {
    match case.target.kind {
        SafetyTargetKind::Bot | SafetyTargetKind::Application | SafetyTargetKind::DiscoverBot => {
            Some(case.target.id.clone())
        }
        _ => None,
    }
}

/// Accounts staff can't act against: themselves, privileged accounts and
/// platform staff (remove their staff access first)
async fn check_subject(db: &Database, actor: &User, subject: &str) -> Result<User> {
    if subject == actor.id {
        return Err(create_error!(CannotReportYourself));
    }
    let user = db.fetch_user(subject).await?;
    if user.privileged {
        return Err(create_error!(NotPrivileged));
    }
    if db
        .fetch_staff_grant(subject)
        .await?
        .is_some_and(|grant| grant.is_active(staff_now()))
    {
        return Err(create_error!(NotPrivileged));
    }
    Ok(user)
}

/// Carry out an action. Returns the account whose standing it affects.
async fn enforce(
    db: &Database,
    actor: &User,
    case: &SafetyCase,
    kind: SafetyActionKind,
    duration_days: Option<u32>,
    reason: &str,
) -> Result<(Option<String>, Option<i64>)> {
    let owner = case.target_owner_id.clone();
    let now = safety_now();
    match kind {
        // recorded by the creator routes, never taken on a case
        SafetyActionKind::SuspendCreator => Err(create_error!(InvalidOperation)),
        SafetyActionKind::Warning => {
            let owner = owner.ok_or_else(|| create_error!(InvalidOperation))?;
            check_subject(db, actor, &owner).await?;
            Ok((Some(owner), None))
        }
        SafetyActionKind::RemoveMessage => {
            let message_id = match case.target.kind {
                SafetyTargetKind::Message => Some(case.target.id.clone()),
                _ => case.target.message_id.clone(),
            }
            .ok_or_else(|| create_error!(InvalidOperation))?;
            // already gone is fine: the evidence snapshot keeps it
            if let Ok(message) = db.fetch_message(&message_id).await {
                if let Some(owner) = &owner {
                    check_subject(db, actor, owner).await?;
                }
                // the same files forwarded elsewhere, or the original a
                // removed copy came from, go as well
                let mut files = message.attachments.clone().unwrap_or_default();
                for id in revolt_database::Message::embed_file_ids(message.embeds.as_deref()) {
                    if let Ok(file) = db.fetch_attachment("attachments", &id).await {
                        files.push(file);
                    }
                }
                revolt_database::File::remove_with_copies(db, &files).await?;
                message.delete(db).await?;
            }
            Ok((owner, None))
        }
        SafetyActionKind::SuspendAccount | SafetyActionKind::BanAccount => {
            let owner = owner.ok_or_else(|| create_error!(InvalidOperation))?;
            let mut user = check_subject(db, actor, &owner).await?;
            if user.bot.is_some() {
                // bots are disabled, not suspended
                return Err(create_error!(IsBot));
            }
            if kind == SafetyActionKind::SuspendAccount {
                let days = duration_days.ok_or_else(|| create_error!(FailedValidation {
                    error: "duration_days required".to_string()
                }))?;
                // no email: the user sees the reason in their account standing
                user.suspend(db, Some(days as usize), None).await?;
                Ok((Some(owner), Some(now + days as i64 * 24 * 60 * 60 * 1000)))
            } else {
                let mut account = db.fetch_account(&user.id).await?;
                account.disable(db).await?;
                user.update(
                    db,
                    PartialUser {
                        flags: Some(user.flags.unwrap_or_default() | BANNED),
                        ..Default::default()
                    },
                    vec![],
                )
                .await?;
                Ok((Some(owner), None))
            }
        }
        SafetyActionKind::DisableBot => {
            let id = bot_id(case).ok_or_else(|| create_error!(InvalidOperation))?;
            let mut bot = db.fetch_bot(&id).await?;
            check_subject(db, actor, &bot.owner).await?;
            bot.update(
                db,
                PartialBot {
                    flags: Some(bot.flags.unwrap_or_default() | BOT_FLAG_SUSPENDED),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
            if bot.discoverable {
                set_discoverable(db, DiscoverRequestType::Bot, &id, false).await?;
            }
            // drop the bot's open connections; its token stops working
            EventV1::Logout.p(id.clone()).await;
            Ok((Some(bot.owner), None))
        }
        SafetyActionKind::RemoveDiscoverListing => {
            let request_type = match case.target.kind {
                SafetyTargetKind::Server | SafetyTargetKind::DiscoverServer => {
                    DiscoverRequestType::Server
                }
                SafetyTargetKind::Bot
                | SafetyTargetKind::Application
                | SafetyTargetKind::DiscoverBot => DiscoverRequestType::Bot,
                _ => return Err(create_error!(InvalidOperation)),
            };
            set_discoverable(db, request_type.clone(), &case.target.id, false).await?;
            if let Ok(mut request) = db
                .fetch_discover_request_by_item_id(request_type, &case.target.id)
                .await
            {
                request.status = DiscoverRequestStatus::Removed(Some(reason.to_string()));
                request.featured = false;
                db.save_discover_request(&request).await?;
            }
            Ok((owner, None))
        }
        SafetyActionKind::SuspendCommunityItem => {
            if case.target.kind != SafetyTargetKind::CommunityItem {
                return Err(create_error!(InvalidOperation));
            }
            let before = db.fetch_community_item(&case.target.id).await?;
            let status = community_transition(&before.status, &CommunityReviewAction::Suspend)
                .ok_or_else(|| create_error!(InvalidOperation))?;
            let mut after = before.clone();
            after.status = status;
            after.review_note = Some(reason.to_string());
            after.version = before.version + 1;
            if !db.update_community_item(&after, before.version, false).await? {
                return Err(create_error!(CommunityItemConflict));
            }
            take_off_wearers(db, &after, 2000).await?;
            revolt_database::util::cdn_purge::purge_files_later(vec![after.file.clone()]);
            db.insert_community_review(&CommunityReview {
                id: Ulid::new().to_string(),
                item_id: after.id.clone(),
                revision: after.revision,
                actor_id: actor.id.clone(),
                action: CommunityReviewAction::Suspend,
                reason: Some(format!("Trust & Safety {}", case.reference)),
                created_at: now,
            })
            .await?;
            Ok((Some(after.creator_id), None))
        }
    }
}

/// A reviewer or the report counter took a design down outside the Safety
/// queue: it gets the same case, action, statement of reasons and appeal as
/// one taken there (D-23). An automatic takedown stays an open, urgent case:
/// it counts as temporary until a person looks.
pub(crate) async fn record_takedown(
    db: &Database,
    actor: &str,
    item: &CommunityItem,
    reason: &str,
    automated: bool,
) -> Result<()> {
    let now = safety_now();
    let id = Ulid::new().to_string();
    let target = SafetyTarget {
        kind: SafetyTargetKind::CommunityItem,
        id: item.id.clone(),
        facet: None,
        message_id: None,
    };
    let mut evidence = targets::community_item_evidence(item, now);
    evidence.case_id = id.clone();
    for file in &evidence.files {
        revolt_database::File::mark_reported_with_copies(db, &file.tag, &file.id).await.ok();
    }
    db.insert_safety_evidence(&evidence).await?;

    let action = SafetyAction {
        id: Ulid::new().to_string(),
        case_id: Some(id.clone()),
        actor_id: actor.to_string(),
        kind: SafetyActionKind::SuspendCommunityItem,
        target: target.clone(),
        subject_user_id: Some(item.creator_id.clone()),
        category: SafetyCategory::Other,
        public_reason: clean_text(reason),
        internal_note: None,
        created_at: now,
        expires_at: None,
        revoked: None,
        appealable: true,
        automated_detection: automated,
        automated_decision: automated,
        ground: SafetyGround::Terms {
            rule: format!("{}{:?}", crate::util::official::RULE_PREFIX, SafetyCategory::Other),
        },
        facts: clean_text(reason),
        territorial_scope: default_territorial_scope(),
        basis: SafetyActionBasis::OwnInitiative,
    };
    db.insert_safety_action(&action).await?;

    let case = SafetyCase {
        reference: revolt_models::v0::safety_reference(&id),
        id: id.clone(),
        reporter_id: actor.to_string(),
        public_reporter: None,
        target,
        target_owner_id: Some(item.creator_id.clone()),
        server_id: None,
        channel_id: None,
        channel_type: None,
        route: SafetyRoute::Platform,
        category: SafetyCategory::Other,
        subcategory: Some(if automated { "automatic" } else { "staff" }.to_string()),
        description: format!("Design taken down: {}", clean_text(reason)),
        evidence_ids: vec![evidence.id.clone()],
        status: if automated {
            SafetyStatus::Queued
        } else {
            SafetyStatus::ActionTaken
        },
        priority: if automated {
            SafetyPriority::Urgent
        } else {
            SafetyPriority::Normal
        },
        assignee_id: None,
        resolution: (!automated).then(|| SafetyResolution {
            decided_by: actor.to_string(),
            decided_at: now,
            public_note: None,
            action_ids: vec![action.id.clone()],
        }),
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
    log(db, &id, actor, "Action", Some(action.id.clone()), Some(format!("{:?}", action.kind)), None).await?;
    crate::util::official::action_taken(db, &item.creator_id, &action).await;
    Ok(())
}

type Words = std::collections::HashMap<String, String>;

/// Title and body of the notice for people wearing a design that was taken
/// down, in their language
pub(crate) fn wearer_notice(kind: &CommunityItemKind) -> (Words, Words) {
    let words = |en: &str, ar: &str, de: &str| {
        Words::from([
            ("en".to_string(), en.to_string()),
            ("ar".to_string(), ar.to_string()),
            ("de".to_string(), de.to_string()),
        ])
    };
    match kind {
        CommunityItemKind::Effect => (
            words("A profile effect was removed", "أُزيل مؤثر من ملفك الشخصي", "Ein Profileffekt wurde entfernt"),
            words(
                "The effect on your profile is no longer available, so it has been taken off. You can choose another one on Designs.",
                "لم يعد المؤثر الموجود في ملفك الشخصي متاحًا، لذا أُزيل منه. يمكنك اختيار مؤثر آخر من صفحة التصاميم.",
                "Der Effekt auf deinem Profil ist nicht mehr verfügbar und wurde entfernt. Unter Designs kannst du einen anderen wählen.",
            ),
        ),
        CommunityItemKind::Decoration => (
            words("A decoration was removed", "أُزيل إطار من ملفك الشخصي", "Ein Avatar-Rahmen wurde entfernt"),
            words(
                "The decoration on your profile is no longer available, so it has been taken off. You can choose another one on Designs.",
                "لم يعد الإطار الموجود في ملفك الشخصي متاحًا، لذا أُزيل منه. يمكنك اختيار إطار آخر من صفحة التصاميم.",
                "Der Avatar-Rahmen auf deinem Profil ist nicht mehr verfügbar und wurde entfernt. Unter Designs kannst du einen anderen wählen.",
            ),
        ),
        CommunityItemKind::Nameplate => (
            words("A nameplate was removed", "أُزيلت لوحة اسم من ملفك الشخصي", "Ein Namensschild wurde entfernt"),
            words(
                "The nameplate on your profile is no longer available, so it has been taken off. You can choose another one on Designs.",
                "لم تعد لوحة الاسم الموجودة في ملفك الشخصي متاحة، لذا أُزيلت منه. يمكنك اختيار لوحة أخرى من صفحة التصاميم.",
                "Das Namensschild auf deinem Profil ist nicht mehr verfügbar und wurde entfernt. Unter Designs kannst du ein anderes wählen.",
            ),
        ),
    }
}

/// A design that was taken down comes off everyone first; nobody waits on the
/// notices for that. Those wearing it then hear it from us in the background,
/// in their language. Who reported it, and why, is not theirs to know.
pub(crate) async fn take_off_wearers(db: &Database, item: &CommunityItem, limit: usize) -> Result<()> {
    // read while they still wear it; a failed read costs the notices, not the takedown
    let wearers = db.community_item_wearers(&item.id, limit).await.unwrap_or_default();
    db.unset_community_item_from_users(&item.id).await?;
    let (title, body) = wearer_notice(&item.kind);
    let db = db.clone();
    tokio::spawn(async move {
        for user in wearers {
            crate::util::official::notice(&db, &user, &title, &body, None).await;
        }
    });
    Ok(())
}

/// Undo what can be undone. Removed messages and Discover listings can't
/// be restored here (the owner can apply to Discover again).
async fn reverse(db: &Database, actor: &User, action: &SafetyAction) -> Result<()> {
    match action.kind {
        SafetyActionKind::SuspendAccount | SafetyActionKind::BanAccount => {
            if let Some(subject) = &action.subject_user_id {
                let mut user = db.fetch_user(subject).await?;
                if action.kind == SafetyActionKind::BanAccount {
                    user.update(
                        db,
                        PartialUser {
                            flags: Some(user.flags.unwrap_or_default() & !BANNED),
                            ..Default::default()
                        },
                        vec![],
                    )
                    .await?;
                }
                user.unsuspend(db).await?;
            }
        }
        SafetyActionKind::DisableBot => {
            let mut bot = db.fetch_bot(&action.target.id).await?;
            bot.update(
                db,
                PartialBot {
                    flags: Some(bot.flags.unwrap_or_default() & !BOT_FLAG_SUSPENDED),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
        }
        SafetyActionKind::SuspendCommunityItem => {
            // erased with its creator's account: nothing is left to put back
            let before = match db.fetch_community_item(&action.target.id).await {
                Err(error) if matches!(error.error_type, ErrorType::NotFound) => return Ok(()),
                found => found?,
            };
            if before.status == CommunityItemStatus::Suspended {
                let mut after = before.clone();
                after.status = CommunityItemStatus::Published;
                after.version = before.version + 1;
                if !db.update_community_item(&after, before.version, false).await? {
                    return Err(create_error!(CommunityItemConflict));
                }
                db.insert_community_review(&CommunityReview {
                    id: Ulid::new().to_string(),
                    item_id: after.id.clone(),
                    revision: after.revision,
                    actor_id: actor.id.clone(),
                    action: CommunityReviewAction::Approve,
                    reason: Some("Trust & Safety action reversed".to_string()),
                    created_at: safety_now(),
                })
                .await?;
            }
        }
        SafetyActionKind::SuspendCreator => {
            crate::routes::community::admin::lift_creator_suspension(
                db,
                &actor.id,
                &action.target.id,
                "Trust & Safety action reversed".to_string(),
            )
            .await?;
        }
        SafetyActionKind::Warning
        | SafetyActionKind::RemoveMessage
        | SafetyActionKind::RemoveDiscoverListing => {}
    }
    Ok(())
}

/// Revoke an action (reviewer revoke or overturned appeal)
pub(crate) async fn revoke_action(
    db: &Database,
    actor: &User,
    mut action: SafetyAction,
    reason: String,
) -> Result<SafetyAction> {
    if action.revoked.is_some() {
        return Err(create_error!(NoEffect));
    }
    reverse(db, actor, &action).await?;
    action.revoked = Some(SafetyRevocation {
        actor_id: actor.id.clone(),
        reason: reason.clone(),
        at: safety_now(),
    });
    db.save_safety_action(&action).await?;
    if let Some(case_id) = &action.case_id {
        log(
            db,
            case_id,
            &actor.id,
            "ActionRevoked",
            Some(action.id.clone()),
            Some(format!("{:?}", action.kind)),
            Some(reason),
        )
        .await?;
    }
    Ok(action)
}

/// # Take action
///
/// Act on a case. The case moves to `ActionTaken`; the affected account
/// sees the public reason in its standing, never the reporter or notes.
#[openapi(tag = "Trust & Safety")]
#[post("/admin/reports/<id>/actions", data = "<data>")]
pub async fn take(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataTakeSafetyAction>,
) -> Result<Json<SafetyAction>> {
    let perms = safety_permissions(db, &user).await?;
    let DataTakeSafetyAction { action, statement } = data.into_inner();
    take_with_statement(db, &user, &perms, id, action, statement).await.map(Json)
}

/// An action with its statement of reasons (DSA Art. 17). Clients should
/// always send the statement. Until every client does, a request without one
/// is accepted and gets a default (our terms for the case's category, the
/// reason as the facts) and a warning in the log; the statement becomes
/// required once the apps in use send it.
#[derive(Deserialize, JsonSchema)]
pub struct DataTakeSafetyAction {
    #[serde(flatten)]
    pub action: DataSafetyAction,
    #[serde(default)]
    pub statement: Option<DataSafetyStatement>,
}

/// What a request without a statement gets: our terms for the category
/// (translated when shown), the reason as the facts, nothing automated
fn default_statement(case: &SafetyCase, data: &DataSafetyAction) -> DataSafetyStatement {
    DataSafetyStatement {
        ground: SafetyGround::Terms {
            rule: format!(
                "{}{:?}",
                crate::util::official::RULE_PREFIX,
                data.category.unwrap_or(case.category)
            ),
        },
        facts: data.public_reason.clone(),
        territorial_scope: default_territorial_scope(),
        automated_detection: false,
        automated_decision: false,
    }
}

/// Check a statement of reasons: every part present and of a sensible size
pub(crate) fn validate_statement(statement: &DataSafetyStatement) -> Result<()> {
    validate(statement)?;
    let missing = |error: &str| create_error!(FailedValidation { error: error.to_string() });
    let filled = |text: &str, max: usize| {
        let n = clean_text(text).chars().count();
        n > 0 && n <= max
    };
    match &statement.ground {
        SafetyGround::Law { law, provision } => {
            if !filled(law, 200) || !filled(provision, 200) {
                return Err(missing("ground: law and provision are required"));
            }
        }
        SafetyGround::Terms { rule } => {
            if !filled(rule, 200) {
                return Err(missing("ground: rule is required"));
            }
        }
    }
    if clean_text(&statement.facts).chars().count() < 3 || !filled(&statement.territorial_scope, 200) {
        return Err(missing("facts and territorial_scope are required"));
    }
    Ok(())
}

fn clean_ground(ground: SafetyGround) -> SafetyGround {
    match ground {
        SafetyGround::Law { law, provision } => SafetyGround::Law {
            law: clean_text(&law),
            provision: clean_text(&provision),
        },
        SafetyGround::Terms { rule } => SafetyGround::Terms { rule: clean_text(&rule) },
    }
}

/// For callers that only have the old request (the admin console's account
/// and bot actions): the statement is derived from the case (see
/// `default_statement`). New callers pass a real statement.
pub(crate) async fn take_as(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    id: &str,
    data: DataSafetyAction,
) -> Result<SafetyAction> {
    take_with_statement(db, user, perms, id, data, None).await
}

pub(crate) async fn take_with_statement(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    id: &str,
    data: DataSafetyAction,
    statement: Option<DataSafetyStatement>,
) -> Result<SafetyAction> {
    validate(&data)?;
    if let Some(statement) = &statement {
        validate_statement(statement)?;
    }
    require(perms, PlatformPermission::SafetyAction)?;

    let mut before = db.fetch_safety_case(id).await?;
    let statement = match statement {
        Some(statement) => statement,
        None => {
            log::warn!("action on case {id} without a statement of reasons: default used (deprecated)");
            default_statement(&before, &data)
        }
    };
    if before.version != data.version {
        return Err(create_error!(SafetyCaseConflict));
    }
    if !(before.status.is_open() || before.status == SafetyStatus::ActionTaken) {
        return Err(create_error!(InvalidOperation));
    }
    if before.status == SafetyStatus::Escalated {
        require(perms, PlatformPermission::SafetyManage)?;
    }
    // the heaviest actions are senior decisions
    if data.kind == SafetyActionKind::BanAccount {
        require(perms, PlatformPermission::SafetyManage)?;
    }
    // only now, with every permission checked: a report without an account
    // has no copy of the content yet, so take it under this reviewer before
    // anything is removed, and find who is responsible when the report did
    // not say. Both are saved at once, so they survive a failing action.
    let original = before.clone();
    prepare_public_case(db, user, &mut before).await?;
    if before != original {
        before = save_case(db, &original, before).await?;
    }
    if data.kind == SafetyActionKind::RemoveMessage && before.evidence_ids.is_empty() {
        return Err(create_error!(FailedValidation {
            error: "no evidence of the message is kept yet".to_string()
        }));
    }
    // without a responsible account nobody can be told (Art. 17), so the
    // action is refused unless it only removes a message whose author is gone
    if before.target_owner_id.is_none() && data.kind != SafetyActionKind::RemoveMessage {
        return Err(create_error!(FailedValidation {
            error: "the account responsible for the target is unknown".to_string()
        }));
    }

    let public_reason = clean_text(&data.public_reason);
    let (subject, expires_at) =
        enforce(db, user, &before, data.kind, data.duration_days, &public_reason).await?;

    let now = safety_now();
    let basis = if before.subcategory.as_deref() == Some("staff") {
        SafetyActionBasis::OwnInitiative
    } else if before.public_reporter.is_some() || matches!(statement.ground, SafetyGround::Law { .. }) {
        SafetyActionBasis::Notice
    } else {
        SafetyActionBasis::Report
    };
    let action = SafetyAction {
        id: Ulid::new().to_string(),
        case_id: Some(before.id.clone()),
        actor_id: user.id.clone(),
        kind: data.kind,
        target: before.target.clone(),
        subject_user_id: subject,
        category: data.category.unwrap_or(before.category),
        public_reason,
        internal_note: data.internal_note.as_deref().map(clean_text).filter(|s| !s.is_empty()),
        created_at: now,
        expires_at,
        revoked: None,
        appealable: true,
        automated_detection: statement.automated_detection,
        automated_decision: statement.automated_decision,
        ground: clean_ground(statement.ground),
        facts: clean_text(&statement.facts),
        territorial_scope: clean_text(&statement.territorial_scope),
        basis,
    };
    db.insert_safety_action(&action).await?;

    let mut after = before.clone();
    after.status = SafetyStatus::ActionTaken;
    let mut action_ids = before
        .resolution
        .as_ref()
        .map(|r| r.action_ids.clone())
        .unwrap_or_default();
    action_ids.push(action.id.clone());
    after.resolution = Some(SafetyResolution {
        decided_by: user.id.clone(),
        decided_at: now,
        // the reviewer may answer the reporter with the decision
        public_note: data
            .reporter_note
            .as_deref()
            .map(clean_text)
            .filter(|s| !s.is_empty())
            .or_else(|| before.resolution.as_ref().and_then(|r| r.public_note.clone())),
        action_ids,
    });
    if after.assignee_id.is_none() {
        after.assignee_id = Some(user.id.clone());
    }
    // the action is already done; a concurrent edit must not hide it
    if save_case(db, &before, after.clone()).await.is_err() {
        let current = db.fetch_safety_case(id).await?;
        let mut retry = current.clone();
        retry.status = SafetyStatus::ActionTaken;
        retry.resolution = after.resolution;
        for evidence in &after.evidence_ids {
            if !retry.evidence_ids.contains(evidence) {
                retry.evidence_ids.push(evidence.clone());
            }
        }
        if retry.target_owner_id.is_none() {
            retry.target_owner_id = after.target_owner_id.clone();
        }
        save_case(db, &current, retry).await?;
    }
    log(
        db,
        id,
        &user.id,
        "Action",
        Some(action.id.clone()),
        Some(format!("{:?}", action.kind)),
        action.internal_note.clone(),
    )
    .await?;

    // tell the person affected, and answer the reporter, in their private messages
    if let Some(subject) = &action.subject_user_id {
        crate::util::official::action_taken(db, subject, &action).await;
    }
    // tell the person who reported, whatever the reviewer wrote (Art. 16(5))
    let note = data.reporter_note.as_deref().map(clean_text).filter(|n| !n.is_empty());
    if before.public_reporter.is_some()
        || (before.reporter_id != user.id && action.subject_user_id.as_deref() != Some(before.reporter_id.as_str()))
    {
        crate::util::official::report_outcome(db, &before, ReportOutcome::Action(&action), note.as_deref()).await;
    }
    Ok(action)
}

/// # Revoke action
///
/// Reverse a mistaken action where possible and record why.
#[openapi(tag = "Trust & Safety")]
#[post("/admin/actions/<id>/revoke", data = "<data>")]
pub async fn revoke(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataSafetyRevoke>,
) -> Result<Json<SafetyAction>> {
    let perms = safety_permissions(db, &user).await?;
    let data = data.into_inner();
    let reason = clean_text(&data.reason);
    let revoked = revoke_as(db, &user, &perms, id, data).await?;
    if let Some(subject) = &revoked.subject_user_id {
        crate::util::official::action_revoked(db, subject, &reason).await;
    }
    Ok(Json(revoked))
}

pub(crate) async fn revoke_as(
    db: &Database,
    user: &User,
    perms: &PlatformPermissions,
    id: &str,
    data: DataSafetyRevoke,
) -> Result<SafetyAction> {
    validate(&data)?;
    require(perms, PlatformPermission::SafetyAction)?;
    let action = db.fetch_safety_action(id).await?;
    if action.kind == SafetyActionKind::BanAccount {
        require(perms, PlatformPermission::SafetyManage)?;
    }
    let revoked = revoke_action(db, user, action, clean_text(&data.reason)).await?;
    // whoever reported it was told of the decision, so they hear it was
    // reversed; an overturned appeal tells them in `decide`
    if let Some(case_id) = &revoked.case_id
        && let Ok(case) = db.fetch_safety_case(case_id).await
        && (case.public_reporter.is_some()
            || (case.reporter_id != user.id && revoked.subject_user_id.as_deref() != Some(case.reporter_id.as_str())))
    {
        crate::util::official::report_outcome(db, &case, ReportOutcome::Revoked, None).await;
    }
    Ok(revoked)
}

/// Fill in what a report without an account could not know: who is
/// responsible for the target (any kind), and a copy of the content, made
/// under the reviewer's account (an anonymous caller never causes a copy).
/// Kinds that have nothing to copy, and targets that no longer exist, leave
/// the case without a copy; a legal hold tells the reviewer so.
pub(crate) async fn prepare_public_case(db: &Database, reviewer: &User, case: &mut SafetyCase) -> Result<()> {
    if owner_is_stale(db, case).await {
        let (owner, message_id) = super::public_report::resolve_owner(db, case.target.kind, &case.target.id).await;
        case.target_owner_id = owner;
        if case.target.message_id.is_none() {
            case.target.message_id = message_id;
        }
    }
    if case.public_reporter.is_some() && case.evidence_ids.is_empty() {
        capture_public_evidence(db, reviewer, case).await?;
    }
    Ok(())
}

/// Nobody is named yet, or the case names the webhook or bot that posted the
/// message (older cases): neither can be acted against or told anything
async fn owner_is_stale(db: &Database, case: &SafetyCase) -> bool {
    match &case.target_owner_id {
        None => true,
        Some(owner) if matches!(case.target.kind, SafetyTargetKind::Message | SafetyTargetKind::Attachment) => {
            !db.fetch_user(owner).await.is_ok_and(|user| user.bot.is_none())
        }
        Some(_) => false,
    }
}

/// The copy itself. A case under legal hold holds it from the start.
pub(crate) async fn capture_public_evidence(db: &Database, reviewer: &User, case: &mut SafetyCase) -> Result<()> {
    let now = safety_now();
    let mut snap = match case.target.kind {
        SafetyTargetKind::Message | SafetyTargetKind::Attachment => {
            let message_id = match case.target.kind {
                SafetyTargetKind::Message => Some(case.target.id.clone()),
                _ => case.target.message_id.clone(),
            };
            let message = match message_id {
                Some(id) => db.fetch_message(&id).await.ok(),
                None => None,
            };
            // the reported file itself, also when no message holds it
            let file = match case.target.kind {
                SafetyTargetKind::Attachment => db.fetch_attachment("attachments", &case.target.id).await.ok(),
                _ => None,
            };
            match (message, file) {
                (Some(message), file) => {
                    let mut snap = targets::message_evidence(db, &message, now).await?;
                    snap.kind = case.target.kind;
                    if let Some(file) = file {
                        snap.files = vec![targets::file_ref(&file)];
                    }
                    snap
                }
                (None, Some(file)) => targets::file_evidence(&file, now),
                (None, None) => return Ok(()),
            }
        }
        SafetyTargetKind::User => match db.fetch_user(&case.target.id).await {
            Ok(user) => targets::user_evidence(&user, None, now),
            Err(_) => return Ok(()),
        },
        SafetyTargetKind::Server | SafetyTargetKind::DiscoverServer => match db.fetch_server(&case.target.id).await {
            Ok(server) => targets::server_evidence(&server, case.target.kind, now),
            Err(_) => return Ok(()),
        },
        SafetyTargetKind::Bot | SafetyTargetKind::Application | SafetyTargetKind::DiscoverBot => {
            let Ok(bot) = db.fetch_bot(&case.target.id).await else {
                return Ok(());
            };
            let Ok(bot_user) = db.fetch_user(&bot.id).await else {
                return Ok(());
            };
            targets::bot_evidence(&bot, &bot_user, case.target.kind, now)
        }
        SafetyTargetKind::CommunityItem => match db.fetch_community_item(&case.target.id).await {
            Ok(item) => targets::community_item_evidence(&item, now),
            Err(_) => return Ok(()),
        },
        _ => return Ok(()),
    };
    snap.case_id = case.id.clone();
    if case.legal_hold {
        snap.legal_hold = true;
        snap.expires_at = None;
    }
    for file in &snap.files {
        revolt_database::File::mark_reported_with_copies(db, &file.tag, &file.id).await.ok();
    }
    db.insert_safety_evidence(&snap).await?;
    case.evidence_ids.push(snap.id.clone());
    log(db, &case.id, &reviewer.id, "EvidenceCaptured", None, Some(snap.id), None).await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests_support {
    use revolt_models::v0::{
        SafetyCase, SafetyCategory, SafetyPriority, SafetyRoute, SafetyStatus, SafetyTarget,
        SafetyTargetKind,
    };

    pub(crate) fn case(category: SafetyCategory) -> SafetyCase {
        SafetyCase {
            id: "c".into(),
            reference: "ZR-C".into(),
            reporter_id: "r".into(),
            public_reporter: None,
            target: SafetyTarget { kind: SafetyTargetKind::Message, id: "m".into(), facet: None, message_id: None },
            target_owner_id: None,
            server_id: None,
            channel_id: None,
            channel_type: None,
            route: SafetyRoute::Platform,
            category,
            subcategory: None,
            description: String::new(),
            evidence_ids: vec![],
            status: SafetyStatus::Queued,
            priority: SafetyPriority::Normal,
            assignee_id: None,
            resolution: None,
            duplicate_of: None,
            info_request: None,
            policy_version: "test".into(),
            created_at: 0,
            updated_at: 0,
            closed_at: None,
            version: 0,
            legal_hold: false,
        }
    }
}

#[cfg(test)]
mod statement_tests {
    use revolt_models::v0::SafetyCategory;

    use super::*;

    fn statement() -> DataSafetyStatement {
        DataSafetyStatement {
            ground: SafetyGround::Terms { rule: "Spam".into() },
            facts: "Posted the same ad forty times".into(),
            territorial_scope: "worldwide".into(),
            automated_detection: false,
            automated_decision: false,
        }
    }

    #[test]
    fn a_complete_statement_passes() {
        validate_statement(&statement()).unwrap();
    }

    #[test]
    fn the_ground_is_required() {
        let mut s = statement();
        s.ground = SafetyGround::Terms { rule: "  ".into() };
        assert!(validate_statement(&s).is_err());
        s.ground = SafetyGround::Law { law: "StGB".into(), provision: "".into() };
        assert!(validate_statement(&s).is_err());
        s.ground = SafetyGround::Law { law: "StGB".into(), provision: "130".into() };
        validate_statement(&s).unwrap();
    }

    #[test]
    fn facts_and_scope_are_required_and_limited() {
        let mut s = statement();
        s.facts = "ab".into();
        assert!(validate_statement(&s).is_err());
        let mut s = statement();
        s.territorial_scope = "".into();
        assert!(validate_statement(&s).is_err());
        let mut s = statement();
        s.facts = "x".repeat(1001);
        assert!(validate_statement(&s).is_err());
    }

    #[test]
    fn a_request_without_a_statement_still_parses_and_automation_defaults_to_false() {
        // deprecated: until every client sends one, a default is derived
        let without: DataTakeSafetyAction = serde_json::from_value(serde_json::json!({
            "kind": "Warning", "public_reason": "Spam", "version": 0
        }))
        .unwrap();
        assert!(without.statement.is_none());
        let case = crate::routes::safety::actions::tests_support::case(SafetyCategory::HateOrThreats);
        let derived = default_statement(&case, &without.action);
        assert!(matches!(&derived.ground, SafetyGround::Terms { rule } if rule == "rule:HateOrThreats"));
        assert_eq!(derived.facts, "Spam");
        assert!(!derived.automated_detection && !derived.automated_decision);
        let with: DataTakeSafetyAction = serde_json::from_value(serde_json::json!({
            "kind": "Warning", "public_reason": "Spam", "version": 0,
            "statement": { "ground": { "type": "Terms", "rule": "Spam" }, "facts": "Posted ads" }
        }))
        .unwrap();
        let with = with.statement.unwrap();
        assert!(!with.automated_detection && !with.automated_decision);
        assert_eq!(with.territorial_scope, "worldwide");
    }
}
