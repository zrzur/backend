use revolt_models::v0::{ApplicationCommand, CommandPermissionType, CommandPermissions};
use revolt_result::Result;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractApplicationCommands: Sync + Send {
    /// Insert a command; fails with `CommandAlreadyExists` on a duplicate type and name in its scope
    async fn insert_application_command(&self, command: &ApplicationCommand) -> Result<()>;

    /// Fetch a command by its id
    async fn fetch_application_command(&self, id: &str) -> Result<ApplicationCommand>;

    /// Fetch the commands of one scope: global when `server_id` is `None`, else that server's
    async fn fetch_application_commands(
        &self,
        application_id: &str,
        server_id: Option<&str>,
    ) -> Result<Vec<ApplicationCommand>>;

    /// Fetch every command usable in a server from the given applications
    /// (their global commands and the ones scoped to this server)
    async fn fetch_server_application_commands(
        &self,
        server_id: &str,
        application_ids: &[String],
    ) -> Result<Vec<ApplicationCommand>>;

    /// Ids of the applications with commands usable in this server (global or scoped to it)
    async fn fetch_command_application_ids(&self, server_id: &str) -> Result<Vec<String>>;

    /// Replace a stored command with a new revision
    async fn replace_application_command(&self, command: &ApplicationCommand) -> Result<()>;

    /// Delete a command by its id, with the rules servers set for it
    async fn delete_application_command(&self, id: &str) -> Result<()>;

    /// Delete every command of an application, in all scopes, with every
    /// rule servers set for it
    async fn delete_all_application_commands(&self, application_id: &str) -> Result<()>;

    /// Command permission rules set in a server: every application when
    /// `application_id` is `None`
    async fn fetch_command_permissions(
        &self,
        server_id: &str,
        application_id: Option<&str>,
    ) -> Result<Vec<CommandPermissions>>;

    /// The rules that decide one command in a server: the command's own and
    /// the whole app's (`command_id` the command or absent)
    async fn fetch_command_rules(
        &self,
        server_id: &str,
        application_id: &str,
        command_id: &str,
    ) -> Result<Vec<CommandPermissions>>;

    /// The rules of some applications in a server (the command list)
    async fn fetch_command_permissions_of(
        &self,
        server_id: &str,
        application_ids: &[String],
    ) -> Result<Vec<CommandPermissions>>;

    /// Take every rule about one role, member or channel out of a server's
    /// rules (it was deleted)
    async fn remove_command_permission_target(
        &self,
        server_id: &str,
        kind: CommandPermissionType,
        id: &str,
    ) -> Result<()>;

    /// Store the rules of one scope; an empty list removes the scope
    async fn set_command_permissions(&self, permissions: &CommandPermissions) -> Result<()>;

    /// Remove every rule of an application in a server (when it is removed)
    async fn delete_command_permissions(&self, server_id: &str, application_id: &str) -> Result<()>;
}

/// Storage key of one scope
pub fn command_permissions_key(permissions: &CommandPermissions) -> String {
    format!(
        "{}:{}:{}",
        permissions.server_id,
        permissions.application_id,
        permissions
            .command_id
            .as_deref()
            .unwrap_or(&permissions.application_id)
    )
}

