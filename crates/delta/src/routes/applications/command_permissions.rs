//! Who may use an app's commands in a server, and where (Server settings →
//! Apps → a bot → Command permissions). Rules and how they are read:
//! `revolt_models::v0::command_permissions`.
//!
//! Who may write which rule (besides Manage Server and Manage Roles):
//! - an Allow only for commands whose `default_member_permissions` the
//!   editor holds (for the whole app: every command of the app here);
//! - a rule about a role only below the editor's highest role, about a
//!   member only for members the editor outranks (adding, changing or
//!   removing it);
//! - the server owner may write any rule.
//!
//! Rules must name roles, channels and members of the server; a rule whose
//! role, channel or member is gone is dropped on the next save.

use revolt_database::{
    util::permissions::DatabasePermissionQuery, AuditLogEntryAction, Channel, Database, Server,
    User,
};
use revolt_models::v0::{
    command_access, ApplicationCommand, CommandAccess, CommandAccessQuery, CommandPermission,
    CommandPermissionType, CommandPermissions, DataEditCommandPermissions,
};
use revolt_permissions::{
    calculate_server_permissions, ChannelPermission, PermissionQuery, PermissionValue,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::audit_log_reason::AuditLogReason;

/// The channel whose rules apply: a thread or forum post follows the
/// channel it is in
pub fn rules_channel(channel: &Channel) -> &str {
    match channel {
        Channel::TextChannel {
            parent: Some(parent),
            ..
        } => parent,
        _ => channel.id(),
    }
}

/// The rules of a server, read once per request
pub struct ServerCommandRules(Vec<CommandPermissions>);

impl ServerCommandRules {
    /// The rules that decide one command: its own and its app's
    pub async fn load_for_command(
        db: &Database,
        server_id: &str,
        command: &ApplicationCommand,
    ) -> Result<Self> {
        Ok(Self(
            db.fetch_command_rules(server_id, &command.application_id, &command.id)
                .await?,
        ))
    }

    /// The rules of the given apps (a command list)
    pub async fn load_for_apps(db: &Database, server_id: &str, apps: &[String]) -> Result<Self> {
        Ok(Self(db.fetch_command_permissions_of(server_id, apps).await?))
    }

    fn scope(&self, application_id: &str, command_id: Option<&str>) -> &[CommandPermission] {
        self.0
            .iter()
            .find(|p| p.application_id == application_id && p.command_id.as_deref() == command_id)
            .map(|p| p.permissions.as_slice())
            .unwrap_or(&[])
    }

    /// Decide for one command. `channel_id` `None` skips channel rules; a
    /// thread is given as its parent channel (`rules_channel`).
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

/// The editor's server permissions and highest rank; Manage Server always,
/// Manage Roles too when editing
async fn manager(
    db: &Database,
    user: &User,
    server: &Server,
    edit: bool,
) -> Result<(PermissionValue, i64)> {
    let mut query = DatabasePermissionQuery::new(db, user).server(server);
    let permissions = calculate_server_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    if edit {
        // rules name roles, so changing them needs Manage Roles too
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageRole)?;
    }
    let rank = query.get_member_rank().unwrap_or(i64::MIN);
    Ok((permissions, rank))
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

/// Whether a rule names a role, channel or member of this server (the
/// server id stands for everyone / every channel)
async fn target_exists(db: &Database, server: &Server, rule: &CommandPermission) -> Result<bool> {
    if rule.id == server.id && rule.kind != CommandPermissionType::Member {
        return Ok(true);
    }
    Ok(match rule.kind {
        CommandPermissionType::Role => server.roles.contains_key(&rule.id),
        CommandPermissionType::Channel => server.channels.contains(&rule.id),
        CommandPermissionType::Member => db.fetch_member(&server.id, &rule.id).await.is_ok(),
    })
}

/// Refuse a rule about a role at or above the editor, or a member who
/// outranks them
async fn check_rank(
    db: &Database,
    server: &Server,
    editor: &User,
    editor_rank: i64,
    rule: &CommandPermission,
) -> Result<()> {
    match rule.kind {
        CommandPermissionType::Role if rule.id != server.id => {
            if let Some(role) = server.roles.get(&rule.id) {
                if role.rank <= editor_rank {
                    return Err(create_error!(NotElevated));
                }
            }
        }
        CommandPermissionType::Member if rule.id != editor.id => {
            if rule.id == server.owner {
                return Err(create_error!(NotElevated));
            }
            if let Ok(member) = db.fetch_member(&server.id, &rule.id).await {
                if member.get_ranking(server) <= editor_rank {
                    return Err(create_error!(NotElevated));
                }
            }
        }
        _ => {}
    }
    Ok(())
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
        manager(db, &user, &server, false).await?;
    }
    installed_app(db, server_id, application_id).await?;
    Ok(Json(
        db.fetch_command_permissions(server_id, Some(application_id))
            .await?,
    ))
}

/// # Fetch App Commands In Server
///
/// Every command of an app that works in this server, global and limited to
/// this server, without any filtering: for the people who manage the app's
/// rules. Needs Manage Server.
#[openapi(tag = "Application Commands")]
#[get("/<server_id>/applications/<application_id>/commands")]
pub async fn fetch_app_commands(
    db: &State<Database>,
    user: User,
    server_id: &str,
    application_id: &str,
) -> Result<Json<Vec<ApplicationCommand>>> {
    let server = db.fetch_server(server_id).await?;
    manager(db, &user, &server, false).await?;
    installed_app(db, server_id, application_id).await?;
    let mut commands = db
        .fetch_server_application_commands(server_id, &[application_id.to_string()])
        .await?;
    commands.sort_by(|a, b| a.name.cmp(&b.name).then(a.server_id.cmp(&b.server_id)));
    Ok(Json(commands))
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
    let (editor_permissions, editor_rank) = manager(db, user, &server, true).await?;
    installed_app(db, server_id, application_id).await?;
    data.validate_rules()
        .map_err(|error| create_error!(FailedValidation { error }))?;

    // the commands these rules are about
    let commands = match command_id {
        Some(command_id) => {
            let command = db
                .fetch_application_command(command_id)
                .await
                .map_err(|_| create_error!(UnknownCommand))?;
            if command.application_id != application_id
                || command.server_id.as_deref().is_some_and(|id| id != server_id)
            {
                return Err(create_error!(UnknownCommand));
            }
            vec![command]
        }
        None => {
            db.fetch_server_application_commands(server_id, &[application_id.to_string()])
                .await?
        }
    };

    let before: Vec<CommandPermission> = db
        .fetch_command_permissions(server_id, Some(application_id))
        .await?
        .into_iter()
        .find(|rules| rules.command_id.as_deref() == command_id)
        .map(|rules| rules.permissions)
        .unwrap_or_default();

    // unknown ids are refused; rules for things since deleted are dropped
    let mut after = Vec::with_capacity(data.permissions.len());
    for rule in data.permissions {
        if target_exists(db, &server, &rule).await? {
            after.push(rule);
        } else if !before.contains(&rule) {
            return Err(create_error!(FailedValidation {
                error: format!("{} is not a role, channel or member of this server", rule.id)
            }));
        }
    }

    if server.owner != user.id {
        let added: Vec<&CommandPermission> =
            after.iter().filter(|rule| !before.contains(rule)).collect();
        let removed: Vec<&CommandPermission> =
            before.iter().filter(|rule| !after.contains(rule)).collect();

        // an Allow hands out a command: only to the extent the editor may use it
        if added.iter().any(|rule| rule.permission) {
            let lacking = commands.iter().any(|command| {
                command
                    .default_member_permissions
                    .is_some_and(|required| !editor_permissions.has(required as u64))
            });
            if lacking {
                return Err(create_error!(MissingPermission {
                    permission: "Command".to_string()
                }));
            }
        }

        for rule in added.into_iter().chain(removed) {
            check_rank(db, &server, user, editor_rank, rule).await?;
        }
    }

    let rules = CommandPermissions {
        server_id: server_id.to_string(),
        application_id: application_id.to_string(),
        command_id: command_id.map(str::to_string),
        permissions: after.clone(),
    };
    db.set_command_permissions(&rules).await?;
    AuditLogEntryAction::CommandPermissionsUpdate {
        bot: application_id.to_string(),
        command: command_id.map(str::to_string),
        before,
        after,
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
/// removes them. Needs Manage Server and Manage Roles; see the module
/// documentation for which rules an editor may write.
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
/// Roles; see the module documentation for which rules an editor may write.
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
