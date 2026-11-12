//! Who made a bot is the owner's choice (`show_creator`, on the bot's public
//! page, by name only). The owner's account id in `User.bot.owner` is sent
//! only to the owner, the bot itself and platform staff.

use revolt_database::PartialUser;
use rocket::http::{Header, Status};
use serde_json::Value;

use crate::util::test::TestHarness;

async fn bot_owner_as_seen_by(harness: &TestHarness, bot: &str, session: Option<&str>) -> Option<String> {
    let mut request = harness.client.get(format!("/users/{bot}"));
    if let Some(token) = session {
        request = request.header(Header::new("x-session-token", token.to_string()));
    }
    let response = request.dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    let user: Value = response.into_json().await.unwrap();
    assert!(user["bot"].is_object(), "still marked as a bot: {user}");
    user["bot"]["owner"].as_str().map(str::to_string)
}

#[rocket::async_test]
async fn a_bots_owner_id_is_only_shown_to_its_owner_and_staff() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, stranger_session, _) = harness.new_user().await;
    let (_, staff_session, staff) = harness.new_user().await;
    harness
        .db
        .update_user(
            &staff.id,
            &PartialUser {
                privileged: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (bot, _) = harness.new_bot(&owner).await;

    assert_eq!(bot_owner_as_seen_by(&harness, &bot.id, Some(&stranger_session.token)).await, None);
    assert_eq!(
        bot_owner_as_seen_by(&harness, &bot.id, Some(&owner_session.token)).await,
        Some(owner.id.clone())
    );
    assert_eq!(
        bot_owner_as_seen_by(&harness, &bot.id, Some(&staff_session.token)).await,
        Some(owner.id.clone())
    );
}

#[rocket::async_test]
async fn the_owner_still_gets_the_owner_id_of_their_bots() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;

    for path in ["/bots/@me".to_string(), format!("/bots/{}", bot.id)] {
        let response = harness
            .client
            .get(path.clone())
            .header(Header::new("x-session-token", owner_session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok, "{path}");
        let body: Value = response.into_json().await.unwrap();
        let user = if path == "/bots/@me" { &body["users"][0] } else { &body["user"] };
        assert_eq!(user["bot"]["owner"].as_str(), Some(owner.id.as_str()), "{path}");
    }
}

#[rocket::async_test]
async fn a_bot_sees_its_own_owner() {
    let harness = TestHarness::new().await;
    let (_, _, owner) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    let response = harness
        .client
        .get("/users/@me")
        .header(Header::new("x-bot-token", bot.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let user: Value = response.into_json().await.unwrap();
    assert_eq!(user["bot"]["owner"].as_str(), Some(owner.id.as_str()));
}
