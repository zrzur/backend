use iso8601_timestamp::Timestamp;
use revolt_models::v0::*;
use revolt_permissions::{calculate_user_permissions, UserPermission};

use crate::{util::permissions::DatabasePermissionQuery, Database};

impl crate::Bot {
    pub fn into_public_bot(self, user: crate::User) -> PublicBot {
        #[cfg(debug_assertions)]
        assert_eq!(self.id, user.id);

        let verified = self.is_verified();
        PublicBot {
            verified,
            creator: None,
            server_count: None,
            id: self.id,
            username: user.username,
            avatar: user.avatar.map(|x| x.id).unwrap_or_default(),
            description: user
                .profile
                .as_ref()
                .and_then(|profile| profile.public_content())
                .map(str::to_string)
                .unwrap_or_default(),
            privacy_policy_url: self.privacy_policy_url,
            terms_of_service_url: self.terms_of_service_url,
            default_permissions: self.default_permissions,
        }
    }
}

impl From<crate::Bot> for Bot {
    fn from(value: crate::Bot) -> Self {
        Bot {
            id: value.id,
            owner_id: value.owner,
            token: value.token,
            public: value.public,
            default_permissions: value.default_permissions,
            analytics: value.analytics,
            discoverable: value.discoverable,
            interactions_url: value.interactions_url,
            terms_of_service_url: value.terms_of_service_url,
            privacy_policy_url: value.privacy_policy_url,
            flags: value.flags.unwrap_or_default() as u32,
            privileged_intents: value.privileged_intents,
            approved_intents: value.approved_intents,
            interactions_endpoint: value.interactions_endpoint,
            interactions_secret_version: value.interactions_secret_version,
            show_creator: value.show_creator,
        }
    }
}

impl From<FieldsBot> for crate::FieldsBot {
    fn from(value: FieldsBot) -> Self {
        match value {
            FieldsBot::InteractionsURL => crate::FieldsBot::InteractionsURL,
            FieldsBot::InteractionsEndpoint => crate::FieldsBot::InteractionsEndpoint,
            FieldsBot::Token => crate::FieldsBot::Token,
        }
    }
}

impl From<crate::FieldsBot> for FieldsBot {
    fn from(value: crate::FieldsBot) -> Self {
        match value {
            crate::FieldsBot::InteractionsURL => FieldsBot::InteractionsURL,
            crate::FieldsBot::InteractionsEndpoint => FieldsBot::InteractionsEndpoint,
            crate::FieldsBot::Token => FieldsBot::Token,
        }
    }
}

impl From<crate::Invite> for Invite {
    fn from(value: crate::Invite) -> Self {
        match value {
            crate::Invite::Group {
                code,
                creator,
                channel,
                max_uses,
                uses,
                expires
            } => Invite::Group {
                code,
                creator,
                channel,
                max_uses,
                uses,
                expires,

            },
            crate::Invite::Server {
                code,
                server,
                creator,
                channel,
                max_uses,
                uses,
                expires,
                ..
            } => Invite::Server {
                code,
                server,
                creator,
                channel,
                max_uses,
                uses,
                expires,
            },
        }
    }
}

impl From<crate::ChannelUnread> for ChannelUnread {
    fn from(value: crate::ChannelUnread) -> Self {
        ChannelUnread {
            id: value.id.into(),
            last_id: value.last_id,
            mentions: value.mentions.unwrap_or_default(),
        }
    }
}

impl From<crate::ChannelCompositeKey> for ChannelCompositeKey {
    fn from(value: crate::ChannelCompositeKey) -> Self {
        ChannelCompositeKey {
            channel: value.channel,
            user: value.user,
        }
    }
}

impl From<crate::DiscoverBan> for DiscoverBan {
    fn from(value: crate::DiscoverBan) -> Self {
        DiscoverBan {
            id: value.id,
            item_type: value.item_type.into(),
            item_id: value.item_id,
        }
    }
}

impl From<crate::DiscoverRequest> for DiscoverRequest {
    fn from(value: crate::DiscoverRequest) -> Self {
        DiscoverRequest {
            request_type: value.request_type.into(),
            request_id: value.request_id,
            status: value.status.into(),
            tags: value.tags,
            category: value.category,
            language: value.language,
            featured: value.featured,
        }
    }
}

impl From<crate::DiscoverRequestType> for DiscoverRequestType {
    fn from(value: crate::DiscoverRequestType) -> Self {
        match value {
            crate::DiscoverRequestType::Bot => DiscoverRequestType::Bot,
            crate::DiscoverRequestType::Server => DiscoverRequestType::Server,
        }
    }
}

impl From<crate::DiscoverRequestStatus> for DiscoverRequestStatus {
    fn from(value: crate::DiscoverRequestStatus) -> Self {
        match value {
            crate::DiscoverRequestStatus::Removed(s) => DiscoverRequestStatus::Removed(s),
            crate::DiscoverRequestStatus::Approved(s) => DiscoverRequestStatus::Approved(s),
            crate::DiscoverRequestStatus::Denied(s) => DiscoverRequestStatus::Denied(s),
            crate::DiscoverRequestStatus::Pending => DiscoverRequestStatus::Pending,
            crate::DiscoverRequestStatus::UnderReview => DiscoverRequestStatus::UnderReview,
        }
    }
}

impl From<crate::Webhook> for Webhook {
    fn from(value: crate::Webhook) -> Self {
        Webhook {
            id: value.id,
            name: value.name,
            avatar: value.avatar.map(|file| file.into()),
            creator_id: value.creator_id,
            channel_id: value.channel_id,
            // Never from the database, whatever is stored there: the route
            // that makes a token sets it on the response
            token: None,
            url: None,
            permissions: value.permissions,
            last_used: value.last_used,
        }
    }
}

impl From<crate::PartialWebhook> for PartialWebhook {
    fn from(value: crate::PartialWebhook) -> Self {
        PartialWebhook {
            id: value.id,
            name: value.name,
            avatar: value.avatar.map(|file| file.into()),
            creator_id: value.creator_id,
            channel_id: value.channel_id,
            token: None,
            url: None,
            permissions: value.permissions,
            last_used: value.last_used,
        }
    }
}

impl From<FieldsWebhook> for crate::FieldsWebhook {
    fn from(_value: FieldsWebhook) -> Self {
        Self::Avatar
    }
}

impl From<crate::FieldsWebhook> for FieldsWebhook {
    fn from(_value: crate::FieldsWebhook) -> Self {
        Self::Avatar
    }
}

impl From<crate::Channel> for Channel {
    #[allow(deprecated)]
    fn from(value: crate::Channel) -> Self {
        match value {
            crate::Channel::SavedMessages { id, user } => Channel::SavedMessages { id, user },
            crate::Channel::DirectMessage {
                id,
                active,
                recipients,
                last_message_id,
            } => Channel::DirectMessage {
                id,
                active,
                recipients,
                last_message_id,
            },
            crate::Channel::Group {
                id,
                name,
                owner,
                description,
                recipients,
                icon,
                last_message_id,
                permissions,
                nsfw,
            } => Channel::Group {
                id,
                name,
                owner,
                description,
                recipients,
                icon: icon.map(|file| file.into()),
                last_message_id,
                permissions,
                nsfw,
            },
            crate::Channel::TextChannel {
                id,
                server,
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
                permissions_synced,
                parent,
                thread,
                forum,
            } => Channel::TextChannel {
                id,
                server,
                name,
                description,
                icon: icon.map(|file| file.into()),
                last_message_id,
                default_permissions,
                role_permissions,
                member_permissions,
                nsfw,
                voice: voice.map(|voice| voice.into()),
                slowmode,
                announcement,
                permissions_synced,
                parent,
                thread: thread.map(Into::into),
                forum: forum.map(Into::into),
            },
        }
    }
}

impl From<Channel> for crate::Channel {
    #[allow(deprecated)]
    fn from(value: Channel) -> crate::Channel {
        match value {
            Channel::SavedMessages { id, user } => crate::Channel::SavedMessages { id, user },
            Channel::DirectMessage {
                id,
                active,
                recipients,
                last_message_id,
            } => crate::Channel::DirectMessage {
                id,
                active,
                recipients,
                last_message_id,
            },
            Channel::Group {
                id,
                name,
                owner,
                description,
                recipients,
                icon,
                last_message_id,
                permissions,
                nsfw,
            } => crate::Channel::Group {
                id,
                name,
                owner,
                description,
                recipients,
                icon: icon.map(|file| file.into()),
                last_message_id,
                permissions,
                nsfw,
            },
            Channel::TextChannel {
                id,
                server,
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
                permissions_synced,
                parent,
                thread,
                forum,
            } => crate::Channel::TextChannel {
                id,
                server,
                name,
                description,
                icon: icon.map(|file| file.into()),
                last_message_id,
                default_permissions,
                role_permissions,
                member_permissions,
                nsfw,
                voice: voice.map(|voice| voice.into()),
                slowmode,
                announcement,
                permissions_synced,
                parent,
                thread: thread.map(Into::into),
                forum: forum.map(Into::into),
            },
        }
    }
}

