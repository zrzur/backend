//! The one gate for every badge change a person asks for.
//!
//! The admin console, the retired `/badges/admin` routes, the owner's
//! `/badge` command and the `zrzur-staff badge` command line all call
//! [`mutate_badge`]; each path only says who is asking and with which real
//! rights ([`BadgeActor`]). The rules, in order:
//!
//! 1. the badge exists; Founder is `Locked` (no path changes it), Staff and
//!    Platform Moderator follow appointments only, a retired (or
//!    `Historical`) badge is never given again, and badges that follow a record are never changed by hand;
//! 2. the person is not a bot, not deleted, and not the one asking;
//! 3. the asker holds the right: `platform.badges.manage` for `Staff`-rule
//!    badges, a staff Owner with a fresh step-up for `Privileged` ones;
//! 4. the eligibility record backs a grant;
//! 5. sensitive changes ([`needs_second_person`]) wait for a different
//!    staff member ([`approve_badge_request`]).
//!
//! The system's own grants (Creator from published designs, Community
//! Builder and Big Community from the member count) come through here too,
//! as [`BadgePath::System`] with the record they rest on: only badges that
//! follow a record, never one a person gives, and never one staff took
//! away ([`BadgeDenial::StaffRevoked`]). Earned achievements
//! ([`PERMANENT_ACHIEVEMENTS`]) stay for good; staff may take one away for
//! fraud, with a second person.
//!
//! Every refusal is written to the platform audit log as Denied with a
//! reason code; every change is written before it is made (Success) and
//! again as Failed if it then fails. Records hold ids, badge ids, codes and
//! the staff reason only: never tokens, codes, evidence or payment data.

use revolt_models::v0::UserFlags;
use revolt_result::{create_error, Error, Result};
use ulid::Ulid;

use crate::{
    badge_definition, badge_eligibility, epoch_ms, grant_badge, held_badges, record_platform_audit,
    revoke_badge, AuditOutcome, AuditRecord, BadgeChangeRequest, BadgeDefinition, BadgeGrant,
    BadgeGrantRule, BadgeRequestStatus, BadgeSource, Database, PlatformAuditQuery, StaffAppointment,
    StaffGrant, StaffPermission, User, BADGE_REQUEST_TTL_MS, BADGE_SYSTEM_ACTOR,
};

/// Where a badge change comes from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgePath {
    /// `POST /staff/users/<id>/badges/*` (admin console)
    StaffConsole,
    /// `POST /badges/admin/grant|revoke`: retired, always refused
    AdminRoute,
    /// The owner's `/badge` command
    BadgeCommand,
    /// Zrzur Desk (no badge actions yet; refused like any path without rights)
    Desk,
    /// `zrzur-staff badge` on the server host
    Cli,
    /// The system, from its own records (no person asks)
    System,
}

impl BadgePath {
    pub fn name(&self) -> &'static str {
        match self {
            BadgePath::StaffConsole => "staff_console",
            BadgePath::AdminRoute => "admin_route",
            BadgePath::BadgeCommand => "badge_command",
            BadgePath::Desk => "desk",
            BadgePath::Cli => "cli",
            BadgePath::System => "system",
        }
    }

    /// Start of the audit action, kept as the paths wrote it before
    fn action_prefix(&self) -> &'static str {
        match self {
            BadgePath::StaffConsole => "staff.badge",
            BadgePath::AdminRoute => "badge.admin",
            BadgePath::BadgeCommand => "badge.command",
            BadgePath::Desk => "staff.desk.badge",
            BadgePath::Cli => "staff.cli.badge",
            BadgePath::System => "badge.system",
        }
    }
}

/// Who asks for a badge change, with the rights they really hold
#[derive(Debug, Clone)]
pub struct BadgeActor {
    /// User id of the person asking
    pub id: String,
    pub path: BadgePath,
    /// Holds `platform.badges.manage` (changes `Staff`-rule badges)
    pub staff: bool,
    /// An active staff Owner (`Privileged`-rule badges, with a step-up)
    pub owner: bool,
    /// Confirmed with a fresh TOTP code
    pub step_up: bool,
    /// Staff session, for the audit record
    pub session_id: Option<String>,
}

impl BadgeActor {
    /// The system acting on its own records: no staff rights at all
    pub fn system() -> BadgeActor {
        BadgeActor {
            id: BADGE_SYSTEM_ACTOR.to_string(),
            path: BadgePath::System,
            staff: false,
            owner: false,
            step_up: false,
            session_id: None,
        }
    }

    /// The rights `user_id`'s staff grant gives right now, without a
    /// step-up. No grant, a disabled or an expired one: no rights. The
    /// `privileged` database flag gives none.
    pub async fn from_staff_grant(db: &Database, user_id: &str, path: BadgePath, now: i64) -> Result<BadgeActor> {
        let grant = db.fetch_staff_grant(user_id).await?;
        let (staff, owner) = match &grant {
            Some(grant) => (
                grant.effective(now).contains(&StaffPermission::BadgesManage),
                grant.is_owner(now),
            ),
            None => (false, false),
        };
        Ok(BadgeActor {
            id: user_id.to_string(),
            path,
            staff,
            owner,
            step_up: false,
            session_id: None,
        })
    }
}

