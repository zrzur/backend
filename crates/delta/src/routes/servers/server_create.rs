use std::sync::LazyLock;

use revolt_config::{config, Settings};
use revolt_database::{Database, Member, Server, User};
use revolt_models::v0;
use revolt_permissions::ChannelPermission;
use revolt_result::{create_error, Result};

use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// Most channels and roles a blueprint can hold, below the server limits
const BLUEPRINT_CHANNELS: usize = 50;
const BLUEPRINT_ROLES: usize = 20;

/// All a role from a blueprint may hold: everyday rights and moderation of
/// members and channels. Control over the server (its settings, permissions,
/// roles, bans, webhooks, audit log, join requests) stays with the owner, who
/// can hand it out afterwards, and a new permission stays out until it is
/// listed here.
static BLUEPRINT_ROLE_RIGHTS: LazyLock<u64> = LazyLock::new(|| {
    [
        ChannelPermission::ManageChannel,
        ChannelPermission::UseExternalEmojis,
        ChannelPermission::KickMembers,
        ChannelPermission::TimeoutMembers,
        ChannelPermission::AssignRoles,
        ChannelPermission::ChangeNickname,
        ChannelPermission::ManageNicknames,
        ChannelPermission::ChangeAvatar,
        ChannelPermission::RemoveAvatars,
        ChannelPermission::ViewChannel,
        ChannelPermission::ReadMessageHistory,
        ChannelPermission::SendMessage,
        ChannelPermission::ManageMessages,
        ChannelPermission::InviteOthers,
        ChannelPermission::SendEmbeds,
        ChannelPermission::UploadFiles,
        ChannelPermission::Masquerade,
        ChannelPermission::React,
        ChannelPermission::BypassSlowmode,
        ChannelPermission::Connect,
        ChannelPermission::Speak,
        ChannelPermission::Video,
        ChannelPermission::MuteMembers,
        ChannelPermission::DeafenMembers,
        ChannelPermission::MoveMembers,
        ChannelPermission::Listen,
        ChannelPermission::MentionEveryone,
        ChannelPermission::MentionRoles,
        ChannelPermission::CreateThreads,
        ChannelPermission::SendMessageInThreads,
        ChannelPermission::CreateForumPosts,
    ]
    .into_iter()
    .fold(0, |rights, permission| rights | permission as u64)
});

/// What the field rules can't see: counts over the whole blueprint, the
/// server limits, the rules channel, and what a role may hold
fn check_blueprint(blueprint: &v0::DataServerBlueprint, config: &Settings) -> Result<()> {
    let refuse = |error: &str| {
        Err(create_error!(FailedValidation {
            error: error.to_string()
        }))
    };

    let limits = &config.features.limits.global;
    let rooms = || {
        blueprint
            .channels
            .iter()
            .chain(blueprint.categories.iter().flat_map(|c| &c.channels))
    };
    let max = BLUEPRINT_CHANNELS.min(limits.server_channels);
    if rooms().count() > max {
        return Err(create_error!(TooManyChannels { max }));
    }
    let max = BLUEPRINT_ROLES.min(limits.server_roles);
    if blueprint.roles.len() > max {
        return Err(create_error!(TooManyRoles { max }));
    }

    if rooms().any(|room| matches!(room.channel_type, v0::LegacyServerChannelType::Forum)) {
        return refuse("a blueprint makes text and voice channels only");
    }
    if rooms().filter(|room| room.rules).count() > 1 {
        return refuse("a server has one rules channel");
    }
    let text = |room: &v0::BlueprintChannel| {
        matches!(room.channel_type, v0::LegacyServerChannelType::Text)
    };
    if rooms().any(|room| room.rules && !text(room)) {
        return refuse("the rules channel is a text channel");
    }
    if blueprint.roles.iter().any(|role| role.allow & !*BLUEPRINT_ROLE_RIGHTS != 0) {
        return refuse("a role in a blueprint can't hold server-wide control");
    }
    Ok(())
}

