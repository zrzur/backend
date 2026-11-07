//! `Authorization: Bearer <access token>` for the `/oauth2/@me` resources only.

use revolt_database::{hash_secret, Database, OAuthToken};
use revolt_result::{create_error, Error};
use revolt_rocket_okapi::{
    r#gen::OpenApiGenerator,
    request::{OpenApiFromRequest, RequestHeaderInput},
};
use rocket::{
    http::Status,
    request::{FromRequest, Outcome},
    Request,
};

/// A valid, unexpired, unrevoked access token
pub struct Bearer(pub OAuthToken);

impl Bearer {
    /// The token must carry this scope
    pub fn require(&self, scope: &str) -> Result<(), Error> {
        if self.0.scopes.iter().any(|s| s == scope) {
            Ok(())
        } else {
            Err(create_error!(MissingPermission {
                permission: scope.to_string()
            }))
        }
    }
}

#[async_trait]
impl<'r> FromRequest<'r> for Bearer {
    type Error = Error;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let Some(token) = request
            .headers()
            .get_one("Authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            return Outcome::Error((Status::Unauthorized, create_error!(NotAuthenticated)));
        };
        let db = request.rocket().state::<Database>().expect("database");
        match db.fetch_oauth_token_by_access(&hash_secret(token)).await {
            Ok(Some(token)) if token.is_access_valid() => Outcome::Success(Bearer(token)),
            _ => Outcome::Error((Status::Unauthorized, create_error!(InvalidSession))),
        }
    }
}

impl OpenApiFromRequest<'_> for Bearer {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<RequestHeaderInput> {
        Ok(RequestHeaderInput::None)
    }
}
