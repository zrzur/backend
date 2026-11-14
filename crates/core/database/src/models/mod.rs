mod account_invites;
mod application_commands;
mod accounts;
mod admin_migrations;
mod audit_logs;
mod badges;
mod bots;
mod channel_invites;
mod channel_unreads;
mod channel_webhooks;
mod billing;
mod channels;
mod community_items;
mod platform_words;
mod discover_requests;
mod emojis;
mod file_hashes;
mod files;
mod interactions;
mod linked_accounts;
mod messages;
mod oauth;
mod mfa_tickets;
mod platform_staff;
mod bot_verifications;
mod account_change_requests;
mod official_mark_requests;
mod platform_notices;
mod support_tickets;
mod plugins;
mod policy_changes;
mod ratelimit_events;
mod safety_reports;
mod safety_snapshots;
mod server_bans;
mod server_events;
mod server_insights;
mod server_join_requests;
mod server_members;
mod servers;
mod sessions;
mod soundboard;
mod trust_safety;
mod user_settings;
mod users;

pub use account_invites::*;
pub use application_commands::*;
pub use accounts::*;
pub use admin_migrations::*;
pub use audit_logs::*;
pub use badges::*;
pub use bots::*;
pub use channel_invites::*;
pub use channel_unreads::*;
pub use channel_webhooks::*;
pub use billing::*;
pub use channels::*;
pub use community_items::*;
pub use platform_words::*;
pub use discover_requests::*;
pub use emojis::*;
pub use file_hashes::*;
pub use files::*;
pub use interactions::*;
pub use linked_accounts::*;
pub use messages::*;
pub use oauth::*;
pub use mfa_tickets::*;
pub use platform_staff::*;
pub use bot_verifications::*;
pub use account_change_requests::*;
pub use official_mark_requests::*;
pub use platform_notices::*;
pub use support_tickets::*;
pub use plugins::*;
pub use policy_changes::*;
pub use ratelimit_events::*;
pub use safety_reports::*;
pub use safety_snapshots::*;
pub use server_bans::*;
pub use server_events::*;
pub use server_insights::*;
pub use server_join_requests::*;
pub use server_members::*;
pub use servers::*;
pub use sessions::*;
pub use soundboard::*;
pub use trust_safety::*;
pub use user_settings::*;
pub use users::*;

use crate::{Database, ReferenceDb};

#[cfg(feature = "mongodb")]
use crate::MongoDb;

pub trait AbstractDatabase:
    Sync
    + Send
    + admin_migrations::AbstractMigrations
    + application_commands::AbstractApplicationCommands
    + interactions::AbstractInteractions
    + oauth::AbstractOAuth
    + audit_logs::AbstractAuditLogs
    + badges::AbstractBadges
    + bots::AbstractBots
    + channels::AbstractChannels
    + channel_invites::AbstractChannelInvites
    + channel_unreads::AbstractChannelUnreads
    + channel_webhooks::AbstractWebhooks
    + emojis::AbstractEmojis
    + soundboard::AbstractSoundboard
    + file_hashes::AbstractAttachmentHashes
    + files::AbstractAttachments
    + messages::AbstractMessages
    + policy_changes::AbstractPolicyChange
    + ratelimit_events::AbstractRatelimitEvents
    + safety_reports::AbstractReport
    + safety_snapshots::AbstractSnapshot
    + server_bans::AbstractServerBans
    + server_events::AbstractServerEvents
    + server_insights::AbstractServerInsights
    + server_join_requests::AbstractServerJoinRequests
    + server_members::AbstractServerMembers
    + servers::AbstractServers
    + user_settings::AbstractUserSettings
    + users::AbstractUsers
    + accounts::AbstractAccounts
    + account_invites::AbstractAccountInvites
    + sessions::AbstractSessions
    + mfa_tickets::AbstractMFATickets
    + discover_requests::AbstractDiscoverRequest
    + community_items::AbstractCommunityItems
    + platform_words::AbstractPlatformWords
    + plugins::AbstractPlugins
    + billing::AbstractBilling
    + trust_safety::AbstractTrustSafety
    + platform_staff::AbstractPlatformStaff
    + bot_verifications::AbstractBotVerification
    + account_change_requests::AbstractAccountChangeRequest
    + official_mark_requests::AbstractOfficialMarkRequest
    + platform_notices::AbstractPlatformNotice
    + support_tickets::AbstractSupportTicket
    + linked_accounts::AbstractLinkedAccounts
{
}

impl AbstractDatabase for ReferenceDb {}

#[cfg(feature = "mongodb")]
impl AbstractDatabase for MongoDb {}

impl std::ops::Deref for Database {
    type Target = dyn AbstractDatabase;

    fn deref(&self) -> &Self::Target {
        match &self {
            Database::Reference(dummy) => dummy,
            #[cfg(feature = "mongodb")]
            Database::MongoDb(mongo) => mongo,
        }
    }
}
