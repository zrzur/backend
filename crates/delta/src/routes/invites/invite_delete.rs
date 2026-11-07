use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Channel, Database, Invite, User,
};
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::State;
use rocket_empty::EmptyResponse;

use crate::util::audit_log_reason::AuditLogReason;

/// # Delete Invite
///
/// Delete an invite by its id.
#[openapi(tag = "Invites")]
#[delete("/<target>")]
pub async fn delete(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
) -> Result<EmptyResponse> {
    let invite = target.as_invite(db).await?;

    // a custom link is removed through DELETE /servers/:id/vanity, which also
    // clears the server's claim on the name
    if matches!(invite, Invite::Server { vanity: true, .. }) {
        return Err(create_error!(InvalidOperation));
    }

    if user.id == invite.creator() {
        db.delete_invite(invite.code()).await?;
    } else {
        match invite {
            Invite::Server {
                code,
                server,
                channel,
                creator,
                ..
            } => {
                let server = db.fetch_server(&server).await?;
                let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
                calculate_server_permissions(&mut query)
                    .await
                    .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;

                db.delete_invite(&code).await?;

                AuditLogEntryAction::InviteDelete {
                    invite: code,
                    channel,
                }
                .insert(db, server.id, reason, user.id, Some(creator))
                .await;
            }
            Invite::Group { code, channel, .. } => {
                // the group's owner may remove any invite to it; everyone
                // else is refused (this used to hit `unreachable!` → 500)
                match db.fetch_channel(&channel).await? {
                    Channel::Group { owner, .. } if owner == user.id => {
                        db.delete_invite(&code).await?;
                    }
                    _ => {
                        return Err(create_error!(MissingPermission {
                            permission: ChannelPermission::ManageChannel.to_string()
                        }))
                    }
                }
            }
        }
    }

    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Channel, Invite};
    use rocket::http::Status;

    /// a group (owned by `owner`) with an invite made by `creator`
    async fn group_invite(harness: &TestHarness, owner: &str, creator: &str) -> String {
        let channel = ulid::Ulid::new().to_string();
        harness
            .db
            .insert_channel(&Channel::Group {
                id: channel.clone(),
                name: "group".to_string(),
                owner: owner.to_string(),
                description: None,
                recipients: vec![owner.to_string(), creator.to_string()],
                icon: None,
                last_message_id: None,
                permissions: None,
                nsfw: false,
            })
            .await
            .expect("group");
        let code = ulid::Ulid::new().to_string()[..8].to_string();
        harness
            .db
            .insert_invite(&Invite::Group {
                code: code.clone(),
                creator: creator.to_string(),
                channel,
                max_uses: None,
                uses: 0,
                expires: None,
            })
            .await
            .expect("invite");
        code
    }

    #[rocket::async_test]
    async fn outsider_cannot_delete_group_invite() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, outsider, _) = harness.new_user().await;
        let code = group_invite(&harness, &owner.id, &owner.id).await;

        let response = TestHarness::with_session(
            outsider,
            harness.client.delete(format!("/invites/{code}")),
        )
        .await;
        // a clean refusal, never a 500, and the invite stays
        assert_eq!(response.status(), Status::Forbidden);
        assert!(harness.db.fetch_invite(&code).await.is_ok());
    }

    #[rocket::async_test]
    async fn group_owner_can_delete_a_members_invite() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, _, member) = harness.new_user().await;
        let code = group_invite(&harness, &owner.id, &member.id).await;

        let response = TestHarness::with_session(
            owner_session,
            harness.client.delete(format!("/invites/{code}")),
        )
        .await;
        assert_eq!(response.status(), Status::NoContent);
        assert!(harness.db.fetch_invite(&code).await.is_err());
    }

    #[rocket::async_test]
    async fn creator_can_still_delete_own_group_invite() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, member_session, member) = harness.new_user().await;
        let code = group_invite(&harness, &owner.id, &member.id).await;

        let response = TestHarness::with_session(
            member_session,
            harness.client.delete(format!("/invites/{code}")),
        )
        .await;
        assert_eq!(response.status(), Status::NoContent);
    }
}