impl From<crate::PartialChannel> for PartialChannel {
    fn from(value: crate::PartialChannel) -> Self {
        PartialChannel {
            name: value.name,
            owner: value.owner,
            description: value.description,
            icon: value.icon.map(|file| file.into()),
            nsfw: value.nsfw,
            active: value.active,
            permissions: value.permissions,
            role_permissions: value.role_permissions,
            member_permissions: value.member_permissions,
            default_permissions: value.default_permissions,
            last_message_id: value.last_message_id,
            voice: value.voice.map(|voice| voice.into()),
            slowmode: value.slowmode,
            announcement: value.announcement,
            permissions_synced: value.permissions_synced,
            thread: value.thread.map(Into::into),
            forum: value.forum.map(Into::into),
        }
    }
}

impl From<PartialChannel> for crate::PartialChannel {
    fn from(value: PartialChannel) -> crate::PartialChannel {
        crate::PartialChannel {
            name: value.name,
            owner: value.owner,
            description: value.description,
            icon: value.icon.map(|file| file.into()),
            nsfw: value.nsfw,
            active: value.active,
            permissions: value.permissions,
            role_permissions: value.role_permissions,
            member_permissions: value.member_permissions,
            default_permissions: value.default_permissions,
            last_message_id: value.last_message_id,
            voice: value.voice.map(|voice| voice.into()),
            slowmode: value.slowmode,
            announcement: value.announcement,
            permissions_synced: value.permissions_synced,
            thread: value.thread.map(Into::into),
            forum: value.forum.map(Into::into),
        }
    }
}

impl From<FieldsChannel> for crate::FieldsChannel {
    fn from(value: FieldsChannel) -> Self {
        match value {
            FieldsChannel::Description => crate::FieldsChannel::Description,
            FieldsChannel::Icon => crate::FieldsChannel::Icon,
            FieldsChannel::DefaultPermissions => crate::FieldsChannel::DefaultPermissions,
            FieldsChannel::Voice => crate::FieldsChannel::Voice,
            FieldsChannel::Slowmode => crate::FieldsChannel::Slowmode,
        }
    }
}

impl From<crate::FieldsChannel> for FieldsChannel {
    fn from(value: crate::FieldsChannel) -> Self {
        match value {
            crate::FieldsChannel::Description => FieldsChannel::Description,
            crate::FieldsChannel::Icon => FieldsChannel::Icon,
            crate::FieldsChannel::DefaultPermissions => FieldsChannel::DefaultPermissions,
            crate::FieldsChannel::Voice => FieldsChannel::Voice,
            crate::FieldsChannel::Slowmode => FieldsChannel::Slowmode,
        }
    }
}

impl From<crate::Emoji> for Emoji {
    fn from(value: crate::Emoji) -> Self {
        Emoji {
            id: value.id,
            parent: value.parent.into(),
            creator_id: value.creator_id,
            name: value.name,
            animated: value.animated,
            nsfw: value.nsfw,
        }
    }
}

impl From<crate::EmojiParent> for EmojiParent {
    fn from(value: crate::EmojiParent) -> Self {
        match value {
            crate::EmojiParent::Detached => EmojiParent::Detached,
            crate::EmojiParent::Server { id } => EmojiParent::Server { id },
        }
    }
}

impl From<EmojiParent> for crate::EmojiParent {
    fn from(value: EmojiParent) -> Self {
        match value {
            EmojiParent::Detached => crate::EmojiParent::Detached,
            EmojiParent::Server { id } => crate::EmojiParent::Server { id },
        }
    }
}

impl From<crate::File> for File {
    fn from(value: crate::File) -> Self {
        File {
            id: value.id,
            tag: value.tag,
            filename: value.filename,
            metadata: value.metadata.into(),
            content_type: value.content_type,
            size: value.size,
            deleted: value.deleted,
            reported: value.reported,
            message_id: value.message_id,
            user_id: value.user_id,
            server_id: value.server_id,
            object_id: value.object_id,
        }
    }
}

impl From<File> for crate::File {
    fn from(value: File) -> crate::File {
        crate::File {
            id: value.id,
            tag: value.tag,
            filename: value.filename,
            metadata: value.metadata.into(),
            content_type: value.content_type,
            size: value.size,
            deleted: value.deleted,
            reported: value.reported,
            purged: None,
            message_id: value.message_id,
            user_id: value.user_id,
            server_id: value.server_id,
            object_id: value.object_id,
            hash: None,
            uploaded_at: None,
            uploader_id: None,
            used_for: None,
            forwarded_from: None,
        }
    }
}

impl From<crate::Metadata> for Metadata {
    fn from(value: crate::Metadata) -> Self {
        match value {
            crate::Metadata::File => Metadata::File,
            crate::Metadata::Text => Metadata::Text,
            crate::Metadata::Image {
                width,
                height,
                thumbhash,
                animated,
            } => Metadata::Image {
                width: width as usize,
                height: height as usize,
                thumbhash,
                animated,
            },
            crate::Metadata::Video { width, height } => Metadata::Video {
                width: width as usize,
                height: height as usize,
            },
            crate::Metadata::Audio { duration_ms } => Metadata::Audio { duration_ms },
        }
    }
}

impl From<Metadata> for crate::Metadata {
    fn from(value: Metadata) -> crate::Metadata {
        match value {
            Metadata::File => crate::Metadata::File,
            Metadata::Text => crate::Metadata::Text,
            Metadata::Image {
                width,
                height,
                thumbhash,
                animated,
            } => crate::Metadata::Image {
                width: width as isize,
                height: height as isize,
                thumbhash,
                animated,
            },
            Metadata::Video { width, height } => crate::Metadata::Video {
                width: width as isize,
                height: height as isize,
            },
            Metadata::Audio { duration_ms } => crate::Metadata::Audio { duration_ms },
        }
    }
}

impl crate::Message {
    pub fn into_model(self, user: Option<User>, member: Option<Member>) -> Message {
        Message {
            id: self.id,
            nonce: self.nonce,
            channel: self.channel,
            author: self.author,
            user,
            member,
            webhook: self.webhook,
            content: self.content,
            system: self.system.map(Into::into),
            attachments: self
                .attachments
                .map(|v| v.into_iter().map(|f| f.into()).collect()),
            edited: self.edited,
            embeds: self.embeds,
            mentions: self.mentions,
            role_mentions: self.role_mentions,
            replies: self.replies,
            reactions: self.reactions,
            interactions: self.interactions.into(),
            components: self.components,
            interaction: self.interaction,
            masquerade: self.masquerade.map(Into::into),
            flags: self.flags.unwrap_or_default(),
            pinned: self.pinned,
            forwarded: self.forwarded,
            poll: self.poll.map(Into::into),
        }
    }
}

impl From<crate::PartialMessage> for PartialMessage {
    fn from(value: crate::PartialMessage) -> Self {
        PartialMessage {
            id: value.id,
            nonce: value.nonce,
            channel: value.channel,
            author: value.author,
            user: None,
            member: None,
            webhook: value.webhook,
            content: value.content,
            system: value.system.map(Into::into),
            attachments: value
                .attachments
                .map(|v| v.into_iter().map(|f| f.into()).collect()),
            edited: value.edited,
            embeds: value.embeds,
            mentions: value.mentions,
            role_mentions: value.role_mentions,
            replies: value.replies,
            reactions: value.reactions,
            interactions: value.interactions.map(Into::into),
            components: value.components,
            interaction: value.interaction,
            masquerade: value.masquerade.map(Into::into),
            flags: value.flags,
            pinned: value.pinned,
            forwarded: value.forwarded,
            poll: value.poll.map(Into::into),
        }
    }
}

impl From<crate::SystemMessage> for SystemMessage {
    fn from(value: crate::SystemMessage) -> Self {
        match value {
            crate::SystemMessage::ChannelDescriptionChanged { by } => {
                Self::ChannelDescriptionChanged { by }
            }
            crate::SystemMessage::ChannelIconChanged { by } => Self::ChannelIconChanged { by },
            crate::SystemMessage::ChannelOwnershipChanged { from, to } => {
                Self::ChannelOwnershipChanged { from, to }
            }
            crate::SystemMessage::ChannelRenamed { name, by } => Self::ChannelRenamed { name, by },
            crate::SystemMessage::Text { content } => Self::Text { content },
            crate::SystemMessage::UserAdded { id, by } => Self::UserAdded { id, by },
            crate::SystemMessage::UserBanned { id } => Self::UserBanned { id },
            crate::SystemMessage::UserJoined { id } => Self::UserJoined { id },
            crate::SystemMessage::UserKicked { id } => Self::UserKicked { id },
            crate::SystemMessage::UserLeft { id } => Self::UserLeft { id },
            crate::SystemMessage::UserRemove { id, by } => Self::UserRemove { id, by },
            crate::SystemMessage::MessagePinned { id, by } => Self::MessagePinned { id, by },
            crate::SystemMessage::MessageUnpinned { id, by } => Self::MessageUnpinned { id, by },
            crate::SystemMessage::CallStarted { by, finished_at } => {
                Self::CallStarted { by, finished_at }
            }
            crate::SystemMessage::RaidAlert { joins, minutes } => Self::RaidAlert { joins, minutes },
            crate::SystemMessage::PollEnded {
                id,
                question,
                winners,
                votes,
                total,
            } => Self::PollEnded {
                id,
                question,
                winners,
                votes,
                total,
            },
            crate::SystemMessage::EventStarted { id, title } => Self::EventStarted { id, title },
            crate::SystemMessage::AutoModAlert {
                user,
                rule,
                channel,
                content,
            } => Self::AutoModAlert {
                user,
                rule,
                channel,
                content,
            },
        }
    }
}

