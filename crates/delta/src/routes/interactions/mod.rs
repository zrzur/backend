use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod callback;
mod create;
mod followups;
mod options;
mod respond;
pub(crate) use callback::answer;
pub(crate) use respond::state_update;

#[cfg(test)]
mod component_tests;
#[cfg(test)]
mod endpoint_tests;
#[cfg(test)]
mod followup_tests;
#[cfg(test)]
mod tests;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        create::create_interaction,
        create::request_autocomplete,
        callback::interaction_callback,
        callback::edit_original_response,
        followups::send_followup,
        followups::edit_interaction_message,
        followups::delete_interaction_message,
        followups::delete_original_response,
    ]
}
