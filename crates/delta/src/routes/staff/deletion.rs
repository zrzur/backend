//! Deleting servers and bots from the admin console, for accounts that
//! were created to abuse the platform (for example to grab custom links).
//!
//! - `POST /staff/servers/<id>/actions` with `"action": "delete"`
//! - `POST /staff/bots/<id>/delete`
//! - `POST /staff/users/<id>/delete-owned`: every server and bot of an
//!   account that staff already banned, in one step
//!
//! Gate, the strictest one staff actions have (the same as handing a
//! server over): the Owner role, `platform.servers.restrict` and a fresh
//! step-up. Every deletion needs a reason. The deletion, the official
//! refusal and the audit records are `revolt_database::staff_deletion`,
//! shared with `zrzur-staff delete-owned`.

use revolt_database::OfficialAccount;
use revolt_database::{
    AuditOutcome, AuditRecord, Bot, Database, DeletionInfo, Server, StaffPermission,
    record_platform_audit,
    staff_deletion::{self, DeleteOwnedResult, PlanIds},
    voice::VoiceClient,
};
use revolt_models::v0::SafetyCategory;
use revolt_result::{Result, create_error};
use rocket::{State, serde::json::Json};
use serde::{Deserialize, Serialize};

use super::{StaffContext, reason};

pub(super) const PERMISSION: StaffPermission = StaffPermission::ServersRestrict;

/// The API's own command-list cache goes with a deleted bot
fn forget(bot_id: &str) {
    crate::util::public_commands::forget(bot_id);
}

/// The staff member and session, for the audit records
fn actor(staff: &StaffContext) -> AuditRecord {
    staff.record("", Some(PERMISSION))
}

