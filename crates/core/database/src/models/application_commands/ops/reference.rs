use revolt_models::v0::{ApplicationCommand, CommandPermissionType, CommandPermissions};
use revolt_result::Result;

use crate::ReferenceDb;

use super::{command_permissions_key, AbstractApplicationCommands};

fn same_slot(a: &ApplicationCommand, b: &ApplicationCommand) -> bool {
    a.id != b.id
        && a.application_id == b.application_id
        && a.server_id == b.server_id
        && a.kind == b.kind
        && a.name == b.name
}

#[async_trait]
impl AbstractApplicationCommands for ReferenceDb {
    async fn insert_application_command(&self, command: &ApplicationCommand) -> Result<()> {
        let mut commands = self.application_commands.lock().await;
        if commands.contains_key(&command.id)
            || commands.values().any(|other| same_slot(other, command))
        {
            return Err(create_error!(CommandAlreadyExists));
        }
        commands.insert(command.id.clone(), command.clone());
        Ok(())
    }

    async fn fetch_application_command(&self, id: &str) -> Result<ApplicationCommand> {
        self.application_commands
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(UnknownCommand))
    }

    async fn fetch_application_commands(
        &self,
        application_id: &str,
        server_id: Option<&str>,
    ) -> Result<Vec<ApplicationCommand>> {
        Ok(self
            .application_commands
            .lock()
            .await
            .values()
            .filter(|c| c.application_id == application_id && c.server_id.as_deref() == server_id)
            .cloned()
            .collect())
    }

    async fn fetch_server_application_commands(
        &self,
        server_id: &str,
        application_ids: &[String],
    ) -> Result<Vec<ApplicationCommand>> {
        Ok(self
            .application_commands
            .lock()
            .await
            .values()
            .filter(|c| {
                application_ids.contains(&c.application_id)
                    && c.server_id.as_deref().is_none_or(|id| id == server_id)
            })
            .cloned()
            .collect())
    }

    async fn fetch_command_application_ids(&self, server_id: &str) -> Result<Vec<String>> {
        let mut ids: Vec<String> = self
            .application_commands
            .lock()
            .await
            .values()
            .filter(|c| c.server_id.as_deref().is_none_or(|id| id == server_id))
            .map(|c| c.application_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn replace_application_command(&self, command: &ApplicationCommand) -> Result<()> {
        let mut commands = self.application_commands.lock().await;
        if !commands.contains_key(&command.id) {
            return Err(create_error!(UnknownCommand));
        }
        if commands.values().any(|other| same_slot(other, command)) {
            return Err(create_error!(CommandAlreadyExists));
        }
        commands.insert(command.id.clone(), command.clone());
        Ok(())
    }

    async fn delete_application_command(&self, id: &str) -> Result<()> {
        self.application_commands
            .lock()
            .await
            .remove(id)
            .ok_or_else(|| create_error!(UnknownCommand))?;
        self.command_permissions
            .lock()
            .await
            .retain(|_, p| p.command_id.as_deref() != Some(id));
        Ok(())
    }

    async fn delete_all_application_commands(&self, application_id: &str) -> Result<()> {
        self.application_commands
            .lock()
            .await
            .retain(|_, c| c.application_id != application_id);
        self.command_permissions
            .lock()
            .await
            .retain(|_, p| p.application_id != application_id);
        Ok(())
    }

    async fn fetch_command_rules(
        &self,
        server_id: &str,
        application_id: &str,
        command_id: &str,
    ) -> Result<Vec<CommandPermissions>> {
        Ok(self
            .command_permissions
            .lock()
            .await
            .values()
            .filter(|p| {
                p.server_id == server_id
                    && p.application_id == application_id
                    && p.command_id.as_deref().is_none_or(|id| id == command_id)
            })
            .cloned()
            .collect())
    }

    async fn fetch_command_permissions_of(
        &self,
        server_id: &str,
        application_ids: &[String],
    ) -> Result<Vec<CommandPermissions>> {
        Ok(self
            .command_permissions
            .lock()
            .await
            .values()
            .filter(|p| p.server_id == server_id && application_ids.contains(&p.application_id))
            .cloned()
            .collect())
    }

    async fn remove_command_permission_target(
        &self,
        server_id: &str,
        kind: CommandPermissionType,
        id: &str,
    ) -> Result<()> {
        let mut store = self.command_permissions.lock().await;
        for rules in store.values_mut() {
            if rules.server_id == server_id {
                rules.permissions.retain(|rule| !(rule.kind == kind && rule.id == id));
            }
        }
        store.retain(|_, rules| !rules.permissions.is_empty());
        Ok(())
    }

    async fn fetch_command_permissions(
        &self,
        server_id: &str,
        application_id: Option<&str>,
    ) -> Result<Vec<CommandPermissions>> {
        Ok(self
            .command_permissions
            .lock()
            .await
            .values()
            .filter(|p| {
                p.server_id == server_id
                    && application_id.is_none_or(|id| p.application_id == id)
            })
            .cloned()
            .collect())
    }

    async fn set_command_permissions(&self, permissions: &CommandPermissions) -> Result<()> {
        let key = command_permissions_key(permissions);
        let mut store = self.command_permissions.lock().await;
        if permissions.permissions.is_empty() {
            store.remove(&key);
        } else {
            store.insert(key, permissions.clone());
        }
        Ok(())
    }

    async fn delete_command_permissions(&self, server_id: &str, application_id: &str) -> Result<()> {
        self.command_permissions
            .lock()
            .await
            .retain(|_, p| !(p.server_id == server_id && p.application_id == application_id));
        Ok(())
    }
}
