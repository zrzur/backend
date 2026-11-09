use futures::StreamExt;
use revolt_models::v0::{
    ApplicationCommand, CommandPermission, CommandPermissionType, CommandPermissions,
};
use serde::{Deserialize, Serialize};
use revolt_result::Result;

use crate::MongoDb;

use super::{command_permissions_key, AbstractApplicationCommands};

pub(crate) static COL: &str = "application_commands";
static PERMISSIONS_COL: &str = "command_permissions";

/// One scope of command permission rules as stored
#[derive(Serialize, Deserialize)]
struct StoredCommandPermissions {
    #[serde(rename = "_id")]
    id: String,
    server_id: String,
    application_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    command_id: Option<String>,
    permissions: Vec<CommandPermission>,
}

fn is_duplicate(error: &mongodb::error::Error) -> bool {
    error.to_string().contains("E11000")
}

impl MongoDb {
    async fn find_commands(&self, filter: bson::Document) -> Result<Vec<ApplicationCommand>> {
        Ok(self
            .col::<ApplicationCommand>(COL)
            .find(filter)
            .await
            .map_err(|_| create_database_error!("find", COL))?
            .filter_map(|s| async { s.ok() })
            .collect()
            .await)
    }

    /// Rules as stored. A document that does not read is an error, never
    /// skipped: a missing deny would let people in.
    async fn find_command_permissions(
        &self,
        filter: bson::Document,
    ) -> Result<Vec<CommandPermissions>> {
        let stored: Vec<std::result::Result<StoredCommandPermissions, _>> = self
            .col::<StoredCommandPermissions>(PERMISSIONS_COL)
            .find(filter)
            .await
            .map_err(|_| create_database_error!("find", PERMISSIONS_COL))?
            .collect()
            .await;
        stored
            .into_iter()
            .map(|entry| {
                entry
                    .map(|stored| CommandPermissions {
                        server_id: stored.server_id,
                        application_id: stored.application_id,
                        command_id: stored.command_id,
                        permissions: stored.permissions,
                    })
                    .map_err(|_| create_database_error!("decode", PERMISSIONS_COL))
            })
            .collect()
    }
}

#[async_trait]
impl AbstractApplicationCommands for MongoDb {
    async fn insert_application_command(&self, command: &ApplicationCommand) -> Result<()> {
        self.col::<ApplicationCommand>(COL)
            .insert_one(command)
            .await
            .map(|_| ())
            .map_err(|error| {
                if is_duplicate(&error) {
                    create_error!(CommandAlreadyExists)
                } else {
                    create_database_error!("insert_one", COL)
                }
            })
    }

    async fn fetch_application_command(&self, id: &str) -> Result<ApplicationCommand> {
        self.col::<ApplicationCommand>(COL)
            .find_one(doc! { "_id": id })
            .await
            .map_err(|_| create_database_error!("find_one", COL))?
            .ok_or_else(|| create_error!(UnknownCommand))
    }

    async fn fetch_application_commands(
        &self,
        application_id: &str,
        server_id: Option<&str>,
    ) -> Result<Vec<ApplicationCommand>> {
        let filter = match server_id {
            Some(server_id) => doc! { "application_id": application_id, "server_id": server_id },
            None => doc! { "application_id": application_id, "server_id": { "$exists": false } },
        };
        self.find_commands(filter).await
    }

    async fn fetch_server_application_commands(
        &self,
        server_id: &str,
        application_ids: &[String],
    ) -> Result<Vec<ApplicationCommand>> {
        if application_ids.is_empty() {
            return Ok(vec![]);
        }
        self.find_commands(doc! {
            "application_id": { "$in": application_ids },
            "$or": [
                { "server_id": { "$exists": false } },
                { "server_id": server_id },
            ]
        })
        .await
    }

    async fn fetch_command_application_ids(&self, server_id: &str) -> Result<Vec<String>> {
        let ids = self
            .col::<ApplicationCommand>(COL)
            .distinct(
                "application_id",
                doc! { "$or": [ { "server_id": { "$exists": false } }, { "server_id": server_id } ] },
            )
            .await
            .map_err(|_| create_database_error!("distinct", COL))?;
        Ok(ids
            .into_iter()
            .filter_map(|id| id.as_str().map(str::to_string))
            .collect())
    }

