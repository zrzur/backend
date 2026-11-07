//! Staff roles, permission checks, the audit hash chain and the TOTP check
//! for platform staff. Pure functions, so routes, the CLI and tests all use
//! the same rules.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::{PlatformAuditEntry, StaffGrant, StaffPermission, StaffRole, AUDIT_GENESIS_HASH};

impl StaffRole {
    pub const ALL: [StaffRole; 9] = [
        StaffRole::Owner,
        StaffRole::Security,
        StaffRole::Operations,
        StaffRole::TrustSafety,
        StaffRole::SeniorSupport,
        StaffRole::Support,
        StaffRole::Billing,
        StaffRole::Analyst,
        StaffRole::Auditor,
    ];

    /// What a role allows. This table is the permission matrix.
    pub fn permissions(&self) -> &'static [StaffPermission] {
        use StaffPermission::*;
        match self {
            StaffRole::Owner => &StaffPermission::ALL,
            StaffRole::Security => &[
                DashboardRead,
                AnalyticsRead,
                UsersRead,
                UsersPiiRead,
                UsersSuspend,
                UsersSessionsRevoke,
                ServersRead,
                ReportsRead,
                SecurityRead,
                SecurityRespond,
                OperationsRead,
                AuditRead,
                StaffRead,
                SettingsRead,
                OfficialManage,
            ],
            StaffRole::Operations => &[
                DashboardRead,
                AnalyticsRead,
                SecurityRead,
                OperationsRead,
                OperationsManage,
                SettingsRead,
            ],
            StaffRole::TrustSafety => &[
                DashboardRead,
                UsersRead,
                UsersSuspend,
                UsersDeleteRequest,
                ServersRead,
                ServersRestrict,
                ServersDeleteRequest,
                ReportsRead,
                ReportsResolve,
                SupportRead,
                CreatorsManage,
            ],
            StaffRole::SeniorSupport => &[
                DashboardRead,
                UsersRead,
                UsersPiiRead,
                UsersSessionsRevoke,
                ServersRead,
                ReportsRead,
                SupportRead,
                SupportManage,
                BadgesManage,
                CreatorsManage,
            ],
            StaffRole::Support => &[DashboardRead, UsersRead, ServersRead, SupportRead, SupportManage],
            StaffRole::Billing => &[
                DashboardRead,
                AnalyticsRead,
                UsersRead,
                SupportRead,
                BillingRead,
                BillingManage,
            ],
            StaffRole::Analyst => &[DashboardRead, AnalyticsRead],
            StaffRole::Auditor => &[DashboardRead, AuditRead, StaffRead],
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            StaffRole::Owner => "owner",
            StaffRole::Security => "security",
            StaffRole::Operations => "operations",
            StaffRole::TrustSafety => "trust_safety",
            StaffRole::SeniorSupport => "senior_support",
            StaffRole::Support => "support",
            StaffRole::Billing => "billing",
            StaffRole::Analyst => "analyst",
            StaffRole::Auditor => "auditor",
        }
    }

    pub fn from_name(name: &str) -> Option<StaffRole> {
        StaffRole::ALL.into_iter().find(|r| r.name() == name)
    }
}

impl StaffPermission {
    pub const ALL: [StaffPermission; 28] = [
        StaffPermission::DashboardRead,
        StaffPermission::AnalyticsRead,
        StaffPermission::UsersRead,
        StaffPermission::UsersPiiRead,
        StaffPermission::UsersSuspend,
        StaffPermission::UsersSessionsRevoke,
        StaffPermission::UsersDeleteRequest,
        StaffPermission::ServersRead,
        StaffPermission::ServersRestrict,
        StaffPermission::ServersDeleteRequest,
        StaffPermission::ReportsRead,
        StaffPermission::ReportsResolve,
        StaffPermission::SupportRead,
        StaffPermission::SupportManage,
        StaffPermission::SecurityRead,
        StaffPermission::SecurityRespond,
        StaffPermission::OperationsRead,
        StaffPermission::OperationsManage,
        StaffPermission::BillingRead,
        StaffPermission::BillingManage,
        StaffPermission::AuditRead,
        StaffPermission::StaffRead,
        StaffPermission::StaffManage,
        StaffPermission::SettingsRead,
        StaffPermission::SettingsManage,
        StaffPermission::BadgesManage,
        StaffPermission::CreatorsManage,
        StaffPermission::OfficialManage,
    ];

