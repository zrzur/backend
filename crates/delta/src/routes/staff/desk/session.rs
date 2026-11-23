//! Desk sessions: sign in with a TOTP code from a bot form, see who you
//! are, step up, sign out. The console's limits, lockout and audit action
//! names apply unchanged; every record's `summary` starts with `desk`.

use revolt_database::{
    new_staff_token, record_platform_audit, staff_token_hash, AuditOutcome, Database,
    StaffPermission, StaffRole, StaffSession, StaffSessionKind, User, STAFF_SESSION_IDLE_MS,
    STAFF_SESSION_MAX_MS, STAFF_STEP_UP_MS,
};
use revolt_result::Result;
use rocket::{serde::json::Json, State};
use serde::Serialize;
use ulid::Ulid;

use super::super::{check_code, StepUp};
use super::actor::{context, desk_actor, field, press, DataDeskInteraction, Expect};

/// The custom id of the sign-in form
pub const LOGIN_MODAL: &str = "zd:login";
/// The custom id of the step-up form
pub const STEP_UP_MODAL: &str = "zd:stepup";
/// The form field that carries the TOTP code
pub const CODE_FIELD: &str = "code";

/// Stand-in for the ordinary session a console session is bound to: a desk
/// session has none, and no real session id ever equals this
const DESK_SESSION_ID: &str = "desk";

/// Areas the desk knows, and the permission each one needs
const AREAS: &[(&str, StaffPermission)] = &[("overview", StaffPermission::DashboardRead)];

#[derive(Serialize, Debug)]
pub struct DeskOpened {
    pub expires_at: i64,
}

/// Who the presser is on the desk, for the bot's private answer. The bot
/// shows it; every route still checks every press.
#[derive(Serialize, Debug)]
pub struct DeskMe {
    pub user_id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub roles: Vec<StaffRole>,
    pub permissions: Vec<StaffPermission>,
    /// Hard end of the desk session
    pub expires_at: i64,
    pub idle_timeout_ms: i64,
    pub step_up_until: Option<i64>,
    /// Set while a break-glass grant is active
    pub access_expires_at: Option<i64>,
    /// The enabled desk areas this person may use, in settings order
    pub areas: Vec<String>,
}

/// # Desk: sign in
///
/// From the bot's sign-in form (`zd:login`, field `code`). A current TOTP
/// code opens a desk session and ends any earlier one of this person. The
/// session's token is never handed out: a desk session is found by person
/// and bot, and only through a fresh press.
#[post("/desk/session", data = "<data>")]
pub async fn open(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskInteraction>,
) -> Result<Json<DeskOpened>> {
    let press = press(db, &user, &data, Expect::Modal(LOGIN_MODAL)).await?;
    let base = press.record("staff.session.create", "desk sign-in");
    if let Err(error) = check_code(db, &press.grant, press.field(CODE_FIELD)).await {
        let mut record = base;
        record.summary = Some("desk: second factor missing or wrong".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(error);
    }

    let now = press.now;
    // one desk session per person
    db.revoke_desk_sessions_for_user(&press.user.id, now).await?;
    let staff = StaffSession {
        id: Ulid::new().to_string(),
        // a random value nobody is given: no token opens this session
        token_hash: staff_token_hash(&new_staff_token()),
        user_id: press.user.id.clone(),
        session_id: DESK_SESSION_ID.to_string(),
        created_at: now,
        last_seen_at: now,
        expires_at: now + STAFF_SESSION_MAX_MS,
        step_up_until: None,
        revoked_at: None,
        user_agent: None,
        kind: StaffSessionKind::Desk,
        bot_id: Some(press.settings.bot.clone()),
    };
    db.insert_staff_session(&staff).await?;
    let mut record = base;
    record.staff_session_id = Some(staff.id.clone());
    if let Err(error) = record_platform_audit(db, record, AuditOutcome::Success).await {
        // no audit record, no session
        db.revoke_staff_session(&staff.id, now).await?;
        return Err(error);
    }
    Ok(Json(DeskOpened {
        expires_at: staff.expires_at,
    }))
}

/// # Desk: sign out
///
/// From `/desk logout`. Ends the presser's desk session.
#[post("/desk/session/close", data = "<data>")]
pub async fn close(db: &State<Database>, user: User, data: Json<DataDeskInteraction>) -> Result<()> {
    let (staff, _) = desk_actor(db, &user, &data, Expect::Command("logout")).await?;
    db.revoke_staff_session(&staff.session.id, staff.now).await?;
    let mut record = staff.record("staff.session.close", None);
    record.summary = Some("desk sign-out".to_string());
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    Ok(())
}

/// # Desk: step up
///
/// From the bot's step-up form (`zd:stepup`, field `code`). A fresh TOTP
/// code unlocks high-risk actions on this desk session for five minutes.
#[post("/desk/step-up", data = "<data>")]
pub async fn step_up(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskInteraction>,
) -> Result<Json<StepUp>> {
    let (staff, interaction) = desk_actor(db, &user, &data, Expect::Modal(STEP_UP_MODAL)).await?;
    if let Err(error) = check_code(db, &staff.grant, field(&interaction, CODE_FIELD)).await {
        let mut record = staff.record("staff.session.step_up", None);
        record.summary = Some("desk: second factor wrong".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(error);
    }
    let until = (staff.now + STAFF_STEP_UP_MS).min(staff.session.expires_at);
    let mut record = staff.record("staff.session.step_up", None);
    record.summary = Some("desk step-up".to_string());
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    db.set_staff_step_up(&staff.session.id, until).await?;
    Ok(Json(StepUp { step_up_until: until }))
}

/// # Desk: who am I
///
/// From `/desk me`. Roles, permissions, session times and the desk areas
/// this person may use.
#[post("/desk/me", data = "<data>")]
pub async fn me(db: &State<Database>, user: User, data: Json<DataDeskInteraction>) -> Result<Json<DeskMe>> {
    let press = press(db, &user, &data, Expect::Command("me")).await?;
    let enabled = press.settings.areas.clone();
    let (staff, _) = context(db, press).await?;
    let mut areas: Vec<String> = Vec::new();
    for area in enabled {
        let allowed = AREAS
            .iter()
            .any(|(name, permission)| *name == area && staff.has(*permission));
        if allowed && !areas.contains(&area) {
            areas.push(area);
        }
    }
    let mut record = staff.record("staff.desk.me", None);
    record.summary = Some("desk me".to_string());
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    Ok(Json(DeskMe {
        user_id: staff.user.id.clone(),
        username: staff.user.username.clone(),
        display_name: staff.user.display_name.clone(),
        roles: staff.grant.roles.clone(),
        permissions: staff.permissions.iter().copied().collect(),
        expires_at: staff.session.expires_at,
        idle_timeout_ms: STAFF_SESSION_IDLE_MS,
        step_up_until: staff.session.step_up_until.filter(|u| *u > staff.now),
        access_expires_at: staff.grant.expires_at,
        areas,
    }))
}
