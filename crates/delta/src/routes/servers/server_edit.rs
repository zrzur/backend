use std::collections::HashSet;

use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Database, FieldsServer, File, PartialServer, User, ValidatedTicket,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission, PermissionQuery};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// # Edit Server
///
/// Edit a server by its id.
#[openapi(tag = "Server Information")]
#[patch("/<target>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    data: Json<v0::DataEditServer>,
    validated_ticket: Option<ValidatedTicket>,
) -> Result<Json<v0::Server>> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let mut server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    let permissions = calculate_server_permissions(&mut query).await;

    // Check permissions
    if data.name.is_none()
        && data.description.is_none()
        && data.icon.is_none()
        && data.banner.is_none()
        && data.system_messages.is_none()
        && data.categories.is_none()
        // && data.nsfw.is_none()
        && data.flags.is_none()
        && data.analytics.is_none()
        && data.discoverable.is_none()
        && data.access.is_none()
        && data.verification_level.is_none()
        && data.rules_screening.is_none()
        && data.owner.is_none()
        && data.rules_channel.is_none()
        && data.stats.is_none()
        && data.lobby.is_none()
        && data.threads_disabled.is_none()
        && data.forums_disabled.is_none()
        && data.default_notifications.is_none()
        && data.remove.is_empty()
    {
        // an empty edit returns the server: only to its members, like a
        // fetch (it used to hand any server to anyone who knew its ID)
        if !query.are_we_a_member().await {
            return Err(create_error!(NotFound));
        }
        return Ok(Json(server.into(db).await));
    } else if data.name.is_some()
        || data.description.is_some()
        || data.icon.is_some()
        || data.banner.is_some()
        || data.system_messages.is_some()
        || data.analytics.is_some()
        || data.rules_channel.is_some()
        || data.stats.is_some()
        || data.lobby.is_some()
        || data.threads_disabled.is_some()
        || data.forums_disabled.is_some()
        || data.default_notifications.is_some()
        || data.access.is_some()
        || data.verification_level.is_some()
        || data.rules_screening.is_some()
        || !data.remove.is_empty()
    {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    }

    // Discover lists only servers anyone can join: a server that needs
    // approval or is locked can never be listed
    let access = data.access.or(server.access).unwrap_or_default();
    if data.discoverable == Some(true) && access != v0::ServerAccess::Open {
        return Err(create_error!(InvalidOperation));
    }

    // Check we are the server owner or privileged if changing sensitive fields
    if data.owner.is_some() {
        if user.id != server.owner && !user.privileged {
            return Err(create_error!(NotOwner));
        }

        if validated_ticket.is_none() {
            return Err(create_error!(InvalidCredentials));
        }
    }

    // Check we are privileged if changing sensitive fields
    if (data.flags.is_some() /*|| data.nsfw.is_some()*/ || data.discoverable.is_some())
        && !user.privileged
    {
        return Err(create_error!(NotPrivileged));
    }

    // Changing categories requires manage channel
    if data.categories.is_some() {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;
    }

    let v0::DataEditServer {
        name,
        description,
        icon,
        banner,
        categories,
        system_messages,
        flags,
        // nsfw,
        discoverable,
        analytics,
        owner,
        rules_channel,
        stats,
        lobby,
        threads_disabled,
        forums_disabled,
        default_notifications,
        access: new_access,
        verification_level,
        rules_screening,
        remove,
    } = data;

    // closing a listed server also takes it off Discover
    let discoverable = if access != v0::ServerAccess::Open && server.discoverable {
        Some(false)
    } else {
        discoverable
    };

    let mut partial = PartialServer {
        name,
        description,
        categories: categories.map(|v| v.into_iter().map(Into::into).collect()),
        system_messages: system_messages.map(Into::into),
        flags,
        // nsfw,
        discoverable,
        access: new_access,
        verification_level,
        analytics,
        owner: owner.clone(),
        rules_channel,
        stats,
        lobby,
        threads_disabled,
        forums_disabled,
        default_notifications,
        ..Default::default()
    };

    // 1. Remove fields from object
    if remove.contains(&v0::FieldsServer::Banner) {
        if let Some(banner) = &server.banner {
            db.mark_attachment_as_deleted(&banner.id).await?;
        }
    }

    if remove.contains(&v0::FieldsServer::Icon) {
        if let Some(icon) = &server.icon {
            db.mark_attachment_as_deleted(&icon.id).await?;
        }
    }

    // 2. Validate changes
    // the rules live in a text channel of this server
    if let Some(rules_channel) = &partial.rules_channel {
        if !server.channels.contains(rules_channel) {
            return Err(create_error!(NotFound));
        }
        let channel = db.fetch_channel(rules_channel).await?;
        if !matches!(channel, revolt_database::Channel::TextChannel { .. }) || channel.voice().is_some() {
            return Err(create_error!(InvalidOperation));
        }
    }

    // the lobby points only at this server's channels and roles
    if let Some(lobby) = &mut partial.lobby {
        check_lobby(lobby, &server)?;
        // compare with the stored questions minus deleted roles, as check_lobby drops those
        let mut previous = server.lobby.as_ref().map(|l| l.onboarding.clone()).unwrap_or_default();
        for question in &mut previous {
            question.options.retain(|option| server.roles.contains_key(&option.role));
        }
        previous.retain(|question| !question.options.is_empty());
        if lobby.onboarding != previous {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::AssignRoles)?;
            let rank = query.get_member_rank().unwrap_or(i64::MIN);
            for option in lobby.onboarding.iter().flat_map(|q| &q.options) {
                let role = server.roles.get(&option.role).ok_or_else(|| create_error!(InvalidRole))?;
                if user.id != server.owner && role.rank <= rank {
                    return Err(create_error!(NotElevated));
                }
            }
        }
        for option in lobby.onboarding.iter().flat_map(|q| &q.options) {
            super::lobby_answer::check_public_role(db, &server, &option.role).await?;
        }
    }

    // each counter once, in the order given
    if let Some(stats) = &mut partial.stats {
        let mut seen = HashSet::new();
        stats.retain(|stat| seen.insert(*stat));
    }

    if let Some(system_messages) = &partial.system_messages {
        for id in system_messages.clone().into_channel_ids() {
            if !server.channels.contains(&id) {
                return Err(create_error!(NotFound));
            }
        }
    }

    if let Some(categories) = &mut partial.categories {
        let mut channel_ids = HashSet::new();
        for category in categories {
            for channel in &category.channels {
                if channel_ids.contains(channel) {
                    return Err(create_error!(InvalidOperation));
                }

                channel_ids.insert(channel.to_string());
            }

            category
                .channels
                .retain(|item| server.channels.contains(item));

            // a category's permissions change only through its permission
            // routes (Manage Permissions and override checks); here they
            // keep their stored value, and a new category starts with none
            let stored = server
                .categories
                .as_ref()
                .and_then(|stored| stored.iter().find(|c| c.id == category.id));
            category.default_permissions = stored.and_then(|c| c.default_permissions);
            category.role_permissions = stored.map(|c| c.role_permissions.clone()).unwrap_or_default();
        }
    }

    // 3. Apply new icon
    if let Some(icon) = icon {
        partial.icon = Some(File::use_server_icon(db, &icon, &server.id, &user.id).await?);
        server.icon = partial.icon.clone();
    }

    // 4. Apply new banner
    if let Some(banner) = banner {
        partial.banner = Some(File::use_server_banner(db, &banner, &server.id, &user.id).await?);
        server.banner = partial.banner.clone();
    }

    // 5. Transfer ownership
    let previous_owner = owner.is_some().then(|| server.owner.clone());
    if let Some(owner) = owner {
        let owner_reference = Reference::from_unchecked(&owner);
        // Check if member exists
        owner_reference.as_member(db, &server.id).await?;
        let owner_user = owner_reference.as_user(db).await?;

        if owner_user.bot.is_some() {
            return Err(create_error!(InvalidOperation));
        }

        server.owner = owner;
        partial.owner = Some(server.owner.clone());
    }

    let mut remove = remove
        .into_iter()
        .map(Into::into)
        .collect::<Vec<FieldsServer>>();

    // Rules screening: members who join from now on accept the rules first
    match rules_screening {
        Some(true) if server.rules_screening_since.is_none() => {
            if partial.rules_channel.is_none() && server.rules_channel.is_none() {
                return Err(create_error!(InvalidOperation));
            }
            partial.rules_screening_since = Some(iso8601_timestamp::Timestamp::now_utc());
        }
        Some(false) if server.rules_screening_since.is_some() => {
            remove.push(FieldsServer::RulesScreening)
        }
        _ => {}
    }

    let before = server.generate_diff(&partial, &remove);

    server.update(db, partial.clone(), remove).await?;

    // the verified owner badge moves with the server
    if let Some(previous_owner) = previous_owner {
        for owner in [&previous_owner, &server.owner] {
            if let Err(error) = revolt_database::sync_verified_server_owner(db, owner).await {
                log::error!("verified server owner badge for {owner}: {error:?}");
            }
        }
    }

    AuditLogEntryAction::ServerEdit {
        before,
        after: partial,
    }
    .insert(db, server.id.clone(), reason, user.id, None)
    .await;

    Ok(Json(server.into(db).await))
}

