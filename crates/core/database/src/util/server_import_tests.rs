use super::*;
use serde_json::{json, Value};

const LIMITS: ImportLimits = ImportLimits { channels: 200, roles: 200 };

fn bit(position: u32) -> u64 {
    1 << position
}
fn has(rights: u64, permission: P) -> bool {
    rights & permission as u64 != 0
}
fn read(template: Value) -> Imported {
    read_template(&template.to_string(), LIMITS).expect("a template that reads")
}

/// A community as an owner would have it: Arabic and English names, emoji,
/// private staff rooms, read-only rooms, voice rooms with limits
fn community() -> Value {
    let view = bit(10).to_string();
    let send = bit(11).to_string();
    json!({
        "code": "hgM48av5Q69A",
        "name": "Night Owls template",
        "description": "copy of our server",
        "usage_count": 3,
        "creator_id": "132837293881950208",
        "creator": { "id": "132837293881950208", "username": "hoges", "avatar": "79b0d9f8c340f2d43e1f78b09f175b62" },
        "created_at": "2020-04-02T21:10:38+00:00",
        "updated_at": "2020-05-01T17:57:38+00:00",
        "source_guild_id": "678070694164299796",
        "serialized_source_guild": {
            "name": "شام غيمرز | Night Owls 🦉",
            "description": "ألعاب وسهر",
            "region": "europe",
            "verification_level": 1,
            "icon_hash": "abcdef",
            "afk_channel_id": 9,
            "system_channel_id": 3,
            "roles": json!(vec![
                json!({ "id": 0, "name": "@everyone", "permissions": (bit(10) | bit(11) | bit(16) | bit(6) | bit(20) | bit(21) | bit(26) | bit(42)).to_string(), "color": 0, "hoist": false, "mentionable": false }),
                json!({ "id": 1, "name": "عضو", "permissions": "0", "color": 0x93c5fd, "hoist": false, "mentionable": true }),
                json!({ "id": 2, "name": "VIP ✨", "permissions": bit(18).to_string(), "color": 16755924, "hoist": true, "mentionable": false }),
                json!({ "id": 3, "name": "مشرف", "permissions": (bit(1) | bit(2) | bit(13) | bit(40) | bit(27)).to_string(), "color": 9153776, "hoist": true, "mentionable": true }),
                json!({ "id": 4, "name": "الإدارة", "permissions": (bit(5) | bit(28) | bit(13)).to_string(), "color": 15889544, "hoist": true, "mentionable": false }),
                json!({ "id": 5, "name": "المؤسس", "permissions": bit(3).to_string(), "color": 15972715, "hoist": true, "mentionable": false })
            ]),
            "channels": json!(vec![
                json!({ "id": 1, "type": 4, "name": "الاستقبال", "position": 0, "parent_id": null, "permission_overwrites": [
                    { "id": 0, "type": 0, "allow": "0", "deny": send },
                    { "id": 3, "type": 0, "allow": send, "deny": "0" }
                ] }),
                json!({ "id": 2, "type": 0, "name": "القوانين", "position": 0, "topic": "اقرأها قبل أي شيء", "nsfw": false, "rate_limit_per_user": 0, "parent_id": 1, "permission_overwrites": [
                    { "id": 0, "type": 0, "allow": "0", "deny": send },
                    { "id": 3, "type": 0, "allow": send, "deny": "0" }
                ] }),
                json!({ "id": 3, "type": 0, "name": "الترحيب", "position": 1, "topic": null, "parent_id": 1, "permission_overwrites": [] }),
                json!({ "id": 4, "type": 4, "name": "Chat 💬", "position": 1, "permission_overwrites": [] }),
                json!({ "id": 5, "type": 0, "name": "general", "position": 2, "topic": "Anything goes", "rate_limit_per_user": 5, "parent_id": 4, "permission_overwrites": [] }),
                json!({ "id": 6, "type": 2, "name": "🔊 Lounge", "position": 0, "bitrate": 64000, "user_limit": 0, "parent_id": 4, "permission_overwrites": [] }),
                json!({ "id": 7, "type": 2, "name": "Squad 1", "position": 1, "bitrate": 96000, "user_limit": 5, "parent_id": 4, "permission_overwrites": [] }),
                json!({ "id": 8, "type": 4, "name": "Staff", "position": 2, "permission_overwrites": [
                    { "id": 0, "type": 0, "allow": "0", "deny": view },
                    { "id": 3, "type": 0, "allow": view, "deny": "0" }
                ] }),
                json!({ "id": 9, "type": 0, "name": "staff-room", "position": 3, "nsfw": true, "parent_id": 8, "permission_overwrites": [
                    { "id": 0, "type": 0, "allow": "0", "deny": view },
                    { "id": 3, "type": 0, "allow": view, "deny": "0" }
                ] }),
                json!({ "id": 10, "type": 0, "name": "lobby", "position": 0, "parent_id": null, "permission_overwrites": [] })
            ])
        }
    })
}

