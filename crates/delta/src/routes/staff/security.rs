//! The security centre in the admin console: what went wrong recently,
//! read from the platform audit log, and a fast way to cut off a staff
//! session that looks wrong.
//!
//! - `platform.security.read`: overview of refused and failed staff
//!   actions, failed staff sign-ins and step-ups, the newest problems
//! - `platform.security.respond` + a fresh step-up: end every staff session
//!   of a staff member at once (their roles stay; they sign in again)

use std::collections::HashMap;

use revolt_database::{record_platform_audit, AuditOutcome, Database, PlatformAuditEntry, PlatformAuditQuery, StaffPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};

use super::{reason, StaffContext};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// How far back the overview reads
const SCAN: usize = 2000;

#[derive(Serialize, Default)]
pub struct Window {
    pub denied: usize,
    pub failed: usize,
    pub failed_sign_ins: usize,
    pub failed_step_ups: usize,
    pub sessions_revoked: usize,
}

#[derive(Serialize)]
pub struct ActorCount {
    pub actor_id: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct SecurityOverview {
    pub day: Window,
    pub week: Window,
    /// Staff members refused most often this week
    pub top_denied: Vec<ActorCount>,
    /// Newest refused or failed records
    pub recent: Vec<PlatformAuditEntry>,
    /// Records read (the overview covers at most this many)
    pub scanned: usize,
}

fn count(window: &mut Window, e: &PlatformAuditEntry) {
    let bad = matches!(e.outcome, AuditOutcome::Denied | AuditOutcome::Failed);
    match e.outcome {
        AuditOutcome::Denied => window.denied += 1,
        AuditOutcome::Failed => window.failed += 1,
        _ => {}
    }
    if bad && e.action == "staff.session.create" {
        window.failed_sign_ins += 1;
    }
    if bad && e.action == "staff.session.step_up" {
        window.failed_step_ups += 1;
    }
    if e.action == "staff.sessions.revoke" || e.action == "staff.security.lock" {
        window.sessions_revoked += 1;
    }
}

/// # Security overview
#[get("/security")]
pub async fn overview(db: &State<Database>, staff: StaffContext) -> Result<Json<SecurityOverview>> {
    staff.require(db, StaffPermission::SecurityRead).await?;
    let entries = db
        .fetch_platform_audit(PlatformAuditQuery {
            before_seq: None,
            actor_id: None,
            target_id: None,
            limit: SCAN,
        })
        .await?;
    let mut day = Window::default();
    let mut week = Window::default();
    let mut denied_by: HashMap<String, usize> = HashMap::new();
    let mut recent = vec![];
    for e in &entries {
        let age = staff.now - e.created_at;
        if age < DAY_MS {
            count(&mut day, e);
        }
        if age < 7 * DAY_MS {
            count(&mut week, e);
            if e.outcome == AuditOutcome::Denied {
                *denied_by.entry(e.actor_id.clone()).or_default() += 1;
            }
        }
        if matches!(e.outcome, AuditOutcome::Denied | AuditOutcome::Failed) && recent.len() < 50 {
            recent.push(e.clone());
        }
    }
    let mut top_denied: Vec<_> = denied_by
        .into_iter()
        .map(|(actor_id, count)| ActorCount { actor_id, count })
        .collect();
    top_denied.sort_by(|a, b| b.count.cmp(&a.count).then(a.actor_id.cmp(&b.actor_id)));
    top_denied.truncate(5);
    Ok(Json(SecurityOverview {
        day,
        week,
        top_denied,
        recent,
        scanned: entries.len(),
    }))
}

#[derive(Deserialize)]
pub struct DataLock {
    pub reason: String,
}

/// # End every staff session of a staff member
///
/// For a session that looks stolen. Their roles stay; they have to sign in
/// to the console again (with their authenticator).
#[post("/security/members/<id>/lock", data = "<data>")]
pub async fn lock(db: &State<Database>, staff: StaffContext, id: &str, data: Json<DataLock>) -> Result<Json<usize>> {
    staff.require_step_up(db, StaffPermission::SecurityRespond).await?;
    let why = reason(&data.reason)?;
    if id == staff.user.id {
        // signing yourself out is the sign-out button
        return Err(create_error!(InvalidOperation));
    }
    if db.fetch_staff_grant(id).await?.is_none() {
        return Err(create_error!(NotFound));
    }
    let mut record = staff.record("staff.security.lock", Some(StaffPermission::SecurityRespond));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    record.reason = Some(why);
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    Ok(Json(db.revoke_staff_sessions_for_user(id, staff.now).await?))
}
