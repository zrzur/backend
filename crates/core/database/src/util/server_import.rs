//! Reads a Discord server template (the document its owner makes in
//! Discord's server settings) into a Zrzur server layout.
//!
//! Pure: no network, no database. Everything about a person in the document
//! (`creator`, ids of the source server) is never looked at, so it can't be
//! stored or logged from here. The mapping fails closed: a role never gets
//! more than it had, a permission with no match is dropped, and a channel
//! whose permissions can't be read is hidden.

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;
use revolt_models::v0::{
    DataServerStructure, ImportReport, LegacyServerChannelType, StructureCategory,
    StructureChannel, StructureRole,
};
use revolt_permissions::{ChannelPermission as P, Override};
use serde_json::Value;

/// Largest template text that is read at all
pub const MAX_TEMPLATE_BYTES: usize = 1_000_000;
/// Most categories a layout holds
pub const MAX_CATEGORIES: usize = 50;
const NAME_LIMIT: usize = 32;
const DESCRIPTION_LIMIT: usize = 1024;
const SLOWMODE_LIMIT: u64 = 21_600;
const VOICE_LIMIT: u64 = 99;

/// All a role or a channel override of a layout may hold: everyday rights and
/// moderation. Control of the server itself (its settings, roles, permissions,
/// webhooks, customisation, join requests) stays with the owner, who can hand
/// it out afterwards; a new permission stays out until it is listed here.
pub static IMPORT_RIGHTS: Lazy<u64> = Lazy::new(|| {
    [
        P::ManageChannel,
        P::UseExternalEmojis,
        P::KickMembers,
        P::BanMembers,
        P::TimeoutMembers,
        P::AssignRoles,
        P::ChangeNickname,
        P::ManageNicknames,
        P::ChangeAvatar,
        P::ViewChannel,
        P::ReadMessageHistory,
        P::SendMessage,
        P::ManageMessages,
        P::InviteOthers,
        P::SendEmbeds,
        P::UploadFiles,
        P::React,
        P::BypassSlowmode,
        P::Connect,
        P::Speak,
        P::Video,
        P::MuteMembers,
        P::DeafenMembers,
        P::MoveMembers,
        P::Listen,
        P::MentionEveryone,
        P::MentionRoles,
        P::ViewAuditLogs,
        P::CreateThreads,
        P::SendMessageInThreads,
        P::CreateForumPosts,
    ]
    .into_iter()
    .fold(0, |rights, permission| rights | permission as u64)
});