#[test]
fn reads_a_whole_community() {
    let imported = read(community());
    assert_eq!(imported.name, "شام غيمرز | Night Owls 🦉");
    let structure = &imported.structure;
    assert_eq!(structure.description.as_deref(), Some("ألعاب وسهر"));

    // channels outside a category come first
    assert_eq!(structure.channels.len(), 1);
    assert_eq!(structure.channels[0].name, "lobby");
    assert!(!structure.channels[0].synced);

    let titles: Vec<&str> = structure.categories.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["الاستقبال", "Chat 💬", "Staff"]);
    let names = |index: usize| -> Vec<&str> {
        structure.categories[index].channels.iter().map(|c| c.name.as_str()).collect()
    };
    assert_eq!(names(0), ["القوانين", "الترحيب"]);
    // text before voice, each by its position
    assert_eq!(names(1), ["general", "🔊 Lounge", "Squad 1"]);
    assert_eq!(names(2), ["staff-room"]);

    assert_eq!(imported.report.categories, 3);
    assert_eq!(imported.report.channels, 7);
    assert_eq!(imported.report.roles, 5);
    assert_eq!(imported.report.private_channels, 1);
    assert_eq!(imported.report.mature_channels, 1);
    assert_eq!(structure.system_channel.as_deref(), Some("c3"));
}

#[test]
fn roles_come_highest_first_with_colour_and_hoist() {
    let imported = read(community());
    let names: Vec<&str> = imported.structure.roles.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["المؤسس", "الإدارة", "مشرف", "VIP ✨", "عضو"]);
    let vip = &imported.structure.roles[3];
    assert_eq!(vip.colour.as_deref(), Some("#ffacd4"));
    assert!(vip.hoist);
    assert_eq!(vip.allow, P::UseExternalEmojis as u64);
    let member = &imported.structure.roles[4];
    assert_eq!(member.colour.as_deref(), Some("#93c5fd"));
    assert_eq!(member.allow, 0);
}

#[test]
fn a_position_wins_over_the_order_in_the_document() {
    let mut template = community();
    let roles = template["serialized_source_guild"]["roles"].as_array_mut().unwrap();
    for (position, role) in roles.iter_mut().enumerate() {
        // upside down: the first named role is the highest
        role["position"] = json!(if position == 0 { 0 } else { 10 - position });
    }
    let imported = read(template);
    let names: Vec<&str> = imported.structure.roles.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["عضو", "VIP ✨", "مشرف", "الإدارة", "المؤسس"]);
}

