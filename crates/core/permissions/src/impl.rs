use crate::{
    ChannelPermission, ChannelType, PermissionQuery, PermissionValue, RelationshipStatus,
    UserPermission, ALLOW_IN_TIMEOUT, DEFAULT_PERMISSION_DIRECT_MESSAGE,
    DEFAULT_PERMISSION_SAVED_MESSAGES, DEFAULT_PERMISSION_VIEW_ONLY,
};

/// Calculate permissions against a user
pub async fn calculate_user_permissions<P: PermissionQuery>(query: &mut P) -> PermissionValue {
    if query.are_we_privileged().await || query.are_the_users_same().await {
        return u64::MAX.into();
    }

    let mut permissions = 0_u64;
    match query.user_relationship().await {
        RelationshipStatus::Friend => return u64::MAX.into(),
        RelationshipStatus::Blocked | RelationshipStatus::BlockedOther => {
            return (UserPermission::Access as u64).into()
        }
        RelationshipStatus::Incoming | RelationshipStatus::Outgoing => {
            permissions = UserPermission::Access as u64;
        }
        _ => {}
    }

    if query.have_mutual_connection().await || query.user_is_bot().await {
        permissions = UserPermission::Access as u64 + UserPermission::ViewProfile as u64;
    };

    // A person may write to a bot they share a server or group with
    if query.have_mutual_connection().await && query.user_is_bot().await {
        permissions |= UserPermission::SendMessage as u64;
    };

    // A bot may only start a conversation when the person's privacy allows
    // it: `Everyone` or `Mutual`, with a server or group in common. Bots are
    // never friends, so `Friends` keeps them out; blocking returned above.
    if query.are_we_a_bot().await
        && query.have_mutual_connection().await
        && query.user_accepts_direct_messages().await
    {
        permissions |= UserPermission::Access as u64 + UserPermission::SendMessage as u64;
    }

    // People (not bots) may message each other when the other
    // person's privacy settings allow it
    if !query.are_we_a_bot().await
        && !query.user_is_bot().await
        && query.user_accepts_direct_messages().await
    {
        permissions |= UserPermission::Access as u64 + UserPermission::SendMessage as u64;
    }

    permissions.into()
}

/// Calculate permissions against a server
pub async fn calculate_server_permissions<P: PermissionQuery>(query: &mut P) -> PermissionValue {
    if query.are_we_privileged().await || query.are_we_server_owner().await {
        return ChannelPermission::GrantAllSafe.into();
    }

    if !query.are_we_a_member().await {
        return 0_u64.into();
    }

    if is_administrator(query).await {
        let mut permissions: PermissionValue = ChannelPermission::GrantAllSafe.into();
        // a server mute and deafen still hold; a timeout does not
        if !query.do_we_have_publish_overwrites().await {
            permissions.revoke(ChannelPermission::Speak as u64);
            permissions.revoke(ChannelPermission::Video as u64);
        }
        if !query.do_we_have_receive_overwrites().await {
            permissions.revoke(ChannelPermission::Listen as u64);
        }
        return permissions;
    }

    let mut permissions: PermissionValue = query.get_default_server_permissions().await.into();
    revoke_default_masquerade_for_bots(query, &mut permissions).await;

    for role_override in query.get_our_server_role_overrides().await {
        permissions.apply(role_override);
    }

    if !query.do_we_have_publish_overwrites().await {
        permissions.revoke(ChannelPermission::Speak as u64);
        permissions.revoke(ChannelPermission::Video as u64);
    }

    if !query.do_we_have_receive_overwrites().await {
        permissions.revoke(ChannelPermission::Listen as u64);
    }

    if query.are_we_timed_out().await {
        permissions.restrict(*ALLOW_IN_TIMEOUT);
    }

    permissions
}

