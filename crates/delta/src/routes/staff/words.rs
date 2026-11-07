//! The platform-wide word list, kept by the Zrzur team.
//!
//! - `platform.reports.read`: see the list
//! - `platform.reports.resolve`: add or remove entries; each change needs a
//!   reason and goes to the platform audit log

use revolt_database::{record_platform_audit, AuditOutcome, Database, StaffPermission};
use revolt_models::v0::{DataPlatformWord, PlatformWord};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use validator::Validate;

use super::{reason, DataReason, StaffContext};

/// # The word list
#[get("/words")]
pub async fn list(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<PlatformWord>>> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    Ok(Json(db.fetch_platform_words().await?))
}

#[derive(Deserialize)]
pub struct DataAddWord {
    #[serde(flatten)]
    pub word: DataPlatformWord,
    pub reason: String,
}

#[derive(Serialize)]
pub struct WordCheck {
    /// What the list would say about this text
    pub category: Option<revolt_models::v0::PlatformWordCategory>,
}

#[derive(Deserialize)]
pub struct DataWordCheck {
    pub text: String,
}

/// # Add a word
#[post("/words", data = "<data>")]
pub async fn add(
    db: &State<Database>,
    staff: StaffContext,
    data: Json<DataAddWord>,
) -> Result<Json<PlatformWord>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    data.word.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let pattern = data.word.pattern.trim().to_string();
    if pattern.trim_matches('*').trim().is_empty() {
        return Err(create_error!(FailedValidation {
            error: "empty pattern".to_string()
        }));
    }
    if db
        .fetch_platform_words()
        .await?
        .iter()
        .any(|word| word.pattern == pattern && word.allow == data.word.allow)
    {
        return Err(create_error!(NoEffect));
    }
    let word = PlatformWord {
        id: Ulid::new().to_string(),
        pattern,
        category: data.word.category,
        allow: data.word.allow,
        note: data.word.note,
        added_by: staff.user.id.clone(),
        created_at: staff.now,
    };
    let mut record = staff.record("platform.words.add", Some(StaffPermission::ReportsResolve));
    record.target_kind = Some("platform_word".to_string());
    record.target_id = Some(word.id.clone());
    record.reason = Some(reason(&data.reason)?);
    record.summary = Some(word.pattern.clone());
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    db.insert_platform_word(&word).await?;
    Ok(Json(word))
}

/// # Remove a word
#[delete("/words/<id>", data = "<data>")]
pub async fn remove(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataReason>,
) -> Result<rocket_empty::EmptyResponse> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let mut record = staff.record("platform.words.remove", Some(StaffPermission::ReportsResolve));
    record.target_kind = Some("platform_word".to_string());
    record.target_id = Some(id.to_string());
    record.reason = Some(reason(&data.reason)?);
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    db.delete_platform_word(id).await?;
    Ok(rocket_empty::EmptyResponse)
}

/// # Try a text against the list
#[post("/words/check", data = "<data>")]
pub async fn check(
    db: &State<Database>,
    staff: StaffContext,
    data: Json<DataWordCheck>,
) -> Result<Json<WordCheck>> {
    staff.require(db, StaffPermission::ReportsRead).await?;
    Ok(Json(WordCheck {
        category: revolt_database::check_platform_words(db, &data.text).await?,
    }))
}
