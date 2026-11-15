//! Platform staff: roles and permissions for the admin console, staff
//! sessions, and the append-only platform audit log.
//!
//! This is separate from server roles and from `User::privileged`: neither
//! grants anything here, and nothing here reads them.

mod logic;
mod model;
mod ops;

pub use logic::*;
pub use model::*;
pub use ops::*;

use revolt_result::Result;
use ulid::Ulid;

use crate::Database;

/// Milliseconds since the epoch
pub fn staff_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// What to write to the platform audit log
#[derive(Debug, Clone, Default)]
pub struct AuditRecord {
    pub actor_id: String,
    pub action: String,
    pub permission: Option<StaffPermission>,
    pub target_kind: Option<String>,
    pub target_id: Option<String>,
    pub reason: Option<String>,
    pub staff_session_id: Option<String>,
    pub summary: Option<String>,
}

/// Append a record to the platform audit log. Fails closed: callers write
/// the record before (or instead of) the change, and a failed write stops
/// the change.
pub async fn record_platform_audit(
    db: &Database,
    record: AuditRecord,
    outcome: AuditOutcome,
) -> Result<PlatformAuditEntry> {
    let mut entry = PlatformAuditEntry {
        id: Ulid::new().to_string(),
        seq: 0,
        prev_hash: String::new(),
        hash: String::new(),
        actor_id: record.actor_id,
        action: record.action,
        permission: record.permission.map(|p| p.name().to_string()),
        target_kind: record.target_kind,
        target_id: record.target_id,
        reason: record.reason.map(|r| r.chars().take(500).collect()),
        outcome,
        staff_session_id: record.staff_session_id,
        summary: record.summary.map(|s| s.chars().take(1000).collect()),
        created_at: staff_now(),
    };
    // another writer may take the same seq; re-link and try again
    for _ in 0..8 {
        let last = db.fetch_last_platform_audit().await?;
        seal_audit_entry(&mut entry, last.as_ref());
        if db.insert_platform_audit(&entry).await? {
            return Ok(entry);
        }
    }
    Err(create_database_error!("insert_one", "platform_audit"))
}

/// Whether the `privileged` flag of `user_id` may count on a request made
/// with the ordinary session `session_id`.
///
/// `privileged` overrides every server and channel permission and opens
/// the old admin routes (other people's profiles and flags, server flags,
/// Discover decisions, billing admin). An ordinary session alone must not
/// carry that: a stolen app token would be the whole platform. So the flag
/// counts only while the account holds an active Owner staff grant and has
/// an open, not idle staff session opened (with a TOTP code) from this same
/// ordinary session. The everyday app session never has one; the admin
/// console signs in with its own.
pub async fn privileged_in_effect(db: &Database, user_id: &str, session_id: &str) -> bool {
    let now = staff_now();
    match db.fetch_staff_grant(user_id).await {
        Ok(Some(grant)) if grant.is_owner(now) => {}
        _ => return false,
    }
    db.fetch_staff_sessions_for_user(user_id, now)
        .await
        .map(|sessions| {
            sessions.iter().any(|staff| {
                staff.session_id == session_id
                    && staff.user_id == user_id
                    && staff.revoked_at.is_none()
                    && now < staff.expires_at
                    && now - staff.last_seen_at < STAFF_SESSION_IDLE_MS
            })
        })
        .unwrap_or(false)
}

/// In-memory store used by the reference driver
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformStaffStore {
    pub grants: std::collections::HashMap<String, StaffGrant>,
    pub sessions: std::collections::HashMap<String, StaffSession>,
    pub audit: Vec<PlatformAuditEntry>,
}
