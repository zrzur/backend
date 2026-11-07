use revolt_models::v0::{
    validate_command_set, ApplicationCommand, DataApplicationCommand, DataEditApplicationCommand,
};
use revolt_result::Result;
use ulid::Ulid;

use crate::Database;

fn build(
    id: String,
    application_id: &str,
    server_id: Option<&str>,
    data: DataApplicationCommand,
) -> ApplicationCommand {
    ApplicationCommand {
        id,
        application_id: application_id.to_string(),
        server_id: server_id.map(str::to_string),
        kind: data.kind,
        name: data.name,
        name_localizations: data.name_localizations,
        description: data.description,
        description_localizations: data.description_localizations,
        options: data.options,
        default_member_permissions: data.default_member_permissions,
        nsfw: data.nsfw,
        version: Ulid::new().to_string(),
    }
}

fn invalid(error: String) -> revolt_result::Error {
    create_error!(FailedValidation { error })
}

/// Register one new command in a scope (global when `server_id` is `None`)
pub async fn create_application_command(
    db: &Database,
    application_id: &str,
    server_id: Option<&str>,
    data: DataApplicationCommand,
) -> Result<ApplicationCommand> {
    data.validate_command().map_err(invalid)?;

    let existing = db
        .fetch_application_commands(application_id, server_id)
        .await?;
    if existing
        .iter()
        .any(|command| command.kind == data.kind && command.name == data.name)
    {
        return Err(create_error!(CommandAlreadyExists));
    }
    let max = data.kind.limit();
    if existing.iter().filter(|c| c.kind == data.kind).count() >= max {
        return Err(create_error!(TooManyCommands { max }));
    }

    let command = build(Ulid::new().to_string(), application_id, server_id, data);
    db.insert_application_command(&command).await?;
    Ok(command)
}

/// Apply a partial update; the command gets a new version
pub async fn edit_application_command(
    db: &Database,
    command: ApplicationCommand,
    edit: DataEditApplicationCommand,
) -> Result<ApplicationCommand> {
    let id = command.id.clone();
    let application_id = command.application_id.clone();
    let server_id = command.server_id.clone();

    let mut data: DataApplicationCommand = command.into();
    data.apply_edit(edit);
    data.validate_command().map_err(invalid)?;

    let updated = build(id, &application_id, server_id.as_deref(), data);
    db.replace_application_command(&updated).await?;
    Ok(updated)
}

/// Replace every command of a scope with the given list.
///
/// Commands are matched by type and name, so unchanged commands keep their
/// id and version and edited ones keep their id. The whole list is validated
/// before anything is written.
pub async fn sync_application_commands(
    db: &Database,
    application_id: &str,
    server_id: Option<&str>,
    commands: Vec<DataApplicationCommand>,
) -> Result<Vec<ApplicationCommand>> {
    validate_command_set(&commands).map_err(invalid)?;

    let mut existing = db
        .fetch_application_commands(application_id, server_id)
        .await?;

    // Removals first, so a later insert never collides with a stale entry
    let mut kept = Vec::with_capacity(existing.len());
    for command in existing.drain(..) {
        if commands
            .iter()
            .any(|data| data.kind == command.kind && data.name == command.name)
        {
            kept.push(command);
        } else {
            db.delete_application_command(&command.id).await?;
        }
    }

    let mut result = Vec::with_capacity(commands.len());
    for data in commands {
        match kept
            .iter()
            .position(|command| command.kind == data.kind && command.name == data.name)
        {
            Some(index) => {
                let current = kept.swap_remove(index);
                if DataApplicationCommand::from(current.clone()) == data {
                    result.push(current);
                } else {
                    let updated = build(current.id, application_id, server_id, data);
                    db.replace_application_command(&updated).await?;
                    result.push(updated);
                }
            }
            None => {
                let command = build(Ulid::new().to_string(), application_id, server_id, data);
                db.insert_application_command(&command).await?;
                result.push(command);
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};
    use revolt_models::v0::{ApplicationCommandType, DataApplicationCommand};

    use super::*;

    fn hello(description: &str) -> DataApplicationCommand {
        DataApplicationCommand {
            name: "hello".to_string(),
            description: description.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn create_edit_sync() {
        database_test!(|db| async move {
            let app = Ulid::new().to_string();
            let created = create_application_command(&db, &app, None, hello("Say hello"))
                .await
                .unwrap();
            assert_eq!(
                db.fetch_application_command(&created.id).await.unwrap(),
                created
            );

            // Same name in the same scope is a conflict, other scopes are separate
            assert!(create_application_command(&db, &app, None, hello("again"))
                .await
                .is_err());
            create_application_command(&db, &app, Some("server"), hello("server one"))
                .await
                .unwrap();

            let edited = edit_application_command(
                &db,
                created.clone(),
                DataEditApplicationCommand {
                    description: Some("Wave".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(edited.id, created.id);
            assert_ne!(edited.version, created.version);

            let menu = DataApplicationCommand {
                kind: ApplicationCommandType::Message,
                name: "Quote".to_string(),
                ..Default::default()
            };
            let synced =
                sync_application_commands(&db, &app, None, vec![hello("Wave"), menu.clone()])
                    .await
                    .unwrap();
            assert_eq!(synced.len(), 2);
            // Unchanged command keeps id and version
            assert_eq!(synced[0], edited);

            let synced = sync_application_commands(&db, &app, None, vec![menu])
                .await
                .unwrap();
            assert_eq!(synced.len(), 1);
            assert!(db.fetch_application_command(&created.id).await.is_err());
            // The server-scoped command is untouched by a global sync
            assert_eq!(
                db.fetch_application_commands(&app, Some("server"))
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                db.fetch_server_application_commands("server", &[app.clone()])
                    .await
                    .unwrap()
                    .len(),
                2
            );

            db.delete_all_application_commands(&app).await.unwrap();
            assert!(db
                .fetch_server_application_commands("server", &[app])
                .await
                .unwrap()
                .is_empty());
        });
    }
}
