//! One way to make a staff change: the Success record first, then the
//! change; a change that fails afterwards gets a second record, Failed.

use std::future::Future;

use revolt_database::{record_platform_audit, AuditOutcome, AuditRecord, Database};
use revolt_result::Result;

/// Longest error text kept in the Failed record
const FAILED_SUMMARY_MAX: usize = 200;

/// Write the Success record, then run `change`.
///
/// If the first write fails, nothing happens and the error comes back. If
/// `change` fails, a second record (Failed, `summary` = the error type,
/// 200 characters at most) is written best effort and the error returned.
pub(crate) async fn audited<T, F: Future<Output = Result<T>>>(
    db: &Database,
    record: AuditRecord,
    change: F,
) -> Result<T> {
    audited_noting(db, record, async { change.await.map_err(|error| (error, None)) }).await
}

/// Like [`audited`], but the change may add a short note (a fixed text, no
/// secrets) that goes after the error type in the Failed record, e.g.
/// "revert failed".
pub(crate) async fn audited_noting<T, F>(db: &Database, record: AuditRecord, change: F) -> Result<T>
where
    F: Future<Output = std::result::Result<T, (revolt_result::Error, Option<&'static str>)>>,
{
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    match change.await {
        Ok(value) => Ok(value),
        Err((error, note)) => {
            let mut failed = record;
            let mut summary = failed_summary(&error);
            if let Some(note) = note {
                summary = format!("{summary}; {note}");
            }
            failed.summary = Some(summary.chars().take(FAILED_SUMMARY_MAX).collect());
            // best effort: the first record already shows the attempt
            let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
            Err(error)
        }
    }
}

fn failed_summary(error: &revolt_result::Error) -> String {
    // the variant name only: the payload may hold user text
    let text = format!("{:?}", error.error_type);
    text.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .take(FAILED_SUMMARY_MAX)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test::TestHarness;
    use revolt_database::PlatformAuditQuery;
    use revolt_result::create_error;

    fn record(action: &str) -> AuditRecord {
        AuditRecord {
            actor_id: "actor".to_string(),
            action: action.to_string(),
            permission: None,
            target_kind: None,
            target_id: None,
            reason: None,
            staff_session_id: None,
            summary: None,
        }
    }

    async fn outcomes(db: &Database, action: &str) -> Vec<(String, Option<String>)> {
        let mut entries: Vec<_> = db
            .fetch_platform_audit(PlatformAuditQuery { limit: 100, ..Default::default() })
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.action == action)
            .collect();
        entries.sort_by_key(|e| e.seq);
        entries
            .into_iter()
            .map(|e| (format!("{:?}", e.outcome), e.summary))
            .collect()
    }

    #[rocket::async_test]
    async fn failed_change_writes_failed_record() {
        let harness = TestHarness::new().await;
        let result: Result<()> =
            audited(&harness.db, record("test.audited.fail"), async { Err(create_error!(NotFound)) }).await;
        assert!(result.is_err());
        let seen = outcomes(&harness.db, "test.audited.fail").await;
        assert_eq!(
            seen,
            vec![
                ("Success".to_string(), None),
                ("Failed".to_string(), Some("NotFound".to_string()))
            ]
        );
    }

    #[rocket::async_test]
    async fn a_change_that_works_writes_one_record() {
        let harness = TestHarness::new().await;
        let value = audited(&harness.db, record("test.audited.ok"), async { Ok(7) }).await.unwrap();
        assert_eq!(value, 7);
        assert_eq!(outcomes(&harness.db, "test.audited.ok").await.len(), 1);
    }

    #[test]
    fn failed_summary_is_the_error_type_and_short() {
        let summary = failed_summary(&create_error!(NotFound));
        assert_eq!(summary, "NotFound");
        let long = create_error!(FailedValidation { error: "secret text".repeat(50) });
        assert_eq!(failed_summary(&long), "FailedValidation");
    }
}