/// Give or take away
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeChange {
    Grant,
    Revoke,
}

impl BadgeChange {
    fn word(&self) -> &'static str {
        match self {
            BadgeChange::Grant => "grant",
            BadgeChange::Revoke => "revoke",
        }
    }
}

/// One asked-for badge change
#[derive(Debug, Clone)]
pub struct BadgeMutation {
    pub user_id: String,
    pub badge: String,
    pub change: BadgeChange,
    /// Staff text, 3 to 500 characters (kept in the audit log)
    pub reason: String,
    /// Grants only: the badge goes away then
    pub expires_at: Option<i64>,
    /// System grants only: the record the grant rests on (`server:<id>`,
    /// `designs`)
    pub source_ref: Option<String>,
}

/// Why a badge change was refused. The code goes to the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadgeDenial {
    /// `/badges/admin/grant|revoke` no longer change anything
    RouteRetired,
    UnknownBadge,
    UnknownUser,
    /// Founder: no path changes it
    Locked,
    /// Staff, Platform Moderator: only an appointment gives or ends them
    Appointment,
    /// A retired badge is never given again
    Retired,
    /// Feather, Creator, Beta, anniversaries...: change the record instead
    FollowsRecord,
    Bot,
    DeletedAccount,
    /// Nobody changes their own badges
    SelfChange,
    /// Lacks `platform.badges.manage`
    NoPermission,
    /// `Privileged` badges: a staff Owner only
    OwnerOnly,
    /// `Privileged` badges: a fresh step-up is needed
    StepUpRequired,
    /// The record does not back the grant (the text says what is missing)
    Ineligible(String),
    /// The second person must differ from the requester and the subject
    SameApprover,
    /// The request is no longer waiting (decided or expired)
    NotPending,
    /// The reason is not 3 to 500 characters (its text is never logged)
    BadReason,
    /// The system never gives back a badge a person took away
    StaffRevoked,
    /// The system never takes away an earned achievement
    Permanent,
}

impl BadgeDenial {
    /// Reason code written to the audit log
    pub fn code(&self) -> &'static str {
        match self {
            BadgeDenial::RouteRetired => "route_retired",
            BadgeDenial::UnknownBadge => "unknown_badge",
            BadgeDenial::UnknownUser => "unknown_user",
            BadgeDenial::Locked => "locked",
            BadgeDenial::Appointment => "appointment_only",
            BadgeDenial::Retired => "retired",
            BadgeDenial::FollowsRecord => "follows_record",
            BadgeDenial::Bot => "bot",
            BadgeDenial::DeletedAccount => "deleted_account",
            BadgeDenial::SelfChange => "self",
            BadgeDenial::NoPermission => "no_permission",
            BadgeDenial::OwnerOnly => "owner_only",
            BadgeDenial::StepUpRequired => "step_up_required",
            BadgeDenial::Ineligible(_) => "ineligible",
            BadgeDenial::SameApprover => "same_person",
            BadgeDenial::NotPending => "not_pending",
            BadgeDenial::BadReason => "bad_reason",
            BadgeDenial::StaffRevoked => "staff_revoked",
            BadgeDenial::Permanent => "permanent",
        }
    }

    /// The error the caller gets
    pub fn error(&self) -> Error {
        match self {
            BadgeDenial::RouteRetired => create_error!(FailedValidation {
                error: ADMIN_ROUTE_RETIRED.to_string()
            }),
            BadgeDenial::UnknownBadge => create_error!(NotFound),
            BadgeDenial::UnknownUser | BadgeDenial::DeletedAccount => create_error!(UnknownUser),
            BadgeDenial::Locked
            | BadgeDenial::Appointment
            | BadgeDenial::Retired
            | BadgeDenial::FollowsRecord
            | BadgeDenial::NotPending
            | BadgeDenial::StaffRevoked
            | BadgeDenial::Permanent => create_error!(InvalidOperation),
            BadgeDenial::Bot => create_error!(IsBot),
            BadgeDenial::SelfChange | BadgeDenial::OwnerOnly | BadgeDenial::SameApprover => {
                create_error!(NotPrivileged)
            }
            BadgeDenial::NoPermission => create_error!(MissingPlatformPermission {
                permission: StaffPermission::BadgesManage.name().to_string()
            }),
            BadgeDenial::StepUpRequired => create_error!(NotElevated),
            BadgeDenial::Ineligible(reason) => create_error!(FailedValidation { error: reason.clone() }),
            BadgeDenial::BadReason => revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::length("reason", 3, 500),
            ]),
        }
    }
}

/// What the retired admin routes answer
pub const ADMIN_ROUTE_RETIRED: &str = "This route is retired. Badges are given and taken only in the admin console (POST /staff/users/<id>/badges/grant or /revoke).";

/// What the rules allow
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeDecision {
    Allowed,
    /// Allowed once a different staff member approves
    NeedsSecondPerson,
}

/// Achievements the system gives from a record and never takes back:
/// Creator (first published design), Community Builder and Big Community
/// (genuine human members). Staff may take one away for fraud or
/// infringement, with a second person; the system then never gives it
/// back.
pub const PERMANENT_ACHIEVEMENTS: [&str; 3] = ["creator", "community_builder", "big_community"];