/// Drop channels and roles that the stored lobby used and that were deleted since,
/// so deleting one never locks managers out of editing their lobby. Unknown IDs
/// that were not stored before are left in place and rejected by `check_lobby`.
fn drop_deleted(lobby: &mut v0::ServerLobby, server: &revolt_database::Server) {
    let Some(stored) = &server.lobby else { return };
    let used: HashSet<&String> = stored.actions.iter().map(|a| &a.channel)
        .chain(&stored.channels)
        .chain(&stored.staff_roles)
        .chain(stored.news_channel.iter().chain(&stored.intro_channel))
        .chain(stored.onboarding.iter().flat_map(|q| q.options.iter().map(|o| &o.role)))
        .collect();
    let gone_channel = |id: &String| used.contains(id) && !server.channels.contains(id);
    let gone_role = |id: &String| used.contains(id) && !server.roles.contains_key(id);

    lobby.actions.retain(|action| !gone_channel(&action.channel));
    lobby.channels.retain(|id| !gone_channel(id));
    lobby.staff_roles.retain(|id| !gone_role(id));
    for channel in [&mut lobby.news_channel, &mut lobby.intro_channel] {
        if channel.as_ref().is_some_and(gone_channel) {
            *channel = None;
        }
    }
    // a question loses its place only when its last answer pointed at a deleted role
    lobby.onboarding.retain_mut(|question| {
        let before = question.options.len();
        question.options.retain(|option| !gone_role(&option.role));
        before == 0 || !question.options.is_empty()
    });
}

