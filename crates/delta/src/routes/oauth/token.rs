//! Token endpoint (RFC 6749 §4.1.3, §6) and revocation (RFC 7009).
//!
//! Requests are `application/x-www-form-urlencoded`; errors use the standard
//! `{ "error", "error_description" }` body so OAuth client libraries work.

use revolt_database::{
    hash_secret, in_seconds, pkce_matches, secret_hash_eq, Database, OAuthClient, OAuthToken,
};
use revolt_models::v0::{oauth_limits, OAuthTokenResponse};
use rocket::{
    form::Form,
    http::{Header, Status},
    response::Responder,
    serde::json::Json,
    Request, State,
};
use serde::Serialize;
use ulid::Ulid;

#[derive(Serialize)]
pub struct OAuthErrorBody {
    error: &'static str,
    error_description: String,
}

/// Standard OAuth2 error response
pub struct OAuthError(Status, OAuthErrorBody);

fn oauth_error(error: &'static str, description: impl Into<String>) -> OAuthError {
    let status = if error == "invalid_client" {
        Status::Unauthorized
    } else {
        Status::BadRequest
    };
    OAuthError(
        status,
        OAuthErrorBody {
            error,
            error_description: description.into(),
        },
    )
}

impl<'r> Responder<'r, 'static> for OAuthError {
    fn respond_to(self, request: &'r Request<'_>) -> rocket::response::Result<'static> {
        let mut response = (self.0, Json(self.1)).respond_to(request)?;
        response.set_header(Header::new("Cache-Control", "no-store"));
        Ok(response)
    }
}

/// Successful token response, never cached
pub struct TokenResponse(OAuthTokenResponse);

impl<'r> Responder<'r, 'static> for TokenResponse {
    fn respond_to(self, request: &'r Request<'_>) -> rocket::response::Result<'static> {
        let mut response = Json(self.0).respond_to(request)?;
        response.set_header(Header::new("Cache-Control", "no-store"));
        response.set_header(Header::new("Pragma", "no-cache"));
        Ok(response)
    }
}

#[derive(FromForm)]
pub struct TokenRequest<'r> {
    grant_type: &'r str,
    client_id: Option<&'r str>,
    client_secret: Option<&'r str>,
    code: Option<&'r str>,
    redirect_uri: Option<&'r str>,
    code_verifier: Option<&'r str>,
    refresh_token: Option<&'r str>,
    scope: Option<&'r str>,
}

/// Authenticate the client: it must exist, and send its secret if it has one
async fn authenticate_client(
    db: &Database,
    client_id: Option<&str>,
    client_secret: Option<&str>,
) -> Result<OAuthClient, OAuthError> {
    let client_id = client_id.ok_or_else(|| oauth_error("invalid_client", "client_id is required"))?;
    let client = db
        .fetch_oauth_client(client_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| oauth_error("invalid_client", "unknown client"))?;
    if let Some(expected) = &client.client_secret_hash {
        match client_secret {
            Some(secret) if secret_hash_eq(&hash_secret(secret), expected) => {}
            _ => return Err(oauth_error("invalid_client", "client authentication failed")),
        }
    }
    Ok(client)
}

/// A new access + refresh token pair
async fn issue(
    db: &Database,
    application_id: &str,
    user_id: &str,
    scopes: Vec<String>,
    family: String,
) -> Result<TokenResponse, OAuthError> {
    let access = format!("zat_{}", nanoid::nanoid!(48));
    let refresh = format!("zrt_{}", nanoid::nanoid!(48));
    let token = OAuthToken {
        id: Ulid::new().to_string(),
        family,
        application_id: application_id.to_string(),
        user_id: user_id.to_string(),
        scopes: scopes.clone(),
        access_hash: hash_secret(&access),
        refresh_hash: hash_secret(&refresh),
        access_expires_at: in_seconds(oauth_limits::ACCESS_TOKEN_LIFETIME_SECS),
        expires_at: in_seconds(oauth_limits::REFRESH_TOKEN_LIFETIME_SECS),
        revoked: false,
    };
    db.insert_oauth_token(&token)
        .await
        .map_err(|_| oauth_error("server_error", "could not store the token"))?;
    Ok(TokenResponse(OAuthTokenResponse {
        access_token: access,
        token_type: "Bearer".to_string(),
        expires_in: oauth_limits::ACCESS_TOKEN_LIFETIME_SECS,
        refresh_token: refresh,
        scope: scopes.join(" "),
    }))
}