/// Whether a badge is an earned achievement that stays
pub fn is_permanent_achievement(definition: &BadgeDefinition) -> bool {
    definition.rule == BadgeGrantRule::Automatic && PERMANENT_ACHIEVEMENTS.contains(&definition.id)
}

/// Sensitive changes that need two people: giving Partner or Elite Bug
/// Hunter, and taking away any `Privileged` or `Historical` badge or an
/// earned achievement. (Founder, Staff and Platform Moderator never get
/// here: they are refused before.)
pub fn needs_second_person(definition: &BadgeDefinition, change: BadgeChange) -> bool {
    match change {
        BadgeChange::Grant => matches!(definition.id, "partner" | "elite_bug_hunter"),
        BadgeChange::Revoke => {
            matches!(
                definition.rule,
                BadgeGrantRule::Privileged | BadgeGrantRule::Historical
            ) || is_permanent_achievement(definition)
        }
    }
}

/// The system's own changes: only badges that follow a record, to a person
/// (not a bot, not deleted); an earned achievement is never taken back.
fn system_rule_check(
    definition: &BadgeDefinition,
    subject: &User,
    change: BadgeChange,
) -> std::result::Result<BadgeDecision, BadgeDenial> {
    match definition.rule {
        BadgeGrantRule::Automatic => {}
        BadgeGrantRule::Locked => return Err(BadgeDenial::Locked),
        BadgeGrantRule::Appointment => return Err(BadgeDenial::Appointment),
        BadgeGrantRule::Historical => return Err(BadgeDenial::Retired),
        BadgeGrantRule::Dynamic | BadgeGrantRule::Staff | BadgeGrantRule::Privileged => {
            return Err(BadgeDenial::NoPermission)
        }
    }
    if change == BadgeChange::Grant && definition.retired {
        return Err(BadgeDenial::Retired);
    }
    if change == BadgeChange::Revoke && is_permanent_achievement(definition) {
        return Err(BadgeDenial::Permanent);
    }
    if subject.bot.is_some() {
        return Err(BadgeDenial::Bot);
    }
    if subject.flags.unwrap_or_default() & UserFlags::Deleted as i32 != 0 {
        return Err(BadgeDenial::DeletedAccount);
    }
    Ok(BadgeDecision::Allowed)
}

/// The rules that need no database (1 to 3 above, and whether a second
/// person is needed). Also used to list which badges someone may give.
pub fn badge_rule_check(
    actor: &BadgeActor,
    definition: &BadgeDefinition,
    subject: &User,
    change: BadgeChange,
) -> std::result::Result<BadgeDecision, BadgeDenial> {
    if actor.path == BadgePath::AdminRoute {
        return Err(BadgeDenial::RouteRetired);
    }
    if actor.path == BadgePath::System {
        return system_rule_check(definition, subject, change);
    }
    // an earned achievement may be taken away for fraud (staff, two people)
    let fraud_revoke = change == BadgeChange::Revoke && is_permanent_achievement(definition);
    match definition.rule {
        BadgeGrantRule::Locked => return Err(BadgeDenial::Locked),
        BadgeGrantRule::Appointment => return Err(BadgeDenial::Appointment),
        BadgeGrantRule::Automatic if fraud_revoke => {}
        BadgeGrantRule::Dynamic | BadgeGrantRule::Automatic => return Err(BadgeDenial::FollowsRecord),
        BadgeGrantRule::Historical if change == BadgeChange::Grant => return Err(BadgeDenial::Retired),
        BadgeGrantRule::Staff | BadgeGrantRule::Privileged | BadgeGrantRule::Historical => {}
    }
    if change == BadgeChange::Grant && definition.retired {
        return Err(BadgeDenial::Retired);
    }
    if subject.bot.is_some() {
        return Err(BadgeDenial::Bot);
    }
    if subject.flags.unwrap_or_default() & UserFlags::Deleted as i32 != 0 {
        return Err(BadgeDenial::DeletedAccount);
    }
    if subject.id == actor.id {
        return Err(BadgeDenial::SelfChange);
    }
    match definition.rule {
        BadgeGrantRule::Staff | BadgeGrantRule::Automatic if !actor.staff => {
            return Err(BadgeDenial::NoPermission)
        }
        BadgeGrantRule::Privileged | BadgeGrantRule::Historical if !actor.owner => {
            return Err(BadgeDenial::OwnerOnly)
        }
        BadgeGrantRule::Privileged | BadgeGrantRule::Historical if !actor.step_up => {
            return Err(BadgeDenial::StepUpRequired)
        }
        _ => {}
    }
    Ok(if needs_second_person(definition, change) {
        BadgeDecision::NeedsSecondPerson
    } else {
        BadgeDecision::Allowed
    })
}

/// A change the rules allow
pub struct AuthorizedBadgeChange {
    pub definition: &'static BadgeDefinition,
    pub subject: User,
    pub decision: BadgeDecision,
}

