use revolt_database::{Database, Session};
use revolt_result::{ErrorType, Result};
use rocket::State;

use rocket::serde::json::Json;
use serde::Serialize;

/// # Onboarding Status
#[derive(Serialize, JsonSchema)]
pub struct DataHello {
    /// Whether onboarding is required
    onboarding: bool,
}

/// # Check Onboarding Status
///
/// This will tell you whether the current account requires onboarding or whether you can continue to send requests as usual. You may skip calling this if you're restoring an existing session.
#[openapi(tag = "Onboarding")]
#[get("/hello")]
pub async fn hello(db: &State<Database>, session: Session) -> Result<Json<DataHello>> {
    // no user row yet is a new account; a failing lookup is an error, not
    // "onboarding needed" (the app would then try to create a second user)
    let onboarding = match db.fetch_user(&session.user_id).await {
        Ok(_) => false,
        Err(error) if matches!(error.error_type, ErrorType::NotFound) => true,
        Err(error) => return Err(error),
    };
    Ok(Json(DataHello { onboarding }))
}
