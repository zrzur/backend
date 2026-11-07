use revolt_database::{util::reference::Reference, Database, DiscoverRequestType, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::routes::discover::listing;

/// # Add server to Discover
///
/// This puts your server into the Discover request queue, optionally with
/// listing details (tags, category, language).
#[openapi(tag = "Discover")]
#[put("/<server>/discover", data = "<data>")]
pub async fn discover_add(
    db: &State<Database>,
    server: Reference<'_>,
    user: User,
    data: Option<Json<v0::DataDiscoverListing>>,
) -> Result<Json<v0::DiscoverRequest>> {
    let server = server.as_server(db).await?;
    if server.owner != user.id && !user.privileged {
        return Err(create_error!(NotOwner));
    }

    listing::submit(
        db,
        DiscoverRequestType::Server,
        &server.id,
        data.map(Json::into_inner),
    )
    .await
    .map(Json)
}
