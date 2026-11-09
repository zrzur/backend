#![allow(deprecated)]
use std::{borrow::Cow, collections::HashMap};

use redis_kiss::get_connection;
use revolt_config::config;
use revolt_models::v0::{self, MessageAuthor};
use revolt_permissions::{ChannelPermission, OverrideField};
use revolt_result::Result;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::{
    events::client::EventV1, Category, Database, File, PartialServer, Server, SystemMessage, User,
    AMQP,
};

#[cfg(feature = "mongodb")]
use crate::IntoDocumentPath;

auto_derived!(
    #[serde(tag = "channel_type")]
    pub enum Channel {
        /// Personal "Saved Notes" channel which allows users to save messages
        SavedMessages {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,
            /// Id of the user this channel belongs to
            user: String,
        },
        /// Direct message channel between two users
        DirectMessage {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,

            /// Whether this direct message channel is currently open on both sides
            active: bool,
            /// 2-tuple of user ids participating in direct message
            recipients: Vec<String>,
            /// Id of the last message sent in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            last_message_id: Option<String>,
        },
        /// Group channel between 1 or more participants
        Group {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,

            /// Display name of the channel
            name: String,
            /// User id of the owner of the group
            owner: String,
            /// Channel description
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<String>,
            /// Array of user ids participating in channel
            recipients: Vec<String>,

            /// Custom icon attachment
            #[serde(skip_serializing_if = "Option::is_none")]
            icon: Option<File>,
            /// Id of the last message sent in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            last_message_id: Option<String>,

            /// Permissions assigned to members of this group
            /// (does not apply to the owner of the group)
            #[serde(skip_serializing_if = "Option::is_none")]
            permissions: Option<i64>,

            /// Whether this group is marked as not safe for work
            #[serde(skip_serializing_if = "crate::if_false", default)]
            nsfw: bool,
        },
        /// Text channel belonging to a server
        TextChannel {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,
            /// Id of the server this channel belongs to
            server: String,

            /// Display name of the channel
            name: String,
            /// Channel description
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<String>,

            /// Custom icon attachment
            #[serde(skip_serializing_if = "Option::is_none")]
            icon: Option<File>,
            /// Id of the last message sent in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            last_message_id: Option<String>,

            /// Default permissions assigned to users in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            default_permissions: Option<OverrideField>,
            /// Permissions assigned based on role to this channel
            #[serde(
                default = "HashMap::<String, OverrideField>::new",
                skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
            )]
            role_permissions: HashMap<String, OverrideField>,
            /// Permissions for single members, applied after the roles (for
            /// example a ticket only its author and the staff can see)
            #[serde(
                default = "HashMap::<String, OverrideField>::new",
                skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
            )]
            member_permissions: HashMap<String, OverrideField>,

            /// Whether this channel is marked as not safe for work
            #[serde(skip_serializing_if = "crate::if_false", default)]
            nsfw: bool,

            /// Voice Information for when this channel is also a voice channel
            #[serde(skip_serializing_if = "Option::is_none")]
            voice: Option<VoiceInformation>,

            /// The channel's slowmode delay in seconds
            #[serde(skip_serializing_if = "Option::is_none")]
            slowmode: Option<u64>,

            /// Announcement channel: only people who can manage messages post
            #[serde(skip_serializing_if = "crate::if_false", default)]
            announcement: bool,

            /// Uses its category's default and role permissions instead of
            /// its own; per-member overrides still apply after them
            #[serde(skip_serializing_if = "crate::if_false", default)]
            permissions_synced: bool,

            /// The channel this thread belongs to (a thread uses its permissions)
            #[serde(skip_serializing_if = "Option::is_none", default)]
            parent: Option<String>,
            /// Thread state, when this channel is a thread
            #[serde(skip_serializing_if = "Option::is_none", default)]
            thread: Option<ThreadInformation>,
            /// Forum settings, when this channel is a forum (posts are threads)
            #[serde(skip_serializing_if = "Option::is_none", default)]
            forum: Option<ForumInformation>,
        },
    }

    /// State of a thread
    #[derive(Default)]
    pub struct ThreadInformation {
        /// Who started it
        pub owner: String,
        /// Message it was started from
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub starter_message: Option<String>,
        /// Hidden from the active list; a new message reopens it
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub archived: bool,
        /// Only people who can manage messages write in it
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub locked: bool,
        /// Forum tags of this post
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub tags: Vec<String>,
        /// Messages sent in it
        #[serde(default)]
        pub message_count: u32,
    }

    /// Settings of a forum channel
    #[derive(Default)]
    pub struct ForumInformation {
        /// Tags posts can carry
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub tags: Vec<ForumTag>,
        /// Every post needs at least one tag
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub require_tag: bool,
    }

    /// A forum tag
    pub struct ForumTag {
        pub id: String,
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub emoji: Option<String>,
    }

    #[derive(Default)]
    pub struct VoiceInformation {
        /// Maximium amount of users allowed in the voice channel at once
        #[serde(skip_serializing_if = "Option::is_none")]
        pub max_users: Option<usize>,
    }
);

