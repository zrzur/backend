use revolt_database::{Bot, Database, User};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod commands;
pub mod install;
mod oauth_client;
pub mod server_commands;

#[cfg(test)]
mod audit_tests;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        commands::list_global_commands,
        commands::create_global_command,
        commands::sync_global_commands,
        commands::fetch_global_command,
        commands::edit_global_command,
        commands::delete_global_command,
        commands::list_server_commands,
        commands::create_server_command,
        commands::sync_server_commands,
        commands::fetch_server_command,
        commands::edit_server_command,
        commands::delete_server_command,
        oauth_client::fetch_oauth_settings,
        oauth_client::edit_oauth_settings,
        oauth_client::reset_client_secret,
    ]
}

/// Resolve an application the caller may manage.
///
/// The bot itself (authenticated with its token) and its owner (with a user
/// session) may manage it. Everyone else gets `UnknownApplication`, so the
/// response does not reveal whether the application exists.
pub async fn manageable_application(db: &Database, user: &User, application_id: &str) -> Result<Bot> {
    let bot = db
        .fetch_bot(application_id)
        .await
        .map_err(|_| create_error!(UnknownApplication))?;

    let is_bot_itself = user.id == bot.id;
    let is_owner = user.bot.is_none() && user.id == bot.owner;
    if is_bot_itself || is_owner {
        Ok(bot)
    } else {
        Err(create_error!(UnknownApplication))
    }
}

/// Server-scoped commands only exist for servers the bot is installed in
pub async fn require_installed(db: &Database, bot: &Bot, server_id: &str) -> Result<()> {
    db.fetch_member(server_id, &bot.id)
        .await
        .map(|_| ())
        .map_err(|_| create_error!(ApplicationNotInstalled))
}