/// What a Discord permission bit becomes
enum Maps {
    /// These permissions here
    To(&'static [P]),
    /// Control of the server: never given by an import, the role is named
    /// in the report. It may still carry a lesser right.
    Held(&'static [P]),
    /// Nothing here matches it
    Nothing,
}

/// Discord's permission bits by position, with their documented names
const DISCORD_BITS: &[(u32, &str, Maps)] = &[
    (0, "CREATE_INSTANT_INVITE", Maps::To(&[P::InviteOthers])),
    (1, "KICK_MEMBERS", Maps::To(&[P::KickMembers])),
    (2, "BAN_MEMBERS", Maps::To(&[P::BanMembers])),
    (3, "ADMINISTRATOR", Maps::Held(&[])),
    (4, "MANAGE_CHANNELS", Maps::To(&[P::ManageChannel])),
    (5, "MANAGE_GUILD", Maps::Held(&[])),
    (6, "ADD_REACTIONS", Maps::To(&[P::React])),
    (7, "VIEW_AUDIT_LOG", Maps::To(&[P::ViewAuditLogs])),
    (8, "PRIORITY_SPEAKER", Maps::Nothing),
    (9, "STREAM", Maps::To(&[P::Video])),
    (10, "VIEW_CHANNEL", Maps::To(&[P::ViewChannel])),
    (11, "SEND_MESSAGES", Maps::To(&[P::SendMessage, P::CreateForumPosts])),
    (12, "SEND_TTS_MESSAGES", Maps::Nothing),
    (13, "MANAGE_MESSAGES", Maps::To(&[P::ManageMessages])),
    (14, "EMBED_LINKS", Maps::To(&[P::SendEmbeds])),
    (15, "ATTACH_FILES", Maps::To(&[P::UploadFiles])),
    (16, "READ_MESSAGE_HISTORY", Maps::To(&[P::ReadMessageHistory])),
    (17, "MENTION_EVERYONE", Maps::To(&[P::MentionEveryone, P::MentionRoles])),
    (18, "USE_EXTERNAL_EMOJIS", Maps::To(&[P::UseExternalEmojis])),
    (19, "VIEW_GUILD_INSIGHTS", Maps::Nothing),
    (20, "CONNECT", Maps::To(&[P::Connect, P::Listen])),
    (21, "SPEAK", Maps::To(&[P::Speak])),
    (22, "MUTE_MEMBERS", Maps::To(&[P::MuteMembers])),
    (23, "DEAFEN_MEMBERS", Maps::To(&[P::DeafenMembers])),
    (24, "MOVE_MEMBERS", Maps::To(&[P::MoveMembers])),
    (25, "USE_VAD", Maps::Nothing),
    (26, "CHANGE_NICKNAME", Maps::To(&[P::ChangeNickname, P::ChangeAvatar])),
    (27, "MANAGE_NICKNAMES", Maps::To(&[P::ManageNicknames])),
    (28, "MANAGE_ROLES", Maps::Held(&[P::AssignRoles])),
    (29, "MANAGE_WEBHOOKS", Maps::Held(&[])),
    (30, "MANAGE_GUILD_EXPRESSIONS", Maps::Held(&[])),
    (31, "USE_APPLICATION_COMMANDS", Maps::Nothing),
    (32, "REQUEST_TO_SPEAK", Maps::Nothing),
    (33, "MANAGE_EVENTS", Maps::Nothing),
    (34, "MANAGE_THREADS", Maps::Nothing),
    (35, "CREATE_PUBLIC_THREADS", Maps::To(&[P::CreateThreads])),
    (36, "CREATE_PRIVATE_THREADS", Maps::Nothing),
    (37, "USE_EXTERNAL_STICKERS", Maps::Nothing),
    (38, "SEND_MESSAGES_IN_THREADS", Maps::To(&[P::SendMessageInThreads])),
    (39, "USE_EMBEDDED_ACTIVITIES", Maps::Nothing),
    (40, "MODERATE_MEMBERS", Maps::To(&[P::TimeoutMembers])),
    (41, "VIEW_CREATOR_MONETIZATION_ANALYTICS", Maps::Nothing),
    (42, "USE_SOUNDBOARD", Maps::Nothing),
    (43, "CREATE_GUILD_EXPRESSIONS", Maps::Nothing),
    (44, "CREATE_EVENTS", Maps::Nothing),
    (45, "USE_EXTERNAL_SOUNDS", Maps::Nothing),
    (46, "SEND_VOICE_MESSAGES", Maps::Nothing),
    (48, "SET_VOICE_CHANNEL_STATUS", Maps::Nothing),
    (49, "SEND_POLLS", Maps::Nothing),
    (50, "USE_EXTERNAL_APPS", Maps::Nothing),
    // not Manage Messages: that would also hand out deleting
    (51, "PIN_MESSAGES", Maps::Nothing),
    (52, "BYPASS_SLOWMODE", Maps::To(&[P::BypassSlowmode])),
];

/// One Discord permission set, mapped
#[derive(Default)]
struct Mapped {
    /// The rights it becomes here
    rights: u64,
    /// It held control of the server
    held: bool,
}

/// Why a template can't be read
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ImportError {
    /// Longer than `MAX_TEMPLATE_BYTES`
    TooLarge,
    /// Not a server template: other text, cut off, or the wrong document
    NotATemplate,
    /// A template with nothing in it to build
    Empty,
}

/// Why a pasted link can't be used
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LinkError {
    /// An invite link: the common mistake
    Invite,
    /// Neither a template link nor a template code
    NotALink,
}

/// A layout as read from a template
#[derive(Debug)]
pub struct Imported {
    /// The source server's name, cleaned and cut
    pub name: String,
    pub structure: DataServerStructure,
    pub report: ImportReport,
}

static RE_CODE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[A-Za-z0-9]{2,32}$").unwrap());

/// The template code inside what a person pasted: a template link in any of
/// its public forms, or the bare code
pub fn template_code(input: &str) -> Result<String, LinkError> {
    let text = input.trim();
    if text.len() > 300 || text.chars().any(char::is_whitespace) {
        return Err(LinkError::NotALink);
    }
    if RE_CODE.is_match(text) {
        return Ok(text.to_string());
    }
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest).trim_end_matches('/');
    let (host, path) = rest.split_once('/').ok_or(LinkError::NotALink)?;
    let host = host.to_ascii_lowercase();
    let code = match host.as_str() {
        "discord.new" => path,
        "discord.gg" => return Err(LinkError::Invite),
        "discord.com" | "discordapp.com" | "ptb.discord.com" | "canary.discord.com" => {
            if path.starts_with("invite/") {
                return Err(LinkError::Invite);
            }
            match path.strip_prefix("template/") {
                Some(code) => code,
                None => path
                    .strip_prefix("api/")
                    .and_then(|api| api.split_once("guilds/templates/"))
                    .map(|(_, code)| code)
                    .ok_or(LinkError::NotALink)?,
            }
        }
        _ => return Err(LinkError::NotALink),
    };
    if RE_CODE.is_match(code) {
        Ok(code.to_string())
    } else {
        Err(LinkError::NotALink)
    }
}

/// Characters a name never keeps: controls, and the marks that turn the
/// direction of the text that follows them
fn unwanted(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{200B}' | '\u{2060}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
        )
}

