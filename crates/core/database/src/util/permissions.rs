use std::borrow::Cow;

use revolt_permissions::{
    calculate_user_permissions, ChannelType, Override, PermissionQuery, PermissionValue,
    RelationshipStatus, DEFAULT_PERMISSION_DIRECT_MESSAGE,
};

use crate::{
    Channel, Database, Member, MessageFilter, MessageQuery, MessageTimePeriod, Server, User,
};

/// Permissions calculator
#[derive(Clone)]
pub struct DatabasePermissionQuery<'a> {
    #[allow(dead_code)]
    database: &'a Database,

    perspective: &'a User,
    user: Option<Cow<'a, User>>,
    channel: Option<Cow<'a, Channel>>,
    server: Option<Cow<'a, Server>>,
    member: Option<Cow<'a, Member>>,

    // flag_known_relationship: Option<&'a RelationshipStatus>,
    cached_user_permission: Option<PermissionValue>,
    cached_mutual_connection: Option<bool>,
    cached_permission: Option<u64>,
    /// A thread's parent channel, whose permissions the thread uses
    parent: Option<Channel>,
}

#[async_trait]
impl PermissionQuery for DatabasePermissionQuery<'_> {
    // * For calculating user permission

    /// Is our perspective user privileged?
    async fn are_we_privileged(&mut self) -> bool {
        self.perspective.privileged
    }

    /// Is our perspective user a bot?
    async fn are_we_a_bot(&mut self) -> bool {
        self.perspective.bot.is_some()
    }

    /// Is our perspective user and the currently selected user the same?
    async fn are_the_users_same(&mut self) -> bool {
        if let Some(other_user) = &self.user {
            self.perspective.id == other_user.id
        } else {
            false
        }
    }

    /// Get the relationship with have with the currently selected user
    async fn user_relationship(&mut self) -> RelationshipStatus {
        if let Some(other_user) = &self.user {
            if self.perspective.id == other_user.id {
                return RelationshipStatus::User;
            } else if let Some(bot) = &other_user.bot {
                // For the purposes of permissions checks,
                // assume owner is the same as bot
                if self.perspective.id == bot.owner {
                    return RelationshipStatus::User;
                }
            }

            if let Some(relations) = &self.perspective.relations {
                for entry in relations {
                    if entry.id == other_user.id {
                        return match entry.status {
                            crate::RelationshipStatus::None => RelationshipStatus::None,
                            crate::RelationshipStatus::User => RelationshipStatus::User,
                            crate::RelationshipStatus::Friend => RelationshipStatus::Friend,
                            crate::RelationshipStatus::Outgoing => RelationshipStatus::Outgoing,
                            crate::RelationshipStatus::Incoming => RelationshipStatus::Incoming,
                            crate::RelationshipStatus::Blocked => RelationshipStatus::Blocked,
                            crate::RelationshipStatus::BlockedOther => {
                                RelationshipStatus::BlockedOther
                            }
                        };
                    }
                }
            }
        }

        RelationshipStatus::None
    }

    /// Whether the currently selected user is a bot
    async fn user_is_bot(&mut self) -> bool {
        if let Some(other_user) = &self.user {
            other_user.bot.is_some()
        } else {
            false
        }
    }

    /// Do we have a mutual connection with the currently selected user?
    async fn have_mutual_connection(&mut self) -> bool {
        if let Some(value) = self.cached_mutual_connection {
            value
        } else if let Some(user) = &self.user {
            let value = self
                .perspective
                .has_mutual_connection(self.database, &user.id)
                .await
                .unwrap_or_default();

            self.cached_mutual_connection = Some(value);
            value
        } else {
            false
        }
    }

    /// Do the privacy settings of the currently selected user let us start a direct message?
    async fn user_accepts_direct_messages(&mut self) -> bool {
        // Nothing shared at all (already cached): only "Everyone" can allow it
        let shares_nothing = !self.have_mutual_connection().await;
        if let Some(user) = &self.user {
            if shares_nothing {
                // a saved "Everyone" still gives way to the under-18 rule
                return matches!(
                    user.privacy().direct_messages,
                    revolt_models::v0::DirectMessagePrivacy::Everyone
                ) && matches!(
                    user.effective_privacy(self.database).await.direct_messages,
                    revolt_models::v0::DirectMessagePrivacy::Everyone
                );
            }

            user.accepts_direct_messages_from(self.database, self.perspective)
                .await
        } else {
            false
        }
    }

    /// Has the currently selected user written in this direct message channel?
    async fn is_user_official(&mut self) -> bool {
        self.user.as_ref().is_some_and(|user| user.official)
    }

    async fn has_user_written_in_channel(&mut self) -> bool {
        if let (Some(user), Some(channel)) = (&self.user, &self.channel) {
            self.database
                .fetch_messages(MessageQuery {
                    limit: Some(1),
                    filter: MessageFilter {
                        channel: Some(channel.id().to_string()),
                        author: Some(user.id.clone()),
                        ..Default::default()
                    },
                    time_period: MessageTimePeriod::Absolute {
                        before: None,
                        after: None,
                        sort: None,
                    },
                })
                .await
                .is_ok_and(|messages| !messages.is_empty())
        } else {
            false
        }
    }

    // * For calculating server permission

    /// Is our perspective user the server's owner?
    async fn are_we_server_owner(&mut self) -> bool {
        if let Some(server) = &self.server {
            server.owner == self.perspective.id
        } else {
            false
        }
    }

    /// Is our perspective user a member of the server?
    async fn are_we_a_member(&mut self) -> bool {
        if let Some(server) = &self.server {
            if self.member.is_some() {
                true
            } else if let Ok(member) = self
                .database
                .fetch_member(&server.id, &self.perspective.id)
                .await
            {
                self.member = Some(Cow::Owned(member));
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Get default server permission
    async fn get_default_server_permissions(&mut self) -> u64 {
        if let Some(server) = &self.server {
            server.default_permissions as u64
        } else {
            0
        }
    }

    /// Get the ordered role overrides (from lowest to highest) for this member in this server
    async fn get_our_server_role_overrides(&mut self) -> Vec<Override> {
        if let Some(server) = &self.server {
            let member_roles = self
                .member
                .as_ref()
                .map(|member| member.roles.clone())
                .unwrap_or_default();

            let mut roles = server
                .roles
                .iter()
                .filter(|(id, _)| member_roles.contains(id))
                .map(|(_, role)| {
                    let v: Override = role.permissions.into();
                    (role.rank, v)
                })
                .collect::<Vec<(i64, Override)>>();

            roles.sort_by(|a, b| b.0.cmp(&a.0));
            roles.into_iter().map(|(_, v)| v).collect()
        } else {
            vec![]
        }
    }

    /// Is our perspective user timed out on this server?
    async fn are_we_timed_out(&mut self) -> bool {
        if let Some(member) = &self.member {
            member.in_timeout()
        } else {
            false
        }
    }

    async fn do_we_have_publish_overwrites(&mut self) -> bool {
        if let Some(member) = &self.member {
            member.can_publish
        } else {
            true
        }
    }

    async fn do_we_have_receive_overwrites(&mut self) -> bool {
        if let Some(member) = &self.member {
            member.can_receive
        } else {
            true
        }
    }

    // * For calculating channel permission

    /// Get the type of the channel
    #[allow(deprecated)]
    async fn get_channel_type(&mut self) -> ChannelType {
        if let Some(channel) = &self.channel {
            match channel {
                Cow::Borrowed(Channel::DirectMessage { .. })
                | Cow::Owned(Channel::DirectMessage { .. }) => ChannelType::DirectMessage,
                Cow::Borrowed(Channel::Group { .. }) | Cow::Owned(Channel::Group { .. }) => {
                    ChannelType::Group
                }
                Cow::Borrowed(Channel::SavedMessages { .. })
                | Cow::Owned(Channel::SavedMessages { .. }) => ChannelType::SavedMessages,
                Cow::Borrowed(Channel::TextChannel { .. })
                | Cow::Owned(Channel::TextChannel { .. }) => ChannelType::ServerChannel,
            }
        } else {
            ChannelType::Unknown
        }
    }

    /// Get the default channel permissions
    /// Group channel defaults should be mapped to an allow-only override
    async fn get_default_channel_permissions(&mut self) -> Override {
        match self.channel.as_deref() {
            Some(Channel::Group { permissions, .. }) => Override {
                allow: permissions.unwrap_or(*DEFAULT_PERMISSION_DIRECT_MESSAGE as i64) as u64,
                deny: 0,
            },
            Some(Channel::TextChannel { .. }) => {
                // a channel synced with its category uses the category's;
                // a thread uses its parent channel's
                self.set_server_from_channel().await;
                self.load_parent().await;
                self.permission_channel()
                    .and_then(|channel| channel.effective_overrides(self.server.as_deref()))
                    .and_then(|(default, _)| default)
                    .unwrap_or_default()
                    .into()
            }
            _ => Default::default(),
        }
    }

    /// Get the ordered role overrides (from lowest to highest) for this member in this channel
    async fn get_our_channel_role_overrides(&mut self) -> Vec<Override> {
        if !matches!(self.channel.as_deref(), Some(Channel::TextChannel { .. })) {
            return vec![];
        }
        self.set_server_from_channel().await;
        self.load_parent().await;
        let (Some(channel), Some(server)) = (self.permission_channel(), self.server.as_deref()) else {
            return vec![];
        };
        let Some((_, role_permissions)) = channel.effective_overrides(Some(server)) else {
            return vec![];
        };
        let member_roles = self
            .member
            .as_ref()
            .map(|member| member.roles.clone())
            .unwrap_or_default();

        let mut roles = role_permissions
            .iter()
            .filter(|(id, _)| member_roles.contains(id))
            .filter_map(|(id, permission)| {
                server.roles.get(id).map(|role| {
                    let v: Override = (*permission).into();
                    (role.rank, v)
                })
            })
            .collect::<Vec<(i64, Override)>>();

        roles.sort_by(|a, b| b.0.cmp(&a.0));
        roles.into_iter().map(|(_, v)| v).collect()
    }

    /// This member's own override in this channel, applied after every role
    async fn get_our_channel_member_override(&mut self) -> Option<Override> {
        self.load_parent().await;
        match self.permission_channel() {
            Some(Channel::TextChannel {
                member_permissions, ..
            }) => member_permissions
                .get(&self.perspective.id)
                .map(|permission| (*permission).into()),
            _ => None,
        }
    }

    /// Do we own this group or saved messages channel if it is one of those?
    async fn do_we_own_the_channel(&mut self) -> bool {
        if let Some(channel) = &self.channel {
            match channel {
                Cow::Borrowed(Channel::Group { owner, .. })
                | Cow::Owned(Channel::Group { owner, .. }) => owner == &self.perspective.id,
                Cow::Borrowed(Channel::SavedMessages { user, .. })
                | Cow::Owned(Channel::SavedMessages { user, .. }) => user == &self.perspective.id,
                _ => false,
            }
        } else {
            false
        }
    }

    /// Are we a recipient of this channel?
    async fn are_we_part_of_the_channel(&mut self) -> bool {
        if let Some(
            Cow::Borrowed(Channel::DirectMessage { recipients, .. })
            | Cow::Owned(Channel::DirectMessage { recipients, .. })
            | Cow::Borrowed(Channel::Group { recipients, .. })
            | Cow::Owned(Channel::Group { recipients, .. }),
        ) = &self.channel
        {
            recipients.contains(&self.perspective.id)
        } else {
            false
        }
    }

    /// Set the current user as the recipient of this channel
    /// (this will only ever be called for DirectMessage channels, use unimplemented!() for other code paths)
    async fn set_recipient_as_user(&mut self) {
        if let Some(channel) = &self.channel {
            match channel {
                Cow::Borrowed(Channel::DirectMessage { recipients, .. })
                | Cow::Owned(Channel::DirectMessage { recipients, .. }) => {
                    let recipient_id = recipients
                        .iter()
                        .find(|recipient| recipient != &&self.perspective.id)
                        .expect("Missing recipient for DM");

                    if let Ok(user) = self.database.fetch_user(recipient_id).await {
                        self.user.replace(Cow::Owned(user));
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    /// Set the current server as the server owning this channel
    /// (this will only ever be called for server channels, use unimplemented!() for other code paths)
    async fn set_server_from_channel(&mut self) {
        if let Some(channel) = &self.channel {
            #[allow(deprecated)]
            match channel {
                Cow::Borrowed(Channel::TextChannel { server, .. })
                | Cow::Owned(Channel::TextChannel { server, .. }) => {
                    if let Some(known_server) =
                        // I'm not sure why I can't just pattern match both at once here?
                        // It throws some weird error and the provided fix doesn't work :/
                        if let Some(Cow::Borrowed(known_server)) = self.server {
                                Some(known_server)
                            } else if let Some(Cow::Owned(ref known_server)) = self.server {
                                Some(known_server)
                            } else {
                                None
                            }
                    {
                        if server == &known_server.id {
                            // Already cached, return early.
                            return;
                        }
                    }

                    if let Ok(server) = self.database.fetch_server(server).await {
                        self.server.replace(Cow::Owned(server));
                    }
                }
                _ => unimplemented!(),
            }
        }
    }
}

impl<'a> DatabasePermissionQuery<'a> {
    /// Load a thread's parent channel once; a thread uses its permissions
    async fn load_parent(&mut self) {
        if self.parent.is_some() {
            return;
        }
        if let Some(Channel::TextChannel {
            parent: Some(parent),
            ..
        }) = self.channel.as_deref()
        {
            let parent = parent.clone();
            if let Ok(channel) = self.database.fetch_channel(&parent).await {
                self.parent = Some(channel);
            }
        }
    }

    /// The channel whose overrides apply: the parent for a thread
    fn permission_channel(&self) -> Option<&Channel> {
        self.parent.as_ref().or(self.channel.as_deref())
    }

    /// Create a new permission calculator
    pub fn new(database: &'a Database, perspective: &'a User) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            database,
            perspective,
            user: None,
            channel: None,
            server: None,
            member: None,

            cached_mutual_connection: None,
            cached_user_permission: None,
            cached_permission: None,
            parent: None,
        }
    }

    /// Calculate the user permission value
    pub async fn calc_user(mut self) -> DatabasePermissionQuery<'a> {
        if self.cached_user_permission.is_some() {
            return self;
        }

        if self.user.is_none() {
            panic!("Expected `PermissionCalculator.user to exist.");
        }

        DatabasePermissionQuery {
            cached_user_permission: Some(calculate_user_permissions(&mut self).await),
            ..self
        }
    }

    /// Calculate the permission value
    pub async fn calc(self) -> DatabasePermissionQuery<'a> {
        if self.cached_permission.is_some() {
            return self;
        }

        self
    }

    /// Use user
    pub fn user(self, user: &'a User) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            user: Some(Cow::Borrowed(user)),
            ..self
        }
    }

    /// Use channel
    pub fn channel(self, channel: &'a Channel) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            channel: Some(Cow::Borrowed(channel)),
            ..self
        }
    }

    /// Use a thread's parent channel already in hand (saves a lookup)
    pub fn parent(self, parent: &Channel) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            parent: Some(parent.clone()),
            ..self
        }
    }

    /// Use server
    pub fn server(self, server: &'a Server) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            server: Some(Cow::Borrowed(server)),
            ..self
        }
    }

    /// Use member
    pub fn member(self, member: &'a Member) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            member: Some(Cow::Borrowed(member)),
            ..self
        }
    }

    /// Access the underlying user
    pub fn user_ref(&self) -> &Option<Cow<User>> {
        &self.user
    }

    /// Access the underlying server
    pub fn channel_ref(&self) -> &Option<Cow<Channel>> {
        &self.channel
    }

    /// Access the underlying server
    pub fn server_ref(&self) -> &Option<Cow<Server>> {
        &self.server
    }

    /// Access the underlying member
    pub fn member_ref(&self) -> &Option<Cow<Member>> {
        &self.member
    }

    /// Get the known member's current ranking
    pub fn get_member_rank(&self) -> Option<i64> {
        self.member
            .as_ref()
            .map(|member| member.get_ranking(self.server.as_ref().unwrap()))
    }
}

/// Short-hand for creating a permission calculator
pub fn perms<'a>(database: &'a Database, perspective: &'a User) -> DatabasePermissionQuery<'a> {
    DatabasePermissionQuery::new(database, perspective)
}