/// The Owner role first, then the permission and a fresh step-up. A staff
/// member without the Owner role gets one clear refusal, written to the
/// audit log on every attempt, whether or not they stepped up.
pub(super) async fn require_delete_gate(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    target_kind: &str,
    target_id: &str,
) -> Result<()> {
    if !staff.grant.is_owner(staff.now) {
        let mut record = staff.record(action, Some(PERMISSION));
        record.target_kind = Some(target_kind.to_string());
        record.target_id = Some(target_id.to_string());
        record.summary = Some("owner role required".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    staff.require_step_up(db, PERMISSION).await
}

/// Delete one server, audited first. An official server is refused.
pub(super) async fn delete_server(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    server: Server,
    why: &str,
) -> Result<()> {
    staff_deletion::staff_delete_server(db, voice_client, &actor(staff), server, why, None).await
}

/// Delete one bot, audited first. An official bot is refused.
pub(super) async fn delete_bot(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    bot: &Bot,
    why: &str,
) -> Result<()> {
    staff_deletion::staff_delete_bot(db, voice_client, &actor(staff), bot, why, None, forget).await
}

#[derive(Deserialize)]
pub struct DataDeleteOwned {
    pub reason: String,
}

/// # Delete every server and bot an account owns
///
/// Only for an account that staff already banned. Official servers and
/// bots are kept and listed under `skipped`. Bots go first, so they leave
/// the servers before those are deleted.
#[post("/users/<id>/delete-owned", data = "<data>")]
pub async fn delete_owned(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    staff: StaffContext,
    id: &str,
    data: Json<DataDeleteOwned>,
) -> Result<Json<DeleteOwnedResult>> {
    delete_owned_core(db, voice_client, &staff, id, data.into_inner(), None)
        .await
        .map(Json)
}

/// The console's `delete_owned`, for any caller holding a
/// [`StaffContext`]. `expected`: the servers and bots the person was shown
/// (Zrzur Desk's card); a plan that changed since is refused, as on the
/// command line.
pub(super) async fn delete_owned_core(
    db: &Database,
    voice_client: &VoiceClient,
    staff: &StaffContext,
    id: &str,
    data: DataDeleteOwned,
    expected: Option<&PlanIds>,
) -> Result<DeleteOwnedResult> {
    require_delete_gate(db, staff, "staff.user.delete_owned", "user", id).await?;
    let why = reason(&data.reason)?;
    let user = db.fetch_user(id).await?;
    staff_deletion::staff_delete_owned(
        db,
        voice_client,
        &actor(staff),
        &user,
        &why,
        forget,
        expected,
    )
    .await
}

// ---- account deletion requests ----

/// What an account deletion request asks for
#[derive(Deserialize)]
pub struct DataDeletionRequest {
    /// The facts, written for the person: at most 300 characters, no
    /// names or details of other people
    pub reason: String,
    /// The ground: the rule broken (required for a request; not used by
    /// a cancel)
    #[serde(default)]
    pub category: Option<SafetyCategory>,
}

/// An account waiting for its deletion
#[derive(Serialize)]
pub struct ScheduledDeletion {
    pub user_id: String,
    pub username: Option<String>,
    /// When the deletion runs, milliseconds since the epoch
    pub after: i64,
}

const REQUEST: &str = "staff.user.delete_request";
const CANCEL: &str = "staff.user.delete_request_cancel";
const NOTICE: &str = "staff.user.delete_request_notice";
/// The facts the person reads: kept short
pub(super) const FACTS_MAX: usize = 300;

/// A refusal written to the audit log, as every refused staff deletion is
async fn refused(
    db: &Database,
    mut record: AuditRecord,
    summary: &str,
    error: revolt_result::Error,
) -> Result<revolt_result::Error> {
    record.summary = Some(summary.to_string());
    record_platform_audit(db, record, AuditOutcome::Denied).await?;
    Ok(error)
}

/// Open safety matters about an account, one line each (case references,
/// appeal ids): an open case about the account or its content, a pending
/// appeal, a legal hold, evidence on hold, and preserved evidence of
/// illegal content, an underage case or a public report. Deleting the
/// account would lose or prejudge them.
pub(super) async fn open_matters(db: &Database, user_id: &str, now: i64) -> Result<Vec<String>> {
    use revolt_models::v0::{SafetyAppealStatus, SafetyStatus, SafetyTargetKind};
    let mut out = Vec::new();
    let cases = db
        .fetch_safety_cases(revolt_database::SafetyCaseQuery {
            search: Some(user_id.to_string()),
            limit: 500,
            ..Default::default()
        })
        .await?;
    for case in cases.iter().filter(|c| {
        (c.target.kind == SafetyTargetKind::User && c.target.id == user_id)
            || c.target_owner_id.as_deref() == Some(user_id)
    }) {
        let open = matches!(
            case.status,
            SafetyStatus::Submitted
                | SafetyStatus::Queued
                | SafetyStatus::UnderReview
                | SafetyStatus::MoreInfoRequired
                | SafetyStatus::Escalated
                | SafetyStatus::Appealed
        );
        if open {
            out.push(format!("case {} open", case.reference));
        }
        if case.legal_hold {
            out.push(format!("case {} legal hold", case.reference));
            continue;
        }
        let evidence = db.fetch_safety_evidence(&case.id).await?;
        if evidence.iter().any(|e| e.legal_hold) {
            out.push(format!("case {} evidence on hold", case.reference));
            continue;
        }
        let preserved = matches!(
            case.category,
            SafetyCategory::IllegalContent | SafetyCategory::Underage
        ) || case.public_reporter.is_some();
        if preserved
            && evidence
                .iter()
                .any(|e| e.expires_at.is_none_or(|t| t > now))
        {
            out.push(format!("case {} preserved evidence", case.reference));
        }
    }
    for appeal in db.fetch_safety_appeals_for_user(user_id).await? {
        if appeal.status == SafetyAppealStatus::Pending {
            out.push(format!("appeal {} pending", appeal.id));
        }
    }
    out.dedup();
    Ok(out)
}

/// Who may have their account deleted by staff, and what goes with it:
/// the Owner role first; never yourself, a bot, staff, a privileged or
/// official account, an account that owns an official server or bot (the
/// deletion job removes everything the account owns), nor one with open
/// safety matters ([`open_matters`]: no override yet). Every refusal is
/// recorded. Shared by the console route and Zrzur Desk's card.
pub(super) async fn delete_request_check(
    db: &Database,
    staff: &StaffContext,
    id: &str,
) -> Result<(revolt_database::User, staff_deletion::DeleteOwnedPlan)> {
    let mut record = staff.record(REQUEST, Some(StaffPermission::UsersDeleteRequest));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    if !staff.grant.is_owner(staff.now) {
        return Err(refused(
            db,
            record,
            "owner role required",
            create_error!(NotPrivileged),
        )
        .await?);
    }
    let user = db.fetch_user(id).await?;
    if user.id == staff.user.id {
        return Err(refused(db, record, "yourself", create_error!(CannotReportYourself)).await?);
    }
    if user.bot.is_some() {
        return Err(refused(db, record, "bot", create_error!(IsBot)).await?);
    }
    let staff_member = db
        .fetch_staff_grant(&user.id)
        .await?
        .is_some_and(|grant| grant.is_active(staff.now));
    if user.privileged || user.official || staff_member {
        return Err(refused(
            db,
            record,
            "protected account",
            create_error!(NotPrivileged),
        )
        .await?);
    }
    let plan = staff_deletion::plan_delete_owned(db, &user).await?;
    if !plan.skipped.is_empty() {
        return Err(refused(
            db,
            record,
            "owns official servers or bots",
            create_error!(NotPrivileged),
        )
        .await?);
    }
    let matters = open_matters(db, &user.id, staff.now).await?;
    if !matters.is_empty() {
        // the record names how many, the refusal names which
        let summary = format!("open safety matters: {}", matters.len());
        let error = create_error!(FailedValidation {
            error: format!("open safety matters: {}", matters.join("; "))
        });
        return Err(refused(db, record, &summary, error).await?);
    }
    Ok((user, plan))
}

/// Whether the person can be told at all: by e-mail (mail switched on and
/// an address that can take it) or by an official message
async fn reachable(db: &Database, email: &str) -> (bool, bool) {
    let smtp = revolt_config::config().await.api.smtp;
    let mail = !smtp.host.is_empty() && revolt_database::util::email::validate_email(email).is_ok();
    let message = revolt_database::official_account(db, OfficialAccount::Safety)
        .await
        .is_ok();
    (mail, message)
}

/// # Schedule an account's deletion
///
/// The same path as a person deleting their own account: the account is
/// disabled now (signed out everywhere) and deleted after the grace
/// period by the scheduled job, with every server and bot it owns. The
/// person gets a statement of reasons by official message and e-mail:
/// that a person decided, the ground (`category`) and the facts
/// (`reason`, 300 characters at most), the date, how to object or appeal
/// (the deletion is paused: an Owner cancels it, `.../cancel`), that they
/// can ask for a copy of their data, and the out-of-court line. Refused
/// when the person cannot be told; undone when no notice arrived. The
/// Owner role, `platform.users.delete.request` and a fresh step-up; see
/// [`delete_request_check`] for who is refused.
#[post("/users/<id>/delete-request", data = "<data>")]
pub async fn delete_request(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataDeletionRequest>,
) -> Result<Json<ScheduledDeletion>> {
    delete_request_core(db, &staff, id, data.into_inner(), None)
        .await
        .map(Json)
}

/// The facts the person reads: the reason rules, and 300 characters at most
pub(super) fn facts(text: &str) -> Result<String> {
    let why = reason(text)?;
    if why.chars().count() > FACTS_MAX {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length("reason", 3, FACTS_MAX),
        ]));
    }
    Ok(why)
}

