//! Permissions of a category: channels synced with it follow them.

use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{sync_voice_permissions, VoiceClient},
    AuditLogEntryAction, Database, PartialServer, Server, User,
};
use revolt_models::v0;
use revolt_permissions::{
    calculate_server_permissions, ChannelPermission, Override, OverrideField,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::audit_log_reason::AuditLogReason;

/// Which override of the category is being set
enum Target<'a> {
    Default,
    Role(&'a str),
}

async fn set(
    db: &Database,
    voice_client: &VoiceClient,
    user: User,
    reason: AuditLogReason,
    mut server: Server,
    category_id: &str,
    target: Target<'_>,
    value: Override,
) -> Result<Json<v0::Server>> {
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    let permissions = calculate_server_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;

    let categories = server.categories.clone().unwrap_or_default();
    let category = categories
        .iter()
        .find(|category| category.id == category_id)
        .ok_or_else(|| create_error!(NotFound))?;

    let current: Option<OverrideField> = match target {
        Target::Default => category.default_permissions,
        Target::Role(role_id) => {
            let role = server
                .roles
                .get(role_id)
                .ok_or_else(|| create_error!(NotFound))?;
            // nobody sets permissions for a role at or above their own
            if server.owner != user.id && role.rank <= query.get_member_rank().unwrap_or(i64::MIN)
            {
                return Err(create_error!(NotElevated));
            }
            category.role_permissions.get(role_id).copied()
        }
    };
    permissions
        .throw_permission_override(current.map(|value| value.into()), &value)
        .await?;

    let field: OverrideField = value.into();
    let mut next = categories.clone();
    let entry = next
        .iter_mut()
        .find(|category| category.id == category_id)
        .expect("found above");
    match target {
        Target::Default => entry.default_permissions = Some(field),
        Target::Role(role_id) => {
            if field.a == 0 && field.d == 0 {
                entry.role_permissions.remove(role_id);
            } else {
                entry.role_permissions.insert(role_id.to_string(), field);
            }
        }
    }
    let channels = entry.channels.clone();

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
        .insert(db, server.id.clone(), reason, user.id, None)
        .await;

    // voice rooms synced with the category pick up the change
    let role = match target {
        Target::Role(role_id) => Some(role_id.to_string()),
        Target::Default => None,
    };
    for channel_id in &channels {
        if let Ok(channel) = Reference::from_unchecked(channel_id).as_channel(db).await {
            if channel.synced_category(&server).is_some() {
                sync_voice_permissions(db, voice_client, &channel, Some(&server), role.as_deref())
                    .await?;
            }
        }
    }

    Ok(Json(server.into(db).await))
}

/// # Set Category Role Permission
///
/// Sets a role's permissions for a category. Channels synced with the
/// category use them instead of their own. An empty override removes it.
#[openapi(tag = "Server Permissions")]
#[put(
    "/<target>/categories/<category_id>/permissions/<role_id>",
    data = "<data>",
    rank = 2
)]
#[allow(clippy::too_many_arguments)]
pub async fn set_category_role_permission(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    category_id: String,
    role_id: String,
    data: Json<v0::DataSetRolePermissions>,
) -> Result<Json<v0::Server>> {
    let server = target.as_server(db).await?;
    set(
        db,
        voice_client,
        user,
        reason,
        server,
        &category_id,
        Target::Role(&role_id),
        data.into_inner().permissions,
    )
    .await
}

