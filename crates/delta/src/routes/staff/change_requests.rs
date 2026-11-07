//! Asking a user to agree to a change of their profile, from the admin
//! console. Nothing changes until the user accepts in the app; unanswered
//! requests expire after 14 days.
//!
//! - `platform.users.read`: see the requests about a user
//! - `platform.support.manage`: ask for a change, withdraw a request
//!
//! For rule breaks (an offensive name or picture) use Trust & Safety
//! instead: those don't wait for consent.

use revolt_config::config;
use revolt_database::{
    record_platform_audit, staff_token_hash,
    util::email::{email_templates, normalise_email, send_email, validate_email},
    AccountChangeRequest, AccountChangeStatus, AccountChanges, AuditOutcome, Database,
    StaffPermission, ACCOUNT_CHANGE_MAX_OPEN, ACCOUNT_CHANGE_REMOVABLE, ACCOUNT_CHANGE_TTL_MS,
    ACCOUNT_SECURITY_CHANGE_TTL_MS,
};
use revolt_models::v0::AccountChangeRequestView;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use validator::Validate;

use super::{reason, StaffContext};
use crate::routes::users::change_requests::{as_edit, status_of, view};

#[derive(Serialize)]
pub struct StaffChangeRequest {
    #[serde(flatten)]
    pub request: AccountChangeRequestView,
    /// New email of an email change (only with `users.pii.read`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub reset_totp: bool,
    pub requested_by: String,
    pub decided_at: Option<i64>,
}

fn staff_view(request: &AccountChangeRequest, now: i64) -> StaffChangeRequest {
    StaffChangeRequest {
        request: view(request, now),
        email: request.changes.email.clone(),
        reset_totp: request.changes.reset_totp,
        requested_by: request.requested_by.clone(),
        decided_at: request.decided_at,
    }
}