/// See [`delete_request`]; shared with Zrzur Desk. `expected`: the servers
/// and bots the person was shown (the desk's card); a change since is
/// refused.
pub(super) async fn delete_request_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataDeletionRequest,
    expected: Option<&PlanIds>,
) -> Result<ScheduledDeletion> {
    let permission = StaffPermission::UsersDeleteRequest;
    let (user, plan) = delete_request_check(db, staff, id).await?;
    staff.require_step_up(db, permission).await?;
    let why = facts(&data.reason)?;
    let category = data.category.ok_or_else(|| {
        revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new(
            "category", "required",
        )])
    })?;
    if expected.is_some_and(|ids| *ids != plan.ids()) {
        return Err(create_error!(InvalidOperation));
    }
    let mut account = db.fetch_account(&user.id).await?;
    if matches!(
        account.deletion,
        Some(DeletionInfo::Scheduled { .. } | DeletionInfo::Deleted)
    ) {
        return Err(create_error!(NoEffect));
    }
    let mut record = staff.record(REQUEST, Some(permission));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(user.id.clone());
    // no way to tell the person: no deletion
    let (mail, message) = reachable(db, &account.email).await;
    if !mail && !message {
        return Err(refused(
            db,
            record,
            "notice cannot be delivered",
            create_error!(FailedValidation {
                error: "the person cannot be told: no e-mail delivery and no official message"
                    .to_string()
            }),
        )
        .await?);
    }
    let disabled_before = account.disabled;
    record.reason = Some(why.clone());
    record.summary = Some(format!(
        "scheduled; category={category:?}; owns {} servers, {} bots",
        plan.servers.len(),
        plan.bots.len()
    ));
    super::audited::audited(
        db,
        record,
        account.schedule_deletion_for(db, Some(staff.user.id.clone())),
    )
    .await?;
    let (after, date) = match &account.deletion {
        Some(DeletionInfo::Scheduled { after, .. }) => {
            (millis(after), after.to_string().chars().take(10).collect())
        }
        _ => (0, String::new()),
    };
    let delivery = tell(
        db,
        &user,
        &account.email,
        Notice::Scheduled {
            date,
            category,
            facts: why,
        },
        staff.now,
    )
    .await;
    // the delivery, by category only (no free text)
    let mut sent = staff.record(NOTICE, Some(permission));
    sent.target_kind = Some("user".to_string());
    sent.target_id = Some(user.id.clone());
    sent.summary = Some(format!("category={category:?}; {}", delivery.summary()));
    if !delivery.arrived() {
        // nobody was told: the request is undone
        account.deletion = None;
        account.disabled = disabled_before;
        account.save(db).await?;
        record_platform_audit(db, sent, AuditOutcome::Failed).await?;
        return Err(create_error!(FailedValidation {
            error: "the person could not be told: the request was undone".to_string()
        }));
    }
    record_platform_audit(db, sent, AuditOutcome::Success).await?;
    Ok(ScheduledDeletion {
        user_id: user.id,
        username: Some(revolt_models::v0::user_handle(
            &user.username,
            &user.discriminator,
        )),
        after,
    })
}