/// # Set Category Default Permission
///
/// Sets the default permissions for a category. Channels synced with the
/// category use them instead of their own.
#[openapi(tag = "Server Permissions")]
#[put(
    "/<target>/categories/<category_id>/permissions/default",
    data = "<data>",
    rank = 1
)]
pub async fn set_category_default_permission(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    category_id: String,
    data: Json<v0::DataSetRolePermissions>,
) -> Result<Json<v0::Server>> {
    let server = target.as_server(db).await?;
    set(
        db,
        voice_client,
        user,
        reason,
        server,
        &category_id,
        Target::Default,
        data.into_inner().permissions,
    )
    .await
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        util::{permissions::DatabasePermissionQuery, reference::Reference},
        Category, Channel, Member, PartialServer, Server, User,
    };
    use revolt_models::v0;
    use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    const VIEW: u64 = ChannelPermission::ViewChannel as u64;

    struct Setup {
        harness: TestHarness,
        owner_token: String,
        server: Server,
        member: User,
    }

    /// A server with a category "Staff" and a member who is not the owner
    async fn setup() -> Setup {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&owner).await;
        server
            .update(
                &harness.db,
                PartialServer {
                    categories: Some(vec![Category {
                        id: "STAFF".to_string(),
                        title: "Staff".to_string(),
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        let (_, _, member) = harness.new_user().await;
        Member::create(&harness.db, &server, &member, None).await.unwrap();
        Setup {
            harness,
            owner_token: session.token,
            server,
            member,
        }
    }

    impl Setup {
        async fn create_channel(&self, body: serde_json::Value) -> String {
            let channel: v0::Channel = self
                .harness
                .client
                .post(format!("/servers/{}/channels", self.server.id))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", self.owner_token.clone()))
                .body(body.to_string())
                .dispatch()
                .await
                .into_json()
                .await
                .expect("`Channel`");
            channel.id().to_string()
        }

        async fn put(&self, path: String, body: serde_json::Value) -> Status {
            self.harness
                .client
                .put(path)
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", self.owner_token.clone()))
                .body(body.to_string())
                .dispatch()
                .await
                .status()
        }

        async fn hide_category(&self) {
            let status = self
                .put(
                    format!("/servers/{}/categories/STAFF/permissions/default", self.server.id),
                    json!({ "permissions": { "allow": 0, "deny": VIEW } }),
                )
                .await;
            assert_eq!(status, Status::Ok);
        }

        async fn member_sees(&self, channel_id: &str) -> bool {
            let channel = Reference::from_unchecked(channel_id)
                .as_channel(&self.harness.db)
                .await
                .unwrap();
            let mut query = DatabasePermissionQuery::new(&self.harness.db, &self.member)
                .channel(&channel);
            calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::ViewChannel)
        }

        async fn channel(&self, channel_id: &str) -> Channel {
            Reference::from_unchecked(channel_id)
                .as_channel(&self.harness.db)
                .await
                .unwrap()
        }
    }

    #[rocket::async_test]
    async fn a_synced_channel_follows_its_category() {
        let s = setup().await;
        let channel = s
            .create_channel(json!({ "type": "Text", "name": "mods", "category": "STAFF" }))
            .await;
        assert!(matches!(
            s.channel(&channel).await,
            Channel::TextChannel { permissions_synced: true, .. }
        ));
        assert!(s.member_sees(&channel).await);

        s.hide_category().await;
        assert!(!s.member_sees(&channel).await);
    }

    #[rocket::async_test]
    async fn existing_and_private_channels_keep_their_own_permissions() {
        let s = setup().await;
        // made outside the category, then moved in: not synced
        let outside = s.create_channel(json!({ "type": "Text", "name": "general" })).await;
        let mut server = Reference::from_unchecked(&s.server.id)
            .as_server(&s.harness.db)
            .await
            .unwrap();
        let mut categories = server.categories.clone().unwrap();
        categories[0].channels.push(outside.clone());
        server
            .update(
                &s.harness.db,
                PartialServer {
                    categories: Some(categories),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        // a private channel made in the category starts with its own
        let private = s
            .create_channel(
                json!({ "type": "Text", "name": "secret", "category": "STAFF", "private": true }),
            )
            .await;
        assert!(matches!(
            s.channel(&private).await,
            Channel::TextChannel { permissions_synced: false, .. }
        ));

        s.hide_category().await;
        assert!(s.member_sees(&outside).await);
        assert!(!s.member_sees(&private).await);
    }

    #[rocket::async_test]
    async fn editing_a_synced_channel_keeps_a_copy_and_unsyncs_it() {
        let s = setup().await;
        let channel = s
            .create_channel(json!({ "type": "Text", "name": "mods", "category": "STAFF" }))
            .await;
        s.hide_category().await;

        // a change to the channel's role permissions: the category's
        // hiding is kept, the channel stops following the category
        let role = s.harness.new_role(&s.server, 1, None).await;
        let status = s
            .put(
                format!("/channels/{channel}/permissions/{}", role.id),
                json!({ "permissions": { "allow": 0, "deny": 0 } }),
            )
            .await;
        assert_eq!(status, Status::Ok);
        let Channel::TextChannel {
            permissions_synced,
            default_permissions,
            ..
        } = s.channel(&channel).await
        else {
            unreachable!()
        };
        assert!(!permissions_synced);
        assert_eq!(default_permissions.map(|d| d.d as u64), Some(VIEW));
        assert!(!s.member_sees(&channel).await);

        // later category changes no longer reach it
        let status = s
            .put(
                format!("/servers/{}/categories/STAFF/permissions/default", s.server.id),
                json!({ "permissions": { "allow": 0, "deny": 0 } }),
            )
            .await;
        assert_eq!(status, Status::Ok);
        assert!(!s.member_sees(&channel).await);

        // "Sync now" follows the category again
        let status = s
            .harness
            .client
            .post(format!("/channels/{channel}/permissions/sync"))
            .header(Header::new("x-session-token", s.owner_token.clone()))
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::Ok);
        assert!(s.member_sees(&channel).await);
    }

    #[rocket::async_test]
    async fn editing_the_server_never_changes_category_permissions() {
        let s = setup().await;
        s.hide_category().await;
        // a category list sent through server edit: permissions in it are
        // ignored, stored ones are kept, a new category gets none
        let status = s
            .harness
            .client
            .patch(format!("/servers/{}", s.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", s.owner_token.clone()))
            .body(
                json!({ "categories": [
                    { "id": "STAFF", "title": "Staff", "channels": [],
                      "default_permissions": { "a": VIEW, "d": 0 } },
                    { "id": "NEW", "title": "New", "channels": [],
                      "role_permissions": { "X": { "a": VIEW, "d": 0 } } }
                ] })
                .to_string(),
            )
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::Ok);
        let server = Reference::from_unchecked(&s.server.id)
            .as_server(&s.harness.db)
            .await
            .unwrap();
        let categories = server.categories.unwrap();
        assert_eq!(categories[0].default_permissions.map(|d| d.d as u64), Some(VIEW));
        assert_eq!(categories[0].default_permissions.map(|d| d.a), Some(0));
        assert!(categories[1].role_permissions.is_empty());
    }

    #[rocket::async_test]
    async fn only_managers_set_category_permissions() {
        let s = setup().await;
        let (_, session, outsider) = s.harness.new_user().await;
        Member::create(&s.harness.db, &s.server, &outsider, None).await.unwrap();
        let status = s
            .harness
            .client
            .put(format!("/servers/{}/categories/STAFF/permissions/default", s.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token))
            .body(json!({ "permissions": { "allow": 0, "deny": VIEW } }).to_string())
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::Forbidden);
        let status = s
            .put(
                format!("/servers/{}/categories/NOPE/permissions/default", s.server.id),
                json!({ "permissions": { "allow": 0, "deny": VIEW } }),
            )
            .await;
        assert_eq!(status, Status::NotFound);
    }
}
