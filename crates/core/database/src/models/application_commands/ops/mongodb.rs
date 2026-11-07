use futures::StreamExt;
use revolt_models::v0::ApplicationCommand;
use revolt_result::Result;

use crate::MongoDb;

use super::AbstractApplicationCommands;

pub(crate) static COL: &str = "application_commands";

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
        Ok(())
    }

    async fn delete_all_application_commands(&self, application_id: &str) -> Result<()> {
        self.col::<ApplicationCommand>(COL)
            .delete_many(doc! { "application_id": application_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", COL))
    }
}