#[test]
fn moderation_comes_over_and_control_of_the_server_is_held_back() {
    let imported = read(community());
    let moderator = &imported.structure.roles[2];
    for permission in [P::KickMembers, P::BanMembers, P::ManageMessages, P::TimeoutMembers, P::ManageNicknames] {
        assert!(has(moderator.allow, permission), "{permission}");
    }
    let admin = &imported.structure.roles[1];
    assert!(has(admin.allow, P::ManageMessages));
    // manage roles leaves the lesser right, never the control
    assert!(has(admin.allow, P::AssignRoles));
    for permission in [P::ManageServer, P::ManageRole, P::ManagePermissions, P::ManageWebhooks, P::ManageCustomisation] {
        assert!(!has(admin.allow, permission), "{permission}");
    }
    let founder = &imported.structure.roles[0];
    assert_eq!(founder.allow, 0, "administrator alone gives nothing");
    assert_eq!(imported.report.held_back, ["المؤسس", "الإدارة"]);
}

#[test]
fn everyone_becomes_the_default_permissions() {
    let imported = read(community());
    let rights = imported.structure.default_permissions.unwrap();
    for permission in [P::ViewChannel, P::SendMessage, P::CreateForumPosts, P::ReadMessageHistory, P::React, P::Connect, P::Listen, P::Speak, P::ChangeNickname, P::ChangeAvatar] {
        assert!(has(rights, permission), "{permission}");
    }
    assert!(!has(rights, P::InviteOthers));
    assert_eq!(imported.report.unmatched_permissions, ["USE_SOUNDBOARD"]);
}

/// Every documented Discord bit either maps to rights an import may hold,
/// or is held back, or is reported: none is dropped in silence and none
/// widens into control of the server
#[test]
fn every_discord_bit_is_accounted_for() {
    for (position, name, maps) in DISCORD_BITS {
        let mut unmatched = Vec::new();
        let mapped = map_bits(bit(*position), &mut unmatched);
        assert_eq!(mapped.rights & !*IMPORT_RIGHTS, 0, "{name}");
        match maps {
            Maps::To(rights) => {
                assert!(!rights.is_empty(), "{name}");
                assert!(!mapped.held, "{name}");
                assert!(unmatched.is_empty(), "{name}");
                for right in rights.iter() {
                    assert!(has(mapped.rights, *right), "{name} -> {right}");
                }
            }
            Maps::Held(_) => assert!(mapped.held, "{name}"),
            Maps::Nothing => {
                assert_eq!(mapped.rights, 0, "{name}");
                assert_eq!(unmatched, [name.to_string()]);
            }
        }
    }
    let control = P::ManageServer as u64
        | P::ManageRole as u64
        | P::ManagePermissions as u64
        | P::ManageWebhooks as u64
        | P::ManageCustomisation as u64
        | P::ManageJoinRequests as u64
        | P::Masquerade as u64
        | P::RemoveAvatars as u64;
    assert_eq!(*IMPORT_RIGHTS & control, 0);
    // all 64 bits at once: nothing beyond the import rights, and it is noted
    let mut unmatched = Vec::new();
    let all = map_bits(u64::MAX, &mut unmatched);
    assert_eq!(all.rights, *IMPORT_RIGHTS & all.rights);
    assert_eq!(all.rights & control, 0);
    assert!(all.held);
    assert!(unmatched.contains(&"UNKNOWN_47".to_string()));
    assert!(unmatched.contains(&"UNKNOWN_63".to_string()));
}

