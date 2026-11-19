use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod add_friend;
mod block_user;
mod change_username;
pub(crate) mod connections;
pub(crate) mod connections_oauth;
#[cfg(test)]
mod connections_oauth_tests;
#[cfg(test)]
mod connections_tests;
pub(crate) mod change_requests;
mod edit_user;
mod fetch_dms;
mod fetch_mentions;
mod reminders;
#[cfg(test)]
mod saved_reminders_tests;
mod saved;
mod fetch_profile;
mod fetch_self;
mod fetch_user;
mod fetch_user_flags;
mod find_mutual;
mod get_default_avatar;
mod open_dm;
mod privacy;
mod profile_fields;
#[cfg(test)]
mod profile_tests;
#[cfg(test)]
mod status_mutual_tests;
mod remove_friend;
mod send_friend_request;
mod unblock_user;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        // User Information
        fetch_self::fetch,
        fetch_user::fetch,
        fetch_user_flags::fetch_user_flags,
        edit_user::edit,
        change_username::change_username,
        get_default_avatar::default_avatar,
        fetch_profile::profile,
        fetch_mentions::fetch_mentions,
        // Saved messages and reminders
        saved::list,
        saved::save,
        saved::unsave,
        reminders::list,
        reminders::create,
        reminders::edit,
        reminders::delete,
        // Your events across your servers
        crate::routes::servers::events::mine,
        privacy::fetch_privacy,
        privacy::edit_privacy,
        // Linked accounts
        connections::list,
        connections::steam_start,
        connections::steam_finish,
        connections::steam_edit,
        connections::steam_refresh,
        connections::steam_disconnect,
        connections_oauth::service_start,
        connections_oauth::service_finish,
        connections_oauth::service_edit,
        connections_oauth::service_refresh,
        connections_oauth::service_disconnect,
        // Direct Messaging
        fetch_dms::direct_messages,
        open_dm::open_dm,
        // Relationships
        find_mutual::mutual,
        add_friend::add,
        remove_friend::remove,
        block_user::block,
        unblock_user::unblock,
        send_friend_request::send_friend_request,
        // Changes Zrzur asks you to agree to
        change_requests::list,
        change_requests::answer,
    ]
}