    pub fn name(&self) -> &'static str {
        use StaffPermission::*;
        match self {
            DashboardRead => "platform.dashboard.read",
            AnalyticsRead => "platform.analytics.read",
            UsersRead => "platform.users.read",
            UsersPiiRead => "platform.users.pii.read",
            UsersSuspend => "platform.users.suspend",
            UsersSessionsRevoke => "platform.users.sessions.revoke",
            UsersDeleteRequest => "platform.users.delete.request",
            ServersRead => "platform.servers.read",
            ServersRestrict => "platform.servers.restrict",
            ServersDeleteRequest => "platform.servers.delete.request",
            ReportsRead => "platform.reports.read",
            ReportsResolve => "platform.reports.resolve",
            SupportRead => "platform.support.read",
            SupportManage => "platform.support.manage",
            SecurityRead => "platform.security.read",
            SecurityRespond => "platform.security.respond",
            OperationsRead => "platform.operations.read",
            OperationsManage => "platform.operations.manage",
            BillingRead => "platform.billing.read",
            BillingManage => "platform.billing.manage",
            AuditRead => "platform.audit.read",
            StaffRead => "platform.staff.read",
            StaffManage => "platform.staff.manage",
            SettingsRead => "platform.settings.read",
            SettingsManage => "platform.settings.manage",
            BadgesManage => "platform.badges.manage",
            CreatorsManage => "platform.creators.manage",
            OfficialManage => "platform.official.manage",
        }
    }

    /// Permissions whose actions always need a fresh TOTP (step-up)
    pub fn needs_step_up(&self) -> bool {
        matches!(
            self,
            StaffPermission::UsersDeleteRequest
                | StaffPermission::ServersDeleteRequest
                | StaffPermission::BillingManage
                | StaffPermission::StaffManage
                | StaffPermission::SettingsManage
        )
    }
}

/// Permissions of a set of roles
pub fn staff_role_permissions<'a>(
    roles: impl IntoIterator<Item = &'a StaffRole>,
) -> BTreeSet<StaffPermission> {
    roles
        .into_iter()
        .flat_map(|r| r.permissions().iter().copied())
        .collect()
}

impl StaffGrant {
    /// Whether the grant gives any access at `now`
    pub fn is_active(&self, now: i64) -> bool {
        !self.disabled && !self.roles.is_empty() && self.expires_at.is_none_or(|end| now < end)
    }

    /// What this grant allows at `now`; empty when disabled or expired
    pub fn effective(&self, now: i64) -> BTreeSet<StaffPermission> {
        if self.is_active(now) {
            staff_role_permissions(&self.roles)
        } else {
            BTreeSet::new()
        }
    }

    pub fn is_owner(&self, now: i64) -> bool {
        self.is_active(now) && self.roles.contains(&StaffRole::Owner)
    }
}

/// Why a staff change over the API was refused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaffChangeDenied {
    /// The actor lacks `platform.staff.manage`
    NotManager,
    /// Nobody changes their own staff access
    SelfChange,
    /// Owner is granted and removed only from the command line
    OwnerRole,
    /// The change touches a permission the actor doesn't hold
    BeyondOwnPermissions,
}