    async fn replace_application_command(&self, command: &ApplicationCommand) -> Result<()> {
        let result = self
            .col::<ApplicationCommand>(COL)
            .replace_one(doc! { "_id": &command.id }, command)
            .await
            .map_err(|error| {
                if is_duplicate(&error) {
                    create_error!(CommandAlreadyExists)
                } else {
                    create_database_error!("replace_one", COL)
                }
            })?;
        if result.matched_count == 0 {
            return Err(create_error!(UnknownCommand));
        }
        Ok(())
    }

    async fn delete_application_command(&self, id: &str) -> Result<()> {
        let result = self
            .col::<ApplicationCommand>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map_err(|_| create_database_error!("delete_one", COL))?;
        if result.deleted_count == 0 {
            return Err(create_error!(UnknownCommand));
        }
        self.col::<StoredCommandPermissions>(PERMISSIONS_COL)
            .delete_many(doc! { "command_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", PERMISSIONS_COL))
    }

    async fn delete_all_application_commands(&self, application_id: &str) -> Result<()> {
        self.col::<ApplicationCommand>(COL)
            .delete_many(doc! { "application_id": application_id })
            .await
            .map_err(|_| create_database_error!("delete_many", COL))?;
        self.col::<StoredCommandPermissions>(PERMISSIONS_COL)
            .delete_many(doc! { "application_id": application_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", PERMISSIONS_COL))
    }

    async fn fetch_command_rules(
        &self,
        server_id: &str,
        application_id: &str,
        command_id: &str,
    ) -> Result<Vec<CommandPermissions>> {
        self.find_command_permissions(doc! {
            "server_id": server_id,
            "application_id": application_id,
            "$or": [
                { "command_id": command_id },
                { "command_id": { "$exists": false } },
            ],
        })
        .await
    }

    async fn fetch_command_permissions_of(
        &self,
        server_id: &str,
        application_ids: &[String],
    ) -> Result<Vec<CommandPermissions>> {
        if application_ids.is_empty() {
            return Ok(vec![]);
        }
        self.find_command_permissions(doc! {
            "server_id": server_id,
            "application_id": { "$in": application_ids },
        })
        .await
    }

    async fn remove_command_permission_target(
        &self,
        server_id: &str,
        kind: CommandPermissionType,
        id: &str,
    ) -> Result<()> {
        let kind = match kind {
            CommandPermissionType::Role => "Role",
            CommandPermissionType::Member => "Member",
            CommandPermissionType::Channel => "Channel",
        };
        let col = self.col::<StoredCommandPermissions>(PERMISSIONS_COL);
        col.update_many(
            doc! { "server_id": server_id },
            doc! { "$pull": { "permissions": { "type": kind, "id": id } } },
        )
        .await
        .map_err(|_| create_database_error!("update_many", PERMISSIONS_COL))?;
        col.delete_many(doc! { "server_id": server_id, "permissions": { "$size": 0 } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", PERMISSIONS_COL))
    }

    async fn fetch_command_permissions(
        &self,
        server_id: &str,
        application_id: Option<&str>,
    ) -> Result<Vec<CommandPermissions>> {
        let mut filter = doc! { "server_id": server_id };
        if let Some(application_id) = application_id {
            filter.insert("application_id", application_id);
        }
        self.find_command_permissions(filter).await
    }

    async fn set_command_permissions(&self, permissions: &CommandPermissions) -> Result<()> {
        let id = command_permissions_key(permissions);
        let col = self.col::<StoredCommandPermissions>(PERMISSIONS_COL);
        if permissions.permissions.is_empty() {
            col.delete_one(doc! { "_id": &id })
                .await
                .map_err(|_| create_database_error!("delete_one", PERMISSIONS_COL))?;
            return Ok(());
        }
        let stored = StoredCommandPermissions {
            id: id.clone(),
            server_id: permissions.server_id.clone(),
            application_id: permissions.application_id.clone(),
            command_id: permissions.command_id.clone(),
            permissions: permissions.permissions.clone(),
        };
        col.replace_one(doc! { "_id": &id }, &stored)
            .upsert(true)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", PERMISSIONS_COL))
    }

    async fn delete_command_permissions(&self, server_id: &str, application_id: &str) -> Result<()> {
        self.col::<StoredCommandPermissions>(PERMISSIONS_COL)
            .delete_many(doc! { "server_id": server_id, "application_id": application_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", PERMISSIONS_COL))
    }
}
