//! Per-target checks and evidence snapshots.
//!
//! Each target kind proves the reporter could already see it. Anything the
//! reporter can't see answers `NotFound`, exactly like a target that does
//! not exist, so reporting can't be used to probe private servers,
//! channels or DMs. Evidence is a bounded snapshot (the item and at most
//! three messages either side), never a whole conversation.

use std::collections::BTreeMap;

use revolt_database::{
    util::permissions::DatabasePermissionQuery, Bot, Channel, CommunityItem, CommunityItemStatus,
    Database, File, Invite, Message, MessageFilter, MessageQuery, MessageTimePeriod,
    Server, User,
    SAFETY_EVIDENCE_RETENTION_MS,
};
use revolt_models::v0::{
    DataSafetyTarget, MessageSort, SafetyContextMessage, SafetyEvidence, SafetyEvidenceFile,
    SafetyProfileFacet, SafetyTarget, SafetyTargetKind,
};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use ulid::Ulid;

/// Messages kept either side of a reported message
const CONTEXT_EACH_SIDE: i64 = 3;
const CONTENT_MAX: usize = 4000;
const CONTEXT_CONTENT_MAX: usize = 500;

/// A target that passed its checks, with what to store
pub struct ResolvedTarget {
    pub target: SafetyTarget,
    pub owner_id: Option<String>,
    pub server_id: Option<String>,
    pub channel_id: Option<String>,
    pub channel_type: Option<String>,
    /// Evidence without `case_id` (filled in once the case exists)
    pub evidence: Vec<SafetyEvidence>,
    /// Channel of a reported message (extra evidence must come from it)
    pub channel: Option<Channel>,
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn not_found<T>(_: T) -> revolt_result::Error {
    create_error!(NotFound)
}

fn evidence(kind: SafetyTargetKind, object_id: &str, now: i64) -> SafetyEvidence {
    SafetyEvidence {
        id: Ulid::new().to_string(),
        case_id: String::new(),
        kind,
        object_id: object_id.to_string(),
        captured_at: now,
        author_id: None,
        title: None,
        content: None,
        files: vec![],
        context: vec![],
        metadata: BTreeMap::new(),
        expires_at: Some(now + SAFETY_EVIDENCE_RETENTION_MS),
        legal_hold: false,
    }
}

pub(crate) fn file_ref(file: &File) -> SafetyEvidenceFile {
    SafetyEvidenceFile {
        id: file.id.clone(),
        tag: file.tag.clone(),
        filename: truncate(&file.filename, 128),
        content_type: file.content_type.clone(),
        size: file.size,
    }
}

fn channel_type(channel: &Channel) -> &'static str {
    match channel {
        Channel::SavedMessages { .. } => "SavedMessages",
        Channel::DirectMessage { .. } => "DirectMessage",
        Channel::Group { .. } => "Group",
        _ => "ServerChannel",
    }
}

/// A message the user may read (ViewChannel + ReadMessageHistory)
pub async fn visible_message(db: &Database, user: &User, id: &str) -> Result<(Message, Channel)> {
    let message = db.fetch_message(id).await.map_err(not_found)?;
    let channel = db.fetch_channel(&message.channel).await.map_err(not_found)?;
    can_read(db, user, &channel).await?;
    Ok((message, channel))
}

async fn can_read(db: &Database, user: &User, channel: &Channel) -> Result<()> {
    let mut query = DatabasePermissionQuery::new(db, user).channel(channel);
    let perms = calculate_channel_permissions(&mut query).await;
    if perms.has_channel_permission(ChannelPermission::ViewChannel)
        && perms.has_channel_permission(ChannelPermission::ReadMessageHistory)
    {
        Ok(())
    } else {
        Err(create_error!(NotFound))
    }
}

async fn is_member(db: &Database, server_id: &str, user_id: &str) -> bool {
    db.fetch_member(server_id, user_id).await.is_ok()
}

/// The account that answers for a message: who sent it, the creator of the
/// webhook, or the owner of the bot. `None` once the webhook is gone. A webhook
/// or a bot cannot be warned, suspended or told anything.
pub(crate) async fn message_owner(db: &Database, message: &Message) -> Option<String> {
    if message.webhook.is_some() {
        return db.fetch_webhook(&message.author).await.ok().map(|webhook| webhook.creator_id);
    }
    match db.fetch_user(&message.author).await {
        Ok(User { bot: Some(bot), .. }) => Some(bot.owner),
        _ => Some(message.author.clone()),
    }
}

