//! Who may write in threads and forum posts. A forum post is a thread whose
//! parent is a forum, so one check covers both; every way of writing
//! (messages, bots, interactions, webhooks) comes through here.

use revolt_permissions::{
    calculate_channel_permissions, calculate_standing_channel_permissions, ChannelPermission,
    PermissionValue,
};
use revolt_result::{create_error, Result};

use crate::{util::permissions::DatabasePermissionQuery, Channel, Database, Server, User};

/// The parent of a thread; None for anything else
pub async fn thread_parent(db: &Database, channel: &Channel) -> Result<Option<Channel>> {
    match channel {
        Channel::TextChannel {
            thread: Some(_),
            parent: Some(parent),
            ..
        } => Ok(Some(db.fetch_channel(parent).await?)),
        _ => Ok(None),
    }
}

/// Refuse when the server switched off what this channel holds: threads for a
/// text channel, posts for a forum. Applies to everyone, the owner too: they
/// can switch it back on, and until then the content is read-only.
pub fn assert_feature_on(server: &Server, parent: &Channel) -> Result<()> {
    if matches!(parent, Channel::TextChannel { forum: Some(_), .. }) {
        if server.forums_disabled {
            return Err(create_error!(ForumsDisabled));
        }
    } else if server.threads_disabled {
        return Err(create_error!(ThreadsDisabled));
    }
    Ok(())
}

/// Refuse adding to a thread whose feature is switched off: writing, editing,
/// reacting, pinning. Taking away stays allowed (deleting a message, removing
/// a reaction), so people keep control of what they wrote. Webhooks follow the
/// same line: no new messages or edits, but they can delete their own. Other
/// channels pass.
pub async fn assert_not_read_only(db: &Database, channel: &Channel) -> Result<()> {
    if let (Some(parent), Some(server)) = (thread_parent(db, channel).await?, channel.server()) {
        assert_feature_on(&db.fetch_server(server).await?, &parent)?;
    }
    Ok(())
}

/// Refuse writing in a thread without the right to. `permissions` are those of
/// the writer in the thread, which come from its parent with the parent's
/// overrides. Other channels pass.
pub async fn assert_can_write(
    db: &Database,
    channel: &Channel,
    permissions: &PermissionValue,
) -> Result<()> {
    assert_not_read_only(db, channel).await?;
    if matches!(channel, Channel::TextChannel { thread: Some(_), .. }) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessageInThreads)?;
    }
    Ok(())
}

/// Refuse showing someone as typing where they can't write. One rule for the
/// REST route and the gateway.
pub async fn assert_can_type(db: &Database, user: &User, channel: &Channel) -> Result<()> {
    let mut query = DatabasePermissionQuery::new(db, user).channel(channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
    assert_can_write(db, channel, &permissions).await?;
    super::age::assert_can_view_channel(db, user, channel).await
}

/// What a webhook's creator stands to get in `parent`. A webhook carries its
/// own permissions and has no roles, so this is what closes threads and forums
/// to it: a permission denied to the role of the person (or bot) who made it
/// is denied to the webhook. A creator who left counts as everyone else, as
/// does an account that no longer exists.
pub async fn webhook_creator_permissions(
    db: &Database,
    creator_id: &str,
    parent: &Channel,
) -> Result<PermissionValue> {
    if let Ok(creator) = db.fetch_user(creator_id).await {
        let mut query = DatabasePermissionQuery::new(db, &creator).channel(parent);
        return Ok(calculate_standing_channel_permissions(&mut query).await);
    }

    let server_id = parent
        .server()
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let server = db.fetch_server(server_id).await?;
    let mut permissions = PermissionValue::from(server.default_permissions as u64);
    if let Some((Some(default), _)) = parent.effective_overrides(Some(&server)) {
        permissions.apply(default.into());
    }
    Ok(permissions)
}