/// # Cancel a staff deletion request
///
/// During the grace period, for example on an appeal: the deletion is
/// called off and the account goes back to exactly the state it had before
/// the request: enabled only if it was enabled then and is not suspended,
/// banned or marked as spam now. Only a deletion staff asked for; a
/// person's own deletion is theirs. The Owner role,
/// `platform.users.delete.request` and a fresh step-up; the person is told
/// with a fixed text (the reason stays in the audit log).
#[post("/users/<id>/delete-request/cancel", data = "<data>")]
pub async fn cancel_delete_request(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataDeletionRequest>,
) -> Result<Json<ScheduledDeletion>> {
    cancel_delete_request_core(db, &staff, id, data.into_inner())
        .await
        .map(Json)
}

/// What cancelling the account's scheduled deletion would leave: `None`
/// if there is no deletion staff asked for (a person's own deletion is
/// theirs), else whether the account stays disabled: it was disabled
/// before the request, or it is suspended, banned or marked as spam now
pub(super) fn cancel_outcome(
    account: &revolt_database::Account,
    user: &revolt_database::User,
) -> Option<bool> {
    match &account.deletion {
        Some(DeletionInfo::Scheduled {
            requested_by: Some(_),
            disabled_before,
            ..
        }) => {
            use revolt_models::v0::UserFlags;
            let held = UserFlags::SuspendedUntil as i32
                | UserFlags::Banned as i32
                | UserFlags::Spam as i32;
            Some(*disabled_before || user.flags.unwrap_or_default() & held != 0)
        }
        _ => None,
    }
}