/// A reported file on its own, when no message holds it any more
pub(crate) fn file_evidence(file: &File, now: i64) -> SafetyEvidence {
    let mut snap = evidence(SafetyTargetKind::Attachment, &file.id, now);
    snap.author_id = file.uploader_id.clone();
    snap.title = Some(truncate(&file.filename, 64));
    snap.files = vec![file_ref(file)];
    snap
}

/// Name, description and pictures of a server or a Discover listing
pub(crate) fn server_evidence(server: &Server, kind: SafetyTargetKind, now: i64) -> SafetyEvidence {
    let mut snap = evidence(kind, &server.id, now);
    snap.author_id = Some(server.owner.clone());
    snap.title = Some(truncate(&server.name, 64));
    snap.content = server.description.as_deref().map(|d| truncate(d, CONTENT_MAX));
    snap.files = [server.icon.as_ref(), server.banner.as_ref()]
        .into_iter()
        .flatten()
        .map(file_ref)
        .collect();
    snap.metadata.insert("discoverable".into(), server.discoverable.to_string());
    snap
}

/// The public profile of a bot and its listing details
pub(crate) fn bot_evidence(bot: &Bot, bot_user: &User, kind: SafetyTargetKind, now: i64) -> SafetyEvidence {
    let mut snap = user_evidence(bot_user, None, now);
    snap.kind = kind;
    snap.metadata.insert("public".into(), bot.public.to_string());
    snap.metadata.insert("discoverable".into(), bot.discoverable.to_string());
    if !bot.terms_of_service_url.is_empty() {
        snap.metadata.insert("terms_url".into(), truncate(&bot.terms_of_service_url, 256));
    }
    if !bot.privacy_policy_url.is_empty() {
        snap.metadata.insert("privacy_url".into(), truncate(&bot.privacy_policy_url, 256));
    }
    snap
}

/// A design from the community gallery
pub(crate) fn community_item_evidence(item: &CommunityItem, now: i64) -> SafetyEvidence {
    let mut snap = evidence(SafetyTargetKind::CommunityItem, &item.id, now);
    snap.author_id = Some(item.creator_id.clone());
    snap.title = Some(item.name.clone());
    snap.content = item.description.clone();
    snap.files = vec![file_ref(&item.file)];
    snap.metadata.insert("kind".into(), format!("{:?}", item.kind));
    snap.metadata.insert("revision".into(), item.revision.to_string());
    snap
}

/// Snapshot of one message, with a few neighbours from the same channel
pub async fn message_evidence(db: &Database, message: &Message, now: i64) -> Result<SafetyEvidence> {
    let mut snap = evidence(SafetyTargetKind::Message, &message.id, now);
    snap.author_id = Some(message.author.clone());
    snap.content = message.content.as_deref().map(|c| truncate(c, CONTENT_MAX));
    snap.files = message
        .attachments
        .iter()
        .flatten()
        .map(file_ref)
        .collect();
    snap.metadata.insert("channel_id".into(), message.channel.clone());
    if let Some(edited) = &message.edited {
        snap.metadata.insert("edited".into(), edited.format().to_string());
    }
    if let Some(webhook) = &message.webhook {
        snap.metadata.insert("webhook_name".into(), truncate(&webhook.name, 64));
    }
    if let Some(masquerade) = message.masquerade.as_ref().and_then(|m| m.name.as_ref()) {
        snap.metadata.insert("masquerade_name".into(), truncate(masquerade, 64));
    }
    if let Some(embeds) = &message.embeds {
        let links: Vec<String> = embeds
            .iter()
            .filter_map(|embed| match embed {
                revolt_models::v0::Embed::Website(site) => site.url.clone(),
                _ => None,
            })
            .take(5)
            .collect();
        if !links.is_empty() {
            snap.metadata.insert("links".into(), links.join(" "));
        }
    }

    for (before, after) in [(Some(message.id.clone()), None), (None, Some(message.id.clone()))] {
        let sort = if before.is_some() {
            MessageSort::Latest
        } else {
            MessageSort::Oldest
        };
        let messages = db
            .fetch_messages(MessageQuery {
                filter: MessageFilter {
                    channel: Some(message.channel.clone()),
                    ..Default::default()
                },
                limit: Some(CONTEXT_EACH_SIDE),
                time_period: MessageTimePeriod::Absolute {
                    before,
                    after,
                    sort: Some(sort),
                },
            })
            .await
            .unwrap_or_default();
        snap.context.extend(messages.into_iter().map(|m| SafetyContextMessage {
            content: m
                .content
                .as_deref()
                .map(|c| truncate(c, CONTEXT_CONTENT_MAX))
                .unwrap_or_default(),
            attachments: m.attachments.as_ref().map(Vec::len).unwrap_or_default(),
            id: m.id,
            author_id: m.author,
        }));
    }
    snap.context.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(snap)
}