/// Every rule, the eligibility record included. Database errors come back
/// as `Err`; a refusal as `Ok(Err(denial))`.
pub async fn authorize_badge_mutation(
    db: &Database,
    actor: &BadgeActor,
    request: &BadgeMutation,
) -> Result<std::result::Result<AuthorizedBadgeChange, BadgeDenial>> {
    if actor.path == BadgePath::AdminRoute {
        return Ok(Err(BadgeDenial::RouteRetired));
    }
    let Some(definition) = badge_definition(&request.badge) else {
        return Ok(Err(BadgeDenial::UnknownBadge));
    };
    let subject = match db.fetch_user(&request.user_id).await {
        Ok(user) => user,
        Err(error) if matches!(error.error_type, revolt_result::ErrorType::NotFound) => {
            return Ok(Err(BadgeDenial::UnknownUser))
        }
        Err(error) => return Err(error),
    };
    let decision = match badge_rule_check(actor, definition, &subject, request.change) {
        Ok(decision) => decision,
        Err(denial) => return Ok(Err(denial)),
    };
    if request.change == BadgeChange::Grant {
        if actor.path == BadgePath::System && revoked_by_a_person(db, &subject.id, definition.id).await? {
            return Ok(Err(BadgeDenial::StaffRevoked));
        }
        if let Some(reason) = badge_eligibility(db, &subject.id, definition.id).await? {
            return Ok(Err(BadgeDenial::Ineligible(reason)));
        }
    }
    Ok(Ok(AuthorizedBadgeChange {
        definition,
        subject,
        decision,
    }))
}

/// Whether the user's record of this badge was ended by a person (staff
/// took it away): the system never gives it back then
pub async fn revoked_by_a_person(db: &Database, user_id: &str, badge: &str) -> Result<bool> {
    let now = now_ms();
    Ok(db.fetch_badge_grants(user_id).await?.iter().any(|grant| {
        grant.badge == badge
            && !grant.held(now)
            && grant
                .revoked_by
                .as_deref()
                .is_some_and(|by| by != BADGE_SYSTEM_ACTOR)
    }))
}

/// What came of a badge change
#[derive(Debug, Clone)]
pub enum BadgeOutcome {
    Applied(BadgeGrant),
    /// Waits for a second staff member
    Pending(BadgeChangeRequest),
}

fn now_ms() -> i64 {
    epoch_ms(iso8601_timestamp::Timestamp::now_utc())
}

/// The reason as staff typed it: trimmed, 3 to 500 characters
fn checked_reason(reason: &str) -> Result<String> {
    let reason = reason.trim();
    let length = reason.chars().count();
    if !(3..=500).contains(&length) {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length("reason", 3, 500),
        ]));
    }
    Ok(reason.to_string())
}

/// The badge id for a log: only ids the catalogue knows, never user text
fn known(badge: &str) -> &'static str {
    badge_definition(badge).map(|d| d.id).unwrap_or("unknown")
}

fn base_record(actor: &BadgeActor, action: String, user_id: &str, reason: Option<String>) -> AuditRecord {
    AuditRecord {
        actor_id: actor.id.clone(),
        action,
        // the system acts on its records, not on a staff right
        permission: (actor.path != BadgePath::System).then_some(StaffPermission::BadgesManage),
        target_kind: Some("user".to_string()),
        // only a real id: never text a caller typed in its place
        target_id: Ulid::from_string(user_id).ok().map(|_| user_id.to_string()),
        reason,
        staff_session_id: actor.session_id.clone(),
        summary: None,
    }
}

/// Write a refusal and give back its error
async fn deny(db: &Database, mut record: AuditRecord, badge: &str, path: BadgePath, denial: BadgeDenial) -> Error {
    record.summary = Some(format!("{} denied:{} via {}", known(badge), denial.code(), path.name()));
    if let Err(error) = record_platform_audit(db, record, AuditOutcome::Denied).await {
        return error;
    }
    denial.error()
}

/// The success record first, then the change; Failed if it then fails
async fn apply(
    db: &Database,
    mut record: AuditRecord,
    summary: String,
    change: impl std::future::Future<Output = Result<BadgeGrant>>,
) -> Result<BadgeGrant> {
    record.summary = Some(summary.clone());
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    match change.await {
        Ok(grant) => Ok(grant),
        Err(error) => {
            // the variant name only: its payload may hold user text
            let kind: String = format!("{:?}", error.error_type)
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .take(100)
                .collect();
            record.summary = Some(format!("{summary} failed:{kind}"));
            let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
            Err(error)
        }
    }
}

/// Ask for, or make, a badge change. The single entry point for every path.
pub async fn mutate_badge(db: &Database, actor: &BadgeActor, request: BadgeMutation) -> Result<BadgeOutcome> {
    mutate_badge_inner(db, actor, request, None).await
}

