use revolt_database::{util::reference::Reference, Database, DiscoverRequestType, User};
use revolt_result::{create_error, Result};
use rocket::State;
use rocket_empty::EmptyResponse;

use crate::routes::discover::listing;

/// # Remove from Discover
///
/// Cancels your request, or takes your server off Discover if it is
/// listed. Owners can do this at any time; listing again needs a new
/// review. Servers removed by moderators need support.
#[openapi(tag = "Discover")]
#[delete("/<server>/discover")]
pub async fn discover_remove(
    db: &State<Database>,
    server: Reference<'_>,
    user: User,
) -> Result<EmptyResponse> {
    let server = server.as_server(db).await?;
    if server.owner != user.id && !user.privileged {
        return Err(create_error!(NotOwner));
    }

    listing::withdraw(db, DiscoverRequestType::Server, &server.id).await?;
    Ok(EmptyResponse)
}
