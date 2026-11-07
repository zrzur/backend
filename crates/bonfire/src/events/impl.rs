use std::collections::{HashMap, HashSet};

use futures::future::join_all;
use redis_kiss::AsyncCommands;
use revolt_database::{
    events::client::{EventV1, ReadyPayloadFields},
    util::{
        age::{mature_access, may_open_mature},
        permissions::DatabasePermissionQuery,
    },
    voice::{get_channel_voice_state, UserVoiceChannel},
    Channel, Database, Member, MemberCompositeKey, Presence, RelationshipStatus, Role,
};
use revolt_models::v0::{self, GatewayIntents};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_presence::filter_online;
use revolt_result::Result;

use super::state::{Cache, State};

/// Cache Manager
impl Cache {
    /// Check whether the current user can view a channel
    pub async fn can_view_channel(&self, db: &Database, channel: &Channel) -> bool {
        self.can_view_channel_in(db, channel, None).await
    }

    /// Whether this user may see a channel's webhooks. Only checked for
    /// webhook events, which are rare.
    pub async fn can_manage_webhooks(&self, db: &Database, channel_id: &str) -> bool {
        let Some(channel) = self.channels.get(channel_id) else {
            return false;
        };
        let mut query = DatabasePermissionQuery::new(db, self.users.get(&self.user_id).unwrap())
            .channel(channel);
        if let Channel::TextChannel { server, .. } = channel {
            if let Some(member) = self.members.get(server) {
                query = query.member(member);
            }
            if let Some(server) = self.servers.get(server) {
                query = query.server(server);
            }
        }
        calculate_channel_permissions(&mut query)
            .await
            .has_channel_permission(ChannelPermission::ManageWebhooks)
    }

    /// As can_view_channel; a thread's parent may be passed when known
    pub async fn can_view_channel_in(
        &self,
        db: &Database,
        channel: &Channel,
        parent: Option<&Channel>,
    ) -> bool {
        // a thread's parent is usually cached already
        let parent = parent.or_else(|| match channel {
            Channel::TextChannel {
                parent: Some(parent),
                ..
            } => self.channels.get(parent),
            _ => None,
        });
        #[allow(deprecated)]
        match &channel {
            Channel::TextChannel { server, .. } => {
                let member = self.members.get(server);
                let server = self.servers.get(server);
                let mut query =
                    DatabasePermissionQuery::new(db, self.users.get(&self.user_id).unwrap())
                        .channel(channel);

                if let Some(parent) = parent {
                    query = query.parent(parent);
                }
                // let mut perms = perms(self.users.get(&self.user_id).unwrap()).channel(channel);

                if let Some(member) = member {
                    query = query.member(member);
                }

                if let Some(server) = server {
                    query = query.server(server);
                }

                calculate_channel_permissions(&mut query)
                    .await
                    .has_channel_permission(ChannelPermission::ViewChannel)
            }
            _ => true,
        }
    }

    /// Filter a given vector of channels to only include the ones we can access
    pub async fn filter_accessible_channels(
        &self,
        db: &Database,
        channels: Vec<Channel>,
    ) -> Vec<Channel> {
        let mut viewable_channels = vec![];
        let mut threads = vec![];
        for channel in channels {
            if matches!(channel, Channel::TextChannel { parent: Some(_), .. }) {
                threads.push(channel);
            } else if self.can_view_channel(db, &channel).await {
                viewable_channels.push(channel);
            }
        }

        // a thread is only visible with its parent, which is in this list
        for thread in threads {
            let Channel::TextChannel {
                parent: Some(parent_id),
                ..
            } = &thread
            else {
                continue;
            };
            let parent = viewable_channels.iter().find(|c| c.id() == parent_id).cloned();
            if let Some(parent) = parent {
                if self.can_view_channel_in(db, &thread, Some(&parent)).await {
                    viewable_channels.push(thread);
                }
            }
        }

        viewable_channels
    }

    /// Whether messages of a channel may reach this connection: mature
    /// channels, and channels of mature Servers, only reach declared adults
    pub fn may_receive(&self, channel: &Channel) -> bool {
        if self.is_bot || self.mature_allowed {
            return true;
        }

        !channel.is_nsfw()
            && !channel
                .server()
                .and_then(|id| self.servers.get(id))
                .is_some_and(|server| server.nsfw)
    }