/// Public profile fields of a user. Never relations, email or settings.
/// The copy taken when a comment is reported. It stays with the report under
/// the reports rule, whatever happens to the comment afterwards.

pub(crate) fn user_evidence(user: &User, facet: Option<SafetyProfileFacet>, now: i64) -> SafetyEvidence {
    let mut snap = evidence(SafetyTargetKind::User, &user.id, now);
    snap.author_id = Some(user.id.clone());
    snap.title = Some(format!("{}#{}", user.username, user.discriminator));
    snap.content = user
        .profile
        .as_ref()
        .and_then(|p| p.content.as_deref())
        .map(|c| truncate(c, CONTENT_MAX));
    if let Some(name) = &user.display_name {
        snap.metadata.insert("display_name".into(), truncate(name, 64));
    }
    if let Some(text) = user.status.as_ref().and_then(|s| s.text.as_ref()) {
        snap.metadata.insert("status".into(), truncate(text, 128));
    }
    if let Some(profile) = &user.profile {
        if !profile.links.is_empty() {
            snap.metadata.insert("links".into(), profile.links.join(" "));
        }
        if !profile.interests.is_empty() {
            snap.metadata.insert("interests".into(), profile.interests.join(" "));
        }
    }
    if let Some(effect) = user.style.as_ref().and_then(|s| s.effect.as_ref()) {
        snap.metadata.insert("effect".into(), effect.clone());
    }
    if let Some(facet) = facet {
        snap.metadata.insert("facet".into(), format!("{facet:?}"));
    }
    if user.bot.is_some() {
        snap.metadata.insert("bot".into(), "true".into());
    }
    snap.files = [
        user.avatar.as_ref(),
        user.profile.as_ref().and_then(|p| p.background.as_ref()),
    ]
    .into_iter()
    .flatten()
    .map(file_ref)
    .collect();
    snap
}