/// # Change requests about a user
#[get("/users/<id>/change-requests")]
pub async fn list(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<Vec<StaffChangeRequest>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let pii = staff.has(StaffPermission::UsersPiiRead);
    Ok(Json(
        db.fetch_account_change_requests_for_user(id)
            .await?
            .iter()
            .map(|r| {
                let mut v = staff_view(r, staff.now);
                // a new email address is personal data
                if !pii {
                    v.email = v.email.map(|_| "•••".to_string());
                }
                v
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct DataChangeRequest {
    pub changes: AccountChanges,
    /// Shown to the user
    pub reason: String,
}

fn trimmed(value: &Option<String>) -> Option<String> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

/// # Ask a user to agree to a change
#[post("/users/<id>/change-requests", data = "<data>")]
pub async fn create(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataChangeRequest>,
) -> Result<Json<StaffChangeRequest>> {
    staff.require(db, StaffPermission::SupportManage).await?;
    let data = data.into_inner();
    let why = reason(&data.reason)?;
    let subject = db.fetch_user(id).await?;
    if subject.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if subject.id == staff.user.id {
        return Err(create_error!(CannotReportYourself));
    }

    let mut remove: Vec<String> = vec![];
    for field in &data.changes.remove {
        if !ACCOUNT_CHANGE_REMOVABLE.contains(&field.as_str()) {
            return Err(create_error!(FailedValidation {
                error: format!("can't clear {field}")
            }));
        }
        if !remove.contains(field) {
            remove.push(field.clone());
        }
    }
    let changes = AccountChanges {
        display_name: trimmed(&data.changes.display_name),
        pronouns: trimmed(&data.changes.pronouns),
        status_text: trimmed(&data.changes.status_text),
        bio: trimmed(&data.changes.bio),
        remove,
        // email and two-factor go through /security-requests (emailed link)
        email: None,
        reset_totp: false,
    };
    if changes == AccountChanges::default() {
        return Err(create_error!(FailedValidation {
            error: "nothing to change".to_string()
        }));
    }
    if changes.display_name.as_deref().is_some_and(revolt_database::passes_for_official) {
        return Err(create_error!(InvalidUsername));
    }
    let request = AccountChangeRequest {
        id: Ulid::new().to_string(),
        user_id: subject.id.clone(),
        requested_by: staff.user.id.clone(),
        reason: why.clone(),
        changes,
        status: AccountChangeStatus::Pending,
        created_at: staff.now,
        expires_at: staff.now + ACCOUNT_CHANGE_TTL_MS,
        decided_at: None,
        token_hash: None,
    };
    // the same rules as the user editing their own profile
    let edit = as_edit(&request);
    edit.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    if let Some(bio) = &request.changes.bio {
        if bio.chars().count() > 2000 {
            return Err(create_error!(FailedValidation {
                error: "bio too long".to_string()
            }));
        }
    }
    let open = db
        .fetch_account_change_requests_for_user(&subject.id)
        .await?
        .iter()
        .filter(|r| status_of(r, staff.now) == "Pending")
        .count();
    if open >= ACCOUNT_CHANGE_MAX_OPEN {
        return Err(create_error!(InvalidOperation));
    }

    let mut record = staff.record("staff.user.change_request", Some(StaffPermission::SupportManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(subject.id.clone());
    record.reason = Some(why);
    record.summary = Some(summary(&request.changes));
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    db.insert_account_change_request(&request).await?;
    crate::util::official::change_request(db, &subject.id).await;
    Ok(Json(staff_view(&request, staff.now)))
}

fn summary(changes: &AccountChanges) -> String {
    let mut parts = vec![];
    if changes.display_name.is_some() {
        parts.push("display_name".to_string());
    }
    if changes.pronouns.is_some() {
        parts.push("pronouns".to_string());
    }
    if changes.status_text.is_some() {
        parts.push("status_text".to_string());
    }
    if changes.bio.is_some() {
        parts.push("bio".to_string());
    }
    for field in &changes.remove {
        parts.push(format!("clear:{field}"));
    }
    parts.join(" ")
}

/// # Withdraw a change request
#[post("/change-requests/<id>/cancel")]
pub async fn cancel(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<StaffChangeRequest>> {
    staff.require(db, StaffPermission::SupportManage).await?;
    let mut request = db.fetch_account_change_request(id).await?;
    if status_of(&request, staff.now) != "Pending" {
        return Err(create_error!(InvalidOperation));
    }
    let mut record = staff.record("staff.user.change_request.cancel", Some(StaffPermission::SupportManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(request.user_id.clone());
    record.summary = Some(request.id.clone());
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    request.status = AccountChangeStatus::Cancelled;
    request.decided_at = Some(staff.now);
    if !db
        .decide_account_change_request(&request, AccountChangeStatus::Pending)
        .await?
    {
        return Err(create_error!(InvalidOperation));
    }
    Ok(Json(staff_view(&request, staff.now)))
}

#[derive(Deserialize)]
pub struct DataSecurityChange {
    /// "email" or "two_factor"
    pub kind: String,
    /// The new email (email changes)
    pub new_email: Option<String>,
    /// Shown to the user in the email
    pub reason: String,
}

/// # Ask to change a user's email or turn off two-factor
///
/// For someone who lost their authenticator, or needs a new sign-in email.
/// A link goes to the account's current email; nothing changes unless
/// they confirm there within 24 hours. Needs a fresh step-up, and an email
/// server: without one the link can't be delivered and the request is
/// refused.
#[post("/users/<id>/security-requests", data = "<data>")]
pub async fn security(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataSecurityChange>,
) -> Result<Json<StaffChangeRequest>> {
    staff.require_step_up(db, StaffPermission::SupportManage).await?;
    let data = data.into_inner();
    let why = reason(&data.reason)?;
    let subject = db.fetch_user(id).await?;
    if subject.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if subject.id == staff.user.id {
        return Err(create_error!(CannotReportYourself));
    }
    let account = db.fetch_account(&subject.id).await?;

    let mut changes = AccountChanges::default();
    let (what, what_ar, what_de) = match data.kind.as_str() {
        "two_factor" => {
            if !account.mfa.is_active() {
                return Err(create_error!(InvalidOperation));
            }
            changes.reset_totp = true;
            (
                "Turn off two-factor authentication".to_string(),
                "إيقاف التحقق بخطوتين".to_string(),
                "Zwei-Faktor-Authentifizierung ausschalten".to_string(),
            )
        }
        "email" => {
            let email = data.new_email.as_deref().map(str::trim).unwrap_or_default().to_string();
            validate_email(&email)?;
            if db
                .fetch_account_by_normalised_email(&normalise_email(email.clone()))
                .await?
                .is_some()
            {
                return Err(create_error!(FailedValidation {
                    error: "email already used".to_string()
                }));
            }
            changes.email = Some(email.clone());
            (
                format!("Change your sign-in email to {email}"),
                format!("تغيير بريد تسجيل الدخول إلى {email}"),
                format!("Deine Anmelde-E-Mail in {email} ändern"),
            )
        }
        _ => return Err(create_error!(InvalidOperation)),
    };

    let smtp = &config().await.api.smtp;
    if smtp.host.is_empty() {
        // the confirmation link could never arrive
        return Err(create_error!(InvalidOperation));
    }
    let open = db
        .fetch_account_change_requests_for_user(&subject.id)
        .await?
        .iter()
        .filter(|r| status_of(r, staff.now) == "Pending")
        .count();
    if open >= ACCOUNT_CHANGE_MAX_OPEN {
        return Err(create_error!(InvalidOperation));
    }

    let token = nanoid::nanoid!(48);
    if changes.display_name.as_deref().is_some_and(revolt_database::passes_for_official) {
        return Err(create_error!(InvalidUsername));
    }
    let request = AccountChangeRequest {
        id: Ulid::new().to_string(),
        user_id: subject.id.clone(),
        requested_by: staff.user.id.clone(),
        reason: why.clone(),
        changes,
        status: AccountChangeStatus::Pending,
        created_at: staff.now,
        expires_at: staff.now + ACCOUNT_SECURITY_CHANGE_TTL_MS,
        decided_at: None,
        token_hash: Some(staff_token_hash(&token)),
    };
    let mut record = staff.record("staff.user.security_request", Some(StaffPermission::SupportManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(subject.id.clone());
    record.reason = Some(why.clone());
    // the new address is personal data: the log says what kind of change only
    record.summary = Some(data.kind.clone());
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    db.insert_account_change_request(&request).await?;

    let template = email_templates().await.account_change;
    let sent = send_email(
        smtp,
        account.email.clone(),
        &template,
        serde_json::json!({
            "url": format!("{}{}", template.url, token),
            "what": what,
            "what_ar": what_ar,
            "what_de": what_de,
            "reason": why,
        }),
    );
    if let Err(error) = sent {
        // the link never arrived: the request must not stay open
        let mut cancelled = request.clone();
        cancelled.status = AccountChangeStatus::Cancelled;
        cancelled.decided_at = Some(staff.now);
        db.decide_account_change_request(&cancelled, AccountChangeStatus::Pending)
            .await?;
        record.summary = Some(format!("{} (email failed)", data.kind));
        let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
        return Err(error);
    }
    Ok(Json(staff_view(&request, staff.now)))
}