/// See [`cancel_delete_request`]; shared with Zrzur Desk
pub(super) async fn cancel_delete_request_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataDeletionRequest,
) -> Result<ScheduledDeletion> {
    let permission = StaffPermission::UsersDeleteRequest;
    let mut record = staff.record(CANCEL, Some(permission));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    if !staff.grant.is_owner(staff.now) {
        return Err(refused(
            db,
            record,
            "owner role required",
            create_error!(NotPrivileged),
        )
        .await?);
    }
    staff.require_step_up(db, permission).await?;
    let why = facts(&data.reason)?;
    let user = db.fetch_user(id).await?;
    let mut account = db.fetch_account(&user.id).await?;
    if !matches!(account.deletion, Some(DeletionInfo::Scheduled { .. })) {
        return Err(create_error!(NoEffect));
    }
    let Some(stays_disabled) = cancel_outcome(&account, &user) else {
        return Err(refused(
            db,
            record,
            "not a staff request",
            create_error!(InvalidOperation),
        )
        .await?);
    };
    record.reason = Some(why);
    record.summary = Some(
        if stays_disabled {
            "cancelled; stays disabled as before"
        } else {
            "cancelled; enabled again"
        }
        .to_string(),
    );
    account.deletion = None;
    account.disabled = stays_disabled;
    super::audited::audited(db, record, account.save(db)).await?;
    let delivery = tell(
        db,
        &user,
        &account.email,
        Notice::Cancelled { stays_disabled },
        staff.now,
    )
    .await;
    let mut sent = staff.record(NOTICE, Some(permission));
    sent.target_kind = Some("user".to_string());
    sent.target_id = Some(user.id.clone());
    sent.summary = Some(format!("cancelled; {}", delivery.summary()));
    record_platform_audit(
        db,
        sent,
        if delivery.arrived() {
            AuditOutcome::Success
        } else {
            AuditOutcome::Failed
        },
    )
    .await?;
    Ok(ScheduledDeletion {
        user_id: user.id.clone(),
        username: Some(revolt_models::v0::user_handle(
            &user.username,
            &user.discriminator,
        )),
        after: 0,
    })
}

pub(super) enum Notice {
    /// The statement of reasons: the day the deletion runs (`YYYY-MM-DD`,
    /// UTC), the ground and the facts staff wrote
    Scheduled {
        date: String,
        category: SafetyCategory,
        facts: String,
    },
    /// A fixed text: no words of staff or of other people
    Cancelled { stays_disabled: bool },
}

/// How the notice went: the official message, and the e-mail (`None`: mail
/// is off or the address cannot take it)
struct Delivery {
    message: bool,
    email: Option<bool>,
}

impl Delivery {
    fn arrived(&self) -> bool {
        self.message || self.email == Some(true)
    }

    /// For the audit summary: delivery states only
    fn summary(&self) -> String {
        let email = match self.email {
            Some(true) => "sent",
            Some(false) => "failed",
            None => "off",
        };
        let message = if self.message { "sent" } else { "failed" };
        format!("message={message}; email={email}")
    }
}

/// The person's language: Arabic, German, else English
fn language_of(user: &revolt_database::User) -> &'static str {
    let locale = user.locale.as_deref().unwrap_or_default().to_lowercase();
    if locale.starts_with("ar") {
        "ar"
    } else if locale.starts_with("de") {
        "de"
    } else {
        "en"
    }
}