async fn mutate_badge_inner(
    db: &Database,
    actor: &BadgeActor,
    request: BadgeMutation,
    approval: Option<&BadgeChangeRequest>,
) -> Result<BadgeOutcome> {
    // Rights first, so a reason of the wrong length never skips the Denied
    // record (security review F3). The retired route never keeps the
    // caller's text, and a reason of the wrong length is never written.
    let reason = checked_reason(&request.reason).ok();
    let action = format!("{}.{}", actor.path.action_prefix(), request.change.word());
    let kept = if actor.path == BadgePath::AdminRoute { None } else { reason.clone() };
    let mut record = base_record(actor, action.clone(), &request.user_id, kept);

    let authorized = match authorize_badge_mutation(db, actor, &request).await? {
        Ok(authorized) => authorized,
        Err(denial) => return Err(deny(db, record, &request.badge, actor.path, denial).await),
    };
    let Some(reason) = reason else {
        record.reason = None;
        return Err(deny(db, record, &request.badge, actor.path, BadgeDenial::BadReason).await);
    };
    let definition = authorized.definition;
    let mut summary = format!("{} via {}", definition.id, actor.path.name());

    if authorized.decision == BadgeDecision::NeedsSecondPerson {
        match approval {
            None => return ask_second_person(db, actor, &request, definition, reason, record).await.map(BadgeOutcome::Pending),
            Some(approved) => {
                if approved.requested_by == actor.id || approved.user_id == actor.id {
                    return Err(deny(db, record, definition.id, actor.path, BadgeDenial::SameApprover).await);
                }
                summary = format!("{summary}; approves {} asked by {}", approved.id, approved.requested_by);
            }
        }
    }

    let subject = authorized.subject.id.clone();
    let grant = match request.change {
        BadgeChange::Grant => {
            apply(
                db,
                record,
                summary,
                grant_badge(
                    db,
                    &subject,
                    definition.id,
                    if actor.path == BadgePath::System { BadgeSource::System } else { BadgeSource::Admin },
                    if actor.path == BadgePath::System { request.source_ref.clone() } else { None },
                    &actor.id,
                    Some(reason),
                    request.expires_at,
                ),
            )
            .await?
        }
        BadgeChange::Revoke => {
            apply(db, record, summary, revoke_badge(db, &subject, definition.id, &actor.id, Some(reason))).await?
        }
    };
    Ok(BadgeOutcome::Applied(grant))
}

/// A sensitive change: a request waits for a second person
async fn ask_second_person(
    db: &Database,
    actor: &BadgeActor,
    request: &BadgeMutation,
    definition: &'static BadgeDefinition,
    reason: String,
    mut record: AuditRecord,
) -> Result<BadgeChangeRequest> {
    let now = now_ms();
    let grant = request.change == BadgeChange::Grant;
    record.action = format!("{}.request", record.action);
    // one open request per change: asking again shows the same one
    if let Some(open) = db.fetch_open_badge_request(&request.user_id, definition.id, grant, now).await? {
        record.summary = Some(format!("{} request {} already open via {}", definition.id, open.id, actor.path.name()));
        record_platform_audit(db, record, AuditOutcome::Success).await?;
        return Ok(open);
    }
    let pending = BadgeChangeRequest {
        id: Ulid::new().to_string(),
        user_id: request.user_id.clone(),
        badge: definition.id.to_string(),
        grant,
        reason,
        grant_expires_at: if grant { request.expires_at } else { None },
        requested_by: actor.id.clone(),
        requested_via: actor.path.name().to_string(),
        status: BadgeRequestStatus::Pending,
        created_at: now,
        expires_at: now + BADGE_REQUEST_TTL_MS,
        decided_by: None,
        decided_at: None,
    };
    record.summary = Some(format!(
        "{} request {} via {} (needs a second person)",
        definition.id,
        pending.id,
        actor.path.name()
    ));
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    db.insert_badge_request(&pending).await?;
    Ok(pending)
}

/// Whether a request still waits
pub fn badge_request_open(request: &BadgeChangeRequest, now: i64) -> bool {
    request.status == BadgeRequestStatus::Pending && now < request.expires_at
}

/// A different staff member approves a request: the change is made with the
/// approver's own rights, checked again in full.
pub async fn approve_badge_request(db: &Database, approver: &BadgeActor, id: &str) -> Result<BadgeGrant> {
    let request = db.fetch_badge_request(id).await?;
    approve_loaded_badge_request(db, approver, request).await
}

/// [`approve_badge_request`] with the request already read (tests use it to
/// stand in for a request decided meanwhile)
#[doc(hidden)]
pub async fn approve_loaded_badge_request(db: &Database, approver: &BadgeActor, request: BadgeChangeRequest) -> Result<BadgeGrant> {
    let action = format!("{}.request.approve", approver.path.action_prefix());
    let record = base_record(approver, action, &request.user_id, None);
    if !badge_request_open(&request, now_ms()) {
        return Err(deny(db, record, &request.badge, approver.path, BadgeDenial::NotPending).await);
    }
    if request.requested_by == approver.id || request.user_id == approver.id {
        return Err(deny(db, record, &request.badge, approver.path, BadgeDenial::SameApprover).await);
    }
    let change = BadgeMutation {
        user_id: request.user_id.clone(),
        badge: request.badge.clone(),
        change: if request.grant { BadgeChange::Grant } else { BadgeChange::Revoke },
        reason: request.reason.clone(),
        expires_at: request.grant_expires_at,
        source_ref: None,
    };
    // the approver's rights before the request is marked: someone without
    // them never holds it as Approved, not even for a moment, so a real
    // approver at the same time is never turned away (security review F4)
    if let Err(denial) = authorize_badge_mutation(db, approver, &change).await? {
        return Err(deny(db, record, &request.badge, approver.path, denial).await);
    }
    let mut decided = request.clone();
    decided.status = BadgeRequestStatus::Approved;
    decided.decided_by = Some(approver.id.clone());
    decided.decided_at = Some(now_ms());
    if !db.decide_badge_request(&decided, BadgeRequestStatus::Pending).await? {
        return Err(deny(db, record, &request.badge, approver.path, BadgeDenial::NotPending).await);
    }
    match mutate_badge_inner(db, approver, change, Some(&request)).await {
        Ok(BadgeOutcome::Applied(grant)) => Ok(grant),
        other => {
            // not made: the request waits again
            if !db.decide_badge_request(&request, BadgeRequestStatus::Approved).await.unwrap_or(false) {
                log::error!("badge request {} could not be put back", request.id);
            }
            match other {
                Err(error) => Err(error),
                Ok(_) => Err(create_error!(InternalError)),
            }
        }
    }
}

