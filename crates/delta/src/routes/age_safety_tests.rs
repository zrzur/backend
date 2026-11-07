//! Age safety across the API: mature content must be refused on every path,
//! whatever the client shows or claims.
use crate::{rocket, util::test::TestHarness};
use iso8601_timestamp::Timestamp;
use revolt_database::{Account, AccountAge, Channel, Member, PartialServer, Server, Session, User};
use revolt_models::v0;
use revolt_result::{Error, ErrorType};
use rocket::http::{ContentType, Header, Status};

/// Year and month for someone who turned `years` this month
fn born(years: i32) -> (i32, u8) {
    let now = Timestamp::now_utc();
    (now.year() - years, u8::from(now.month()))
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

/// A mature Server (owned by a declared adult) that `members` already
/// belong to, e.g. because it was marked mature after they joined
async fn mature_server(harness: &TestHarness, owner: &User, members: &[&User]) -> (Server, Channel) {
    let (server, channels) = Server::create(
        &harness.db,
        v0::DataCreateServer {
            name: "Mature".to_string(),
            nsfw: Some(true),
            ..Default::default()
        },
        owner,
        true,
    )
    .await
    .unwrap();

    for member in members {
        Member::create(&harness.db, &server, member, None)
            .await
            .unwrap();
    }

    (server, channels[0].clone())
}

fn token(session: &Session) -> Header<'static> {
    Header::new("X-Session-Token", session.token.clone())
}

/// An account without a declared age reaches nothing until it declares one
async fn assert_age_not_declared(res: rocket::local::asynchronous::LocalResponse<'_>) {
    assert_eq!(res.status(), Status::Forbidden);
    assert!(matches!(
        res.into_json::<Error>().await.unwrap().error_type,
        ErrorType::AgeNotDeclared,
    ));
}

async fn assert_age_restricted(res: rocket::local::asynchronous::LocalResponse<'_>) {
    assert_eq!(res.status(), Status::Forbidden);
    assert!(matches!(
        res.into_json::<Error>().await.unwrap().error_type,
        ErrorType::AgeRestricted,
    ));
}

#[rocket::async_test]
async fn minors_and_unknown_ages_cannot_reach_mature_content() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = person(&harness, Some(30)).await;
    let (_, teen_session, teen) = person(&harness, Some(17)).await;
    let (_, unknown_session, unknown) = person(&harness, None).await;
    let (_, channel) = mature_server(&harness, &owner, &[&teen, &unknown]).await;
    let id = channel.id();

    // the owner (declared adult) posts a message
    let res = harness
        .client
        .post(format!("/channels/{id}/messages"))
        .header(ContentType::JSON)
        .header(token(&owner_session))
        .body(json!({ "content": "hello" }).to_string())
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    let message: v0::Message = res.into_json().await.unwrap();

    // without a declared age nothing is reached, mature or not
    let res = harness
        .client
        .get(format!("/channels/{id}/messages"))
        .header(token(&unknown_session))
        .dispatch()
        .await;
    assert_age_not_declared(res).await;

    for session in [&teen_session] {
        // history
        let res = harness
            .client
            .get(format!("/channels/{id}/messages"))
            .header(token(session))
            .dispatch()
            .await;
        assert_age_restricted(res).await;

        // a single message by id
        let res = harness
            .client
            .get(format!("/channels/{id}/messages/{}", message.id))
            .header(token(session))
            .dispatch()
            .await;
        assert_age_restricted(res).await;

        // search
        let res = harness
            .client
            .post(format!("/channels/{id}/search"))
            .header(ContentType::JSON)
            .header(token(session))
            .body(json!({ "query": "hello" }).to_string())
            .dispatch()
            .await;
        assert_age_restricted(res).await;

        // sending
        let res = harness
            .client
            .post(format!("/channels/{id}/messages"))
            .header(ContentType::JSON)
            .header(token(session))
            .body(json!({ "content": "hi" }).to_string())
            .dispatch()
            .await;
        assert_age_restricted(res).await;
    }

    // the declared adult still can
    let res = harness
        .client
        .get(format!("/channels/{id}/messages"))
        .header(token(&owner_session))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
}

#[rocket::async_test]
async fn minors_cannot_join_mature_server_by_invite() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = person(&harness, Some(30)).await;
    let (_, teen_session, _) = person(&harness, Some(16)).await;
    let (_, adult_session, _) = person(&harness, Some(40)).await;
    let (_, channel) = mature_server(&harness, &owner, &[]).await;

    let res = harness
        .client
        .post(format!("/channels/{}/invites", channel.id()))
        .header(ContentType::JSON)
        .header(token(&owner_session))
        .body("{}")
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
    let invite: serde_json::Value = res.into_json().await.unwrap();
    let code = invite["_id"].as_str().unwrap().to_string();

    let res = harness
        .client
        .post(format!("/invites/{code}"))
        .header(token(&teen_session))
        .dispatch()
        .await;
    assert_age_restricted(res).await;

    let res = harness
        .client
        .post(format!("/invites/{code}"))
        .header(token(&adult_session))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
}

#[rocket::async_test]
async fn mature_server_hidden_in_discover_for_minors_and_visitors() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = person(&harness, Some(30)).await;
    let (_, teen_session, _) = person(&harness, Some(17)).await;
    let (_, adult_session, _) = person(&harness, Some(30)).await;
    let (mut server, _) = mature_server(&harness, &owner, &[]).await;
    server
        .update(
            &harness.db,
            PartialServer {
                discoverable: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let path = format!("/discover/servers/{}", server.id);

    // logged out
    let res = harness.client.get(path.clone()).dispatch().await;
    assert_eq!(res.status(), Status::NotFound);

    // teen
    let res = harness
        .client
        .get(path.clone())
        .header(token(&teen_session))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::NotFound);

    // declared adult
    let res = harness
        .client
        .get(path)
        .header(token(&adult_session))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Ok);
}

#[rocket::async_test]
async fn ordinary_content_waits_for_a_declared_age() {
    let harness = TestHarness::new().await;
    let (mut account, session, user) = person(&harness, None).await;
    let (_, channels) = harness.new_server(&user).await;
    let path = format!("/channels/{}/messages", channels[0].id());

    let res = harness.client.get(path.clone()).header(token(&session)).dispatch().await;
    assert_age_not_declared(res).await;

    // a declared teen reads ordinary channels
    account.age = Some(AccountAge::declare(born(16)));
    account.save(&harness.db).await.unwrap();
    let res = harness.client.get(path).header(token(&session)).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
}

#[rocket::async_test]
async fn strangers_cannot_open_a_dm_with_a_teen() {
    let harness = TestHarness::new().await;
    let (_, stranger_session, stranger) = person(&harness, Some(30)).await;
    let (_, _, teen) = person(&harness, Some(16)).await;

    // even sharing a Server is not enough: only friends can start a DM
    let (server, _) = harness.new_server(&stranger).await;
    Member::create(&harness.db, &server, &teen, None)
        .await
        .unwrap();

    let res = harness
        .client
        .get(format!("/users/{}/dm", teen.id))
        .header(token(&stranger_session))
        .dispatch()
        .await;
    assert_eq!(res.status(), Status::Forbidden);
}