/// Calculate permissions against a channel
pub async fn calculate_channel_permissions<P: PermissionQuery>(query: &mut P) -> PermissionValue {
    // A privileged account keeps its override in server channels only:
    // private conversations (DMs, groups, saved notes) stay private to the
    // people in them
    if query.are_we_privileged().await
        && matches!(query.get_channel_type().await, ChannelType::ServerChannel)
    {
        return ChannelPermission::GrantAllSafe.into();
    }

    match query.get_channel_type().await {
        ChannelType::SavedMessages => {
            if query.do_we_own_the_channel().await {
                DEFAULT_PERMISSION_SAVED_MESSAGES.into()
            } else {
                0_u64.into()
            }
        }
        ChannelType::DirectMessage => {
            if query.are_we_part_of_the_channel().await {
                query.set_recipient_as_user().await;

                // messages from official accounts can't be answered
                if query.is_user_official().await {
                    return (*DEFAULT_PERMISSION_VIEW_ONLY).into();
                }

                let permissions = calculate_user_permissions(query).await;
                // Anyone can reply to someone who wrote to them first,
                // whatever their privacy settings say (blocking stops it)
                let blocked = matches!(
                    query.user_relationship().await,
                    RelationshipStatus::Blocked | RelationshipStatus::BlockedOther
                );
                if permissions.has_user_permission(UserPermission::SendMessage)
                    || (!blocked && query.has_user_written_in_channel().await)
                {
                    (*DEFAULT_PERMISSION_DIRECT_MESSAGE).into()
                } else {
                    (*DEFAULT_PERMISSION_VIEW_ONLY).into()
                }
            } else {
                0_u64.into()
            }
        }
        ChannelType::Group => {
            if query.do_we_own_the_channel().await {
                ChannelPermission::GrantAllSafe.into()
            } else if query.are_we_part_of_the_channel().await {
                (*DEFAULT_PERMISSION_VIEW_ONLY
                    | query.get_default_channel_permissions().await.allow)
                    .into()
            } else {
                0_u64.into()
            }
        }
        ChannelType::ServerChannel => {
            query.set_server_from_channel().await;

            if query.are_we_server_owner().await {
                ChannelPermission::GrantAllSafe.into()
            } else if query.are_we_a_member().await {
                // an administrator gets everything, whatever the channel says
                let administrator = is_administrator(query).await;
                let mut permissions = if administrator {
                    ChannelPermission::GrantAllSafe.into()
                } else {
                    granted_in_channel(query).await
                };

                // Server mute is the microphone. Camera and screen share stay on Video.
                if !query.do_we_have_publish_overwrites().await {
                    permissions.revoke(ChannelPermission::Speak as u64);
                }

                if !query.do_we_have_receive_overwrites().await {
                    permissions.revoke(ChannelPermission::Listen as u64);
                }

                if !administrator && query.are_we_timed_out().await {
                    permissions.restrict(*ALLOW_IN_TIMEOUT);
                }

                if !permissions.has_channel_permission(ChannelPermission::ViewChannel) {
                    permissions.revoke_all();
                }

                permissions
            } else {
                0_u64.into()
            }
        }
        ChannelType::Unknown => 0_u64.into(),
    }
}

/// What a server channel's defaults and overrides give the perspective user,
/// from the server default to their own override
async fn granted_in_channel<P: PermissionQuery>(query: &mut P) -> PermissionValue {
    let mut permissions = PermissionValue::from(query.get_default_server_permissions().await);
    revoke_default_masquerade_for_bots(query, &mut permissions).await;
    permissions.apply(query.get_default_channel_permissions().await);

    for role_override in query.get_our_server_role_overrides().await {
        permissions.apply(role_override);
    }

    for role_override in query.get_our_channel_role_overrides().await {
        permissions.apply(role_override);
    }

    // a member's own override wins over every role (as on Discord)
    if let Some(member_override) = query.get_our_channel_member_override().await {
        permissions.apply(member_override);
    }

    permissions
}

/// What the server's settings say about someone in a server channel, whatever
/// their present state: this also answers for an account that can't see the
/// channel, is timed out, or has left (then it is what everyone gets).
///
/// For things that act in someone's name without being them: a webhook is
/// refused what its creator's roles deny, and only that.
pub async fn calculate_standing_channel_permissions<P: PermissionQuery>(
    query: &mut P,
) -> PermissionValue {
    if !matches!(query.get_channel_type().await, ChannelType::ServerChannel) {
        return 0_u64.into();
    }

    query.set_server_from_channel().await;
    if query.are_we_privileged().await || query.are_we_server_owner().await {
        return ChannelPermission::GrantAllSafe.into();
    }

    query.are_we_a_member().await;
    if is_administrator(query).await {
        return ChannelPermission::GrantAllSafe.into();
    }
    granted_in_channel(query).await
}

/// Whether the server default and the member's roles give Administrator.
/// Only server-level permissions count: a channel can't make someone an
/// administrator, nor take it away.
async fn is_administrator<P: PermissionQuery>(query: &mut P) -> bool {
    let mut permissions = PermissionValue::from(query.get_default_server_permissions().await);
    for role_override in query.get_our_server_role_overrides().await {
        permissions.apply(role_override);
    }
    permissions.has(ChannelPermission::Administrator as u64)
}

/// Bots may only masquerade with an explicit grant (their own role, another
/// role, or a channel override), never because the server's defaults allow
/// it: otherwise any bot could post under people's names
async fn revoke_default_masquerade_for_bots<P: PermissionQuery>(
    query: &mut P,
    permissions: &mut PermissionValue,
) {
    if query.are_we_a_bot().await {
        permissions.revoke(ChannelPermission::Masquerade as u64);
    }
}
