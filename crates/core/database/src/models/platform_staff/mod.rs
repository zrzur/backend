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
                // only the console's own session: a Zrzur Desk session also
                // names the ordinary session it was opened from, and must
                // never lift that everyday app session to `privileged`
                staff.kind == StaffSessionKind::Console
                    && staff.session_id == session_id
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

#[cfg(test)]
mod desk_session_tests {
    use super::*;
    use crate::DatabaseInfo;

    fn session(id: &str, user: &str, kind: StaffSessionKind, bot: Option<&str>) -> StaffSession {
        StaffSession {
            id: id.into(),
            token_hash: format!("hash-{id}"),
            user_id: user.into(),
            session_id: "s".into(),
            created_at: 100,
            last_seen_at: 100,
            expires_at: 1_000,
            step_up_until: None,
            revoked_at: None,
            user_agent: None,
            kind,
            bot_id: bot.map(Into::into),
        }
    }

    /// An Owner signed in on Zrzur Desk from their everyday app session: that
    /// app session must not count as `privileged`; a console session from it
    /// still does
    #[tokio::test]
    async fn desk_session_never_makes_privileged() {
        let db = DatabaseInfo::Reference.connect().await.unwrap();
        let now = staff_now();
        let owner = StaffGrant {
            id: "owner".into(),
            roles: vec![StaffRole::Owner],
            disabled: false,
            expires_at: None,
            last_totp_step: 0,
            version: 1,
            updated_at: now,
            updated_by: "test".into(),
        };
        assert!(db.save_staff_grant(&owner, None).await.unwrap());
        let live = |id: &str, kind: StaffSessionKind, bot: Option<&str>| StaffSession {
            session_id: "app".into(),
            created_at: now,
            last_seen_at: now,
            expires_at: now + STAFF_SESSION_MAX_MS,
            ..session(id, "owner", kind, bot)
        };
        db.insert_staff_session(&live("desk", StaffSessionKind::Desk, Some("bot")))
            .await
            .unwrap();
        assert!(!privileged_in_effect(&db, "owner", "app").await);

        db.insert_staff_session(&live("console", StaffSessionKind::Console, None))
            .await
            .unwrap();
        assert!(privileged_in_effect(&db, "owner", "app").await);
    }

    #[test]
    fn old_document_without_kind_reads_as_console() {
        let old = serde_json::json!({
            "_id": "a", "token_hash": "h", "user_id": "u", "session_id": "s",
            "created_at": 1, "last_seen_at": 1, "expires_at": 2
        });
        let parsed: StaffSession = serde_json::from_value(old).unwrap();
        assert_eq!(parsed.kind, StaffSessionKind::Console);
        assert_eq!(parsed.bot_id, None);
        let out = serde_json::to_value(&parsed).unwrap();
        assert_eq!(out["kind"], "console");
        assert!(out.get("bot_id").is_none());
        assert_eq!(serde_json::to_value(StaffSessionKind::Desk).unwrap(), "desk");
    }

    #[tokio::test]
    async fn fetch_desk_session_filters() {
        let db = DatabaseInfo::Reference.connect().await.unwrap();
        db.insert_staff_session(&session("console", "u", StaffSessionKind::Console, None))
            .await
            .unwrap();
        let mut revoked = session("revoked", "u", StaffSessionKind::Desk, Some("bot"));
        revoked.revoked_at = Some(150);
        db.insert_staff_session(&revoked).await.unwrap();
        let mut expired = session("expired", "u", StaffSessionKind::Desk, Some("bot"));
        expired.expires_at = 500;
        db.insert_staff_session(&expired).await.unwrap();
        db.insert_staff_session(&session("other", "u", StaffSessionKind::Desk, Some("bot2")))
            .await
            .unwrap();
        assert!(db.fetch_desk_session("u", "bot", 600).await.unwrap().is_none());

        let mut older = session("older", "u", StaffSessionKind::Desk, Some("bot"));
        older.created_at = 200;
        db.insert_staff_session(&older).await.unwrap();
        let mut newer = session("newer", "u", StaffSessionKind::Desk, Some("bot"));
        newer.created_at = 300;
        db.insert_staff_session(&newer).await.unwrap();
        let found = db.fetch_desk_session("u", "bot", 600).await.unwrap().unwrap();
        assert_eq!(found.id, "newer");
        assert!(db.fetch_desk_session("someone", "bot", 600).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn revoking_desk_sessions_leaves_console_sessions() {
        let db = DatabaseInfo::Reference.connect().await.unwrap();
        db.insert_staff_session(&session("console", "u", StaffSessionKind::Console, None))
            .await
            .unwrap();
        db.insert_staff_session(&session("desk", "u", StaffSessionKind::Desk, Some("bot")))
            .await
            .unwrap();
        db.insert_staff_session(&session("desk2", "v", StaffSessionKind::Desk, Some("bot")))
            .await
            .unwrap();
        assert_eq!(db.revoke_desk_sessions_for_user("u", 700).await.unwrap(), 1);
        assert_eq!(db.revoke_desk_sessions_for_user("u", 800).await.unwrap(), 0);
        assert!(db.fetch_desk_session("u", "bot", 600).await.unwrap().is_none());
        assert!(db.fetch_desk_session("v", "bot", 600).await.unwrap().is_some());
        let open = db.fetch_staff_sessions_for_user("u", 600).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "console");
    }
}
