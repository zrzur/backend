use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{sync_voice_permissions, VoiceClient},
    AuditLogEntryAction, Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, Override, OverrideField, PermissionQuery, calculate_channel_permissions};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::audit_log_reason::AuditLogReason;

/// # Set Role Permission
///
/// Sets permissions for the specified role in this channel.
///
/// Channel must be a `TextChannel`.
#[openapi(tag = "Channel Permissions")]
#[put("/<target>/permissions/<role_id>", data = "<data>", rank = 2)]
pub async fn set_role_permissions(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    role_id: String,
    data: Json<v0::DataSetRolePermissions>,
) -> Result<Json<v0::Channel>> {
    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions: revolt_permissions::PermissionValue =
        calculate_channel_permissions(&mut query).await;

    query.set_server_from_channel().await;

    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;

    if let Some(server) = query.server_ref() {
        if let Some(role) = server.roles.get(&role_id) {
            if server.owner != user.id
                && role.rank <= query.get_member_rank().unwrap_or(i64::MIN)
            {
                return Err(create_error!(NotElevated));
            }

            // Compare with what applies to the role here now (the category's
            // when the channel is synced; not the role's server permissions),
            // so lifting a channel deny needs the same permission as setting
            // it, as for default permissions.
            let current_value: Option<Override> = channel
                .effective_overrides(Some(server))
                .and_then(|(_, roles)| roles.get(&role_id))
                .map(|value| (*value).into());
            permissions
                .throw_permission_override(current_value, &data.permissions)
                .await?;

            let mut new_channel = channel.clone();
            let override_field: OverrideField = data.permissions.clone().into();
            let server_id = server.id.clone();

            // editing a synced channel stops it following its category
            new_channel.unsync_permissions(db, server).await?;

            new_channel
                .set_role_permission(db, &role_id, data.permissions.clone().into())
                .await?;

            sync_voice_permissions(db, voice_client, &new_channel, Some(server), Some(&role_id))
                .await?;

            AuditLogEntryAction::ChannelRolePermissionsEdit {
                channel: new_channel.id().to_string(),
                role: role_id,
                permissions: override_field,
            }
            .insert(db, server_id, reason, user.id, None)
            .await;

            Ok(Json(new_channel.into()))
        } else {
            Err(create_error!(NotFound))
        }
    } else {
        Err(create_error!(InvalidOperation))
    }
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        util::permissions::DatabasePermissionQuery, Channel, Member, PartialMember, Role, Server,
        User,
    };
    use revolt_permissions::{calculate_channel_permissions, ChannelPermission, OverrideField};
    use rocket::http::{ContentType, Header, Status};

    const MANAGE_MESSAGES: i64 = ChannelPermission::ManageMessages as i64;
    const SEND_MESSAGE: i64 = ChannelPermission::SendMessage as i64;

    /// zrzur-u3-01: a moderator who may manage permissions but not messages,
    /// and a member role that may manage messages server-wide.
    struct Setup {
        harness: TestHarness,
        channel: Channel,
        moderator_role: Role,
        member_role: Role,
        member: User,
        owner_token: String,
        moderator_token: String,
    }

    async fn setup() -> Setup {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, moderator_session, moderator) = harness.new_user().await;
        let (_, _, member) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        let channel = harness.new_channel(&server).await;

        // Role ranks come from the role count, so read the server again
        // after each role; the first role outranks the second.
        let moderator_role = harness
            .new_role(
                &server,
                0,
                Some(OverrideField {
                    a: ChannelPermission::ManagePermissions as i64,
                    d: 0,
                }),
            )
            .await;
        let server = harness.db.fetch_server(&server.id).await.unwrap();
        let member_role = harness
            .new_role(
                &server,
                1,
                Some(OverrideField {
                    a: MANAGE_MESSAGES,
                    d: 0,
                }),
            )
            .await;
        let server = harness.db.fetch_server(&server.id).await.unwrap();

        join(&harness, &server, &moderator, &moderator_role).await;
        join(&harness, &server, &member, &member_role).await;

        Setup {
            harness,
            channel,
            moderator_role,
            member_role,
            member,
            owner_token: owner_session.token,
            moderator_token: moderator_session.token,
        }
    }

    async fn join(harness: &TestHarness, server: &Server, user: &User, role: &Role) {
        let (mut member, _) = Member::create(&harness.db, server, user, None)
            .await
            .expect("member");
        member
            .update(
                &harness.db,
                PartialMember {
                    roles: Some(vec![role.id.clone()]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("member roles");
    }

    /// Set a role override on the channel directly, as the owner had done.
    async fn owner_override(setup: &Setup, role: &Role, a: i64, d: i64) {
        let mut channel = setup.harness.db.fetch_channel(setup.channel.id()).await.unwrap();
        channel
            .set_role_permission(&setup.harness.db, &role.id, OverrideField { a, d })
            .await
            .unwrap();
    }

    async fn put(setup: &Setup, token: &str, role: &Role, allow: i64, deny: i64) -> Status {
        setup
            .harness
            .client
            .put(format!(
                "/channels/{}/permissions/{}",
                setup.channel.id(),
                role.id
            ))
            .header(ContentType::JSON)
            .header(Header::new("X-Session-Token", token.to_string()))
            .body(json!({ "permissions": { "allow": allow, "deny": deny } }).to_string())
            .dispatch()
            .await
            .status()
    }

    async fn channel_override(setup: &Setup, role: &Role) -> Option<OverrideField> {
        match setup.harness.db.fetch_channel(setup.channel.id()).await.unwrap() {
            Channel::TextChannel {
                role_permissions, ..
            } => role_permissions.get(&role.id).copied(),
            _ => None,
        }
    }

    async fn member_can_manage_messages(setup: &Setup) -> bool {
        let channel = setup.harness.db.fetch_channel(setup.channel.id()).await.unwrap();
        let mut query = DatabasePermissionQuery::new(&setup.harness.db, &setup.member).channel(&channel);
        calculate_channel_permissions(&mut query)
            .await
            .has_channel_permission(ChannelPermission::ManageMessages)
    }

    #[rocket::async_test]
    async fn moderator_cannot_lift_deny_they_do_not_hold() {
        let setup = setup().await;
        owner_override(&setup, &setup.member_role, 0, MANAGE_MESSAGES).await;

        let status = put(&setup, &setup.moderator_token, &setup.member_role, 0, 0).await;

        assert_eq!(status, Status::Forbidden);
        assert_eq!(
            channel_override(&setup, &setup.member_role).await,
            Some(OverrideField { a: 0, d: MANAGE_MESSAGES })
        );
        assert!(!member_can_manage_messages(&setup).await);
    }

    #[rocket::async_test]
    async fn moderator_can_lift_deny_they_hold() {
        let setup = setup().await;
        owner_override(&setup, &setup.member_role, 0, SEND_MESSAGE).await;

        let status = put(&setup, &setup.moderator_token, &setup.member_role, 0, 0).await;

        assert_eq!(status, Status::Ok);
        assert_eq!(
            channel_override(&setup, &setup.member_role).await,
            Some(OverrideField { a: 0, d: 0 })
        );
    }

    #[rocket::async_test]
    async fn moderator_cannot_grant_permission_they_lack() {
        let setup = setup().await;

        let status = put(&setup, &setup.moderator_token, &setup.member_role, MANAGE_MESSAGES, 0).await;

        assert_eq!(status, Status::Forbidden);
        assert_eq!(channel_override(&setup, &setup.member_role).await, None);
    }

    #[rocket::async_test]
    async fn moderator_can_grant_then_deny_permission_they_hold() {
        let setup = setup().await;

        let status = put(&setup, &setup.moderator_token, &setup.member_role, SEND_MESSAGE, 0).await;
        assert_eq!(status, Status::Ok);

        let status = put(&setup, &setup.moderator_token, &setup.member_role, 0, SEND_MESSAGE).await;
        assert_eq!(status, Status::Ok);
        assert_eq!(
            channel_override(&setup, &setup.member_role).await,
            Some(OverrideField { a: 0, d: SEND_MESSAGE })
        );
    }

    #[rocket::async_test]
    async fn moderator_keeps_owner_grant_while_editing() {
        let setup = setup().await;
        owner_override(&setup, &setup.member_role, MANAGE_MESSAGES, 0).await;

        // The owner's grant stays as it is; only the new bit is checked.
        let status = put(
            &setup,
            &setup.moderator_token,
            &setup.member_role,
            MANAGE_MESSAGES | SEND_MESSAGE,
            0,
        )
        .await;

        assert_eq!(status, Status::Ok);
    }

    #[rocket::async_test]
    async fn owner_can_lift_any_deny() {
        let setup = setup().await;
        owner_override(&setup, &setup.member_role, 0, MANAGE_MESSAGES).await;
        assert!(!member_can_manage_messages(&setup).await);

        let status = put(&setup, &setup.owner_token, &setup.member_role, 0, 0).await;

        assert_eq!(status, Status::Ok);
        // The member role's server permission applies again.
        assert!(member_can_manage_messages(&setup).await);
    }

    #[rocket::async_test]
    async fn moderator_cannot_edit_own_role() {
        let setup = setup().await;

        let status = put(&setup, &setup.moderator_token, &setup.moderator_role, 0, 0).await;

        assert_eq!(status, Status::Forbidden);
    }
}
