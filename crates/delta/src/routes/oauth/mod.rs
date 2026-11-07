//! OAuth2 authorization code flow with PKCE (RFC 6749, RFC 7636, RFC 7009).
//!
//! Defaults: codes live 10 minutes and work once; access tokens live 1 hour;
//! refresh tokens live 30 days and are replaced on every use (a reused one
//! revokes its whole family). PKCE with S256 is always required; apps with a
//! client secret must also send it. Redirect URIs must match exactly.
//! Access tokens only work on `/oauth2/@me...`: they never act as the user
//! on the rest of the API, so scopes cannot bypass any permission.

use revolt_database::{Bot, Database};
use revolt_models::v0::{parse_scopes, oauth_limits, oauth_scopes};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod authorize;
mod bearer;
mod resources;
#[cfg(test)]
mod tests;
mod token;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        authorize::authorize_preview,
        authorize::authorize,
        token::token,
        token::revoke,
        resources::me,
        resources::my_servers,
        resources::authorized_apps,
        resources::revoke_authorized_app,
    ]
}

pub fn invalid(error: impl Into<String>) -> revolt_result::Error {
    create_error!(FailedValidation {
        error: error.into()
    })
}

/// Everything about an authorization request that does not depend on the user
pub struct ValidRequest {
    pub bot: Bot,
    pub scopes: Vec<String>,
}

/// Check client, redirect URI, scopes, state and PKCE. Any failure here is
/// shown to the user and never redirected (the redirect URI may not be trusted).
#[allow(clippy::too_many_arguments)]
pub async fn validate_request(
    db: &Database,
    client_id: &str,
    redirect_uri: &str,
    response_type: &str,
    scope: &str,
    state: &str,
    code_challenge: &str,
    code_challenge_method: &str,
    permissions: Option<i64>,
) -> Result<ValidRequest> {
    let bot = db
        .fetch_bot(client_id)
        .await
        .map_err(|_| create_error!(UnknownApplication))?;
    let client = db
        .fetch_oauth_client(client_id)
        .await?
        .ok_or_else(|| invalid("this application has no OAuth2 redirect URIs"))?;
    if !client.redirect_uris.iter().any(|uri| uri == redirect_uri) {
        return Err(invalid("redirect_uri is not registered for this application"));
    }
    if response_type != "code" {
        return Err(invalid("only response_type=code is supported"));
    }
    let scopes = parse_scopes(scope).map_err(invalid)?;
    if state.is_empty() || state.len() > oauth_limits::STATE_LENGTH {
        return Err(invalid("state is required"));
    }
    if code_challenge_method != "S256" || !(43..=128).contains(&code_challenge.len()) {
        return Err(invalid("PKCE with code_challenge_method=S256 is required"));
    }
    if permissions.is_some() && !scopes.iter().any(|s| s == oauth_scopes::BOT_INSTALL) {
        return Err(invalid("permissions need the bot.install scope"));
    }
    if permissions.is_some_and(|p| p < 0) {
        return Err(invalid("permissions must not be negative"));
    }
    Ok(ValidRequest { bot, scopes })
}

#[cfg(test)]
mod unit {
    use revolt_database::pkce_matches;

    #[test]
    fn pkce_example_from_rfc7636() {
        // RFC 7636 appendix B
        assert!(pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
        assert!(!pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXX",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
        assert!(!pkce_matches("short", "x"));
    }
}