    /// Whether this connection receives events of an intent (people always do)
    pub fn has_intent(&self, intent: u64) -> bool {
        !self.is_bot || self.intents & intent == intent
    }

    /// Whether a channel is a direct message or group (else a server channel)
    fn is_private_channel(&self, channel: &str) -> bool {
        matches!(
            self.channels.get(channel),
            Some(Channel::DirectMessage { .. } | Channel::Group { .. })
        )
    }

    /// Without the Message Content intent a bot still sees its own messages,
    /// messages in its DMs and messages that mention it
    fn sees_content(&self, author: Option<&str>, mentions: Option<&[String]>, channel: &str) -> bool {
        self.has_intent(GatewayIntents::MESSAGE_CONTENT)
            || author == Some(self.user_id.as_str())
            || matches!(self.channels.get(channel), Some(Channel::DirectMessage { .. }))
            || mentions.is_some_and(|m| m.iter().any(|id| id == &self.user_id))
    }

    /// Without the Presence intent a bot sees nobody online, and no status
    fn limit_user(&self, user: &mut v0::User) {
        if user.id != self.user_id && !self.has_intent(GatewayIntents::PRESENCE) {
            user.online = false;
            user.status = None;
        }
    }

    /// Whether this bot wrote or is mentioned in a stored message
    async fn is_party_to(&self, db: &Database, message: &str) -> bool {
        db.fetch_message(message).await.is_ok_and(|message| {
            message.author == self.user_id
                || message
                    .mentions
                    .is_some_and(|mentions| mentions.contains(&self.user_id))
        })
    }

    /// Check whether we can subscribe to another user
    pub fn can_subscribe_to_user(&self, user_id: &str) -> bool {
        if let Some(user) = self.users.get(&self.user_id) {
            match user.relationship_with(user_id) {
                RelationshipStatus::Friend
                | RelationshipStatus::Incoming
                | RelationshipStatus::Outgoing
                | RelationshipStatus::User => true,
                _ => {
                    let user_id = &user_id.to_string();
                    for channel in self.channels.values() {
                        match channel {
                            Channel::DirectMessage { recipients, .. }
                            | Channel::Group { recipients, .. } => {
                                if recipients.contains(user_id) {
                                    return true;
                                }
                            }
                            _ => {}
                        }
                    }

                    false
                }
            }
        } else {
            false
        }
    }
}

