//! Who may use an app's commands in a server, and where (Server settings →
//! Apps → a bot → Command permissions). Rules and how they are read:
//! `revolt_models::v0::command_permissions`.

use revolt_database::{
    util::permissions::DatabasePermissionQuery, AuditLogEntryAction, Database, Server, User,
};
use revolt_models::v0::{
    command_access, ApplicationCommand, CommandAccess, CommandAccessQuery, CommandPermission,
    CommandPermissions, DataEditCommandPermissions,
};
use revolt_permissions::{calculate_server_permissions, ChannelPermission, PermissionValue};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::audit_log_reason::AuditLogReason;

/// Every rule set in a server, read once per request
pub struct ServerCommandRules(Vec<CommandPermissions>);

impl ServerCommandRules {
    pub async fn load(db: &Database, server_id: &str) -> Result<Self> {
        Ok(Self(db.fetch_command_permissions(server_id, None).await?))
    }

    fn scope(&self, application_id: &str, command_id: Option<&str>) -> &[CommandPermission] {
        self.0
            .iter()
            .find(|p| p.application_id == application_id && p.command_id.as_deref() == command_id)
            .map(|p| p.permissions.as_slice())
            .unwrap_or(&[])
    }

    /// Decide for one command. `channel_id` `None` skips channel rules.
    pub fn access(
        &self,
        command: &ApplicationCommand,
        server: &Server,
        user_id: &str,
        roles: &[String],
        channel_id: Option<&str>,
        permissions: PermissionValue,
    ) -> CommandAccess {
        command_access(
            self.scope(&command.application_id, Some(&command.id)),
            self.scope(&command.application_id, None),
            &CommandAccessQuery {
                server_id: &server.id,
                user_id,
                roles,
                channel_id,
                is_owner: server.owner == user_id,
                has_default_permissions: command
                    .default_member_permissions
                    .is_none_or(|required| permissions.has(required as u64)),
            },
        )
    }
}

/// Reading needs Manage Server; the bot itself may read its own rules
async fn require_manager(db: &Database, user: &User, server: &Server, edit: bool) -> Result<()> {
    let mut query = DatabasePermissionQuery::new(db, user).server(server);
    let permissions = calculate_server_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    if edit {
        // rules name roles, so changing them needs Manage Roles too
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageRole)?;
    }
    Ok(())
}

async fn installed_app(db: &Database, server_id: &str, application_id: &str) -> Result<()> {
    db.fetch_bot(application_id)
        .await
        .map_err(|_| create_error!(UnknownApplication))?;
    db.fetch_member(server_id, application_id)
        .await
        .map(|_| ())
        .map_err(|_| create_error!(ApplicationNotInstalled))
}

/// # Fetch Command Permissions
///
/// The rules set in this server for an app: the whole app (no
/// `command_id`) and each command that has its own.
#[openapi(tag = "Application Commands")]
#[get("/<server_id>/applications/<application_id>/permissions")]
pub async fn fetch_command_permissions(
    db: &State<Database>,
    user: User,
    server_id: &str,
    application_id: &str,
) -> Result<Json<Vec<CommandPermissions>>> {
    let server = db.fetch_server(server_id).await?;
    if user.id != application_id {
        require_manager(db, &user, &server, false).await?;
    }
    installed_app(db, server_id, application_id).await?;
    Ok(Json(
        db.fetch_command_permissions(server_id, Some(application_id))
            .await?,
    ))
}

async fn store(
    db: &Database,
    user: &User,
    reason: AuditLogReason,
    server_id: &str,
    application_id: &str,
    command_id: Option<&str>,
    data: DataEditCommandPermissions,
) -> Result<Json<CommandPermissions>> {
    let server = db.fetch_server(server_id).await?;
    require_manager(db, user, &server, true).await?;
    installed_app(db, server_id, application_id).await?;
    data.validate_rules()
        .map_err(|error| create_error!(FailedValidation { error }))?;

    if let Some(command_id) = command_id {
        let command = db
            .fetch_application_command(command_id)
            .await
            .map_err(|_| create_error!(UnknownCommand))?;
        if command.application_id != application_id
            || command.server_id.as_deref().is_some_and(|id| id != server_id)
        {
            return Err(create_error!(UnknownCommand));
        }
    }

    let rules = CommandPermissions {
        server_id: server_id.to_string(),
        application_id: application_id.to_string(),
        command_id: command_id.map(str::to_string),
        permissions: data.permissions,
    };
    db.set_command_permissions(&rules).await?;
    AuditLogEntryAction::CommandPermissionsUpdate {
        bot: application_id.to_string(),
        command: command_id.map(str::to_string),
        before: vec![],
        after: vec![],
    }
    .insert(
        db,
        server_id.to_string(),
        reason,
        user.id.clone(),
        Some(application_id.to_string()),
    )
    .await;
    Ok(Json(rules))
}

/// # Edit App Permissions
///
/// Replace the rules for the whole app in this server. An empty list
/// removes them. Needs Manage Server and Manage Roles.
#[openapi(tag = "Application Commands")]
#[put("/<server_id>/applications/<application_id>/permissions", data = "<data>")]
pub async fn edit_app_command_permissions(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    server_id: &str,
    application_id: &str,
    data: Json<DataEditCommandPermissions>,
) -> Result<Json<CommandPermissions>> {
    store(db, &user, reason, server_id, application_id, None, data.into_inner()).await
}

/// # Edit Command Permissions
///
/// Replace the rules for one command in this server; they come before the
/// app's rules. An empty list removes them. Needs Manage Server and Manage
/// Roles.
#[openapi(tag = "Application Commands")]
#[put(
    "/<server_id>/applications/<application_id>/commands/<command_id>/permissions",
    data = "<data>"
)]
pub async fn edit_command_permissions(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    server_id: &str,
    application_id: &str,
    command_id: &str,
    data: Json<DataEditCommandPermissions>,
) -> Result<Json<CommandPermissions>> {
    store(
        db,
        &user,
        reason,
        server_id,
        application_id,
        Some(command_id),
        data.into_inner(),
    )
    .await
}
