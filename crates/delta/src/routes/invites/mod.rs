use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod invite_delete;
mod invite_fetch;
mod invite_join;

#[cfg(test)]
mod invite_use_tests;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        invite_fetch::fetch,
        invite_join::join,
        invite_delete::delete
    ]
}
