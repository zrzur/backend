use revolt_database::{util::reference::Reference, Database, DiscoverRequestType, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::routes::discover::listing;

/// # Edit Discover listing
///
/// Updates the tags, category or language of your bot's Discover
/// listing or pending request. Approved listings stay approved.
#[openapi(tag = "Discover")]
#[patch("/<bot_id>/discover", data = "<data>")]
pub async fn discover_edit_bot(
    db: &State<Database>,
    bot_id: Reference<'_>,
    user: User,
    data: Json<v0::DataDiscoverListing>,
) -> Result<Json<v0::DiscoverRequest>> {
    let bot = bot_id.as_bot(db).await?;
    if (bot.owner != user.id && bot.id != user.id) && !user.privileged {
        return Err(create_error!(NotOwner));
    }

    listing::edit(db, DiscoverRequestType::Bot, &bot.id, data.into_inner())
        .await
        .map(Json)
}