/// # Token
///
/// Exchange an authorization code (with its PKCE verifier) or a refresh token
/// for an access token. Refresh tokens are replaced on every use.
#[openapi(skip)]
#[post("/token", data = "<form>")]
pub async fn token(
    db: &State<Database>,
    form: Form<TokenRequest<'_>>,
) -> Result<TokenResponse, OAuthError> {
    let client = authenticate_client(db, form.client_id, form.client_secret).await?;

    match form.grant_type {
        "authorization_code" => {
            let code = form
                .code
                .ok_or_else(|| oauth_error("invalid_request", "code is required"))?;
            // The code is consumed even if the rest fails: it works once
            let grant = db
                .take_oauth_code(&hash_secret(code))
                .await
                .ok()
                .flatten()
                .ok_or_else(|| oauth_error("invalid_grant", "unknown or used code"))?;
            if grant.application_id != client.id {
                return Err(oauth_error("invalid_grant", "code was issued to another client"));
            }
            if iso8601_timestamp::Timestamp::now_utc() >= grant.expires_at {
                return Err(oauth_error("invalid_grant", "code expired"));
            }
            if form.redirect_uri != Some(grant.redirect_uri.as_str()) {
                return Err(oauth_error("invalid_grant", "redirect_uri does not match"));
            }
            let verifier = form
                .code_verifier
                .ok_or_else(|| oauth_error("invalid_request", "code_verifier is required"))?;
            if !pkce_matches(verifier, &grant.code_challenge) {
                return Err(oauth_error("invalid_grant", "PKCE verification failed"));
            }
            issue(db, &client.id, &grant.user_id, grant.scopes, Ulid::new().to_string()).await
        }
        "refresh_token" => {
            let refresh = form
                .refresh_token
                .ok_or_else(|| oauth_error("invalid_request", "refresh_token is required"))?;
            let current = db
                .fetch_oauth_token_by_refresh(&hash_secret(refresh))
                .await
                .ok()
                .flatten()
                .filter(|token| token.application_id == client.id)
                .ok_or_else(|| oauth_error("invalid_grant", "unknown refresh token"))?;
            if current.revoked {
                // A replaced refresh token came back: assume theft, end the whole family
                db.revoke_oauth_token_family(&current.family).await.ok();
                return Err(oauth_error("invalid_grant", "refresh token was already used"));
            }
            if !current.is_refresh_valid() {
                return Err(oauth_error("invalid_grant", "refresh token expired"));
            }
            let scopes = match form.scope {
                Some(scope) => {
                    let requested = revolt_models::v0::parse_scopes(scope)
                        .map_err(|error| oauth_error("invalid_scope", error))?;
                    if requested.iter().any(|s| !current.scopes.contains(s)) {
                        return Err(oauth_error("invalid_scope", "scope exceeds the grant"));
                    }
                    requested
                }
                None => current.scopes.clone(),
            };
            if !db.revoke_oauth_token(&current.id).await.unwrap_or(false) {
                return Err(oauth_error("invalid_grant", "refresh token was already used"));
            }
            issue(db, &client.id, &current.user_id, scopes, current.family).await
        }
        _ => Err(oauth_error(
            "unsupported_grant_type",
            "use authorization_code or refresh_token",
        )),
    }
}

#[derive(FromForm)]
pub struct RevokeRequest<'r> {
    token: &'r str,
    client_id: Option<&'r str>,
    client_secret: Option<&'r str>,
}

/// # Revoke
///
/// Revoke an access or refresh token (and the rest of its family). Unknown
/// tokens are not an error (RFC 7009 §2.2).
#[openapi(skip)]
#[post("/revoke", data = "<form>")]
pub async fn revoke(
    db: &State<Database>,
    form: Form<RevokeRequest<'_>>,
) -> Result<Status, OAuthError> {
    let client = authenticate_client(db, form.client_id, form.client_secret).await?;
    let hash = hash_secret(form.token);
    let token = match db.fetch_oauth_token_by_access(&hash).await.ok().flatten() {
        Some(token) => Some(token),
        None => db.fetch_oauth_token_by_refresh(&hash).await.ok().flatten(),
    };
    if let Some(token) = token.filter(|token| token.application_id == client.id) {
        db.revoke_oauth_token_family(&token.family)
            .await
            .map_err(|_| oauth_error("server_error", "could not revoke"))?;
    }
    Ok(Status::Ok)
}
