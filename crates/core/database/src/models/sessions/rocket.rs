use crate::{Database, Session};
use revolt_result::Error;
use rocket::{
    Request,
    http::Status,
    request::{FromRequest, Outcome},
};

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Session {
    type Error = Error;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        request
            .local_cache_async(async {
                if let Some(token) = request.headers().get("x-session-token").next() {
                    let db = request.rocket().state::<Database>().expect("`Database`");
                    match Session::from_token(db, token).await {
                        Ok(session) => Outcome::Success(session),
                        // a token nobody knows is refused; a database that
                        // fails is not the client's fault
                        Err(error) => {
                            let error = crate::User::token_lookup_error(error);
                            Outcome::Error(refusal(request, error))
                        }
                    }
                } else {
                    Outcome::Error((Status::Unauthorized, create_error!(MissingHeaders)))
                }
            })
            .await
            .clone()
    }
}

/// The status and error a guard refuses with; the error reaches the
/// catcher through the request
pub(crate) fn refusal(request: &Request<'_>, error: Error) -> (Status, Error) {
    let status = match error.error_type {
        revolt_result::ErrorType::InvalidSession | revolt_result::ErrorType::MissingHeaders => {
            Status::Unauthorized
        }
        _ => Status::InternalServerError,
    };
    let shown = error.clone();
    request.local_cache(|| revolt_result::rocket::GuardRefusal(Some(shown)));
    (status, error)
}
