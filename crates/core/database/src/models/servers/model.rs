use std::collections::{HashMap, HashSet};

use redis_kiss::{
    get_connection,
    redis::{SetExpiry, SetOptions},
    AsyncCommands,
};
use iso8601_timestamp::Timestamp;
use revolt_models::v0::{self, DataCreateServerChannel};
use revolt_permissions::{ChannelPermission, OverrideField, DEFAULT_PERMISSION_SERVER};
use revolt_result::Result;
use ulid::Ulid;

use crate::{events::client::EventV1, Channel, Database, File, User};

auto_derived_partial!(
    /// Server
    pub struct Server {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,
        /// User id of the owner
        pub owner: String,

        /// Name of the server
        pub name: String,
        /// Description for the server
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,

        /// Channels within this server
        // TODO: investigate if this is redundant and can be removed
        pub channels: Vec<String>,
        /// Categories for this server
        #[serde(skip_serializing_if = "Option::is_none")]
        pub categories: Option<Vec<Category>>,
        /// Configuration for sending system event messages
        #[serde(skip_serializing_if = "Option::is_none")]
        pub system_messages: Option<SystemMessageChannels>,

        /// Roles for this server
        #[serde(
            default = "HashMap::<String, Role>::new",
            skip_serializing_if = "HashMap::<String, Role>::is_empty"
        )]
        pub roles: HashMap<String, Role>,
        /// Default set of server and channel permissions
        pub default_permissions: i64,

        /// Icon attachment
        #[serde(skip_serializing_if = "Option::is_none")]
        pub icon: Option<File>,
        /// Banner attachment
        #[serde(skip_serializing_if = "Option::is_none")]
        pub banner: Option<File>,

        /// Bitfield of server flags
        #[serde(skip_serializing_if = "Option::is_none")]
        pub flags: Option<i32>,

        /// Whether this server is flagged as not safe for work
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub nsfw: bool,
        /// Whether to enable analytics
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub analytics: bool,
        /// Whether this server should be publicly discoverable
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub discoverable: bool,
        /// Who can become a member (missing = open, the old behaviour);
        /// separate from Discover listing
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub access: Option<v0::ServerAccess>,
        /// What members without a role need before they can talk (missing = none)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub verification_level: Option<v0::ServerVerificationLevel>,
        /// Security action: every invite and Discover join is refused until then
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub invites_paused_until: Option<Timestamp>,
        /// Security action: members who aren't friends can't DM each other until then
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub dms_paused_until: Option<Timestamp>,
        /// AutoMod rules; only for people who manage the server (never
        /// part of the server object members receive)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub automod: Option<Vec<v0::AutoModRule>>,
        /// Rules screening: members who joined since then accept the rules
        /// before they can talk (missing = off)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub rules_screening_since: Option<Timestamp>,

        /// Supercharges on the server and the level they reach
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub supercharge: Option<v0::ServerSupercharge>,
        /// Custom invite link, a Wings level perk
        #[serde(skip_serializing_if = "Option::is_none")]
        pub vanity_invite: Option<v0::ServerVanityInvite>,

        /// Channel that holds the server rules
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub rules_channel: Option<String>,
        /// Live counters shown at the top of the channel list, in order
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub stats: Option<Vec<v0::ServerStat>>,
        /// Welcome page shown before the channels
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub lobby: Option<v0::ServerLobby>,

        /// Threads are switched off on this server: nothing new, the
        /// threads it has stay readable
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub threads_disabled: bool,
        /// Forums are switched off on this server: nothing new, the forums
        /// and posts it has stay readable
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub forums_disabled: bool,
        /// Notifications for members who never chose a level for this
        /// server (missing = mentions only)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub default_notifications: Option<v0::ServerNotificationLevel>,
    },
    "PartialServer"
);

auto_derived_partial!(
    /// Role
    pub struct Role {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,
        /// Role name
        pub name: String,
        /// Permissions available to this role
        pub permissions: OverrideField,
        /// Colour used for this role
        ///
        /// This can be any valid CSS colour
        #[serde(skip_serializing_if = "Option::is_none")]
        pub colour: Option<String>,
        /// Whether this role should be shown separately on the member sidebar
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub hoist: bool,
        /// Ranking of this role
        #[serde(default)]
        pub rank: i64,
        /// Custom icon attachment
        #[serde(skip_serializing_if = "Option::is_none")]
        pub icon: Option<File>,
        /// Id of the bot that owns this role, if it is a managed role
        ///
        /// Managed roles are created automatically (e.g. via bot invite) and
        /// should be hidden from "assign role" UI and cleaned up when the
        /// owning bot leaves the server.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub owner: Option<String>,
    },
    "PartialRole"
);

