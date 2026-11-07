//! The `StaffContext` request guard. Every `/staff` request needs both the
//! ordinary session (`x-session-token`) and the staff session it opened
//! (`x-staff-session`); permissions are read from the database on every
//! request, so a revoked role stops working at once.

use std::collections::BTreeSet;

use revolt_config::config;
use revolt_database::{
    record_platform_audit, staff_now, staff_token_hash, AuditOutcome, AuditRecord, Database,
    Session, StaffGrant, StaffPermission, StaffSession, User, STAFF_SESSION_IDLE_MS,
};
use revolt_result::{create_error, Error, Result};
use rocket::{
    http::Status,
    request::{FromRequest, Outcome},
    Request,
};

pub const STAFF_HEADER: &str = "x-staff-session";

/// Only write `last_seen_at` once a minute
const TOUCH_EVERY_MS: i64 = 60 * 1000;

/// Requests from a browser must come from a configured admin origin.
/// Requests without an `Origin` header (curl, server-side) still need
/// both tokens.
pub async fn origin_allowed(request: &Request<'_>) -> bool {
    match request.headers().get_one("origin") {
        None => true,
        Some(origin) => config()
            .await
            .api
            .security
            .staff_origins
            .iter()
            .any(|allowed| allowed == origin),
    }
}

pub struct StaffContext {
    pub user: User,
    pub session: StaffSession,
    pub grant: StaffGrant,
    pub permissions: BTreeSet<StaffPermission>,
    pub now: i64,
    /// Request path (no query string), for denied-access records
    pub path: String,
}

fn unauthorized<T>() -> Outcome<T, Error> {
    Outcome::Error((Status::Unauthorized, create_error!(InvalidSession)))
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for StaffContext {
    type Error = Error;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let db = request.rocket().state::<Database>().expect("`Database`");
        let Outcome::Success(session) = request.guard::<Session>().await else {
            return unauthorized();
        };
        let Some(token) = request.headers().get_one(STAFF_HEADER) else {
            return unauthorized();
        };
        if !origin_allowed(request).await {
            log::warn!("staff request from a disallowed origin");
            return unauthorized();
        }
        let Ok(user) = db.fetch_user(&session.user_id).await else {
            return unauthorized();
        };
        if user.bot.is_some() {
            return unauthorized();
        }
        let Ok(Some(staff)) = db.fetch_staff_session_by_hash(&staff_token_hash(token)).await
        else {
            return unauthorized();
        };
        let now = staff_now();
        let valid = staff.revoked_at.is_none()
            && now < staff.expires_at
            && now - staff.last_seen_at < STAFF_SESSION_IDLE_MS
            && staff.user_id == user.id
            && staff.session_id == session.id;
        if !valid {
            return unauthorized();
        }
        let grant = match db.fetch_staff_grant(&user.id).await {
            Ok(Some(grant)) if grant.is_active(now) => grant,
            Ok(_) => {
                // access was removed: this session is over
                let _ = db.revoke_staff_session(&staff.id, now).await;
                return unauthorized();
            }
            Err(_) => return unauthorized(),
        };
        if now - staff.last_seen_at > TOUCH_EVERY_MS {
            let _ = db.touch_staff_session(&staff.id, now).await;
        }
        Outcome::Success(StaffContext {
            permissions: grant.effective(now),
            user,
            session: staff,
            grant,
            now,
            path: request.uri().path().to_string(),
        })
    }
}

impl StaffContext {
    pub fn has(&self, permission: StaffPermission) -> bool {
        self.permissions.contains(&permission)
    }

    pub fn record(&self, action: &str, permission: Option<StaffPermission>) -> AuditRecord {
        AuditRecord {
            actor_id: self.user.id.clone(),
            action: action.to_string(),
            permission,
            staff_session_id: Some(self.session.id.clone()),
            ..Default::default()
        }
    }

    /// Refuse (and record the attempt) unless the staff member holds
    /// `permission`
    pub async fn require(&self, db: &Database, permission: StaffPermission) -> Result<()> {
        if self.has(permission) {
            return Ok(());
        }
        let mut record = self.record("staff.access.denied", Some(permission));
        record.summary = Some(self.path.clone());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        Err(create_error!(MissingPlatformPermission {
            permission: permission.name().to_string()
        }))
    }

    /// `require`, plus a fresh TOTP (step-up) within the last few minutes
    pub async fn require_step_up(&self, db: &Database, permission: StaffPermission) -> Result<()> {
        self.require(db, permission).await?;
        if self.session.step_up_until.is_some_and(|until| self.now < until) {
            Ok(())
        } else {
            Err(create_error!(NotElevated))
        }
    }
}
