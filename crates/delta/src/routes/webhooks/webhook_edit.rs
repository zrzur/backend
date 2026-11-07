use revolt_database::{
    events::client::EventV1,
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Channel, Database, File, PartialWebhook, User,
};
use revolt_models::v0::{DataEditWebhook, Webhook};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, Override};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

use crate::routes::channels::webhook_create::assert_room_for_webhook;
use crate::util::audit_log_reason::AuditLogReason;

/// # Edits a webhook
///
/// Edits a webhook: name, avatar, permissions, channel. Moving it needs
/// Manage Webhooks in both channels, within the same server.
#[openapi(tag = "Webhooks")]
#[patch("/<webhook_id>", data = "<data>")]
pub async fn webhook_edit(
    db: &State<Database>,
    webhook_id: Reference<'_>,
    user: User,
    reason: AuditLogReason,
    data: Json<DataEditWebhook>,
) -> Result<Json<Webhook>> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let mut webhook = webhook_id.as_webhook(db).await?;
    let channel = db.fetch_channel(&webhook.channel_id).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let user_permissions = calculate_channel_permissions(&mut query).await;

    user_permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageWebhooks)?;

    if data.name.is_none()
        && data.avatar.is_none()
        && data.permissions.is_none()
        && data.channel_id.is_none()
        && data.remove.is_empty()
    {
        return Ok(Json(webhook.into()));
    };

    if let Some(new_permissions) = data.permissions {
        let current_value = webhook.permissions;

        user_permissions
            .throw_permission_override(
                Override {
                    allow: current_value,
                    deny: 0,
                },
                &Override {
                    allow: new_permissions,
                    deny: 0,
                },
            )
            .await?;
    }

    let DataEditWebhook {
        name,
        avatar,
        permissions,
        channel_id,
        remove,
    } = data;

    if let Some(name) = &name {
        revolt_database::assert_webhook_name(db, name).await?;
    }

    // Move: both channels in the same server, managed by this user
    let move_from = match channel_id.filter(|target| *target != webhook.channel_id) {
        Some(target) => {
            let target = Reference::from_unchecked(&target).as_channel(db).await?;
            if !matches!(target, Channel::TextChannel { parent: None, voice: None, .. })
                || target.server().is_none()
                || target.server() != channel.server()
            {
                return Err(create_error!(InvalidOperation));
            }
            let mut query = DatabasePermissionQuery::new(db, &user).channel(&target);
            calculate_channel_permissions(&mut query)
                .await
                .throw_if_lacking_channel_permission(ChannelPermission::ManageWebhooks)?;
            assert_room_for_webhook(db, &target).await?;
            Some((webhook.channel_id.clone(), target.id().to_string()))
        }
        None => None,
    };

    let mut partial = PartialWebhook {
        name,
        permissions,
        channel_id: move_from.as_ref().map(|(_, to)| to.clone()),
        ..Default::default()
    };

    if let Some(avatar) = avatar {
        let file = File::use_webhook_avatar(db, &avatar, &webhook.id, &webhook.creator_id).await?;
        partial.avatar = Some(file)
    }

    webhook
        .update(db, partial, remove.into_iter().map(|v| v.into()).collect())
        .await?;

    // Managers of each channel follow the webhook out of one and into the other
    if let Some((from, _)) = &move_from {
        EventV1::WebhookDelete {
            id: webhook.id.clone(),
            channel_id: from.clone(),
        }
        .p(from.clone())
        .await;
        EventV1::WebhookCreate(webhook.clone().into())
            .p(webhook.channel_id.clone())
            .await;
    }

    if let Some(server_id) = channel.server() {
        AuditLogEntryAction::WebhookUpdate {
            webhook: webhook.id.clone(),
            name: webhook.name.clone(),
            channel: webhook.channel_id.clone(),
        }
        .insert(
            db,
            server_id.to_string(),
            reason,
            user.id,
            Some(webhook.creator_id.clone()),
        )
        .await;
    }

    Ok(Json(webhook.into()))
}
