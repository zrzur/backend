use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod audit_log_query;
mod automod;
mod webhook_fetch_all;
mod ban_create;
mod ban_list;
mod ban_remove;
mod channel_create;
mod discover;
mod emoji_list;
mod invites_fetch;
mod join_requests;
mod lobby_answer;
mod vanity;
mod vanity_reserved;
mod member_edit;
mod member_experimental_query;
mod member_fetch;
mod member_fetch_all;
mod member_remove;
mod category_permissions;
mod permissions_set;
mod permissions_set_default;
mod roles_create;
mod roles_delete;
mod roles_edit;
mod roles_edit_positions;
mod roles_fetch;
mod rules_accept;
mod safety_pause;
mod server_ack;
mod server_create;
mod server_delete;
mod server_edit;
mod server_fetch;

#[cfg(test)]
mod member_timeout_tests;

#[cfg(test)]
mod locked_discover_tests;

#[cfg(test)]
mod member_page_tests;

#[cfg(test)]
mod verification_level_tests;

#[cfg(test)]
mod safety_pause_tests;

#[cfg(test)]
mod automod_tests;

#[cfg(test)]
mod rules_screening_tests;

#[cfg(test)]
mod kick_reason_tests;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        webhook_fetch_all::fetch_server_webhooks,
        server_create::create_server,
        super::applications::server_commands::fetch_server_commands,
        super::applications::install::list_server_apps,
        super::applications::install::uninstall_server_app,
        server_delete::delete,
        server_fetch::fetch,
        server_edit::edit,
        server_ack::ack,
        channel_create::create_server_channel,
        member_fetch_all::fetch_all,
        member_remove::kick,
        member_fetch::fetch,
        member_edit::edit,
        safety_pause::pause,
        rules_accept::accept,
        automod::fetch,
        automod::set,
        lobby_answer::answer,
        lobby_answer::remove,
        vanity::claim,
        vanity::remove,
        member_experimental_query::member_experimental_query,
        ban_create::ban,
        ban_remove::unban,
        ban_list::list,
        join_requests::list,
        join_requests::approve,
        join_requests::reject,
        join_requests::fetch_own,
        join_requests::cancel_own,
        invites_fetch::invites,
        roles_create::create,
        roles_edit::edit,
        roles_fetch::fetch,
        roles_delete::delete,
        permissions_set::set_role_permission,
        permissions_set_default::set_default_server_permissions,
        category_permissions::set_category_role_permission,
        category_permissions::set_category_default_permission,
        emoji_list::list_emoji,
        roles_edit_positions::edit_role_ranks,
        audit_log_query::query,
        discover::discover_add::discover_add,
        discover::discover_edit::discover_edit,
        discover::discover_get::discover_get,
        discover::discover_remove::discover_remove,
    ]
}
