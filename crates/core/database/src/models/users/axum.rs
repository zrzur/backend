use axum::{extract::{FromRef, FromRequestParts}, http::request::Parts};

use revolt_result::{create_error, Error, Result};

use crate::{Database, Session, User};

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for User
where
    Database: FromRef<S>,
    S: Send + Sync
{
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<User> {
        let db = Database::from_ref(state);

        if let Some(Ok(bot_token)) = parts.headers.get("x-bot-token").map(|v| v.to_str()) {
            // a token nobody knows (reset, or never valid) is an invalid
            // session, as in the API, not a missing file
            let bot = db
                .fetch_bot_by_token(bot_token)
                .await
                .map_err(User::token_lookup_error)?;
            // a disabled bot, or one whose owner was suspended or banned
            if !crate::bot_may_act(&db, &bot).await? {
                return Err(create_error!(InvalidSession));
            }
            Ok(db
                .fetch_user(&bot.id)
                .await
                .map_err(User::token_lookup_error)?
                .confirm_privileged(&db, None)
                .await)
        } else if let Some(Ok(session_token)) =
            parts.headers.get("x-session-token").map(|v| v.to_str())
        {
            let session = Session::from_token(&db, session_token)
                .await
                .map_err(User::token_lookup_error)?;
            Ok(db
                .fetch_user(&session.user_id)
                .await?
                .confirm_privileged(&db, Some(&session.id))
                .await)
        } else {
            Err(create_error!(NotAuthenticated))
        }
    }
}
