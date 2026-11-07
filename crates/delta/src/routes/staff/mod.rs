//! Platform staff API for the admin console, mounted at `/staff`.
//!
//! Separate from server permissions and from `User::privileged`: only a
//! staff grant (`platform_staff`, created from the command line or by a
//! staff manager) and a staff session opened with a TOTP code give access.
//! Every change and every refused attempt is written to the platform audit
//! log. Not part of the public OpenAPI document or the SDK.

mod guard;
mod badges;
mod change_requests;
mod beta;
mod billing;
mod content;
mod creators;
mod bots;
mod discover;
mod metrics;
mod official;
mod platform;
mod policies;
mod review;
mod safety;
mod security;
mod servers;
mod support;
mod words;
#[cfg(test)]
mod test;

pub use guard::StaffContext;
pub use metrics::Metrics;

use revolt_database::{
    check_staff_change, new_staff_token, record_platform_audit, staff_now, staff_token_hash,
    verify_audit_chain, verify_staff_totp, AuditOutcome, AuditRecord, Database, PlatformAuditEntry,
    PlatformAuditQuery, Session, StaffGrant, StaffPermission, StaffRole, StaffSession, Totp, User,
    STAFF_SESSION_IDLE_MS, STAFF_SESSION_MAX_MS, STAFF_STEP_UP_MS,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, Request, Route, State};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

pub fn routes() -> Vec<Route> {
    platform::mark_started();
    routes![
        words::list,
        words::add,
        words::remove,
        words::check,
        platform::stats,
        platform::daily,
        platform::users,
        platform::user,
        platform::revoke_user_sessions,
        platform::servers,
        platform::server,
        platform::health,
        safety::cases,
        safety::case,
        safety::set_status,
        safety::edit,
        safety::note,
        safety::take,
        safety::revoke,
        safety::appeals,
        safety::decide,
        safety::user_history,
        safety::user_action,
        discover::requests,
        discover::decide,
        badges::badge_catalogue,
        badges::user_badges,
        badges::grant,
        badges::revoke,
        bots::list,
        bots::detail,
        bots::disable,
        bots::verifications,
        bots::decide_verification,
        bots::unverify,
        change_requests::list,
        change_requests::create,
        change_requests::cancel,
        change_requests::security,
        content::plugins,
        content::plugin,
        content::designs,
        creators::list,
        creators::history,
        creators::change,
        official::overview,
        official::propose,
        official::confirm,
        official::refuse,
        official::name_conflicts,
        review::design_queue,
        review::design_decide,
        review::design_file,
        review::plugin_queue,
        review::plugin_decide,
        review::bugs,
        review::bug,
        review::bug_review,
        policies::list,
        policies::publish,
        policies::withdraw,
        support::tickets,
        support::ticket,
        support::reply,
        support::change,
        support::notices,
        support::send,
        support::withdraw,
        servers::act,
        security::overview,
        security::lock,
        metrics::metrics,
        billing::summary,
        billing::orders,
        billing::order,
        billing::audit,
        billing::webhooks,
        billing::issues,
        billing::refund,
        billing::deliver,
        billing::retry_webhook,
        billing::reconcile_now,
        billing::grant,
        beta::features,
        beta::save,
        beta::members,
        beta::member,
        open_session,
        close_session,
        step_up,
        me,
        roles,
        members,
        set_member,
        disable_member,
        revoke_member_sessions,
        audit,
        verify_audit
    ]
}

// ---- request and response bodies ----

#[derive(Deserialize)]
pub struct DataTotp {
    pub totp_code: String,
}

#[derive(Deserialize)]
pub struct DataReason {
    pub reason: String,
}

#[derive(Deserialize)]
pub struct DataSetMember {
    pub roles: Vec<String>,
    pub reason: String,
    /// The grant version you looked at (`None` if the user had none)
    pub version: Option<i64>,
}

#[derive(Serialize)]
pub struct OpenedSession {
    /// Keep in memory only; send as `x-staff-session`
    pub token: String,
    pub expires_at: i64,
    pub idle_timeout_ms: i64,
}