/// A name as it can be stored: cleaned, on one line, at most `limit`
/// characters. The flag says it was cut.
fn clean(text: &str, limit: usize) -> (String, bool) {
    let cleaned: String = text
        .chars()
        .map(|character| if character == '\n' || character == '\t' { ' ' } else { character })
        .filter(|character| !unwanted(*character))
        .collect();
    let cleaned = cleaned.trim();
    let cut: String = cleaned.chars().take(limit).collect();
    let was_cut = cut.chars().count() < cleaned.chars().count();
    (cut.trim_end().to_string(), was_cut)
}

/// A longer text (topic, description): line breaks stay
fn clean_text(text: &str, limit: usize) -> Option<String> {
    let cleaned: String = text
        .chars()
        .filter(|character| *character == '\n' || !unwanted(*character))
        .collect();
    let cut: String = cleaned.trim().chars().take(limit).collect();
    let cut = cut.trim_end();
    (!cut.is_empty()).then(|| cut.to_string())
}

/// An id as the template gives it: a small number, or a string
fn key_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) if !text.is_empty() && text.len() <= 24 => Some(text.clone()),
        _ => None,
    }
}

/// A permission set: a number, or a number in a string. `None` when it is
/// there but can't be read.
fn bits_of(value: Option<&Value>) -> Option<u64> {
    match value {
        None | Some(Value::Null) => Some(0),
        Some(Value::Number(number)) => number.as_u64(),
        // bits above 63 don't exist yet; a longer number keeps its low part
        Some(Value::String(text)) => text.trim().parse::<u128>().ok().map(|bits| bits as u64),
        _ => None,
    }
}

