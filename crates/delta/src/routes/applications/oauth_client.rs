use iso8601_timestamp::Timestamp;
use revolt_database::{hash_secret, Database, OAuthClient, User, ValidatedTicket};
use revolt_models::v0::{
    oauth_limits, validate_redirect_uri, DataEditOAuthClient, OAuthClientSecret,
    OAuthClientSettings,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::manageable_application;

fn settings(client: &OAuthClient) -> OAuthClientSettings {
    OAuthClientSettings {
        client_id: client.id.clone(),
        redirect_uris: client.redirect_uris.clone(),
        has_secret: client.client_secret_hash.is_some(),
        secret_created_at: client.secret_created_at,
    }
}

async fn load(db: &Database, application_id: &str) -> Result<OAuthClient> {
    Ok(db
        .fetch_oauth_client(application_id)
        .await?
        .unwrap_or(OAuthClient {
            id: application_id.to_string(),
            ..Default::default()
        }))
}

/// # Fetch OAuth2 Settings
#[openapi(tag = "OAuth2")]
#[get("/<application_id>/oauth")]
pub async fn fetch_oauth_settings(
    db: &State<Database>,
    user: User,
    application_id: &str,
) -> Result<Json<OAuthClientSettings>> {
    manageable_application(db, &user, application_id).await?;
    Ok(Json(settings(&load(db, application_id).await?)))
}

/// # Edit OAuth2 Settings
///
/// Replace the redirect URIs: https, or http for localhost only; exact match
/// is required when authorizing.
#[openapi(tag = "OAuth2")]
#[patch("/<application_id>/oauth", data = "<data>")]
pub async fn edit_oauth_settings(
    db: &State<Database>,
    user: User,
    application_id: &str,
    data: Json<DataEditOAuthClient>,
) -> Result<Json<OAuthClientSettings>> {
    manageable_application(db, &user, application_id).await?;
    let data = data.into_inner();
    if data.redirect_uris.len() > oauth_limits::MAX_REDIRECT_URIS {
        return Err(create_error!(FailedValidation {
            error: format!("at most {} redirect URIs", oauth_limits::MAX_REDIRECT_URIS)
        }));
    }
    for uri in &data.redirect_uris {
        validate_redirect_uri(uri).map_err(|error| create_error!(FailedValidation { error }))?;
    }
    let mut client = load(db, application_id).await?;
    client.redirect_uris = data.redirect_uris;
    client.redirect_uris.dedup();
    db.save_oauth_client(&client).await?;
    Ok(Json(settings(&client)))
}

/// # Reset Client Secret
///
/// Create a new client secret (the old one stops working). Only the owner,
/// with a validated MFA ticket (`X-MFA-Ticket`). The secret is shown once.
#[openapi(tag = "OAuth2")]
#[post("/<application_id>/oauth/secret")]
pub async fn reset_client_secret(
    db: &State<Database>,
    user: User,
    ticket: Option<ValidatedTicket>,
    application_id: &str,
) -> Result<Json<OAuthClientSecret>> {
    let bot = manageable_application(db, &user, application_id).await?;
    // The bot's own token must not mint client secrets
    if user.id != bot.owner {
        return Err(create_error!(NotOwner));
    }
    if ticket.is_none() {
        return Err(create_error!(InvalidCredentials));
    }
    let secret = format!("zcs_{}", nanoid::nanoid!(48));
    let mut client = load(db, application_id).await?;
    client.client_secret_hash = Some(hash_secret(&secret));
    client.secret_created_at = Some(Timestamp::now_utc());
    db.save_oauth_client(&client).await?;
    Ok(Json(OAuthClientSecret {
        client_secret: secret,
    }))
}
