use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{sync_voice_permissions, VoiceClient},
    AuditLogEntryAction, Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{
    calculate_channel_permissions, ChannelPermission, Override, OverrideField, PermissionQuery,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::audit_log_reason::AuditLogReason;

/// # Set Member Permission
///
/// Sets one member's permissions in this channel, applied after every role
/// (for example a ticket only its author and the staff can see). An empty
/// override (allow 0, deny 0) removes it.
///
/// Channel must be a `TextChannel`. Needs Manage Permissions in the channel,
/// a higher rank than the member, and only the permissions you hold yourself.
#[openapi(tag = "Channel Permissions")]
#[put("/<target>/permissions/members/<user_id>", data = "<data>", rank = 1)]
pub async fn set_member_permissions(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    user_id: String,
    data: Json<v0::DataSetRolePermissions>,
) -> Result<Json<v0::Channel>> {
    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions: revolt_permissions::PermissionValue =
        calculate_channel_permissions(&mut query).await;

    query.set_server_from_channel().await;

    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;

    let Some(server) = query.server_ref() else {
        return Err(create_error!(InvalidOperation));
    };

    // only members of this server; others only below one's own rank. One's own
    // entry is allowed (a bot keeps itself in a room it hides from everyone);
    // the override check below limits it to permissions already held.
    let member = db.fetch_member(&server.id, &user_id).await?;
    if server.owner != user.id && user_id != user.id {
        if user_id == server.owner
            || member.get_ranking(server) <= query.get_member_rank().unwrap_or(i64::MIN)
        {
            return Err(create_error!(NotElevated));
        }
    }

    // as for roles: compare with what the channel holds for this member now
    let current_value: Option<Override> = match &channel {
        Channel::TextChannel {
            member_permissions, ..
        } => member_permissions.get(&user_id).map(|value| (*value).into()),
        _ => None,
    };
    permissions
        .throw_permission_override(current_value, &data.permissions)
        .await?;

    let mut new_channel = channel.clone();
    let override_field: OverrideField = data.permissions.clone().into();
    let server_id = server.id.clone();
    new_channel
        .set_member_permission(db, &user_id, override_field)
        .await?;

    sync_voice_permissions(db, voice_client, &new_channel, Some(server), None).await?;

    AuditLogEntryAction::ChannelMemberPermissionsEdit {
        channel: new_channel.id().to_string(),
        member: user_id,
        permissions: override_field,
    }
    .insert(db, server_id, reason, user.id, None)
    .await;

    Ok(Json(new_channel.into()))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        util::permissions::DatabasePermissionQuery, Channel, Member, PartialMember, Role, Server, User,
    };
    use revolt_permissions::{calculate_channel_permissions, ChannelPermission, OverrideField};
    use rocket::http::{ContentType, Header, Status};

    const VIEW: i64 = ChannelPermission::ViewChannel as i64;
    const SEND: i64 = ChannelPermission::SendMessage as i64;
    const MANAGE_MESSAGES: i64 = ChannelPermission::ManageMessages as i64;

    /// A server with a hidden channel, a moderator role that may manage
    /// permissions (and view and send), a member and an outsider
    struct Setup {
        harness: TestHarness,
        server: Server,
        channel: Channel,
        moderator: User,
        moderator_token: String,
        owner_token: String,
        member: User,
        outsider: User,
        moderator_role: Role,
    }

    async fn setup() -> Setup {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, moderator_session, moderator) = harness.new_user().await;
        let (_, _, member) = harness.new_user().await;
        let (_, _, outsider) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        let mut channel = harness.new_channel(&server).await;
        channel
            .update(
                &harness.db,
                revolt_database::PartialChannel {
                    default_permissions: Some(OverrideField { a: 0, d: VIEW }),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        let moderator_role = harness
            .new_role(
                &server,
                0,
                Some(OverrideField {
                    a: ChannelPermission::ManagePermissions as i64 | VIEW | SEND,
                    d: 0,
                }),
            )
            .await;
        let server = harness.db.fetch_server(&server.id).await.unwrap();
        join(&harness, &server, &moderator, Some(&moderator_role)).await;
        join(&harness, &server, &member, None).await;
        Setup {
            harness,
            server,
            channel,
            moderator,
            moderator_token: moderator_session.token,
            owner_token: owner_session.token,
            member,
            outsider,
            moderator_role,
        }
    }

    async fn join(harness: &TestHarness, server: &Server, user: &User, role: Option<&Role>) {
        let (mut member, _) = Member::create(&harness.db, server, user, None).await.unwrap();
        if let Some(role) = role {
            member
                .update(
                    &harness.db,
                    PartialMember { roles: Some(vec![role.id.clone()]), ..Default::default() },
                    vec![],
                )
                .await
                .unwrap();
        }
    }

    async fn put(s: &Setup, token: &str, user: &User, allow: i64, deny: i64) -> Status {
        s.harness
            .client
            .put(format!("/channels/{}/permissions/members/{}", s.channel.id(), user.id))
            .header(ContentType::JSON)
            .header(Header::new("X-Session-Token", token.to_string()))
            .body(json!({ "permissions": { "allow": allow, "deny": deny } }).to_string())
            .dispatch()
            .await
            .status()
    }

    async fn can(s: &Setup, user: &User, permission: ChannelPermission) -> bool {
        let channel = s.harness.db.fetch_channel(s.channel.id()).await.unwrap();
        let mut query = DatabasePermissionQuery::new(&s.harness.db, user).channel(&channel);
        calculate_channel_permissions(&mut query).await.has_channel_permission(permission)
    }

    async fn stored(s: &Setup, user: &User) -> Option<OverrideField> {
        match s.harness.db.fetch_channel(s.channel.id()).await.unwrap() {
            Channel::TextChannel { member_permissions, .. } => member_permissions.get(&user.id).copied(),
            _ => None,
        }
    }

    #[rocket::async_test]
    async fn a_member_sees_a_hidden_channel_once_granted() {
        let s = setup().await;
        assert!(!can(&s, &s.member, ChannelPermission::ViewChannel).await);

        assert_eq!(put(&s, &s.moderator_token, &s.member, VIEW | SEND, 0).await, Status::Ok);
        assert!(can(&s, &s.member, ChannelPermission::ViewChannel).await);
        assert!(can(&s, &s.member, ChannelPermission::SendMessage).await);
        // nobody else
        let (_, _, other) = s.harness.new_user().await;
        join(&s.harness, &s.server, &other, None).await;
        assert!(!can(&s, &other, ChannelPermission::ViewChannel).await);

        // an empty override removes the entry again
        assert_eq!(put(&s, &s.moderator_token, &s.member, 0, 0).await, Status::Ok);
        assert_eq!(stored(&s, &s.member).await, None);
        assert!(!can(&s, &s.member, ChannelPermission::ViewChannel).await);
    }

    #[rocket::async_test]
    async fn a_member_override_wins_over_roles() {
        let s = setup().await;
        // the moderator role may send; the owner denies it for this one person
        assert!(can(&s, &s.moderator, ChannelPermission::SendMessage).await);
        assert_eq!(put(&s, &s.owner_token, &s.moderator, 0, SEND).await, Status::Ok);
        assert!(can(&s, &s.moderator, ChannelPermission::ViewChannel).await);
        assert!(!can(&s, &s.moderator, ChannelPermission::SendMessage).await);
    }

    #[rocket::async_test]
    async fn no_one_raises_themselves_or_their_betters() {
        let s = setup().await;
        // own entry: only what one already holds
        assert_eq!(put(&s, &s.moderator_token, &s.moderator, MANAGE_MESSAGES, 0).await, Status::Forbidden);
        assert_eq!(put(&s, &s.moderator_token, &s.moderator, VIEW | SEND, 0).await, Status::Ok);
        // the owner's entry
        let owner = s.harness.db.fetch_user(&s.server.owner).await.unwrap();
        // the test server has no member entry for its owner; a real one does
        if s.harness.db.fetch_member(&s.server.id, &owner.id).await.is_err() {
            join(&s.harness, &s.server, &owner, None).await;
        }
        assert_eq!(put(&s, &s.moderator_token, &owner, 0, SEND).await, Status::Forbidden);
        // someone of the same rank
        let (_, _, peer) = s.harness.new_user().await;
        join(&s.harness, &s.server, &peer, Some(&s.moderator_role)).await;
        assert_eq!(put(&s, &s.moderator_token, &peer, 0, VIEW).await, Status::Forbidden);
        // a permission the moderator does not hold
        assert_eq!(put(&s, &s.moderator_token, &s.member, MANAGE_MESSAGES, 0).await, Status::Forbidden);
        assert_eq!(stored(&s, &s.member).await, None);
    }

    #[rocket::async_test]
    async fn only_members_of_the_server() {
        let s = setup().await;
        assert_eq!(put(&s, &s.moderator_token, &s.outsider, VIEW, 0).await, Status::NotFound);
    }

    #[rocket::async_test]
    async fn a_bot_opens_a_ticket_channel_for_one_member() {
        let s = setup().await;
        let owner = s.harness.db.fetch_user(&s.server.owner).await.unwrap();
        let (bot, bot_user) = s.harness.new_bot(&owner).await;
        join(&s.harness, &s.server, &bot_user, Some(&s.moderator_role)).await;
        let status = s
            .harness
            .client
            .put(format!("/channels/{}/permissions/members/{}", s.channel.id(), s.member.id))
            .header(ContentType::JSON)
            .header(Header::new("x-bot-token", bot.token.clone()))
            .body(json!({ "permissions": { "allow": VIEW | SEND, "deny": 0 } }).to_string())
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::Ok);
        assert!(can(&s, &s.member, ChannelPermission::SendMessage).await);
    }
}