fn number_of(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// Maps one Discord permission set and notes what had no match
fn map_bits(bits: u64, unmatched: &mut Vec<String>) -> Mapped {
    let mut mapped = Mapped::default();
    let mut note = |name: String| {
        if !unmatched.contains(&name) {
            unmatched.push(name);
        }
    };
    for position in 0..64 {
        if bits & (1 << position) == 0 {
            continue;
        }
        let Some((_, name, maps)) = DISCORD_BITS.iter().find(|(bit, ..)| *bit == position) else {
            note(format!("UNKNOWN_{position}"));
            continue;
        };
        let rights = match maps {
            Maps::To(rights) => rights,
            Maps::Held(rights) => {
                mapped.held = true;
                rights
            }
            Maps::Nothing => {
                note(name.to_string());
                continue;
            }
        };
        for right in rights.iter() {
            mapped.rights |= *right as u64;
        }
    }
    mapped.rights &= *IMPORT_RIGHTS;
    mapped
}

/// The permissions of one channel or category
#[derive(Default, PartialEq, Eq, Clone)]
struct Rights {
    default: Option<Override>,
    roles: HashMap<String, Override>,
}

const HIDDEN: Override = Override { allow: 0, deny: P::ViewChannel as u64 };

/// Reads `permission_overwrites`. Anything that can't be read hides the
/// channel from everyone; chosen roles keep what could be read.
fn read_rights(
    channel: &Value,
    everyone: &str,
    roles: &HashSet<String>,
    report: &mut ImportReport,
) -> Rights {
    let mut rights = Rights::default();
    let entries = match channel.get("permission_overwrites") {
        None | Some(Value::Null) => return rights,
        Some(Value::Array(entries)) => entries,
        Some(_) => {
            rights.default = Some(HIDDEN);
            return rights;
        }
    };
    let mut unreadable = false;
    for entry in entries {
        let member = match entry.get("type") {
            Some(Value::Number(kind)) => kind.as_u64() == Some(1),
            Some(Value::String(kind)) => kind == "member" || kind == "1",
            _ => false,
        };
        if member {
            report.member_overrides += 1;
            continue;
        }
        let (Some(id), Some(allow), Some(deny)) = (
            key_of(entry.get("id")),
            bits_of(entry.get("allow")),
            bits_of(entry.get("deny")),
        ) else {
            unreadable = true;
            continue;
        };
        let allow = map_bits(allow, &mut report.unmatched_permissions).rights;
        // a deny wins where both name the same right
        let deny = map_bits(deny, &mut report.unmatched_permissions).rights;
        let field = Override { allow: allow & !deny, deny };
        if field.allow == 0 && field.deny == 0 {
            continue;
        }
        if id == everyone {
            rights.default = Some(field);
        } else if roles.contains(&id) {
            rights.roles.insert(format!("r{id}"), field);
        }
    }
    if unreadable {
        let mut field = rights.default.take().unwrap_or_default();
        field.allow &= !(P::ViewChannel as u64);
        field.deny |= P::ViewChannel as u64;
        rights.default = Some(field);
    }
    rights
}

/// The limits of the server the layout is built on
#[derive(Clone, Copy, Debug)]
pub struct ImportLimits {
    pub channels: usize,
    pub roles: usize,
}

/// Read a template document into a layout
pub fn read_template(text: &str, limits: ImportLimits) -> Result<Imported, ImportError> {
    if text.len() > MAX_TEMPLATE_BYTES {
        return Err(ImportError::TooLarge);
    }
    let document: Value = serde_json::from_str(text).map_err(|_| ImportError::NotATemplate)?;
    // the whole template, or the server part of it on its own
    let guild = match document.get("serialized_source_guild") {
        Some(guild) => guild,
        None => &document,
    };
    let (Some(roles), Some(channels)) = (
        guild.get("roles").and_then(Value::as_array),
        guild.get("channels").and_then(Value::as_array),
    ) else {
        return Err(ImportError::NotATemplate);
    };

    let mut report = ImportReport::default();
    fn note_cut(report: &mut ImportReport, name: &str, cut: bool) {
        if cut && report.shortened.len() < 50 {
            report.shortened.push(name.to_string());
        }
    }

    // --- roles
    let is_everyone = |role: &Value| {
        role.get("name").and_then(Value::as_str) == Some("@everyone")
            || matches!(role.get("id"), Some(Value::Number(id)) if id.as_u64() == Some(0))
    };
    let everyone = roles.iter().find(|role| is_everyone(role));
    let everyone_key = everyone
        .and_then(|role| key_of(role.get("id")))
        .unwrap_or_else(|| "0".to_string());
    let default_permissions = everyone
        .and_then(|role| bits_of(role.get("permissions")))
        .map(|bits| map_bits(bits, &mut report.unmatched_permissions).rights);

    let mut listed: Vec<(usize, &Value)> = roles
        .iter()
        .enumerate()
        .filter(|(_, role)| role.is_object() && !is_everyone(role))
        // a role a bot owns means nothing without the bot
        .filter(|(_, role)| {
            role.get("managed").and_then(Value::as_bool) != Some(true)
                && role.get("tags").and_then(|tags| tags.get("bot_id")).is_none()
        })
        .collect();
    // highest first: by position where the template gives one, else the
    // template's own order, which runs from the lowest role up
    let positioned = listed.iter().all(|(_, role)| number_of(role.get("position")).is_some());
    if positioned {
        listed.sort_by_key(|(index, role)| {
            (std::cmp::Reverse(number_of(role.get("position")).unwrap_or(0)), std::cmp::Reverse(*index))
        });
    } else {
        listed.reverse();
    }
    report.over_limit_roles = listed.len().saturating_sub(limits.roles);
    listed.truncate(limits.roles);

    let mut role_keys = HashSet::new();
    let mut built_roles = Vec::with_capacity(listed.len());
    for (index, role) in listed {
        let Some(id) = key_of(role.get("id")) else { continue };
        if id == everyone_key || !role_keys.insert(id.clone()) {
            continue;
        }
        let (name, cut) = clean(role.get("name").and_then(Value::as_str).unwrap_or(""), NAME_LIMIT);
        let name = if name.is_empty() { format!("role-{}", index + 1) } else { name };
        note_cut(&mut report, &name, cut);
        let mapped = map_bits(
            bits_of(role.get("permissions")).unwrap_or(0),
            &mut report.unmatched_permissions,
        );
        if mapped.held {
            report.held_back.push(name.clone());
        }
        let colour = number_of(role.get("color"))
            .filter(|colour| *colour > 0 && *colour <= 0xFF_FFFF)
            .map(|colour| format!("#{colour:06x}"));
        built_roles.push(StructureRole {
            key: format!("r{id}"),
            name,
            colour,
            hoist: role.get("hoist").and_then(Value::as_bool).unwrap_or(false),
            allow: mapped.rights,
        });
    }

    // --- channels
    struct Room<'a> {
        index: usize,
        value: &'a Value,
        kind: u64,
        position: u64,
    }
    let rooms: Vec<Room> = channels
        .iter()
        .enumerate()
        .filter(|(_, channel)| channel.is_object())
        .map(|(index, value)| Room {
            index,
            value,
            kind: number_of(value.get("type")).unwrap_or(u64::MAX),
            position: number_of(value.get("position")).unwrap_or(u64::MAX),
        })
        .collect();

    let mut category_rooms: Vec<&Room> = rooms.iter().filter(|room| room.kind == 4).collect();
    category_rooms.sort_by_key(|room| (room.position, room.index));
    let category_ids: Vec<Option<String>> =
        category_rooms.iter().map(|room| key_of(room.value.get("id"))).collect();

    // text first, then voice, each by position: the order Discord shows
    let mut others: Vec<&Room> = rooms.iter().filter(|room| room.kind != 4).collect();
    others.sort_by_key(|room| (room.kind == 2 || room.kind == 13, room.position, room.index));

    let mut budget = limits.channels;
    let mut seen = HashSet::new();
    let mut system_channel = None;
    let system_id = key_of(guild.get("system_channel_id"));
    let mut build = |room: &Room,
                     parent: Option<&Rights>,
                     report: &mut ImportReport|
     -> Option<StructureChannel> {
        let raw = room.value.get("name").and_then(Value::as_str).unwrap_or("");
        let (name, cut) = clean(raw, NAME_LIMIT);
        let name = if name.is_empty() { format!("channel-{}", room.index + 1) } else { name };
        let (channel_type, announcement) = match room.kind {
            0 => (LegacyServerChannelType::Text, false),
            5 => (LegacyServerChannelType::Text, true),
            2 => (LegacyServerChannelType::Voice, false),
            15 => (LegacyServerChannelType::Forum, false),
            _ => {
                if report.skipped_channels.len() < 50 {
                    report.skipped_channels.push(name);
                }
                return None;
            }
        };
        let id = key_of(room.value.get("id")).unwrap_or_else(|| format!("n{}", room.index));
        if !seen.insert(id.clone()) {
            return None;
        }
        if budget == 0 {
            report.over_limit_channels += 1;
            return None;
        }
        budget -= 1;
        note_cut(report, &name, cut);

        let rights = read_rights(room.value, &everyone_key, &role_keys, report);
        let hidden = |rights: &Rights| {
            rights.default.as_ref().is_some_and(|field| field.deny & P::ViewChannel as u64 != 0)
        };
        let synced = parent.is_some_and(|parent| *parent == rights);
        if hidden(&rights) {
            report.private_channels += 1;
        }
        let nsfw = room.value.get("nsfw").and_then(Value::as_bool).unwrap_or(false);
        if nsfw {
            report.mature_channels += 1;
        }
        let voice = matches!(channel_type, LegacyServerChannelType::Voice);
        if !voice && !announcement && system_id.as_deref() == Some(id.as_str()) && room.kind == 0 {
            system_channel = Some(format!("c{id}"));
        }
        Some(StructureChannel {
            key: format!("c{id}"),
            channel_type,
            name,
            description: room
                .value
                .get("topic")
                .and_then(Value::as_str)
                .and_then(|topic| clean_text(topic, DESCRIPTION_LIMIT)),
            nsfw,
            announcement,
            slowmode: (!voice)
                .then(|| number_of(room.value.get("rate_limit_per_user")))
                .flatten()
                .filter(|seconds| *seconds > 0)
                .map(|seconds| seconds.min(SLOWMODE_LIMIT)),
            max_users: voice
                .then(|| number_of(room.value.get("user_limit")))
                .flatten()
                .filter(|people| *people > 0)
                .map(|people| people.min(VOICE_LIMIT) as usize),
            synced,
            default_permissions: if synced { None } else { rights.default },
            role_permissions: if synced { HashMap::new() } else { rights.roles },
        })
    };

    let parent_of = |room: &Room| {
        key_of(room.value.get("parent_id"))
            .and_then(|parent| category_ids.iter().position(|id| id.as_ref() == Some(&parent)))
            .filter(|position| *position < MAX_CATEGORIES)
    };

    let loose: Vec<StructureChannel> = others
        .iter()
        .copied()
        .filter(|room| parent_of(room).is_none())
        .filter_map(|room| build(room, None, &mut report))
        .collect();

    let mut categories = Vec::new();
    for (position, category) in category_rooms.iter().enumerate().take(MAX_CATEGORIES) {
        let raw = category.value.get("name").and_then(Value::as_str).unwrap_or("");
        let (title, cut) = clean(raw, NAME_LIMIT);
        let title = if title.is_empty() { format!("category-{}", position + 1) } else { title };
        note_cut(&mut report, &title, cut);
        let rights = read_rights(category.value, &everyone_key, &role_keys, &mut report);
        let channels: Vec<StructureChannel> = others
            .iter()
            .copied()
            .filter(|room| parent_of(room) == Some(position))
            .filter_map(|room| build(room, Some(&rights), &mut report))
            .collect();
        categories.push(StructureCategory {
            title,
            channels,
            default_permissions: rights.default,
            role_permissions: rights.roles,
        });
    }
    // channels of categories beyond the limit were counted as loose above

    let count: usize = loose.len() + categories.iter().map(|c| c.channels.len()).sum::<usize>();
    if count == 0 && built_roles.is_empty() {
        return Err(ImportError::Empty);
    }
    // a category that ended up empty only because its channels can't be
    // built says nothing: keep those the owner left empty on purpose too,
    // they cost nothing
    report.categories = categories.len();
    report.channels = count;
    report.roles = built_roles.len();
    report.unmatched_permissions.sort();

    let (name, _) = clean(guild.get("name").and_then(Value::as_str).unwrap_or(""), NAME_LIMIT);
    Ok(Imported {
        name,
        structure: DataServerStructure {
            description: guild
                .get("description")
                .and_then(Value::as_str)
                .and_then(|text| clean_text(text, DESCRIPTION_LIMIT)),
            default_permissions,
            channels: loose,
            categories,
            roles: built_roles,
            system_channel,
        },
        report,
    })
}