auto_derived!(
    /// Channel category
    #[derive(Default)]
    pub struct Category {
        /// Unique ID for this category
        pub id: String,
        /// Title for this category
        pub title: String,
        /// Channels in this category
        pub channels: Vec<String>,
        /// Default permissions for the channels synced with this category
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub default_permissions: Option<OverrideField>,
        /// Role permissions for the channels synced with this category
        #[serde(
            default = "HashMap::<String, OverrideField>::new",
            skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
        )]
        pub role_permissions: HashMap<String, OverrideField>,
    }

    /// System message channel assignments
    pub struct SystemMessageChannels {
        /// ID of channel to send user join messages in
        #[serde(skip_serializing_if = "Option::is_none")]
        pub user_joined: Option<String>,
        /// ID of channel to send user left messages in
        #[serde(skip_serializing_if = "Option::is_none")]
        pub user_left: Option<String>,
        /// ID of channel to send user kicked messages in
        #[serde(skip_serializing_if = "Option::is_none")]
        pub user_kicked: Option<String>,
        /// ID of channel to send user banned messages in
        #[serde(skip_serializing_if = "Option::is_none")]
        pub user_banned: Option<String>,
    }

    /// Optional fields on server object
    pub enum FieldsServer {
        Description,
        Categories,
        SystemMessages,
        Icon,
        Banner,
        RulesChannel,
        RulesScreening,
        Stats,
        Lobby,
        VanityInvite,
    }

    /// Optional fields on server object
    pub enum FieldsRole {
        Colour,
        Icon,
    }
);

#[allow(clippy::disallowed_methods)]
impl Server {
    /// A security action refuses every invite and Discover join right now
    pub fn invites_paused(&self) -> bool {
        self.invites_paused_until
            .is_some_and(|until| until > Timestamp::now_utc())
    }

    /// A security action stops DMs between members who aren't friends
    pub fn dms_paused(&self) -> bool {
        self.dms_paused_until
            .is_some_and(|until| until > Timestamp::now_utc())
    }

    /// Shown on Discover: listed, and anyone can join (a server listed before
    /// it was locked or set to approval stays off the list)
    pub fn is_listed(&self) -> bool {
        self.discoverable && self.access.unwrap_or_default() == v0::ServerAccess::Open
    }

    /// Create a server
    pub async fn create(
        db: &Database,
        mut data: v0::DataCreateServer,
        owner: &User,
        create_default_channels: bool,
    ) -> Result<(Server, Vec<Channel>)> {
        let blueprint = data.blueprint.take();
        let mut server = Server {
            id: ulid::Ulid::new().to_string(),
            owner: owner.id.to_string(),
            name: data.name,
            description: data.description,
            channels: vec![],
            nsfw: data.nsfw.unwrap_or(false),
            default_permissions: *DEFAULT_PERMISSION_SERVER as i64,

            analytics: false,
            banner: None,
            categories: None,
            // never listed at creation, whatever the access choice
            discoverable: false,
            access: data.access,
            verification_level: None,
            default_notifications: None,
            invites_paused_until: None,
            dms_paused_until: None,
            automod: None,
            rules_screening_since: None,
            flags: None,
            icon: None,
            roles: HashMap::new(),
            system_messages: None,
            rules_channel: None,
            stats: None,
            lobby: None,
            threads_disabled: false,
            forums_disabled: false,
            supercharge: None,
            vanity_invite: None,
        };

        let mut channels = match &blueprint {
            Some(blueprint) => server.apply_blueprint(blueprint),
            None => vec![],
        };
        if channels.is_empty() && create_default_channels {
            let general = DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Text,
                name: "General".to_string(),
                ..Default::default()
            };
            let id = Ulid::new().to_string();
            channels.push(Channel::build_server_channel(&server.id, id, general, None, false));
        }
        server.channels = channels.iter().map(|c| c.id().to_string()).collect();

        // first, so a refused picture costs nothing
        if let Some(icon) = blueprint.and_then(|blueprint| blueprint.icon) {
            server.icon = Some(File::use_server_icon(db, &icon, &server.id, &owner.id).await?);
        }