auto_derived!(
    #[derive(Default)]
    pub struct PartialChannel {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub owner: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub icon: Option<File>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub nsfw: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub active: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub permissions: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub role_permissions: Option<HashMap<String, OverrideField>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub member_permissions: Option<HashMap<String, OverrideField>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_permissions: Option<OverrideField>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub last_message_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub voice: Option<VoiceInformation>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub slowmode: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub announcement: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub permissions_synced: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub thread: Option<ThreadInformation>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub forum: Option<ForumInformation>,
    }

    /// Optional fields on channel object
    pub enum FieldsChannel {
        Description,
        Icon,
        DefaultPermissions,
        Voice,
        Slowmode,
    }
);

#[allow(clippy::disallowed_methods)]
impl Channel {
    /* /// Create a channel
    pub async fn create(&self, db: &Database) -> Result<()> {
        db.insert_channel(self).await?;

        let event = EventV1::ChannelCreate(self.clone().into());
        match self {
            Self::SavedMessages { user, .. } => event.private(user.clone()).await,
            Self::DirectMessage { recipients, .. } | Self::Group { recipients, .. } => {
                for recipient in recipients {
                    event.clone().private(recipient.clone()).await;
                }
            }
            Self::TextChannel { server, .. } | Self::VoiceChannel { server, .. } => {
                event.p(server.clone()).await;
            }
        }

        Ok(())
    }*/

    /// A server channel, not saved yet
    pub fn build_server_channel(
        server: &str,
        id: String,
        data: v0::DataCreateServerChannel,
        default_permissions: Option<OverrideField>,
        permissions_synced: bool,
    ) -> Channel {
        match data.channel_type {
            v0::LegacyServerChannelType::Text => Channel::TextChannel {
                id: id.clone(),
                server: server.to_owned(),
                name: data.name,
                description: data.description,
                icon: None,
                last_message_id: None,
                default_permissions,
                role_permissions: HashMap::new(),
                member_permissions: HashMap::new(),
                nsfw: data.nsfw.unwrap_or(false),
                voice: data.voice.map(|voice| voice.into()),
                slowmode: None,
                announcement: data.announcement.unwrap_or(false),
                permissions_synced,
                parent: None,
                thread: None,
                forum: None,
            },
            v0::LegacyServerChannelType::Voice => Channel::TextChannel {
                id: id.clone(),
                server: server.to_owned(),
                name: data.name,
                description: data.description,
                icon: None,
                last_message_id: None,
                default_permissions,
                role_permissions: HashMap::new(),
                member_permissions: HashMap::new(),
                nsfw: data.nsfw.unwrap_or(false),
                voice: Some(data.voice.unwrap_or_default().into()),
                slowmode: None,
                announcement: false,
                permissions_synced,
                parent: None,
                thread: None,
                forum: None,
            },
            v0::LegacyServerChannelType::Forum => Channel::TextChannel {
                id: id.clone(),
                server: server.to_owned(),
                name: data.name,
                description: data.description,
                icon: None,
                last_message_id: None,
                default_permissions,
                role_permissions: HashMap::new(),
                member_permissions: HashMap::new(),
                nsfw: data.nsfw.unwrap_or(false),
                voice: None,
                slowmode: None,
                announcement: false,
                permissions_synced,
                parent: None,
                thread: None,
                forum: Some(ForumInformation::default()),
            },
        }
    }