/// # Create Server
///
/// Create a new server, optionally built from a blueprint in this one request.
#[openapi(tag = "Server Information")]
#[post("/create", data = "<data>")]
pub async fn create_server(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataCreateServer>,
) -> Result<Json<v0::CreateServerLegacyResponse>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let config = config().await;

    if !config
        .features
        .limits
        .global
        .restrict_server_creation
        .is_empty()
        && !config
            .features
            .limits
            .global
            .restrict_server_creation
            .contains(&user.id)
    {
        return Err(create_error!(CantCreateServers));
    }

    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    if let Some(blueprint) = &data.blueprint {
        check_blueprint(blueprint, &config)?;
    }

    user.can_acquire_server(db).await?;

    let (server, channels) = Server::create(db, data, &user, true).await?;
    let channels = match Member::create(db, &server, &user, Some(channels.clone())).await {
        Ok((_, channels)) => channels,
        Err(error) => {
            server.discard(db, &channels).await;
            return Err(error);
        }
    };

    Ok(Json(v0::CreateServerLegacyResponse {
        server: server.into(db).await,
        channels: channels.into_iter().map(|channel| channel.into()).collect(),
    }))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Channel, Database, File, Metadata, Server};
    use revolt_models::v0;
    use revolt_permissions::{ChannelPermission, OverrideField};
    use rocket::http::{ContentType, Header, Status};
    use rocket::local::asynchronous::LocalResponse;
    use serde_json::{json, Value};

    const SEND: i64 = ChannelPermission::SendMessage as i64;
    const INVITE: i64 = ChannelPermission::InviteOthers as i64;
    const MODERATOR: u64 = ChannelPermission::ManageMessages as u64
        | ChannelPermission::TimeoutMembers as u64
        | ChannelPermission::KickMembers as u64
        | ChannelPermission::AssignRoles as u64;

    async fn post<'a>(harness: &'a TestHarness, token: &str, body: Value) -> LocalResponse<'a> {
        harness
            .client
            .post("/servers/create")
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.to_string()))
            .body(body.to_string())
            .dispatch()
            .await
    }

    /// Creates a server from this blueprint; the answer must be a success
    async fn built(harness: &TestHarness, blueprint: Value) -> (Server, Vec<Channel>) {
        let (_, session, _) = harness.new_user().await;
        let body = json!({ "name": "Built", "blueprint": blueprint });
        let response = post(harness, &session.token, body).await;
        assert_eq!(response.status(), Status::Ok);
        let body: v0::CreateServerLegacyResponse = response.into_json().await.unwrap();
        let server = harness.db.fetch_server(&body.server.id).await.unwrap();
        let channels = harness.db.fetch_channels(&server.channels).await.unwrap();
        // in the order the blueprint gave
        let channels = server
            .channels
            .iter()
            .map(|id| channels.iter().find(|c| c.id() == id).unwrap().clone())
            .collect();
        (server, channels)
    }

    /// A refused blueprint: the status, the error type, and nothing left over
    async fn refused(harness: &TestHarness, blueprint: Value, status: Status, kind: &str) {
        let (_, session, user) = harness.new_user().await;
        let body = json!({ "name": "Refused", "blueprint": blueprint });
        let response = post(harness, &session.token, body).await;
        assert_eq!(response.status(), status, "{blueprint}");
        let error: Value = response.into_json().await.unwrap();
        assert_eq!(error["type"], kind, "{blueprint}");
        assert_eq!(harness.db.fetch_server_count(&user.id).await.unwrap(), 0);
    }

    fn gaming() -> Value {
        json!({
            "categories": [
                { "title": "Start here", "channels": [
                    { "name": "rules", "type": "Text", "read_only": true, "rules": true },
                    { "name": "announcements", "type": "Text", "read_only": true },
                ] },
                { "title": "Community", "channels": [
                    { "name": "General", "type": "Text" },
                    { "name": "clips", "type": "Text" },
                ] },
                { "title": "Gaming rooms", "channels": [
                    { "name": "Lobby", "type": "Voice" },
                    { "name": "Squad", "type": "Voice" },
                ] },
            ],
            "roles": [
                {
                    "name": "Moderators",
                    "colour": "#8b5cf6",
                    "hoist": true,
                    "allow": MODERATOR,
                    "posts_in_read_only": true,
                },
                { "name": "Members", "colour": "#93c5fd", "allow": 0 },
            ],
        })
    }

    fn rooms(count: usize) -> Value {
        Value::Array((0..count).map(|i| json!({ "name": format!("room-{i}") })).collect())
    }

    fn name(channel: &Channel) -> String {
        let Channel::TextChannel { name, .. } = channel else {
            panic!("a server channel");
        };
        name.clone()
    }

    /// The whole server exists after one request
    #[rocket::async_test]
    async fn builds_everything_at_once() {
        let harness = TestHarness::new().await;
        let (server, channels) = built(&harness, gaming()).await;

        let names: Vec<String> = channels.iter().map(name).collect();
        assert_eq!(names, ["rules", "announcements", "General", "clips", "Lobby", "Squad"]);
        assert!(channels[0].voice().is_none());
        assert!(channels[4].voice().is_some());
        assert!(channels[5].voice().is_some());

        let categories = server.categories.as_ref().unwrap();
        let titles: Vec<&str> = categories.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Start here", "Community", "Gaming rooms"]);
        for (category, ids) in categories.iter().zip(server.channels.chunks(2)) {
            assert_eq!(category.channels, ids);
            assert!(!category.id.is_empty());
        }
        assert_eq!(server.rules_channel.as_deref(), Some(channels[0].id()));

        let roles = server.ordered_roles();
        assert_eq!(roles[0].1.name, "Moderators");
        assert_eq!(roles[0].1.colour.as_deref(), Some("#8b5cf6"));
        assert!(roles[0].1.hoist);
        assert_eq!(roles[0].1.permissions, OverrideField { a: MODERATOR as i64, d: 0 });
        // ranked in the given order, so moderators stay above members
        assert_eq!(roles[1].1.name, "Members");
        assert!(roles[0].1.rank < roles[1].1.rank);
        assert!(!roles[1].1.hoist);
        assert_eq!(roles[1].1.permissions, OverrideField { a: 0, d: 0 });
    }

    /// A read-only room is closed in the stored document itself, and only
    /// the roles that may post in it hold the allow
    #[rocket::async_test]
    async fn read_only_rooms_are_closed_from_the_start() {
        let harness = TestHarness::new().await;
        let (server, channels) = built(&harness, gaming()).await;
        let moderators = server.ordered_roles()[0].0.clone();
        for channel in &channels[..2] {
            let Channel::TextChannel {
                default_permissions,
                role_permissions,
                permissions_synced,
                ..
            } = channel
            else {
                panic!("a text channel");
            };
            assert_eq!(*default_permissions, Some(OverrideField { a: 0, d: SEND }));
            // its own rights: a change to the category never reopens it
            assert!(!permissions_synced);
            assert_eq!(role_permissions.len(), 1);
            assert_eq!(role_permissions[&moderators], OverrideField { a: SEND, d: 0 });
        }
        for channel in &channels[2..] {
            let Channel::TextChannel {
                default_permissions,
                role_permissions,
                permissions_synced,
                ..
            } = channel
            else {
                panic!("a text channel");
            };
            assert_eq!(*default_permissions, None);
            assert!(permissions_synced);
            assert!(role_permissions.is_empty());
        }
    }

    /// Class or course: closed to member invites, approval to join
    #[rocket::async_test]
    async fn invites_off_and_access() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        let body = json!({
            "name": "Class",
            "access": "Approval",
            "blueprint": { "channels": [{ "name": "general" }], "member_invites": false },
        });
        let response = post(&harness, &session.token, body).await;
        assert_eq!(response.status(), Status::Ok);
        let body: v0::CreateServerLegacyResponse = response.into_json().await.unwrap();
        let server = harness.db.fetch_server(&body.server.id).await.unwrap();
        assert_eq!(server.access, Some(v0::ServerAccess::Approval));
        assert_eq!(server.default_permissions & INVITE, 0);
        assert_ne!(server.default_permissions & SEND, 0);
    }

    /// Without categories the rooms stay in the given order, and the default
    /// General room is not added next to them
    #[rocket::async_test]
    async fn rooms_without_categories() {
        let harness = TestHarness::new().await;
        let blueprint = json!({
            "channels": [{ "name": "عام", "type": "Text" }, { "name": "استراحة", "type": "Voice" }],
        });
        let (server, channels) = built(&harness, blueprint).await;
        assert_eq!(channels.len(), 2);
        assert_eq!(name(&channels[0]), "عام");
        assert_eq!(name(&channels[1]), "استراحة");
        assert!(server.categories.unwrap_or_default().is_empty());
        assert_eq!(server.rules_channel, None);
        assert_ne!(server.default_permissions & INVITE, 0);
    }

    #[rocket::async_test]
    async fn no_rooms_keeps_the_default_room() {
        let harness = TestHarness::new().await;
        let (_, channels) = built(&harness, json!({})).await;
        assert_eq!(channels.len(), 1);
        assert_eq!(name(&channels[0]), "General");
    }

    #[rocket::async_test]
    async fn without_a_blueprint_nothing_changes() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        let response = post(&harness, &session.token, json!({ "name": "Plain" })).await;
        assert_eq!(response.status(), Status::Ok);
        let body: v0::CreateServerLegacyResponse = response.into_json().await.unwrap();
        assert_eq!(body.channels.len(), 1);
        assert!(body.server.roles.is_empty());
    }

    /// Roles may never hold server-wide control, nor a bit nobody has named
    #[rocket::async_test]
    async fn roles_cannot_hold_server_wide_control() {
        let harness = TestHarness::new().await;
        for bit in [
            ChannelPermission::ManageServer as u64,
            ChannelPermission::ManagePermissions as u64,
            ChannelPermission::ManageRole as u64,
            ChannelPermission::ManageCustomisation as u64,
            ChannelPermission::BanMembers as u64,
            ChannelPermission::ManageWebhooks as u64,
            ChannelPermission::ViewAuditLogs as u64,
            ChannelPermission::ManageJoinRequests as u64,
            1 << 5,
            1 << 50,
            ChannelPermission::GrantAllSafe as u64,
        ] {
            let blueprint = json!({ "roles": [{ "name": "Boss", "allow": MODERATOR | bit }] });
            refused(&harness, blueprint, Status::BadRequest, "FailedValidation").await;
        }
    }

    /// The rights of the class template stay possible
    #[rocket::async_test]
    async fn teacher_role_is_allowed() {
        let harness = TestHarness::new().await;
        let teacher = ChannelPermission::ManageChannel as u64
            | ChannelPermission::ManageMessages as u64
            | ChannelPermission::InviteOthers as u64
            | ChannelPermission::KickMembers as u64
            | ChannelPermission::TimeoutMembers as u64
            | ChannelPermission::AssignRoles as u64
            | ChannelPermission::ManageNicknames as u64;
        let blueprint = json!({ "roles": [{ "name": "Teacher", "allow": teacher }] });
        let (server, _) = built(&harness, blueprint).await;
        assert_eq!(server.ordered_roles()[0].1.permissions.a, teacher as i64);
    }

    /// The same rules as the separate routes
    #[rocket::async_test]
    async fn same_validation_as_the_separate_routes() {
        let harness = TestHarness::new().await;
        let long = "x".repeat(33);
        for blueprint in [
            json!({ "channels": [{ "name": long }] }),
            json!({ "channels": [{ "name": "" }] }),
            json!({ "categories": [{ "title": long, "channels": [] }] }),
            json!({ "categories": [{ "title": "", "channels": [] }] }),
            json!({ "categories": [{ "title": "One", "channels": [{ "name": long }] }] }),
            json!({ "roles": [{ "name": long, "allow": 0 }] }),
            json!({ "roles": [{ "name": "", "allow": 0 }] }),
            json!({ "roles": [{ "name": "Red", "colour": "url(x)", "allow": 0 }] }),
        ] {
            refused(&harness, blueprint, Status::BadRequest, "FailedValidation").await;
        }
    }

    /// The rules room is one text room; a blueprint makes no forums
    #[rocket::async_test]
    async fn refuses_what_a_template_cannot_be() {
        let harness = TestHarness::new().await;
        for blueprint in [
            json!({ "channels": [
                { "name": "one", "rules": true },
                { "name": "two", "rules": true },
            ] }),
            json!({ "channels": [{ "name": "talk", "type": "Voice", "rules": true }] }),
            json!({ "channels": [{ "name": "ideas", "type": "Forum" }] }),
        ] {
            refused(&harness, blueprint, Status::BadRequest, "FailedValidation").await;
        }
    }

    #[rocket::async_test]
    async fn limits_on_counts() {
        let harness = TestHarness::new().await;
        let limit = Status::BadRequest;
        refused(&harness, json!({ "channels": rooms(51) }), limit, "TooManyChannels").await;
        let split = json!({
            "channels": rooms(30),
            "categories": [{ "title": "More", "channels": rooms(21) }],
        });
        refused(&harness, split, limit, "TooManyChannels").await;
        let roles: Vec<Value> = (0..21)
            .map(|i| json!({ "name": format!("role-{i}"), "allow": 0 }))
            .collect();
        refused(&harness, json!({ "roles": roles }), limit, "TooManyRoles").await;
        let categories: Vec<Value> = (0..21)
            .map(|i| json!({ "title": format!("c{i}"), "channels": [] }))
            .collect();
        let many = json!({ "categories": categories });
        refused(&harness, many, limit, "FailedValidation").await;

        let (_, channels) = built(&harness, json!({ "channels": rooms(50) })).await;
        assert_eq!(channels.len(), 50);
    }

    /// The icon lands in the same request; a bad one leaves nothing behind
    #[rocket::async_test]
    async fn icon_in_the_same_request() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let icon = File {
            id: "ICON".to_string(),
            tag: "icons".to_string(),
            filename: "icon.png".to_string(),
            hash: None,
            metadata: Metadata::File,
            content_type: "image/png".to_string(),
            size: 1,
            deleted: None,
            reported: None,
            purged: None,
            message_id: None,
            object_id: None,
            server_id: None,
            user_id: None,
            uploaded_at: None,
            uploader_id: None,
            used_for: None,
        };
        harness.db.insert_attachment(&icon).await.unwrap();
        let body = |icon: &str| {
            json!({
                "name": "Pictured",
                "blueprint": { "icon": icon, "channels": [{ "name": "general" }] },
            })
        };

        let response = post(&harness, &session.token, body("NOPE")).await;
        assert_eq!(response.status(), Status::NotFound);
        assert_eq!(harness.db.fetch_server_count(&user.id).await.unwrap(), 0);

        let response = post(&harness, &session.token, body("ICON")).await;
        assert_eq!(response.status(), Status::Ok);
        let created: v0::CreateServerLegacyResponse = response.into_json().await.unwrap();
        let server = harness.db.fetch_server(&created.server.id).await.unwrap();
        assert_eq!(server.icon.map(|file| file.id).as_deref(), Some("ICON"));
    }

    /// A step that fails takes back what came before it, and never what
    /// already existed
    #[rocket::async_test]
    async fn a_failed_step_leaves_nothing() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (first, channels) = harness.new_server(&user).await;
        let existing = channels[0].clone();

        // rooms of a server that was never saved can be taken back on both drivers
        let mut unsaved = existing.clone();
        if let Channel::TextChannel { id, server, .. } = &mut unsaved {
            *id = "UNSAVED".to_string();
            *server = "NO-SUCH-SERVER".to_string();
        }
        harness.db.insert_channel(&unsaved).await.unwrap();
        let ids = ["UNSAVED".to_string()];
        harness.db.delete_unattached_channels(&ids).await.unwrap();
        assert!(harness.db.fetch_channel("UNSAVED").await.is_err());

        // debug builds panic on a MongoDB write error (the query macro), so
        // the failing writes below only run on the in-memory driver
        if matches!(harness.db, Database::MongoDb(_)) {
            return;
        }

        // the second room can't be saved: the first goes again, no server
        let half = Server { id: "HALF".to_string(), channels: vec![], ..first.clone() };
        let mut room = existing.clone();
        if let Channel::TextChannel { id, .. } = &mut room {
            *id = "ROOM".to_string();
        }
        let rooms = [room.clone(), existing.clone()];
        assert!(half.save_with_channels(&harness.db, &rooms).await.is_err());
        assert!(harness.db.fetch_channel("ROOM").await.is_err());
        assert!(harness.db.fetch_channel(existing.id()).await.is_ok());
        assert!(harness.db.fetch_server("HALF").await.is_err());

        // the server can't be saved: the rooms go again
        let rooms = std::slice::from_ref(&room);
        assert!(first.save_with_channels(&harness.db, rooms).await.is_err());
        assert!(harness.db.fetch_channel("ROOM").await.is_err());
        assert!(harness.db.fetch_server(&first.id).await.is_ok());
    }

    /// A server that could not be finished goes with its rooms
    #[rocket::async_test]
    async fn discard_removes_the_rooms_too() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let id = server.id.clone();
        server.discard(&harness.db, &channels).await;
        assert!(harness.db.fetch_server(&id).await.is_err());
        assert!(harness.db.fetch_channel(channels[0].id()).await.is_err());
    }
}
