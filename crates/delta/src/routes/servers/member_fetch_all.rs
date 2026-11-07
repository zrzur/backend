use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::PermissionQuery;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// Members per page when only `after` is given
const DEFAULT_PAGE: i64 = 100;
/// Most members one request returns
const MAX_PAGE: i64 = 1000;

/// # Fetch Members
///
/// Fetch server members, a page at a time (`limit`, `after`).
#[openapi(tag = "Server Members")]
#[get("/<target>/members?<options..>")]
pub async fn fetch_all(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: v0::OptionsFetchAllMembers,
) -> Result<Json<v0::AllMemberResponse>> {
    let server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    if !query.are_we_a_member().await {
        return Err(create_error!(NotFound));
    }

    // the member list is the Server Members intent's
    if revolt_database::enabled_intents(db, &user).await & v0::GatewayIntents::SERVER_MEMBERS == 0 {
        return Err(create_error!(MissingIntent {
            intent: "SERVER_MEMBERS".to_string()
        }));
    }

    // Paged by user id. The old unpaged call stays for one release, capped
    // at MAX_PAGE, so a member can't copy a huge member list in one request.
    let mut next = None;
    let mut members = if options.limit.is_some() || options.after.is_some() {
        let limit = options.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        let page = db
            .fetch_members_page(&server.id, options.after.as_deref(), limit)
            .await?;
        // a full page may have more after it (taken before exclude_offline)
        if page.len() as i64 == limit {
            next = page.last().map(|member| member.id.user.clone());
        }
        page
    } else {
        let mut all = db.fetch_all_members(&server.id).await?;
        if all.len() as i64 > MAX_PAGE {
            log::warn!(
                "unpaged member list of server {} ({} members) is deprecated: capped at {MAX_PAGE}",
                server.id,
                all.len()
            );
            all.sort_by(|a, b| a.id.user.cmp(&b.id.user));
            all.truncate(MAX_PAGE as usize);
        }
        all
    };

    let user_ids: Vec<String> = members
        .iter()
        .map(|member| member.id.user.clone())
        .collect();

    let mut users = User::fetch_many_ids_as_mutuals(db, &user, &user_ids).await?;

    // Ensure the lists match up exactly.
    members.sort_by(|a, b| a.id.user.cmp(&b.id.user));
    users.sort_by(|a, b| a.id.cmp(&b.id));

    // before the filter: `exclude_offline` must not reveal who is online
    revolt_database::hide_presence(db, &user, &mut users).await;

    // Optionally, remove all offline user entries.
    if let Some(true) = options.exclude_offline {
        users.retain(|user| user.online);
        let online: std::collections::HashSet<&str> =
            users.iter().map(|user| user.id.as_str()).collect();
        members.retain(|member| online.contains(member.id.user.as_str()));
    }

    Ok(Json(v0::AllMemberResponse {
        members: members.into_iter().map(Into::into).collect(),
        users,
        next,
    }))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Bot, Member, PartialBot};
    use revolt_models::v0::GatewayIntents;
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn the_member_list_needs_the_server_members_intent() {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        let (mut bot, bot_user) = harness.new_bot(&owner).await;
        // the test server has no member entry for its owner; a real one does
        Member::create(&harness.db, &server, &owner, None).await.unwrap();
        Member::create(&harness.db, &server, &bot_user, None).await.unwrap();

        let list = |token: String, header: &'static str| {
            harness
                .client
                .get(format!("/servers/{}/members", server.id))
                .header(Header::new(header, token))
        };
        let response = list(bot.token.clone(), "x-bot-token").dispatch().await;
        assert_eq!(response.status(), Status::Forbidden);
        assert!(response.into_string().await.unwrap().contains("MissingIntent"));

        // nor can it search the list without it
        let search = format!(
            "/servers/{}/members_experimental_query?query=a&experimental_api=true",
            server.id
        );
        let response = harness
            .client
            .get(&search)
            .header(Header::new("x-bot-token", bot.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Forbidden);
        assert!(response.into_string().await.unwrap().contains("MissingIntent"));

        // people need no intent
        let response = list(session.token.clone(), "x-session-token").dispatch().await;
        assert_eq!(response.status(), Status::Ok);

        // once the owner enables it, the bot gets the list
        bot.update(
            &harness.db,
            PartialBot {
                privileged_intents: Some(GatewayIntents::SERVER_MEMBERS as i64),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
        let response = list(bot.token.clone(), "x-bot-token").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
    }
}