#[derive(Serialize)]
pub struct StepUp {
    pub step_up_until: i64,
}

#[derive(Serialize)]
pub struct StaffMe {
    pub user_id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub roles: Vec<StaffRole>,
    pub permissions: Vec<StaffPermission>,
    pub session_expires_at: i64,
    pub idle_timeout_ms: i64,
    pub step_up_until: Option<i64>,
    /// Set while a break-glass grant is active
    pub access_expires_at: Option<i64>,
}

#[derive(Serialize)]
pub struct RoleDefinition {
    pub id: StaffRole,
    pub permissions: Vec<StaffPermission>,
}

#[derive(Serialize)]
pub struct StaffMember {
    pub user_id: String,
    pub username: Option<String>,
    pub roles: Vec<StaffRole>,
    pub disabled: bool,
    pub active: bool,
    pub expires_at: Option<i64>,
    pub open_sessions: usize,
    pub version: i64,
    pub updated_at: i64,
    pub updated_by: String,
}

#[derive(Serialize)]
pub struct AuditCheck {
    pub checked: usize,
    pub from_seq: i64,
    pub ok: bool,
    pub first_bad_seq: Option<i64>,
}

// ---- helpers ----

pub(super) fn reason(text: &str) -> Result<String> {
    let text = text.trim();
    if text.chars().count() < 3 || text.chars().count() > 500 {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length("reason", 3, 500),
        ]));
    }
    Ok(text.to_string())
}

fn now_secs() -> u64 {
    (staff_now() / 1000).max(0) as u64
}

/// The TOTP secret of an account, if TOTP is switched on
async fn totp_secret(db: &Database, user_id: &str) -> Result<String> {
    let account = db.fetch_account(user_id).await?;
    match account.mfa.totp_token {
        Totp::Enabled { secret } => Ok(secret),
        _ => Err(create_error!(DisallowedMFAMethod)),
    }
}

/// Check a TOTP code for a staff member and use it up
async fn check_code(db: &Database, grant: &StaffGrant, code: &str) -> Result<()> {
    let secret = totp_secret(db, &grant.id).await?;
    let step = verify_staff_totp(&secret, code, now_secs(), grant.last_totp_step)
        .ok_or_else(|| create_error!(InvalidCredentials))?;
    if db.claim_staff_totp_step(&grant.id, step).await? {
        Ok(())
    } else {
        Err(create_error!(InvalidCredentials))
    }
}

/// The request's `User-Agent` (shortened), and a check that a browser
/// request comes from a configured admin origin
struct ConsoleClient {
    user_agent: Option<String>,
}

#[rocket::async_trait]
impl<'r> rocket::request::FromRequest<'r> for ConsoleClient {
    type Error = revolt_result::Error;

    async fn from_request(request: &'r Request<'_>) -> rocket::request::Outcome<Self, Self::Error> {
        if !guard::origin_allowed(request).await {
            return rocket::request::Outcome::Error((
                rocket::http::Status::Unauthorized,
                create_error!(InvalidSession),
            ));
        }
        rocket::request::Outcome::Success(ConsoleClient {
            user_agent: request
                .headers()
                .get_one("user-agent")
                .map(|a| a.chars().take(200).collect()),
        })
    }
}

async fn fetch_grant(db: &Database, user_id: &str) -> Result<Option<StaffGrant>> {
    db.fetch_staff_grant(user_id).await
}

// ---- sessions ----

