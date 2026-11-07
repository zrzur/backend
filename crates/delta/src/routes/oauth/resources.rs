use revolt_database::{Database, User};
use revolt_models::v0::{
    oauth_scopes, AuthorizedApplication, CommandApplication, OAuthServer, OAuthUser,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

use super::bearer::Bearer;

/// # Current User (OAuth2)
///
/// Needs `identify`. Only id, name and avatar; never e-mail or settings.
#[openapi(tag = "OAuth2")]
#[get("/@me")]
pub async fn me(db: &State<Database>, bearer: Bearer) -> Result<Json<OAuthUser>> {
    bearer.require(oauth_scopes::IDENTIFY)?;
    let user = db.fetch_user(&bearer.0.user_id).await?;
    Ok(Json(OAuthUser {
        id: user.id,
        username: user.username,
        discriminator: user.discriminator,
        display_name: user.display_name,
        avatar: user.avatar.map(Into::into),
    }))
}

/// # Current User's Servers (OAuth2)
///
/// Needs `servers.read`: id, name and icon of each server the user is in.
#[openapi(tag = "OAuth2")]
#[get("/@me/servers")]
pub async fn my_servers(db: &State<Database>, bearer: Bearer) -> Result<Json<Vec<OAuthServer>>> {
    bearer.require(oauth_scopes::SERVERS_READ)?;
    let mut servers = vec![];
    for membership in db.fetch_all_memberships(&bearer.0.user_id).await? {
        if let Ok(server) = db.fetch_server(&membership.id.server).await {
            servers.push(OAuthServer {
                id: server.id,
                name: server.name,
                icon: server.icon.map(Into::into),
            });
        }
    }
    Ok(Json(servers))
}

/// # Authorized Apps
///
/// Applications this user authorized with OAuth2.
#[openapi(tag = "OAuth2")]
#[get("/authorized")]
pub async fn authorized_apps(
    db: &State<Database>,
    user: User,
) -> Result<Json<Vec<AuthorizedApplication>>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let grants = db.fetch_oauth_grants(&user.id).await?;
    let ids: Vec<String> = grants.iter().map(|g| g.application_id.clone()).collect();
    let users = db.fetch_users(&ids).await?;
    Ok(Json(
        grants
            .into_iter()
            .filter_map(|grant| {
                let app = users.iter().find(|u| u.id == grant.application_id)?;
                Some(AuthorizedApplication {
                    application: CommandApplication {
                        id: app.id.clone(),
                        username: app.username.clone(),
                        display_name: app.display_name.clone(),
                        avatar: app.avatar.clone().map(Into::into),
                    },
                    scopes: grant.scopes,
                    authorized_at: grant.created_at,
                })
            })
            .collect(),
    ))
}

/// # Revoke Authorized App
///
/// Revoke every token the user gave this application. It does not remove the
/// bot from servers; that is done per server by its managers.
#[openapi(tag = "OAuth2")]
#[delete("/authorized/<application_id>")]
pub async fn revoke_authorized_app(
    db: &State<Database>,
    user: User,
    application_id: &str,
) -> Result<EmptyResponse> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    db.revoke_oauth_tokens(&user.id, application_id).await?;
    if !db.delete_oauth_grant(&user.id, application_id).await? {
        return Err(create_error!(UnknownApplication));
    }
    Ok(EmptyResponse)
}