/// State Manager
impl State {
    /// Generate a Ready packet for the current user
    pub async fn generate_ready_payload(
        &mut self,
        db: &Database,
        fields: &ReadyPayloadFields,
    ) -> Result<EventV1> {
        let user = self.clone_user();
        self.cache.is_bot = user.bot.is_some();
        // unknown or failed lookups count as not allowed
        self.cache.mature_allowed = may_open_mature(db, &user).await.unwrap_or(false);

        // Fetch pending policy changes.
        let policy_changes = if user.bot.is_some() || !fields.policy_changes {
            None
        } else {
            Some(
                db.fetch_policy_changes()
                    .await?
                    .into_iter()
                    .filter(|policy| policy.created_time > user.last_acknowledged_policy_change)
                    .map(Into::into)
                    .collect(),
            )
        };

        // Find all relationships to the user.
        let mut user_ids: HashSet<String> = user
            .relations
            .as_ref()
            .map(|arr| arr.iter().map(|x| x.id.to_string()).collect())
            .unwrap_or_default();

        // Fetch all memberships with their corresponding servers.
        let mut members: Vec<Member> = db.fetch_all_memberships(&user.id).await?;

        let server_ids: Vec<String> = members.iter().map(|x| x.id.server.clone()).collect();
        let servers = db.fetch_servers(&server_ids).await?;
        self.cache.servers = servers.iter().cloned().map(|x| (x.id.clone(), x)).collect();

        // Collect channel ids from servers.
        let mut channel_ids = vec![];
        for server in &servers {
            channel_ids.append(&mut server.channels.clone());
        }

        // Fetch DMs and server channels.
        let mut channels = db.find_direct_messages(&user.id).await?;
        channels.append(&mut db.fetch_channels(&channel_ids).await?);

        // Active threads, so their messages reach this session (the same
        // permission check below hides those of channels it can't see)
        channels.append(&mut db.fetch_active_threads(&channel_ids, 1000).await?);

        // Filter server channels by permission.
        let channels = self.cache.filter_accessible_channels(db, channels).await;

        // Append known user IDs from DMs.
        for channel in &channels {
            match channel {
                Channel::DirectMessage { recipients, .. } | Channel::Group { recipients, .. } => {
                    user_ids.extend(&mut recipients.clone().into_iter());
                }
                _ => {}
            }
        }

        let voice_states = if self.wants_voice_states(fields) {
            let mut voice_state_server_members: HashMap<String, HashSet<String>> = HashMap::new();

            // fetch voice states for all the channels we can see
            let mut voice_states = Vec::new();

            for channel in channels.iter().filter(|c| {
                matches!(
                    c,
                    Channel::DirectMessage { .. }
                        | Channel::Group { .. }
                        | Channel::TextChannel { voice: Some(_), .. }
                )
            }) {
                if let Ok(Some(voice_state)) =
                    get_channel_voice_state(&UserVoiceChannel::from_channel(channel)).await
                {
                    if let Some(server) = channel.server() {
                        let set = voice_state_server_members
                            .entry(server.to_string())
                            .or_default();

                        for participant in &voice_state.participants {
                            user_ids.insert(participant.id.clone());
                            set.insert(participant.id.clone());
                        }
                    } else {
                        for participant in &voice_state.participants {
                            user_ids.insert(participant.id.clone());
                        }
                    }

                    voice_states.push(voice_state);
                }
            }

            // Fetch all the members for for the participants who are in a server
            for (server, user_ids) in voice_state_server_members {
                let user_ids = user_ids.into_iter().collect::<Vec<_>>();
                let voice_members = db.fetch_members(&server, &user_ids).await?;

                members.extend(voice_members);
            }

            Some(voice_states)
        } else {
            None
        };

        // Fetch presence data for known users.
        let online_ids = filter_online(&user_ids.iter().cloned().collect::<Vec<String>>()).await;

        // Fetch user data.
        let users = db
            .fetch_users(
                &user_ids
                    .into_iter()
                    .filter(|x| x != &user.id)
                    .collect::<Vec<String>>(),
            )
            .await?;

        self.cache.members = members
            .iter()
            .cloned()
            .map(|x| (x.id.server.clone(), x))
            .collect();

        // Fetch customisations.
        let emojis = if fields.emojis {
            Some(
                db.fetch_emoji_by_parent_ids(
                    &servers
                        .iter()
                        .map(|x| x.id.to_string())
                        .collect::<Vec<String>>(),
                )
                .await?
                .into_iter()
                .map(|emoji| emoji.into())
                .collect(),
            )
        } else {
            None
        };

        // Fetch user settings
        let user_settings = if !fields.user_settings.is_empty() {
            Some(
                db.fetch_user_settings(&user.id, &fields.user_settings)
                    .await?,
            )
        } else {
            None
        };

        // Fetch channel unreads
        let channel_unreads = if fields.channel_unreads {
            Some(
                db.fetch_unreads(&user.id)
                    .await?
                    .into_iter()
                    .map(|unread| unread.into())
                    .collect(),
            )
        } else {
            None
        };

        // Copy data into local state cache.
        self.cache.users = users.iter().cloned().map(|x| (x.id.clone(), x)).collect();
        self.cache
            .users
            .insert(self.cache.user_id.clone(), user.clone());
        self.cache.channels = channels
            .iter()
            .cloned()
            .map(|x| (x.id().to_string(), x))
            .collect();

        // Make all users appear from our perspective.
        let mut users: Vec<v0::User> = join_all(users.into_iter().map(|other_user| async {
            let is_online = online_ids.contains(&other_user.id);
            other_user.into_known(&user, is_online).await
        }))
        .await;

        // Make sure we see our own user correctly.
        users.push(user.into_self(true).await);

        // Set subscription state internally.
        self.reset_state().await;
        self.insert_subscription(self.private_topic.clone()).await;

        for user in &users {
            self.insert_subscription(user.id.clone()).await;
        }

        for server in &servers {
            self.insert_subscription(server.id.clone()).await;

            if self.cache.is_bot && self.cache.has_intent(GatewayIntents::SERVER_MEMBERS) {
                self.insert_subscription(format!("{}u", server.id)).await;
            }
        }

        for channel in &channels {
            self.insert_subscription(channel.id().to_string()).await;
        }

        let mut ready = EventV1::Ready {
            users: if fields.users { Some(users) } else { None },
            servers: if fields.servers {
                Some(join_all(servers.into_iter().map(|server| server.into(db))).await)
            } else {
                None
            },
            channels: if fields.channels {
                Some(channels.into_iter().map(Into::into).collect())
            } else {
                None
            },
            members: if fields.members {
                Some(members.into_iter().map(Into::into).collect())
            } else {
                None
            },
            voice_states,

            emojis,
            user_settings,
            channel_unreads,

            policy_changes,
        };
        self.limit_ready(&mut ready);
        Ok(ready)
    }