/// Check a change of `target`'s roles from `current` to `new` by `actor`.
/// An actor may only add or remove roles whose every permission they hold
/// themselves, so nobody can hand out (or take away) more than they have.
pub fn check_staff_change(
    actor_id: &str,
    actor_permissions: &BTreeSet<StaffPermission>,
    target_id: &str,
    current: &[StaffRole],
    new: &[StaffRole],
) -> Result<(), StaffChangeDenied> {
    if !actor_permissions.contains(&StaffPermission::StaffManage) {
        return Err(StaffChangeDenied::NotManager);
    }
    if actor_id == target_id {
        return Err(StaffChangeDenied::SelfChange);
    }
    if current.contains(&StaffRole::Owner) || new.contains(&StaffRole::Owner) {
        return Err(StaffChangeDenied::OwnerRole);
    }
    let current: BTreeSet<_> = current.iter().copied().collect();
    let new: BTreeSet<_> = new.iter().copied().collect();
    let touched = staff_role_permissions(current.symmetric_difference(&new));
    if !touched.is_subset(actor_permissions) {
        return Err(StaffChangeDenied::BeyondOwnPermissions);
    }
    Ok(())
}

// ---- audit hash chain ----

fn field(hasher: &mut Sha256, value: Option<&str>) {
    // length-prefixed, so no two different records hash the same input
    match value {
        Some(v) => hasher.update(format!("{}:{};", v.len(), v).as_bytes()),
        None => hasher.update(b"-;"),
    }
}

/// Hash of a record's content and the hash before it
pub fn audit_hash(entry: &PlatformAuditEntry) -> String {
    let mut h = Sha256::new();
    field(&mut h, Some(&entry.id));
    field(&mut h, Some(&entry.seq.to_string()));
    field(&mut h, Some(&entry.prev_hash));
    field(&mut h, Some(&entry.actor_id));
    field(&mut h, Some(&entry.action));
    field(&mut h, entry.permission.as_deref());
    field(&mut h, entry.target_kind.as_deref());
    field(&mut h, entry.target_id.as_deref());
    field(&mut h, entry.reason.as_deref());
    field(&mut h, Some(&format!("{:?}", entry.outcome)));
    field(&mut h, entry.staff_session_id.as_deref());
    field(&mut h, entry.summary.as_deref());
    field(&mut h, Some(&entry.created_at.to_string()));
    hex::encode(h.finalize())
}

/// Link `entry` after `previous` (or start the chain) and seal it
pub fn seal_audit_entry(entry: &mut PlatformAuditEntry, previous: Option<&PlatformAuditEntry>) {
    match previous {
        Some(p) => {
            entry.seq = p.seq + 1;
            entry.prev_hash = p.hash.clone();
        }
        None => {
            entry.seq = 1;
            entry.prev_hash = AUDIT_GENESIS_HASH.to_string();
        }
    }
    entry.hash = audit_hash(entry);
}

/// Check consecutive records (oldest first). `before` is the record just
/// before the first one, if the slice doesn't start the chain. Returns the
/// `seq` of the first record that doesn't fit.
pub fn verify_audit_chain(
    before: Option<&PlatformAuditEntry>,
    entries: &[PlatformAuditEntry],
) -> Result<(), i64> {
    let mut prev_hash = before.map(|b| b.hash.clone());
    let mut prev_seq = before.map(|b| b.seq);
    for entry in entries {
        let expected_prev = match (&prev_hash, entry.seq) {
            (Some(h), _) => h.as_str(),
            (None, 1) => AUDIT_GENESIS_HASH,
            // a slice that doesn't start the chain needs `before`
            (None, _) => &entry.prev_hash,
        };
        let seq_ok = prev_seq.is_none_or(|s| entry.seq == s + 1);
        if !seq_ok || entry.prev_hash != expected_prev || audit_hash(entry) != entry.hash {
            return Err(entry.seq);
        }
        prev_hash = Some(entry.hash.clone());
        prev_seq = Some(entry.seq);
    }
    Ok(())
}

// ---- tokens and TOTP ----

