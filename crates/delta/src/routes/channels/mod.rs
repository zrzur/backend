use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod channel_ack;
mod channel_delete;
mod channel_edit;
mod channel_fetch;
mod group_add_member;
mod group_create;
mod group_remove_member;
mod invite_create;
mod members_fetch;
mod message_bulk_delete;
mod message_clear_reactions;
mod message_delete;
mod message_edit;
mod message_fetch;
mod message_forward;
mod message_pin;
mod message_query;
mod message_react;
pub(crate) mod message_search;
mod message_send;
mod message_unpin;
mod message_unreact;
mod permissions_set;
mod permissions_set_member;
mod permissions_set_default;
mod permissions_sync;
mod threads;
mod typing;
mod voice_join;
mod voice_stop_ring;
pub(crate) mod webhook_create;
mod webhook_fetch_all;

#[cfg(test)]
mod bot_api_tests;

#[cfg(test)]
mod masquerade_tests;
#[cfg(test)]
mod official_tests;
#[cfg(test)]
mod forward_tests;

#[cfg(test)]
mod invite_defaults_tests;

#[cfg(test)]
mod masquerade_member_tests;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        channel_ack::ack,
        channel_fetch::fetch,
        members_fetch::fetch_members,
        channel_delete::delete,
        channel_edit::edit,
        invite_create::create_invite,
        message_send::message_send,
        message_forward::forward,
        message_query::query,
        message_search::search,
        typing::start_typing,
        message_pin::message_pin,
        message_fetch::fetch,
        message_edit::edit,
        message_bulk_delete::bulk_delete_messages,
        message_delete::delete,
        message_unpin::message_unpin,
        group_create::create_group,
        group_add_member::add_member,
        group_remove_member::remove_member,
        voice_join::call,
        voice_stop_ring::stop_ring,
        permissions_set::set_role_permissions,
        permissions_set_member::set_member_permissions,
        permissions_set_default::set_default_channel_permissions,
        permissions_sync::sync_permissions,
        threads::create_thread,
        threads::list_threads,
        threads::edit_thread,
        message_react::react_message,
        message_unreact::unreact_message,
        message_clear_reactions::clear_reactions,
        webhook_create::create_webhook,
        webhook_fetch_all::fetch_webhooks,
    ]
}

#[cfg(test)]
mod thread_tests;
#[cfg(test)]
mod thread_permission_tests;
