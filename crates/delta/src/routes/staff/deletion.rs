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

use revolt_database::{
    AuditOutcome, AuditRecord, Bot, Database, DeletionInfo, Server, StaffPermission,
    record_platform_audit,
    staff_deletion::{self, DeleteOwnedResult, PlanIds},
    voice::VoiceClient,
};
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
    pub reason: String,
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

/// Who may have their account deleted by staff, and what goes with it:
/// the Owner role first; never yourself, a bot, staff, a privileged or
/// official account, nor an account that owns an official server or bot
/// (the deletion job removes everything the account owns). Every refusal
/// is recorded. Shared by the console route and Zrzur Desk's card.
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
    Ok((user, plan))
}

/// # Schedule an account's deletion
///
/// The same path as a person deleting their own account: the account is
/// disabled now (signed out everywhere) and deleted after the grace
/// period by the scheduled job, with every server and bot it owns. The
/// person is told by message and e-mail, with the reason and the privacy
/// contact; staff can cancel it during the grace period
/// (`.../delete-request/cancel`). The Owner role,
/// `platform.users.delete.request` and a fresh step-up; see
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
    let why = reason(&data.reason)?;
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
    record.reason = Some(why.clone());
    record.summary = Some(format!(
        "scheduled; owns {} servers, {} bots",
        plan.servers.len(),
        plan.bots.len()
    ));
    super::audited::audited(db, record, account.schedule_deletion(db)).await?;
    let (after, date) = match &account.deletion {
        Some(DeletionInfo::Scheduled { after }) => {
            (millis(after), after.to_string().chars().take(10).collect())
        }
        _ => (0, String::new()),
    };
    tell(db, &user, &account.email, Notice::Scheduled { date }, &why).await;
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
/// During the grace period: the deletion is called off and the account
/// enabled again (unless staff banned it). Only a deletion staff asked for;
/// a person's own deletion is theirs. The Owner role,
/// `platform.users.delete.request` and a fresh step-up; the person is told.
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

/// Whether the account's scheduled deletion was asked for by staff: the
/// newest request or cancel record about it is a request
pub(super) async fn requested_by_staff(db: &Database, id: &str) -> Result<bool> {
    let records = db
        .fetch_platform_audit(revolt_database::PlatformAuditQuery {
            target_id: Some(id.to_string()),
            action_prefix: Some("staff.user.delete_request".to_string()),
            limit: 50,
            ..Default::default()
        })
        .await?;
    Ok(records
        .iter()
        .find(|e| (e.action == REQUEST || e.action == CANCEL) && e.outcome == AuditOutcome::Success)
        .is_some_and(|e| e.action == REQUEST))
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
    let why = reason(&data.reason)?;
    let user = db.fetch_user(id).await?;
    let mut account = db.fetch_account(&user.id).await?;
    if !matches!(account.deletion, Some(DeletionInfo::Scheduled { .. })) {
        return Err(create_error!(NoEffect));
    }
    if !requested_by_staff(db, &user.id).await? {
        return Err(refused(
            db,
            record,
            "not a staff request",
            create_error!(InvalidOperation),
        )
        .await?);
    }
    record.reason = Some(why.clone());
    let banned = staff_deletion::is_banned(&user);
    record.summary = Some(
        if banned {
            "cancelled; stays disabled (banned)"
        } else {
            "cancelled"
        }
        .to_string(),
    );
    account.deletion = None;
    if !banned {
        account.disabled = false;
    }
    super::audited::audited(db, record, account.save(db)).await?;
    tell(db, &user, &account.email, Notice::Cancelled, &why).await;
    Ok(ScheduledDeletion {
        user_id: user.id.clone(),
        username: Some(revolt_models::v0::user_handle(
            &user.username,
            &user.discriminator,
        )),
        after: 0,
    })
}

enum Notice {
    /// The day the deletion runs, `YYYY-MM-DD` (UTC)
    Scheduled {
        date: String,
    },
    Cancelled,
}

/// Tell the person, in their messages and by e-mail (they are signed out
/// and cannot read messages until the deletion is cancelled): what
/// happens, why, and where to write. Best effort: the change is saved.
async fn tell(db: &Database, user: &revolt_database::User, email: &str, notice: Notice, why: &str) {
    let contact = revolt_database::ACCOUNT_CONTACT_EMAIL;
    let (title, body): ([&str; 3], [String; 3]) = match notice {
        Notice::Scheduled { date } => (
            [
                "Your account will be deleted",
                "سيُحذف حسابك",
                "Dein Konto wird gelöscht",
            ],
            [
                format!(
                    "The Zrzur team has scheduled the deletion of your account. It is disabled now and will be deleted for good on {date}, with the servers and bots it owns.\n\nReason: {why}\n\nIf you think this is a mistake, write to {contact} before then."
                ),
                format!(
                    "جدول فريق زرزور حذف حسابك. الحساب معطّل الآن وسيُحذف نهائيًا في {date} مع السيرفرات والبوتات التي يملكها.\n\nالسبب: {why}\n\nإن رأيت أن في ذلك خطأ فراسلنا على {contact} قبل هذا الموعد."
                ),
                format!(
                    "Das Zrzur-Team hat die Löschung deines Kontos geplant. Es ist jetzt deaktiviert und wird am {date} endgültig gelöscht, mit den Servern und Bots, die ihm gehören.\n\nGrund: {why}\n\nWenn du das für einen Fehler hältst, schreib vorher an {contact}."
                ),
            ],
        ),
        Notice::Cancelled => (
            [
                "Your account will not be deleted",
                "لن يُحذف حسابك",
                "Dein Konto wird nicht gelöscht",
            ],
            [
                format!(
                    "The Zrzur team has cancelled the deletion of your account.\n\nReason: {why}\n\nQuestions? Write to {contact}."
                ),
                format!(
                    "ألغى فريق زرزور حذف حسابك.\n\nالسبب: {why}\n\nلأي سؤال راسلنا على {contact}."
                ),
                format!(
                    "Das Zrzur-Team hat die Löschung deines Kontos abgesagt.\n\nGrund: {why}\n\nFragen? Schreib an {contact}."
                ),
            ],
        ),
    };
    let languages = ["en", "ar", "de"];
    let titles = languages
        .iter()
        .zip(title)
        .map(|(l, t)| (l.to_string(), t.to_string()))
        .collect();
    let bodies = languages
        .iter()
        .zip(body.clone())
        .map(|(l, b)| (l.to_string(), b))
        .collect();
    crate::util::official::notice(db, &user.id, &titles, &bodies, None).await;
    let locale = user.locale.as_deref().unwrap_or_default().to_lowercase();
    let index = if locale.starts_with("ar") {
        1
    } else if locale.starts_with("de") {
        2
    } else {
        0
    };
    crate::util::official::email_in_language(email, languages[index], title[index], &body[index])
        .await;
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
            Some(DeletionInfo::Scheduled { after }) => Some(ScheduledDeletion {
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
