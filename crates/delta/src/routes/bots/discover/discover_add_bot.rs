use revolt_database::{util::reference::Reference, Database, DiscoverRequestType, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::routes::discover::listing;

/// # Add bot to Discover
///
/// This puts your bot into the Discover request queue, optionally with
/// listing details (tags, category, language).
#[openapi(tag = "Discover")]
#[put("/<bot_id>/discover", data = "<data>")]
pub async fn discover_add_bot(
    db: &State<Database>,
    bot_id: Reference<'_>,
    user: User,
    data: Option<Json<v0::DataDiscoverListing>>,
) -> Result<Json<v0::DiscoverRequest>> {
    let bot = bot_id.as_bot(db).await?;
    if (bot.owner != user.id && bot.id != user.id) && !user.privileged {
        return Err(create_error!(NotOwner));
    }

    // Discover lists bots others can invite, so the bot must be public
    if !bot.public {
        return Err(create_error!(InvalidOperation));
    }

    listing::submit(
        db,
        DiscoverRequestType::Bot,
        &bot.id,
        data.map(Json::into_inner),
    )
    .await
    .map(Json)
}
