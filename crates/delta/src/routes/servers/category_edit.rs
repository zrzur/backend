//! A category's own information: its title and its note.

use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Database, PartialServer, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// One line of plain text: no line breaks or control characters
fn one_line(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

/// # Edit Category
///
/// Change a category's title or note. An empty note removes it. The
/// category's channels and permissions are not touched.
#[openapi(tag = "Server Information")]
#[patch("/<target>/categories/<category_id>", data = "<data>")]
pub async fn edit_category(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    category_id: String,
    data: Json<v0::DataEditCategory>,
) -> Result<Json<v0::Server>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let mut server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;

    let categories = server.categories.clone().unwrap_or_default();
    let mut next = categories.clone();
    let category = next
        .iter_mut()
        .find(|category| category.id == category_id)
        .ok_or_else(|| create_error!(NotFound))?;

    if let Some(title) = data.title {
        let title = title.trim().to_string();
        if title.is_empty() || !one_line(&title) {
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::length("title", 1, 32),
            ]));
        }
        category.title = title;
    }
    if let Some(description) = data.description {
        let description = description.trim().to_string();
        if !one_line(&description) {
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::length("description", 0, 120),
            ]));
        }
        category.description = (!description.is_empty()).then_some(description);
    }

    if next != categories {
        let before = PartialServer {
            categories: Some(categories),
            ..Default::default()
        };
        let after = PartialServer {
            categories: Some(next),
            ..Default::default()
        };
        server.update(db, after.clone(), vec![]).await?;

        AuditLogEntryAction::ServerEdit { before, after }
            .insert(db, server.id.clone(), reason, user.id.clone(), None)
            .await;
    }

    Ok(Json(server.into_for(db, &user).await))
}

#[cfg(test)]
mod test {
    use crate::util::test::TestHarness;
    use revolt_database::{
        util::{permissions::DatabasePermissionQuery, reference::Reference},
        Category, Channel, Member, PartialMember, PartialServer, Server, User,
    };
    use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
    use rocket::http::{ContentType, Header, Status};
    use serde_json::{json, Value};

    const VIEW: u64 = ChannelPermission::ViewChannel as u64;

    struct Setup {
        harness: TestHarness,
        owner_token: String,
        server: Server,
        member: User,
        member_token: String,
    }

    /// A server with the categories "Staff" and "Open", and a plain member
    async fn setup() -> Setup {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&owner).await;
        server
            .update(
                &harness.db,
                PartialServer {
                    categories: Some(vec![
                        Category {
                            id: "STAFF".to_string(),
                            title: "Staff".to_string(),
                            ..Default::default()
                        },
                        Category {
                            id: "OPEN".to_string(),
                            title: "Open".to_string(),
                            ..Default::default()
                        },
                    ]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        Member::create(&harness.db, &server, &owner, None)
            .await
            .unwrap();
        let (_, member_session, member) = harness.new_user().await;
        Member::create(&harness.db, &server, &member, None)
            .await
            .unwrap();
        Setup {
            harness,
            owner_token: session.token,
            server,
            member,
            member_token: member_session.token,
        }
    }

    impl Setup {
        async fn send(
            &self,
            method: rocket::http::Method,
            token: &str,
            path: String,
            body: Value,
        ) -> (Status, Value) {
            let response = self
                .harness
                .client
                .req(method, path)
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", token.to_string()))
                .body(body.to_string())
                .dispatch()
                .await;
            let status = response.status();
            (status, response.into_json().await.unwrap_or(Value::Null))
        }

        async fn owner(&self, method: rocket::http::Method, path: String, body: Value) -> Status {
            self.send(method, &self.owner_token, path, body).await.0
        }

        async fn create_channel(&self, body: Value) -> String {
            let (status, channel) = self
                .send(
                    rocket::http::Method::Post,
                    &self.owner_token,
                    format!("/servers/{}/channels", self.server.id),
                    body,
                )
                .await;
            assert_eq!(status, Status::Ok);
            channel["_id"].as_str().unwrap().to_string()
        }

        async fn hide(&self, category: &str) {
            let status = self
                .owner(
                    rocket::http::Method::Put,
                    format!(
                        "/servers/{}/categories/{category}/permissions/default",
                        self.server.id
                    ),
                    json!({ "permissions": { "allow": 0, "deny": VIEW } }),
                )
                .await;
            assert_eq!(status, Status::Ok);
        }

        /// The category titles the server fetch gives this session
        async fn titles(&self, token: &str) -> Vec<String> {
            let (status, server) = self
                .send(
                    rocket::http::Method::Get,
                    token,
                    format!("/servers/{}", self.server.id),
                    Value::Null,
                )
                .await;
            assert_eq!(status, Status::Ok);
            server["categories"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|c| c["title"].as_str().unwrap().to_string())
                        .collect()
                })
                .unwrap_or_default()
        }

        async fn stored(&self) -> Server {
            Reference::from_unchecked(&self.server.id)
                .as_server(&self.harness.db)
                .await
                .unwrap()
        }

        async fn channel(&self, id: &str) -> Channel {
            Reference::from_unchecked(id)
                .as_channel(&self.harness.db)
                .await
                .unwrap()
        }

        async fn member_sees(&self, channel_id: &str) -> bool {
            let channel = self.channel(channel_id).await;
            let mut query =
                DatabasePermissionQuery::new(&self.harness.db, &self.member).channel(&channel);
            calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::ViewChannel)
        }

