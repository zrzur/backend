use revolt_database::{Database, User};
use revolt_models::v0::{
    oauth_scopes, AuthorizedApplication, CommandApplication, OAuthServer, OAuthUser,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

use super::{bearer::Bearer, oauth_server};

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
/// Needs `servers.read`: every server the user is a member of, with whether
/// they own it (`owner`), can manage it (`can_manage`, Manage Server) and
/// their server-wide permissions (`permissions`, a decimal string). Being in
/// this list does not mean the user may manage the server. With
/// `manageable=true` only the servers they can manage are listed: use that
/// for a bot dashboard.
#[openapi(tag = "OAuth2")]
#[get("/@me/servers?<manageable>")]
pub async fn my_servers(
    db: &State<Database>,
    bearer: Bearer,
    manageable: Option<bool>,
) -> Result<Json<Vec<OAuthServer>>> {
    bearer.require(oauth_scopes::SERVERS_READ)?;
    let mut user = db.fetch_user(&bearer.0.user_id).await?;
    // a staff override belongs to Zrzur's own tools, never to a third-party site
    user.privileged = false;
    let only_manageable = manageable.unwrap_or(false);
    let mut servers = vec![];
    for membership in db.fetch_all_memberships(&user.id).await? {
        if let Ok(server) = db.fetch_server(&membership.id.server).await {
            let server = oauth_server(db, &user, server).await;
            if !only_manageable || server.can_manage {
                servers.push(server);
            }
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
