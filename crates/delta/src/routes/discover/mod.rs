use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

pub(crate) mod decide;
pub mod listing;
mod list_bots;
pub(crate) mod list_servers;
pub(crate) mod queue;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        list_servers::list_servers,
        list_servers::fetch_server,
        list_servers::promoted,
        list_servers::server_event,
        list_bots::list_bots,
        list_bots::fetch_bot,
        list_bots::promoted_bots,
        list_bots::bot_event,
        queue::queue,
        decide::decide,
    ]
}