    /// Voice states bring their participants into `users` and `members`, so
    /// a bot without the intent must not even look them up
    pub fn wants_voice_states(&self, fields: &ReadyPayloadFields) -> bool {
        fields.voice_states && self.cache.has_intent(GatewayIntents::VOICE_STATES)
    }

    /// Take out of a Ready packet what a bot's intents don't cover. The
    /// connection's own cache keeps everything: permission checks need it.
    pub fn limit_ready(&self, ready: &mut EventV1) {
        let cache = &self.cache;
        if !cache.is_bot {
            return;
        }
        let EventV1::Ready {
            users,
            servers,
            channels,
            members,
            emojis,
            voice_states,
            ..
        } = ready
        else {
            return;
        };

        if !cache.has_intent(GatewayIntents::SERVERS) {
            *servers = None;
            *emojis = None;
            if let Some(channels) = channels {
                channels.retain(|channel| {
                    matches!(
                        channel,
                        v0::Channel::DirectMessage { .. } | v0::Channel::Group { .. }
                    )
                });
            }
        }
        if !cache.has_intent(GatewayIntents::VOICE_STATES) {
            *voice_states = None;
        }
        if !cache.has_intent(GatewayIntents::SERVER_MEMBERS) {
            if let Some(members) = members {
                members.retain(|member| member.id.user == cache.user_id);
            }
        }
        if let Some(users) = users {
            for user in users {
                cache.limit_user(user);
            }
        }
    }

    /// Re-determine the currently accessible server channels
    pub async fn recalculate_server(&mut self, db: &Database, id: &str, event: &mut EventV1) {
        if let Some(server) = self.cache.servers.get(id) {
            let mut channel_ids = HashSet::new();
            let mut added_channels = vec![];
            let mut removed_channels = vec![];

            let id = &id.to_string();
            for (channel_id, channel) in &self.cache.channels {
                if channel.server() == Some(id) {
                    channel_ids.insert(channel_id.clone());

                    if self.cache.can_view_channel(db, channel).await {
                        added_channels.push(channel_id.clone());
                    } else {
                        removed_channels.push(channel_id.clone());
                    }
                }
            }

            let known_ids = server.channels.iter().cloned().collect::<HashSet<String>>();

            let mut bulk_events = vec![];

            for id in added_channels {
                self.insert_subscription(id).await;
            }

            for id in removed_channels {
                self.remove_subscription(&id).await;
                self.cache.channels.remove(&id);

                bulk_events.push(EventV1::ChannelDelete { id });
            }

            // * NOTE: currently all channels should be cached
            // * provided that a server was loaded from payload
            let unknowns = known_ids
                .difference(&channel_ids)
                .cloned()
                .collect::<Vec<String>>();

            if !unknowns.is_empty() {
                if let Ok(channels) = db.fetch_channels(&unknowns).await {
                    let viewable_channels =
                        self.cache.filter_accessible_channels(db, channels).await;

                    for channel in viewable_channels {
                        self.cache
                            .channels
                            .insert(channel.id().to_string(), channel.clone());

                        self.insert_subscription(channel.id().to_string()).await;
                        bulk_events.push(EventV1::ChannelCreate(channel.into()));
                    }
                }
            }

            if !bulk_events.is_empty() {
                let mut new_event = EventV1::Bulk { v: bulk_events };
                std::mem::swap(&mut new_event, event);

                if let EventV1::Bulk { v } = event {
                    v.push(new_event);
                }
            }
        }
    }

