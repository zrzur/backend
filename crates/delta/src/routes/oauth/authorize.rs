use iso8601_timestamp::Timestamp;
use revolt_database::{
    hash_secret, in_seconds, util::permissions::DatabasePermissionQuery, Database,
    OAuthCode, OAuthGrant, User,
};
use revolt_models::v0::{
    oauth_limits, oauth_scopes, AuthorizationPreview, AuthorizeRedirect, BotFlags, CommandApplication,
    DataAuthorize, OAuthServer,
};
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::{invalid, validate_request};
use crate::{routes::applications::install::install_bot, util::audit_log_reason::AuditLogReason};

/// The registered redirect URI with extra query parameters
fn redirect_with(redirect_uri: &str, params: &[(&str, &str)]) -> Result<String> {
    let mut url = url::Url::parse(redirect_uri).map_err(|_| invalid("invalid redirect_uri"))?;
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in params {
            query.append_pair(key, value);
        }
    }
    Ok(url.to_string())
}

/// Servers the user can add a bot to (Manage Server), without the ones it is already in
async fn installable_servers(db: &Database, user: &User, bot_id: &str) -> Result<Vec<OAuthServer>> {
    let mut servers = vec![];
    for membership in db.fetch_all_memberships(&user.id).await? {
        let Ok(server) = db.fetch_server(&membership.id.server).await else {
            continue;
        };
        if db.fetch_member(&server.id, bot_id).await.is_ok() {
            continue;
        }
        let mut query = DatabasePermissionQuery::new(db, user).server(&server);
        if calculate_server_permissions(&mut query)
            .await
            .has_channel_permission(ChannelPermission::ManageServer)
        {
            servers.push(OAuthServer {
                id: server.id,
                name: server.name,
                icon: server.icon.map(Into::into),
            });
        }
    }
    Ok(servers)
}

/// # Authorization Preview
///
/// What the consent page shows: the application, the scopes it asks for, and
/// (for `bot.install`) the servers the user can add it to. Every check of the
/// request happens here first; errors are shown, never redirected.
#[openapi(tag = "OAuth2")]
#[get(
    "/authorize?<client_id>&<redirect_uri>&<response_type>&<scope>&<state>&<code_challenge>&<code_challenge_method>&<permissions>"
)]
#[allow(clippy::too_many_arguments)]
pub async fn authorize_preview(
    db: &State<Database>,
    user: User,
    client_id: &str,
    redirect_uri: &str,
    response_type: &str,
    scope: &str,
    state: &str,
    code_challenge: &str,
    code_challenge_method: &str,
    permissions: Option<i64>,
) -> Result<Json<AuthorizationPreview>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let request = validate_request(
        db,
        client_id,
        redirect_uri,
        response_type,
        scope,
        state,
        code_challenge,
        code_challenge_method,
        permissions,
    )
    .await?;

    let bot_user = db.fetch_user(&request.bot.id).await?;
    let installs = request.scopes.iter().any(|s| s == oauth_scopes::BOT_INSTALL);
    if installs && !request.bot.public && request.bot.owner != user.id {
        return Err(create_error!(BotIsPrivate));
    }

    Ok(Json(AuthorizationPreview {
        description: bot_user
            .profile
            .as_ref()
            .and_then(|profile| profile.public_content())
            .map(str::to_string),
        verified: request
            .bot
            .flags
            .is_some_and(|flags| flags & BotFlags::Verified as i32 != 0),
        application: CommandApplication {
            id: bot_user.id.clone(),
            username: bot_user.username.clone(),
            display_name: bot_user.display_name.clone(),
            avatar: bot_user.avatar.clone().map(Into::into),
        },
        scopes: request.scopes,
        redirect_uri: redirect_uri.to_string(),
        permissions,
        servers: if installs {
            installable_servers(db, &user, &request.bot.id).await?
        } else {
            vec![]
        },
    }))
}

/// # Authorize
///
/// The user's decision on the consent page. Approving issues a single-use
/// code (and installs the bot for `bot.install`); denying returns
/// `error=access_denied`. Either way the answer is the URL to go to next.
#[openapi(tag = "OAuth2")]
#[post("/authorize", data = "<data>")]
pub async fn authorize(
    db: &State<Database>,
    user: User,
    data: Json<DataAuthorize>,
) -> Result<Json<AuthorizeRedirect>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let data = data.into_inner();
    let request = validate_request(
        db,
        &data.client_id,
        &data.redirect_uri,
        &data.response_type,
        &data.scope,
        &data.state,
        &data.code_challenge,
        &data.code_challenge_method,
        data.permissions,
    )
    .await?;

    if !data.approve {
        return Ok(Json(AuthorizeRedirect {
            redirect: redirect_with(
                &data.redirect_uri,
                &[("error", "access_denied"), ("state", &data.state)],
            )?,
        }));
    }

    // Install first: if it fails nothing was granted
    if request.scopes.iter().any(|s| s == oauth_scopes::BOT_INSTALL) {
        let server_id = data
            .server_id
            .as_deref()
            .ok_or_else(|| invalid("choose a server to add the bot to"))?;
        let server = db.fetch_server(server_id).await?;
        if db.fetch_member(&server.id, &request.bot.id).await.is_ok() {
            return Err(create_error!(AlreadyInServer));
        }
        install_bot(
            db,
            &user,
            &request.bot,
            &server,
            data.permissions,
            request.scopes.clone(),
            "oauth",
            AuditLogReason(Some("Added through OAuth2".to_string())),
        )
        .await?;
    }

    let code = nanoid::nanoid!(48);
    db.insert_oauth_code(&OAuthCode {
        code_hash: hash_secret(&code),
        application_id: request.bot.id.clone(),
        user_id: user.id.clone(),
        redirect_uri: data.redirect_uri.clone(),
        scopes: request.scopes.clone(),
        code_challenge: data.code_challenge.clone(),
        expires_at: in_seconds(oauth_limits::CODE_LIFETIME_SECS),
    })
    .await?;

    // Remember the authorization for "Authorized apps"; scopes add up
    let now = Timestamp::now_utc();
    let mut grant = db
        .fetch_oauth_grants(&user.id)
        .await?
        .into_iter()
        .find(|grant| grant.application_id == request.bot.id)
        .unwrap_or(OAuthGrant {
            id: OAuthGrant::key(&user.id, &request.bot.id),
            user_id: user.id.clone(),
            application_id: request.bot.id.clone(),
            scopes: vec![],
            created_at: now,
            updated_at: now,
        });
    for scope in &request.scopes {
        if !grant.scopes.contains(scope) {
            grant.scopes.push(scope.clone());
        }
    }
    grant.updated_at = now;
    db.save_oauth_grant(&grant).await?;

    Ok(Json(AuthorizeRedirect {
        redirect: redirect_with(&data.redirect_uri, &[("code", &code), ("state", &data.state)])?,
    }))
}