#[test]
fn the_named_pairs_of_the_mapping_table() {
    let one = |position: u32| map_bits(bit(position), &mut Vec::new()).rights;
    assert_eq!(one(0), P::InviteOthers as u64);
    assert_eq!(one(1), P::KickMembers as u64);
    assert_eq!(one(2), P::BanMembers as u64);
    assert_eq!(one(4), P::ManageChannel as u64);
    assert_eq!(one(6), P::React as u64);
    assert_eq!(one(7), P::ViewAuditLogs as u64);
    assert_eq!(one(9), P::Video as u64);
    assert_eq!(one(10), P::ViewChannel as u64);
    assert_eq!(one(11), P::SendMessage as u64 | P::CreateForumPosts as u64);
    assert_eq!(one(13), P::ManageMessages as u64);
    assert_eq!(one(14), P::SendEmbeds as u64);
    assert_eq!(one(15), P::UploadFiles as u64);
    assert_eq!(one(16), P::ReadMessageHistory as u64);
    assert_eq!(one(17), P::MentionEveryone as u64 | P::MentionRoles as u64);
    assert_eq!(one(18), P::UseExternalEmojis as u64);
    assert_eq!(one(20), P::Connect as u64 | P::Listen as u64);
    assert_eq!(one(21), P::Speak as u64);
    assert_eq!(one(22), P::MuteMembers as u64);
    assert_eq!(one(23), P::DeafenMembers as u64);
    assert_eq!(one(24), P::MoveMembers as u64);
    assert_eq!(one(26), P::ChangeNickname as u64 | P::ChangeAvatar as u64);
    assert_eq!(one(27), P::ManageNicknames as u64);
    assert_eq!(one(28), P::AssignRoles as u64);
    assert_eq!(one(35), P::CreateThreads as u64);
    assert_eq!(one(38), P::SendMessageInThreads as u64);
    assert_eq!(one(40), P::TimeoutMembers as u64);
    assert_eq!(one(52), P::BypassSlowmode as u64);
    // pinning must not become deleting
    assert_eq!(one(51), 0);
}

#[test]
fn a_channel_like_its_category_follows_it_and_others_keep_their_own() {
    let imported = read(community());
    let start = &imported.structure.categories[0];
    let send = P::SendMessage as u64 | P::CreateForumPosts as u64;
    assert_eq!(start.default_permissions, Some(Override { allow: 0, deny: send }));
    assert_eq!(start.role_permissions["r3"], Override { allow: send, deny: 0 });
    // the rules room has the category's rights: it follows the category
    assert!(start.channels[0].synced);
    assert!(start.channels[0].default_permissions.is_none());
    assert!(start.channels[0].role_permissions.is_empty());
    // the welcome room is open again: its own (empty) rights
    assert!(!start.channels[1].synced);
    assert!(start.channels[1].default_permissions.is_none());

    let staff = &imported.structure.categories[2];
    assert_eq!(staff.default_permissions, Some(Override { allow: 0, deny: P::ViewChannel as u64 }));
    assert!(staff.channels[0].synced);
    assert!(staff.channels[0].nsfw, "the age-restricted mark is never lost");
}

#[test]
fn topics_slow_mode_and_voice_limits() {
    let imported = read(community());
    let rules = &imported.structure.categories[0].channels[0];
    assert_eq!(rules.description.as_deref(), Some("اقرأها قبل أي شيء"));
    assert_eq!(rules.slowmode, None);
    let chat = &imported.structure.categories[1].channels;
    assert_eq!(chat[0].slowmode, Some(5));
    assert!(matches!(chat[1].channel_type, LegacyServerChannelType::Voice));
    assert_eq!(chat[1].max_users, None);
    assert_eq!(chat[2].max_users, Some(5));
    assert_eq!(chat[2].slowmode, None);
}

#[test]
fn member_overrides_are_dropped_and_counted() {
    let mut template = community();
    template["serialized_source_guild"]["channels"][9]["permission_overwrites"] = json!([
        { "id": "132837293881950208", "type": 1, "allow": bit(10).to_string(), "deny": "0" },
        { "id": "232837293881950208", "type": "member", "allow": "0", "deny": bit(11).to_string() }
    ]);
    let imported = read(template);
    let lobby = &imported.structure.channels[0];
    assert!(lobby.default_permissions.is_none());
    assert!(lobby.role_permissions.is_empty());
    assert_eq!(imported.report.member_overrides, 2);
    let text = format!("{:?}", imported);
    assert!(!text.contains("132837293881950208"));
}

/// Nothing about the person who made the template, or the ids of the source
/// server, is in what comes out
#[test]
fn nothing_about_the_maker_or_the_source_survives() {
    let imported = read(community());
    let out = format!(
        "{} {} {}",
        imported.name,
        serde_json::to_string(&imported.structure).unwrap(),
        serde_json::to_string(&imported.report).unwrap()
    );
    for secret in ["hoges", "132837293881950208", "678070694164299796", "hgM48av5Q69A", "79b0d9f8", "abcdef", "2020-04-02", "Night Owls template"] {
        assert!(!out.contains(secret), "{secret} leaked");
    }
}