/// # Open a staff session
///
/// Needs an ordinary session, an active staff grant, TOTP switched on and
/// a current TOTP code. The token is returned once; only its hash is kept.
#[post("/session", data = "<data>")]
async fn open_session(
    db: &State<Database>,
    user: User,
    session: Session,
    client: ConsoleClient,
    data: Json<DataTotp>,
) -> Result<Json<OpenedSession>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let now = staff_now();
    let grant = match fetch_grant(db, &user.id).await? {
        Some(grant) if grant.is_active(now) => grant,
        // ordinary users: no record, to keep the log meaningful
        _ => return Err(create_error!(NotPrivileged)),
    };
    let base = AuditRecord {
        actor_id: user.id.clone(),
        action: "staff.session.create".to_string(),
        target_kind: Some("user".to_string()),
        target_id: Some(user.id.clone()),
        ..Default::default()
    };
    if let Err(error) = check_code(db, &grant, &data.totp_code).await {
        let mut record = base.clone();
        record.summary = Some("second factor missing or wrong".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(error);
    }

    let token = new_staff_token();
    let staff = StaffSession {
        id: Ulid::new().to_string(),
        token_hash: staff_token_hash(&token),
        user_id: user.id.clone(),
        session_id: session.id.clone(),
        created_at: now,
        last_seen_at: now,
        expires_at: now + STAFF_SESSION_MAX_MS,
        step_up_until: None,
        revoked_at: None,
        user_agent: client.user_agent,
    };
    db.insert_staff_session(&staff).await?;
    let mut record = base;
    record.staff_session_id = Some(staff.id.clone());
    if let Err(error) = record_platform_audit(db, record, AuditOutcome::Success).await {
        // no audit record, no session
        db.revoke_staff_session(&staff.id, now).await?;
        return Err(error);
    }
    Ok(Json(OpenedSession {
        token,
        expires_at: staff.expires_at,
        idle_timeout_ms: STAFF_SESSION_IDLE_MS,
    }))
}

/// # Close the staff session (sign out of the console)
#[delete("/session")]
async fn close_session(db: &State<Database>, staff: StaffContext) -> Result<()> {
    db.revoke_staff_session(&staff.session.id, staff.now).await?;
    record_platform_audit(db, staff.record("staff.session.close", None), AuditOutcome::Success)
        .await?;
    Ok(())
}