    /// Create a new server channel
    pub async fn create_server_channel(
        db: &Database,
        server: &mut Server,
        data: v0::DataCreateServerChannel,
        update_server: bool,
    ) -> Result<Channel> {
        let config = config().await;
        // a category the server doesn't have is refused before anything is made
        let category = data.category.clone();
        if let Some(category) = &category {
            let exists = server
                .categories
                .as_ref()
                .is_some_and(|categories| categories.iter().any(|c| &c.id == category));
            if !exists {
                return Err(create_error!(NotFound));
            }
        }
        if server.channels.len() > config.features.limits.global.server_channels {
            return Err(create_error!(TooManyChannels {
                max: config.features.limits.global.server_channels,
            }));
        };

        let id = ulid::Ulid::new().to_string();
        // a private channel starts hidden from @everyone
        let default_permissions = data.private.unwrap_or(false).then_some(OverrideField {
            a: 0,
            d: ChannelPermission::ViewChannel as i64,
        });
        // a channel made in a category follows its permissions, unless it
        // starts private with its own
        let permissions_synced = category.is_some() && default_permissions.is_none();
        let channel = Channel::build_server_channel(
            &server.id,
            id.clone(),
            data,
            default_permissions,
            permissions_synced,
        );

        db.insert_channel(&channel).await?;

        if update_server {
            server
                .update(
                    db,
                    PartialServer {
                        channels: Some([server.channels.clone(), [id.clone()].into()].concat()),
                        categories: category.map(|category| {
                            let mut categories = server.categories.clone().unwrap_or_default();
                            if let Some(entry) = categories.iter_mut().find(|c| c.id == category) {
                                entry.channels.push(id.clone());
                            }
                            categories
                        }),
                        ..Default::default()
                    },
                    vec![],
                )
                .await?;

            EventV1::ChannelCreate(channel.clone().into())
                .p(server.id.clone())
                .await;
        }

        Ok(channel)
    }

    /// Create a group
    pub async fn create_group(
        db: &Database,
        mut data: v0::DataCreateGroup,
        owner_id: String,
    ) -> Result<Channel> {
        data.users.insert(owner_id.to_string());

        let config = config().await;
        if data.users.len() > config.features.limits.global.group_size {
            return Err(create_error!(GroupTooLarge {
                max: config.features.limits.global.group_size,
            }));
        }

        let id = ulid::Ulid::new().to_string();

        let icon = if let Some(icon_id) = data.icon {
            Some(File::use_channel_icon(db, &icon_id, &id, &owner_id).await?)
        } else {
            None
        };

        let recipients = data.users.into_iter().collect::<Vec<String>>();
        let channel = Channel::Group {
            id,

            name: data.name,
            owner: owner_id,
            description: data.description,
            recipients: recipients.clone(),

            icon,
            last_message_id: None,

            permissions: None,

            nsfw: data.nsfw.unwrap_or(false),
        };

        db.insert_channel(&channel).await?;

        EventV1::ChannelCreate(channel.clone().into())
            .p_broadcast(recipients)
            .await;

        Ok(channel)
    }

    /// Create a DM (or return the existing one / saved messages)
    pub async fn create_dm(db: &Database, user_a: &User, user_b: &User) -> Result<Channel> {
        // Try to find existing channel
        if let Ok(channel) = db.find_direct_message_channel(&user_a.id, &user_b.id).await {
            Ok(channel)
        } else {
            let channel = if user_a.id == user_b.id {
                // Create a new saved messages channel
                Channel::SavedMessages {
                    id: Ulid::new().to_string(),
                    user: user_a.id.to_string(),
                }
            } else {
                // Create a new DM channel
                Channel::DirectMessage {
                    id: Ulid::new().to_string(),
                    active: true, // show by default
                    recipients: vec![user_a.id.clone(), user_b.id.clone()],
                    last_message_id: None,
                }
            };

            db.insert_channel(&channel).await?;

            if let Channel::DirectMessage { .. } = &channel {
                EventV1::ChannelCreate(channel.clone().into())
                    .p_broadcast(vec![user_a.id.clone(), user_b.id.clone()])
                    .await;
            };

            Ok(channel)
        }
    }