#[test]
fn an_override_that_cannot_be_read_hides_the_channel() {
    for broken in [
        json!([{ "id": 0, "type": 0, "allow": "lots", "deny": "0" }]),
        json!([{ "type": 0, "allow": "0", "deny": "0" }]),
        json!([{ "id": 3, "type": 0, "allow": [], "deny": "0" }]),
        json!("everything"),
    ] {
        let mut template = community();
        template["serialized_source_guild"]["channels"][9]["permission_overwrites"] = broken.clone();
        let imported = read(template);
        let lobby = &imported.structure.channels[0];
        let field = lobby.default_permissions.clone().expect("hidden");
        assert!(has(field.deny, P::ViewChannel), "{broken}");
        assert!(!has(field.allow, P::ViewChannel), "{broken}");
    }
}

#[test]
fn a_deny_wins_over_an_allow_of_the_same_right() {
    let mut template = community();
    template["serialized_source_guild"]["channels"][9]["permission_overwrites"] =
        json!([{ "id": 0, "type": 0, "allow": bit(10).to_string(), "deny": bit(10).to_string() }]);
    let imported = read(template);
    assert_eq!(
        imported.structure.channels[0].default_permissions,
        Some(Override { allow: 0, deny: P::ViewChannel as u64 })
    );
}

#[test]
fn overrides_never_carry_control_and_unknown_roles_are_dropped() {
    let mut template = community();
    template["serialized_source_guild"]["channels"][9]["permission_overwrites"] = json!([
        { "id": 3, "type": 0, "allow": (bit(28) | bit(29) | bit(3) | bit(11)).to_string(), "deny": "0" },
        { "id": 77, "type": 0, "allow": bit(11).to_string(), "deny": "0" }
    ]);
    let imported = read(template);
    let lobby = &imported.structure.channels[0];
    assert_eq!(lobby.role_permissions.len(), 1);
    let field = &lobby.role_permissions["r3"];
    assert_eq!(field.allow & !*IMPORT_RIGHTS, 0);
    assert!(has(field.allow, P::SendMessage));
    assert!(!has(field.allow, P::ManagePermissions));
    assert!(check_structure(&imported.structure, LIMITS).is_ok());
}

#[test]
fn long_and_odd_names_are_cleaned_and_cut() {
    let long = "ق".repeat(100);
    let template = json!({ "name": "  spaced\u{202E}out\u{0000}  ", "roles": [
        { "id": 0, "name": "@everyone", "permissions": "0" },
        { "id": 1, "name": "x".repeat(100), "permissions": 0 },
        { "id": 2, "name": "\u{202E}\u{200B}\u{0007}", "permissions": 0 },
        { "id": 3, "name": "👨‍👩‍👧‍👦 family", "permissions": 0 }
    ], "channels": [
        { "id": 1, "type": 0, "name": long, "topic": "t".repeat(3000) },
        { "id": 2, "type": 0, "name": "" },
        { "id": 3, "type": 0, "name": "line\nbreak\ttab" },
        { "id": 4, "type": 4, "name": "c".repeat(64) }
    ] });
    let imported = read(template);
    assert_eq!(imported.name, "spacedout");
    let roles = &imported.structure.roles;
    assert_eq!(roles[0].name, "👨‍👩‍👧‍👦 family", "joined emoji stay whole");
    assert_eq!(roles[2].name.chars().count(), 32);
    assert!(!roles[1].name.is_empty());
    assert!(!roles[1].name.chars().any(unwanted));
    let rooms = &imported.structure.channels;
    assert_eq!(rooms[0].name.chars().count(), 32);
    assert_eq!(rooms[0].description.as_ref().unwrap().chars().count(), 1024);
    assert_eq!(rooms[1].name, "channel-2");
    assert_eq!(rooms[2].name, "line break tab");
    assert_eq!(imported.structure.categories[0].title.chars().count(), 32);
    assert_eq!(imported.report.shortened.len(), 3);
    assert!(check_structure(&imported.structure, LIMITS).is_ok());
}

