use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::AuditLogEntryAction;
use revolt_database::{util::reference::Reference, Channel, Database, User};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};

use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// # Create Channel
///
/// Create a new Text or Voice channel.
#[openapi(tag = "Server Information")]
#[post("/<server>/channels", data = "<data>")]
pub async fn create_server_channel(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    server: Reference<'_>,
    data: Json<v0::DataCreateServerChannel>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    if let Some(voice) = &data.voice {
        voice.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    }

    let mut server = server.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;

    // a forum can't be added while the server has forums switched off
    if matches!(data.channel_type, v0::LegacyServerChannelType::Forum) && server.forums_disabled {
        return Err(create_error!(ForumsDisabled));
    }

    let channel_name = data.name.clone();

    let channel = Channel::create_server_channel(db, &mut server, data, true).await?;

    AuditLogEntryAction::ChannelCreate {
        channel: channel.id().to_string(),
        name: channel_name,
    }
    .insert(db, server.id, reason, user.id, None)
    .await;

    Ok(Json(channel.into()))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Category, PartialServer};
    use rocket::http::{ContentType, Header, Status};

    /// A bot or moderator creates a channel straight into a category
    #[rocket::async_test]
    async fn create_into_a_category() {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&owner).await;
        server
            .update(
                &harness.db,
                PartialServer {
                    categories: Some(vec![Category {
                        id: "TICKETS".to_string(),
                        title: "Tickets".to_string(),
                        channels: vec![],
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        let create = |category: &str| {
            harness
                .client
                .post(format!("/servers/{}/channels", server.id))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", session.token.clone()))
                .body(json!({ "type": "Text", "name": "ticket-0001", "category": category }).to_string())
        };

        let response = create("TICKETS").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let channel: serde_json::Value = response.into_json().await.unwrap();
        let id = channel["_id"].as_str().unwrap().to_string();
        let server = harness.db.fetch_server(&server.id).await.unwrap();
        let tickets = server.categories.unwrap();
        assert_eq!(tickets[0].channels, vec![id.clone()]);
        assert!(server.channels.contains(&id));

        // a category the server doesn't have: nothing is created
        let response = create("NOPE").dispatch().await;
        assert_eq!(response.status(), Status::NotFound);
        let after = harness.db.fetch_server(&server.id).await.unwrap();
        assert_eq!(after.channels.len(), server.channels.len());
    }

    /// A private channel is hidden from members until they are given access
    #[rocket::async_test]
    async fn create_a_private_channel_with_a_description() {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        let (_, member, member_user) = harness.new_user().await;
        revolt_database::Member::create(&harness.db, &server, &member_user, None)
            .await
            .unwrap();

        let response = harness
            .client
            .post(format!("/servers/{}/channels", server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(
                json!({ "type": "Text", "name": "staff", "description": "Moderators only", "private": true })
                    .to_string(),
            )
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let channel: serde_json::Value = response.into_json().await.unwrap();
        assert_eq!(channel["description"], "Moderators only");
        let id = channel["_id"].as_str().unwrap();

        let response = harness
            .client
            .get(format!("/channels/{id}"))
            .header(Header::new("x-session-token", member.token.clone()))
            .dispatch()
            .await;
        assert_ne!(response.status(), Status::Ok);

        let response = harness
            .client
            .get(format!("/channels/{id}"))
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
    }
}
