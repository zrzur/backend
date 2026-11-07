use revolt_result::Result;

use crate::{
    Bot, PlatformAuditEntry, Server, StaffCounted, StaffDbStats, StaffGrant, StaffSession, User,
};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

/// Filters for reading the platform audit log, newest first
#[derive(Default, Clone, Debug)]
pub struct PlatformAuditQuery {
    /// Only records with a `seq` below this
    pub before_seq: Option<i64>,
    pub actor_id: Option<String>,
    pub target_id: Option<String>,
    pub limit: usize,
}

#[async_trait]
pub trait AbstractPlatformStaff: Sync + Send {
    async fn fetch_staff_grant(&self, user_id: &str) -> Result<Option<StaffGrant>>;
    /// Every grant, active or not (staff lists are small; capped at 500)
    async fn fetch_staff_grants(&self) -> Result<Vec<StaffGrant>>;
    /// Save a grant if its stored version is still `expected_version`
    /// (`None`: only if there is no grant yet). Returns false on a race.
    async fn save_staff_grant(
        &self,
        grant: &StaffGrant,
        expected_version: Option<i64>,
    ) -> Result<bool>;
    /// Move `last_totp_step` forward to `step` if it is newer. Returns
    /// false when the step was already used.
    async fn claim_staff_totp_step(&self, user_id: &str, step: u64) -> Result<bool>;

    async fn insert_staff_session(&self, session: &StaffSession) -> Result<()>;
    async fn fetch_staff_session_by_hash(&self, token_hash: &str) -> Result<Option<StaffSession>>;
    async fn touch_staff_session(&self, id: &str, at: i64) -> Result<()>;
    async fn set_staff_step_up(&self, id: &str, until: i64) -> Result<()>;
    async fn revoke_staff_session(&self, id: &str, at: i64) -> Result<()>;
    /// Revoke every open staff session of a user; returns how many
    async fn revoke_staff_sessions_for_user(&self, user_id: &str, at: i64) -> Result<usize>;
    /// Open (not revoked, not expired) staff sessions of a user
    async fn fetch_staff_sessions_for_user(
        &self,
        user_id: &str,
        now: i64,
    ) -> Result<Vec<StaffSession>>;

    /// The newest audit record, if any
    async fn fetch_last_platform_audit(&self) -> Result<Option<PlatformAuditEntry>>;
    /// Insert a sealed record. Returns false if its `seq` is taken (another
    /// record was written first); the caller re-seals and retries.
    async fn insert_platform_audit(&self, entry: &PlatformAuditEntry) -> Result<bool>;
    async fn fetch_platform_audit(&self, query: PlatformAuditQuery)
        -> Result<Vec<PlatformAuditEntry>>;
    /// Records with `from_seq <= seq`, oldest first, at most `limit`
    async fn fetch_platform_audit_range(
        &self,
        from_seq: i64,
        limit: usize,
    ) -> Result<Vec<PlatformAuditEntry>>;

    // ---- statistics and directories (read-only) ----

    /// Documents with `from_id <= _id < to_id` (ULIDs); the whole
    /// collection (estimated, fast) without bounds
    async fn staff_count(
        &self,
        what: StaffCounted,
        from_id: Option<&str>,
        to_id: Option<&str>,
    ) -> Result<u64>;
    /// Trust & Safety cases that still need someone
    async fn staff_open_safety_cases(&self) -> Result<u64>;
    /// Channels per type (`TextChannel`, `VoiceChannel`, `DirectMessage`, ...)
    async fn staff_channel_types(&self) -> Result<Vec<(String, u64)>>;
    /// Users, newest first, before a user id cursor
    async fn staff_list_users(&self, before: Option<&str>, limit: usize) -> Result<Vec<User>>;
    /// Servers, newest first; `query` matches the id or a part of the name
    async fn staff_list_servers(
        &self,
        query: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Server>>;
    /// Bots, newest first; `query` is a bot id or an owner id
    async fn staff_list_bots(
        &self,
        query: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Bot>>;
    /// Database size and per-collection document counts
    async fn staff_db_stats(&self) -> Result<StaffDbStats>;
}
