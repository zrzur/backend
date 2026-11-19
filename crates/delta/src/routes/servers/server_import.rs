//! "Copy a server setup": read a server layout out of a Discord server
//! template, show it, and build it. Channels, categories and roles only;
//! messages and members are never part of a template.
//!
//! The preview stores nothing. The build knows nothing about where a layout
//! came from: it checks it from scratch, like one a person wrote by hand.

use revolt_config::config;
use revolt_database::util::server_import::{
    check_structure, read_template, template_code, ImportError, ImportLimits, LinkError,
};
use revolt_database::util::server_import_fetch::{count_read, fetch_template};
use revolt_database::{
    Database, Member, MessageFilter, MessageQuery, MessageTimePeriod, Server, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};

use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

fn invalid(reason: &str) -> revolt_result::Error {
    create_error!(ImportInvalid { reason: reason.to_string() })
}

/// Who may use it at all, and the limits a layout is cut to
async fn open_to(user: &User) -> Result<ImportLimits> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let config = config().await;
    if !config.features.server_import.enabled {
        return Err(create_error!(FeatureDisabled {
            feature: "server_import".to_string()
        }));
    }
    let limits = &config.features.limits.global;
    Ok(ImportLimits {
        channels: limits.server_channels,
        roles: limits.server_roles,
    })
}

/// # Preview a Server Layout
///
/// Read the layout (categories, channels, roles) out of a Discord server
/// template: by its link, or from the template's text. Nothing is created
/// and nothing is kept.
#[openapi(tag = "Server Information")]
#[post("/import/preview", data = "<data>")]
pub async fn preview(
    user: User,
    data: Json<v0::DataImportPreview>,
) -> Result<Json<v0::ImportPreviewResponse>> {
    let limits = open_to(&user).await?;
    let data = data.into_inner();

    let text = match (data.text.filter(|text| !text.trim().is_empty()), data.link) {
        (Some(text), _) => text,
        (None, Some(link)) => {
            let code = template_code(&link).map_err(|error| match error {
                LinkError::Invite => invalid("invite_link"),
                LinkError::NotALink => invalid("not_a_link"),
            })?;
            let config = config().await;
            let import = &config.features.server_import;
            if !import.link_fetch {
                return Err(create_error!(ImportSourceUnavailable));
            }
            count_read(&user.id, import.reads_per_hour).await?;
            fetch_template(&import.template_endpoint, &code).await?
        }
        (None, None) => return Err(invalid("nothing_given")),
    };

    let imported = read_template(&text, limits).map_err(|error| match error {
        ImportError::TooLarge => invalid("too_large"),
        ImportError::NotATemplate => invalid("not_a_template"),
        ImportError::Empty => invalid("empty"),
    })?;
    Ok(Json(v0::ImportPreviewResponse {
        name: imported.name,
        structure: imported.structure,
        report: imported.report,
    }))
}