/// The fixed texts of a notice in `language` (MSA Arabic): title and body
pub(super) fn notice_text(language: &str, notice: &Notice, today: &str) -> (String, String) {
    let contact = revolt_database::ACCOUNT_CONTACT_EMAIL;
    match notice {
        Notice::Scheduled {
            date,
            category,
            facts,
        } => {
            let ground = crate::util::official::rule_name(language, *category);
            let out_of_court = crate::util::official::out_of_court_lines(language);
            match language {
                "ar" => (
                    "سيُحذف حسابك".to_string(),
                    format!(
                        "في {today} قرّر أحد أعضاء فريق زرزور حذف حسابك. اتخذ هذا القرار إنسان، لا نظام آلي.\n\nالأساس: {ground} (شروط الخدمة وإرشادات المجتمع).\nما وجدناه: {facts}\n\nحسابك معطّل الآن وقد سُجّل خروجك. وفي {date} يُحذف نهائيًا مع السيرفرات والبوتات التي يملكها.\n\nللاعتراض أو الطعن راسل {contact} قبل {date}. ويتوقف الحذف ريثما يُبت في طعنك، ويبت فيه شخص لم يتخذ هذا القرار.\n\nويمكنك قبل {date} أن تطلب من {contact} نسخة من بياناتك.\n\n{out_of_court}\n\nويمكنك أيضًا تقديم شكوى إلى سلطة حماية البيانات."
                    ),
                ),
                "de" => (
                    "Dein Konto wird gelöscht".to_string(),
                    format!(
                        "Am {today} hat ein Mitglied des Zrzur-Teams entschieden, dein Konto zu löschen. Ein Mensch hat das entschieden, kein automatisches System.\n\nGrundlage: {ground} (unsere Nutzungsbedingungen und Community-Richtlinien).\nWas wir festgestellt haben: {facts}\n\nDein Konto ist jetzt deaktiviert und du bist abgemeldet. Am {date} wird es endgültig gelöscht, zusammen mit den Servern und Bots, die ihm gehören.\n\nUm zu widersprechen oder Einspruch einzulegen, schreib vor dem {date} an {contact}. Solange über deinen Einspruch entschieden wird, ist die Löschung angehalten; entscheiden wird eine Person, die diese Entscheidung nicht getroffen hat.\n\nVor dem {date} kannst du bei {contact} auch eine Kopie deiner Daten anfordern.\n\n{out_of_court}\n\nDu kannst dich außerdem bei der Datenschutzbehörde beschweren."
                    ),
                ),
                _ => (
                    "Your account will be deleted".to_string(),
                    format!(
                        "On {today}, a member of the Zrzur team decided to delete your account. A person took this decision, not an automated system.\n\nGround: {ground} (our Terms of Service and Community Guidelines).\nWhat we found: {facts}\n\nYour account is disabled now and you are signed out. On {date} it will be deleted for good, together with the servers and bots it owns.\n\nTo object or appeal, write to {contact} before {date}. While your appeal is decided, the deletion is paused; a person who did not take this decision decides it.\n\nBefore {date} you can also ask {contact} for a copy of your data.\n\n{out_of_court}\n\nYou can also complain to the data protection authority."
                    ),
                ),
            }
        }
        Notice::Cancelled { stays_disabled } => {
            let state = match (language, stays_disabled) {
                ("ar", false) => "يمكنك تسجيل الدخول من جديد.",
                ("ar", true) => "ويبقى حسابك معطّلًا كما كان قبل ذلك.",
                ("de", false) => "Du kannst dich wieder anmelden.",
                ("de", true) => "Dein Konto bleibt deaktiviert, wie es vorher war.",
                (_, false) => "You can sign in again.",
                (_, true) => "Your account stays disabled, as it was before.",
            };
            match language {
                "ar" => (
                    "لن يُحذف حسابك".to_string(),
                    format!(
                        "في {today} ألغى أحد أعضاء فريق زرزور حذف حسابك. {state}\n\nلأي سؤال راسلنا على {contact}."
                    ),
                ),
                "de" => (
                    "Dein Konto wird nicht gelöscht".to_string(),
                    format!(
                        "Am {today} hat ein Mitglied des Zrzur-Teams die Löschung deines Kontos abgesagt. {state}\n\nFragen? Schreib an {contact}."
                    ),
                ),
                _ => (
                    "Your account will not be deleted".to_string(),
                    format!(
                        "On {today}, a member of the Zrzur team cancelled the deletion of your account. {state}\n\nQuestions? Write to {contact}."
                    ),
                ),
            }
        }
    }
}

