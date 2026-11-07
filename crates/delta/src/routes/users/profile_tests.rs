//! Profile fields and sections, through the real routes.

use iso8601_timestamp::Timestamp;
use revolt_database::{
    Account, AccountAge, Channel, Member, RelationshipStatus, Server, Session, User,
};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Method, Status};

use crate::util::test::TestHarness;

const SECRET_LINK: &str = "https://secret-link.example/me";

/// Year and month for someone who is `years` old
fn born(years: i32) -> (i32, u8) {
    let now = Timestamp::now_utc();
    // a birthday only counts once its month is over
    (now.year() - years - 1, u8::from(now.month()))
}

async fn person(harness: &TestHarness, age: Option<i32>) -> (Account, Session, User) {
    let Some(years) = age else {
        return harness.new_user_without_age().await;
    };
    let (mut account, session, user) = harness.new_user().await;
    account.age = Some(AccountAge::declare(born(years)));
    account.save(&harness.db).await.unwrap();
    (account, session, user)
}

async fn call(
    harness: &TestHarness,
    method: Method,
    session: &Session,
    path: &str,
    body: Option<serde_json::Value>,
) -> (Status, String) {
    let mut request = harness
        .client
        .req(method, path.to_string())
        .header(Header::new("x-session-token", session.token.clone()));
    if let Some(body) = body {
        request = request.header(ContentType::JSON).body(body.to_string());
    }
    let response = request.dispatch().await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

async fn befriend(harness: &TestHarness, a: &mut User, b: &mut User) {
    #[allow(clippy::disallowed_methods)]
    a.apply_relationship(
        &harness.db,
        b,
        RelationshipStatus::Friend,
        RelationshipStatus::Friend,
    )
    .await
    .unwrap();
}

/// Share a group, so the profile can be opened at all
async fn share(harness: &TestHarness, a: &User, b: &User) -> Channel {
    let mut group = Channel::create_group(
        &harness.db,
        v0::DataCreateGroup {
            name: TestHarness::rand_string(),
            ..Default::default()
        },
        a.id.to_string(),
    )
    .await
    .unwrap();
    group
        .add_user_to_group(&harness.db, &harness.amqp, b, &a.id)
        .await
        .unwrap();
    group
}

async fn fill_profile(harness: &TestHarness, session: &Session) {
    let (status, body) = call(
        harness,
        Method::Patch,
        session,
        "/users/@me",
        Some(serde_json::json!({ "profile": {
            "content": "SECRET-BIO",
            "links": [SECRET_LINK],
            "interests": ["music"],
        }})),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
}

async fn hide(harness: &TestHarness, session: &Session, sections: &[&str]) {
    let (status, body) = call(
        harness,
        Method::Patch,
        session,
        "/users/@me",
        Some(serde_json::json!({ "profile": { "hidden_sections": sections } })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
}

fn assert_clean(body: &str) {
    for secret in ["SECRET-BIO", "secret-link", "\"music\"", "hidden_sections"] {
        assert!(!body.contains(secret), "{secret} leaked in {body}");
    }
}

#[rocket::async_test]
async fn the_profile_route_leaves_hidden_sections_out_for_everyone_else() {
    let harness = TestHarness::new().await;
    let (_, owner_session, mut owner) = person(&harness, Some(30)).await;
    let (_, friend_session, mut friend) = person(&harness, Some(30)).await;
    let (_, stranger_session, stranger) = person(&harness, Some(30)).await;
    befriend(&harness, &mut owner, &mut friend).await;
    share(&harness, &owner, &stranger).await;
    fill_profile(&harness, &owner_session).await;

    // nothing hidden: both can read
    let (_, body) = call(&harness, Method::Get, &stranger_session,
        &format!("/users/{}/profile", owner.id), None).await;
    assert!(body.contains("SECRET-BIO") && body.contains("secret-link"), "{body}");

    hide(&harness, &owner_session, &["about", "links", "interests"]).await;

    for session in [&friend_session, &stranger_session] {
        let (status, body) = call(&harness, Method::Get, session,
            &format!("/users/{}/profile", owner.id), None).await;
        assert_eq!(status, Status::Ok, "{body}");
        assert_clean(&body);
        let profile: v0::UserProfile = serde_json::from_str(&body).unwrap();
        let sections = profile.sections.unwrap();
        for id in ["about", "links", "interests"] {
            assert!(!sections.iter().any(|s| s == id), "{id}");
        }
    }

    // the person still has all of it, and is the only one told what is hidden
    let (_, body) = call(&harness, Method::Get, &owner_session,
        &format!("/users/{}/profile", owner.id), None).await;
    let profile: v0::UserProfile = serde_json::from_str(&body).unwrap();
    assert_eq!(profile.links, vec![SECRET_LINK.to_string()]);
    assert_eq!(profile.hidden_sections.len(), 3);
}

#[rocket::async_test]
async fn a_minors_links_and_interests_go_to_friends_only() {
    for age in [Some(17), None] {
        let harness = TestHarness::new().await;
        let (_, owner_session, mut owner) = person(&harness, age).await;
        let (_, friend_session, mut friend) = person(&harness, Some(30)).await;
        let (_, stranger_session, stranger) = person(&harness, Some(30)).await;
        befriend(&harness, &mut owner, &mut friend).await;
        share(&harness, &owner, &stranger).await;

        // an account with no age can't call the API, so fill it in directly
        let mut filled = harness.db.fetch_user(&owner.id).await.unwrap();
        filled.profile = Some(revolt_database::UserProfile {
            links: vec![SECRET_LINK.to_string()],
            interests: vec!["music".to_string()],
            ..Default::default()
        });
        harness
            .db
            .update_user(
                &owner.id,
                &revolt_database::PartialUser {
                    profile: filled.profile.clone(),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        let _ = &owner_session;

        let path = format!("/users/{}/profile", owner.id);
        let (_, body) = call(&harness, Method::Get, &friend_session, &path, None).await;
        assert!(body.contains("secret-link"), "{body}");

        let (status, body) = call(&harness, Method::Get, &stranger_session, &path, None).await;
        assert_eq!(status, Status::Ok);
        assert!(!body.contains("secret-link") && !body.contains("\"music\""), "{body}");
        // nothing says why: no age, and it looks like a hidden section
        for word in ["age", "minor", "band"] {
            assert!(!body.to_lowercase().contains(word), "{word} in {body}");
        }
    }
}

/// The other ways a user reaches someone: none of these types has a profile
/// field, so a change that adds one fails here.
#[rocket::async_test]
async fn user_objects_never_carry_profile_fields() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = person(&harness, Some(30)).await;
    let (_, viewer_session, viewer) = person(&harness, Some(30)).await;
    share(&harness, &owner, &viewer).await;
    fill_profile(&harness, &owner_session).await;
    hide(&harness, &owner_session, &["about", "links", "interests"]).await;

    // fetch user, single and as a person's own object
    let (status, body) = call(&harness, Method::Get, &viewer_session,
        &format!("/users/{}", owner.id), None).await;
    assert_eq!(status, Status::Ok);
    assert_clean(&body);
    let (_, body) = call(&harness, Method::Get, &owner_session, "/users/@me", None).await;
    assert_clean(&body);

    // what Ready and member lists are built from
    let stored = harness.db.fetch_user(&owner.id).await.unwrap();
    let known = stored.clone().into_known(&viewer, false).await;
    assert_clean(&serde_json::to_string(&known).unwrap());
    let own = stored.clone().into_self(false).await;
    assert_clean(&serde_json::to_string(&own).unwrap());

    // members with user objects
    let (server, _) = Server::create(
        &harness.db,
        v0::DataCreateServer {
            name: TestHarness::rand_string(),
            ..Default::default()
        },
        &viewer,
        false,
    )
    .await
    .unwrap();
    Member::create(&harness.db, &server, &viewer, None).await.unwrap();
    Member::create(&harness.db, &server, &stored, None).await.unwrap();
    let (status, body) = call(&harness, Method::Get, &viewer_session,
        &format!("/servers/{}/members", server.id), None).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_clean(&body);

    // the update event others receive
    let event: v0::PartialUser = revolt_database::PartialUser {
        profile: stored.profile.clone(),
        ..Default::default()
    }
    .into();
    assert_clean(&serde_json::to_string(&event).unwrap());
}

#[rocket::async_test]
async fn a_bad_link_is_refused_and_a_stored_one_never_leaves() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = person(&harness, Some(30)).await;
    let (_, viewer_session, viewer) = person(&harness, Some(30)).await;
    share(&harness, &owner, &viewer).await;

    for link in ["javascript:alert(1)", "data:text/html,hi", "http://example.com"] {
        let (status, _) = call(&harness, Method::Patch, &owner_session, "/users/@me",
            Some(serde_json::json!({ "profile": { "links": [link] } }))).await;
        assert_eq!(status, Status::BadRequest, "{link}");
    }

    // a value that got in some other way
    harness
        .db
        .update_user(
            &owner.id,
            &revolt_database::PartialUser {
                profile: Some(revolt_database::UserProfile {
                    links: vec!["javascript:alert(1)".to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (_, body) = call(&harness, Method::Get, &viewer_session,
        &format!("/users/{}/profile", owner.id), None).await;
    assert!(!body.contains("javascript"), "{body}");
}

#[rocket::async_test]
async fn an_effect_is_one_of_our_own_or_a_real_community_effect() {
    let harness = TestHarness::new().await;
    let (_, session, _) = person(&harness, Some(30)).await;
    let (status, body) = call(&harness, Method::Patch, &session, "/users/@me",
        Some(serde_json::json!({ "style": { "effect": "snow" } }))).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body.contains("snow"));
    let (status, _) = call(&harness, Method::Patch, &session, "/users/@me",
        Some(serde_json::json!({ "style": { "effect": "c:01ARZ3NDEKTSV4RRFFQ69G5FAV" } }))).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = call(&harness, Method::Patch, &session, "/users/@me",
        Some(serde_json::json!({ "style": { "effect": "no-such-effect" } }))).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn the_word_list_reads_all_public_profile_text() {
    let harness = TestHarness::new().await;
    let (_, session, _) = person(&harness, Some(30)).await;
    if harness.db.fetch_platform_words().await.unwrap().is_empty() {
        for word in revolt_database::platform_word_seed(0).expect("the starter file reads") {
            harness.db.insert_platform_word(&word).await.unwrap();
        }
    }
    for body in [
        serde_json::json!({ "display_name": "kys" }),
        serde_json::json!({ "pronouns": "kys" }),
        serde_json::json!({ "status": { "text": "kys" } }),
        serde_json::json!({ "profile": { "content": "just kill yourself" } }),
        serde_json::json!({ "profile": { "content": "du hurensohn" } }),
    ] {
        let (status, text) = call(&harness, Method::Patch, &session, "/users/@me", Some(body.clone())).await;
        assert_eq!(status, Status::UnprocessableEntity, "{body}: {text}");
        assert!(text.contains("ProfileTextFiltered"), "{text}");
    }
    let (status, text) = call(&harness, Method::Patch, &session, "/users/@me",
        Some(serde_json::json!({ "profile": { "content": "a kind word, سوف انتحر" } }))).await;
    assert_eq!(status, Status::Ok, "{text}");

    let edit = |link: &str| serde_json::json!({ "profile": { "links": [link] } });

    let (status, body) = call(&harness, Method::Patch, &session, "/users/@me",
        Some(edit("https://example.org/kys"))).await;
    assert_eq!(status, Status::UnprocessableEntity, "{body}");
    assert!(body.contains("ProfileTextFiltered") && body.contains("HateOrThreats"), "{body}");
    let (status, body) = call(&harness, Method::Patch, &session, "/users/@me",
        Some(edit("https://example.org/انتحر"))).await;
    assert_eq!(status, Status::Ok, "{body}");
}