#[test]
fn channel_kinds() {
    let template = json!({ "name": "kinds", "roles": [], "channels": [
        { "id": 1, "type": 5, "name": "news" },
        { "id": 2, "type": 15, "name": "help" },
        { "id": 3, "type": 13, "name": "stage" },
        { "id": 4, "type": 16, "name": "media" },
        { "id": 5, "type": 99, "name": "future" },
        { "id": 6, "name": "untyped" },
        { "id": 7, "type": "0", "name": "typed-as-text" }
    ] });
    let imported = read(template);
    let rooms = &imported.structure.channels;
    assert_eq!(rooms.len(), 3);
    assert!(rooms.iter().any(|room| room.name == "news" && room.announcement));
    assert!(rooms.iter().any(|room| room.name == "help" && matches!(room.channel_type, LegacyServerChannelType::Forum)));
    assert!(rooms.iter().any(|room| room.name == "typed-as-text"));
    // in the order they are met: text-like kinds first, then voice-like
    assert_eq!(imported.report.skipped_channels, ["media", "future", "untyped", "stage"]);
}

#[test]
fn orphans_duplicates_and_equal_positions() {
    let template = json!({ "name": "odd", "roles": [
        { "id": 0, "name": "@everyone", "permissions": "0" },
        { "id": 1, "name": "twice", "permissions": "0" },
        { "id": 1, "name": "twice again", "permissions": "0" },
        { "id": 2, "name": "bot role", "permissions": "8", "managed": true },
        { "id": 3, "name": "bot role two", "permissions": "8", "tags": { "bot_id": "1" } },
        "not a role"
    ], "channels": [
        { "id": 1, "type": 0, "name": "b", "position": 1, "parent_id": 404 },
        { "id": 2, "type": 0, "name": "a", "position": 1 },
        { "id": 2, "type": 0, "name": "a again", "position": 2 },
        null, 7, "text"
    ] });
    let imported = read(template);
    let names: Vec<&str> = imported.structure.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["b", "a"]);
    assert_eq!(imported.structure.roles.len(), 1);
    assert!(imported.report.held_back.is_empty());
}

#[test]
fn a_big_server_is_cut_at_the_limits() {
    let roles: Vec<Value> = std::iter::once(json!({ "id": 0, "name": "@everyone", "permissions": "0" }))
        .chain((1..=30).map(|id| json!({ "id": id, "name": format!("role {id}"), "permissions": "0" })))
        .collect();
    let mut channels = Vec::new();
    for category in 0..12 {
        channels.push(json!({ "id": 1000 + category, "type": 4, "name": format!("cat {category}"), "position": category }));
        for room in 0..6 {
            channels.push(json!({ "id": category * 10 + room, "type": if room < 4 { 0 } else { 2 }, "name": format!("room-{category}-{room}"), "position": room, "parent_id": 1000 + category }));
        }
    }
    let template = json!({ "serialized_source_guild": { "name": "big", "roles": roles, "channels": channels } });

    let all = read(template.clone());
    assert_eq!(all.report.channels, 72);
    assert_eq!(all.report.categories, 12);
    assert_eq!(all.report.roles, 30);
    assert!(check_structure(&all.structure, LIMITS).is_ok());

    let limits = ImportLimits { channels: 50, roles: 20 };
    let cut = read_template(&template.to_string(), limits).unwrap();
    assert_eq!(cut.report.channels, 50);
    assert_eq!(cut.report.over_limit_channels, 22);
    assert_eq!(cut.report.roles, 20);
    assert_eq!(cut.report.over_limit_roles, 10);
    // the highest roles are the ones that stay
    assert_eq!(cut.structure.roles[0].name, "role 30");
    assert_eq!(cut.structure.roles[19].name, "role 11");
    assert!(check_structure(&cut.structure, limits).is_ok());
}

