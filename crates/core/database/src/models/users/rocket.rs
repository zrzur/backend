use revolt_result::Error;
use rocket::http::Status;
use rocket::request::{self, FromRequest, Outcome, Request};

use crate::models::sessions::rocket::refusal;
use crate::{Database, Session, User};

#[rocket::async_trait]
impl<'r> FromRequest<'r> for User {
    type Error = Error;

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        // the user, or why there is none: a token nobody knows is an invalid
        // session, a failing database stays the database error
        let user: &Result<User, Error> = request
            .local_cache_async(async {
                let db = request.rocket().state::<Database>().expect("`Database`");

                let header_bot_token = request
                    .headers()
                    .get("x-bot-token")
                    .next()
                    .map(|x| x.to_string());

                if let Some(bot_token) = header_bot_token {
                    let bot = db
                        .fetch_bot_by_token(&bot_token)
                        .await
                        .map_err(User::token_lookup_error)?;
                    // a disabled bot, or one whose owner was suspended or
                    // banned, is refused like a token nobody knows
                    if !crate::bot_may_receive(db, &bot).await {
                        return Err(create_error!(InvalidSession));
                    }
                    let user = db
                        .fetch_user(&bot.id)
                        .await
                        .map_err(User::token_lookup_error)?;
                    Ok(user.confirm_privileged(db, None).await)
                } else {
                    match request.guard::<Session>().await {
                        // `privileged` needs an open staff session from this
                        // sign-in, not only the ordinary token
                        Outcome::Success(session) => {
                            let user = db
                                .fetch_user(&session.user_id)
                                .await
                                .map_err(User::token_lookup_error)?;
                            Ok(user.confirm_privileged(db, Some(&session.id)).await)
                        }
                        Outcome::Error((_, error)) => Err(error),
                        Outcome::Forward(_) => Err(create_error!(InvalidSession)),
                    }
                }
            })
            .await;

        if let Ok(user) = user {
            // the server refuses everything until an age is declared, not
            // only the app (PATCH /auth/account/age needs no User)
            let db = request.rocket().state::<Database>().expect("`Database`");
            if !crate::util::age::has_declared_age(db, user).await {
                let error = create_error!(AgeNotDeclared);
                request.local_cache(|| revolt_result::rocket::GuardRefusal(Some(error.clone())));
                return Outcome::Error((Status::Forbidden, error));
            }
            Outcome::Success(user.clone())
        } else {
            let error = user.as_ref().err().cloned().unwrap_or_else(|| create_error!(InvalidSession));
            Outcome::Error(refusal(request, error))
        }
    }
}