    /// Add user to a group
    pub async fn add_user_to_group(
        &mut self,
        db: &Database,
        amqp: &AMQP,
        user: &User,
        by_id: &str,
    ) -> Result<()> {
        if let Channel::Group { recipients, .. } = self {
            if recipients.contains(&String::from(&user.id)) {
                return Err(create_error!(AlreadyInGroup));
            }

            let config = config().await;
            if recipients.len() >= config.features.limits.global.group_size {
                return Err(create_error!(GroupTooLarge {
                    max: config.features.limits.global.group_size
                }));
            }

            recipients.push(String::from(&user.id));
        }

        match &self {
            Channel::Group { id, .. } => {
                db.add_user_to_group(id, &user.id).await?;

                EventV1::ChannelGroupJoin {
                    id: id.to_string(),
                    user: user.id.to_string(),
                }
                .p(id.to_string())
                .await;

                SystemMessage::UserAdded {
                    id: user.id.to_string(),
                    by: by_id.to_string(),
                }
                .into_message(id.to_string())
                .send(
                    db,
                    Some(amqp),
                    MessageAuthor::System {
                        username: &user.username,
                        avatar: user.avatar.as_ref().map(|file| file.id.as_ref()),
                    },
                    None,
                    None,
                    self,
                    false,
                )
                .await
                .ok();

                EventV1::ChannelCreate(self.clone().into())
                    .p(user.id.to_string())
                    .await;

                Ok(())
            }
            _ => Err(create_error!(InvalidOperation)),
        }
    }

    /// Map out whether it is a direct DM
    pub fn is_direct_dm(&self) -> bool {
        matches!(self, Channel::DirectMessage { .. })
    }

    /// Check whether has a user as a recipient
    pub fn contains_user(&self, user_id: &str) -> bool {
        match self {
            Channel::Group { recipients, .. } | Channel::DirectMessage { recipients, .. } => {
                recipients.iter().any(|recipient| recipient == user_id)
            }
            Channel::SavedMessages { user, .. } => user == user_id,
            _ => false,
        }
    }

    /// Get list of recipients
    pub fn users(&self) -> Result<Vec<String>> {
        match self {
            Channel::Group { recipients, .. } | Channel::DirectMessage { recipients, .. } => {
                Ok(recipients.to_owned())
            }
            _ => Err(create_error!(NotFound)),
        }
    }

    /// Clone this channel's id
    pub fn id(&self) -> &str {
        match self {
            Channel::DirectMessage { id, .. }
            | Channel::Group { id, .. }
            | Channel::SavedMessages { id, .. }
            | Channel::TextChannel { id, .. } => id,
        }
    }

    /// Clone this channel's server id
    pub fn server(&self) -> Option<&str> {
        match self {
            Channel::TextChannel { server, .. } => Some(server),
            _ => None,
        }
    }

