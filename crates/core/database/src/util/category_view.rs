//! Which categories of a server a person is told about.
//!
//! A category with no permissions of its own is known to every member, as it
//! always was. A category with overrides is known to:
//! - whoever can manage channels or permissions in the server (owner and
//!   administrators included), so nothing gets lost while managing;
//! - whoever can view the category itself (its default and role overrides
//!   leave them View Channel);
//! - whoever can view at least one channel inside it (a channel with its own
//!   permissions): the title is that channel's heading.
//!
//! Everyone else does not get the category at all: no title, no note, no
//! channel list.

use std::collections::HashMap;

use revolt_permissions::{
    calculate_channel_permissions, calculate_server_permissions, ChannelPermission,
};

use crate::{
    util::permissions::DatabasePermissionQuery, Category, Channel, Database, Member, Server, User,
};

impl Category {
    /// Whether this category changes anybody's permissions
    pub fn has_overrides(&self) -> bool {
        self.default_permissions.is_some() || !self.role_permissions.is_empty()
    }

    /// A stand-in channel carrying this category's overrides, to ask the
    /// permission calculator what a person gets in the category itself
    fn probe(&self, server: &str) -> Channel {
        Channel::TextChannel {
            id: format!("category:{}", self.id),
            server: server.to_string(),
            name: String::new(),
            description: None,
            icon: None,
            last_message_id: None,
            default_permissions: self.default_permissions,
            role_permissions: self.role_permissions.clone(),
            member_permissions: HashMap::new(),
            nsfw: false,
            voice: None,
            slowmode: None,
            announcement: false,
            permissions_synced: false,
            parent: None,
            thread: None,
            forum: None,
            ticket: None,
        }
    }
}

/// The categories of `server` that `viewer` is told about, in order.
/// `member` saves a lookup when it is already in hand.
pub async fn categories_for(
    db: &Database,
    viewer: &User,
    server: &Server,
    member: Option<&Member>,
) -> Option<Vec<Category>> {
    let categories = server.categories.as_ref()?;
    if !categories.iter().any(Category::has_overrides) {
        return Some(categories.clone());
    }

    let base = {
        let query = DatabasePermissionQuery::new(db, viewer).server(server);
        match member {
            Some(member) => query.member(member),
            None => query,
        }
    };

    let in_server = calculate_server_permissions(&mut base.clone()).await;
    if in_server.has_channel_permission(ChannelPermission::ManageChannel)
        || in_server.has_channel_permission(ChannelPermission::ManagePermissions)
    {
        return Some(categories.clone());
    }

    // channels are looked up once, and only for categories the viewer
    // cannot see as such
    let mut inside: Option<HashMap<String, Channel>> = None;
    let mut visible = Vec::with_capacity(categories.len());
    for category in categories {
        if !category.has_overrides() {
            visible.push(category.clone());
            continue;
        }

        let probe = category.probe(&server.id);
        if calculate_channel_permissions(&mut base.clone().channel(&probe))
            .await
            .has_channel_permission(ChannelPermission::ViewChannel)
        {
            visible.push(category.clone());
            continue;
        }

        if category.channels.is_empty() {
            continue;
        }
        if inside.is_none() {
            let ids: Vec<String> = categories
                .iter()
                .filter(|category| category.has_overrides())
                .flat_map(|category| category.channels.iter().cloned())
                .collect();
            inside = Some(
                db.fetch_channels(&ids)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|channel| (channel.id().to_string(), channel))
                    .collect(),
            );
        }
        let channels = inside.as_ref().expect("filled above");
        for id in &category.channels {
            let Some(channel) = channels.get(id) else {
                continue;
            };
            if calculate_channel_permissions(&mut base.clone().channel(channel))
                .await
                .has_channel_permission(ChannelPermission::ViewChannel)
            {
                visible.push(category.clone());
                break;
            }
        }
    }

    Some(visible)
}

impl Server {
    /// This server as `viewer` gets it: categories hidden from them are left
    /// out (see [`categories_for`])
    pub async fn into_for(self, db: &Database, viewer: &User) -> revolt_models::v0::Server {
        let categories = categories_for(db, viewer, &self, None).await;
        let mut server = self.into(db).await;
        server.categories =
            categories.map(|categories| categories.into_iter().map(Into::into).collect());
        server
    }
}
