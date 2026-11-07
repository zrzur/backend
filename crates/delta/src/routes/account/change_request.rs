//! Confirming an email or two-factor change that Zrzur support asked for.
//! The link goes to the account's current email; holding it is the proof.
//! It works once and for 24 hours, and nothing changes unless the person
//! confirms.

use revolt_database::{
    staff_token_hash, util::email::normalise_email, AccountChangeRequest, AccountChangeStatus,
    Database, Totp,
};
use revolt_models::v0::{AccountSecurityChangeView, DataAnswerChangeRequest};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::routes::users::change_requests::status_of;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

async fn by_token(db: &Database, token: &str) -> Result<AccountChangeRequest> {
    // short or odd tokens never match; don't hint which part was wrong
    if token.len() < 32 || token.len() > 128 {
        return Err(create_error!(NotFound));
    }
    db.fetch_account_change_request_by_token(&staff_token_hash(token))
        .await?
        .ok_or_else(|| create_error!(NotFound))
}

fn view(request: &AccountChangeRequest, now: i64) -> AccountSecurityChangeView {
    AccountSecurityChangeView {
        kind: if request.changes.reset_totp { "two_factor" } else { "email" }.to_string(),
        new_email: request.changes.email.clone(),
        reason: request.reason.clone(),
        status: status_of(request, now).to_string(),
        expires_at: request.expires_at,
    }
}

/// # Account change from an email link
///
/// What Zrzur support asked to change.
#[openapi(tag = "Account")]
#[get("/change-request/<token>")]
pub async fn fetch(db: &State<Database>, token: &str) -> Result<Json<AccountSecurityChangeView>> {
    let request = by_token(db, token).await?;
    Ok(Json(view(&request, now_ms())))
}

/// # Confirm or decline an account change
///
/// Confirming a two-factor reset turns two-factor authentication off and
/// signs the account out everywhere. Confirming an email change sends a
/// verification link to the new address; the email changes once that link
/// is opened.
#[openapi(tag = "Account")]
#[post("/change-request/<token>", data = "<data>")]
pub async fn answer(
    db: &State<Database>,
    token: &str,
    data: Json<DataAnswerChangeRequest>,
) -> Result<Json<AccountSecurityChangeView>> {
    let now = now_ms();
    let mut request = by_token(db, token).await?;
    if status_of(&request, now) != "Pending" {
        return Err(create_error!(InvalidOperation));
    }
    let mut account = db.fetch_account(&request.user_id).await?;
    if data.accept {
        if let Some(email) = &request.changes.email {
            // someone may have taken the address since the request
            if db
                .fetch_account_by_normalised_email(&normalise_email(email.clone()))
                .await?
                .is_some_and(|other| other.id != account.id)
            {
                return Err(revolt_result::failed_validation(vec![
                    revolt_result::ValidationProblem::new("email", "taken"),
                ]));
            }
        }
    }
    request.status = if data.accept {
        AccountChangeStatus::Accepted
    } else {
        AccountChangeStatus::Declined
    };
    request.decided_at = Some(now);
    // a link opened twice: only the first answer counts
    if !db
        .decide_account_change_request(&request, AccountChangeStatus::Pending)
        .await?
    {
        return Err(create_error!(InvalidOperation));
    }
    if data.accept {
        let applied: Result<()> = async {
            if request.changes.reset_totp {
                account.mfa.totp_token = Totp::Disabled;
                account.mfa.recovery_codes = vec![];
                account.save(db).await?;
                // whoever had the old device is signed out too
                account.delete_all_sessions(db, None).await?;
            } else if let Some(email) = request.changes.email.clone() {
                account.start_email_move(db, email).await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = applied {
            // not applied: the link stays usable
            let mut pending = request.clone();
            pending.status = AccountChangeStatus::Pending;
            pending.decided_at = None;
            db.decide_account_change_request(&pending, AccountChangeStatus::Accepted)
                .await?;
            return Err(error);
        }
    }
    Ok(Json(view(&request, now)))
}
