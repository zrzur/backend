use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod create;
mod delete;
mod discover;
mod edit;
mod fetch;
mod fetch_owned;
mod fetch_public;
mod interactions_endpoint;
mod invite;
mod token_reset;
pub(crate) mod verification;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        create::create_bot,
        invite::invite_bot,
        token_reset::reset_token,
        interactions_endpoint::new_secret,
        interactions_endpoint::set_endpoint,
        interactions_endpoint::remove_endpoint,
        fetch_public::fetch_public_bot,
        fetch::fetch_bot,
        fetch_owned::fetch_owned_bots,
        edit::edit_bot,
        delete::delete_bot,
        discover::discover_add_bot::discover_add_bot,
        discover::discover_edit_bot::discover_edit_bot,
        discover::discover_get_bot::discover_get_bot,
        discover::discover_remove_bot::discover_remove_bot,
        verification::verification_state,
        verification::apply_verification,
    ]
}

#[cfg(test)]
mod verification_tests;
#[cfg(test)]
mod group_tests;
