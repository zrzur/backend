//! Trust & Safety storage: cases, evidence, audit trail, actions, appeals.
//!
//! The document types live in `revolt_models::v0` (they carry no secrets
//! and the reviewer API returns them as they are); this module only adds
//! storage and the in-memory store for the reference driver.

mod ops;

pub use ops::*;

use revolt_models::v0::{SafetyAction, SafetyAppeal, SafetyCase, SafetyEvidence, SafetyLogEntry};

/// How long evidence is kept after a report, unless a hold is placed
pub const SAFETY_EVIDENCE_RETENTION_MS: i64 = 180 * 24 * 60 * 60 * 1000;

/// Reports one user may file per day (urgent categories are never blocked)
pub const SAFETY_DAILY_REPORT_LIMIT: usize = 30;

/// `BotFlags::Suspended`
pub const BOT_FLAG_SUSPENDED: i32 = revolt_models::v0::BotFlags::Suspended as i32;

/// Whether Trust & Safety disabled this bot (it can't authenticate)
pub fn bot_suspended(bot: &crate::Bot) -> bool {
    bot.flags.unwrap_or_default() & BOT_FLAG_SUSPENDED != 0
}

/// Whether a bot may be handed people's data: not when it was disabled, or
/// when its owner was suspended, banned or removed as spam.
pub async fn bot_may_receive(db: &crate::Database, bot: &crate::Bot) -> bool {
    use revolt_models::v0::UserFlags;
    if bot_suspended(bot) {
        return false;
    }
    let blocked =
        UserFlags::SuspendedUntil as i32 | UserFlags::Banned as i32 | UserFlags::Spam as i32;
    db.fetch_user(&bot.owner)
        .await
        .is_ok_and(|owner| owner.flags.unwrap_or_default() & blocked == 0)
}

/// Milliseconds since the epoch
pub fn safety_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Lift account suspensions whose time ran out. The action stays in the
/// user's history, marked as expired by the system.
pub async fn expire_safety_suspensions(db: &crate::Database) -> revolt_result::Result<usize> {
    use revolt_models::v0::{SafetyActionKind, SafetyRevocation};

    let expired = db
        .fetch_expired_safety_actions(SafetyActionKind::SuspendAccount, safety_now())
        .await?;
    let count = expired.len();
    for mut action in expired {
        if let Some(subject) = &action.subject_user_id
            && let Ok(mut user) = db.fetch_user(subject).await
            && user.suspended_until.is_some()
        {
            user.unsuspend(db).await?;
        }
        action.revoked = Some(SafetyRevocation {
            actor_id: "system".to_string(),
            reason: "expired".to_string(),
            at: safety_now(),
        });
        db.save_safety_action(&action).await?;
    }
    Ok(count)
}

