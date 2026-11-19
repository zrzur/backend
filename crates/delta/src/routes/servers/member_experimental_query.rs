use futures::future::join_all;
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, Member, User,
};
use revolt_models::v0;
use revolt_permissions::PermissionQuery;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};

/// # Query Parameters
#[derive(Deserialize, JsonSchema, FromForm)]
pub struct OptionsQueryMembers {
    /// String to search for
    query: String,

    /// Discourage use of this API
    experimental_api: bool,
}

/// # Query members by name
#[derive(Serialize, JsonSchema)]
pub struct MemberQueryResponse {
    /// List of members
    members: Vec<v0::Member>,
    /// List of users
    users: Vec<v0::User>,
}

/// # Query members by name
///
/// Query members by a given name, this API is not stable and will be removed in the future.
#[openapi(tag = "Server Members")]
#[get("/<target>/members_experimental_query?<options..>")]
pub async fn member_experimental_query(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: OptionsQueryMembers,
) -> Result<Json<MemberQueryResponse>> {
    if !options.experimental_api {
        return Err(create_error!(InternalError));
    }

    let server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    if !query.are_we_a_member().await {
        return Err(create_error!(NotFound));
    }

    // searching the member list needs the same intent as reading it
    if revolt_database::enabled_intents(db, &user).await & v0::GatewayIntents::SERVER_MEMBERS == 0 {
        return Err(create_error!(MissingIntent {
            intent: "SERVER_MEMBERS".to_string()
        }));
    }

    let mut members = db.fetch_all_members(&server.id).await?;

    let mut user_ids = vec![];
    for member in &members {
        user_ids.push(member.id.user.clone());
    }

    let mut users = db.fetch_users(&user_ids).await?;

    // Ensure the lists match up exactly
    members.sort_by(|a, b| a.id.user.cmp(&b.id.user));
    users.sort_by(|a, b| a.id.cmp(&b.id));

    // Filter all matches
    let mut zipped_vec: Vec<(Member, User)> = members
        .into_iter()
        .zip(users)
        .filter(|(member, user)| {
            // the name shown in the server, the display name or the username, in
            // any spelling (case, alef forms, ta marbuta, vowel marks, digits)
            [
                member.nickname.as_deref(),
                user.display_name.as_deref(),
                Some(user.username.as_str()),
            ]
            .into_iter()
            .flatten()
            .any(|name| revolt_database::util::arabic_fold::text_matches(name, &options.query))
        })
        .collect();

    // Sort remaining matches by length
    zipped_vec.sort_by(|(member_a, user_a), (member_b, user_b)| {
        let left = member_a.nickname.as_ref().unwrap_or(&user_a.username);
        let right = member_b.nickname.as_ref().unwrap_or(&user_b.username);
        left.len().cmp(&right.len())
    });

    // Take the first ten and return them
    let (members, users): (Vec<Member>, Vec<User>) = zipped_vec.into_iter().take(10).unzip();
    let mut users = join_all(
        users
            .into_iter()
            .map(|other_user| other_user.into(db, &user)),
    )
    .await;
    revolt_database::hide_presence(db, &user, &mut users).await;
    Ok(Json(MemberQueryResponse {
        members: members.into_iter().map(Into::into).collect(),
        users,
    }))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Member, PartialUser};
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn a_member_is_found_however_the_name_is_typed() {
        let harness = TestHarness::new().await;
        let (_, session, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;
        Member::create(&harness.db, &server, &owner, None).await.unwrap();

        let (_, _, mut ahmad) = harness.new_user().await;
        ahmad
            .update(
                &harness.db,
                PartialUser {
                    display_name: Some("أحمد الخطيب".to_string()),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        Member::create(&harness.db, &server, &ahmad, None).await.unwrap();

        // (typed, found): the display name counts, in any spelling
        for (typed, found) in [
            ("أحمد", true),
            ("احمد", true),
            ("أَحمد", true),
            ("خطيب", true),
            ("الخطيب", true),
            ("سلمى", false),
        ] {
            let response = harness
                .client
                .get(format!(
                    "/servers/{}/members_experimental_query?query={}&experimental_api=true",
                    server.id,
                    rocket::http::RawStr::new(typed).percent_encode()
                ))
                .header(Header::new("x-session-token", session.token.clone()))
                .dispatch()
                .await;
            assert_eq!(response.status(), Status::Ok);
            let body = response.into_string().await.unwrap();
            assert_eq!(body.contains(&ahmad.id), found, "{typed}");
        }
    }
}
