use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod delivery;
#[cfg(test)]
mod embed_tests;
#[cfg(test)]
mod parity_tests;
#[cfg(test)]
mod replay_tests;
#[cfg(test)]
mod security_tests;
#[cfg(test)]
mod test_support;
mod webhook_delete;
mod webhook_delete_token;
mod webhook_edit;
mod webhook_edit_token;
mod webhook_execute;
mod webhook_execute_github;
mod webhook_execute_slack;
mod webhook_fetch;
mod webhook_fetch_token;
mod webhook_message;
mod webhook_regenerate_token;
mod webhook_send_test;

pub fn routes() -> (Vec<Route>, OpenApi) {
    let (mut routes, spec) = openapi_get_routes_spec![
        webhook_delete_token::webhook_delete_token,
        webhook_delete::webhook_delete,
        webhook_edit_token::webhook_edit_token,
        webhook_edit::webhook_edit,
        webhook_execute_github::webhook_execute_github,
        webhook_execute_slack::webhook_execute_slack,
        webhook_execute::webhook_execute,
        webhook_fetch_token::webhook_fetch_token,
        webhook_fetch::webhook_fetch,
        webhook_message::webhook_delete_message_short,
        webhook_message::webhook_delete_message,
        webhook_message::webhook_edit_message_short,
        webhook_message::webhook_edit_message,
        webhook_message::webhook_fetch_message,
        webhook_regenerate_token::webhook_regenerate_token,
        webhook_send_test::webhook_send_test,
    ];
    // the multipart form of execute: same path, not a route of its own in the docs
    routes.extend(rocket::routes![webhook_execute::webhook_execute_files]);
    (routes, spec)
}