/// Check a target and capture evidence for it
pub async fn resolve(
    db: &Database,
    reporter: &User,
    input: &DataSafetyTarget,
    now: i64,
) -> Result<ResolvedTarget> {
    let id = input.id.trim();
    if id.is_empty() || id.len() > 128 {
        return Err(create_error!(NotFound));
    }

    let mut resolved = ResolvedTarget {
        target: SafetyTarget {
            kind: input.kind,
            id: id.to_string(),
            facet: None,
            message_id: None,
        },
        owner_id: None,
        server_id: None,
        channel_id: None,
        channel_type: None,
        evidence: vec![],
        channel: None,
    };

    match input.kind {
        SafetyTargetKind::Message => {
            let (message, channel) = visible_message(db, reporter, id).await?;
            let owner = message_owner(db, &message).await;
            if message.author == reporter.id || owner.as_deref() == Some(reporter.id.as_str()) {
                return Err(create_error!(CannotReportYourself));
            }
            if message.system.is_some() {
                return Err(create_error!(InvalidOperation));
            }
            resolved.owner_id = owner;
            resolved.evidence.push(message_evidence(db, &message, now).await?);
            set_channel(&mut resolved, channel);
        }

        SafetyTargetKind::Attachment | SafetyTargetKind::Webhook => {
            let message_id = input.message_id.as_deref().ok_or_else(|| create_error!(NotFound))?;
            let (message, channel) = visible_message(db, reporter, message_id).await?;
            if input.kind == SafetyTargetKind::Attachment {
                let file = message
                    .attachments
                    .iter()
                    .flatten()
                    .find(|file| file.id == id)
                    .ok_or_else(|| create_error!(NotFound))?;
                let owner = message_owner(db, &message).await;
                if message.author == reporter.id || owner.as_deref() == Some(reporter.id.as_str()) {
                    return Err(create_error!(CannotReportYourself));
                }
                resolved.owner_id = owner;
                let mut snap = message_evidence(db, &message, now).await?;
                snap.kind = SafetyTargetKind::Attachment;
                snap.files = vec![file_ref(file)];
                resolved.evidence.push(snap);
            } else {
                // a webhook message's author is the webhook
                if message.webhook.is_none() || message.author != id {
                    return Err(create_error!(NotFound));
                }
                let webhook = db.fetch_webhook(id).await.ok();
                if webhook.as_ref().is_some_and(|w| w.creator_id == reporter.id) {
                    return Err(create_error!(CannotReportYourself));
                }
                resolved.owner_id = webhook.map(|w| w.creator_id);
                let mut snap = message_evidence(db, &message, now).await?;
                snap.kind = SafetyTargetKind::Webhook;
                resolved.evidence.push(snap);
            }
            resolved.target.message_id = Some(message.id.clone());
            set_channel(&mut resolved, channel);
        }

        SafetyTargetKind::User => {
            let user = db.fetch_user(id).await.map_err(not_found)?;
            if user.id == reporter.id {
                return Err(create_error!(CannotReportYourself));
            }
            resolved.target.facet = input.facet;
            resolved.owner_id = Some(user.id.clone());
            let mut snap = user_evidence(&user, input.facet, now);
            // what the profile shows from a linked Steam account, so staff can
            // judge the games and achievements a report is about
            if let Ok(Some(steam)) = db.fetch_linked_account(&user.id, "steam").await {
                if steam.shown && !steam.private {
                    let games: Vec<&str> = steam
                        .games
                        .iter()
                        .filter(|g| !steam.hidden_titles.contains(&g.appid))
                        .map(|g| g.name.as_str())
                        .collect();
                    let achievements: Vec<&str> = steam
                        .achievements
                        .iter()
                        .filter(|a| !steam.hidden_titles.contains(&a.appid))
                        .map(|a| a.name.as_str())
                        .collect();
                    snap.metadata.insert(
                        "steam".into(),
                        truncate(
                            &format!(
                                "{} | games: {} | achievements: {}",
                                steam.display_name,
                                games.join(", "),
                                achievements.join(", ")
                            ),
                            1000,
                        ),
                    );
                }
            }
            resolved.evidence.push(snap);
            // a message the user sent, as context (only if the reporter can read it)
            if let Some(message_id) = &input.message_id {
                let (message, channel) = visible_message(db, reporter, message_id).await?;
                if message.author != user.id {
                    return Err(create_error!(NotFound));
                }
                resolved.target.message_id = Some(message.id.clone());
                resolved.evidence.push(message_evidence(db, &message, now).await?);
                set_channel(&mut resolved, channel);
            }
        }

        SafetyTargetKind::Server | SafetyTargetKind::DiscoverServer => {
            let server = db.fetch_server(id).await.map_err(not_found)?;
            let member = is_member(db, &server.id, &reporter.id).await;
            let visible = if input.kind == SafetyTargetKind::DiscoverServer {
                server.discoverable
            } else {
                member || server.discoverable
            };
            if !visible {
                return Err(create_error!(NotFound));
            }
            if server.owner == reporter.id {
                return Err(create_error!(CannotReportYourself));
            }
            resolved.owner_id = Some(server.owner.clone());
            resolved.server_id = Some(server.id.clone());
            let mut snap = server_evidence(&server, input.kind, now);
            snap.metadata.insert("reporter_is_member".into(), member.to_string());
            resolved.evidence.push(snap);
        }

        SafetyTargetKind::Channel => {
            let channel = db.fetch_channel(id).await.map_err(not_found)?;
            let Some(server_id) = channel.server().map(str::to_string) else {
                // DMs and groups are reported through their messages or members
                return Err(create_error!(NotFound));
            };
            let mut query = DatabasePermissionQuery::new(db, reporter).channel(&channel);
            if !calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::ViewChannel)
            {
                return Err(create_error!(NotFound));
            }
            let server = db.fetch_server(&server_id).await.map_err(not_found)?;
            resolved.owner_id = Some(server.owner.clone());
            let mut snap = evidence(SafetyTargetKind::Channel, channel.id(), now);
            if let Channel::TextChannel {
                name, description, ..
            } = &channel
            {
                snap.title = Some(truncate(name, 64));
                snap.content = description.as_deref().map(|d| truncate(d, CONTENT_MAX));
            }
            snap.metadata.insert("server_id".into(), server_id);
            resolved.evidence.push(snap);
            set_channel(&mut resolved, channel);
        }

        SafetyTargetKind::Bot | SafetyTargetKind::Application | SafetyTargetKind::DiscoverBot => {
            let bot = db.fetch_bot(id).await.map_err(not_found)?;
            let bot_user = db.fetch_user(&bot.id).await.map_err(not_found)?;
            if bot.owner == reporter.id {
                return Err(create_error!(CannotReportYourself));
            }
            let visible = if input.kind == SafetyTargetKind::DiscoverBot {
                bot.discoverable
            } else if bot.public || bot.discoverable {
                true
            } else if let Some(server_id) = &input.server_id {
                // a private bot is visible to people who share a server with it
                is_member(db, server_id, &reporter.id).await && is_member(db, server_id, &bot.id).await
            } else if let Some(message_id) = &input.message_id {
                // ... or who can read a message it sent
                visible_message(db, reporter, message_id)
                    .await
                    .is_ok_and(|(message, _)| message.author == bot.id)
            } else {
                false
            };
            if !visible {
                return Err(create_error!(NotFound));
            }
            resolved.owner_id = Some(bot.owner.clone());
            resolved.evidence.push(bot_evidence(&bot, &bot_user, input.kind, now));
            if let Some(message_id) = &input.message_id
                && let Ok((message, channel)) = visible_message(db, reporter, message_id).await
                && message.author == bot.id
            {
                resolved.target.message_id = Some(message.id.clone());
                resolved.evidence.push(message_evidence(db, &message, now).await?);
                set_channel(&mut resolved, channel);
            }
        }

        SafetyTargetKind::CommunityItem => {
            let item = db.fetch_community_item(id).await.map_err(not_found)?;
            if item.status != CommunityItemStatus::Published {
                return Err(create_error!(NotFound));
            }
            if item.creator_id == reporter.id {
                return Err(create_error!(CannotReportYourself));
            }
            resolved.owner_id = Some(item.creator_id.clone());
            resolved.evidence.push(community_item_evidence(&item, now));
        }


        SafetyTargetKind::Invite => {
            let invite = db.fetch_invite(id).await.map_err(not_found)?;
            let mut snap = evidence(SafetyTargetKind::Invite, id, now);
            let (creator, server_id) = match &invite {
                Invite::Server {
                    creator, server, ..
                } => (creator.clone(), Some(server.clone())),
                Invite::Group { creator, .. } => (creator.clone(), None),
            };
            if creator == reporter.id {
                return Err(create_error!(CannotReportYourself));
            }
            if let Some(server_id) = &server_id
                && let Ok(server) = db.fetch_server(server_id).await
            {
                snap.title = Some(truncate(&server.name, 64));
                snap.content = server.description.as_deref().map(|d| truncate(d, CONTENT_MAX));
                snap.metadata.insert("server_id".into(), server.id.clone());
            }
            snap.author_id = Some(creator.clone());
            resolved.owner_id = Some(creator);
            resolved.server_id = server_id;
            resolved.evidence.push(snap);
        }
    }

    Ok(resolved)
}

fn set_channel(resolved: &mut ResolvedTarget, channel: Channel) {
    resolved.channel_id = Some(channel.id().to_string());
    resolved.channel_type = Some(channel_type(&channel).to_string());
    if resolved.server_id.is_none() {
        resolved.server_id = channel.server().map(str::to_string);
    }
    resolved.channel = Some(channel);
}

/// Extra messages offered as evidence: each must be readable by the
/// reporter and come from the reported message's channel
pub async fn extra_message_evidence(
    db: &Database,
    reporter: &User,
    channel: Option<&Channel>,
    ids: &[String],
    now: i64,
) -> Result<Vec<SafetyEvidence>> {
    let mut out = vec![];
    if ids.is_empty() {
        return Ok(out);
    }
    let channel = channel.ok_or_else(|| create_error!(InvalidOperation))?;
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let (message, _) = visible_message(db, reporter, id).await?;
        if message.channel != channel.id() {
            return Err(create_error!(NotFound));
        }
        let mut snap = message_evidence(db, &message, now).await?;
        // neighbours of extra messages would multiply the copied context
        snap.context.clear();
        out.push(snap);
    }
    Ok(out)
}
