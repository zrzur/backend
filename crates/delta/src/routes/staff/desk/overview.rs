//! Desk overview: the console's platform statistics (`stats_core`), for a
//! person pressing `/desk stats`. The permission check is the console's own.

use revolt_database::{Database, User};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::super::platform::{stats_core, PlatformStats};
use super::actor::{desk_actor, DataDeskInteraction, Expect};

/// The desk area this route belongs to
const AREA: &str = "overview";
const PATH_OVERVIEW: &str = "/staff/desk/overview";

/// # Desk: overview
///
/// From `/desk stats`, when the `overview` area is enabled. The same
/// numbers as the console's `GET /staff/stats`; needs the same permission.
#[post("/desk/overview", data = "<data>")]
pub async fn overview(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskInteraction>,
) -> Result<Json<PlatformStats>> {
    // an area that is off does not exist: 404 before the press is used up
    if !super::settings(&user).await?.areas.iter().any(|a| a == AREA) {
        return Err(create_error!(NotFound));
    }
    let (staff, _) = desk_actor(db, &user, &data, Expect::Command("stats"), PATH_OVERVIEW).await?;
    stats_core(db, &staff).await.map(Json)
}