    /// Push presence change to the user and all associated server topics
    pub async fn broadcast_presence_change(&self, target: bool) {
        let config = revolt_config::config().await;
        if config.disable_events_dont_use {
            return;
        }

        if if let Some(status) = &self.cache.users.get(&self.cache.user_id).unwrap().status {
            status.presence != Some(Presence::Invisible)
        } else {
            true
        } {
            let event = EventV1::UserUpdate {
                id: self.cache.user_id.clone(),
                data: v0::PartialUser {
                    online: Some(target),
                    ..Default::default()
                },
                clear: vec![],
                event_id: Some(ulid::Ulid::new().to_string()),
            };

            for server in self.cache.servers.keys() {
                event.clone().p(server.clone()).await;
            }

            event.p(self.cache.user_id.clone()).await;
        }
    }

    /// Handle an incoming event for protocol version 1
    ///
    /// `topic` is the pub/sub topic the event arrived on.
    pub async fn handle_incoming_event_v1(
        &mut self,
        db: &Database,
        event: &mut EventV1,
        topic: &str,
    ) -> bool {
        /* Superseded by private topics.
          if match event {
            EventV1::UserRelationship { id, .. }
            | EventV1::UserSettingsUpdate { id, .. }
            | EventV1::ChannelAck { id, .. } => id != &self.cache.user_id,
            EventV1::ServerCreate { server, .. } => server.owner != self.cache.user_id,
            EventV1::ChannelCreate(channel) => match channel {
                Channel::SavedMessages { user, .. } => user != &self.cache.user_id,
                Channel::DirectMessage { recipients, .. } | Channel::Group { recipients, .. } => {
                    !recipients.contains(&self.cache.user_id)
                }
                _ => false,
            },
            _ => false,
        } {
            return false;
        }*/

        // A change of age group, or a channel or Server being marked (or
        // unmarked) as mature, changes which channels may be followed
        // the state changes below forget a channel that is deleted
        let private_channel = match event {
            EventV1::ChannelUpdate { id, .. } | EventV1::ChannelDelete { id } => {
                self.cache.is_private_channel(id)
            }
            _ => false,
        };

        let refresh_mature = match event {
            EventV1::AccountAgeUpdate { .. } => true,
            EventV1::ChannelUpdate { data, .. } => {
                data.nsfw.is_some() && !self.cache.mature_allowed && !self.cache.is_bot
            }
            EventV1::ServerUpdate { data, .. } => {
                data.nsfw.is_some() && !self.cache.mature_allowed && !self.cache.is_bot
            }
            _ => false,
        };
        if let EventV1::AccountAgeUpdate { band } = event {
            if !self.cache.is_bot {
                let setting = revolt_config::config()
                    .await
                    .features
                    .age_safety
                    .mature_content;
                self.cache.mature_allowed =
                    mature_access(*band, setting) == v0::MatureAccess::Allowed;
            }
        }

        if self.cache.is_bot && !self.allowed_by_intents(db, event).await {
            return false;
        }

        // Webhooks are for those who manage them, not all who read the channel
        if let EventV1::WebhookCreate(v0::Webhook { channel_id, .. })
        | EventV1::WebhookUpdate { channel_id, .. }
        | EventV1::WebhookDelete { channel_id, .. } = event
        {
            if !self.cache.can_manage_webhooks(db, channel_id).await {
                return false;
            }
        }

        // An event may trigger recalculation of an entire server's permission.
        // Keep track of whether we need to do anything.
        let mut queue_server = None;

        // It may also need to sub or unsub a single value.
        let mut queue_add = None;
        let mut queue_remove = None;

        match event {
            EventV1::ChannelCreate(channel) => {
                // Server channels are announced to the whole server, so only
                // follow (and forward) the ones this user can see, as on Ready.
                let channel: Channel = channel.clone().into();
                if !self.cache.can_view_channel(db, &channel).await {
                    return false;
                }

                // cache first, so the subscription sees whether it is mature
                let id = channel.id().to_string();
                self.cache.channels.insert(id.clone(), channel);
                self.insert_subscription(id).await;
            }
            EventV1::ChannelUpdate {
                id, data, clear, ..
            } => {
                let could_view: bool = if let Some(channel) = self.cache.channels.get(id) {
                    self.cache.can_view_channel(db, channel).await
                } else {
                    false
                };

                if let Some(channel) = self.cache.channels.get_mut(id) {
                    for field in clear {
                        channel.remove_field(&field.clone().into());
                    }

                    channel.apply_options(data.clone().into());
                }

                if !self.cache.channels.contains_key(id) {
                    if let Ok(channel) = db.fetch_channel(id).await {
                        self.cache.channels.insert(id.clone(), channel);
                    }
                }

                let channel_id = id.clone();
                let mut hidden = false;
                if let Some(channel) = self.cache.channels.get(id) {
                    let can_view = self.cache.can_view_channel(db, channel).await;
                    if could_view != can_view {
                        if can_view {
                            queue_add = Some(id.clone());
                            *event = EventV1::ChannelCreate(channel.clone().into());
                        } else {
                            queue_remove = Some(id.clone());
                            *event = EventV1::ChannelDelete { id: id.clone() };
                        }
                    } else if !can_view {
                        hidden = true;
                    } else if topic != channel_id {
                        // Permission changes are also sent to the server topic so
                        // members without access can pick up a grant; everyone who
                        // can see the channel gets the channel topic copy instead.
                        return false;
                    }
                }

                if hidden {
                    // Not visible before or after: keep it out of the cache, as
                    // on Ready, and say nothing about it.
                    self.cache.channels.remove(&channel_id);
                    return false;
                }
            }
            EventV1::ChannelDelete { id } => {
                self.remove_subscription(id).await;
                self.cache.channels.remove(id);
            }
            EventV1::ChannelGroupJoin { user, .. } => {
                self.insert_subscription(user.clone()).await;
            }
            EventV1::ChannelGroupLeave { id, user, .. } => {
                if user == &self.cache.user_id {
                    self.remove_subscription(id).await;
                } else if !self.cache.can_subscribe_to_user(user) {
                    self.remove_subscription(user).await;
                }
            }

            EventV1::ServerCreate {
                id,
                server,
                channels,
                emojis: _,
                voice_states: _,
            } => {
                self.insert_subscription(id.clone()).await;

                if self.cache.is_bot && self.cache.has_intent(GatewayIntents::SERVER_MEMBERS) {
                    self.insert_subscription(format!("{}u", id)).await;
                }

                self.cache.servers.insert(id.clone(), server.clone().into());
                let member = Member {
                    id: MemberCompositeKey {
                        server: server.id.clone(),
                        user: self.cache.user_id.clone(),
                    },
                    ..Default::default()
                };
                self.cache.members.insert(id.clone(), member);

                for channel in channels {
                    self.cache
                        .channels
                        .insert(channel.id().to_string(), channel.clone().into());
                }

                queue_server = Some(id.clone());
            }
            EventV1::ServerUpdate {
                id, data, clear, ..
            } => {
                if let Some(server) = self.cache.servers.get_mut(id) {
                    for field in clear {
                        server.remove_field(&field.clone().into());
                    }

                    server.apply_options(data.clone().into());
                }

                // category permissions decide what synced channels show
                if data.default_permissions.is_some() || data.categories.is_some() {
                    queue_server = Some(id.clone());
                }
            }
            EventV1::ServerMemberJoin { .. } => {
                // We will always receive ServerCreate when joining a new server.
            }
            EventV1::ServerMemberLeave { id, user, .. } => {
                if user == &self.cache.user_id {
                    self.remove_subscription(id).await;
                    self.remove_active_server(id).await;

                    if let Some(server) = self.cache.servers.remove(id) {
                        for channel in &server.channels {
                            self.remove_subscription(channel).await;
                            self.cache.channels.remove(channel);
                        }
                    }
                    self.cache.members.remove(id);
                }
            }
            EventV1::ServerDelete { id } => {
                self.remove_subscription(id).await;

                if let Some(server) = self.cache.servers.remove(id) {
                    for channel in &server.channels {
                        self.remove_subscription(channel).await;
                        self.cache.channels.remove(channel);
                    }
                }
                self.cache.members.remove(id);
            }
            EventV1::ServerMemberUpdate { id, data, clear } => {
                if id.user == self.cache.user_id {
                    if let Some(member) = self.cache.members.get_mut(&id.server) {
                        for field in &clear.clone() {
                            member.remove_field(&field.clone().into());
                        }

                        member.apply_options(data.clone().into());
                    }

                    if data.roles.is_some() || clear.contains(&v0::FieldsMember::Roles) {
                        queue_server = Some(id.server.clone());
                    }
                }
            }
            EventV1::ServerRoleUpdate {
                id,
                role_id,
                data,
                clear,
                ..
            } => {
                if let Some(server) = self.cache.servers.get_mut(id) {
                    if let Some(role) = server.roles.get_mut(role_id) {
                        for field in &clear.clone() {
                            role.remove_field(&field.clone().into());
                        }

                        role.apply_options(data.clone().into());
                    } else if let Some(name) = &data.name {
                        // A role created after Ready: Role::create sends the
                        // whole role, so follow it or permission checks that
                        // depend on it will ignore it until reconnect.
                        let mut role = Role {
                            id: role_id.clone(),
                            name: name.clone(),
                            permissions: Default::default(),
                            colour: None,
                            hoist: false,
                            rank: 0,
                            icon: None,
                            owner: None,
                        };

                        role.apply_options(data.clone().into());
                        server.roles.insert(role_id.clone(), role);
                    }
                }

                if data.rank.is_some() || data.permissions.is_some() {
                    if let Some(member) = self.cache.members.get(id) {
                        if member.roles.contains(role_id) {
                            queue_server = Some(id.clone());
                        }
                    }
                }
            }
            EventV1::ServerRoleDelete { id, role_id } => {
                if let Some(server) = self.cache.servers.get_mut(id) {
                    server.roles.remove(role_id);
                }

                if let Some(member) = self.cache.members.get(id) {
                    if member.roles.contains(role_id) {
                        queue_server = Some(id.clone());
                    }
                }
            }

            EventV1::UserUpdate { event_id, .. } => {
                if let Some(id) = event_id {
                    if self.cache.seen_events.contains(id) {
                        return false;
                    }

                    self.cache.seen_events.put(id.to_string(), ());
                }

                *event_id = None;
            }
            EventV1::UserRelationship { id, user, .. } => {
                self.cache.users.insert(id.clone(), user.clone().into());

                if self.cache.can_subscribe_to_user(id) {
                    self.insert_subscription(id.clone()).await;
                } else {
                    self.remove_subscription(id).await;
                }
            }

            EventV1::Message(message) => {
                // Second line of defence behind the subscriptions
                if let Some(channel) = self.cache.channels.get(&message.channel) {
                    if !self.cache.may_receive(channel) {
                        return false;
                    }
                }

                // Since Message events are fanned out to many clients,
                // we must reconstruct the relationship value at this end.
                if let Some(user) = &mut message.user {
                    user.relationship = self
                        .cache
                        .users
                        .get(&self.cache.user_id)
                        .expect("missing self?")
                        .relationship_with(&message.author)
                        .into();
                }
            }

            _ => {}
        }

        // Calculate server permissions if requested.
        if let Some(server_id) = queue_server {
            self.recalculate_server(db, &server_id, event).await;
        }

        // Sub / unsub accordingly.
        if let Some(id) = queue_add {
            self.insert_subscription(id).await;
        }

        if let Some(id) = queue_remove {
            self.remove_subscription(&id).await;
        }

        if refresh_mature {
            self.refresh_mature_subscriptions(db).await;
        }

        !self.cache.is_bot || self.keep_after_state(event, private_channel)
    }