#[test]
fn more_categories_than_a_layout_holds() {
    let channels: Vec<Value> = (0..60)
        .flat_map(|category| {
            [
                json!({ "id": 1000 + category, "type": 4, "name": format!("cat {category}"), "position": category }),
                json!({ "id": category, "type": 0, "name": format!("room-{category}"), "parent_id": 1000 + category }),
            ]
        })
        .collect();
    let imported = read(json!({ "name": "wide", "roles": [], "channels": channels }));
    assert_eq!(imported.structure.categories.len(), MAX_CATEGORIES);
    // the rooms of the categories left out stay, outside any category
    assert_eq!(imported.structure.channels.len(), 10);
    assert!(check_structure(&imported.structure, LIMITS).is_ok());
}

#[test]
fn permissions_as_numbers_strings_and_nonsense() {
    let template = json!({ "name": "p", "roles": [
        { "id": 0, "name": "@everyone", "permissions": 1024 },
        { "id": 1, "name": "as string", "permissions": "2048" },
        { "id": 2, "name": "missing" },
        { "id": 3, "name": "null", "permissions": null },
        { "id": 4, "name": "nonsense", "permissions": "many" },
        { "id": 5, "name": "huge", "permissions": "340282366920938463463374607431768211455" },
        { "id": 6, "name": "negative", "permissions": -1 }
    ], "channels": [] });
    let imported = read(template);
    assert_eq!(imported.structure.default_permissions, Some(P::ViewChannel as u64));
    let allow = |name: &str| imported.structure.roles.iter().find(|r| r.name == name).unwrap().allow;
    assert_eq!(allow("as string"), P::SendMessage as u64 | P::CreateForumPosts as u64);
    assert_eq!(allow("missing"), 0);
    assert_eq!(allow("null"), 0);
    assert_eq!(allow("nonsense"), 0);
    assert_eq!(allow("negative"), 0);
    assert_eq!(allow("huge") & !*IMPORT_RIGHTS, 0);
    assert!(imported.report.held_back.contains(&"huge".to_string()));
}

#[test]
fn only_everyone_is_still_a_server_when_it_has_channels() {
    let imported = read(json!({ "name": "small", "roles": [{ "id": 0, "name": "@everyone", "permissions": "1024" }],
        "channels": [{ "id": 1, "type": 0, "name": "general" }] }));
    assert_eq!(imported.report.roles, 0);
    assert_eq!(imported.report.channels, 1);
}