/// SHA-256 (hex) of a staff session token; only this is stored
pub fn staff_token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// A new staff session token (64 url-safe characters)
pub fn new_staff_token() -> String {
    nanoid::nanoid!(64)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Check a 6-digit TOTP code against a base32 secret, allowing one 30 s
/// step of clock drift either way. Returns the matched time step if it is
/// newer than `last_step`, so a code can't be used twice.
pub fn verify_staff_totp(secret: &str, code: &str, now_secs: u64, last_step: u64) -> Option<u64> {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let key = base32::decode(base32::Alphabet::RFC4648 { padding: false }, secret)?;
    let step = now_secs / totp_lite::DEFAULT_STEP;
    for candidate in [step.saturating_sub(1), step, step + 1] {
        if candidate <= last_step {
            continue;
        }
        let expected = totp_lite::totp_custom::<totp_lite::Sha1>(
            totp_lite::DEFAULT_STEP,
            6,
            &key,
            candidate * totp_lite::DEFAULT_STEP,
        );
        if constant_time_eq(expected.as_bytes(), code.as_bytes()) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuditOutcome;
    use StaffPermission as P;
    use StaffRole as R;

    fn grant(roles: Vec<StaffRole>) -> StaffGrant {
        StaffGrant {
            id: "u".into(),
            roles,
            disabled: false,
            expires_at: None,
            last_totp_step: 0,
            version: 1,
            updated_at: 0,
            updated_by: "x".into(),
        }
    }

    #[test]
    fn roles_are_least_privilege() {
        let support = staff_role_permissions(&[R::Support]);
        assert!(support.contains(&P::SupportManage));
        for p in [P::UsersPiiRead, P::SecurityRead, P::StaffManage, P::BillingRead, P::AuditRead] {
            assert!(!support.contains(&p), "support must not have {}", p.name());
        }
        let analyst = staff_role_permissions(&[R::Analyst]);
        assert_eq!(analyst.len(), 2);
        let auditor = staff_role_permissions(&[R::Auditor]);
        assert!(auditor.contains(&P::AuditRead) && !auditor.contains(&P::StaffManage));
        // only Owner can manage staff
        for role in StaffRole::ALL {
            assert_eq!(
                role.permissions().contains(&P::StaffManage),
                role == R::Owner,
                "{}",
                role.name()
            );
        }
        assert_eq!(staff_role_permissions(&[R::Owner]).len(), StaffPermission::ALL.len());
    }

    #[test]
    fn names_round_trip() {
        for role in StaffRole::ALL {
            assert_eq!(StaffRole::from_name(role.name()), Some(role));
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json, format!("\"{}\"", role.name()));
        }
        for p in StaffPermission::ALL {
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{}\"", p.name()));
        }
    }

    #[test]
    fn disabled_or_expired_grants_give_nothing() {
        let mut g = grant(vec![R::Security]);
        assert!(!g.effective(10).is_empty());
        g.disabled = true;
        assert!(g.effective(10).is_empty());
        g.disabled = false;
        g.expires_at = Some(10);
        assert!(!g.effective(9).is_empty());
        assert!(g.effective(10).is_empty());
        assert!(grant(vec![]).effective(0).is_empty());
    }

    #[test]
    fn staff_changes_cannot_escalate() {
        let owner = staff_role_permissions(&[R::Owner]);
        let security = staff_role_permissions(&[R::Security]);
        // managers only
        assert_eq!(
            check_staff_change("a", &security, "b", &[], &[R::Support]),
            Err(StaffChangeDenied::NotManager)
        );
        // never yourself
        assert_eq!(
            check_staff_change("a", &owner, "a", &[R::Owner], &[R::Owner, R::Billing]),
            Err(StaffChangeDenied::SelfChange)
        );
        // never Owner over the API, in either direction
        assert_eq!(
            check_staff_change("a", &owner, "b", &[], &[R::Owner]),
            Err(StaffChangeDenied::OwnerRole)
        );
        assert_eq!(
            check_staff_change("a", &owner, "b", &[R::Owner], &[]),
            Err(StaffChangeDenied::OwnerRole)
        );
        assert!(check_staff_change("a", &owner, "b", &[R::Support], &[R::SeniorSupport]).is_ok());
        // a manager without billing powers can't hand out billing
        let mut partial = owner.clone();
        partial.remove(&P::BillingManage);
        assert_eq!(
            check_staff_change("a", &partial, "b", &[], &[R::Billing]),
            Err(StaffChangeDenied::BeyondOwnPermissions)
        );
        // ...or take it away
        assert_eq!(
            check_staff_change("a", &partial, "b", &[R::Billing], &[]),
            Err(StaffChangeDenied::BeyondOwnPermissions)
        );
        // untouched roles don't count
        assert!(check_staff_change("a", &partial, "b", &[R::Billing], &[R::Billing, R::Support]).is_ok());
    }

    fn entry(id: &str) -> PlatformAuditEntry {
        PlatformAuditEntry {
            id: id.into(),
            seq: 0,
            prev_hash: String::new(),
            hash: String::new(),
            actor_id: "actor".into(),
            action: "staff.test".into(),
            permission: None,
            target_kind: Some("user".into()),
            target_id: Some("t".into()),
            reason: Some("because".into()),
            outcome: AuditOutcome::Success,
            staff_session_id: None,
            summary: None,
            created_at: 5,
        }
    }

    #[test]
    fn audit_chain_detects_tampering() {
        let mut chain = vec![];
        for id in ["a", "b", "c"] {
            let mut e = entry(id);
            seal_audit_entry(&mut e, chain.last());
            chain.push(e);
        }
        assert_eq!(chain[2].seq, 3);
        assert!(verify_audit_chain(None, &chain).is_ok());
        assert!(verify_audit_chain(Some(&chain[0]), &chain[1..]).is_ok());

        let mut edited = chain.clone();
        edited[1].reason = Some("changed".into());
        assert_eq!(verify_audit_chain(None, &edited), Err(2));

        let mut removed = chain.clone();
        removed.remove(1);
        assert_eq!(verify_audit_chain(None, &removed), Err(3));

        // re-hashing an edited record still breaks the next link
        let mut rehashed = chain.clone();
        rehashed[1].outcome = AuditOutcome::Denied;
        rehashed[1].hash = audit_hash(&rehashed[1]);
        assert_eq!(verify_audit_chain(None, &rehashed), Err(3));
    }

    #[test]
    fn totp_allows_drift_and_blocks_replay() {
        let secret = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";
        let key = base32::decode(base32::Alphabet::RFC4648 { padding: false }, secret).unwrap();
        let now = 1_800_000_000u64;
        let step = now / 30;
        let code = |s: u64| totp_lite::totp_custom::<totp_lite::Sha1>(30, 6, &key, s * 30);
        assert_eq!(verify_staff_totp(secret, &code(step), now, 0), Some(step));
        assert_eq!(verify_staff_totp(secret, &code(step - 1), now, 0), Some(step - 1));
        assert_eq!(verify_staff_totp(secret, &code(step + 1), now, 0), Some(step + 1));
        assert_eq!(verify_staff_totp(secret, &code(step - 2), now, 0), None);
        // already used
        assert_eq!(verify_staff_totp(secret, &code(step), now, step), None);
        assert_eq!(verify_staff_totp(secret, "12345", now, 0), None);
        assert_eq!(verify_staff_totp(secret, "abcdef", now, 0), None);
    }

    #[test]
    fn tokens_are_hashed() {
        let token = new_staff_token();
        assert_eq!(token.len(), 64);
        let hash = staff_token_hash(&token);
        assert_eq!(hash.len(), 64);
        assert_ne!(hash, token);
        assert_ne!(staff_token_hash(&new_staff_token()), hash);
    }
}
