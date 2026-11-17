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