#[test]
fn what_is_not_a_template() {
    let read = |text: &str| read_template(text, LIMITS).map(|_| ()).unwrap_err();
    assert_eq!(read(""), ImportError::NotATemplate);
    assert_eq!(read("hello"), ImportError::NotATemplate);
    assert_eq!(read("[]"), ImportError::NotATemplate);
    assert_eq!(read("{}"), ImportError::NotATemplate);
    assert_eq!(read(r#"{"message": "Unknown server template", "code": 10057}"#), ImportError::NotATemplate);
    assert_eq!(read(r#"{"roles": {}, "channels": []}"#), ImportError::NotATemplate);
    assert_eq!(read(r#"{"serialized_source_guild": {"roles": [], "chan"#), ImportError::NotATemplate);
    assert_eq!(read(r#"{"roles": [], "channels": []}"#), ImportError::Empty);
    assert_eq!(read(r#"{"roles": [{"id":0,"name":"@everyone"}], "channels": [{"id":1,"type":4,"name":"only a category"}]}"#), ImportError::Empty);
    assert_eq!(read(&"x".repeat(MAX_TEMPLATE_BYTES + 1)), ImportError::TooLarge);
    // deep nesting must fail, not crash
    assert_eq!(read(&"[".repeat(100_000)), ImportError::NotATemplate);
}

#[test]
fn links_and_codes() {
    for link in [
        "hgM48av5Q69A",
        "  hgM48av5Q69A  ",
        "https://discord.new/hgM48av5Q69A",
        "discord.new/hgM48av5Q69A",
        "https://discord.com/template/hgM48av5Q69A",
        "https://www.discord.com/template/hgM48av5Q69A/",
        "https://discordapp.com/template/hgM48av5Q69A?x=1",
        "https://discord.com/api/v10/guilds/templates/hgM48av5Q69A",
        "https://discord.com/api/guilds/templates/hgM48av5Q69A",
        "HTTPS://DISCORD.NEW/hgM48av5Q69A".replace("HTTPS://DISCORD.NEW", "https://DISCORD.NEW").as_str(),
    ] {
        assert_eq!(template_code(link).as_deref(), Ok("hgM48av5Q69A"), "{link}");
    }
    for invite in ["https://discord.gg/abc123", "discord.gg/abc123", "https://discord.com/invite/abc123"] {
        assert_eq!(template_code(invite), Err(LinkError::Invite), "{invite}");
    }
    for wrong in [
        "",
        "https://example.com/template/hgM48av5Q69A",
        "https://discord.new.evil.example/hgM48av5Q69A",
        "https://discord.new/",
        "https://discord.new/../../users/@me",
        "https://discord.new/hgM48av5Q69A/extra",
        "https://discord.com/channels/1/2",
        "https://discord.new/abc%2F..%2Fx",
        "two words",
        "https://discord.new/a b",
        &"a".repeat(33),
        "a",
        "https://user@discord.new/hgM48av5Q69A",
    ] {
        assert_eq!(template_code(wrong), Err(LinkError::NotALink), "{wrong}");
    }
}

#[test]
fn a_changed_layout_is_refused() {
    let good = read(community()).structure;
    assert!(check_structure(&good, LIMITS).is_ok());
    let refused = |change: &dyn Fn(&mut DataServerStructure)| {
        let mut structure = good.clone();
        change(&mut structure);
        assert!(check_structure(&structure, LIMITS).is_err());
    };
    refused(&|s| s.roles[0].allow = P::ManageServer as u64);
    refused(&|s| s.roles[0].allow = P::ManagePermissions as u64);
    refused(&|s| s.roles[0].allow = u64::MAX);
    refused(&|s| s.default_permissions = Some(P::ManageRole as u64));
    refused(&|s| s.channels[0].default_permissions = Some(Override { allow: P::ManageWebhooks as u64, deny: 0 }));
    refused(&|s| s.categories[0].default_permissions = Some(Override { allow: 0, deny: 1 << 60 }));
    refused(&|s| {
        s.channels[0].role_permissions.insert("r404".to_string(), Override::default());
    });
    refused(&|s| {
        s.categories[0].role_permissions.insert("nobody".to_string(), Override::default());
    });
    refused(&|s| s.roles[1].key = s.roles[0].key.clone());
    refused(&|s| s.channels[0].key = s.categories[0].channels[0].key.clone());
    refused(&|s| s.channels[0].synced = true);
    refused(&|s| s.channels[0].max_users = Some(5));
    refused(&|s| s.categories[1].channels[1].announcement = true);
    refused(&|s| s.system_channel = Some("c404".to_string()));
    refused(&|s| s.channels[0].name = "evil\u{202E}".to_string());
    refused(&|s| s.roles[0].name = "evil\u{0000}".to_string());
    refused(&|s| s.categories[0].title = "evil\u{2066}".to_string());
    assert!(check_structure(&good, ImportLimits { channels: 6, roles: 200 }).is_err());
    assert!(check_structure(&good, ImportLimits { channels: 200, roles: 4 }).is_err());
    let mut wide = good.clone();
    wide.categories = vec![StructureCategory::default(); MAX_CATEGORIES + 1];
    assert!(check_structure(&wide, LIMITS).is_err());
}
