use revolt_database::{util::reference::Reference, Database, DiscoverRequestType, User};
use revolt_result::{create_error, Result};
use rocket::State;
use rocket_empty::EmptyResponse;

use crate::routes::discover::listing;

/// # Remove bot from Discover
///
/// Cancels your request, or takes your bot off Discover if it is listed.
/// Owners can do this at any time; listing again needs a new review.
/// Bots removed by moderators need support.
#[openapi(tag = "Discover")]
#[delete("/<bot_id>/discover")]
pub async fn discover_remove_bot(
    db: &State<Database>,
    bot_id: Reference<'_>,
    user: User,
) -> Result<EmptyResponse> {
    let bot = bot_id.as_bot(db).await?;
    if (bot.owner != user.id && bot.id != user.id) && !user.privileged {
        return Err(create_error!(NotOwner));
    }

    listing::withdraw(db, DiscoverRequestType::Bot, &bot.id).await?;
    Ok(EmptyResponse)
}