/// # Build a Server from a Layout
///
/// Create a server with the whole layout in this one request, or build it
/// into an empty server of the caller (`into`). It exists complete, or the
/// request fails and nothing is left behind.
#[openapi(tag = "Server Information")]
#[post("/import", data = "<data>")]
pub async fn import(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataImportServer>,
) -> Result<Json<v0::CreateServerLegacyResponse>> {
    let limits = open_to(&user).await?;
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    check_structure(&data.structure, limits).map_err(|error| {
        create_error!(FailedValidation {
            error: error.to_string()
        })
    })?;

    if let Some(target) = &data.into {
        let mut server = db.fetch_server(target).await?;
        if server.owner != user.id {
            return Err(create_error!(NotOwner));
        }
        let starting = db.fetch_channels(&server.channels).await?;
        if !server.is_unbuilt(&starting) {
            return Err(create_error!(ImportTargetNotEmpty));
        }
        // the channel it started with goes away: never one somebody wrote in
        for channel in &starting {
            let written = db
                .fetch_messages(MessageQuery {
                    limit: Some(1),
                    filter: MessageFilter {
                        channel: Some(channel.id().to_string()),
                        ..Default::default()
                    },
                    time_period: MessageTimePeriod::Absolute {
                        before: None,
                        after: None,
                        sort: Some(v0::MessageSort::Latest),
                    },
                })
                .await?;
            if !written.is_empty() {
                return Err(create_error!(ImportTargetNotEmpty));
            }
        }
        let channels = server.fill_from_structure(db, &data.structure, starting).await?;
        return Ok(Json(v0::CreateServerLegacyResponse {
            server: server.into(db).await,
            channels: channels.into_iter().map(|channel| channel.into()).collect(),
        }));
    }

    let config = config().await;
    let allowed = &config.features.limits.global.restrict_server_creation;
    if !allowed.is_empty() && !allowed.contains(&user.id) {
        return Err(create_error!(CantCreateServers));
    }
    user.can_acquire_server(db).await?;

    let (server, channels) = Server::create_from_structure(db, data, &user).await?;
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
    use revolt_database::{Channel, Server};
    use revolt_models::v0;
    use revolt_permissions::{ChannelPermission, OverrideField};
    use rocket::http::{ContentType, Header, Status};
    use rocket::local::asynchronous::LocalResponse;
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const VIEW: i64 = ChannelPermission::ViewChannel as i64;
    const SEND: i64 = ChannelPermission::SendMessage as i64 | ChannelPermission::CreateForumPosts as i64;

    /// The configuration can be set once in a test: everything goes here
    async fn switch_on_with(link_fetch: bool, endpoint: Option<String>, reads_per_hour: i64) {
        revolt_config::overwrite_config(move |config| {
            config.features.server_import.enabled = true;
            config.features.server_import.link_fetch = link_fetch;
            config.features.server_import.reads_per_hour = reads_per_hour;
            if let Some(endpoint) = endpoint {
                config.features.server_import.template_endpoint = endpoint;
            }
        })
        .await;
    }

    async fn switch_on(link_fetch: bool, endpoint: Option<String>) {
        switch_on_with(link_fetch, endpoint, 5).await;
    }

    async fn post<'a>(harness: &'a TestHarness, token: &str, path: &str, body: Value) -> LocalResponse<'a> {
        harness
            .client
            .post(format!("/servers/import{path}"))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.to_string()))
            .body(body.to_string())
            .dispatch()
            .await
    }

    async fn refused(response: LocalResponse<'_>, status: Status, kind: &str) -> Value {
        assert_eq!(response.status(), status);
        let body: Value = response.into_json().await.unwrap();
        assert_eq!(body["type"], kind, "{body}");
        body
    }

    /// A small community: a read-only category, a private one, a voice room
    fn template() -> Value {
        let bit = |position: u32| (1u64 << position).to_string();
        json!({
            "code": "hgM48av5Q69A",
            "creator": { "id": "132837293881950208", "username": "hoges" },
            "source_guild_id": "678070694164299796",
            "serialized_source_guild": {
                "name": "شام غيمرز",
                "description": "ألعاب وسهر",
                "system_channel_id": 3,
                "roles": json!(vec![
                    json!({ "id": 0, "name": "@everyone", "permissions": ((1u64 << 10) | (1 << 11) | (1 << 16)).to_string(), "color": 0 }),
                    json!({ "id": 1, "name": "عضو", "permissions": "0", "color": 0x93c5fd }),
                    json!({ "id": 2, "name": "مشرف", "permissions": ((1u64 << 1) | (1 << 13)).to_string(), "color": 9153776, "hoist": true }),
                    json!({ "id": 3, "name": "المؤسس", "permissions": bit(3), "color": 15972715, "hoist": true })
                ]),
                "channels": json!(vec![
                    json!({ "id": 1, "type": 4, "name": "الاستقبال", "position": 0, "permission_overwrites": [
                        { "id": 0, "type": 0, "allow": "0", "deny": bit(11) },
                        { "id": 2, "type": 0, "allow": bit(11), "deny": "0" } ] }),
                    json!({ "id": 2, "type": 0, "name": "القوانين", "position": 0, "topic": "اقرأها أولًا", "parent_id": 1, "permission_overwrites": [
                        { "id": 0, "type": 0, "allow": "0", "deny": bit(11) },
                        { "id": 2, "type": 0, "allow": bit(11), "deny": "0" } ] }),
                    json!({ "id": 3, "type": 0, "name": "عام", "position": 1, "rate_limit_per_user": 10, "parent_id": 1, "permission_overwrites": [] }),
                    json!({ "id": 4, "type": 2, "name": "الصالة", "position": 0, "user_limit": 5, "parent_id": 1, "permission_overwrites": [] }),
                    json!({ "id": 5, "type": 4, "name": "Staff", "position": 1, "permission_overwrites": [
                        { "id": 0, "type": 0, "allow": "0", "deny": bit(10) },
                        { "id": 2, "type": 0, "allow": bit(10), "deny": "0" } ] }),
                    json!({ "id": 6, "type": 0, "name": "staff-room", "position": 0, "nsfw": true, "parent_id": 5, "permission_overwrites": [
                        { "id": 0, "type": 0, "allow": "0", "deny": bit(10) },
                        { "id": 2, "type": 0, "allow": bit(10), "deny": "0" } ] }),
                    json!({ "id": 7, "type": 13, "name": "stage", "position": 5 })
                ])
            }
        })
    }

    async fn previewed(harness: &TestHarness, token: &str) -> v0::ImportPreviewResponse {
        let body = json!({ "text": template().to_string() });
        let response = post(harness, token, "/preview", body).await;
        assert_eq!(response.status(), Status::Ok);
        response.into_json().await.unwrap()
    }

    fn fields(channel: &Channel) -> (&str, &Option<OverrideField>, &std::collections::HashMap<String, OverrideField>, bool) {
        let Channel::TextChannel { name, default_permissions, role_permissions, permissions_synced, .. } = channel else {
            panic!("a server channel");
        };
        (name, default_permissions, role_permissions, *permissions_synced)
    }

    async fn stored(harness: &TestHarness, id: &str) -> (Server, Vec<Channel>) {
        let server = harness.db.fetch_server(id).await.unwrap();
        let channels = harness.db.fetch_channels(&server.channels).await.unwrap();
        let channels = server
            .channels
            .iter()
            .map(|id| channels.iter().find(|c| c.id() == id).unwrap().clone())
            .collect();
        (server, channels)
    }

    /// Off is the shipped state: both routes are closed
    #[rocket::async_test]
    async fn closed_while_the_feature_is_off() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let body = json!({ "text": template().to_string() });
        let response = post(&harness, &session.token, "/preview", body).await;
        let error = refused(response, Status::BadRequest, "FeatureDisabled").await;
        assert_eq!(error["feature"], "server_import");
        let body = json!({ "name": "x", "structure": {} });
        let response = post(&harness, &session.token, "", body).await;
        refused(response, Status::BadRequest, "FeatureDisabled").await;
        assert_eq!(harness.db.fetch_server_count(&user.id).await.unwrap(), 0);
    }

    #[rocket::async_test]
    async fn needs_a_session() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let response = post(&harness, "nobody", "/preview", json!({ "text": "{}" })).await;
        assert_eq!(response.status(), Status::Unauthorized);
        let response = post(&harness, "nobody", "", json!({ "name": "x", "structure": {} })).await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    /// The preview of pasted text: the layout, the report, nothing about the
    /// template's maker, and nothing created
    #[rocket::async_test]
    async fn preview_from_pasted_text() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let body = json!({ "text": template().to_string() });
        let response = post(&harness, &session.token, "/preview", body).await;
        assert_eq!(response.status(), Status::Ok);
        let text = response.into_string().await.unwrap();
        for secret in ["hoges", "132837293881950208", "678070694164299796", "hgM48av5Q69A"] {
            assert!(!text.contains(secret), "{secret} leaked");
        }
        let preview: v0::ImportPreviewResponse = serde_json::from_str(&text).unwrap();
        assert_eq!(preview.name, "شام غيمرز");
        assert_eq!(preview.report.categories, 2);
        assert_eq!(preview.report.channels, 4);
        assert_eq!(preview.report.roles, 3);
        assert_eq!(preview.report.held_back, ["المؤسس"]);
        assert_eq!(preview.report.skipped_channels, ["stage"]);
        assert_eq!(preview.report.private_channels, 1);
        assert_eq!(preview.report.mature_channels, 1);
        assert_eq!(harness.db.fetch_server_count(&user.id).await.unwrap(), 0);
    }

    #[rocket::async_test]
    async fn what_cannot_be_read_says_why() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        for (body, reason) in [
            (json!({}), "nothing_given"),
            (json!({ "text": "   " }), "nothing_given"),
            (json!({ "text": "hello" }), "not_a_template"),
            (json!({ "text": r#"{"message": "Unknown server template", "code": 10057}"# }), "not_a_template"),
            (json!({ "text": r#"{"roles": [], "channels": []}"# }), "empty"),
            (json!({ "link": "https://discord.gg/abc123" }), "invite_link"),
            (json!({ "link": "https://example.com/template/hgM48av5Q69A" }), "not_a_link"),
            (json!({ "link": "https://discord.new/../../users/@me" }), "not_a_link"),
        ] {
            let response = post(&harness, &session.token, "/preview", body.clone()).await;
            let error = refused(response, Status::BadRequest, "ImportInvalid").await;
            assert_eq!(error["reason"], reason, "{body}");
        }
    }

    /// With reading by link switched off, a good link asks for the text
    /// instead and no call is made
    #[rocket::async_test]
    async fn a_link_while_link_reading_is_off() {
        switch_on(false, Some("http://127.0.0.1:9/".to_string())).await;
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        let body = json!({ "link": "https://discord.new/hgM48av5Q69A" });
        let response = post(&harness, &session.token, "/preview", body).await;
        refused(response, Status::ServiceUnavailable, "ImportSourceUnavailable").await;
    }

    /// A stand-in for the template address: answers each request with what
    /// `answer` gives for its path, and records the requests it saw
    async fn stub(
        answer: fn(&str) -> (u16, String, &'static str),
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let log = log.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 2048];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match socket.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let request = String::from_utf8_lossy(&request).to_string();
                    let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                    log.lock().unwrap().push(request);
                    let (status, body, extra) = answer(&path);
                    if status == 0 {
                        // never answers
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        return;
                    }
                    let head = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (format!("http://{address}/api/v10/guilds/templates/"), seen)
    }

    fn answers(path: &str) -> (u16, String, &'static str) {
        match path.rsplit('/').next().unwrap_or("") {
            "hgM48av5Q69A" => (200, template().to_string(), ""),
            "gone00000000" => (404, r#"{"message": "Unknown server template", "code": 10057}"#.to_string(), ""),
            "limited00000" => (429, r#"{"retry_after": 5}"#.to_string(), ""),
            "refused00000" => (403, "{}".to_string(), ""),
            "broken000000" => (500, "oops".to_string(), ""),
            "moved0000000" => (302, String::new(), "location: /api/v10/guilds/templates/hgM48av5Q69A\r\n"),
            "huge00000000" => (200, "x".repeat(1_200_000), ""),
            "page00000000" => (200, "<html>not json</html>".to_string(), ""),
            "silent000000" => (0, String::new(), ""),
            _ => (404, "{}".to_string(), ""),
        }
    }

    /// The link path end to end against the stand-in: one call, to the one
    /// path, with no credential and an honest name
    #[rocket::async_test]
    async fn preview_from_a_link_makes_one_plain_call() {
        let (endpoint, seen) = stub(answers).await;
        switch_on(true, Some(endpoint)).await;
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let body = json!({ "link": "https://discord.new/hgM48av5Q69A" });
        let response = post(&harness, &session.token, "/preview", body).await;
        assert_eq!(response.status(), Status::Ok);
        let preview: v0::ImportPreviewResponse = response.into_json().await.unwrap();
        assert_eq!(preview.report.channels, 4);

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        let request = seen[0].to_lowercase();
        assert!(request.starts_with("get /api/v10/guilds/templates/hgm48av5q69a http/1.1"));
        assert!(request.contains("user-agent: zrzur-serverimport/1.0 (+https://zrzur.com/bot)"));
        for header in ["authorization", "cookie", "x-session-token", &session.token.to_lowercase(), &user.id.to_lowercase()] {
            assert!(!request.contains(header), "{header} was sent");
        }
    }

    #[rocket::async_test]
    async fn the_source_failing_is_told_apart() {
        let (endpoint, seen) = stub(answers).await;
        switch_on_with(true, Some(endpoint), 100).await;
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        for (code, status, kind) in [
            ("gone00000000", Status::NotFound, "ImportTemplateNotFound"),
            ("limited00000", Status::ServiceUnavailable, "ImportSourceUnavailable"),
            ("refused00000", Status::ServiceUnavailable, "ImportSourceUnavailable"),
            ("broken000000", Status::ServiceUnavailable, "ImportSourceUnavailable"),
            // a redirect is never followed
            ("moved0000000", Status::ServiceUnavailable, "ImportSourceUnavailable"),
            ("huge00000000", Status::BadRequest, "ImportInvalid"),
            ("page00000000", Status::BadRequest, "ImportInvalid"),
        ] {
            let body = json!({ "link": format!("https://discord.com/template/{code}") });
            let response = post(&harness, &session.token, "/preview", body).await;
            refused(response, status, kind).await;
        }
        // one call each: no retry, and the redirect's target was not asked for
        assert_eq!(seen.lock().unwrap().len(), 7);
    }

    #[rocket::async_test]
    async fn a_source_that_never_answers_times_out() {
        let (endpoint, _) = stub(answers).await;
        switch_on(true, Some(endpoint)).await;
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        let started = std::time::Instant::now();
        let body = json!({ "link": "silent000000" });
        let response = post(&harness, &session.token, "/preview", body).await;
        refused(response, Status::ServiceUnavailable, "ImportSourceUnavailable").await;
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    /// Link reads are counted per account; pasted text is not a call out
    #[rocket::async_test]
    async fn link_reads_have_an_hourly_limit() {
        let (endpoint, seen) = stub(answers).await;
        switch_on_with(true, Some(endpoint), 2).await;
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        let link = json!({ "link": "hgM48av5Q69A" });
        for _ in 0..2 {
            let response = post(&harness, &session.token, "/preview", link.clone()).await;
            assert_eq!(response.status(), Status::Ok);
        }
        let response = post(&harness, &session.token, "/preview", link.clone()).await;
        let error = refused(response, Status::TooManyRequests, "RateLimited").await;
        assert!(error["retry_after"].as_u64().unwrap() > 0);
        assert_eq!(seen.lock().unwrap().len(), 2);
        // another person is not held up, and text still reads
        let (_, other, _) = harness.new_user().await;
        let response = post(&harness, &other.token, "/preview", link).await;
        assert_eq!(response.status(), Status::Ok);
        previewed(&harness, &session.token).await;
    }

    /// What the preview showed is what gets built
    #[rocket::async_test]
    async fn builds_what_the_preview_showed() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let preview = previewed(&harness, &session.token).await;
        let body = json!({ "name": "Sham Gamers", "structure": preview.structure });
        let response = post(&harness, &session.token, "", body).await;
        assert_eq!(response.status(), Status::Ok);
        let made: v0::CreateServerLegacyResponse = response.into_json().await.unwrap();
        assert_eq!(made.channels.len(), 4);
        let (server, channels) = stored(&harness, &made.server.id).await;

        assert_eq!(server.name, "Sham Gamers");
        assert_eq!(server.owner, user.id);
        assert_eq!(server.description.as_deref(), Some("ألعاب وسهر"));
        assert!(!server.discoverable);
        assert_eq!(
            server.default_permissions,
            VIEW | SEND | ChannelPermission::ReadMessageHistory as i64
        );
        let member = harness.db.fetch_member(&server.id, &user.id).await.unwrap();
        assert!(member.roles.is_empty());

        let roles = server.ordered_roles();
        let names: Vec<&str> = roles.iter().map(|(_, role)| role.name.as_str()).collect();
        assert_eq!(names, ["المؤسس", "مشرف", "عضو"]);
        assert_eq!(roles[0].1.permissions, OverrideField { a: 0, d: 0 }, "administrator is not handed out");
        assert_eq!(roles[1].1.colour.as_deref(), Some("#8bacf0"));
        assert!(roles[1].1.hoist);
        assert_eq!(
            roles[1].1.permissions.a,
            ChannelPermission::KickMembers as i64 | ChannelPermission::ManageMessages as i64
        );
        let moderator = roles[1].0.clone();

        let categories = server.categories.clone().unwrap();
        assert_eq!(categories.len(), 2);
        assert_eq!(categories[0].title, "الاستقبال");
        assert_eq!(categories[0].channels, server.channels[..3]);
        assert_eq!(categories[0].default_permissions, Some(OverrideField { a: 0, d: SEND }));
        assert_eq!(categories[0].role_permissions[&moderator], OverrideField { a: SEND, d: 0 });
        assert_eq!(categories[1].channels, server.channels[3..]);

        // the rules room follows its category and is closed in its own document
        let (name, default, per_role, synced) = fields(&channels[0]);
        assert_eq!(name, "القوانين");
        assert!(synced);
        assert_eq!(default, &Some(OverrideField { a: 0, d: SEND }));
        assert_eq!(per_role[&moderator], OverrideField { a: SEND, d: 0 });
        let Channel::TextChannel { description, slowmode, .. } = &channels[0] else { unreachable!() };
        assert_eq!(description.as_deref(), Some("اقرأها أولًا"));
        assert_eq!(slowmode, &None);

        // the open room has its own (empty) rights, and its slow mode
        let (name, default, per_role, synced) = fields(&channels[1]);
        assert_eq!(name, "عام");
        assert!(!synced);
        assert!(default.is_none() && per_role.is_empty());
        let Channel::TextChannel { slowmode, .. } = &channels[1] else { unreachable!() };
        assert_eq!(slowmode, &Some(10));
        assert_eq!(server.system_messages.as_ref().unwrap().user_joined.as_deref(), Some(channels[1].id()));

        let Channel::TextChannel { voice, .. } = &channels[2] else { unreachable!() };
        assert_eq!(voice.as_ref().unwrap().max_users, Some(5));

        // the staff room is hidden from the first moment, and stays age-restricted
        let (name, default, per_role, _) = fields(&channels[3]);
        assert_eq!(name, "staff-room");
        assert_eq!(default, &Some(OverrideField { a: 0, d: VIEW }));
        assert_eq!(per_role[&moderator], OverrideField { a: VIEW, d: 0 });
        let Channel::TextChannel { nsfw, .. } = &channels[3] else { unreachable!() };
        assert!(nsfw);
    }

    /// A member with no role can't open the private room; the owner can
    #[rocket::async_test]
    async fn a_private_room_is_private_for_members() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        let preview = previewed(&harness, &session.token).await;
        let body = json!({ "name": "Private", "structure": preview.structure });
        let made: v0::CreateServerLegacyResponse =
            post(&harness, &session.token, "", body).await.into_json().await.unwrap();
        let (server, channels) = stored(&harness, &made.server.id).await;
        let (_, member, member_user) = harness.new_user().await;
        revolt_database::Member::create(&harness.db, &server, &member_user, None).await.unwrap();

        let get = |token: String, id: String| {
            harness.client.get(format!("/channels/{id}")).header(Header::new("x-session-token", token))
        };
        let staff = channels[3].id().to_string();
        assert_ne!(get(member.token.clone(), staff.clone()).dispatch().await.status(), Status::Ok);
        assert_eq!(get(session.token.clone(), staff).dispatch().await.status(), Status::Ok);
        let open = channels[1].id().to_string();
        assert_eq!(get(member.token.clone(), open).dispatch().await.status(), Status::Ok);
    }

    /// A layout a client changed after the preview is checked from scratch
    #[rocket::async_test]
    async fn a_tampered_layout_is_refused_and_nothing_is_left() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let good = serde_json::to_value(previewed(&harness, &session.token).await.structure).unwrap();
        let manage_server = ChannelPermission::ManageServer as u64;
        let changes: Vec<Box<dyn Fn(&mut Value)>> = vec![
            Box::new(move |s| s["roles"][0]["allow"] = json!(manage_server)),
            Box::new(|s| s["roles"][0]["allow"] = json!(u64::MAX)),
            Box::new(move |s| s["default_permissions"] = json!(manage_server)),
            Box::new(|s| s["roles"][0]["colour"] = json!("url(https://example.com/x.png)")),
            Box::new(|s| s["roles"][0]["name"] = json!("")),
            Box::new(|s| s["categories"][0]["title"] = json!("x".repeat(33))),
            Box::new(|s| s["categories"][0]["channels"][0]["name"] = json!("x".repeat(33))),
            Box::new(|s| s["categories"][0]["channels"][0]["description"] = json!("x".repeat(1025))),
            Box::new(|s| s["categories"][0]["channels"][0]["slowmode"] = json!(99999)),
            Box::new(|s| s["categories"][0]["channels"][0]["role_permissions"]["r404"] = json!({ "allow": 0, "deny": 0 })),
            Box::new(move |s| s["categories"][1]["default_permissions"] = json!({ "allow": manage_server, "deny": 0 })),
            Box::new(|s| s["system_channel"] = json!("c404")),
            Box::new(|s| {
                let room = s["categories"][0]["channels"][0].clone();
                s["channels"] = Value::Array((0..300).map(|i| { let mut r = room.clone(); r["key"] = json!(format!("x{i}")); r["synced"] = json!(false); r }).collect());
            }),
        ];
        for (index, change) in changes.iter().enumerate() {
            let mut structure = good.clone();
            change(&mut structure);
            let body = json!({ "name": "Tampered", "structure": structure });
            let response = post(&harness, &session.token, "", body).await;
            assert_eq!(response.status(), Status::BadRequest, "change {index}");
            let error: Value = response.into_json().await.unwrap();
            assert_eq!(error["type"], "FailedValidation", "change {index}");
        }
        for name in ["", &"x".repeat(33)] {
            let body = json!({ "name": name, "structure": good });
            let response = post(&harness, &session.token, "", body).await;
            assert_eq!(response.status(), Status::BadRequest);
        }
        assert_eq!(harness.db.fetch_server_count(&user.id).await.unwrap(), 0);
    }

    /// Into an empty server the caller owns: its starting channel goes, the
    /// layout comes, and the server keeps its name
    #[rocket::async_test]
    async fn builds_into_an_empty_server_of_the_owner() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (empty, starting) = harness.new_server(&user).await;
        revolt_database::Member::create(&harness.db, &empty, &user, Some(starting.clone())).await.unwrap();
        let preview = previewed(&harness, &session.token).await;
        let body = json!({ "name": "ignored", "structure": preview.structure, "into": empty.id });
        let response = post(&harness, &session.token, "", body.clone()).await;
        assert_eq!(response.status(), Status::Ok);
        let (server, channels) = stored(&harness, &empty.id).await;
        assert_eq!(server.name, "Test Server");
        assert_eq!(channels.len(), 4);
        assert_eq!(server.roles.len(), 3);
        assert_eq!(server.categories.as_ref().unwrap().len(), 2);
        assert!(harness.db.fetch_channel(starting[0].id()).await.is_err(), "the starting channel is gone");
        assert_eq!(harness.db.fetch_server_count(&user.id).await.unwrap(), 1);

        // a second time it is no longer empty
        let response = post(&harness, &session.token, "", body).await;
        refused(response, Status::Conflict, "ImportTargetNotEmpty").await;
        let (after, _) = stored(&harness, &empty.id).await;
        assert_eq!(after.channels, server.channels);
    }

    #[rocket::async_test]
    async fn never_into_someone_elses_server_or_a_used_one() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (_, _, other) = harness.new_user().await;
        let structure = serde_json::to_value(previewed(&harness, &session.token).await.structure).unwrap();

        let (theirs, _) = harness.new_server(&other).await;
        revolt_database::Member::create(&harness.db, &theirs, &user, None).await.unwrap();
        let body = json!({ "name": "x", "structure": structure, "into": theirs.id });
        let response = post(&harness, &session.token, "", body).await;
        assert_eq!(response.status(), Status::Forbidden);
        let (after, _) = stored(&harness, &theirs.id).await;
        assert!(after.roles.is_empty());
        assert_eq!(after.channels, theirs.channels);

        // mine, but someone wrote in it
        let (mine, channels) = harness.new_server(&user).await;
        harness.new_message(&user, &mine, channels).await;
        let body = json!({ "name": "x", "structure": structure, "into": mine.id });
        let response = post(&harness, &session.token, "", body).await;
        refused(response, Status::Conflict, "ImportTargetNotEmpty").await;

        // mine, with a role
        let (mine, _) = harness.new_server(&user).await;
        revolt_database::Role::create(&harness.db, &mine, "Helpers".to_string()).await.unwrap();
        let body = json!({ "name": "x", "structure": structure, "into": mine.id });
        let response = post(&harness, &session.token, "", body).await;
        refused(response, Status::Conflict, "ImportTargetNotEmpty").await;

        let body = json!({ "name": "x", "structure": structure, "into": "01NOSUCHSERVER0000000000000" });
        let response = post(&harness, &session.token, "", body).await;
        assert_eq!(response.status(), Status::NotFound);
    }

    #[rocket::async_test]
    async fn a_bot_cannot_use_it() {
        switch_on(false, None).await;
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (bot, _) = harness.new_bot(&owner).await;
        let response = harness
            .client
            .post("/servers/import/preview")
            .header(ContentType::JSON)
            .header(Header::new("x-bot-token", bot.token.clone()))
            .body(json!({ "text": template().to_string() }).to_string())
            .dispatch()
            .await;
        refused(response, Status::BadRequest, "IsBot").await;
    }
}
