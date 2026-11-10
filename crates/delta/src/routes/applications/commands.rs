use revolt_database::{
    create_application_command, events::client::EventV1, edit_application_command, sync_application_commands, Database,
    User,
};
use revolt_models::v0::{ApplicationCommand, DataApplicationCommand, DataEditApplicationCommand};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

use super::{manageable_application, require_installed};

/// Check access and resolve the scope of a request
async fn scope(
    db: &Database,
    user: &User,
    application_id: &str,
    server_id: Option<&str>,
) -> Result<()> {
    let bot = manageable_application(db, user, application_id).await?;
    if let Some(server_id) = server_id {
        require_installed(db, &bot, server_id).await?;
    }
    Ok(())
}

/// Tell the servers where these commands show up to refetch them
async fn notify(db: &Database, application_id: &str, server_id: Option<&str>) {
    if server_id.is_none() {
        // the bot's page and profile list its global commands from a cache
        crate::util::public_commands::forget(application_id);
    }
    let servers = match server_id {
        Some(server_id) => vec![server_id.to_string()],
        None => db
            .fetch_all_memberships(application_id)
            .await
            .map(|members| members.into_iter().map(|member| member.id.server).collect())
            .unwrap_or_default(),
    };
    for server_id in servers {
        EventV1::ApplicationCommandsUpdate {
            application_id: application_id.to_string(),
            server_id: server_id.clone(),
        }
        .p(server_id)
        .await;
    }
}

/// Fetch a command and make sure it belongs to this application and scope
async fn scoped_command(
    db: &Database,
    application_id: &str,
    server_id: Option<&str>,
    command_id: &str,
) -> Result<ApplicationCommand> {
    let command = db.fetch_application_command(command_id).await?;
    if command.application_id != application_id || command.server_id.as_deref() != server_id {
        return Err(create_error!(UnknownCommand));
    }
    Ok(command)
}

/// # List Global Commands
///
/// Commands of this application available in every server it is installed in.
#[openapi(tag = "Application Commands")]
#[get("/<application_id>/commands")]
pub async fn list_global_commands(
    db: &State<Database>,
    user: User,
    application_id: &str,
) -> Result<Json<Vec<ApplicationCommand>>> {
    scope(db, &user, application_id, None).await?;
    Ok(Json(db.fetch_application_commands(application_id, None).await?))
}

/// # Create Global Command
///
/// Register a new command. A command with the same type and name is a conflict.
#[openapi(tag = "Application Commands")]
#[post("/<application_id>/commands", data = "<data>")]
pub async fn create_global_command(
    db: &State<Database>,
    user: User,
    application_id: &str,
    data: Json<DataApplicationCommand>,
) -> Result<Json<ApplicationCommand>> {
    scope(db, &user, application_id, None).await?;
    let result = create_application_command(db, application_id, None, data.into_inner()).await?;
    notify(db, application_id, None).await;
    Ok(Json(result))
}

/// # Sync Global Commands
///
/// Replace every global command with the given list, matched by type and name.
/// The list is validated as a whole before anything changes.
#[openapi(tag = "Application Commands")]
#[put("/<application_id>/commands", data = "<data>")]
pub async fn sync_global_commands(
    db: &State<Database>,
    user: User,
    application_id: &str,
    data: Json<Vec<DataApplicationCommand>>,
) -> Result<Json<Vec<ApplicationCommand>>> {
    scope(db, &user, application_id, None).await?;
    let result = sync_application_commands(db, application_id, None, data.into_inner()).await?;
    notify(db, application_id, None).await;
    Ok(Json(result))
}

/// # Fetch Global Command
#[openapi(tag = "Application Commands")]
#[get("/<application_id>/commands/<command_id>")]
pub async fn fetch_global_command(
    db: &State<Database>,
    user: User,
    application_id: &str,
    command_id: &str,
) -> Result<Json<ApplicationCommand>> {
    scope(db, &user, application_id, None).await?;
    scoped_command(db, application_id, None, command_id)
        .await
        .map(Json)
}

/// # Edit Global Command
#[openapi(tag = "Application Commands")]
#[patch("/<application_id>/commands/<command_id>", data = "<data>")]
pub async fn edit_global_command(
    db: &State<Database>,
    user: User,
    application_id: &str,
    command_id: &str,
    data: Json<DataEditApplicationCommand>,
) -> Result<Json<ApplicationCommand>> {
    scope(db, &user, application_id, None).await?;
    let command = scoped_command(db, application_id, None, command_id).await?;
    let command = edit_application_command(db, command, data.into_inner()).await?;
    notify(db, application_id, command.server_id.as_deref()).await;
    Ok(Json(command))
}