/// Refuse a request (its requester may withdraw it). Never the person the
/// badge is about.
pub async fn refuse_badge_request(db: &Database, actor: &BadgeActor, id: &str) -> Result<BadgeChangeRequest> {
    let request = db.fetch_badge_request(id).await?;
    refuse_loaded_badge_request(db, actor, request).await
}

/// [`refuse_badge_request`] with the request already read (tests use it to
/// stand in for a request decided meanwhile)
#[doc(hidden)]
pub async fn refuse_loaded_badge_request(db: &Database, actor: &BadgeActor, request: BadgeChangeRequest) -> Result<BadgeChangeRequest> {
    let action = format!("{}.request.refuse", actor.path.action_prefix());
    let mut record = base_record(actor, action, &request.user_id, None);
    if !badge_request_open(&request, now_ms()) {
        return Err(deny(db, record, &request.badge, actor.path, BadgeDenial::NotPending).await);
    }
    if request.user_id == actor.id {
        return Err(deny(db, record, &request.badge, actor.path, BadgeDenial::SelfChange).await);
    }
    if request.requested_by != actor.id {
        let subject = db.fetch_user(&request.user_id).await?;
        let change = if request.grant { BadgeChange::Grant } else { BadgeChange::Revoke };
        let definition = badge_definition(&request.badge).ok_or_else(|| create_error!(NotFound))?;
        // step-up is not needed to say no
        let mut rights = actor.clone();
        rights.step_up = true;
        if let Err(denial) = badge_rule_check(&rights, definition, &subject, change) {
            return Err(deny(db, record, &request.badge, actor.path, denial).await);
        }
    }
    let mut decided = request.clone();
    decided.status = BadgeRequestStatus::Refused;
    decided.decided_by = Some(actor.id.clone());
    decided.decided_at = Some(now_ms());
    // the atomic decision first: Success only for a refusal that happened
    // (security review F5)
    let summary = format!("{} request {}", known(&request.badge), request.id);
    if !db.decide_badge_request(&decided, BadgeRequestStatus::Pending).await? {
        record.summary = Some(format!("{summary} failed:not_pending"));
        let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
        return Err(create_error!(InvalidOperation));
    }
    record.summary = Some(summary);
    if let Err(error) = record_platform_audit(db, record, AuditOutcome::Success).await {
        // no change without a record: the request waits again
        if !db.decide_badge_request(&request, BadgeRequestStatus::Refused).await.unwrap_or(false) {
            log::error!("badge request {} could not be put back", request.id);
        }
        return Err(error);
    }
    Ok(decided)
}

// ---- appointments: the only source of the Staff and Platform Moderator badges ----

/// Badges that follow an appointment
pub const APPOINTMENT_BADGES: [&str; 2] = ["staff", "platform_moderator"];

/// Id of an appointment record
pub fn appointment_id(user_id: &str, role: &str) -> String {
    format!("{user_id}:{role}")
}

/// Actor id prefix for appointment records made from the host
const APPOINTMENT_ACTION: &str = "staff.cli.appointment";

/// How long the second person must have held an active staff grant
pub const SECOND_PERSON_MIN_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Why the two people behind an appointment change were refused (the code
/// goes to the audit log)
fn operators_refused(
    first: &str,
    second: &str,
    subject: &str,
    first_grant: Option<&StaffGrant>,
    second_grant: Option<&StaffGrant>,
    now: i64,
) -> Option<&'static str> {
    if first == second {
        return Some("same_person");
    }
    if first == subject || second == subject {
        return Some("self");
    }
    if !first_grant.is_some_and(|g| g.is_owner(now)) {
        return Some("first_not_owner");
    }
    let Some(second_grant) = second_grant.filter(|g| g.is_active(now)) else {
        return Some("second_not_staff");
    };
    // the second person is not someone the first one just made staff: an
    // active grant for more than 7 days, not given by the first person.
    // A grant from before these dates were kept has none: not counted.
    match (second_grant.active_since, second_grant.activated_by.as_deref()) {
        (Some(since), Some(by)) if now - since >= SECOND_PERSON_MIN_AGE_MS && by != first => None,
        (Some(_), Some(by)) if by == first => Some("second_made_by_first"),
        _ => Some("second_too_new"),
    }
}

/// The two people behind an appointment change: `first` an active staff
/// Owner, `second` another active staff member whose grant is older than 7
/// days and was not given by `first`. The command line checks both TOTP
/// codes before it calls in here; this checks their grants again.
async fn check_operators(db: &Database, first: &str, second: &str, subject: &str) -> Result<std::result::Result<(), &'static str>> {
    let now = now_ms();
    let first_grant = db.fetch_staff_grant(first).await?;
    let second_grant = db.fetch_staff_grant(second).await?;
    Ok(match operators_refused(first, second, subject, first_grant.as_ref(), second_grant.as_ref(), now) {
        Some(code) => Err(code),
        None => Ok(()),
    })
}

