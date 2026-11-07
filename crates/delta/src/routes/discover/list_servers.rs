use iso8601_timestamp::Timestamp;
use revolt_database::{
    active_promotions, epoch_ms, note_discover_browser, record_promotion_event, Database,
    DiscoverRequestType, Invite, PromotionStat, Server, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use super::listing::{label_card, ranked_servers, server_card, with_online};

/// Mature Servers are only listed to signed-in people who may open them
/// (logged-out visitors and accounts without a declared adult age don't see them)
async fn show_mature(db: &Database, user: Option<&User>) -> bool {
    match user {
        Some(user) => revolt_database::util::age::may_open_mature(db, user)
            .await
            .unwrap_or(false),
        None => false,
    }
}

/// # List Discover servers
///
/// Lists servers approved for Discover. Public: also used by the
/// standalone Discover website, so no session is required.
#[openapi(tag = "Discover")]
#[get("/servers?<options..>")]
pub async fn list_servers(
    db: &State<Database>,
    user: Option<User>,
    options: v0::OptionsDiscoverQuery,
) -> Result<Json<Vec<v0::DiscoverServer>>> {
    options.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // signed-in browsing feeds the "people browse Discover" figure
    if let Some(user) = user.as_ref().filter(|user| user.bot.is_none()) {
        note_discover_browser(&user.id).await;
    }

    let mature = show_mature(db, user.as_ref()).await;
    let page = ranked_servers(db, Some(&options))
        .await?
        .into_iter()
        .filter(|card| mature || !card.nsfw)
        .skip(options.offset.unwrap_or(0) as usize)
        .take(options.limit.unwrap_or(50) as usize)
        .collect();
    Ok(Json(with_online(db, page).await))
}

/// The server a public page names: its ID, or its custom link name
pub(crate) async fn server_by_id_or_link(db: &Database, id: &str) -> Result<Server> {
    if let Ok(server) = db.fetch_server(id).await {
        return Ok(server);
    }
    match db.fetch_invite(&id.to_lowercase()).await {
        Ok(Invite::Server {
            vanity: true,
            server,
            ..
        }) => db.fetch_server(&server).await,
        _ => Err(create_error!(NotFound)),
    }
}

/// # Fetch Discover server
///
/// Fetches a single server listed on Discover, by its ID or its custom
/// link name.
#[openapi(tag = "Discover")]
#[get("/servers/<id>")]
pub async fn fetch_server(
    db: &State<Database>,
    user: Option<User>,
    id: &str,
) -> Result<Json<v0::DiscoverServer>> {
    let server = server_by_id_or_link(db, id).await?;
    if !server.is_listed() || (server.nsfw && !show_mature(db, user.as_ref()).await) {
        return Err(create_error!(NotFound));
    }

    let request = db
        .fetch_discover_request_by_item_id(DiscoverRequestType::Server, &server.id)
        .await
        .ok();
    let card = label_card(db, server_card(db, server, request.as_ref()).await).await;
    Ok(Json(with_online(db, vec![card]).await.remove(0)))
}

/// # Discover top row
///
/// Servers in the paid top row, oldest placement first, and how many
/// places are free. Servers that aren't listed (locked, taken off
/// Discover) don't show.
#[openapi(tag = "Discover")]
#[get("/promoted")]
pub async fn promoted(
    db: &State<Database>,
    user: Option<User>,
) -> Result<Json<v0::DiscoverPromotedRow>> {
    let mature = show_mature(db, user.as_ref()).await;
    let billing = revolt_config::config().await.features.billing;
    let now = epoch_ms(Timestamp::now_utc());
    let promotions = active_promotions(db).await?;

    let mut placed: Vec<_> = promotions
        .values()
        .filter_map(|promotion| promotion.promoted.clone())
        .filter(|item| item.ends_at > now && item.target_kind == "server")
        .collect();
    placed.sort_by_key(|item| item.started_at);
    let taken = placed.len();

    let mut servers = Vec::new();
    for item in placed {
        let Ok(server) = db.fetch_server(&item.server_id).await else {
            continue;
        };
        if !server.is_listed() || (server.nsfw && !mature) {
            continue;
        }
        let request = db
            .fetch_discover_request_by_item_id(DiscoverRequestType::Server, &server.id)
            .await
            .ok();
        let mut card = server_card(db, server, request.as_ref()).await;
        card.promoted = true;
        card.boost = promotions
            .get(&card.id)
            .and_then(|promotion| promotion.boost.as_ref().map(|(kind, _)| kind.clone()));
        servers.push(card);
    }

    let slots = if billing.available() {
        billing.discover.promoted_slots
    } else {
        0
    };
    Ok(Json(v0::DiscoverPromotedRow {
        slots: slots as u32,
        open: slots.saturating_sub(taken) as u32,
        servers: with_online(db, servers).await,
    }))
}

/// # Discover listing seen or opened
///
/// Counts a view of a top-row server or a click on it, once a day per
/// person. Other servers are ignored.
#[openapi(tag = "Discover")]
#[post("/servers/<id>/events", data = "<data>")]
pub async fn server_event(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataDiscoverEvent>,
) -> Result<EmptyResponse> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let stat = match data.kind.as_str() {
        "view" => PromotionStat::Views,
        "click" => PromotionStat::Clicks,
        _ => return Err(create_error!(InvalidOperation)),
    };
    record_promotion_event(db, id, &user.id, stat).await?;
    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use redis_kiss::AsyncCommands;
    use revolt_database::{Invite, Member, PartialServer};
    use revolt_models::v0;
    use rocket::http::Status;

    #[rocket::async_test]
    async fn lists_only_discoverable_servers() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (listed, _) = harness.new_server(&user).await;
        let (hidden, _) = harness.new_server(&user).await;

        harness
            .db
            .update_server(
                &listed.id,
                &PartialServer {
                    discoverable: Some(true),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("Failed to make server discoverable");

        // No session: the listing is public
        let response = harness.client.get("/discover/servers?limit=100").dispatch().await;

        assert_eq!(response.status(), Status::Ok);
        let servers: Vec<v0::DiscoverServer> = response.into_json().await.expect("body");
        assert!(servers.iter().any(|s| s.id == listed.id));
        assert!(!servers.iter().any(|s| s.id == hidden.id));
    }

    #[rocket::async_test]
    async fn fetches_a_listed_server_by_its_custom_link() {
        let harness = TestHarness::new().await;
        let (_, _, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let code = format!("rabea{}", &server.id[20..].to_lowercase());

        harness
            .db
            .insert_invite(&Invite::Server {
                code: code.clone(),
                server: server.id.clone(),
                creator: user.id.clone(),
                channel: channels[0].id().to_string(),
                max_uses: None,
                uses: 0,
                expires: None,
                vanity: true,
            })
            .await
            .expect("custom link");
        harness
            .db
            .update_server(
                &server.id,
                &PartialServer {
                    discoverable: Some(true),
                    vanity_invite: Some(v0::ServerVanityInvite {
                        code: code.clone(),
                        channel: channels[0].id().to_string(),
                        claimed_at: iso8601_timestamp::Timestamp::now_utc(),
                        below_level_since: None,
                    }),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("listed");

        // by link (any case) and by ID, without a session
        for path in [code.to_uppercase(), server.id.clone()] {
            let response = harness
                .client
                .get(format!("/discover/servers/{path}"))
                .dispatch()
                .await;
            assert_eq!(response.status(), Status::Ok);
            let card: v0::DiscoverServer = response.into_json().await.expect("body");
            assert_eq!(card.id, server.id);
            assert_eq!(card.link.as_deref(), Some(code.as_str()));
            // too few members to show a number without telling who is online
            assert_eq!(card.online_count, None);
        }

        // an unknown name is simply not found
        let response = harness
            .client
            .get("/discover/servers/no-such-link")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);
    }

    #[rocket::async_test]
    async fn online_count_needs_ten_members() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        for _ in 0..10 {
            let (_, _, user) = harness.new_user().await;
            Member::create(&harness.db, &server, &user, Some(channels.clone()))
                .await
                .expect("member");
        }
        harness
            .db
            .update_server(
                &server.id,
                &PartialServer {
                    discoverable: Some(true),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("listed");
        // the member count is cached for an hour, from the first join
        let mut redis = redis_kiss::get_connection().await.expect("redis");
        let _: () = redis
            .del(format!("member_count:{}", server.id))
            .await
            .expect("cached count");

        let response = harness
            .client
            .get(format!("/discover/servers/{}", server.id))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let card: v0::DiscoverServer = response.into_json().await.expect("body");
        assert_eq!(card.online_count, Some(0));
    }
}