/// # Delete Global Command
#[openapi(tag = "Application Commands")]
#[delete("/<application_id>/commands/<command_id>")]
pub async fn delete_global_command(
    db: &State<Database>,
    user: User,
    application_id: &str,
    command_id: &str,
) -> Result<EmptyResponse> {
    scope(db, &user, application_id, None).await?;
    let command = scoped_command(db, application_id, None, command_id).await?;
    db.delete_application_command(&command.id).await?;
    notify(db, application_id, command.server_id.as_deref()).await;
    Ok(EmptyResponse)
}

/// # List Server Commands
///
/// Commands of this application limited to one server it is installed in.
#[openapi(tag = "Application Commands")]
#[get("/<application_id>/servers/<server_id>/commands")]
pub async fn list_server_commands(
    db: &State<Database>,
    user: User,
    application_id: &str,
    server_id: &str,
) -> Result<Json<Vec<ApplicationCommand>>> {
    scope(db, &user, application_id, Some(server_id)).await?;
    Ok(Json(
        db.fetch_application_commands(application_id, Some(server_id))
            .await?,
    ))
}

/// # Create Server Command
#[openapi(tag = "Application Commands")]
#[post("/<application_id>/servers/<server_id>/commands", data = "<data>")]
pub async fn create_server_command(
    db: &State<Database>,
    user: User,
    application_id: &str,
    server_id: &str,
    data: Json<DataApplicationCommand>,
) -> Result<Json<ApplicationCommand>> {
    scope(db, &user, application_id, Some(server_id)).await?;
    let result = create_application_command(db, application_id, Some(server_id), data.into_inner()).await?;
    notify(db, application_id, Some(server_id)).await;
    Ok(Json(result))
}

/// # Sync Server Commands
#[openapi(tag = "Application Commands")]
#[put("/<application_id>/servers/<server_id>/commands", data = "<data>")]
pub async fn sync_server_commands(
    db: &State<Database>,
    user: User,
    application_id: &str,
    server_id: &str,
    data: Json<Vec<DataApplicationCommand>>,
) -> Result<Json<Vec<ApplicationCommand>>> {
    scope(db, &user, application_id, Some(server_id)).await?;
    let result = sync_application_commands(db, application_id, Some(server_id), data.into_inner()).await?;
    notify(db, application_id, Some(server_id)).await;
    Ok(Json(result))
}

/// # Fetch Server Command
#[openapi(tag = "Application Commands")]
#[get("/<application_id>/servers/<server_id>/commands/<command_id>")]
pub async fn fetch_server_command(
    db: &State<Database>,
    user: User,
    application_id: &str,
    server_id: &str,
    command_id: &str,
) -> Result<Json<ApplicationCommand>> {
    scope(db, &user, application_id, Some(server_id)).await?;
    scoped_command(db, application_id, Some(server_id), command_id)
        .await
        .map(Json)
}

/// # Edit Server Command
#[openapi(tag = "Application Commands")]
#[patch(
    "/<application_id>/servers/<server_id>/commands/<command_id>",
    data = "<data>"
)]
pub async fn edit_server_command(
    db: &State<Database>,
    user: User,
    application_id: &str,
    server_id: &str,
    command_id: &str,
    data: Json<DataEditApplicationCommand>,
) -> Result<Json<ApplicationCommand>> {
    scope(db, &user, application_id, Some(server_id)).await?;
    let command = scoped_command(db, application_id, Some(server_id), command_id).await?;
    let command = edit_application_command(db, command, data.into_inner()).await?;
    notify(db, application_id, command.server_id.as_deref()).await;
    Ok(Json(command))
}