    /// The events a connection has to follow itself but a bot only receives
    /// with the Servers intent. Checked after the state changes, so its
    /// cache and subscriptions stay right either way.
    fn keep_after_state(&self, event: &mut EventV1, private_channel: bool) -> bool {
        if self.cache.has_intent(GatewayIntents::SERVERS) {
            if !self.cache.has_intent(GatewayIntents::VOICE_STATES) {
                if let EventV1::ServerCreate { voice_states, .. } = event {
                    voice_states.clear();
                }
            }
            return true;
        }

        let covered = |event: &EventV1| {
            matches!(
                event,
                EventV1::ServerCreate { .. }
                    | EventV1::ServerUpdate { .. }
                    | EventV1::ServerDelete { .. }
                    | EventV1::ServerRoleUpdate { .. }
                    | EventV1::ServerRoleDelete { .. }
                    | EventV1::ServerRoleRanksUpdate { .. }
                    | EventV1::EmojiCreate(_)
                    | EventV1::EmojiUpdate { .. }
                    | EventV1::EmojiDelete { .. }
                    | EventV1::WebhookCreate(_)
                    | EventV1::WebhookUpdate { .. }
                    | EventV1::WebhookDelete { .. }
            ) || match event {
                // direct messages and groups are not a server's
                EventV1::ChannelCreate(channel) => matches!(channel, v0::Channel::TextChannel { .. }),
                EventV1::ChannelUpdate { .. } | EventV1::ChannelDelete { .. } => !private_channel,
                _ => false,
            }
        };

        if let EventV1::Bulk { v } = event {
            v.retain(|event| !covered(event));
            return !v.is_empty();
        }
        !covered(event)
    }