impl From<crate::Interactions> for Interactions {
    fn from(value: crate::Interactions) -> Self {
        Interactions {
            reactions: value
                .reactions
                .map(|reactions| reactions.into_iter().collect()),
            restrict_reactions: value.restrict_reactions,
        }
    }
}

impl From<Interactions> for crate::Interactions {
    fn from(value: Interactions) -> Self {
        crate::Interactions {
            reactions: value
                .reactions
                .map(|reactions| reactions.into_iter().collect()),
            restrict_reactions: value.restrict_reactions,
        }
    }
}

impl From<crate::AppendMessage> for AppendMessage {
    fn from(value: crate::AppendMessage) -> Self {
        AppendMessage {
            embeds: value.embeds,
        }
    }
}

impl From<crate::Masquerade> for Masquerade {
    fn from(value: crate::Masquerade) -> Self {
        Masquerade {
            name: value.name,
            avatar: value.avatar,
            colour: value.colour,
        }
    }
}

impl From<Masquerade> for crate::Masquerade {
    fn from(value: Masquerade) -> Self {
        crate::Masquerade {
            name: value.name,
            avatar: value.avatar,
            colour: value.colour,
        }
    }
}

impl From<crate::PolicyChange> for PolicyChange {
    fn from(value: crate::PolicyChange) -> Self {
        PolicyChange {
            created_time: value.created_time,
            effective_time: value.effective_time,
            description: value.description,
            url: value.url,
            translations: value.translations,
        }
    }
}

impl From<crate::Report> for Report {
    fn from(value: crate::Report) -> Self {
        Report {
            id: value.id,
            author_id: value.author_id,
            content: value.content,
            additional_context: value.additional_context,
            status: value.status,
            notes: value.notes,
        }
    }
}

impl From<crate::ServerBan> for ServerBan {
    fn from(value: crate::ServerBan) -> Self {
        ServerBan {
            id: value.id.into(),
            reason: value.reason,
        }
    }
}

impl From<crate::Member> for Member {
    fn from(value: crate::Member) -> Self {
        Member {
            id: value.id.into(),
            joined_at: value.joined_at,
            nickname: value.nickname,
            pronouns: value.pronouns,
            avatar: value.avatar.map(|f| f.into()),
            roles: value.roles,
            timeout: value.timeout,
            can_publish: value.can_publish,
            can_receive: value.can_receive,
            accepted_rules_at: value.accepted_rules_at,
        }
    }
}

impl From<Member> for crate::Member {
    fn from(value: Member) -> crate::Member {
        crate::Member {
            id: value.id.into(),
            joined_at: value.joined_at,
            nickname: value.nickname,
            pronouns: value.pronouns,
            avatar: value.avatar.map(|f| f.into()),
            roles: value.roles,
            timeout: value.timeout,
            can_publish: value.can_publish,
            can_receive: value.can_receive,
            accepted_rules_at: value.accepted_rules_at,
        }
    }
}

impl From<crate::PartialMember> for PartialMember {
    fn from(value: crate::PartialMember) -> Self {
        PartialMember {
            id: value.id.map(|id| id.into()),
            joined_at: value.joined_at,
            nickname: value.nickname,
            pronouns: value.pronouns,
            avatar: value.avatar.map(|f| f.into()),
            roles: value.roles,
            timeout: value.timeout,
            can_publish: value.can_publish,
            can_receive: value.can_receive,
            accepted_rules_at: value.accepted_rules_at,
        }
    }
}

impl From<PartialMember> for crate::PartialMember {
    fn from(value: PartialMember) -> crate::PartialMember {
        crate::PartialMember {
            id: value.id.map(|id| id.into()),
            joined_at: value.joined_at,
            nickname: value.nickname,
            pronouns: value.pronouns,
            avatar: value.avatar.map(|f| f.into()),
            roles: value.roles,
            timeout: value.timeout,
            can_publish: value.can_publish,
            can_receive: value.can_receive,
            accepted_rules_at: value.accepted_rules_at,
        }
    }
}

impl From<crate::MemberCompositeKey> for MemberCompositeKey {
    fn from(value: crate::MemberCompositeKey) -> Self {
        MemberCompositeKey {
            server: value.server,
            user: value.user,
        }
    }
}

impl From<MemberCompositeKey> for crate::MemberCompositeKey {
    fn from(value: MemberCompositeKey) -> crate::MemberCompositeKey {
        crate::MemberCompositeKey {
            server: value.server,
            user: value.user,
        }
    }
}

impl From<crate::FieldsMember> for FieldsMember {
    fn from(value: crate::FieldsMember) -> Self {
        match value {
            crate::FieldsMember::Avatar => FieldsMember::Avatar,
            crate::FieldsMember::Nickname => FieldsMember::Nickname,
            crate::FieldsMember::Pronouns => FieldsMember::Pronouns,
            crate::FieldsMember::Roles => FieldsMember::Roles,
            crate::FieldsMember::Timeout => FieldsMember::Timeout,
            crate::FieldsMember::CanReceive => FieldsMember::CanReceive,
            crate::FieldsMember::CanPublish => FieldsMember::CanPublish,
            crate::FieldsMember::JoinedAt => FieldsMember::JoinedAt,
            crate::FieldsMember::VoiceChannel => FieldsMember::VoiceChannel,
        }
    }
}

impl From<FieldsMember> for crate::FieldsMember {
    fn from(value: FieldsMember) -> crate::FieldsMember {
        match value {
            FieldsMember::Avatar => crate::FieldsMember::Avatar,
            FieldsMember::Nickname => crate::FieldsMember::Nickname,
            FieldsMember::Pronouns => crate::FieldsMember::Pronouns,
            FieldsMember::Roles => crate::FieldsMember::Roles,
            FieldsMember::Timeout => crate::FieldsMember::Timeout,
            FieldsMember::CanReceive => crate::FieldsMember::CanReceive,
            FieldsMember::CanPublish => crate::FieldsMember::CanPublish,
            FieldsMember::JoinedAt => crate::FieldsMember::JoinedAt,
            FieldsMember::VoiceChannel => crate::FieldsMember::VoiceChannel,
        }
    }
}

impl From<crate::RemovalIntention> for RemovalIntention {
    fn from(value: crate::RemovalIntention) -> Self {
        match value {
            crate::RemovalIntention::Ban => RemovalIntention::Ban,
            crate::RemovalIntention::Kick => RemovalIntention::Kick,
            crate::RemovalIntention::Leave => RemovalIntention::Leave,
        }
    }
}

impl crate::Server {
    pub async fn into(self, db: &Database) -> Server {
        let approximate_member_count = self.get_approximate_member_count(db).await;

        Server {
            id: self.id,
            owner: self.owner,
            name: self.name,
            description: self.description,
            channels: self.channels,
            categories: self
                .categories
                .map(|categories| categories.into_iter().map(|v| v.into()).collect()),
            system_messages: self.system_messages.map(|v| v.into()),
            roles: self.roles.into_iter().map(|(k, v)| (k, v.into())).collect(),
            default_permissions: self.default_permissions,
            icon: self.icon.map(|f| f.into()),
            banner: self.banner.map(|f| f.into()),
            flags: self.flags.unwrap_or_default() as u32,
            nsfw: self.nsfw,
            analytics: self.analytics,
            discoverable: self.discoverable,
            access: self.access,
            verification_level: self.verification_level,
            default_notifications: self.default_notifications,
            invites_paused_until: self.invites_paused_until,
            dms_paused_until: self.dms_paused_until,
            rules_screening_since: self.rules_screening_since,
            supercharge: self.supercharge,
            vanity_invite: self.vanity_invite,
            rules_channel: self.rules_channel,
            stats: self.stats,
            lobby: self.lobby,
            threads_disabled: self.threads_disabled,
            forums_disabled: self.forums_disabled,
            approximate_member_count,
        }
    }
}