/// Tell the person, in their language, by official message and by e-mail
/// (they are signed out and may never open the app again); gives what
/// arrived
async fn tell(
    db: &Database,
    user: &revolt_database::User,
    email: &str,
    notice: Notice,
    now: i64,
) -> Delivery {
    let language = language_of(user);
    let today = day(now);
    let (title, body) = notice_text(language, &notice, &today);
    let message = revolt_database::send_official_message(
        db,
        None,
        OfficialAccount::Safety,
        &user.id,
        format!("**{title}**\n\n{body}"),
    )
    .await
    .is_ok();
    let (mail, _) = reachable(db, email).await;
    let email = if mail {
        Some(send_mail(email, language, &title, &body).await)
    } else {
        None
    };
    Delivery { message, email }
}

/// A day as `YYYY-MM-DD` (UTC)
fn day(ms: i64) -> String {
    (iso8601_timestamp::Timestamp::UNIX_EPOCH + iso8601_timestamp::Duration::milliseconds(ms))
        .to_string()
        .chars()
        .take(10)
        .collect()
}

/// The e-mail, sent now; gives whether the mail server took it
async fn send_mail(to: &str, language: &str, subject: &str, body: &str) -> bool {
    use revolt_database::util::email::{email_templates, send_email};
    let smtp = revolt_config::config().await.api.smtp;
    let mut template = email_templates().await.statement;
    template.title = subject
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let variables = serde_json::json!({
        "body": body.replace("**", ""),
        "dir": if language == "ar" { "rtl" } else { "ltr" },
    });
    let to = to.to_string();
    matches!(
        tokio::task::spawn_blocking(move || send_email(&smtp, to, &template, variables)).await,
        Ok(Ok(()))
    )
}

fn millis(at: &iso8601_timestamp::Timestamp) -> i64 {
    at.duration_since(iso8601_timestamp::Timestamp::UNIX_EPOCH)
        .whole_milliseconds() as i64
}

/// # Accounts waiting for their deletion
///
/// Read only: who, and when the deletion runs. Newest first, 100 at most.
/// Needs `platform.users.pii.read`.
#[get("/deletions")]
pub async fn deletions(
    db: &State<Database>,
    staff: StaffContext,
) -> Result<Json<Vec<ScheduledDeletion>>> {
    deletions_core(db, &staff).await.map(Json)
}

/// See [`deletions`]; shared with Zrzur Desk
pub(super) async fn deletions_core(
    db: &Database,
    staff: &StaffContext,
) -> Result<Vec<ScheduledDeletion>> {
    // who is leaving, and when: personal data (an Owner holds every
    // permission)
    staff.require(db, StaffPermission::UsersPiiRead).await?;
    let accounts = db.fetch_accounts_scheduled_for_deletion(100).await?;
    let ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
    let users = db.fetch_users(&ids).await.unwrap_or_default();
    let mut out: Vec<ScheduledDeletion> = accounts
        .iter()
        .filter_map(|account| match &account.deletion {
            Some(DeletionInfo::Scheduled { after, .. }) => Some(ScheduledDeletion {
                user_id: account.id.clone(),
                username: users
                    .iter()
                    .find(|u| u.id == account.id)
                    .map(|u| revolt_models::v0::user_handle(&u.username, &u.discriminator)),
                after: millis(after),
            }),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| b.after.cmp(&a.after));
    Ok(out)
}
