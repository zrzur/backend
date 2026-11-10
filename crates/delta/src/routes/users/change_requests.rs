//! Account changes that wait for consent. Zrzur staff can ask to change
//! parts of your profile (from the admin console); nothing changes unless
//! you accept. Accepting applies the change through the same rules as
//! editing your profile yourself.

use revolt_database::{
    util::reference::Reference, AccountChangeRequest, AccountChangeStatus, Database, User,
};
use revolt_models::v0::{
    self, AccountChangeRequestView, AccountChangeSummary, DataAnswerChangeRequest, FieldsUser,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::edit_user;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Status as the user sees it (an unanswered request past its date expired)
pub(crate) fn status_of(request: &AccountChangeRequest, now: i64) -> &'static str {
    match request.status {
        AccountChangeStatus::Pending if now >= request.expires_at => "Expired",
        AccountChangeStatus::Pending => "Pending",
        AccountChangeStatus::Accepted => "Accepted",
        AccountChangeStatus::Declined => "Declined",
        AccountChangeStatus::Cancelled => "Cancelled",
    }
}

pub(crate) fn view(request: &AccountChangeRequest, now: i64) -> AccountChangeRequestView {
    let c = &request.changes;
    AccountChangeRequestView {
        id: request.id.clone(),
        reason: request.reason.clone(),
        changes: AccountChangeSummary {
            display_name: c.display_name.clone(),
            pronouns: c.pronouns.clone(),
            status_text: c.status_text.clone(),
            bio: c.bio.clone(),
            remove: c.remove.clone(),
        },
        status: status_of(request, now).to_string(),
        created_at: request.created_at,
        expires_at: request.expires_at,
    }
}

/// The profile edit the request stands for
pub(crate) fn as_edit(request: &AccountChangeRequest) -> v0::DataEditUser {
    let c = &request.changes;
    let remove = c
        .remove
        .iter()
        .filter_map(|field| match field.as_str() {
            "Avatar" => Some(FieldsUser::Avatar),
            "ProfileBackground" => Some(FieldsUser::ProfileBackground),
            "DisplayName" => Some(FieldsUser::DisplayName),
            "Pronouns" => Some(FieldsUser::Pronouns),
            "StatusText" => Some(FieldsUser::StatusText),
            "ProfileContent" => Some(FieldsUser::ProfileContent),
            _ => None,
        })
        .collect();
    v0::DataEditUser {
        display_name: c.display_name.clone(),
        pronouns: c.pronouns.clone(),
        avatar: None,
        status: c.status_text.as_ref().map(|text| v0::UserStatus {
            text: Some(text.clone()),
            ..Default::default()
        }),
        profile: c.bio.as_ref().map(|bio| v0::DataUserProfile {
            content: Some(bio.clone()),
            ..Default::default()
        }),
        badges: None,
        flags: None,
        style: None,
        remove,
    }
}

/// # Change requests
///
/// Changes to your account that Zrzur asked you to agree to and that are
/// still waiting for your answer.
#[openapi(tag = "User Information")]
#[get("/@me/change-requests")]
pub async fn list(db: &State<Database>, user: User) -> Result<Json<Vec<AccountChangeRequestView>>> {
    let now = now_ms();
    Ok(Json(
        db.fetch_account_change_requests_for_user(&user.id)
            .await?
            .iter()
            // email and two-factor changes are answered by the emailed link only
            .filter(|r| status_of(r, now) == "Pending" && r.token_hash.is_none())
            .map(|r| view(r, now))
            .collect(),
    ))
}

/// # Answer a change request
///
/// Accept (the change is applied now) or decline (nothing changes).
#[openapi(tag = "User Information")]
#[post("/@me/change-requests/<id>", data = "<data>")]
pub async fn answer(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataAnswerChangeRequest>,
) -> Result<Json<AccountChangeRequestView>> {
    let now = now_ms();
    let mut request = db.fetch_account_change_request(id).await?;
    // someone else's request looks like no request at all
    if request.user_id != user.id {
        return Err(create_error!(NotFound));
    }
    if status_of(&request, now) != "Pending" || request.token_hash.is_some() {
        return Err(create_error!(InvalidOperation));
    }
    let edit = as_edit(&request);
    request.status = if data.accept {
        AccountChangeStatus::Accepted
    } else {
        AccountChangeStatus::Declined
    };
    request.decided_at = Some(now);
    if !db
        .decide_account_change_request(&request, AccountChangeStatus::Pending)
        .await?
    {
        return Err(create_error!(InvalidOperation));
    }
    if data.accept {
        // same checks and events as editing your own profile
        edit_user::edit(
            db,
            user,
            Reference::from_unchecked("@me"),
            Json(edit),
        )
        .await?;
    }
    Ok(Json(view(&request, now)))
}
