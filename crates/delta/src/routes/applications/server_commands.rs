use revolt_database::{util::permissions::DatabasePermissionQuery, Database, User};
use revolt_models::v0::{CommandApplication, ServerCommands};
use revolt_permissions::calculate_server_permissions;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Fetch Server Commands
///
/// Commands a member can pick from in this server: those of every bot
/// installed here, global or limited to this server. Commands that need
/// server permissions the member lacks are left out; the channel is checked
/// again when a command is used.
#[openapi(tag = "Application Commands")]
#[get("/<server_id>/commands")]
pub async fn fetch_server_commands(
    db: &State<Database>,
    user: User,
    server_id: &str,
) -> Result<Json<ServerCommands>> {
    let server = db.fetch_server(server_id).await?;
    let member = db
        .fetch_member(server_id, &user.id)
        .await
        .map_err(|_| create_error!(NotFound))?;

    // Applications with commands, limited to the ones installed in this server
    let candidates = db.fetch_command_application_ids(server_id).await?;
    let installed: Vec<String> = db
        .fetch_members(server_id, &candidates)
        .await?
        .into_iter()
        .map(|member| member.id.user)
        .collect();

    let mut query = DatabasePermissionQuery::new(db, &user)
        .server(&server)
        .member(&member);
    let permissions = calculate_server_permissions(&mut query).await;

    let commands = db
        .fetch_server_application_commands(server_id, &installed)
        .await?
        .into_iter()
        .filter(|command| {
            command
                .default_member_permissions
                .is_none_or(|required| permissions.has(required as u64))
        })
        .collect();

    let applications = db
        .fetch_users(&installed)
        .await?
        .into_iter()
        .filter(|user| user.bot.is_some())
        .map(|user| CommandApplication {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
            avatar: user.avatar.map(Into::into),
        })
        .collect();

    Ok(Json(ServerCommands {
        applications,
        commands,
    }))
}