    /// The category whose permissions this channel follows, when it is
    /// synced with one
    pub fn synced_category<'a>(&self, server: &'a Server) -> Option<&'a Category> {
        match self {
            Channel::TextChannel {
                id,
                permissions_synced: true,
                ..
            } => server
                .categories
                .as_ref()?
                .iter()
                .find(|category| category.channels.contains(id)),
            _ => None,
        }
    }

    /// The default and role overrides that apply in this server channel:
    /// its category's when it is synced with one, otherwise its own
    pub fn effective_overrides<'a>(
        &'a self,
        server: Option<&'a Server>,
    ) -> Option<(Option<OverrideField>, &'a HashMap<String, OverrideField>)> {
        let Channel::TextChannel {
            default_permissions,
            role_permissions,
            ..
        } = self
        else {
            return None;
        };
        if let Some(category) = server.and_then(|server| self.synced_category(server)) {
            return Some((category.default_permissions, &category.role_permissions));
        }
        Some((*default_permissions, role_permissions))
    }

    /// Stop following the category before the channel's own permissions
    /// are edited. The channel first takes a copy of the category's
    /// overrides, so the edit itself gives or takes nobody's access.
    pub async fn unsync_permissions(&mut self, db: &Database, server: &Server) -> Result<()> {
        if !matches!(
            self,
            Channel::TextChannel {
                permissions_synced: true,
                ..
            }
        ) {
            return Ok(());
        }
        let (default_permissions, role_permissions) = match self.synced_category(server) {
            Some(category) => (category.default_permissions, category.role_permissions.clone()),
            None => (None, HashMap::new()),
        };
        let remove = if default_permissions.is_none() {
            vec![FieldsChannel::DefaultPermissions]
        } else {
            vec![]
        };
        self.update(
            db,
            PartialChannel {
                permissions_synced: Some(false),
                default_permissions,
                role_permissions: Some(role_permissions),
                ..Default::default()
            },
            remove,
        )
        .await
    }

    /// Follow the category's permissions again; the channel's own default
    /// and role overrides are cleared (per-member overrides stay)
    pub async fn sync_permissions(&mut self, db: &Database) -> Result<()> {
        if !matches!(self, Channel::TextChannel { .. }) {
            return Err(create_error!(InvalidOperation));
        }
        self.update(
            db,
            PartialChannel {
                permissions_synced: Some(true),
                role_permissions: Some(HashMap::new()),
                ..Default::default()
            },
            vec![FieldsChannel::DefaultPermissions],
        )
        .await
    }

    /// Gets this channel's voice information
    pub fn voice(&self) -> Option<Cow<VoiceInformation>> {
        match self {
            Self::DirectMessage { .. } | Self::Group { .. } => {
                Some(Cow::Owned(VoiceInformation::default()))
            }
            Self::TextChannel {
                voice: Some(voice), ..
            } => Some(Cow::Borrowed(voice)),
            _ => None,
        }
    }

    /// Set role permission on a channel
    pub async fn set_role_permission(
        &mut self,
        db: &Database,
        role_id: &str,
        permissions: OverrideField,
    ) -> Result<()> {
        match self {
            Channel::TextChannel {
                id,
                server,
                role_permissions,
                ..
            } => {
                db.set_channel_role_permission(id, role_id, permissions)
                    .await?;

                role_permissions.insert(role_id.to_string(), permissions);

                let event = EventV1::ChannelUpdate {
                    id: id.clone(),
                    data: PartialChannel {
                        role_permissions: Some(role_permissions.clone()),
                        ..Default::default()
                    }
                    .into(),
                    clear: vec![],
                };

                // Members who cannot see the channel are not subscribed to it,
                // so they also need the server copy to pick up a grant.
                event.clone().p(server.clone()).await;
                event.p(id.clone()).await;

                Ok(())
            }
            _ => Err(create_error!(InvalidOperation)),
        }
    }

    /// Set one member's permissions on a channel; an empty override removes it
    pub async fn set_member_permission(
        &mut self,
        db: &Database,
        user_id: &str,
        permissions: OverrideField,
    ) -> Result<()> {
        match self {
            Channel::TextChannel {
                id,
                server,
                member_permissions,
                ..
            } => {
                let mut next = member_permissions.clone();
                if permissions.a == 0 && permissions.d == 0 {
                    next.remove(user_id);
                } else {
                    next.insert(user_id.to_string(), permissions);
                }
                let partial = PartialChannel {
                    member_permissions: Some(next.clone()),
                    ..Default::default()
                };
                db.update_channel(id, &partial, vec![]).await?;
                *member_permissions = next;

                let event = EventV1::ChannelUpdate {
                    id: id.clone(),
                    data: partial.into(),
                    clear: vec![],
                };
                // as for roles: someone who couldn't see the channel isn't
                // subscribed to it yet, so the server copy reaches them
                event.clone().p(server.clone()).await;
                event.p(id.clone()).await;

                Ok(())
            }
            _ => Err(create_error!(InvalidOperation)),
        }
    }

    /// Update channel data
    pub async fn update(
        &mut self,
        db: &Database,
        partial: PartialChannel,
        remove: Vec<FieldsChannel>,
    ) -> Result<()> {
        for field in &remove {
            self.remove_field(field);
        }

        self.apply_options(partial.clone());

        let id = self.id().to_string();
        db.update_channel(&id, &partial, remove.clone()).await?;

        let permissions_changed = partial.default_permissions.is_some()
            || partial.role_permissions.is_some()
            || partial.permissions_synced.is_some()
            || remove.contains(&FieldsChannel::DefaultPermissions);

        let event = EventV1::ChannelUpdate {
            id: id.clone(),
            data: partial.into(),
            clear: remove.into_iter().map(|v| v.into()).collect(),
        };

        // As in set_role_permission: members who cannot see the channel need
        // the server copy to pick up a change that lets them in.
        if permissions_changed {
            if let Some(server) = self.server() {
                event.clone().p(server.to_string()).await;
            }
        }

        event.p(id).await;

        Ok(())
    }

    /// Remove a field from Channel object
    pub fn remove_field(&mut self, field: &FieldsChannel) {
        match field {
            FieldsChannel::Description => match self {
                Self::Group { description, .. } | Self::TextChannel { description, .. } => {
                    description.take();
                }
                _ => {}
            },
            FieldsChannel::Icon => match self {
                Self::Group { icon, .. } | Self::TextChannel { icon, .. } => {
                    icon.take();
                }
                _ => {}
            },
            FieldsChannel::DefaultPermissions => match self {
                Self::TextChannel {
                    default_permissions,
                    ..
                } => {
                    default_permissions.take();
                }
                _ => {}
            },
            FieldsChannel::Voice => match self {
                Self::TextChannel { voice, .. } => {
                    voice.take();
                }
                _ => {}
            },
            FieldsChannel::Slowmode => match self {
                Self::TextChannel { slowmode, .. } => {
                    slowmode.take();
                }
                _ => {}
            },
        }
    }

    /// Remove multiple fields from Channel object
    pub fn remove_fields(&mut self, partial: Vec<FieldsChannel>) {
        for field in partial {
            self.remove_field(&field)
        }
    }

    /// Apply partial channel to channel
    #[allow(deprecated)]
    pub fn apply_options(&mut self, partial: PartialChannel) {
        match self {
            Self::SavedMessages { .. } => {}
            Self::DirectMessage { active, .. } => {
                if let Some(v) = partial.active {
                    *active = v;
                }
            }
            Self::Group {
                name,
                owner,
                description,
                icon,
                nsfw,
                permissions,
                ..
            } => {
                if let Some(v) = partial.name {
                    *name = v;
                }

                if let Some(v) = partial.owner {
                    *owner = v;
                }

                if let Some(v) = partial.description {
                    description.replace(v);
                }

                if let Some(v) = partial.icon {
                    icon.replace(v);
                }

                if let Some(v) = partial.nsfw {
                    *nsfw = v;
                }

                if let Some(v) = partial.permissions {
                    permissions.replace(v);
                }
            }
            Self::TextChannel {
                name,
                description,
                icon,
                nsfw,
                default_permissions,
                role_permissions,
                member_permissions,
                voice,
                announcement,
                permissions_synced,
                thread,
                forum,
                ..
            } => {
                if let Some(v) = partial.permissions_synced {
                    *permissions_synced = v;
                }

                if let Some(v) = partial.thread {
                    thread.replace(v);
                }

                if let Some(v) = partial.forum {
                    forum.replace(v);
                }

                if let Some(v) = partial.name {
                    *name = v;
                }

                if let Some(v) = partial.description {
                    description.replace(v);
                }

                if let Some(v) = partial.icon {
                    icon.replace(v);
                }

                if let Some(v) = partial.nsfw {
                    *nsfw = v;
                }

                if let Some(v) = partial.role_permissions {
                    *role_permissions = v;
                }

                if let Some(v) = partial.member_permissions {
                    *member_permissions = v;
                }

                if let Some(v) = partial.default_permissions {
                    default_permissions.replace(v);
                }

                if let Some(v) = partial.voice {
                    voice.replace(v);
                }

                if let Some(v) = partial.announcement {
                    *announcement = v;
                }
            }
        }
    }

    /// Generates a PartialChannel containing the data which has changed in an update
    pub fn generate_diff(
        &self,
        partial: &PartialChannel,
        remove: &[FieldsChannel],
    ) -> PartialChannel {
        let mut before = PartialChannel::default();

        match self {
            Channel::SavedMessages { .. } => {}
            Channel::DirectMessage {
                active,
                last_message_id,
                ..
            } => {
                if partial.active.is_some() {
                    before.active = Some(*active);
                };

                if partial.last_message_id.is_some() {
                    before.last_message_id = last_message_id.clone()
                };
            }
            Channel::Group {
                name,
                owner,
                description,
                icon,
                last_message_id,
                permissions,
                nsfw,
                ..
            } => {
                if partial.name.is_some() {
                    before.name = Some(name.clone());
                };

                if partial.owner.is_some() {
                    before.owner = Some(owner.clone());
                };

                if partial.description.is_some() || remove.contains(&FieldsChannel::Description) {
                    before.description = description.clone();
                };

                if partial.icon.is_some() || remove.contains(&FieldsChannel::Icon) {
                    before.icon = icon.clone();
                };

                if partial.last_message_id.is_some() {
                    before.last_message_id = last_message_id.clone()
                };

                if partial.permissions.is_some() {
                    before.permissions = *permissions;
                };

                if partial.nsfw.is_some() {
                    before.nsfw = Some(*nsfw);
                };
            }
            Channel::TextChannel {
                name,
                description,
                icon,
                last_message_id,
                default_permissions,
                role_permissions,
                member_permissions,
                nsfw,
                voice,
                slowmode,
                announcement,
                ..
            } => {
                if partial.name.is_some() {
                    before.name = Some(name.clone());
                };

                if partial.description.is_some() || remove.contains(&FieldsChannel::Description) {
                    before.description = description.clone();
                };

                if partial.icon.is_some() || remove.contains(&FieldsChannel::Icon) {
                    before.icon = icon.clone();
                };

                if partial.last_message_id.is_some() {
                    before.last_message_id = last_message_id.clone()
                };

                if partial.default_permissions.is_some()
                    || remove.contains(&FieldsChannel::DefaultPermissions)
                {
                    before.default_permissions = *default_permissions;
                };

                if partial.member_permissions.is_some() {
                    before.member_permissions = Some(member_permissions.clone());
                }

                if partial.role_permissions.is_some() {
                    before.role_permissions = Some(role_permissions.clone());
                };

                if partial.nsfw.is_some() {
                    before.nsfw = Some(*nsfw);
                };

                if partial.voice.is_some() || remove.contains(&FieldsChannel::Voice) {
                    before.voice = voice.clone();
                };

                if partial.slowmode.is_some() {
                    before.slowmode = *slowmode;
                }

                if partial.announcement.is_some() {
                    before.announcement = Some(*announcement);
                }
            }
        }

        before
    }

    /// Acknowledge a message
    pub async fn ack(&self, user: &str, message: &str, amqp: &AMQP) -> Result<()> {
        EventV1::ChannelAck {
            id: self.id().to_string(),
            user: user.to_string(),
            message_id: message.to_string(),
        }
        .p(user.to_string())
        .await;

        crate::util::acker::ack_channel(user, self.id(), message, amqp).await
    }

    /// Remove user from a group
    pub async fn remove_user_from_group(
        &self,
        db: &Database,
        amqp: &AMQP,
        user: &User,
        by_id: Option<&str>,
        silent: bool,
    ) -> Result<()> {
        match &self {
            Channel::Group {
                id,
                name,
                owner,
                recipients,
                ..
            } => {
                if &user.id == owner {
                    if let Some(new_owner) = recipients.iter().find(|x| *x != &user.id) {
                        db.update_channel(
                            id,
                            &PartialChannel {
                                owner: Some(new_owner.into()),
                                ..Default::default()
                            },
                            vec![],
                        )
                        .await?;

                        SystemMessage::ChannelOwnershipChanged {
                            from: owner.to_string(),
                            to: new_owner.to_string(),
                        }
                        .into_message(id.to_string())
                        .send(
                            db,
                            Some(amqp),
                            MessageAuthor::System {
                                username: name,
                                avatar: None,
                            },
                            None,
                            None,
                            self,
                            false,
                        )
                        .await
                        .ok();
                    } else {
                        return self.delete(db).await;
                    }
                }

                db.remove_user_from_group(id, &user.id).await?;

                EventV1::ChannelGroupLeave {
                    id: id.to_string(),
                    user: user.id.to_string(),
                }
                .p(id.to_string())
                .await;

                if !silent {
                    if let Some(by) = by_id {
                        SystemMessage::UserRemove {
                            id: user.id.to_string(),
                            by: by.to_string(),
                        }
                    } else {
                        SystemMessage::UserLeft {
                            id: user.id.to_string(),
                        }
                    }
                    .into_message(id.to_string())
                    .send(
                        db,
                        Some(amqp),
                        MessageAuthor::System {
                            username: &user.username,
                            avatar: user.avatar.as_ref().map(|file| file.id.as_ref()),
                        },
                        None,
                        None,
                        self,
                        false,
                    )
                    .await
                    .ok();
                }

                Ok(())
            }

            _ => Err(create_error!(InvalidOperation)),
        }
    }

    /// Delete a channel
    pub async fn delete(&self, db: &Database) -> Result<()> {
        // a channel's threads (and forum posts) go with it
        if let Channel::TextChannel { parent: None, .. } = self {
            for archived in [false, true] {
                loop {
                    let threads = db.fetch_threads(self.id(), archived, None, 100).await?;
                    if threads.is_empty() {
                        break;
                    }
                    for thread in threads {
                        EventV1::ChannelDelete {
                            id: thread.id().to_string(),
                        }
                        .p(thread.id().to_string())
                        .await;
                        db.delete_channel(&thread).await?;
                    }
                }
            }
        }

        // command permission rules about this channel go too
        if let Channel::TextChannel {
            server,
            parent: None,
            ..
        } = self
        {
            db.remove_command_permission_target(
                server,
                revolt_models::v0::CommandPermissionType::Channel,
                self.id(),
            )
            .await?;
        }

        let id = self.id().to_string();
        EventV1::ChannelDelete { id: id.clone() }.p(id).await;
        // TODO: missing functionality:
        // - group invites
        db.delete_channel(self).await
    }
}