#[allow(clippy::too_many_arguments)]
async fn appointment_record(
    db: &Database,
    first: &str,
    second: &str,
    action: &str,
    user_id: &str,
    reason: &str,
    summary: String,
    outcome: AuditOutcome,
) -> Result<()> {
    record_platform_audit(
        db,
        AuditRecord {
            actor_id: first.to_string(),
            action: format!("{APPOINTMENT_ACTION}.{action}"),
            permission: Some(StaffPermission::StaffManage),
            target_kind: Some("user".to_string()),
            target_id: Ulid::from_string(user_id).ok().map(|_| user_id.to_string()),
            reason: Some(reason.to_string()),
            staff_session_id: None,
            summary: Some(format!("{summary}; second person {second}")),
        },
        outcome,
    )
    .await
    .map(|_| ())
}

/// Appoint someone to the team (`staff`) or to platform moderation
/// (`platform_moderator`, needs the training acknowledged). Gives the
/// badge; an existing grant (from before appointments) is kept as it is.
pub async fn appoint_staff(
    db: &Database,
    first: &str,
    second: &str,
    user_id: &str,
    role: &str,
    reason: &str,
    training_acknowledged: bool,
) -> Result<StaffAppointment> {
    let reason = checked_reason(reason)?;
    let role = APPOINTMENT_BADGES
        .iter()
        .copied()
        .find(|r| *r == role)
        .ok_or_else(|| create_error!(NotFound))?;
    let refuse = |code: &str| format!("{role} denied:{code}");
    if let Err(code) = check_operators(db, first, second, user_id).await? {
        appointment_record(db, first, second, "appoint", user_id, &reason, refuse(code), AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    let subject = match db.fetch_user(user_id).await {
        Ok(user) => user,
        Err(error) if matches!(error.error_type, revolt_result::ErrorType::NotFound) => {
            appointment_record(db, first, second, "appoint", user_id, &reason, refuse("unknown_user"), AuditOutcome::Denied).await?;
            return Err(create_error!(UnknownUser));
        }
        Err(error) => return Err(error),
    };
    if subject.bot.is_some() || subject.flags.unwrap_or_default() & UserFlags::Deleted as i32 != 0 {
        appointment_record(db, first, second, "appoint", user_id, &reason, refuse("not_a_person"), AuditOutcome::Denied).await?;
        return Err(create_error!(IsBot));
    }
    if role == "platform_moderator" && !training_acknowledged {
        appointment_record(db, first, second, "appoint", user_id, &reason, refuse("training"), AuditOutcome::Denied).await?;
        return Err(create_error!(FailedValidation {
            error: "platform moderators acknowledge the moderation rules, conduct and training first".to_string()
        }));
    }
    let id = appointment_id(user_id, role);
    if db.fetch_staff_appointment(&id).await?.is_some_and(|a| a.ended_at.is_none()) {
        appointment_record(db, first, second, "appoint", user_id, &reason, refuse("already"), AuditOutcome::Denied).await?;
        return Err(create_error!(NoEffect));
    }
    let now = now_ms();
    let appointment = StaffAppointment {
        id: id.clone(),
        user_id: user_id.to_string(),
        role: role.to_string(),
        appointed_by: vec![first.to_string(), second.to_string()],
        appointed_at: now,
        reason: reason.clone(),
        training_acknowledged_at: training_acknowledged.then_some(now),
        ended_at: None,
        ended_by: vec![],
        end_reason: None,
    };
    appointment_record(db, first, second, "appoint", user_id, &reason, format!("{role} {id}"), AuditOutcome::Success).await?;
    db.save_staff_appointment(&appointment).await?;
    match grant_badge(
        db,
        user_id,
        role,
        BadgeSource::System,
        Some(format!("appointment:{id}")),
        first,
        Some(reason),
        None,
    )
    .await
    {
        Ok(_) => {}
        // held from before appointments: the record now backs it
        Err(error) if matches!(error.error_type, revolt_result::ErrorType::NoEffect) => {}
        Err(error) => return Err(error),
    }
    Ok(appointment)
}

/// The end of an appointment, once it is decided: the record is closed and
/// the badge taken away (also one held from before appointments). The
/// caller has written the Success record. `ended_by` are the people behind
/// it; `actor` takes the badge away in the badge log.
#[allow(clippy::too_many_arguments)]
async fn close_appointment(
    db: &Database,
    user_id: &str,
    role: &str,
    open: Option<StaffAppointment>,
    holds: bool,
    ended_by: Vec<String>,
    actor: &str,
    reason: &str,
) -> Result<()> {
    if let Some(mut appointment) = open {
        appointment.ended_at = Some(now_ms());
        appointment.ended_by = ended_by;
        appointment.end_reason = Some(reason.to_string());
        db.save_staff_appointment(&appointment).await?;
    }
    if holds {
        match revoke_badge(db, user_id, role, actor, Some(format!("appointment ended: {reason}"))).await {
            Ok(_) => {}
            // taken away in the same moment by someone else
            Err(error) if matches!(error.error_type, revolt_result::ErrorType::NoEffect) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// End an appointment and take the badge away. Also takes away a badge
/// held from before appointments (no record), so a grant without one can
/// be removed the same way.
pub async fn end_staff_appointment(
    db: &Database,
    first: &str,
    second: &str,
    user_id: &str,
    role: &str,
    reason: &str,
) -> Result<()> {
    let reason = checked_reason(reason)?;
    let role = APPOINTMENT_BADGES
        .iter()
        .copied()
        .find(|r| *r == role)
        .ok_or_else(|| create_error!(NotFound))?;
    if let Err(code) = check_operators(db, first, second, user_id).await? {
        appointment_record(db, first, second, "end", user_id, &reason, format!("{role} denied:{code}"), AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    let id = appointment_id(user_id, role);
    let open = db.fetch_staff_appointment(&id).await?.filter(|a| a.ended_at.is_none());
    let holds = held_badges(db, user_id).await?.contains(role);
    if open.is_none() && !holds {
        appointment_record(db, first, second, "end", user_id, &reason, format!("{role} denied:not_held"), AuditOutcome::Denied).await?;
        return Err(create_error!(NoEffect));
    }
    appointment_record(db, first, second, "end", user_id, &reason, format!("{role} {id}"), AuditOutcome::Success).await?;
    close_appointment(db, user_id, role, open, holds, vec![first.to_string(), second.to_string()], first, &reason).await
}

/// Audit action when a removed staff grant ends the Staff appointment
pub const STAFF_GRANT_END_ACTION: &str = "staff.appointment.end_on_revoke";

/// A staff grant was removed (no roles left) or switched off: the Staff
/// badge goes with it, through the same end as `end-appointment`. `actor`
/// is who removed the grant (`system:cli` from the host). Nothing to do
/// when the person neither holds the badge nor has an open appointment.
/// Returns whether the badge or an appointment was ended.
pub async fn end_staff_badge_with_grant(db: &Database, actor: &str, user_id: &str, reason: &str) -> Result<bool> {
    let role = "staff";
    let id = appointment_id(user_id, role);
    let open = db.fetch_staff_appointment(&id).await?.filter(|a| a.ended_at.is_none());
    let holds = held_badges(db, user_id).await?.contains(role);
    if open.is_none() && !holds {
        return Ok(false);
    }
    let reason: String = format!("staff grant removed: {}", reason.trim()).chars().take(500).collect();
    record_platform_audit(
        db,
        AuditRecord {
            actor_id: actor.to_string(),
            action: STAFF_GRANT_END_ACTION.to_string(),
            permission: Some(StaffPermission::StaffManage),
            target_kind: Some("user".to_string()),
            target_id: Ulid::from_string(user_id).ok().map(|_| user_id.to_string()),
            reason: Some(reason.clone()),
            staff_session_id: None,
            summary: Some(format!("{role} {id} ended with the staff grant")),
        },
        AuditOutcome::Success,
    )
    .await?;
    close_appointment(db, user_id, role, open, holds, vec![actor.to_string()], actor, &reason).await?;
    Ok(true)
}

/// Read only: people holding Staff or Platform Moderator without an active
/// appointment (`(badge, user id)`). They keep the badge until the owner
/// confirms them with `appoint` or ends it with `end-appointment`. Reads
/// every holder (no page limit).
pub async fn appointment_gaps(db: &Database) -> Result<Vec<(String, String)>> {
    let appointments = db.fetch_staff_appointments().await?;
    let now = now_ms();
    let mut gaps = vec![];
    for badge in APPOINTMENT_BADGES {
        for grant in db.fetch_all_badge_holders(badge).await? {
            if !grant.held(now) {
                continue;
            }
            let id = appointment_id(&grant.user_id, badge);
            if !appointments.iter().any(|a| a.id == id && a.ended_at.is_none()) {
                gaps.push((badge.to_string(), grant.user_id));
            }
        }
    }
    Ok(gaps)
}

// ---- founder: a host-level setting, noticed in the audit log ----

/// Audit action for a change of `[api.users] founder_id`
pub const FOUNDER_CONFIG_ACTION: &str = "config.founder_id";

/// At start: if `founder_id` differs from the last one the audit log saw,
/// write one record ("founder_id changed"). Returns whether it wrote one.
pub async fn note_founder_config(db: &Database, founder: Option<&str>) -> Result<bool> {
    let last = db
        .fetch_platform_audit(PlatformAuditQuery {
            action_prefix: Some(FOUNDER_CONFIG_ACTION.to_string()),
            limit: 1,
            ..Default::default()
        })
        .await?;
    let current = founder.map(str::to_string);
    match last.first() {
        Some(entry) if entry.target_id == current => return Ok(false),
        None if current.is_none() => return Ok(false),
        _ => {}
    }
    log::warn!("founder_id changed");
    record_platform_audit(
        db,
        AuditRecord {
            actor_id: "system:config".to_string(),
            action: FOUNDER_CONFIG_ACTION.to_string(),
            target_kind: Some("user".to_string()),
            target_id: current,
            summary: Some("founder_id changed".to_string()),
            ..Default::default()
        },
        AuditOutcome::Success,
    )
    .await?;
    Ok(true)
}