/// Lengths, colours and references of a lobby
fn check_lobby(lobby: &mut v0::ServerLobby, server: &revolt_database::Server) -> Result<()> {
    fn text(value: &str, max: usize, multiline: bool) -> Result<()> {
        let len = value.chars().count();
        if len == 0
            || len > max
            || value
                .chars()
                .any(|c| c.is_control() && !(multiline && (c == '\n' || c == '\t')))
        {
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::length("lobby", 1, max),
            ]));
        }
        Ok(())
    }

    // empty strings clear a field
    let clear = |value: &mut Option<String>| {
        if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
            *value = None;
        }
    };
    clear(&mut lobby.tagline);
    clear(&mut lobby.about);
    clear(&mut lobby.accent);

    if let Some(tagline) = &lobby.tagline {
        text(tagline, 120, false)?;
    }
    if let Some(about) = &lobby.about {
        text(about, 3000, true)?;
    }
    if let Some(accent) = &lobby.accent {
        let bytes = accent.as_bytes();
        if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::new("accent", "format"),
            ]));
        }
    }

    if let Some(style) = &lobby.appearance {
        for colour in [&style.secondary, &style.background, &style.surface, &style.text].into_iter().flatten() {
            let bytes = colour.as_bytes();
            if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
                return Err(revolt_result::failed_validation(vec![
                    revolt_result::ValidationProblem::new("colour", "format"),
                ]));
            }
        }
        for (value, min, max) in [
            (style.banner_height, 100, 360), (style.radius, 0, 28),
            (style.blend, 0, 100), (style.angle, 0, 360),
            (style.channel_limit, 1, 9), (style.newcomer_limit, 1, 24),
        ] {
            if value.is_some_and(|v| v < min || v > max) {
                return Err(create_error!(FailedValidation { error: "lobby appearance value is out of range".into() }));
            }
        }
    }

    drop_deleted(lobby, server);

    if lobby.onboarding.len() > 5 {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::count("questions", 5),
        ]));
    }
    let mut question_ids = HashSet::new();
    for question in &lobby.onboarding {
        text(&question.id, 64, false)?;
        text(&question.question, 150, false)?;
        if !question_ids.insert(&question.id) || question.options.is_empty() || question.options.len() > 8 {
            return Err(create_error!(FailedValidation { error: "role questions require unique IDs and 1-8 choices".into() }));
        }
        if let Some(description) = &question.description { text(description, 500, true)?; }
        let mut option_ids = HashSet::new();
        for option in &question.options {
            text(&option.id, 64, false)?;
            text(&option.label, 80, false)?;
            if !option_ids.insert(&option.id) || !server.roles.contains_key(&option.role) {
                return Err(create_error!(InvalidRole));
            }
        }
    }

    let too_many = |what: &str, max: usize| {
        create_error!(FailedValidation {
            error: format!("a lobby has at most {max} {what}")
        })
    };
    if lobby.actions.len() > 3 {
        return Err(too_many("buttons", 3));
    }
    for action in &lobby.actions {
        text(&action.label, 32, false)?;
        if let Some(emoji) = &action.emoji {
            text(emoji, 32, false)?;
        }
        if !server.channels.contains(&action.channel) {
            return Err(create_error!(NotFound));
        }
    }

    // each section once, in the order given
    let mut seen = HashSet::new();
    lobby.sections.retain(|section| seen.insert(*section));
    for channel in [&lobby.news_channel, &lobby.intro_channel].into_iter().flatten() {
        if !server.channels.contains(channel) {
            return Err(create_error!(NotFound));
        }
    }

    if lobby.channels.len() > 9 {
        return Err(too_many("channels", 9));
    }
    let mut seen = HashSet::new();
    lobby.channels.retain(|id| seen.insert(id.clone()));
    if lobby.channels.iter().any(|id| !server.channels.contains(id)) {
        return Err(create_error!(NotFound));
    }

    if lobby.faq.len() > 20 {
        return Err(too_many("questions", 20));
    }
    for entry in &lobby.faq {
        text(&entry.question, 150, false)?;
        text(&entry.answer, 2000, true)?;
    }

    if lobby.staff_roles.len() > 10 {
        return Err(too_many("staff roles", 10));
    }
    let mut seen = HashSet::new();
    lobby.staff_roles.retain(|id| seen.insert(id.clone()));
    if lobby.staff_roles.iter().any(|id| !server.roles.contains_key(id)) {
        return Err(create_error!(NotFound));
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use revolt_database::Member;
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    use crate::util::test::TestHarness;

    async fn patch(
        harness: &TestHarness,
        token: &str,
        path: String,
        body: serde_json::Value,
    ) -> Status {
        harness
            .client
            .patch(path)
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.to_string()))
            .body(body.to_string())
            .dispatch()
            .await
            .status()
    }

    /// The rules channel must be a text channel of the server, set by
    /// someone who can manage it; deleting the channel clears it
    #[rocket::async_test]
    async fn rules_channel() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, member_session, member) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        Member::create(&harness.db, &server, &owner, Some(channels.clone())).await.unwrap();
        Member::create(&harness.db, &server, &member, Some(channels.clone())).await.unwrap();
        let (other_server, other_channels) = harness.new_server(&owner).await;
        let text = channels[0].id().to_string();
        let path = format!("/servers/{}", server.id);

        // a plain member may not choose it
        assert_eq!(
            patch(&harness, &member_session.token, path.clone(), json!({ "rules_channel": text })).await,
            Status::Forbidden
        );
        // a channel of another server is not found
        assert_eq!(
            patch(&harness, &owner_session.token, path.clone(), json!({ "rules_channel": other_channels[0].id() })).await,
            Status::NotFound
        );
        let _ = other_server;
        // the owner sets it
        assert_eq!(
            patch(&harness, &owner_session.token, path.clone(), json!({ "rules_channel": text })).await,
            Status::Ok
        );
        let stored = harness.db.fetch_server(&server.id).await.unwrap();
        assert_eq!(stored.rules_channel.as_deref(), Some(text.as_str()));

        // removed with the channel
        harness.db.delete_channel(&channels[0]).await.unwrap();
        let stored = harness.db.fetch_server(&server.id).await.unwrap();
        assert_eq!(stored.rules_channel, None);
    }

    /// The lobby is checked and stored whole; only managers may edit it
    #[rocket::async_test]
    async fn lobby_is_checked() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, member_session, member) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        Member::create(&harness.db, &server, &owner, Some(channels.clone())).await.unwrap();
        Member::create(&harness.db, &server, &member, Some(channels.clone())).await.unwrap();
        let (_, other_channels) = harness.new_server(&owner).await;
        let general = channels[0].id().to_string();
        let path = format!("/servers/{}", server.id);
        let lobby = |extra: serde_json::Value| {
            let mut lobby = json!({
                "enabled": true,
                "tagline": "Where people meet",
                "about": "## Hello
Be kind.",
                "accent": "#FF5BA8",
                "actions": [{ "label": "Say hi", "channel": general, "emoji": "👋" }],
                "channels": [general, general],
                "faq": [{ "question": "Is this official?", "answer": "Yes." }],
                "sections": ["Start", "News", "Start", "Team"],
                "intro_channel": general
            });
            for (key, value) in extra.as_object().unwrap() {
                lobby[key] = value.clone();
            }
            json!({ "lobby": lobby })
        };

        assert_eq!(patch(&harness, &member_session.token, path.clone(), lobby(json!({}))).await, Status::Forbidden);
        for bad in [
            json!({ "accent": "pink" }),
            json!({ "appearance": { "background": "white" } }),
            json!({ "appearance": { "banner_height": 9999 } }),
            json!({ "appearance": { "blend": 101 } }),
            json!({ "appearance": { "layout": "arbitrary" } }),
            json!({ "channels": [other_channels[0].id()] }),
            json!({ "staff_roles": ["01ARZ3NDEKTSV4RRFFQ69G5FAV"] }),
            json!({ "news_channel": other_channels[0].id() }),
            json!({ "sections": ["Gossip"] }),
            json!({ "tagline": "x".repeat(121) }),
            json!({ "actions": [
                { "label": "a", "channel": general }, { "label": "b", "channel": general },
                { "label": "c", "channel": general }, { "label": "d", "channel": general }
            ] }),
        ] {
            let status = patch(&harness, &owner_session.token, path.clone(), lobby(bad.clone())).await;
            assert!(
                matches!(status.code, 400 | 404 | 422),
                "{bad}"
            );
        }

        assert_eq!(patch(&harness, &owner_session.token, path.clone(), lobby(json!({}))).await, Status::Ok);
        let stored = harness.db.fetch_server(&server.id).await.unwrap().lobby.unwrap();
        assert!(stored.enabled);
        assert_eq!(stored.channels, vec![general.clone()]);
        assert_eq!(stored.actions[0].label, "Say hi");
        assert_eq!(
            stored.sections,
            vec![v0::LobbySection::Start, v0::LobbySection::News, v0::LobbySection::Team]
        );

        assert_eq!(patch(&harness, &owner_session.token, path.clone(), json!({ "remove": ["Lobby"] })).await, Status::Ok);
        assert!(harness.db.fetch_server(&server.id).await.unwrap().lobby.is_none());
    }

    /// Deleting a channel or role the lobby uses never blocks later lobby edits
    #[rocket::async_test]
    async fn lobby_survives_deleted_channels_and_roles() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        Member::create(&harness.db, &server, &owner, Some(channels.clone())).await.unwrap();
        let general = channels[0].id().to_string();
        let path = format!("/servers/{}", server.id);
        let create = |path: String, body: serde_json::Value| {
            let harness = &harness;
            let token = owner_session.token.clone();
            async move {
                let response = harness.client.post(path).header(ContentType::JSON)
                    .header(Header::new("x-session-token", token)).body(body.to_string()).dispatch().await;
                response.into_json::<serde_json::Value>().await.unwrap()
            }
        };
        let doomed = create(format!("{path}/channels"), json!({ "type": "Text", "name": "doomed" })).await;
        let doomed = doomed["_id"].as_str().unwrap().to_string();
        let kept_role = create(format!("{path}/roles"), json!({ "name": "Kept" })).await;
        let kept_role = kept_role["id"].as_str().unwrap().to_string();
        let doomed_role = create(format!("{path}/roles"), json!({ "name": "Doomed" })).await;
        let doomed_role = doomed_role["id"].as_str().unwrap().to_string();

        let lobby = json!({ "lobby": {
            "enabled": true,
            "actions": [{ "label": "Gone", "channel": doomed }, { "label": "Stays", "channel": general }],
            "channels": [doomed, general],
            "staff_roles": [doomed_role, kept_role],
            "news_channel": doomed,
            "onboarding": [
                { "id": "q1", "question": "Pick", "options": [
                    { "id": "a", "label": "Kept", "role": kept_role },
                    { "id": "b", "label": "Doomed", "role": doomed_role }
                ] },
                { "id": "q2", "question": "Only doomed", "options": [
                    { "id": "c", "label": "Doomed", "role": doomed_role }
                ] }
            ]
        } });
        assert_eq!(patch(&harness, &owner_session.token, path.clone(), lobby.clone()).await, Status::Ok);

        for target in [format!("/channels/{doomed}"), format!("{path}/roles/{doomed_role}")] {
            let response = harness.client.delete(target)
                .header(Header::new("x-session-token", owner_session.token.clone())).dispatch().await;
            assert_eq!(response.status(), Status::NoContent);
        }

        // the manager's page still holds the old lobby and saves it unchanged
        assert_eq!(patch(&harness, &owner_session.token, path.clone(), lobby).await, Status::Ok);
        let stored = harness.db.fetch_server(&server.id).await.unwrap().lobby.unwrap();
        assert_eq!(stored.actions.len(), 1);
        assert_eq!(stored.channels, vec![general.clone()]);
        assert_eq!(stored.staff_roles, vec![kept_role.clone()]);
        assert_eq!(stored.news_channel, None);
        assert_eq!(stored.onboarding.len(), 1);
        assert_eq!(stored.onboarding[0].options.len(), 1);

        // an ID that was never part of the lobby is still refused
        let foreign = json!({ "lobby": { "enabled": true, "staff_roles": ["01ARZ3NDEKTSV4RRFFQ69G5FAV"] } });
        assert!(matches!(patch(&harness, &owner_session.token, path, foreign).await.code, 400 | 404 | 422));
    }

    /// Counters keep their order and appear once
    #[rocket::async_test]
    async fn stats_are_deduplicated() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        Member::create(&harness.db, &server, &owner, Some(channels.clone())).await.unwrap();

        assert_eq!(
            patch(
                &harness,
                &owner_session.token,
                format!("/servers/{}", server.id),
                json!({ "stats": ["Members", "Roles", "Members"] })
            )
            .await,
            Status::Ok
        );
        let stored = harness.db.fetch_server(&server.id).await.unwrap();
        assert_eq!(
            stored.stats,
            Some(vec![v0::ServerStat::Members, v0::ServerStat::Roles])
        );
    }

    /// Only people who can manage messages post in an announcement channel
    #[rocket::async_test]
    async fn announcement_channel_posting() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (_, member_session, member) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        Member::create(&harness.db, &server, &owner, Some(channels.clone())).await.unwrap();
        Member::create(&harness.db, &server, &member, Some(channels.clone())).await.unwrap();
        let channel = channels[0].id().to_string();

        assert_eq!(
            patch(&harness, &member_session.token, format!("/channels/{channel}"), json!({ "announcement": true })).await,
            Status::Forbidden
        );
        assert_eq!(
            patch(&harness, &owner_session.token, format!("/channels/{channel}"), json!({ "announcement": true })).await,
            Status::Ok
        );

        let send = |token: String| {
            let harness = &harness;
            let channel = channel.clone();
            async move {
                harness
                    .client
                    .post(format!("/channels/{channel}/messages"))
                    .header(ContentType::JSON)
                    .header(Header::new("x-session-token", token))
                    .body(json!({ "content": "news" }).to_string())
                    .dispatch()
                    .await
                    .status()
            }
        };
        assert_eq!(send(member_session.token.clone()).await, Status::Forbidden);
        assert_eq!(send(owner_session.token.clone()).await, Status::Ok);
    }

    /// Only the owner hands the server over, only to a person who is a
    /// member, and only with a fresh MFA ticket (Settings › Ownership)
    #[rocket::async_test]
    async fn ownership_transfer() {
        use revolt_database::MFATicket;

        let harness = TestHarness::new().await;
        let (owner_account, owner_session, owner) = harness.new_user().await;
        let (member_account, member_session, member) = harness.new_user().await;
        let (_, _, outsider) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        Member::create(&harness.db, &server, &member, Some(channels.clone()))
            .await
            .unwrap();
        let (_, bot) = harness.new_bot(&owner).await;
        Member::create(&harness.db, &server, &bot, Some(channels.clone()))
            .await
            .unwrap();
        let path = format!("/servers/{}", server.id);

        // each attempt with a ticket of the person making it
        async fn transfer(
            harness: &TestHarness,
            path: &str,
            token: &str,
            account_id: &str,
            to: &str,
        ) -> Status {
            let ticket = MFATicket::new(account_id.to_string(), true);
            ticket.save(&harness.db).await.unwrap();
            harness
                .client
                .patch(path.to_string())
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", token.to_string()))
                .header(Header::new("x-mfa-ticket", ticket.token.clone()))
                .body(json!({ "owner": to }).to_string())
                .dispatch()
                .await
                .status()
        }

        // without a ticket, even the owner is refused
        assert_eq!(
            patch(&harness, &owner_session.token, path.clone(), json!({ "owner": member.id })).await,
            Status::Unauthorized
        );
        // a member can't take it; not to someone outside; never to a bot
        let (owner_token, owner_id) = (&owner_session.token, &owner_account.id);
        assert_ne!(
            transfer(&harness, &path, &member_session.token, &member_account.id, &member.id).await,
            Status::Ok
        );
        assert_ne!(transfer(&harness, &path, owner_token, owner_id, &outsider.id).await, Status::Ok);
        assert_ne!(transfer(&harness, &path, owner_token, owner_id, &bot.id).await, Status::Ok);
        assert_eq!(harness.db.fetch_server(&server.id).await.unwrap().owner, owner.id);

        // the owner hands it to a member, and can't take it back after
        assert_eq!(transfer(&harness, &path, owner_token, owner_id, &member.id).await, Status::Ok);
        assert_eq!(harness.db.fetch_server(&server.id).await.unwrap().owner, member.id);
        assert_ne!(transfer(&harness, &path, owner_token, owner_id, &owner.id).await, Status::Ok);
    }

    /// An empty edit must not hand a server's details to someone outside it
    #[rocket::async_test]
    async fn empty_edit_by_non_member_is_not_found() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, outsider_session, _) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;

        let response = harness
            .client
            .patch(format!("/servers/{}", server.id))
            .header(ContentType::JSON)
            .header(Header::new(
                "x-session-token",
                outsider_session.token.to_string(),
            ))
            .body("{}")
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NotFound);
    }
}