impl From<Server> for crate::Server {
    fn from(value: Server) -> crate::Server {
        crate::Server {
            id: value.id,
            owner: value.owner,
            name: value.name,
            description: value.description,
            channels: value.channels,
            categories: value
                .categories
                .map(|categories| categories.into_iter().map(|v| v.into()).collect()),
            system_messages: value.system_messages.map(|v| v.into()),
            roles: value
                .roles
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            default_permissions: value.default_permissions,
            icon: value.icon.map(|f| f.into()),
            banner: value.banner.map(|f| f.into()),
            flags: Some(value.flags as i32),
            nsfw: value.nsfw,
            analytics: value.analytics,
            discoverable: value.discoverable,
            access: value.access,
            verification_level: value.verification_level,
            default_notifications: value.default_notifications,
            invites_paused_until: value.invites_paused_until,
            dms_paused_until: value.dms_paused_until,
            rules_screening_since: value.rules_screening_since,
            automod: None,
            supercharge: value.supercharge,
            vanity_invite: value.vanity_invite,
            rules_channel: value.rules_channel,
            stats: value.stats,
            lobby: value.lobby,
            threads_disabled: value.threads_disabled,
            forums_disabled: value.forums_disabled,
        }
    }
}

impl From<crate::PartialServer> for PartialServer {
    fn from(value: crate::PartialServer) -> Self {
        PartialServer {
            id: value.id,
            owner: value.owner,
            name: value.name,
            description: value.description,
            channels: value.channels,
            categories: value
                .categories
                .map(|categories| categories.into_iter().map(|v| v.into()).collect()),
            system_messages: value.system_messages.map(|v| v.into()),
            roles: value
                .roles
                .map(|roles| roles.into_iter().map(|(k, v)| (k, v.into())).collect()),
            default_permissions: value.default_permissions,
            icon: value.icon.map(|f| f.into()),
            banner: value.banner.map(|f| f.into()),
            flags: value.flags.map(|v| v as u32),
            nsfw: value.nsfw,
            analytics: value.analytics,
            discoverable: value.discoverable,
            access: value.access,
            verification_level: value.verification_level,
            default_notifications: value.default_notifications,
            invites_paused_until: value.invites_paused_until,
            dms_paused_until: value.dms_paused_until,
            rules_screening_since: value.rules_screening_since,
            supercharge: value.supercharge,
            vanity_invite: value.vanity_invite,
            rules_channel: value.rules_channel,
            stats: value.stats,
            lobby: value.lobby,
            threads_disabled: value.threads_disabled,
            forums_disabled: value.forums_disabled,
            approximate_member_count: None,
        }
    }
}

impl From<PartialServer> for crate::PartialServer {
    fn from(value: PartialServer) -> crate::PartialServer {
        crate::PartialServer {
            id: value.id,
            owner: value.owner,
            name: value.name,
            description: value.description,
            channels: value.channels,
            categories: value
                .categories
                .map(|categories| categories.into_iter().map(|v| v.into()).collect()),
            system_messages: value.system_messages.map(|v| v.into()),
            roles: value
                .roles
                .map(|roles| roles.into_iter().map(|(k, v)| (k, v.into())).collect()),
            default_permissions: value.default_permissions,
            icon: value.icon.map(|f| f.into()),
            banner: value.banner.map(|f| f.into()),
            flags: value.flags.map(|v| v as i32),
            nsfw: value.nsfw,
            analytics: value.analytics,
            discoverable: value.discoverable,
            access: value.access,
            verification_level: value.verification_level,
            default_notifications: value.default_notifications,
            invites_paused_until: value.invites_paused_until,
            dms_paused_until: value.dms_paused_until,
            rules_screening_since: value.rules_screening_since,
            automod: None,
            supercharge: value.supercharge,
            vanity_invite: value.vanity_invite,
            rules_channel: value.rules_channel,
            stats: value.stats,
            lobby: value.lobby,
            threads_disabled: value.threads_disabled,
            forums_disabled: value.forums_disabled,
        }
    }
}

impl From<crate::FieldsServer> for FieldsServer {
    fn from(value: crate::FieldsServer) -> Self {
        match value {
            crate::FieldsServer::Banner => FieldsServer::Banner,
            crate::FieldsServer::Categories => FieldsServer::Categories,
            crate::FieldsServer::Description => FieldsServer::Description,
            crate::FieldsServer::Icon => FieldsServer::Icon,
            crate::FieldsServer::SystemMessages => FieldsServer::SystemMessages,
            crate::FieldsServer::RulesChannel => FieldsServer::RulesChannel,
            crate::FieldsServer::RulesScreening => FieldsServer::RulesScreening,
            crate::FieldsServer::Stats => FieldsServer::Stats,
            crate::FieldsServer::Lobby => FieldsServer::Lobby,
            crate::FieldsServer::VanityInvite => FieldsServer::VanityInvite,
        }
    }
}

impl From<FieldsServer> for crate::FieldsServer {
    fn from(value: FieldsServer) -> crate::FieldsServer {
        match value {
            FieldsServer::Banner => crate::FieldsServer::Banner,
            FieldsServer::Categories => crate::FieldsServer::Categories,
            FieldsServer::Description => crate::FieldsServer::Description,
            FieldsServer::Icon => crate::FieldsServer::Icon,
            FieldsServer::SystemMessages => crate::FieldsServer::SystemMessages,
            FieldsServer::RulesChannel => crate::FieldsServer::RulesChannel,
            FieldsServer::RulesScreening => crate::FieldsServer::RulesScreening,
            FieldsServer::Stats => crate::FieldsServer::Stats,
            FieldsServer::Lobby => crate::FieldsServer::Lobby,
            FieldsServer::VanityInvite => crate::FieldsServer::VanityInvite,
        }
    }
}

impl From<crate::Category> for Category {
    fn from(value: crate::Category) -> Self {
        Category {
            id: value.id,
            title: value.title,
            channels: value.channels,
            default_permissions: value.default_permissions,
            role_permissions: value.role_permissions,
        }
    }
}

impl From<Category> for crate::Category {
    fn from(value: Category) -> Self {
        crate::Category {
            id: value.id,
            title: value.title,
            channels: value.channels,
            default_permissions: value.default_permissions,
            role_permissions: value.role_permissions,
        }
    }
}

impl From<crate::SystemMessageChannels> for SystemMessageChannels {
    fn from(value: crate::SystemMessageChannels) -> Self {
        SystemMessageChannels {
            user_joined: value.user_joined,
            user_left: value.user_left,
            user_kicked: value.user_kicked,
            user_banned: value.user_banned,
        }
    }
}

impl From<SystemMessageChannels> for crate::SystemMessageChannels {
    fn from(value: SystemMessageChannels) -> Self {
        crate::SystemMessageChannels {
            user_joined: value.user_joined,
            user_left: value.user_left,
            user_kicked: value.user_kicked,
            user_banned: value.user_banned,
        }
    }
}

impl From<crate::Role> for Role {
    fn from(value: crate::Role) -> Self {
        Role {
            id: value.id,
            name: value.name,
            permissions: value.permissions,
            colour: value.colour,
            hoist: value.hoist,
            rank: value.rank,
            icon: value.icon.map(|f| f.into()),
            owner: value.owner
        }
    }
}

impl From<Role> for crate::Role {
    fn from(value: Role) -> crate::Role {
        crate::Role {
            id: value.id,
            name: value.name,
            permissions: value.permissions,
            colour: value.colour,
            hoist: value.hoist,
            rank: value.rank,
            icon: value.icon.map(|f| f.into()),
            owner: value.owner
        }
    }
}

impl From<crate::PartialRole> for PartialRole {
    fn from(value: crate::PartialRole) -> Self {
        PartialRole {
            id: value.id,
            name: value.name,
            permissions: value.permissions,
            colour: value.colour,
            hoist: value.hoist,
            rank: value.rank,
            icon: value.icon.map(|f| f.into()),
            owner: value.owner
        }
    }
}

impl From<PartialRole> for crate::PartialRole {
    fn from(value: PartialRole) -> crate::PartialRole {
        crate::PartialRole {
            id: value.id,
            name: value.name,
            permissions: value.permissions,
            colour: value.colour,
            hoist: value.hoist,
            rank: value.rank,
            icon: value.icon.map(|f| f.into()),
            owner: value.owner
        }
    }
}

impl From<crate::FieldsRole> for FieldsRole {
    fn from(value: crate::FieldsRole) -> Self {
        match value {
            crate::FieldsRole::Colour => FieldsRole::Colour,
            crate::FieldsRole::Icon => FieldsRole::Icon,
        }
    }
}

impl From<FieldsRole> for crate::FieldsRole {
    fn from(value: FieldsRole) -> Self {
        match value {
            FieldsRole::Colour => crate::FieldsRole::Colour,
            FieldsRole::Icon => crate::FieldsRole::Icon,
        }
    }
}