/// In-memory store used by the reference driver
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustSafetyStore {
    pub cases: Vec<SafetyCase>,
    pub evidence: Vec<SafetyEvidence>,
    pub log: Vec<SafetyLogEntry>,
    pub actions: Vec<SafetyAction>,
    pub appeals: Vec<SafetyAppeal>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

    use revolt_models::v0::{
        SafetyCase, SafetyCategory, SafetyEvidence, SafetyPriority, SafetyRoute, SafetyStatus,
        SafetyTarget, SafetyTargetKind,
    };

    fn case(id: &str, category: SafetyCategory, subcategory: Option<&str>) -> SafetyCase {
        SafetyCase {
            id: id.into(),
            reference: format!("ZR-{id}"),
            reporter_id: "reporter".into(),
            public_reporter: None,
            target: SafetyTarget { kind: SafetyTargetKind::Message, id: "m".into(), facet: None, message_id: None },
            target_owner_id: None,
            server_id: None,
            channel_id: None,
            channel_type: None,
            route: SafetyRoute::Platform,
            category,
            subcategory: subcategory.map(str::to_string),
            description: String::new(),
            evidence_ids: vec![],
            status: SafetyStatus::Queued,
            priority: SafetyPriority::Normal,
            assignee_id: None,
            resolution: None,
            duplicate_of: None,
            info_request: None,
            policy_version: "test".into(),
            created_at: 0,
            updated_at: 0,
            closed_at: None,
            version: 0,
            legal_hold: false,
        }
    }

    fn evidence(id: &str, case_id: &str, expires_at: i64) -> SafetyEvidence {
        SafetyEvidence {
            id: id.into(),
            case_id: case_id.into(),
            kind: SafetyTargetKind::Message,
            object_id: "m".into(),
            captured_at: 0,
            author_id: None,
            title: None,
            content: Some("content".into()),
            files: vec![],
            context: vec![],
            metadata: BTreeMap::new(),
            expires_at: Some(expires_at),
            legal_hold: false,
        }
    }

    /// Evidence of child sexual abuse material reports is not purged;
    /// other
    /// evidence is purged when its time is up.
    #[tokio::test]
    async fn the_purge_keeps_evidence_of_child_abuse_reports() {
        database_test!(|db| async move {
            let suffix = ulid::Ulid::new().to_string();
            let csam = format!("csam-{suffix}");
            let normal = format!("normal-{suffix}");
            db.insert_safety_case(&case(&csam, SafetyCategory::SexualContent, Some("minor_safety")))
                .await
                .unwrap();
            db.insert_safety_case(&case(&normal, SafetyCategory::Spam, None)).await.unwrap();
            let minor = format!("minor-{suffix}");
            db.insert_safety_case(&case(&minor, SafetyCategory::Underage, None)).await.unwrap();
            db.insert_safety_evidence(&evidence(&format!("e-minor-{suffix}"), &minor, 1)).await.unwrap();
            // a hold on the case protects evidence that is not marked itself
            let held = format!("held-{suffix}");
            let mut held_case = case(&held, SafetyCategory::Spam, None);
            held_case.legal_hold = true;
            db.insert_safety_case(&held_case).await.unwrap();
            db.insert_safety_evidence(&evidence(&format!("e-held-{suffix}"), &held, 1)).await.unwrap();
            let (kept, purged) = (format!("e-kept-{suffix}"), format!("e-purged-{suffix}"));
            db.insert_safety_evidence(&evidence(&kept, &csam, 1)).await.unwrap();
            db.insert_safety_evidence(&evidence(&purged, &normal, 1)).await.unwrap();

            db.purge_expired_safety_evidence(i64::MAX / 2).await.unwrap();

            let left_csam = db.fetch_safety_evidence(&csam).await.unwrap();
            let left_normal = db.fetch_safety_evidence(&normal).await.unwrap();
            assert_eq!(left_csam.len(), 1, "evidence of a child abuse report must survive the purge");
            assert!(left_normal.is_empty(), "other evidence is purged");
            assert_eq!(db.fetch_safety_evidence(&minor).await.unwrap().len(), 1, "under-age reports are kept too");
            assert_eq!(db.fetch_safety_evidence(&held).await.unwrap().len(), 1, "a case under legal hold keeps its evidence");
        });
    }

    /// Names and e-mail addresses of people who reported without an account
    /// go 12 months after the case closed; open and recent cases keep them.
    #[tokio::test]
    async fn contact_details_of_public_reporters_are_cleared_after_a_year() {
        use revolt_models::v0::SafetyPublicReporter;
        database_test!(|db| async move {
            let suffix = ulid::Ulid::new().to_string();
            let reporter = || SafetyPublicReporter {
                name: Some("Sam".into()),
                email: Some("sam@example.org".into()),
                location: "link".into(),
                law: None,
                language: "en".into(),
                good_faith: true,
            };
            let mut old = case(&format!("old-{suffix}"), SafetyCategory::Spam, None);
            old.public_reporter = Some(reporter());
            old.closed_at = Some(1_000);
            old.status = SafetyStatus::Closed;
            let mut recent = case(&format!("recent-{suffix}"), SafetyCategory::Spam, None);
            recent.public_reporter = Some(reporter());
            recent.closed_at = Some(9_000_000_000_000);
            recent.status = SafetyStatus::Closed;
            // decided by an action (no closed_at): counts from the decision
            let mut acted = case(&format!("acted-{suffix}"), SafetyCategory::Spam, None);
            acted.public_reporter = Some(reporter());
            acted.status = SafetyStatus::ActionTaken;
            acted.resolution = Some(revolt_models::v0::SafetyResolution {
                decided_by: "r".into(),
                decided_at: 1_000,
                public_note: None,
                action_ids: vec![],
            });
            // decided long ago but closed recently: the closing counts
            let mut closed_late = case(&format!("late-{suffix}"), SafetyCategory::Spam, None);
            closed_late.public_reporter = Some(reporter());
            closed_late.status = SafetyStatus::Closed;
            closed_late.closed_at = Some(9_000_000_000_000);
            closed_late.resolution = acted.resolution.clone();
            // waiting on an appeal: not decided for good
            let mut appealed = case(&format!("appealed-{suffix}"), SafetyCategory::Spam, None);
            appealed.public_reporter = Some(reporter());
            appealed.status = SafetyStatus::Appealed;
            appealed.resolution = acted.resolution.clone();
            let mut open = case(&format!("open-{suffix}"), SafetyCategory::Spam, None);
            open.public_reporter = Some(reporter());
            for c in [&old, &recent, &open, &acted, &appealed, &closed_late] {
                db.insert_safety_case(c).await.unwrap();
            }
            db.clear_public_reporters(5_000).await.unwrap();
            let old = db.fetch_safety_case(&old.id).await.unwrap().public_reporter.unwrap();
            assert!(old.name.is_none() && old.email.is_none());
            assert_eq!(old.location, "link");
            let acted = db.fetch_safety_case(&acted.id).await.unwrap().public_reporter.unwrap();
            assert!(acted.email.is_none() && acted.name.is_none(), "cleared through the decision date");
            for id in [&recent.id, &open.id, &appealed.id, &closed_late.id] {
                let kept = db.fetch_safety_case(id).await.unwrap().public_reporter.unwrap();
                assert_eq!(kept.email.as_deref(), Some("sam@example.org"));
            }
            // safe to run twice
            assert_eq!(db.clear_public_reporters(5_000).await.unwrap(), 0);
        });
    }
}
