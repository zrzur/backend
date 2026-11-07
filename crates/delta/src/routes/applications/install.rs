//! Installing an application's bot into a server, and the install records.

use iso8601_timestamp::Timestamp;
use revolt_database::{
    util::permissions::DatabasePermissionQuery, AppInstall, AuditLogEntryAction, Bot, Database,
    Member, PartialMember, PartialRole, RemovalIntention, Role, Server, User,
};
use revolt_models::v0::{AppInstallation, CommandApplication};
use revolt_permissions::{calculate_server_permissions, ChannelPermission, Override};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

use crate::util::audit_log_reason::AuditLogReason;

/// Add a bot to a server on behalf of `user`, with the same rules as the bot
/// invite: the user needs Manage Server, and to grant permissions also
/// Manage Roles and every permission granted. Records who installed it.
pub async fn install_bot(
    db: &Database,
    user: &User,
    bot: &Bot,
    server: &Server,
    permissions: Option<i64>,
    scopes: Vec<String>,
    via: &str,
    reason: AuditLogReason,
) -> Result<()> {
    if !bot.public && bot.owner != user.id {
        return Err(create_error!(BotIsPrivate));
    }
    let bot_user = db.fetch_user(&bot.id).await?;

    let mut query = DatabasePermissionQuery::new(db, user).server(server);
    let user_permissions = calculate_server_permissions(&mut query).await;
    user_permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;

    // Check the permissions before anything changes
    if permissions.is_some_and(|p| p < 0) {
        return Err(create_error!(InvalidOperation));
    }
    let requested = permissions.map(|p| Override {
        allow: p as u64,
        deny: 0,
    });
    if let Some(requested) = &requested {
        user_permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageRole)?;
        user_permissions
            .throw_permission_override(Override::default(), requested)
            .await?;
    }

    // Unverified bots stop at 100 servers
    if !bot.is_verified()
        && db.fetch_server_count(&bot.id).await? >= revolt_database::UNVERIFIED_BOT_SERVER_LIMIT
    {
        return Err(create_error!(BotNeedsVerification));
    }

    let (mut member, _channels) = Member::create(db, server, &bot_user, None).await?;

    if let Some(requested) = requested {
        let mut role =
            Role::create_managed(db, server, bot_user.username.clone(), bot_user.id.clone())
                .await?;
        role.update(
            db,
            &server.id,
            PartialRole {
                permissions: Some(requested.into()),
                ..Default::default()
            },
            vec![],
        )
        .await?;
        AuditLogEntryAction::RoleCreate {
            role: role.id.clone(),
            name: role.name.clone(),
        }
        .insert(db, server.id.clone(), reason.0.clone(), user.id.clone(), None)
        .await;
        member
            .update(
                db,
                PartialMember {
                    roles: Some(vec![role.id.clone()]),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
    }

    AuditLogEntryAction::BotAdd {
        bot: bot.id.clone(),
        permissions: permissions.unwrap_or(0),
        scopes: scopes.clone(),
    }
    .insert(db, server.id.clone(), reason, user.id.clone(), Some(bot.id.clone()))
    .await;

    db.save_app_install(&AppInstall {
        id: AppInstall::key(&server.id, &bot.id),
        server_id: server.id.clone(),
        application_id: bot.id.clone(),
        installed_by: user.id.clone(),
        permissions: permissions.unwrap_or(0),
        scopes,
        via: via.to_string(),
        installed_at: Timestamp::now_utc(),
    })
    .await
}

/// Members who manage the server may see and change its apps
async fn require_manage_server(db: &Database, user: &User, server: &Server) -> Result<()> {
    let mut query = DatabasePermissionQuery::new(db, user).server(server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)
}

/// # List Server Apps
///
/// Apps installed in this server, who installed them and with which permissions.
#[openapi(tag = "Application Commands")]
#[get("/<server_id>/applications")]
pub async fn list_server_apps(
    db: &State<Database>,
    user: User,
    server_id: &str,
) -> Result<Json<Vec<AppInstallation>>> {
    let server = db.fetch_server(server_id).await?;
    require_manage_server(db, &user, &server).await?;

    let installs = db.fetch_app_installs(server_id).await?;
    let ids: Vec<String> = installs.iter().map(|i| i.application_id.clone()).collect();
    let users = db.fetch_users(&ids).await?;
    Ok(Json(
        installs
            .into_iter()
            .filter_map(|install| {
                let bot = users.iter().find(|u| u.id == install.application_id)?;
                Some(AppInstallation {
                    server_id: install.server_id,
                    application: CommandApplication {
                        id: bot.id.clone(),
                        username: bot.username.clone(),
                        display_name: bot.display_name.clone(),
                        avatar: bot.avatar.clone().map(Into::into),
                    },
                    installed_by: install.installed_by,
                    permissions: install.permissions,
                    scopes: install.scopes,
                    via: install.via,
                    installed_at: install.installed_at,
                })
            })
            .collect(),
    ))
}

/// # Uninstall App
///
/// Remove an app's bot from the server: its membership, its managed role
/// and its install record go; its server-scoped commands stop showing.
#[openapi(tag = "Application Commands")]
#[delete("/<server_id>/applications/<application_id>")]
pub async fn uninstall_server_app(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    server_id: &str,
    application_id: &str,
) -> Result<EmptyResponse> {
    let server = db.fetch_server(server_id).await?;
    require_manage_server(db, &user, &server).await?;
    let bot = db
        .fetch_bot(application_id)
        .await
        .map_err(|_| create_error!(UnknownApplication))?;

    if let Ok(member) = db.fetch_member(server_id, &bot.id).await {
        AuditLogEntryAction::BotRemove {
            bot: bot.id.clone(),
        }
        .insert(db, server.id.clone(), reason, user.id.clone(), Some(bot.id.clone()))
        .await;
        member
            .remove(db, &server, RemovalIntention::Kick, false)
            .await?;
        server.cleanup_managed_bot_role(db, &bot.id).await?;
    }
    db.delete_app_install(server_id, &bot.id).await?;
    Ok(EmptyResponse)
}