impl crate::User {
    pub async fn into<'a, P>(self, db: &Database, perspective: P) -> User
    where
        P: Into<Option<&'a crate::User>>,
    {
        let perspective = perspective.into();
        let (relationship, can_see_profile) = if let Some(perspective) = perspective {
            let mut query = DatabasePermissionQuery::new(db, perspective).user(&self);

            if perspective.id == self.id {
                (RelationshipStatus::User, true)
            } else {
                (
                    perspective
                        .relations
                        .as_ref()
                        .map(|relations| {
                            relations
                                .iter()
                                .find(|relationship| relationship.id == self.id)
                                .map(|relationship| relationship.status.clone().into())
                                .unwrap_or_default()
                        })
                        .unwrap_or_default(),
                    calculate_user_permissions(&mut query)
                        .await
                        .has_user_permission(UserPermission::ViewProfile),
                )
            }
        } else {
            (RelationshipStatus::None, false)
        };

        let badges = self.get_badges().await;
        let viewer = perspective.map(|perspective| perspective.id.clone());

        let mut user = User {
            username: self.username,
            discriminator: self.discriminator,
            display_name: self.display_name,
            pronouns: self.pronouns,
            avatar: self.avatar.map(|file| file.into()),
            relations: if let Some(crate::User { id, .. }) = perspective {
                if id == &self.id {
                    self.relations
                        .unwrap_or_default()
                        .into_iter()
                        .map(|relation| relation.into())
                        .collect()
                } else {
                    vec![]
                }
            } else {
                vec![]
            },
            badges,
            online: can_see_profile
                && revolt_presence::is_online(&self.id).await
                && !matches!(
                    self.status,
                    Some(crate::UserStatus {
                        presence: Some(crate::Presence::Invisible),
                        ..
                    })
                ),
            status: if can_see_profile {
                self.status.and_then(|status| status.into(true))
            } else {
                None
            },
            flags: self.flags.unwrap_or_default() as u32,
            privileged: self.privileged,
            official: self.official,
            official_bot: self.official_bot,
            official_names: self.official_names,
            bot: self.bot.map(|bot| bot.into()),
            style: self.style,
            relationship,
            id: self.id,
        };
        // a custom status emoji only for those who share its server
        crate::util::status_emoji::hide_for(db, &mut user.status, &user.id, viewer.as_deref())
            .await;
        user
    }

    /// Convert user object into user model assuming mutual connection, and
    /// keep a custom status emoji only where the viewer may see it (see
    /// `util::status_emoji`). Use this one wherever a single user is shown
    /// to a viewer; lists use `User::fetch_many_ids_as_mutuals`.
    pub async fn into_known_for(self, db: &Database, viewer: &crate::User, is_online: bool) -> User {
        let viewer_id = viewer.id.clone();
        let mut user = self.into_known(viewer, is_online).await;
        crate::util::status_emoji::hide_for(db, &mut user.status, &user.id.clone(), Some(&viewer_id))
            .await;
        user
    }

    /// Convert user object into user model assuming mutual connection
    ///
    /// Relations will never be included, i.e. when we process ourselves
    ///
    /// **Warning:** the status is returned as stored, custom emoji included.
    /// A custom status emoji can name a server the viewer is not in: pass
    /// the result through `util::status_emoji` (as Ready and
    /// `fetch_many_ids_as_mutuals` do) or use `into_known_for`.
    pub async fn into_known<'a, P>(self, perspective: P, is_online: bool) -> User
    where
        P: Into<Option<&'a crate::User>>,
    {
        let perspective = perspective.into();
        let (relationship, can_see_profile) = if let Some(perspective) = perspective {
            if perspective.id == self.id {
                (RelationshipStatus::User, true)
            } else {
                let relationship = perspective
                    .relations
                    .as_ref()
                    .map(|relations| {
                        relations
                            .iter()
                            .find(|relationship| relationship.id == self.id)
                            .map(|relationship| relationship.status.clone().into())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();

                let can_see_profile = relationship != RelationshipStatus::BlockedOther;
                (relationship, can_see_profile)
            }
        } else {
            (RelationshipStatus::None, false)
        };

        let badges = self.get_badges().await;

        User {
            username: self.username,
            discriminator: self.discriminator,
            display_name: self.display_name,
            pronouns: self.pronouns,
            avatar: self.avatar.map(|file| file.into()),
            relations: vec![],
            badges,
            online: can_see_profile
                && is_online
                && !matches!(
                    self.status,
                    Some(crate::UserStatus {
                        presence: Some(crate::Presence::Invisible),
                        ..
                    })
                ),
            status: if can_see_profile {
                self.status.and_then(|status| status.into(true))
            } else {
                None
            },
            flags: self.flags.unwrap_or_default() as u32,
            privileged: self.privileged,
            official: self.official,
            official_bot: self.official_bot,
            official_names: self.official_names,
            bot: self.bot.map(|bot| bot.into()),
            style: self.style,
            relationship,
            id: self.id,
        }
    }

    /// Convert user object into user model without presence information
    pub async fn into_known_static(self, is_online: bool) -> User {
        let badges = self.get_badges().await;

        User {
            username: self.username,
            discriminator: self.discriminator,
            display_name: self.display_name,
            pronouns: self.pronouns,
            avatar: self.avatar.map(|file| file.into()),
            relations: vec![],
            badges,
            online: is_online
                && !matches!(
                    self.status,
                    Some(crate::UserStatus {
                        presence: Some(crate::Presence::Invisible),
                        ..
                    })
                ),
            // sent to everyone in a channel: no custom status emoji, which
            // could name a server some of them are not in
            status: {
                let mut status = self.status.and_then(|status| status.into(true));
                crate::util::status_emoji::drop_custom_emoji(&mut status);
                status
            },
            flags: self.flags.unwrap_or_default() as u32,
            privileged: self.privileged,
            official: self.official,
            official_bot: self.official_bot,
            official_names: self.official_names,
            bot: self.bot.map(|bot| bot.into()),
            style: self.style,
            relationship: RelationshipStatus::None, // events client will populate this from cache
            id: self.id,
        }
    }

    pub async fn into_self(self, force_online: bool) -> User {
        let badges = self.get_badges().await;

        User {
            username: self.username,
            discriminator: self.discriminator,
            display_name: self.display_name,
            pronouns: self.pronouns,
            avatar: self.avatar.map(|file| file.into()),
            relations: self
                .relations
                .map(|relationships| {
                    relationships
                        .into_iter()
                        .map(|relationship| relationship.into())
                        .collect()
                })
                .unwrap_or_default(),
            badges,
            online: (force_online || revolt_presence::is_online(&self.id).await)
                && !matches!(
                    self.status,
                    Some(crate::UserStatus {
                        presence: Some(crate::Presence::Invisible),
                        ..
                    })
                ),
            status: self.status.and_then(|status| status.into(true)),
            flags: self.flags.unwrap_or_default() as u32,
            privileged: self.privileged,
            official: self.official,
            official_bot: self.official_bot,
            official_names: self.official_names,
            bot: self.bot.map(|bot| bot.into()),
            style: self.style,
            relationship: RelationshipStatus::User,
            id: self.id,
        }
    }

    pub fn as_author_for_system(&self) -> MessageAuthor {
        MessageAuthor::System {
            username: &self.username,
            avatar: self.avatar.as_ref().map(|file| file.id.as_ref()),
        }
    }
}

impl From<User> for crate::User {
    fn from(value: User) -> crate::User {
        crate::User {
            id: value.id,
            username: value.username,
            discriminator: value.discriminator,
            display_name: value.display_name,
            pronouns: value.pronouns,
            avatar: value.avatar.map(Into::into),
            relations: None,
            badges: Some(value.badges as i32),
            status: value.status.map(Into::into),
            profile: None,
            flags: Some(value.flags as i32),
            privileged: value.privileged,
            official: value.official,
            official_bot: value.official_bot,
            official_names: value.official_names,
            bot: value.bot.map(Into::into),
            style: value.style,
            plus: None,
            privacy: None,
            locale: None,
            suspended_until: None,
            last_acknowledged_policy_change: Timestamp::UNIX_EPOCH,
        }
    }
}

impl From<crate::PartialUser> for PartialUser {
    fn from(value: crate::PartialUser) -> Self {
        PartialUser {
            username: value.username,
            discriminator: value.discriminator,
            display_name: value.display_name,
            pronouns: value.pronouns,
            avatar: value.avatar.map(|file| file.into()),
            relations: value.relations.map(|relationships| {
                relationships
                    .into_iter()
                    .map(|relationship| relationship.into())
                    .collect()
            }),
            badges: value.badges.map(|badges| badges as u32),
            status: value.status.and_then(|status| status.into(false)),
            flags: value.flags.map(|flags| flags as u32),
            privileged: value.privileged,
            official: value.official,
            official_bot: value.official_bot,
            official_names: value.official_names,
            bot: value.bot.map(|bot| bot.into()),
            style: value.style,
            relationship: None,
            online: None,
            id: value.id,
        }
    }
}

impl From<FieldsUser> for crate::FieldsUser {
    fn from(value: FieldsUser) -> Self {
        match value {
            FieldsUser::Avatar => crate::FieldsUser::Avatar,
            FieldsUser::ProfileBackground => crate::FieldsUser::ProfileBackground,
            FieldsUser::ProfileContent => crate::FieldsUser::ProfileContent,
            FieldsUser::StatusPresence => crate::FieldsUser::StatusPresence,
            FieldsUser::StatusText => crate::FieldsUser::StatusText,
            FieldsUser::DisplayName => crate::FieldsUser::DisplayName,
            FieldsUser::Pronouns => crate::FieldsUser::Pronouns,
            FieldsUser::Style => crate::FieldsUser::Style,

            FieldsUser::Internal => crate::FieldsUser::None,
        }
    }
}

impl From<crate::FieldsUser> for FieldsUser {
    fn from(value: crate::FieldsUser) -> Self {
        match value {
            crate::FieldsUser::Avatar => FieldsUser::Avatar,
            crate::FieldsUser::ProfileBackground => FieldsUser::ProfileBackground,
            crate::FieldsUser::ProfileContent => FieldsUser::ProfileContent,
            crate::FieldsUser::StatusPresence => FieldsUser::StatusPresence,
            crate::FieldsUser::StatusText => FieldsUser::StatusText,
            crate::FieldsUser::DisplayName => FieldsUser::DisplayName,
            crate::FieldsUser::Pronouns => FieldsUser::Pronouns,
            crate::FieldsUser::Style => FieldsUser::Style,

            crate::FieldsUser::Suspension => FieldsUser::Internal,
            crate::FieldsUser::None => FieldsUser::Internal,
        }
    }
}

impl From<crate::RelationshipStatus> for RelationshipStatus {
    fn from(value: crate::RelationshipStatus) -> Self {
        match value {
            crate::RelationshipStatus::None => RelationshipStatus::None,
            crate::RelationshipStatus::User => RelationshipStatus::User,
            crate::RelationshipStatus::Friend => RelationshipStatus::Friend,
            crate::RelationshipStatus::Outgoing => RelationshipStatus::Outgoing,
            crate::RelationshipStatus::Incoming => RelationshipStatus::Incoming,
            crate::RelationshipStatus::Blocked => RelationshipStatus::Blocked,
            crate::RelationshipStatus::BlockedOther => RelationshipStatus::BlockedOther,
        }
    }
}

impl From<crate::Relationship> for Relationship {
    fn from(value: crate::Relationship) -> Self {
        Self {
            user_id: value.id,
            status: value.status.into(),
        }
    }
}

impl From<crate::Presence> for Presence {
    fn from(value: crate::Presence) -> Self {
        match value {
            crate::Presence::Online => Presence::Online,
            crate::Presence::Idle => Presence::Idle,
            crate::Presence::Focus => Presence::Focus,
            crate::Presence::Busy => Presence::Busy,
            crate::Presence::Invisible => Presence::Invisible,
        }
    }
}

impl From<Presence> for crate::Presence {
    fn from(value: Presence) -> crate::Presence {
        match value {
            Presence::Online => crate::Presence::Online,
            Presence::Idle => crate::Presence::Idle,
            Presence::Focus => crate::Presence::Focus,
            Presence::Busy => crate::Presence::Busy,
            Presence::Invisible => crate::Presence::Invisible,
        }
    }
}

impl crate::UserStatus {
    fn into(self, discard_invisible: bool) -> Option<UserStatus> {
        // a status past its expiry is never shown, even before crond clears it
        let expired = self
            .expires_at
            .is_some_and(|at| at <= Timestamp::now_utc());
        let (text, emoji, expires_at) = if expired {
            (None, None, None)
        } else {
            (self.text, self.emoji, self.expires_at)
        };

        let status = UserStatus {
            text,
            emoji,
            expires_at,
            presence: self.presence.and_then(|presence| {
                if discard_invisible && presence == crate::Presence::Invisible {
                    None
                } else {
                    Some(presence.into())
                }
            }),
        };

        if status.text.is_none() && status.emoji.is_none() && status.presence.is_none() {
            None
        } else {
            Some(status)
        }
    }
}

impl From<UserStatus> for crate::UserStatus {
    fn from(value: UserStatus) -> crate::UserStatus {
        crate::UserStatus {
            text: value.text,
            emoji: value.emoji,
            expires_at: value.expires_at,
            presence: value.presence.map(|presence| presence.into()),
        }
    }
}

/// Who is looking at a profile. There is no plain conversion from a stored
/// profile to the API type: this is the only way out, so a field cannot be
/// sent without passing the check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileViewer {
    /// The person themselves: everything, including what they hid
    Owner,
    /// Someone else. `personal` is false on a minor's profile for anyone
    /// who is not a friend (`may_see_personal_sections`).
    Other { personal: bool },
}

impl crate::User {
    /// Whether this section of their profile shows to others
    pub fn shows_section(&self, id: &str) -> bool {
        self.profile.as_ref().is_none_or(|profile| profile.shows_section(id))
    }
}

impl crate::UserProfile {
    /// Sections that show, in the owner's order. A section added after they
    /// saved their order joins at the end, unless they hid it.
    pub fn visible_sections(&self) -> Vec<String> {
        let mut sections: Vec<String> = self
            .sections
            .iter()
            .flatten()
            .filter(|id| PROFILE_SECTIONS.contains(&id.as_str()))
            .cloned()
            .collect();
        for id in PROFILE_SECTIONS {
            if !sections.iter().any(|section| section == id) {
                sections.push(id.to_string());
            }
        }
        sections.retain(|id| !self.hidden_sections.contains(id));
        sections
    }

