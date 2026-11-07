//! Policy updates in the admin console. A published change reaches every
//! user who hasn't acknowledged it yet: the app shows it on the next
//! connection and asks them to read and acknowledge it
//! (`POST /policy/acknowledge`).
//!
//! - `platform.settings.read`: see published changes
//! - `platform.settings.manage` + a fresh step-up: publish or withdraw
//!
//! The message is required in Arabic, English and German. Every change is
//! written to the platform audit log first.

use std::collections::HashMap;

use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{record_platform_audit, AuditOutcome, Database, PolicyChange, StaffPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::Deserialize;
use ulid::Ulid;

use super::StaffContext;

const LANGUAGES: [&str; 3] = ["ar", "en", "de"];

fn invalid(error: &str) -> revolt_result::Error {
    create_error!(FailedValidation {
        error: error.to_string()
    })
}

/// # Policy changes
///
/// Newest first.
#[get("/policies")]
pub async fn list(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<PolicyChange>>> {
    staff.require(db, StaffPermission::SettingsRead).await?;
    let mut changes = db.fetch_policy_changes().await?;
    changes.sort_by(|a, b| b.created_time.cmp(&a.created_time));
    Ok(Json(changes))
}

#[derive(Deserialize)]
pub struct DataPolicyChange {
    /// The message by language: `ar`, `en` and `de`, 3 to 300 characters
    pub messages: HashMap<String, String>,
    /// Page with the full text (https)
    pub url: String,
    /// Days until it takes effect, 0 to 90
    #[serde(default)]
    pub effective_in_days: u32,
}

/// # Publish a policy change
#[post("/policies", data = "<data>")]
pub async fn publish(
    db: &State<Database>,
    staff: StaffContext,
    data: Json<DataPolicyChange>,
) -> Result<Json<PolicyChange>> {
    staff.require_step_up(db, StaffPermission::SettingsManage).await?;
    let data = data.into_inner();
    let mut translations = HashMap::new();
    for language in LANGUAGES {
        let text = data.messages.get(language).map(|t| t.trim()).unwrap_or_default();
        let length = text.chars().count();
        if !(3..=300).contains(&length) {
            return Err(invalid("the message is needed in ar, en and de, 3 to 300 characters each"));
        }
        translations.insert(language.to_string(), text.to_string());
    }
    if data.messages.keys().any(|k| !LANGUAGES.contains(&k.as_str())) {
        return Err(invalid("unknown language"));
    }
    let url = data.url.trim();
    if !url.starts_with("https://") || url.len() > 300 || url.chars().any(char::is_whitespace) {
        return Err(invalid("the link must be an https address"));
    }
    if data.effective_in_days > 90 {
        return Err(invalid("effective in 0 to 90 days"));
    }
    let now = Timestamp::now_utc();
    let change = PolicyChange {
        id: Ulid::new().to_string(),
        created_time: now,
        effective_time: now + Duration::days(data.effective_in_days as i64),
        description: translations["en"].clone(),
        url: url.to_string(),
        translations,
        created_by: Some(staff.user.id.clone()),
    };
    let mut record = staff.record("staff.policy.publish", Some(StaffPermission::SettingsManage));
    record.target_kind = Some("policy_change".to_string());
    record.target_id = Some(change.id.clone());
    record.summary = Some(format!("in {} days: {}", data.effective_in_days, change.url));
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    if let Err(error) = db.insert_policy_change(&change).await {
        let mut failed = record;
        failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
        return Err(error);
    }
    Ok(Json(change))
}

#[derive(Deserialize)]
pub struct DataWithdrawPolicy {
    pub reason: String,
}

/// # Withdraw a policy change
///
/// People who haven't acknowledged it stop seeing it; it can't be undone.
#[post("/policies/<id>/withdraw", data = "<data>")]
pub async fn withdraw(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataWithdrawPolicy>,
) -> Result<()> {
    staff.require_step_up(db, StaffPermission::SettingsManage).await?;
    let why = super::reason(&data.reason)?;
    if !db.fetch_policy_changes().await?.iter().any(|c| c.id == id) {
        return Err(create_error!(NotFound));
    }
    let mut record = staff.record("staff.policy.withdraw", Some(StaffPermission::SettingsManage));
    record.target_kind = Some("policy_change".to_string());
    record.target_id = Some(id.to_string());
    record.reason = Some(why);
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    if let Err(error) = db.delete_policy_change(id).await {
        let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
        return Err(error);
    }
    Ok(())
}