#[cfg(feature = "mongodb")]
impl IntoDocumentPath for FieldsChannel {
    fn as_path(&self) -> Option<&'static str> {
        Some(match self {
            FieldsChannel::Description => "description",
            FieldsChannel::Icon => "icon",
            FieldsChannel::DefaultPermissions => "default_permissions",
            FieldsChannel::Voice => "voice",
            FieldsChannel::Slowmode => "slowmode",
        })
    }
}

#[cfg(test)]
mod tests {
    use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
    use lapin::{ExchangeKind, options::ExchangeDeclareOptions, types::FieldTable};
    use crate::{fixture, util::permissions::DatabasePermissionQuery};

    #[tokio::test]
    async fn permissions_group_channel() {
        database_test!(|db| async move {
            fixture!(db, "group_with_members",
                owner user 0
                member1 user 1
                member2 user 2
                channel channel 3);

            let mut query = DatabasePermissionQuery::new(&db, &owner).channel(&channel);
            assert!(calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));

            let mut query = DatabasePermissionQuery::new(&db, &member1).channel(&channel);
            assert!(calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));

            let mut query = DatabasePermissionQuery::new(&db, &member2).channel(&channel);
            assert!(!calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));
        });
    }

    #[tokio::test]
    async fn permissions_text_channel() {
        database_test!(|db| async move {
            fixture!(db, "server_with_roles",
                owner user 0
                moderator user 1
                user user 2
                channel channel 3);

            let mut query = DatabasePermissionQuery::new(&db, &owner).channel(&channel);
            assert!(calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));

            let mut query = DatabasePermissionQuery::new(&db, &moderator).channel(&channel);
            assert!(calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));

            let mut query = DatabasePermissionQuery::new(&db, &user).channel(&channel);
            assert!(!calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));
        });
    }
}