/// Why a layout can't be built as it stands. The preview only ever returns
/// layouts that pass; this is for one a client changed or wrote itself.
pub fn check_structure(structure: &DataServerStructure, limits: ImportLimits) -> Result<(), &'static str> {
    let rooms = || {
        structure
            .channels
            .iter()
            .chain(structure.categories.iter().flat_map(|category| &category.channels))
    };
    if rooms().count() > limits.channels {
        return Err("too many channels");
    }
    if structure.roles.len() > limits.roles {
        return Err("too many roles");
    }
    if structure.categories.len() > MAX_CATEGORIES {
        return Err("too many categories");
    }
    let mut role_keys = HashSet::new();
    for role in &structure.roles {
        if !role_keys.insert(role.key.as_str()) {
            return Err("two roles share a key");
        }
        if role.allow & !*IMPORT_RIGHTS != 0 {
            return Err("a role in a layout can't hold control of the server");
        }
    }
    if structure.default_permissions.is_some_and(|rights| rights & !*IMPORT_RIGHTS != 0) {
        return Err("the default permissions can't hold control of the server");
    }
    let field_ok = |field: &Override| (field.allow | field.deny) & !*IMPORT_RIGHTS == 0;
    let rights_ok = |default: &Option<Override>, roles: &HashMap<String, Override>| {
        default.as_ref().is_none_or(field_ok)
            && roles
                .iter()
                .all(|(key, field)| role_keys.contains(key.as_str()) && field_ok(field))
    };
    let mut channel_keys = HashSet::new();
    for room in rooms() {
        if !channel_keys.insert(room.key.as_str()) {
            return Err("two channels share a key");
        }
        if !rights_ok(&room.default_permissions, &room.role_permissions) {
            return Err("a channel names an unknown role or a right it can't hold");
        }
        let voice = matches!(room.channel_type, LegacyServerChannelType::Voice);
        if room.announcement && !matches!(room.channel_type, LegacyServerChannelType::Text) {
            return Err("an announcement channel is a text channel");
        }
        if room.max_users.is_some() && !voice {
            return Err("only a voice channel has a limit of people");
        }
        if room.name.chars().any(unwanted) {
            return Err("a channel name holds characters it can't keep");
        }
    }
    if structure.channels.iter().any(|room| room.synced) {
        return Err("a channel outside a category has nothing to follow");
    }
    for category in &structure.categories {
        if !rights_ok(&category.default_permissions, &category.role_permissions) {
            return Err("a category names an unknown role or a right it can't hold");
        }
        if category.title.chars().any(unwanted) {
            return Err("a category title holds characters it can't keep");
        }
    }
    if structure.roles.iter().any(|role| role.name.chars().any(unwanted)) {
        return Err("a role name holds characters it can't keep");
    }
    if let Some(key) = &structure.system_channel {
        if !channel_keys.contains(key.as_str()) {
            return Err("the system channel is not in the layout");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "server_import_tests.rs"]
mod tests;