/// # Delete Server Command
#[openapi(tag = "Application Commands")]
#[delete("/<application_id>/servers/<server_id>/commands/<command_id>")]
pub async fn delete_server_command(
    db: &State<Database>,
    user: User,
    application_id: &str,
    server_id: &str,
    command_id: &str,
) -> Result<EmptyResponse> {
    scope(db, &user, application_id, Some(server_id)).await?;
    let command = scoped_command(db, application_id, Some(server_id), command_id).await?;
    db.delete_application_command(&command.id).await?;
    notify(db, application_id, command.server_id.as_deref()).await;
    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::Member;
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};

    fn hello() -> serde_json::Value {
        serde_json::json!({
            "name": "hello",
            "description": "Say hello",
            "description_localizations": { "ar": "قل مرحباً" },
            "options": [{
                "type": "String",
                "name": "name",
                "description": "Who to greet",
                "max_length": 32
            }]
        })
    }

    #[rocket::async_test]
    async fn bot_registers_and_fetches_hello() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (bot, _) = harness.new_bot(&owner).await;

        let response = harness
            .client
            .post(format!("/applications/{}/commands", bot.id))
            .header(Header::new("x-bot-token", bot.token.clone()))
            .header(ContentType::JSON)
            .body(hello().to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let created: v0::ApplicationCommand = response.into_json().await.unwrap();
        assert_eq!(created.name, "hello");
        assert_eq!(created.application_id, bot.id);

        // Stored
        assert_eq!(
            harness.db.fetch_application_command(&created.id).await.unwrap(),
            created
        );

        // Fetch one and list
        let fetched: v0::ApplicationCommand = harness
            .client
            .get(format!("/applications/{}/commands/{}", bot.id, created.id))
            .header(Header::new("x-bot-token", bot.token.clone()))
            .dispatch()
            .await
            .into_json()
            .await
            .unwrap();
        assert_eq!(fetched, created);

        let list: Vec<v0::ApplicationCommand> = harness
            .client
            .get(format!("/applications/{}/commands", bot.id))
            .header(Header::new("x-bot-token", bot.token.clone()))
            .dispatch()
            .await
            .into_json()
            .await
            .unwrap();
        assert_eq!(list, vec![created]);

        // Duplicate name
        let response = harness
            .client
            .post(format!("/applications/{}/commands", bot.id))
            .header(Header::new("x-bot-token", bot.token.clone()))
            .header(ContentType::JSON)
            .body(hello().to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Conflict);
    }

    #[rocket::async_test]
    async fn owner_session_may_manage_others_may_not() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, other_session, other) = harness.new_user().await;
        let (bot, _) = harness.new_bot(&owner).await;
        let (other_bot, _) = harness.new_bot(&other).await;

        let sync = |token: Header<'static>| {
            harness
                .client
                .put(format!("/applications/{}/commands", bot.id))
                .header(token)
                .header(ContentType::JSON)
                .body(serde_json::json!([hello()]).to_string())
        };

        let response = sync(Header::new("x-session-token", owner_session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        drop(response);

        // Another user, and another bot, look like a missing application
        for header in [
            Header::new("x-session-token", other_session.token.clone()),
            Header::new("x-bot-token", other_bot.token.clone()),
        ] {
            let response = sync(header).dispatch().await;
            assert_eq!(response.status(), Status::NotFound);
        }
    }

    #[rocket::async_test]
    async fn invalid_definitions_are_rejected() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (bot, _) = harness.new_bot(&owner).await;

        for body in [
            serde_json::json!({ "name": "Hello", "description": "x" }),
            serde_json::json!({ "name": "hello", "description": "" }),
            serde_json::json!([{ "name": "a", "description": "x" }, { "name": "a", "description": "y" }]),
        ] {
            let request = if body.is_array() {
                harness.client.put(format!("/applications/{}/commands", bot.id))
            } else {
                harness.client.post(format!("/applications/{}/commands", bot.id))
            };
            let response = request
                .header(Header::new("x-bot-token", bot.token.clone()))
                .header(ContentType::JSON)
                .body(body.to_string())
                .dispatch()
                .await;
            assert_eq!(response.status(), Status::BadRequest, "{body}");
        }
        assert!(harness
            .db
            .fetch_application_commands(&bot.id, None)
            .await
            .unwrap()
            .is_empty());
    }

    #[rocket::async_test]
    async fn server_commands_need_installation() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (bot, bot_user) = harness.new_bot(&owner).await;
        let (server, _) = harness.new_server(&owner).await;

        let create = || {
            harness
                .client
                .post(format!(
                    "/applications/{}/servers/{}/commands",
                    bot.id, server.id
                ))
                .header(Header::new("x-bot-token", bot.token.clone()))
                .header(ContentType::JSON)
                .body(hello().to_string())
        };

        let response = create().dispatch().await;
        assert_eq!(response.status(), Status::Forbidden);
        drop(response);

        Member::create(&harness.db, &server, &bot_user, None)
            .await
            .unwrap();

        let response = create().dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let created: v0::ApplicationCommand = response.into_json().await.unwrap();
        assert_eq!(created.server_id.as_deref(), Some(server.id.as_str()));

        // A server command is not reachable through the global routes
        let response = harness
            .client
            .delete(format!("/applications/{}/commands/{}", bot.id, created.id))
            .header(Header::new("x-bot-token", bot.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);
        drop(response);

        let response = harness
            .client
            .delete(format!(
                "/applications/{}/servers/{}/commands/{}",
                bot.id, server.id, created.id
            ))
            .header(Header::new("x-bot-token", bot.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NoContent);
    }
}