/// # Step up
///
/// A fresh TOTP code unlocks high-risk actions for five minutes.
#[post("/session/step-up", data = "<data>")]
async fn step_up(
    db: &State<Database>,
    staff: StaffContext,
    data: Json<DataTotp>,
) -> Result<Json<StepUp>> {
    if let Err(error) = check_code(db, &staff.grant, &data.totp_code).await {
        let mut record = staff.record("staff.session.step_up", None);
        record.summary = Some("second factor wrong".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(error);
    }
    let until = (staff.now + STAFF_STEP_UP_MS).min(staff.session.expires_at);
    record_platform_audit(db, staff.record("staff.session.step_up", None), AuditOutcome::Success)
        .await?;
    db.set_staff_step_up(&staff.session.id, until).await?;
    Ok(Json(StepUp { step_up_until: until }))
}

/// # Who am I
///
/// Roles and permissions of the signed-in staff member; the console builds
/// its navigation from this (the API still checks every request).
#[get("/me")]
async fn me(staff: StaffContext) -> Json<StaffMe> {
    Json(StaffMe {
        user_id: staff.user.id.clone(),
        username: staff.user.username.clone(),
        display_name: staff.user.display_name.clone(),
        roles: staff.grant.roles.clone(),
        permissions: staff.permissions.iter().copied().collect(),
        session_expires_at: staff.session.expires_at,
        idle_timeout_ms: STAFF_SESSION_IDLE_MS,
        step_up_until: staff.session.step_up_until.filter(|u| *u > staff.now),
        access_expires_at: staff.grant.expires_at,
    })
}

// ---- staff management ----

/// # Role definitions
#[get("/roles")]
async fn roles(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<RoleDefinition>>> {
    staff.require(db, StaffPermission::StaffRead).await?;
    Ok(Json(
        StaffRole::ALL
            .iter()
            .map(|role| RoleDefinition {
                id: *role,
                permissions: role.permissions().to_vec(),
            })
            .collect(),
    ))
}

/// # Staff members
#[get("/members")]
async fn members(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<StaffMember>>> {
    staff.require(db, StaffPermission::StaffRead).await?;
    let grants = db.fetch_staff_grants().await?;
    let ids: Vec<String> = grants.iter().map(|g| g.id.clone()).collect();
    let users = db.fetch_users(&ids).await.unwrap_or_default();
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants {
        let open_sessions = db
            .fetch_staff_sessions_for_user(&grant.id, staff.now)
            .await?
            .len();
        out.push(StaffMember {
            username: users
                .iter()
                .find(|u| u.id == grant.id)
                .map(|u| u.username.clone()),
            active: grant.is_active(staff.now),
            user_id: grant.id,
            roles: grant.roles,
            disabled: grant.disabled,
            expires_at: grant.expires_at,
            open_sessions,
            version: grant.version,
            updated_at: grant.updated_at,
            updated_by: grant.updated_by,
        });
    }
    Ok(Json(out))
}

fn role_names(roles: &[StaffRole]) -> String {
    if roles.is_empty() {
        "none".to_string()
    } else {
        roles.iter().map(|r| r.name()).collect::<Vec<_>>().join(",")
    }
}

/// # Set a staff member's roles
///
/// Needs `platform.staff.manage` and a step-up. Nobody changes their own
/// roles; Owner is granted and removed only from the command line; you can
/// only add or remove roles whose permissions you hold. An empty list
/// removes access. Ends the member's open staff sessions.
#[put("/members/<id>", data = "<data>")]
async fn set_member(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataSetMember>,
) -> Result<Json<StaffMember>> {
    staff
        .require_step_up(db, StaffPermission::StaffManage)
        .await?;
    let data = data.into_inner();
    let reason = reason(&data.reason)?;
    let target = db.fetch_user(id).await?;
    if target.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let mut new_roles = data
        .roles
        .iter()
        .map(|name| {
            StaffRole::from_name(name).ok_or_else(|| {
                create_error!(FailedValidation {
                    error: format!("unknown role {name}")
                })
            })
        })
        .collect::<Result<Vec<_>>>()?;
    new_roles.sort();
    new_roles.dedup();

    let current = fetch_grant(db, id).await?;
    let current_roles = current.as_ref().map(|g| g.roles.clone()).unwrap_or_default();
    let mut record = staff.record("staff.grant.update", Some(StaffPermission::StaffManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(target.id.clone());
    record.reason = Some(reason);
    record.summary = Some(format!(
        "roles {} -> {}",
        role_names(&current_roles),
        role_names(&new_roles)
    ));

    if let Err(denied) = check_staff_change(
        &staff.user.id,
        &staff.permissions,
        &target.id,
        &current_roles,
        &new_roles,
    ) {
        let mut record = record;
        record.summary = Some(format!("{} ({denied:?})", record.summary.unwrap_or_default()));
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }

    let expected = current.as_ref().map(|g| g.version);
    if data.version != expected {
        return Err(create_error!(InvalidOperation));
    }
    let grant = StaffGrant {
        id: target.id.clone(),
        roles: new_roles,
        disabled: false,
        expires_at: None,
        last_totp_step: current.as_ref().map(|g| g.last_totp_step).unwrap_or(0),
        version: expected.unwrap_or(0) + 1,
        updated_at: staff.now,
        updated_by: staff.user.id.clone(),
    };
    if !db.save_staff_grant(&grant, expected).await? {
        return Err(create_error!(InvalidOperation));
    }
    if let Err(error) = record_platform_audit(db, record, AuditOutcome::Success).await {
        // put the old grant back: no change without a record
        match &current {
            Some(old) => {
                db.save_staff_grant(old, Some(grant.version)).await?;
            }
            None => {
                let mut empty = grant.clone();
                empty.roles = vec![];
                empty.version += 1;
                db.save_staff_grant(&empty, Some(grant.version)).await?;
            }
        }
        return Err(error);
    }
    db.revoke_staff_sessions_for_user(&target.id, staff.now).await?;
    Ok(Json(StaffMember {
        user_id: grant.id.clone(),
        username: Some(target.username),
        active: grant.is_active(staff.now),
        roles: grant.roles,
        disabled: false,
        expires_at: None,
        open_sessions: 0,
        version: grant.version,
        updated_at: grant.updated_at,
        updated_by: grant.updated_by,
    }))
}

/// # Disable a staff member
///
/// Keeps the roles on record but switches access off and ends their staff
/// sessions. Not for yourself or an Owner (command line only).
#[post("/members/<id>/disable", data = "<data>")]
async fn disable_member(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataReason>,
) -> Result<()> {
    staff
        .require_step_up(db, StaffPermission::StaffManage)
        .await?;
    let reason = reason(&data.reason)?;
    let mut grant = fetch_grant(db, id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    let mut record = staff.record("staff.grant.disable", Some(StaffPermission::StaffManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    record.reason = Some(reason);
    if id == staff.user.id || grant.roles.contains(&StaffRole::Owner) {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    if grant.disabled {
        return Err(create_error!(NoEffect));
    }
    // fail closed: the record comes first
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    let expected = grant.version;
    grant.disabled = true;
    grant.version += 1;
    grant.updated_at = staff.now;
    grant.updated_by = staff.user.id.clone();
    if !db.save_staff_grant(&grant, Some(expected)).await? {
        return Err(create_error!(InvalidOperation));
    }
    db.revoke_staff_sessions_for_user(id, staff.now).await?;
    Ok(())
}

/// # End a staff member's sessions
///
/// For a lost device or a suspected compromise. Protective, so no step-up;
/// works on anyone, including yourself.
#[post("/members/<id>/revoke-sessions", data = "<data>")]
async fn revoke_member_sessions(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataReason>,
) -> Result<Json<usize>> {
    staff.require(db, StaffPermission::StaffManage).await?;
    let mut record = staff.record("staff.sessions.revoke", Some(StaffPermission::StaffManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    record.reason = Some(reason(&data.reason)?);
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    Ok(Json(db.revoke_staff_sessions_for_user(id, staff.now).await?))
}

// ---- audit log ----

/// # Platform audit log
///
/// Newest first; `before` is a `seq` cursor.
#[get("/audit?<before>&<actor>&<target>&<limit>")]
async fn audit(
    db: &State<Database>,
    staff: StaffContext,
    before: Option<i64>,
    actor: Option<String>,
    target: Option<String>,
    limit: Option<usize>,
) -> Result<Json<Vec<PlatformAuditEntry>>> {
    staff.require(db, StaffPermission::AuditRead).await?;
    Ok(Json(
        db.fetch_platform_audit(PlatformAuditQuery {
            before_seq: before,
            actor_id: actor.filter(|a| !a.is_empty() && a.len() <= 64),
            target_id: target.filter(|t| !t.is_empty() && t.len() <= 64),
            limit: limit.unwrap_or(50).clamp(1, 200),
        })
        .await?,
    ))
}

/// # Check the audit chain
///
/// Recomputes hashes from `from` (default: the last 1000 records).
#[get("/audit/verify?<from>&<limit>")]
async fn verify_audit(
    db: &State<Database>,
    staff: StaffContext,
    from: Option<i64>,
    limit: Option<usize>,
) -> Result<Json<AuditCheck>> {
    staff.require(db, StaffPermission::AuditRead).await?;
    let limit = limit.unwrap_or(1000).clamp(1, 5000);
    let last = db.fetch_last_platform_audit().await?.map(|e| e.seq).unwrap_or(0);
    let from = from.unwrap_or(last - limit as i64 + 1).max(1);
    // include the record before `from` so the first link is checked too
    let mut entries = db
        .fetch_platform_audit_range((from - 1).max(1), limit + 1)
        .await?;
    let before = if from > 1 && !entries.is_empty() {
        Some(entries.remove(0))
    } else {
        None
    };
    let result = verify_audit_chain(before.as_ref(), &entries);
    Ok(Json(AuditCheck {
        checked: entries.len(),
        from_seq: from,
        ok: result.is_ok(),
        first_bad_seq: result.err(),
    }))
}
