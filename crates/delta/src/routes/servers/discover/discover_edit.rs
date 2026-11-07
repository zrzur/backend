use revolt_database::{util::reference::Reference, Database, DiscoverRequestType, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::routes::discover::listing;

/// # Edit Discover listing
///
/// Updates the tags, category or language of your server's Discover
/// listing or pending request. Approved listings stay approved.
#[openapi(tag = "Discover")]
#[patch("/<server>/discover", data = "<data>")]
pub async fn discover_edit(
    db: &State<Database>,
    server: Reference<'_>,
    user: User,
    data: Json<v0::DataDiscoverListing>,
) -> Result<Json<v0::DiscoverRequest>> {
    let server = server.as_server(db).await?;
    if server.owner != user.id && !user.privileged {
        return Err(create_error!(NotOwner));
    }

    listing::edit(db, DiscoverRequestType::Server, &server.id, data.into_inner())
        .await
        .map(Json)
}