        if let Err(error) = server.save_with_channels(db, &channels).await {
            if let Some(icon) = &server.icon {
                if let Err(error) = db.mark_attachment_as_deleted(&icon.id).await {
                    log::error!("icon of a server that failed: {error:?}");
                }
            }
            return Err(error);
        }
        Ok((server, channels))
    }

    /// Put a blueprint on a server that isn't saved yet and return its rooms,
    /// not saved either. Roles and categories live in the server document,
    /// and a read-only room carries its deny in its own document, so nothing
    /// is ever stored in an open or half-built state.
    fn apply_blueprint(&mut self, blueprint: &v0::DataServerBlueprint) -> Vec<Channel> {
        let send = ChannelPermission::SendMessage as i64;
        let mut writers = HashMap::new();
        for (rank, role) in blueprint.roles.iter().enumerate() {
            let id = Ulid::new().to_string();
            if role.posts_in_read_only {
                writers.insert(id.clone(), OverrideField { a: send, d: 0 });
            }
            self.roles.insert(
                id.clone(),
                Role {
                    id,
                    name: role.name.clone(),
                    rank: rank as i64,
                    colour: role.colour.clone(),
                    hoist: role.hoist,
                    permissions: OverrideField { a: role.allow as i64, d: 0 },
                    icon: None,
                    owner: None,
                },
            );
        }
        if blueprint.member_invites == Some(false) {
            self.default_permissions &= !(ChannelPermission::InviteOthers as i64);
        }

        let server = self.id.clone();
        let mut rules = None;
        let mut build = |room: &v0::BlueprintChannel, in_category: bool| {
            let id = Ulid::new().to_string();
            if room.rules {
                rules = Some(id.clone());
            }
            let data = DataCreateServerChannel {
                channel_type: room.channel_type.clone(),
                name: room.name.clone(),
                ..Default::default()
            };
            let closed = room.read_only.then_some(OverrideField { a: 0, d: send });
            // like a channel made in a category, unless it has its own rights
            let synced = in_category && !room.read_only;
            let mut channel = Channel::build_server_channel(&server, id, data, closed, synced);
            if let (true, Channel::TextChannel { role_permissions, .. }) =
                (room.read_only, &mut channel)
            {
                role_permissions.clone_from(&writers);
            }
            channel
        };

        let mut channels: Vec<Channel> = blueprint.channels.iter().map(|room| build(room, false)).collect();
        let mut categories = Vec::with_capacity(blueprint.categories.len());
        for category in &blueprint.categories {
            let rooms: Vec<Channel> = category.channels.iter().map(|room| build(room, true)).collect();
            categories.push(Category {
                id: Ulid::new().to_string(),
                title: category.title.clone(),
                channels: rooms.iter().map(|c| c.id().to_string()).collect(),
                default_permissions: None,
                role_permissions: HashMap::new(),
            });
            channels.extend(rooms);
        }
        if !categories.is_empty() {
            self.categories = Some(categories);
        }
        self.rules_channel = rules;
        channels
    }

    /// Save a new server and its rooms. The rooms come first: until the
    /// server exists nobody can reach them, and if a step fails the rooms
    /// saved so far are taken back, so no part of it stays.
    pub async fn save_with_channels(&self, db: &Database, channels: &[Channel]) -> Result<()> {
        let mut saved = 0;
        let mut result = Ok(());
        for channel in channels {
            result = db.insert_channel(channel).await;
            if result.is_err() {
                break;
            }
            saved += 1;
        }
        if result.is_ok() {
            result = db.insert_server(self).await;
        }
        if result.is_err() {
            let ids: Vec<String> = channels[..saved].iter().map(|c| c.id().to_string()).collect();
            if let Err(error) = db.delete_unattached_channels(&ids).await {
                log::error!("rooms {ids:?} of a failed server: {error:?}");
            }
        }
        result
    }

    /// Take away a server that could not be finished, with its rooms
    pub async fn discard(self, db: &Database, channels: &[Channel]) {
        let id = self.id.clone();
        for channel in channels {
            if let Err(error) = db.delete_channel(channel).await {
                log::error!("room {} of a failed server: {error:?}", channel.id());
            }
        }
        if let Err(error) = self.delete(db).await {
            log::error!("failed server {id}: {error:?}");
        }
    }

    /// Update server data
    pub async fn update(
        &mut self,
        db: &Database,
        partial: PartialServer,
        remove: Vec<FieldsServer>,
    ) -> Result<()> {
        for field in &remove {
            self.remove_field(field);
        }

        self.apply_options(partial.clone());

        db.update_server(&self.id, &partial, remove.clone()).await?;

        EventV1::ServerUpdate {
            id: self.id.clone(),
            data: partial.into(),
            clear: remove.into_iter().map(|v| v.into()).collect(),
        }
        .p(self.id.clone())
        .await;

        Ok(())
    }

    /// Delete a server
    pub async fn delete(self, db: &Database) -> Result<()> {
        EventV1::ServerDelete {
            id: self.id.clone(),
        }
        .p(self.id.clone())
        .await;

        db.unplace_supercharges_from_server(&self.id).await?;
        // its paid Discover places go to whoever waits next
        db.delete_promotions_for_target(&self.id).await?;
        db.delete_server(&self.id).await
    }

    /// Remove a field from Server
    pub fn remove_field(&mut self, field: &FieldsServer) {
        match field {
            FieldsServer::Description => self.description = None,
            FieldsServer::Categories => self.categories = None,
            FieldsServer::SystemMessages => self.system_messages = None,
            FieldsServer::Icon => self.icon = None,
            FieldsServer::Banner => self.banner = None,
            FieldsServer::RulesChannel => self.rules_channel = None,
            FieldsServer::RulesScreening => self.rules_screening_since = None,
            FieldsServer::Stats => self.stats = None,
            FieldsServer::Lobby => self.lobby = None,
            FieldsServer::VanityInvite => self.vanity_invite = None,
        }
    }

    /// Generates a PartialServer containing the data which has changed in an update
    pub fn generate_diff(&self, partial: &PartialServer, remove: &[FieldsServer]) -> PartialServer {
        let mut before = PartialServer::default();

        generate_diff!(
            self, before, partial, remove,
            (
                owner,
                name,
                (FieldsServer::Description) description,
                (FieldsServer::Categories) categories,
                (FieldsServer::SystemMessages) system_messages,
                roles,
                default_permissions,
                (FieldsServer::Icon) icon,
                (FieldsServer::Banner) banner,
                nsfw,
                analytics,
                discoverable,
                (FieldsServer::RulesChannel) rules_channel,
                (FieldsServer::Stats) stats,
                (FieldsServer::Lobby) lobby,
                threads_disabled,
                forums_disabled,
            )
        );

        // optional without a remove field, so outside the macro
        if partial.access.is_some() {
            before.access = self.access;
        }
        if partial.verification_level.is_some() {
            before.verification_level = self.verification_level;
        }
        if partial.default_notifications.is_some() {
            before.default_notifications = self.default_notifications;
        }
        if partial.invites_paused_until.is_some() {
            before.invites_paused_until = self.invites_paused_until;
        }
        if partial.dms_paused_until.is_some() {
            before.dms_paused_until = self.dms_paused_until;
        }
        if partial.rules_screening_since.is_some() {
            before.rules_screening_since = self.rules_screening_since;
        }
        if partial.automod.is_some() {
            before.automod = self.automod.clone();
        }

        before
    }

    /// Ordered roles list
    pub fn ordered_roles(&self) -> Vec<(String, Role)> {
        let mut ordered_roles = self.roles.clone().into_iter().collect::<Vec<_>>();
        ordered_roles.sort_by(|(_, role_a), (_, role_b)| role_a.rank.cmp(&role_b.rank));
        ordered_roles
    }

    /// Set role permission on a server
    pub async fn set_role_permission(
        &mut self,
        db: &Database,
        role_id: &str,
        permissions: OverrideField,
    ) -> Result<()> {
        if let Some(role) = self.roles.get_mut(role_id) {
            role.update(
                db,
                &self.id,
                PartialRole {
                    permissions: Some(permissions),
                    ..Default::default()
                },
                vec![],
            )
            .await?;

            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Reorders the server's roles rankings
    pub async fn set_role_ordering(&mut self, db: &Database, new_order: Vec<String>) -> Result<()> {
        // New order must always contain every role
        debug_assert_eq!(self.roles.len(), new_order.len());

        // Set the role's ranks to the positions in the vec
        for (rank, id) in new_order.iter().enumerate() {
            self.roles.get_mut(id).unwrap().rank = rank as i64;
        }

        db.update_server(
            &self.id,
            &PartialServer {
                roles: Some(self.roles.clone()),
                ..Default::default()
            },
            Vec::new(),
        )
        .await?;

        // Publish bulk update event
        EventV1::ServerRoleRanksUpdate {
            id: self.id.clone(),
            ranks: new_order,
        }
        .p(self.id.clone())
        .await;

        Ok(())
    }

    /// Delete the managed role owned by the given bot in this server, if one exists.
    /// No-op if the bot never had a managed role.
    pub async fn cleanup_managed_bot_role(&self, db: &Database, bot_id: &str) -> Result<()> {
        if let Some(role) = self
            .roles
            .values()
            .find(|role| role.owner.as_deref() == Some(bot_id))
        {
            role.delete(db, &self.id).await?;
        }

        Ok(())
    }

    /// Gets a approximate count of the members in this server
    ///
    /// this value is cached for one hour
    pub async fn get_approximate_member_count(&self, db: &Database) -> usize {
        let Ok(mut redis) = get_connection().await else {
            return 0;
        };
        let key = format!("member_count:{}", &self.id);

        if let Some(count) = redis.get::<_, Option<usize>>(&key).await.ok().flatten() {
            count
        } else {
            let count = db.fetch_member_count(&self.id).await.unwrap_or(0);
            let _ = redis
                .set_options::<_, _, ()>(
                    &key,
                    count,
                    SetOptions::default().with_expiration(SetExpiry::EX(60 * 60)),
                )
                .await;
            count
        }
    }

    /// Gets an approximate count of the members online right now, for
    /// public pages such as Discover.
    ///
    /// Privacy rules (owner, 2026-11-03): members who chose Invisible are
    /// never counted; servers with fewer than `MIN_MEMBERS` members get no
    /// number at all; the number is rounded here, in the API, so no client
    /// can show the exact value. `None` also for servers too big to check
    /// cheaply. The result (including `None`) is cached for five minutes.
    pub async fn get_approximate_online_count(&self, db: &Database) -> Option<usize> {
        const MAX_CHECKED: usize = 5000;
        const MIN_MEMBERS: usize = 10;

        let mut redis = get_connection().await.ok()?;
        let key = format!("online_count:{}", &self.id);
        if let Some(count) = redis.get::<_, Option<i64>>(&key).await.ok().flatten() {
            return usize::try_from(count).ok();
        }

        let members = self.get_approximate_member_count(db).await;
        let count = if !(MIN_MEMBERS..=MAX_CHECKED).contains(&members) {
            None
        } else {
            let ids: Vec<String> = db
                .fetch_all_members(&self.id)
                .await
                .ok()?
                .into_iter()
                .map(|member| member.id.user)
                .collect();
            let online = revolt_presence::filter_online(&ids).await;
            let mut visible = 0;
            let online: Vec<String> = online.into_iter().collect();
            for chunk in online.chunks(500) {
                let users = db.fetch_users(chunk).await.ok()?;
                visible += users
                    .iter()
                    .filter(|user| {
                        user.status.as_ref().and_then(|s| s.presence.as_ref())
                            != Some(&crate::Presence::Invisible)
                    })
                    .count();
            }
            Some(round_online(visible))
        };

        let _ = redis
            .set_options::<_, _, ()>(
                &key,
                count.map(|c| c as i64).unwrap_or(-1),
                SetOptions::default().with_expiration(SetExpiry::EX(5 * 60)),
            )
            .await;
        count
    }
}

/// Round a public online count: nearest 5 under 100, 10 under 1,000, else 50
fn round_online(count: usize) -> usize {
    let step = match count {
        0..=99 => 5,
        100..=999 => 10,
        _ => 50,
    };
    (count + step / 2) / step * step
}

#[cfg(test)]
mod online_rounding {
    use super::round_online;

    #[test]
    fn rounds_in_steps() {
        assert_eq!(round_online(0), 0);
        assert_eq!(round_online(2), 0);
        assert_eq!(round_online(3), 5);
        assert_eq!(round_online(47), 45);
        assert_eq!(round_online(123), 120);
        assert_eq!(round_online(1234), 1250);
    }
}

impl Role {
    /// Into optional struct
    pub fn into_optional(self) -> PartialRole {
        PartialRole {
            id: Some(self.id),
            name: Some(self.name),
            permissions: Some(self.permissions),
            colour: self.colour,
            hoist: Some(self.hoist),
            rank: Some(self.rank),
            icon: self.icon,
            owner: self.owner,
        }
    }

    /// Create a role
    pub async fn create(db: &Database, server: &Server, name: String) -> Result<Self> {
        Self::create_inner(db, server, name, None).await
    }

    /// Create a role owned/managed by a bot
    pub async fn create_managed(
        db: &Database,
        server: &Server,
        name: String,
        owner_bot_id: String,
    ) -> Result<Self> {
        Self::create_inner(db, server, name, Some(owner_bot_id)).await
    }

    /// Helper function to avoid code duplication between `create` and `create_managed`
    async fn create_inner(
        db: &Database,
        server: &Server,
        name: String,
        owner: Option<String>,
    ) -> Result<Self> {
        let role = Role {
            id: Ulid::new().to_string(),
            name,
            // Rank of the new role should be below the lowest role
            rank: server.roles.len() as i64,
            colour: None,
            hoist: false,
            permissions: Default::default(),
            icon: None,
            owner,
        };

        db.insert_role(&server.id, &role).await?;

        EventV1::ServerRoleUpdate {
            id: server.id.clone(),
            role_id: role.id.clone(),
            data: role.clone().into_optional().into(),
            clear: vec![],
        }
            .p(server.id.clone())
            .await;

        Ok(role)
    }

    pub fn is_managed(&self) -> bool {
        self.owner.is_some()
    }

    /// Update server data
    pub async fn update(
        &mut self,
        db: &Database,
        server_id: &str,
        partial: PartialRole,
        remove: Vec<FieldsRole>,
    ) -> Result<()> {
        for field in &remove {
            self.remove_field(field);
        }

        self.apply_options(partial.clone());

        db.update_role(server_id, &self.id, &partial, remove.clone())
            .await?;

        EventV1::ServerRoleUpdate {
            id: server_id.to_string(),
            role_id: self.id.clone(),
            data: partial.into(),
            clear: remove.into_iter().map(Into::into).collect(),
        }
        .p(server_id.to_string())
        .await;

        Ok(())
    }

    /// Remove field from Role
    pub fn remove_field(&mut self, field: &FieldsRole) {
        match field {
            FieldsRole::Colour => self.colour = None,
            FieldsRole::Icon => self.icon = None,
        }
    }

    /// Generates a PartialRole containing the data which has changed in an update
    pub fn generate_diff(&self, partial: &PartialRole, remove: &[FieldsRole]) -> PartialRole {
        let mut before = PartialRole::default();

        generate_diff!(
            self, before, partial, remove,
            (
                name,
                permissions,
                (FieldsRole::Colour) colour,
                hoist,
                rank,
                (FieldsRole::Icon) icon,
            )
        );

        before
    }

    /// Delete a role
    pub async fn delete(&self, db: &Database, server_id: &str) -> Result<()> {
        EventV1::ServerRoleDelete {
            id: server_id.to_string(),
            role_id: self.id.clone(),
        }
        .p(server_id.to_string())
        .await;

        db.delete_role(server_id, &self.id).await
    }
}

impl SystemMessageChannels {
    pub fn into_channel_ids(self) -> HashSet<String> {
        let mut ids = HashSet::new();

        if let Some(id) = self.user_joined {
            ids.insert(id);
        }

        if let Some(id) = self.user_left {
            ids.insert(id);
        }

        if let Some(id) = self.user_kicked {
            ids.insert(id);
        }

        if let Some(id) = self.user_banned {
            ids.insert(id);
        }

        ids
    }
}

#[cfg(test)]
mod tests {
    use revolt_permissions::{calculate_server_permissions, ChannelPermission};

    use lapin::{ExchangeKind, options::ExchangeDeclareOptions, types::FieldTable};
    use crate::{fixture, util::permissions::DatabasePermissionQuery};

    #[tokio::test]
    async fn permissions() {
        database_test!(|db| async move {
            fixture!(db, "server_with_roles",
                owner user 0
                moderator user 1
                user user 2
                server server 4);

            let mut query = DatabasePermissionQuery::new(&db, &owner).server(&server);
            assert!(calculate_server_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::GrantAllSafe));

            let mut query = DatabasePermissionQuery::new(&db, &moderator).server(&server);
            assert!(calculate_server_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::BanMembers));

            let mut query = DatabasePermissionQuery::new(&db, &user).server(&server);
            assert!(!calculate_server_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::BanMembers));
        });
    }
}