        fn synced(channel: &Channel) -> bool {
            matches!(
                channel,
                Channel::TextChannel {
                    permissions_synced: true,
                    ..
                }
            )
        }
    }

    use rocket::http::Method::{Patch, Put};

    #[rocket::async_test]
    async fn a_manager_edits_title_and_note() {
        let s = setup().await;
        let path = format!("/servers/{}/categories/STAFF", s.server.id);

        // a plain member may not
        let (status, _) = s
            .send(Patch, &s.member_token, path.clone(), json!({ "title": "Mine" }))
            .await;
        assert_eq!(status, Status::Forbidden);

        assert_eq!(
            s.owner(
                Patch,
                path.clone(),
                json!({ "title": "  Team  ", "description": "For the moderation team" })
            )
            .await,
            Status::Ok
        );
        let categories = s.stored().await.categories.unwrap();
        assert_eq!(categories[0].title, "Team");
        assert_eq!(
            categories[0].description.as_deref(),
            Some("For the moderation team")
        );
        assert_eq!(categories[1].title, "Open");

        // refused: empty title, too long, line breaks, unknown category
        for bad in [
            json!({ "title": "   " }),
            json!({ "title": "x".repeat(33) }),
            json!({ "description": "x".repeat(121) }),
            json!({ "description": "two\nlines" }),
        ] {
            let status = s.owner(Patch, path.clone(), bad.clone()).await;
            assert!(matches!(status.code, 400 | 422), "{bad}");
        }
        assert_eq!(
            s.owner(
                Patch,
                format!("/servers/{}/categories/NOPE", s.server.id),
                json!({ "title": "x" })
            )
            .await,
            Status::NotFound
        );

        // a category list sent through server edit (a reorder by an app
        // that does not know the note) keeps the note
        assert_eq!(
            s.owner(
                Patch,
                format!("/servers/{}", s.server.id),
                json!({ "categories": [
                    { "id": "OPEN", "title": "Open", "channels": [] },
                    { "id": "STAFF", "title": "Team", "channels": [], "description": "changed" }
                ] })
            )
            .await,
            Status::Ok
        );
        let categories = s.stored().await.categories.unwrap();
        assert_eq!(categories[1].id, "STAFF");
        assert_eq!(
            categories[1].description.as_deref(),
            Some("For the moderation team")
        );

        // an empty note removes it
        assert_eq!(
            s.owner(Patch, path, json!({ "description": "" })).await,
            Status::Ok
        );
        assert_eq!(s.stored().await.categories.unwrap()[1].description, None);
    }

    #[rocket::async_test]
    async fn a_private_category_is_not_told_to_people_outside_it() {
        let s = setup().await;
        let channel = s
            .create_channel(json!({ "type": "Text", "name": "mods", "category": "STAFF" }))
            .await;
        assert_eq!(s.titles(&s.member_token).await, vec!["Staff", "Open"]);

        s.hide("STAFF").await;
        assert_eq!(s.titles(&s.member_token).await, vec!["Open"]);
        assert_eq!(s.titles(&s.owner_token).await, vec!["Staff", "Open"]);
        // with its channels, too
        let (status, with_channels) = s
            .send(
                rocket::http::Method::Get,
                &s.member_token,
                format!("/servers/{}?include_channels=true", s.server.id),
                Value::Null,
            )
            .await;
        assert_eq!(status, Status::Ok);
        assert_eq!(with_channels["categories"].as_array().unwrap().len(), 1);
        assert!(!with_channels["channels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["_id"] == channel.as_str()));
        // and the channel itself is refused
        let (status, _) = s
            .send(
                rocket::http::Method::Get,
                &s.member_token,
                format!("/channels/{channel}"),
                Value::Null,
            )
            .await;
        assert_eq!(status, Status::Forbidden);

        // a role that is let in sees it again
        let role = s.harness.new_role(&s.server, 5, None).await;
        assert_eq!(
            s.owner(
                Put,
                format!(
                    "/servers/{}/categories/STAFF/permissions/{}",
                    s.server.id, role.id
                ),
                json!({ "permissions": { "allow": VIEW, "deny": 0 } })
            )
            .await,
            Status::Ok
        );
        assert_eq!(s.titles(&s.member_token).await, vec!["Open"]);
        let mut member = Reference::from_unchecked(&s.member.id)
            .as_member(&s.harness.db, &s.server.id)
            .await
            .unwrap();
        member
            .update(
                &s.harness.db,
                PartialMember {
                    roles: Some(vec![role.id.clone()]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(s.titles(&s.member_token).await, vec!["Staff", "Open"]);
        assert!(s.member_sees(&channel).await);
    }

    #[rocket::async_test]
    async fn a_visible_channel_inside_shows_its_category() {
        let s = setup().await;
        let channel = s
            .create_channel(json!({ "type": "Text", "name": "plans", "category": "STAFF" }))
            .await;
        s.hide("STAFF").await;
        assert_eq!(s.titles(&s.member_token).await, vec!["Open"]);

        // the channel gets its own permissions and lets everyone in
        assert_eq!(
            s.owner(
                Put,
                format!("/channels/{channel}/permissions/default"),
                json!({ "permissions": { "allow": VIEW, "deny": 0 } })
            )
            .await,
            Status::Ok
        );
        assert!(s.member_sees(&channel).await);
        assert_eq!(s.titles(&s.member_token).await, vec!["Staff", "Open"]);
    }

    #[rocket::async_test]
    async fn deleting_a_category_keeps_its_channels_hidden() {
        let s = setup().await;
        let channel = s
            .create_channel(json!({ "type": "Text", "name": "mods", "category": "STAFF" }))
            .await;
        s.hide("STAFF").await;
        assert!(!s.member_sees(&channel).await);

        assert_eq!(
            s.owner(
                Patch,
                format!("/servers/{}", s.server.id),
                json!({ "categories": [{ "id": "OPEN", "title": "Open", "channels": [] }] })
            )
            .await,
            Status::Ok
        );
        let stored = s.channel(&channel).await;
        assert!(!Setup::synced(&stored));
        assert!(!s.member_sees(&channel).await);
    }

    #[rocket::async_test]
    async fn moving_a_channel_never_changes_who_sees_it() {
        let s = setup().await;
        let hidden = s
            .create_channel(json!({ "type": "Text", "name": "mods", "category": "STAFF" }))
            .await;
        let open = s
            .create_channel(json!({ "type": "Text", "name": "chat", "category": "OPEN" }))
            .await;
        s.hide("STAFF").await;
        let save = |staff: Vec<&str>, open: Vec<&str>| {
            json!({ "categories": [
                { "id": "STAFF", "title": "Staff", "channels": staff },
                { "id": "OPEN", "title": "Open", "channels": open }
            ] })
        };
        let path = format!("/servers/{}", s.server.id);

        // a reorder that moves nothing changes nothing
        assert_eq!(
            s.owner(Patch, path.clone(), save(vec![&hidden], vec![&open])).await,
            Status::Ok
        );
        assert!(Setup::synced(&s.channel(&hidden).await));

        // hidden channel into the open category: still hidden, own permissions
        // open channel into the hidden category: still open, own permissions
        assert_eq!(
            s.owner(Patch, path.clone(), save(vec![&open], vec![&hidden])).await,
            Status::Ok
        );
        assert!(!s.member_sees(&hidden).await);
        assert!(s.member_sees(&open).await);
        assert!(!Setup::synced(&s.channel(&hidden).await));
        assert!(!Setup::synced(&s.channel(&open).await));

        // back where its own permissions say the same as the category: it
        // follows the category again
        assert_eq!(
            s.owner(Patch, path.clone(), save(vec![&hidden], vec![&open])).await,
            Status::Ok
        );
        assert!(Setup::synced(&s.channel(&hidden).await));
        assert!(Setup::synced(&s.channel(&open).await));
        assert!(!s.member_sees(&hidden).await);
        assert!(s.member_sees(&open).await);
    }
}
