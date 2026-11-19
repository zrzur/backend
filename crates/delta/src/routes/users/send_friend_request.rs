// use revolt_database::util::reference::Reference;
use revolt_database::{Database, User, AMQP, UNIQUE_DISCRIMINATOR};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;

/// # Send Friend Request
///
/// Send a friend request to another user.
#[openapi(tag = "Relationships")]
#[post("/friend", data = "<data>")]
pub async fn send_friend_request(
    db: &State<Database>,
    amqp: &State<AMQP>,
    mut user: User,
    data: Json<v0::DataSendFriendRequest>,
) -> Result<Json<v0::User>> {
    // not before the e-mail of an account made from an invite link is confirmed
    revolt_database::Account::assert_confirmed(db, &user.id).await?;

    // "name" finds the account that owns that username; "name#1234" finds an
    // account that still has its old tag
    let (username, discriminator) = data
        .username
        .split_once('#')
        .unwrap_or((data.username.as_str(), UNIQUE_DISCRIMINATOR));
    if username.is_empty() || discriminator.is_empty() {
        return Err(create_error!(InvalidProperty));
    }

    let mut target = db.fetch_user_by_username(username, discriminator).await?;

    if user.bot.is_some() || target.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    user.add_friend(db, amqp, &mut target).await?;
    Ok(Json(target.into(db, &user).await))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::PartialUser;
    use rocket::http::{ContentType, Header, Status};

    async fn ask(harness: &TestHarness, token: &str, name: &str) -> Status {
        harness
            .client
            .post("/users/friend")
            .header(Header::new("x-session-token", token.to_string()))
            .header(ContentType::JSON)
            .body(json!({ "username": name }).to_string())
            .dispatch()
            .await
            .status()
    }

    #[rocket::async_test]
    async fn a_person_is_found_by_name_alone_and_an_old_account_by_its_tag() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        let (_, _, owner_of_name) = harness.new_user().await;
        let (_, _, sdk_target) = harness.new_user().await;
        let (_, _, mut old) = harness.new_user().await;

        // an account from before unique usernames: a number tag
        old.update(
            &harness.db,
            PartialUser {
                discriminator: Some("1151".to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

        // the name alone, in other capitals too
        assert_eq!(
            ask(&harness, &session.token, &owner_of_name.username.to_uppercase()).await,
            Status::Ok
        );
        // the form older apps and the SDK send for the same account
        assert_eq!(
            ask(&harness, &session.token, &format!("{}#0000", sdk_target.username)).await,
            Status::Ok
        );
        // an old account is not found by name alone, only with its tag
        assert_eq!(ask(&harness, &session.token, &old.username).await, Status::NotFound);
        assert_eq!(
            ask(&harness, &session.token, &format!("{}#1151", old.username)).await,
            Status::Ok
        );
        // nothing typed before or after the #
        assert_eq!(ask(&harness, &session.token, "#1151").await, Status::BadRequest);
        assert_eq!(
            ask(&harness, &session.token, &format!("{}#", old.username)).await,
            Status::BadRequest
        );
        assert_eq!(ask(&harness, &session.token, "nobody_by_this_name").await, Status::NotFound);
    }
}
