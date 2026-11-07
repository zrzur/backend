use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{sync_voice_permissions, VoiceClient},
    AuditLogEntryAction, Channel, Database, PartialChannel, User,
};
use revolt_models::v0::{self, DataDefaultChannelPermissions};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, PermissionQuery};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::audit_log_reason::AuditLogReason;

/// # Set Default Permission
///
/// Sets permissions for the default role in this channel.
///
/// Channel must be a `Group` or `TextChannel`. A channel synced with its
/// category stops following it: it keeps a copy of the category's
/// permissions and the change applies on top.
#[openapi(tag = "Channel Permissions")]
#[put("/<target>/permissions/default", data = "<data>", rank = 1)]
pub async fn set_default_channel_permissions(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    data: Json<v0::DataDefaultChannelPermissions>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();

    let mut channel = target.as_channel(db).await?;
    let (permissions, server) = {
        let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
        let permissions = calculate_channel_permissions(&mut query).await;
        let server = if matches!(channel, Channel::TextChannel { .. }) {
            query.set_server_from_channel().await;
            query.server_ref().as_deref().cloned()
        } else {
            None
        };
        (permissions, server)
    };

    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;

    match &channel {
        Channel::Group { .. } => {
            if let DataDefaultChannelPermissions::Value { permissions } = data {
                channel
                    .update(
                        db,
                        PartialChannel {
                            permissions: Some(permissions as i64),
                            ..Default::default()
                        },
                        vec![],
                    )
                    .await?;
            } else {
                return Err(create_error!(InvalidOperation));
            }
        }
        Channel::TextChannel { id, server: server_id, .. } => {
            if let DataDefaultChannelPermissions::Field { permissions: field } = data {
                // compare with what applies now (the category's, when synced)
                let current = channel
                    .effective_overrides(server.as_ref())
                    .and_then(|(default, _)| default);
                permissions
                    .throw_permission_override(current.map(|x| x.into()), &field)
                    .await?;

                let id = id.clone();
                let server_id = server_id.clone();

                if let Some(server) = &server {
                    channel.unsync_permissions(db, server).await?;
                }

                let partial = PartialChannel {
                    default_permissions: Some(field.into()),
                    ..Default::default()
                };

                let before = channel.generate_diff(&partial, &[]);

                channel.update(db, partial.clone(), vec![]).await?;

                AuditLogEntryAction::ChannelEdit {
                    channel: id,
                    before,
                    after: partial,
                }
                .insert(db, server_id, reason, user.id, None)
                .await;
            } else {
                return Err(create_error!(InvalidOperation));
            }
        }
        _ => return Err(create_error!(InvalidOperation)),
    }

    sync_voice_permissions(db, voice_client, &channel, server.as_ref(), None).await?;

    Ok(Json(channel.into()))
}
