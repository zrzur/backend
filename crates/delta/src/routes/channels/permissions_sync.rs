use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{sync_voice_permissions, VoiceClient},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, PermissionQuery};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Sync Permissions With Category
///
/// The channel follows its category's permissions again; its own default
/// and role overrides are cleared. Per-member overrides stay.
#[openapi(tag = "Channel Permissions")]
#[post("/<target>/permissions/sync")]
pub async fn sync_permissions(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::Channel>> {
    let mut channel = target.as_channel(db).await?;
    let (permissions, server) = {
        let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
        let permissions = calculate_channel_permissions(&mut query).await;
        if channel.server().is_none() {
            return Err(create_error!(InvalidOperation));
        }
        query.set_server_from_channel().await;
        (permissions, query.server_ref().as_deref().cloned())
    };
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;

    let server = server.ok_or_else(|| create_error!(NotFound))?;
    // only a channel inside a category has something to follow
    let in_category = server.categories.as_ref().is_some_and(|categories| {
        categories
            .iter()
            .any(|category| category.channels.iter().any(|id| id == channel.id()))
    });
    if !in_category {
        return Err(create_error!(InvalidOperation));
    }

    channel.sync_permissions(db).await?;
    sync_voice_permissions(db, voice_client, &channel, Some(&server), None).await?;

    Ok(Json(channel.into()))
}
