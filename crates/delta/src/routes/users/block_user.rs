use revolt_database::util::reference::Reference;
use revolt_database::{Database, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;

/// # Block User
///
/// Block another user by their id.
#[openapi(tag = "Relationships")]
#[put("/<target>/block")]
pub async fn block(
    db: &State<Database>,
    mut user: User,
    target: Reference<'_>,
) -> Result<Json<v0::User>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let mut target = target.as_user(db).await?;

    user.block_user(db, &mut target).await?;
    // blocking a bot also stops it: after an unblock it waits for Start again
    if target.bot.is_some() {
        if let Ok(channel) = db.find_direct_message_channel(&user.id, &target.id).await {
            db.set_bot_started(channel.id(), &user.id, false).await?;
        }
    }
    Ok(Json(target.into(db, &user).await))
}