    /// Whether this section shows to people other than the owner
    pub fn shows_section(&self, id: &str) -> bool {
        self.visible_sections().iter().any(|section| section == id)
    }

    /// The text of the "about" section for anyone but the owner
    pub fn public_content(&self) -> Option<&str> {
        if self.hidden_sections.iter().any(|id| id == "about") {
            None
        } else {
            self.content.as_deref()
        }
    }

    /// The profile as this viewer may have it. Hidden sections are left out,
    /// not flagged, and `hidden_sections` only ever reaches the owner.
    pub fn for_viewer(self, viewer: ProfileViewer) -> UserProfile {
        let mut sections = self.visible_sections();
        let personal = match viewer {
            ProfileViewer::Owner => {
                return UserProfile {
                    creator: false,
                    content: self.content,
                    background: self.background.map(|file| file.into()),
                    links: self.links,
                    interests: self.interests,
                    sections: Some(sections),
                    hidden_sections: self.hidden_sections,
                };
            }
            ProfileViewer::Other { personal } => personal,
        };
        if !personal {
            sections.retain(|id| !matches!(id.as_str(), "links" | "interests"));
        }
        let shows = |sections: &[String], id: &str| sections.iter().any(|section| section == id);
        let content = self.public_content().map(str::to_string);
        UserProfile {
            creator: false,
            content,
            background: self.background.map(|file| file.into()),
            // refused on saving; a value stored any other way still never leaves
            links: if shows(&sections, "links") {
                self.links
                    .into_iter()
                    .filter(|link| link.starts_with("https://"))
                    .collect()
            } else {
                vec![]
            },
            interests: if shows(&sections, "interests") {
                self.interests
            } else {
                vec![]
            },
            sections: Some(sections),
            hidden_sections: vec![],
        }
    }
}

impl From<UserProfile> for crate::UserProfile {
    fn from(value: UserProfile) -> crate::UserProfile {
        crate::UserProfile {
            content: value.content,
            background: value.background.map(|file| file.into()),
            links: value.links,
            interests: value.interests,
            sections: value.sections,
            hidden_sections: value.hidden_sections,
        }
    }
}

impl From<crate::BotInformation> for BotInformation {
    fn from(value: crate::BotInformation) -> Self {
        BotInformation {
            owner_id: value.owner,
            verified: value.verified,
        }
    }
}

impl From<BotInformation> for crate::BotInformation {
    fn from(value: BotInformation) -> crate::BotInformation {
        crate::BotInformation {
            owner: value.owner_id,
            verified: value.verified,
        }
    }
}

impl From<crate::FieldsMessage> for FieldsMessage {
    fn from(value: crate::FieldsMessage) -> Self {
        match value {
            crate::FieldsMessage::Pinned => FieldsMessage::Pinned,
        }
    }
}
impl From<FieldsMessage> for crate::FieldsMessage {
    fn from(value: FieldsMessage) -> Self {
        match value {
            FieldsMessage::Pinned => crate::FieldsMessage::Pinned,
        }
    }
}

impl From<crate::VoiceInformation> for VoiceInformation {
    fn from(value: crate::VoiceInformation) -> Self {
        VoiceInformation {
            max_users: value.max_users,
            stage: value.stage,
        }
    }
}

impl From<VoiceInformation> for crate::VoiceInformation {
    fn from(value: VoiceInformation) -> Self {
        crate::VoiceInformation {
            max_users: value.max_users,
            stage: value.stage,
        }
    }
}

impl From<crate::AuditLogEntryAction> for AuditLogEntryAction {
    fn from(value: crate::AuditLogEntryAction) -> Self {
        match value {
            crate::AuditLogEntryAction::MessageDelete { author, channel } => {
                AuditLogEntryAction::MessageDelete { author, channel }
            }
            crate::AuditLogEntryAction::BanCreate { user } => {
                AuditLogEntryAction::BanCreate { user }
            }
            crate::AuditLogEntryAction::BanDelete { user } => {
                AuditLogEntryAction::BanDelete { user }
            }
            crate::AuditLogEntryAction::ChannelCreate { channel, name } => {
                AuditLogEntryAction::ChannelCreate { channel, name }
            }
            crate::AuditLogEntryAction::MemberEdit {
                user,
                before,
                after,
            } => AuditLogEntryAction::MemberEdit {
                user,
                before: before.into(),
                after: after.into(),
            },
            crate::AuditLogEntryAction::MemberKick { user } => {
                AuditLogEntryAction::MemberKick { user }
            }
            crate::AuditLogEntryAction::ServerEdit { before, after } => {
                AuditLogEntryAction::ServerEdit {
                    before: before.into(),
                    after: after.into(),
                }
            }
            crate::AuditLogEntryAction::RoleEdit {
                role,
                before,
                after,
            } => AuditLogEntryAction::RoleEdit {
                role,
                before: before.into(),
                after: after.into(),
            },
            crate::AuditLogEntryAction::RoleCreate { role, name } => {
                AuditLogEntryAction::RoleCreate { role, name }
            }
            crate::AuditLogEntryAction::RoleDelete { role, name } => {
                AuditLogEntryAction::RoleDelete { role, name }
            }
            crate::AuditLogEntryAction::RolesReorder { before, after } => {
                AuditLogEntryAction::RolesReorder { before, after }
            }
            crate::AuditLogEntryAction::MessageBulkDelete { channel, count } => {
                AuditLogEntryAction::MessageBulkDelete { channel, count }
            }
            crate::AuditLogEntryAction::ChannelEdit {
                channel,
                before,
                after,
            } => AuditLogEntryAction::ChannelEdit {
                channel,
                before: before.into(),
                after: after.into(),
            },
            crate::AuditLogEntryAction::ChannelRolePermissionsEdit {
                channel,
                role,
                permissions,
            } => AuditLogEntryAction::ChannelRolePermissionsEdit {
                channel,
                role,
                permissions: permissions.into(),
            },
            crate::AuditLogEntryAction::ChannelMemberPermissionsEdit {
                channel,
                member,
                permissions,
            } => AuditLogEntryAction::ChannelMemberPermissionsEdit {
                channel,
                member,
                permissions: permissions.into(),
            },
            crate::AuditLogEntryAction::ChannelDelete { channel, name } => {
                AuditLogEntryAction::ChannelDelete { channel, name }
            }
            crate::AuditLogEntryAction::InviteDelete { invite, channel } => {
                AuditLogEntryAction::InviteDelete { invite, channel }
            }
            crate::AuditLogEntryAction::WebhookCreate {
                webhook,
                name,
                channel,
            } => AuditLogEntryAction::WebhookCreate {
                webhook,
                name,
                channel,
            },
            crate::AuditLogEntryAction::WebhookUpdate {
                webhook,
                name,
                channel,
            } => AuditLogEntryAction::WebhookUpdate {
                webhook,
                name,
                channel,
            },
            crate::AuditLogEntryAction::WebhookTokenReset {
                webhook,
                name,
                channel,
            } => AuditLogEntryAction::WebhookTokenReset {
                webhook,
                name,
                channel,
            },
            crate::AuditLogEntryAction::WebhookDelete {
                webhook,
                name,
                channel,
            } => AuditLogEntryAction::WebhookDelete {
                webhook,
                name,
                channel,
            },
            crate::AuditLogEntryAction::EmojiCreate { emoji, name } => {
                AuditLogEntryAction::EmojiCreate { emoji, name }
            }
            crate::AuditLogEntryAction::EmojiUpdate {
                emoji,
                before,
                after,
            } => AuditLogEntryAction::EmojiUpdate {
                emoji,
                before: before.into(),
                after: after.into(),
            },
            crate::AuditLogEntryAction::EmojiDelete { emoji, name } => {
                AuditLogEntryAction::EmojiDelete { emoji, name }
            }
            crate::AuditLogEntryAction::SoundboardSoundCreate { sound, name } => {
                AuditLogEntryAction::SoundboardSoundCreate { sound, name }
            }
            crate::AuditLogEntryAction::SoundboardSoundDelete { sound, name } => {
                AuditLogEntryAction::SoundboardSoundDelete { sound, name }
            }
            crate::AuditLogEntryAction::StageEnd { channel } => {
                AuditLogEntryAction::StageEnd { channel }
            }
            crate::AuditLogEntryAction::BotAdd {
                bot,
                permissions,
                scopes,
            } => AuditLogEntryAction::BotAdd {
                bot,
                permissions,
                scopes,
            },
            crate::AuditLogEntryAction::BotRemove { bot } => AuditLogEntryAction::BotRemove { bot },
            crate::AuditLogEntryAction::CommandPermissionsUpdate {
                bot,
                command,
                before,
                after,
            } => AuditLogEntryAction::CommandPermissionsUpdate {
                bot,
                command,
                before,
                after,
            },
            crate::AuditLogEntryAction::AutoModAction {
                rule,
                user,
                channel,
                actions,
            } => AuditLogEntryAction::AutoModAction {
                rule,
                user,
                channel,
                actions,
            },
            crate::AuditLogEntryAction::ServerEventCreate { event, title } => {
                AuditLogEntryAction::ServerEventCreate { event, title }
            }
            crate::AuditLogEntryAction::ServerEventEdit { event, title } => {
                AuditLogEntryAction::ServerEventEdit { event, title }
            }
            crate::AuditLogEntryAction::ServerEventCancel { event, title } => {
                AuditLogEntryAction::ServerEventCancel { event, title }
            }
            crate::AuditLogEntryAction::ServerEventDelete { event, title } => {
                AuditLogEntryAction::ServerEventDelete { event, title }
            }
            crate::AuditLogEntryAction::MessagePin {
                message,
                author,
                channel,
            } => AuditLogEntryAction::MessagePin {
                message,
                author,
                channel,
            },
            crate::AuditLogEntryAction::MessageUnpin {
                message,
                author,
                channel,
            } => AuditLogEntryAction::MessageUnpin {
                message,
                author,
                channel,
            },
            crate::AuditLogEntryAction::InviteCreate { invite, channel } => {
                AuditLogEntryAction::InviteCreate { invite, channel }
            }
        }
    }
}

impl From<crate::AuditLogEntry> for AuditLogEntry {
    fn from(value: crate::AuditLogEntry) -> Self {
        AuditLogEntry {
            id: value.id,
            server: value.server,
            reason: value.reason,
            user: value.user,
            target: value.target,
            action: value.action.into(),
        }
    }
}

impl From<crate::Account> for AccountInfo {
    fn from(item: crate::Account) -> Self {
        AccountInfo {
            id: item.id,
            email: item.email,
            age: None,
        }
    }
}

impl From<crate::MFATicket> for MFATicket {
    fn from(value: crate::MFATicket) -> Self {
        MFATicket {
            id: value.id,
            account_id: value.account_id,
            token: value.token,
            validated: value.validated,
            authorised: value.authorised,
            last_totp_code: value.last_totp_code,
        }
    }
}

impl From<crate::MultiFactorAuthentication> for MultiFactorStatus {
    fn from(item: crate::MultiFactorAuthentication) -> Self {
        MultiFactorStatus {
            // email_otp: item.enable_email_otp,
            // trusted_handover: item.enable_trusted_handover,
            // email_mfa: item.enable_email_mfa,
            totp_mfa: !item.totp_token.is_disabled(),
            // security_key_mfa: item.security_key_token.is_some(),
            recovery_active: !item.recovery_codes.is_empty(),
            ..Default::default()
        }
    }
}

impl From<crate::MFAMethod> for MFAMethod {
    fn from(value: crate::MFAMethod) -> Self {
        match value {
            crate::MFAMethod::Password => MFAMethod::Password,
            crate::MFAMethod::Recovery => MFAMethod::Recovery,
            crate::MFAMethod::Totp => MFAMethod::Totp,
        }
    }
}

impl From<crate::Session> for SessionInfo {
    fn from(item: crate::Session) -> Self {
        SessionInfo {
            id: item.id,
            name: item.name,
            last_seen: item.last_seen,
        }
    }
}

impl From<crate::Session> for Session {
    fn from(value: crate::Session) -> Self {
        Session {
            id: value.id,
            user_id: value.user_id,
            token: value.token,
            name: value.name,
            last_seen: value.last_seen,
            origin: value.origin,
            subscription: value.subscription.map(Into::into),
        }
    }
}

impl From<crate::WebPushSubscription> for WebPushSubscription {
    fn from(value: crate::WebPushSubscription) -> Self {
        WebPushSubscription {
            endpoint: value.endpoint,
            p256dh: value.p256dh,
            auth: value.auth,
        }
    }
}

impl From<WebPushSubscription> for crate::WebPushSubscription {
    fn from(value: WebPushSubscription) -> Self {
        crate::WebPushSubscription {
            endpoint: value.endpoint,
            p256dh: value.p256dh,
            auth: value.auth,
        }
    }
}

impl From<crate::PartialEmoji> for PartialEmoji {
    fn from(value: crate::PartialEmoji) -> Self {
        PartialEmoji { name: value.name }
    }
}

impl From<crate::CommunityItemKind> for CommunityItemKind {
    fn from(value: crate::CommunityItemKind) -> Self {
        match value {
            crate::CommunityItemKind::Decoration => CommunityItemKind::Decoration,
            crate::CommunityItemKind::Nameplate => CommunityItemKind::Nameplate,
            crate::CommunityItemKind::Effect => CommunityItemKind::Effect,
        }
    }
}

impl From<CommunityItemKind> for crate::CommunityItemKind {
    fn from(value: CommunityItemKind) -> Self {
        match value {
            CommunityItemKind::Decoration => crate::CommunityItemKind::Decoration,
            CommunityItemKind::Nameplate => crate::CommunityItemKind::Nameplate,
            CommunityItemKind::Effect => crate::CommunityItemKind::Effect,
        }
    }
}

impl From<crate::CommunityItemStatus> for CommunityItemStatus {
    fn from(value: crate::CommunityItemStatus) -> Self {
        match value {
            crate::CommunityItemStatus::Draft => CommunityItemStatus::Draft,
            crate::CommunityItemStatus::PendingReview => CommunityItemStatus::PendingReview,
            crate::CommunityItemStatus::ChangesRequested => CommunityItemStatus::ChangesRequested,
            crate::CommunityItemStatus::Published => CommunityItemStatus::Published,
            crate::CommunityItemStatus::Rejected => CommunityItemStatus::Rejected,
            crate::CommunityItemStatus::Suspended => CommunityItemStatus::Suspended,
            crate::CommunityItemStatus::Archived => CommunityItemStatus::Archived,
        }
    }
}

impl From<crate::CommunityRevision> for CommunityRevision {
    fn from(value: crate::CommunityRevision) -> Self {
        CommunityRevision {
            revision: value.revision,
            name: value.name,
            description: value.description,
            tags: value.tags,
            file: value.file.into(),
            status: value.status.into(),
            review_note: value.review_note,
            submitted_at: value.submitted_at,
        }
    }
}

impl From<crate::CommunityRights> for CommunityRights {
    fn from(value: crate::CommunityRights) -> Self {
        CommunityRights {
            terms_version: value.terms_version,
            confirmed_at: value.confirmed_at,
            third_party: value.third_party,
            ai_parts: value.ai_parts,
        }
    }
}

impl crate::CommunityItem {
    /// API form. Review notes, pending revisions and reports are only
    /// shown to the creator and reviewers (`private`).
    pub fn into_v0(self, private: bool, favourite: bool) -> CommunityItem {
        CommunityItem {
            reports: if private { self.reports.len() as u32 } else { 0 },
            review_note: if private { self.review_note } else { None },
            pending: if private {
                self.pending.map(Into::into)
            } else {
                None
            },
            rights: if private {
                self.rights.map(Into::into)
            } else {
                None
            },
            version: private.then_some(self.version),
            id: self.id,
            kind: self.kind.into(),
            name: self.name,
            creator_id: self.creator_id,
            file: self.file.into(),
            status: self.status.into(),
            description: self.description,
            tags: self.tags,
            revision: self.revision,
            published_at: self.published_at,
            popularity: self.popularity,
            favourite,
        }
    }
}

impl From<crate::CommunityReview> for CommunityReviewEntry {
    fn from(value: crate::CommunityReview) -> Self {
        CommunityReviewEntry {
            id: value.id,
            revision: value.revision,
            actor_id: value.actor_id,
            action: format!("{:?}", value.action),
            reason: value.reason,
            created_at: value.created_at,
        }
    }
}

impl From<crate::CreatorPermissionLog> for CreatorLogEntry {
    fn from(value: crate::CreatorPermissionLog) -> Self {
        CreatorLogEntry {
            id: value.id,
            actor_id: value.actor_id,
            action: format!("{:?}", value.action),
            permissions: value
                .permissions
                .iter()
                .map(|p| p.name().to_string())
                .collect(),
            reason: value.reason,
            created_at: value.created_at,
        }
    }
}

impl crate::User {
    /// Who made a community design, as anyone may see them
    pub fn into_community_creator(self, verified: bool, open_page: bool) -> CommunityCreator {
        CommunityCreator {
            id: self.id,
            display_name: self
                .display_name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(self.username),
            avatar: self.avatar.map(Into::into),
            verified,
            open_page,
        }
    }