    /// Drop or trim an event a bot's intents don't cover. Only events that
    /// don't change this connection's state are dropped here.
    async fn allowed_by_intents(&self, db: &Database, event: &mut EventV1) -> bool {
        let cache = &self.cache;
        let me = cache.user_id.as_str();
        let messages = |channel: &str| {
            cache.has_intent(if cache.is_private_channel(channel) {
                GatewayIntents::DIRECT_MESSAGES
            } else {
                GatewayIntents::SERVER_MESSAGES
            })
        };

        match event {
            EventV1::Message(message) => {
                if !messages(&message.channel) {
                    return false;
                }
                if !cache.sees_content(Some(&message.author), message.mentions.as_deref(), &message.channel) {
                    message.hide_content();
                }
                if let Some(user) = &mut message.user {
                    cache.limit_user(user);
                }
            }
            EventV1::MessageUpdate { id, channel, data, .. } => {
                if !messages(channel) {
                    return false;
                }
                if !cache.sees_content(None, data.mentions.as_deref(), channel)
                    && !cache.is_party_to(db, id).await
                {
                    data.hide_content();
                }
                if let Some(user) = &mut data.user {
                    cache.limit_user(user);
                }
            }
            EventV1::MessageAppend { id, channel, append } => {
                if !messages(channel) {
                    return false;
                }
                if !cache.sees_content(None, None, channel) && !cache.is_party_to(db, id).await {
                    append.hide_content();
                }
            }
            EventV1::MessageDelete { channel, .. }
            | EventV1::BulkMessageDelete { channel, .. }
            | EventV1::ChannelStartTyping { id: channel, .. }
            | EventV1::ChannelStopTyping { id: channel, .. } => return messages(channel),
            EventV1::MessageReact { .. }
            | EventV1::MessageUnreact { .. }
            | EventV1::MessageRemoveReaction { .. } => {
                return cache.has_intent(GatewayIntents::REACTIONS)
            }
            EventV1::ServerMemberJoin { member, .. } => {
                return member.id.user == me || cache.has_intent(GatewayIntents::SERVER_MEMBERS)
            }
            EventV1::ServerMemberLeave { user, .. } => {
                return user == me || cache.has_intent(GatewayIntents::SERVER_MEMBERS)
            }
            EventV1::ServerMemberUpdate { id, .. } => {
                return id.user == me || cache.has_intent(GatewayIntents::SERVER_MEMBERS)
            }
            EventV1::UserUpdate { id, data, clear, .. } => {
                if id != me && !cache.has_intent(GatewayIntents::PRESENCE) {
                    data.online = None;
                    data.status = None;
                    clear.retain(|field| {
                        !matches!(field, v0::FieldsUser::StatusText | v0::FieldsUser::StatusPresence)
                    });
                    // nothing left but presence
                    if *data == v0::PartialUser::default() && clear.is_empty() {
                        return false;
                    }
                }
            }
            EventV1::UserRelationship { user, .. } => cache.limit_user(user),
            EventV1::VoiceChannelJoin { .. }
            | EventV1::VoiceChannelLeave { .. }
            | EventV1::VoiceChannelMove { .. }
            | EventV1::VoiceCallUpdate { .. }
            | EventV1::UserVoiceStateUpdate { .. } => {
                return cache.has_intent(GatewayIntents::VOICE_STATES)
            }
            EventV1::InteractionCreate { interaction } => {
                if !cache.has_intent(GatewayIntents::PRESENCE) {
                    interaction.hide_presence();
                }
                return cache.has_intent(GatewayIntents::INTERACTIONS);
            }
            _ => {}
        }
        true
    }

    /// Follow or drop every cached channel by the current age rules; a
    /// channel is only followed again if its permissions still allow it
    pub async fn refresh_mature_subscriptions(&mut self, db: &Database) {
        let mut changes = vec![];
        for (id, channel) in &self.cache.channels {
            let allowed =
                self.cache.may_receive(channel) && self.cache.can_view_channel(db, channel).await;
            changes.push((id.clone(), allowed));
        }

        for (id, allowed) in changes {
            if allowed {
                self.insert_subscription(id).await;
            } else {
                self.remove_subscription(&id).await;
            }
        }
    }
}
