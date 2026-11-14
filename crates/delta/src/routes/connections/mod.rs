//! Messages services send to Zrzur by themselves. Today: Twitch's live and
//! offline messages for linked channels (EventSub by webhook).

use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

pub mod twitch_events;
#[cfg(test)]
mod twitch_events_tests;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![twitch_events::twitch_events]
}