    /// The same person as staff see them
    pub fn into_creator_account(self, verified: bool) -> CreatorAccount {
        CreatorAccount {
            id: self.id,
            username: self.username,
            discriminator: self.discriminator,
            display_name: self.display_name,
            avatar: self.avatar.map(Into::into),
            verified,
        }
    }
}

impl From<crate::JoinRequestStatus> for JoinRequestStatus {
    fn from(value: crate::JoinRequestStatus) -> Self {
        match value {
            crate::JoinRequestStatus::Pending => JoinRequestStatus::Pending,
            crate::JoinRequestStatus::Approved => JoinRequestStatus::Approved,
            crate::JoinRequestStatus::Rejected => JoinRequestStatus::Rejected,
        }
    }
}

impl From<crate::ServerJoinRequest> for ServerJoinRequest {
    fn from(value: crate::ServerJoinRequest) -> Self {
        ServerJoinRequest {
            server: value.id.server,
            user: value.id.user,
            status: value.status.into(),
            created_at: value.created_at,
            reviewed_by: value.reviewed_by,
            reviewed_at: value.reviewed_at,
        }
    }
}

impl From<crate::ServerEvent> for ServerEvent {
    fn from(value: crate::ServerEvent) -> Self {
        ServerEvent {
            id: value.id,
            server: value.server,
            creator: value.creator,
            title: value.title,
            description: value.description,
            starts_at: value.starts_at,
            ends_at: value.ends_at,
            place: value.place,
            cover: value.cover.map(Into::into),
            status: value.status,
            repeat: value.repeat,
            interested_count: value.interested_count,
            previous: value.previous,
            time_zone: value.time_zone,
        }
    }
}

impl From<crate::ThreadInformation> for ThreadInformation {
    fn from(value: crate::ThreadInformation) -> Self {
        ThreadInformation {
            owner: value.owner,
            starter_message: value.starter_message,
            archived: value.archived,
            locked: value.locked,
            tags: value.tags,
            message_count: value.message_count,
        }
    }
}

impl From<ThreadInformation> for crate::ThreadInformation {
    fn from(value: ThreadInformation) -> Self {
        crate::ThreadInformation {
            owner: value.owner,
            starter_message: value.starter_message,
            archived: value.archived,
            locked: value.locked,
            tags: value.tags,
            message_count: value.message_count,
        }
    }
}

impl From<crate::ForumInformation> for ForumInformation {
    fn from(value: crate::ForumInformation) -> Self {
        ForumInformation {
            tags: value.tags.into_iter().map(Into::into).collect(),
            require_tag: value.require_tag,
        }
    }
}

impl From<ForumInformation> for crate::ForumInformation {
    fn from(value: ForumInformation) -> Self {
        crate::ForumInformation {
            tags: value.tags.into_iter().map(Into::into).collect(),
            require_tag: value.require_tag,
        }
    }
}

impl From<crate::ForumTag> for ForumTag {
    fn from(value: crate::ForumTag) -> Self {
        ForumTag {
            id: value.id,
            name: value.name,
            emoji: value.emoji,
        }
    }
}

impl From<ForumTag> for crate::ForumTag {
    fn from(value: ForumTag) -> Self {
        crate::ForumTag {
            id: value.id,
            name: value.name,
            emoji: value.emoji,
        }
    }
}

impl From<crate::SoundboardSound> for SoundboardSound {
    fn from(value: crate::SoundboardSound) -> Self {
        SoundboardSound {
            id: value.id,
            server: value.server,
            creator_id: value.creator_id,
            name: value.name,
            emoji: value.emoji,
            duration_ms: value.duration_ms,
            volume: value.volume,
            library_id: value.library_id,
        }
    }
}
