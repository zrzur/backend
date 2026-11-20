//! "Join the Zrzur team": people apply for a team role through one bot in
//! one server, and holders of one role there review the applications
//! (`[features.team_applications]`).
//!
//! The bot has no power of its own. It can read the open applications (to
//! show them in the staff channel), and it can hand back an interaction: the
//! record this server made when a person pressed a button or sent a form,
//! with the one-time token only the bot received. Everything else is decided
//! here:
//!
//! - who applied, withdrew or decided: the person stored on the interaction,
//!   read again from the database;
//! - what they pressed: the `custom_id` stored on the interaction
//!   (`ta:<action>:<id>`), never a value the bot sends;
//! - the answers and the reason of a rejection: the form fields stored on
//!   the interaction;
//! - whether a reviewer may decide: member of the server and holder of the
//!   reviewer role right now.
//!
//! What the bot can still do with its token is draw: it writes its own
//! cards, buttons and forms. So an acceptance, the one act that hands out
//! a role, counts only from a private confirmation whose words this server
//! wrote and reads back: it names the applicant and the role, carries one
//! button, and was never changed since it was shown. A card whose button
//! says one thing and carries another therefore accepts nobody. The
//! reviewer is also told by the official account, outside the bot, whom
//! they accepted. Two lesser risks stay, as in the Discover desk: a
//! mislabelled Interview or Reject button on a card, and a form shown to a
//! person under another title (an application or a withdrawal in their own
//! name). Neither hands out a role, and the audit record shows what was
//! really done.
//!
//! A decision is written to the platform audit log first, under the
//! reviewer's name. The reason of a rejection is not copied there: it is
//! deleted with the application. Accepting gives the server role named in the settings,
//! and it is this server that gives it, not the bot. Three things are never
//! given by any path here: a role marked `manual`, the role with the key
//! `administrator`, and any server role that can manage the server, its
//! roles or its permissions. Accepting such a role only records it and
//! tells the owner of the server. Platform staff powers are not touched at
//! all.
//!
//! Every team role is a volunteer role in a volunteer project, and every
//! text says so. Anyone with an account may apply, unless a role is marked
//! `adult` in the settings (none is by default); then the age group the
//! account already states is read. Only while the volunteer care is on
//! (`[features.team_applications.volunteers]`) a first step, "About you",
//! asks name, age (16 or over) and country and the tick for the volunteer
//! rules; they are kept as the person's identity until the application ends.
//!
//! What a person reads (the roles, the questions, the messages) exists in
//! the app's seven languages (`util::team_texts`); what the reviewers read
//! is English.

use revolt_config::{config, FeaturesTeamApplications, TeamRole};
use revolt_database::{
    record_platform_audit, staff_now, util::age::owner_band, AuditOutcome, AuditRecord, Channel,
    Database, Interaction, InteractionSource, PartialChannel, PartialMember, Server, TeamAnswer,
    TeamApplication, TeamApplicationStatus, TeamGrant, TeamIdentity, User,
};
use revolt_models::v0::{self, InteractionType};
use revolt_permissions::ChannelPermission;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, Route, State};
use serde::{Deserialize, Serialize};

use crate::util::team_texts::{self, RoleField};

#[cfg(test)]
mod tests;
pub(crate) mod volunteers;

pub fn routes() -> Vec<Route> {
    routes![
        desk_config,
        queue,
        application_state,
        mine,
        about,
        apply,
        withdraw,
        act,
        identity,
        settle,
        status,
        away,
        leave,
        report
    ]
}

const MIN_REASON: usize = 3;
const MAX_REASON: usize = 512;
const MIN_ANSWER: usize = 10;
const MAX_ANSWER: usize = 1000;
const MIN_LANGUAGES: usize = 2;
const MAX_LANGUAGES: usize = 200;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Milliseconds of a number of days from the settings, kept within 0..=3650
fn days_ms(days: i64) -> i64 {
    days.clamp(0, 3650) * DAY_MS
}
const MIN_AGE: u8 = 16;
const MAX_AGE: u8 = 99;
const MIN_NAME: usize = 2;
const MAX_NAME: usize = 80;
const MIN_COUNTRY: usize = 2;
const MAX_COUNTRY: usize = 56;
/// After withdrawing, a person waits `withdraw_wait_minutes` before applying
/// again; the record without answers lives exactly as long
fn withdraw_wait_ms(settings: &FeaturesTeamApplications) -> i64 {
    settings.withdraw_wait_minutes.max(0) * 60 * 1000
}

/// The time a person could give, as offered in the form (value, text)
pub const HOURS: &[(&str, &str)] = &[
    ("under-3", "Up to 3 hours a week"),
    ("3-7", "3 to 7 hours a week"),
    ("7-15", "7 to 15 hours a week"),
    ("over-15", "More than 15 hours a week"),
];

/// Permissions no role handed out here may carry, on the role itself or in
/// any channel's or category's own rules for that role
const POWERFUL: &[ChannelPermission] = &[
    ChannelPermission::Administrator,
    ChannelPermission::ManageServer,
    ChannelPermission::ManageRole,
    ChannelPermission::ManagePermissions,
    ChannelPermission::AssignRoles,
    ChannelPermission::ManageChannel,
    ChannelPermission::ManageWebhooks,
];

/// The one label an acceptance may be pressed on
pub const ACCEPT_LABEL: &str = "Yes, accept";

/// A name inside the confirmation: letters, digits and a few signs only, so
/// the sentence reads the same wherever it is shown
fn confirm_word(text: &str) -> String {
    let kept: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '#' | ':'))
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(64).collect()
}

/// The words a reviewer confirms before an acceptance. The server writes
/// them, the bot shows them unchanged, and `act` reads them back from the
/// private message the reviewer pressed on.
fn confirm_text(applicant: &str, role: &str, server: &str, manual: bool) -> String {
    let place = confirm_word(server);
    let place = if place.is_empty() { String::new() } else { format!(" in {place}") };
    let effect = if manual {
        "This role is never given automatically: the decision is recorded and the owner is told."
    } else {
        "They become a member of the Zrzur Team, get the role in its server at once (or when they join it) and a welcome message with the rules."
    };
    format!(
        "Accept {} for {}{place}? {effect}",
        confirm_word(applicant),
        confirm_word(role)
    )
}

/// Whether this press is the one button of an unchanged private
/// confirmation of this bot, shown to this person, carrying `expected`
async fn confirmed(
    db: &Database,
    settings: &FeaturesTeamApplications,
    interaction: &Interaction,
    id: &str,
    expected: &str,
) -> bool {
    confirmed_button(
        db,
        settings,
        interaction,
        expected,
        ACCEPT_LABEL,
        &format!("ta:accept:{id}"),
    )
    .await
}

/// The same check for any private confirmation: one row, one button with
/// this `label` and `custom_id`, the server's words `expected` unchanged
async fn confirmed_button(
    db: &Database,
    settings: &FeaturesTeamApplications,
    interaction: &Interaction,
    expected: &str,
    want_label: &str,
    want_custom_id: &str,
) -> bool {
    let Some(InteractionSource::Ephemeral {
        interaction_id,
        message_id,
    }) = &interaction.source
    else {
        return false;
    };
    let Ok(parent) = db.fetch_interaction(interaction_id).await else {
        return false;
    };
    if parent.application_id != settings.bot || parent.user_id != interaction.user_id {
        return false;
    }
    let Some(message) = parent.find_ephemeral(message_id) else {
        return false;
    };
    if message.edited.is_some()
        || !message.embeds.is_empty()
        || message.content.as_deref().map(str::trim) != Some(expected)
    {
        return false;
    }
    match message.components.as_slice() {
        [v0::ComponentBlock::ActionRow { components }] => match components.as_slice() {
            [v0::Component::Button {
                label: Some(label),
                custom_id: Some(custom_id),
                ..
            }] => label == want_label && custom_id == want_custom_id,
            _ => false,
        },
        _ => false,
    }
}

/// Whether a server role is one the system must never hand out: a managed
/// one, or one that can manage the server through its own permissions or
/// through any channel's or category's rules for it
async fn powerful(db: &Database, server: &Server, role_id: &str) -> bool {
    let Some(role) = server.roles.get(role_id) else {
        return true;
    };
    let bits = POWERFUL
        .iter()
        .fold(0_u64, |bits, permission| bits | *permission as u64);
    if role.is_managed() || (role.permissions.a as u64) & bits != 0 {
        return true;
    }
    for category in server.categories.iter().flatten() {
        if category
            .role_permissions
            .get(role_id)
            .is_some_and(|rule| (rule.a as u64) & bits != 0)
        {
            return true;
        }
    }
    for id in &server.channels {
        // a channel that cannot be read is not waved through
        let Ok(channel) = db.fetch_channel(id).await else {
            return true;
        };
        if let Some((_, rules)) = channel.effective_overrides(None) {
            if rules.get(role_id).is_some_and(|rule| (rule.a as u64) & bits != 0) {
                return true;
            }
        }
    }
    false
}

// ---- who may call ----

#[cfg(test)]
pub(crate) mod test_settings {
    //! Tests run side by side, each with its own bot: the settings of a
    //! test are looked up by the calling bot instead of the one config.
    use revolt_config::FeaturesTeamApplications;
    use std::{collections::HashMap, sync::Mutex};

    static DESKS: Mutex<Option<HashMap<String, FeaturesTeamApplications>>> = Mutex::new(None);

    pub fn register(settings: FeaturesTeamApplications) {
        DESKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(settings.bot.clone(), settings);
    }

    pub fn get(bot: &str) -> Option<FeaturesTeamApplications> {
        DESKS.lock().unwrap().as_ref().and_then(|desks| desks.get(bot).cloned())
    }
}

/// The settings for the admin console's Team page: the feature and the
/// volunteer care are on. Tests register them under the staff member's id.
pub(crate) async fn staff_desk(staff_id: &str) -> Result<FeaturesTeamApplications> {
    #[cfg(test)]
    let settings = match test_settings::get(staff_id) {
        Some(settings) => settings,
        None => (*config().await.features.team_applications).clone(),
    };
    #[cfg(not(test))]
    let settings = {
        let _ = staff_id;
        (*config().await.features.team_applications).clone()
    };
    if settings.enabled() && settings.volunteers.enabled {
        Ok(settings)
    } else {
        Err(create_error!(NotFound))
    }
}

/// The settings, if the feature is switched on and `caller` is its bot.
/// Anyone else gets "not found": the routes do not exist for them.
async fn desk(caller: &User) -> Result<FeaturesTeamApplications> {
    if caller.bot.is_none() {
        return Err(create_error!(NotFound));
    }
    #[cfg(test)]
    if let Some(settings) = test_settings::get(&caller.id) {
        return Ok(settings);
    }
    let settings = config().await.features.team_applications;
    if settings.enabled() && settings.bot == caller.id {
        Ok(*settings)
    } else {
        Err(create_error!(NotFound))
    }
}

// ---- small helpers ----

/// Everything whose time is up goes: applications, and the identities of
/// people who are not volunteers any more
async fn purge_due(db: &Database, now: i64) -> Result<()> {
    db.delete_team_applications_due(now).await?;
    db.delete_team_identities_due(now).await?;
    Ok(())
}

/// The person has no other open application and no current membership
async fn out_of_reach(db: &Database, person: &str, except: &str) -> bool {
    let busy = db
        .fetch_team_applications_of(person)
        .await
        .map(|all| all.iter().any(|a| a.is_open() && a.id != except))
        .unwrap_or(true);
    let member = db
        .fetch_team_members_of(person)
        .await
        .map(|all| all.iter().any(|m| m.is_current()))
        .unwrap_or(true);
    !busy && !member
}

/// The identity of someone whose application ended, when nothing else
/// keeps them in the team's reach: erased at once (`erase`) or set to go when
/// the application's own time is up (or after `identity_keep_days` when the
/// application has none)
async fn release_identity(
    db: &Database,
    settings: &FeaturesTeamApplications,
    application: &TeamApplication,
    erase: bool,
) {
    if !out_of_reach(db, &application.user_id, &application.id).await {
        return;
    }
    let Ok(Some(mut identity)) = db.fetch_team_identity(&application.user_id).await else {
        return;
    };
    match erase {
        true => {
            let _ = db.delete_team_identity(&application.user_id).await;
        }
        false => {
            identity.purge_at = Some(application.purge_at.unwrap_or_else(|| {
                staff_now() + days_ms(settings.volunteers.identity_keep_days)
            }));
            let _ = db.upsert_team_identity(&identity).await;
        }
    }
}

/// The identity of a person who is in the team's reach again keeps no
/// deletion time
async fn keep_identity(db: &Database, person: &str) {
    if let Ok(Some(mut identity)) = db.fetch_team_identity(person).await {
        if identity.purge_at.is_some() {
            identity.purge_at = None;
            let _ = db.upsert_team_identity(&identity).await;
        }
    }
}

fn status_word(status: TeamApplicationStatus) -> &'static str {
    match status {
        TeamApplicationStatus::Open => "Open",
        TeamApplicationStatus::Interview => "Interview",
        TeamApplicationStatus::Accepted => "Accepted",
        TeamApplicationStatus::Rejected => "Rejected",
        TeamApplicationStatus::Withdrawn => "Withdrawn",
    }
}

fn grant_word(grant: TeamGrant) -> &'static str {
    match grant {
        TeamGrant::Granted => "granted",
        TeamGrant::PendingJoin => "pending_join",
        TeamGrant::Manual => "manual",
    }
}

fn created_at(id: &str) -> i64 {
    ulid::Ulid::from_string(id)
        .map(|ulid| ulid.timestamp_ms() as i64)
        .unwrap_or_default()
}

fn full_name(user: &User) -> String {
    format!("{}#{}", user.username, user.discriminator)
}

async fn name_of(db: &Database, user_id: &str) -> String {
    match db.fetch_user(user_id).await {
        Ok(user) => full_name(&user),
        Err(_) => user_id.to_string(),
    }
}

/// The English name of a role, as the reviewers read it
pub(crate) fn role_name(settings: &FeaturesTeamApplications, key: &str) -> String {
    settings
        .role(key)
        .map(|role| team_texts::role_text(role, "en", RoleField::Name))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| key.to_string())
}

/// The official servers of the settings (where people apply, and every
/// server a team role belongs to), without repeats
fn official_servers(settings: &FeaturesTeamApplications) -> Vec<String> {
    let mut servers = vec![settings.server.clone()];
    for role in &settings.roles {
        if !role.server.trim().is_empty() && !servers.contains(&role.server) {
            servers.push(role.server.clone());
        }
    }
    servers
}

fn barred(user: &User) -> bool {
    let flags = v0::UserFlags::SuspendedUntil as i32
        | v0::UserFlags::Deleted as i32
        | v0::UserFlags::Banned as i32
        | v0::UserFlags::Spam as i32;
    user.bot.is_some() || user.flags.unwrap_or_default() & flags != 0
}

fn no_permission() -> revolt_result::Error {
    create_error!(MissingPermission {
        permission: "TeamReview".to_string()
    })
}

// ---- what the bot sees ----

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeskQuestion {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub hint: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeskOption {
    pub value: String,
    pub label: String,
}

/// What a person reads about a role, in one language
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeskRoleText {
    pub name: String,
    pub description: String,
    pub questions: Vec<DeskQuestion>,
}

/// A team role as the bot shows it
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeskRole {
    pub key: String,
    /// English, as the reviewers read it
    pub name: String,
    pub description: String,
    /// The same in every language of `languages`, for the people who apply
    pub texts: std::collections::HashMap<String, DeskRoleText>,
    /// Name of the server the role belongs to (empty when it is gone)
    pub server_name: String,
    /// Only accounts whose stated age is 18 or over may apply
    pub adult: bool,
    /// Given by the owner by hand, never by the system
    pub manual: bool,
    pub questions: Vec<DeskQuestion>,
}

/// What the bot needs to draw the Apply message and the form
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeskConfig {
    pub server: String,
    pub channel_apply: String,
    pub channel_review: String,
    /// Every staff channel cards are shown in (the common one first)
    pub review_channels: Vec<String>,
    /// The languages people can read everything in, English first
    pub languages: Vec<String>,
    pub reviewer_role: String,
    pub cooldown_days: i64,
    /// Days a rejected application is kept
    pub retention_days: i64,
    pub accepted_retention_days: i64,
    pub privacy_url: String,
    /// The choices of the "time" field
    pub hours: Vec<DeskOption>,
    pub limits: DeskLimits,
    /// Roles open for applications, in the order shown
    pub roles: Vec<DeskRole>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeskLimits {
    pub min_answer: usize,
    pub max_answer: usize,
    pub min_languages: usize,
    pub max_languages: usize,
    pub min_reason: usize,
    pub max_reason: usize,
}

fn offered(role: &TeamRole) -> bool {
    role.open && role.ready()
}

/// # Team desk: settings for the bot
///
/// The roles open for applications and the numbers the texts name. Only the
/// desk's bot.
#[get("/config")]
async fn desk_config(db: &State<Database>, user: User) -> Result<Json<DeskConfig>> {
    let settings = desk(&user).await?;
    let mut roles = Vec::new();
    for role in settings.roles.iter().filter(|role| offered(role)) {
        let in_language = |language: &str| DeskRoleText {
            name: team_texts::role_text(role, language, RoleField::Name),
            description: team_texts::role_text(role, language, RoleField::Description),
            questions: team_texts::questions(role, language)
                .into_iter()
                .map(|q| DeskQuestion {
                    id: q.id,
                    label: q.label,
                    hint: q.hint,
                })
                .collect(),
        };
        let english = in_language("en");
        roles.push(DeskRole {
            key: role.key.clone(),
            name: english.name.clone(),
            description: english.description.clone(),
            texts: team_texts::LANGUAGES
                .iter()
                .map(|(code, _)| (code.to_string(), in_language(code)))
                .collect(),
            server_name: db
                .fetch_server(&role.server)
                .await
                .map(|server| server.name)
                .unwrap_or_default(),
            adult: role.adult,
            manual: role.is_manual(),
            questions: english.questions,
        });
    }
    Ok(Json(DeskConfig {
        server: settings.server.clone(),
        channel_apply: settings.channel_apply.clone(),
        channel_review: settings.channel_review.clone(),
        review_channels: settings.review_channels(),
        languages: team_texts::LANGUAGES
            .iter()
            .map(|(code, _)| code.to_string())
            .collect(),
        reviewer_role: settings.reviewer_role.clone(),
        cooldown_days: settings.cooldown_days.max(0),
        retention_days: settings.rejected_days(),
        accepted_retention_days: settings.accepted_retention_days.max(0),
        privacy_url: settings.privacy_url.clone(),
        hours: HOURS
            .iter()
            .map(|(value, label)| DeskOption {
                value: value.to_string(),
                label: label.to_string(),
            })
            .collect(),
        limits: DeskLimits {
            min_answer: MIN_ANSWER,
            max_answer: MAX_ANSWER,
            min_languages: MIN_LANGUAGES,
            max_languages: MAX_LANGUAGES,
            min_reason: MIN_REASON,
            max_reason: MAX_REASON,
        },
        roles,
    }))
}

/// An open application with what its card shows
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeskEntry {
    pub id: String,
    /// `Open` or `Interview`
    pub status: String,
    pub role: String,
    pub role_name: String,
    /// Given by the owner by hand, never by the system
    pub manual: bool,
    pub user_id: String,
    /// `name#1234`
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub display_name: Option<String>,
    /// The person's picture on the file server, as `<bucket>/<file id>`
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub avatar: Option<String>,
    /// Name of the server the role belongs to
    pub server_name: String,
    /// When the account was created, milliseconds since the epoch
    pub account_created_at: i64,
    /// The role is open to accounts of 18 or over only, and this account
    /// stated that age when applying. False for every other role: no age
    /// is read or kept for them.
    pub adult: bool,
    /// Names of the official servers the person is a member of
    pub servers: Vec<String>,
    /// The person gave name, age and country in "About you" (the values
    /// themselves are never part of a card)
    pub has_identity: bool,
    /// The age stated in "About you" and the account's own age group
    /// disagree about being 18 or over; a reviewer looks twice
    pub age_note: bool,
    pub answers: Vec<TeamAnswer>,
    pub submitted_at: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub interviewer_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_at: Option<i64>,
    /// The exact words of the private question before an acceptance, with
    /// one button labelled "Yes, accept": anything else accepts nobody
    pub confirm: String,
    /// The staff channel this card belongs in
    pub channel_review: String,
    /// Roles the card's buttons are limited to. Empty when the channel is
    /// in another server than the desk's: the common reviewers hold no
    /// role there, so the buttons stay open and the server alone decides.
    pub restrict_roles: Vec<String>,
}

/// The roles a card's buttons can be limited to without locking out a
/// reviewer who may decide
async fn restrict_roles(
    db: &Database,
    settings: &FeaturesTeamApplications,
    key: &str,
    channel: &str,
) -> Vec<String> {
    let in_desk_server = matches!(
        db.fetch_channel(channel).await,
        Ok(Channel::TextChannel { server, .. }) if server == settings.server
    );
    if !in_desk_server {
        return Vec::new();
    }
    let mut roles = vec![settings.reviewer_role.clone()];
    if let Some(role) = settings.role(key) {
        if role.server == settings.server && !role.reviewer_role.trim().is_empty() {
            roles.push(role.reviewer_role.clone());
        }
    }
    roles
}

async fn entry(
    db: &Database,
    settings: &FeaturesTeamApplications,
    application: &TeamApplication,
) -> Option<DeskEntry> {
    let person = db.fetch_user(&application.user_id).await.ok()?;
    let mut servers = Vec::new();
    for id in official_servers(settings) {
        if db.fetch_member(&id, &person.id).await.is_ok() {
            if let Ok(server) = db.fetch_server(&id).await {
                servers.push(server.name);
            }
        }
    }
    let interviewer_name = match &application.interviewer {
        Some(id) => Some(name_of(db, id).await),
        None => None,
    };
    let channel = settings.review_channel(&application.role).to_string();
    let identity = db.fetch_team_identity(&person.id).await.ok().flatten();
    let age_note = match &identity {
        Some(identity) => age_disagrees(identity.age, owner_band(db, &person).await),
        None => false,
    };
    Some(DeskEntry {
        id: application.id.clone(),
        status: status_word(application.status).to_string(),
        role: application.role.clone(),
        role_name: role_name(settings, &application.role),
        manual: settings
            .role(&application.role)
            .is_none_or(|role| role.is_manual()),
        user_id: person.id.clone(),
        name: full_name(&person),
        display_name: person.display_name.clone(),
        avatar: person
            .avatar
            .as_ref()
            .map(|file| format!("{}/{}", file.tag, file.id)),
        server_name: server_name(db, settings.role(&application.role)).await,
        account_created_at: created_at(&person.id),
        adult: application.adult,
        servers,
        has_identity: identity.is_some(),
        age_note,
        answers: application.answers.clone(),
        submitted_at: application.submitted_at,
        interviewer_name,
        acted_at: application.acted_at,
        confirm: confirm_text(
            &full_name(&person),
            &role_name(settings, &application.role),
            &server_name(db, settings.role(&application.role)).await,
            settings
                .role(&application.role)
                .is_none_or(|role| role.is_manual()),
        ),
        channel_review: channel.clone(),
        restrict_roles: restrict_roles(db, settings, &application.role, &channel).await,
    })
}

/// # Team desk: open applications
///
/// Applications waiting for a decision, oldest first. Only the desk's bot.
#[get("/queue")]
async fn queue(db: &State<Database>, user: User) -> Result<Json<Vec<DeskEntry>>> {
    let settings = desk(&user).await?;
    // nothing outlives its time, whether or not the clean-up task ran
    purge_due(db, staff_now()).await?;
    let mut entries = Vec::new();
    for application in db.fetch_open_team_applications().await? {
        if let Some(entry) = entry(db, &settings, &application).await {
            entries.push(entry);
        }
    }
    Ok(Json(entries))
}

/// Where an application stands, for a card whose application left the queue
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeskState {
    /// `Open`, `Interview`, `Accepted`, `Rejected`, `Withdrawn` or `Gone`
    /// (deleted: its time was up, or the account is gone)
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_at: Option<i64>,
    /// `granted`, `pending_join` or `manual`
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub grant: Option<String>,
}

/// # Team desk: where one application stands
///
/// Gives the status word and who acted, never the answers or the reason.
#[get("/applications/<id>")]
async fn application_state(db: &State<Database>, user: User, id: &str) -> Result<Json<DeskState>> {
    desk(&user).await?;
    purge_due(db, staff_now()).await?;
    let Ok(application) = db.fetch_team_application(id).await else {
        return Ok(Json(DeskState {
            status: "Gone".to_string(),
            acted_by_name: None,
            acted_at: None,
            grant: None,
        }));
    };
    let acted_by_name = match &application.acted_by {
        Some(id) => Some(name_of(db, id).await),
        None => None,
    };
    Ok(Json(DeskState {
        status: status_word(application.status).to_string(),
        acted_by_name,
        acted_at: application.acted_at,
        grant: application.grant.map(|grant| grant_word(grant).to_string()),
    }))
}

// ---- interactions handed back ----

/// The interaction the bot hands back: nothing else is taken from the bot
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DataDeskAct {
    pub interaction_id: String,
    pub token: String,
}

/// The stored interaction behind `data`, if it is genuine and was received
/// by this bot. `channel`: it must have happened there, in the desk's
/// server; `None` leaves the place to the caller (a staff channel may be in
/// another server).
async fn genuine(
    db: &Database,
    settings: &FeaturesTeamApplications,
    data: &DataDeskAct,
    channel: Option<&str>,
) -> Result<Interaction> {
    let interaction = db
        .fetch_interaction(&data.interaction_id)
        .await
        .map_err(|_| create_error!(UnknownInteraction))?;
    if !interaction.token_matches(&data.token) || interaction.application_id != settings.bot {
        return Err(create_error!(UnknownInteraction));
    }
    if interaction.is_expired() {
        return Err(create_error!(InteractionExpired));
    }
    if let Some(channel) = channel {
        if interaction.server_id.as_deref() != Some(settings.server.as_str())
            || interaction.channel_id != channel
        {
            return Err(no_permission());
        }
    }
    Ok(interaction)
}

/// The parts of a stored `custom_id`: `ta:<action>[:<argument>]`
fn pressed(interaction: &Interaction) -> Option<(&str, &str)> {
    let custom_id = interaction.custom_id.as_deref()?;
    let rest = custom_id.strip_prefix("ta:")?;
    let (action, argument) = rest.split_once(':').unwrap_or((rest, ""));
    if action.is_empty() || argument.len() > 64 || argument.contains(':') {
        return None;
    }
    Some((action, argument))
}

/// The person behind an interaction in the apply channel: a member of the
/// server, not a bot, not suspended or banned
async fn applicant(
    db: &Database,
    settings: &FeaturesTeamApplications,
    interaction: &Interaction,
) -> Result<User> {
    let person = db
        .fetch_user(&interaction.user_id)
        .await
        .map_err(|_| no_permission())?;
    if barred(&person) || db.fetch_member(&settings.server, &person.id).await.is_err() {
        return Err(no_permission());
    }
    Ok(person)
}

/// Whether `user_id` holds `role` in `server` right now (read from the
/// database, not taken from the click: the role may be gone since)
async fn holds(db: &Database, server: &str, user_id: &str, role: &str) -> bool {
    if server.trim().is_empty() || role.trim().is_empty() {
        return false;
    }
    match db.fetch_member(server, user_id).await {
        Ok(member) => member.roles.iter().any(|held| held == role),
        Err(_) => false,
    }
}

/// The person who pressed, if they may review applications for `role`
/// right now: a holder of the common reviewer role in the desk's server
/// (they review everything), or of the team role's own reviewer role in
/// that role's server
async fn reviewer(
    db: &Database,
    settings: &FeaturesTeamApplications,
    interaction: &Interaction,
    role: Option<&TeamRole>,
) -> Result<User> {
    let person = db
        .fetch_user(&interaction.user_id)
        .await
        .map_err(|_| no_permission())?;
    if barred(&person) {
        return Err(no_permission());
    }
    let common = holds(db, &settings.server, &person.id, &settings.reviewer_role).await;
    let own = match role {
        Some(role) => holds(db, &role.server, &person.id, &role.reviewer_role).await,
        None => false,
    };
    if !common && !own {
        return Err(no_permission());
    }
    Ok(person)
}

/// Stated 18 or over while the account says otherwise, or the other way
/// round. An account with no age on record disagrees with nothing.
fn age_disagrees(stated: u8, band: v0::AgeBand) -> bool {
    match band {
        v0::AgeBand::Unknown => false,
        v0::AgeBand::AdultDeclared => stated < 18,
        _ => stated >= 18,
    }
}

async fn is_adult(db: &Database, person: &User) -> bool {
    owner_band(db, person).await == v0::AgeBand::AdultDeclared
}

// ---- the applicant's side ----

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MineApplication {
    pub id: String,
    pub role: String,
    pub role_name: String,
    pub status: String,
    pub submitted_at: i64,
}

/// Where the person who pressed stands
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Mine {
    /// `none` (may apply), `open` (has an application waiting) or `wait`
    /// (decided or withdrawn recently)
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub application: Option<MineApplication>,
    /// `wait`: when a new application is possible, milliseconds since the epoch
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub wait_until: Option<i64>,
    /// Keys of the roles this person may apply for
    pub roles: Vec<String>,
    /// Keys of the open roles closed to them because the age stated on the
    /// account is under 18
    pub adult_only: Vec<String>,
}

/// The newest application that still matters, and until when it blocks a
/// new one (`None`: it is open, or it blocks nothing any more)
fn standing(
    settings: &FeaturesTeamApplications,
    applications: &[TeamApplication],
    now: i64,
) -> (Option<TeamApplication>, Option<i64>) {
    if let Some(open) = applications.iter().find(|a| a.is_open()) {
        return (Some(open.clone()), None);
    }
    let mut latest: Option<(TeamApplication, i64)> = None;
    for application in applications {
        let since = application.acted_at.unwrap_or(application.submitted_at);
        let until = match application.status {
            TeamApplicationStatus::Withdrawn => since + withdraw_wait_ms(settings),
            _ => since + days_ms(settings.cooldown_days),
        };
        if until > now && latest.as_ref().is_none_or(|(_, other)| until > *other) {
            latest = Some((application.clone(), until));
        }
    }
    match latest {
        Some((application, until)) => (Some(application), Some(until)),
        None => (None, None),
    }
}

fn mine_application(settings: &FeaturesTeamApplications, a: &TeamApplication) -> MineApplication {
    MineApplication {
        id: a.id.clone(),
        role: a.role.clone(),
        role_name: role_name(settings, &a.role),
        status: status_word(a.status).to_string(),
        submitted_at: a.submitted_at,
    }
}

async fn mine_of(db: &Database, settings: &FeaturesTeamApplications, person: &User) -> Result<Mine> {
    let now = staff_now();
    purge_due(db, now).await?;
    let applications = db.fetch_team_applications_of(&person.id).await?;
    let (application, wait_until) = standing(settings, &applications, now);
    let offered_roles: Vec<_> = settings.roles.iter().filter(|role| offered(role)).collect();
    // the stated age is read only when a role asks for it
    let adult = !offered_roles.iter().any(|role| role.adult) || is_adult(db, person).await;
    let mut roles = Vec::new();
    let mut adult_only = Vec::new();
    for role in offered_roles {
        if role.adult && !adult {
            adult_only.push(role.key.clone());
        } else {
            roles.push(role.key.clone());
        }
    }
    let state = match (&application, wait_until) {
        (Some(_), None) => "open",
        (Some(_), Some(_)) => "wait",
        _ => "none",
    };
    Ok(Mine {
        state: state.to_string(),
        application: application.map(|a| mine_application(settings, &a)),
        wait_until,
        roles,
        adult_only,
    })
}

/// # Team desk: where the person who pressed stands
///
/// The bot hands back a press of "Apply" or "My application" in the apply
/// channel and learns whether that person has an application, must wait, or
/// may apply and for which roles.
#[post("/mine", data = "<data>")]
async fn mine(db: &State<Database>, user: User, data: Json<DataDeskAct>) -> Result<Json<Mine>> {
    let settings = desk(&user).await?;
    let interaction = genuine(db, &settings, &data, Some(&settings.channel_apply)).await?;
    if interaction.kind != InteractionType::Component
        || !matches!(pressed(&interaction), Some(("start" | "mine" | "role", _)))
    {
        return Err(create_error!(InvalidOperation));
    }
    let person = applicant(db, &settings, &interaction).await?;
    mine_of(db, &settings, &person).await.map(Json)
}

/// What came of an application or a withdrawal
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ApplyOutcome {
    pub applied: bool,
    /// Why not: `already_open`, `wait`, `role_closed`, `adult_only`,
    /// `identity_missing` ("About you" not done for the current rules),
    /// `not_open` (withdrawing something already decided)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub problem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub application: Option<MineApplication>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub wait_until: Option<i64>,
}

fn field<'a>(interaction: &'a Interaction, id: &str) -> &'a str {
    interaction
        .modal_fields
        .iter()
        .find(|field| field.custom_id == id)
        .and_then(|field| field.value.as_deref())
        .map(str::trim)
        .unwrap_or_default()
}

pub(crate) fn length_problem(name: &str, min: usize, max: usize) -> revolt_result::Error {
    revolt_result::failed_validation(vec![revolt_result::ValidationProblem::length(
        name, min, max,
    )])
}

/// What came of "About you"
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AboutOutcome {
    pub saved: bool,
    /// Why not: `too_young`, `bad_age`, `rules_not_accepted`, `role_closed`
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub problem: Option<String>,
}

/// What a person chose in a group (`values`), or the one value of a field,
/// trimmed
fn chosen<'a>(interaction: &'a Interaction, id: &str) -> Vec<&'a str> {
    let Some(field) = interaction.modal_fields.iter().find(|field| field.custom_id == id) else {
        return Vec::new();
    };
    field
        .values
        .iter()
        .flatten()
        .map(String::as_str)
        .chain(field.value.as_deref())
        .map(str::trim)
        .collect()
}

/// # Team desk: "About you"
///
/// The bot hands back the submitted form `ta:about:<role>`: name, age,
/// country and the tick for the volunteer rules. They are kept as the
/// person's identity, seen only through "Show identity". Problems with the
/// answers come back in the outcome and store nothing.
#[post("/about", data = "<data>")]
async fn about(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskAct>,
) -> Result<Json<AboutOutcome>> {
    let settings = desk(&user).await?;
    if !settings.volunteers.enabled {
        return Err(create_error!(InvalidOperation));
    }
    let interaction = genuine(db, &settings, &data, Some(&settings.channel_apply)).await?;
    let role_key = match (interaction.kind, pressed(&interaction)) {
        (InteractionType::ModalSubmit, Some(("about", key))) => key.to_string(),
        _ => return Err(create_error!(InvalidOperation)),
    };
    let person = applicant(db, &settings, &interaction).await?;
    let refuse = |problem: &str| {
        Ok(Json(AboutOutcome {
            saved: false,
            problem: Some(problem.to_string()),
        }))
    };
    if role_key == "me" {
        // "Add my details": only a current volunteer with no application
        // open, who is not asked through an application
        let member = db
            .fetch_team_members_of(&person.id)
            .await?
            .iter()
            .any(|m| m.is_current());
        let open = db
            .fetch_team_applications_of(&person.id)
            .await?
            .iter()
            .any(|a| a.is_open());
        if !member || open {
            return refuse("not_a_volunteer");
        }
    } else if !settings.role(&role_key).is_some_and(offered) {
        return refuse("role_closed");
    }

    let age = match team_texts::parse_age(field(&interaction, "age")) {
        Some(age) if (MIN_AGE..=MAX_AGE).contains(&age) => age,
        Some(age) if age < MIN_AGE => return refuse("too_young"),
        _ => return refuse("bad_age"),
    };
    if !chosen(&interaction, "rules").contains(&"accept") {
        return refuse("rules_not_accepted");
    }
    let name = field(&interaction, "name");
    if !(MIN_NAME..=MAX_NAME).contains(&name.chars().count()) {
        return Err(length_problem("name", MIN_NAME, MAX_NAME));
    }
    let country = field(&interaction, "country");
    if !(MIN_COUNTRY..=MAX_COUNTRY).contains(&country.chars().count()) {
        return Err(length_problem("country", MIN_COUNTRY, MAX_COUNTRY));
    }

    purge_due(db, staff_now()).await?;
    // one form, one save
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }
    db.upsert_team_identity(&TeamIdentity {
        id: person.id.clone(),
        name: name.to_string(),
        age,
        country: country.to_string(),
        rules_version: settings.volunteers.rules_version.clone(),
        rules_at: staff_now(),
        purge_at: None,
    })
    .await?;
    Ok(Json(AboutOutcome {
        saved: true,
        problem: None,
    }))
}

/// # Team desk: file an application
///
/// The bot hands back the submitted form `ta:apply:<role>`. Who applies, for
/// which role and with which answers is read from that stored interaction.
#[post("/apply", data = "<data>")]
async fn apply(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskAct>,
) -> Result<Json<ApplyOutcome>> {
    let settings = desk(&user).await?;
    let interaction = genuine(db, &settings, &data, Some(&settings.channel_apply)).await?;
    let role_key = match (interaction.kind, pressed(&interaction)) {
        (InteractionType::ModalSubmit, Some(("apply", key))) => key.to_string(),
        _ => return Err(create_error!(InvalidOperation)),
    };
    let person = applicant(db, &settings, &interaction).await?;
    let refuse = |problem: &str, application: Option<MineApplication>, wait_until: Option<i64>| {
        Ok(Json(ApplyOutcome {
            applied: false,
            problem: Some(problem.to_string()),
            application,
            wait_until,
        }))
    };

    let Some(role) = settings.role(&role_key).filter(|role| offered(role)) else {
        return refuse("role_closed", None, None);
    };
    // the stated age is read only for a role that asks for it
    if role.adult && !is_adult(db, &person).await {
        return refuse("adult_only", None, None);
    }

    // what is past its time goes first, so a stale identity cannot pass
    purge_due(db, staff_now()).await?;
    // "About you" first, when the volunteer care is on
    let identity = db.fetch_team_identity(&person.id).await?;
    if settings.volunteers.enabled
        && !identity
            .as_ref()
            .is_some_and(|identity| identity.rules_version == settings.volunteers.rules_version)
    {
        return refuse("identity_missing", None, None);
    }

    // the answers, as the person sent them
    let languages = field(&interaction, "languages");
    let length = languages.chars().count();
    if !(MIN_LANGUAGES..=MAX_LANGUAGES).contains(&length) {
        return Err(length_problem("languages", MIN_LANGUAGES, MAX_LANGUAGES));
    }
    let hours = field(&interaction, "hours");
    let Some((_, hours_text)) = HOURS.iter().find(|(value, _)| *value == hours) else {
        return Err(length_problem("hours", 1, 1));
    };
    let mut answers = vec![
        TeamAnswer {
            id: "languages".to_string(),
            label: "Languages".to_string(),
            value: languages.to_string(),
        },
        TeamAnswer {
            id: "hours".to_string(),
            label: "Time".to_string(),
            value: hours_text.to_string(),
        },
    ];
    // stored with the English question, as the reviewers read it
    for question in team_texts::questions(role, "en") {
        let value = field(&interaction, &question.id);
        let length = value.chars().count();
        if !(MIN_ANSWER..=MAX_ANSWER).contains(&length) {
            return Err(length_problem(&question.id, MIN_ANSWER, MAX_ANSWER));
        }
        answers.push(TeamAnswer {
            id: question.id,
            label: question.label,
            value: value.to_string(),
        });
    }

    let now = staff_now();
    purge_due(db, now).await?;
    let existing = db.fetch_team_applications_of(&person.id).await?;
    match standing(&settings, &existing, now) {
        (Some(open), None) => {
            return refuse("already_open", Some(mine_application(&settings, &open)), None)
        }
        (Some(last), Some(until)) => {
            return refuse("wait", Some(mine_application(&settings, &last)), Some(until))
        }
        _ => {}
    }
    // one form, one application
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }

    let application = TeamApplication {
        id: ulid::Ulid::new().to_string(),
        user_id: person.id.clone(),
        role: role.key.clone(),
        status: TeamApplicationStatus::Open,
        // kept only for a role that asks for it; no age fact otherwise
        adult: role.adult,
        answers,
        submitted_at: now,
        acted_by: None,
        acted_at: None,
        interviewer: None,
        reason: None,
        grant: None,
        purge_at: None,
    };
    db.insert_team_application(&application).await?;
    if identity.is_some_and(|identity| identity.purge_at.is_some()) {
        keep_identity(db, &person.id).await;
    }
    // two forms sent in the same moment: this one goes if another is open
    let open: Vec<_> = db
        .fetch_team_applications_of(&person.id)
        .await?
        .into_iter()
        .filter(|a| a.is_open())
        .collect();
    // (whichever it is: comparing ids let both stay when the later id was
    // written first; at worst both go and the person sends the form again)
    if let Some(other) = open.iter().find(|a| a.id != application.id) {
        db.delete_team_application(&application.id).await?;
        return refuse("already_open", Some(mine_application(&settings, other)), None);
    }

    team_texts::send(
        db,
        &person.id,
        "received",
        Some(role),
        &role.key,
        &[
            ("channel", &format!("<#{}>", settings.channel_apply)),
            ("privacy", &settings.privacy_url),
        ],
    )
    .await;

    Ok(Json(ApplyOutcome {
        applied: true,
        problem: None,
        application: Some(mine_application(&settings, &application)),
        wait_until: None,
    }))
}

/// # Team desk: the person takes their application back
///
/// The bot hands back the press of `ta:withdraw:<id>` in the apply channel.
/// Only the person who applied can; the answers are erased at once.
#[post("/withdraw", data = "<data>")]
async fn withdraw(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskAct>,
) -> Result<Json<ApplyOutcome>> {
    let settings = desk(&user).await?;
    let interaction = genuine(db, &settings, &data, Some(&settings.channel_apply)).await?;
    let id = match (interaction.kind, pressed(&interaction)) {
        (InteractionType::Component, Some(("withdraw", id))) if !id.is_empty() => id.to_string(),
        _ => return Err(create_error!(InvalidOperation)),
    };
    // not `applicant`: a person who left the server may still withdraw
    let application = match db.fetch_team_application(&id).await {
        Ok(application) if application.user_id == interaction.user_id => application,
        // someone else's, or none: the same answer for both
        _ => return Err(create_error!(NotFound)),
    };
    let not_open = |application: &TeamApplication| {
        Ok(Json(ApplyOutcome {
            applied: false,
            problem: Some("not_open".to_string()),
            application: Some(mine_application(&settings, application)),
            wait_until: None,
        }))
    };
    if !application.is_open() {
        return not_open(&application);
    }
    let now = staff_now();
    let mut withdrawn = application.clone();
    withdrawn.status = TeamApplicationStatus::Withdrawn;
    withdrawn.answers = Vec::new();
    withdrawn.reason = None;
    withdrawn.interviewer = None;
    withdrawn.acted_by = None;
    withdrawn.acted_at = Some(now);
    withdrawn.purge_at = Some(now + withdraw_wait_ms(&settings));
    if !db
        .replace_team_application_if(&withdrawn, application.status)
        .await?
    {
        // decided in the moment between
        return match db.fetch_team_application(&id).await {
            Ok(current) => not_open(&current),
            Err(_) => Err(create_error!(NotFound)),
        };
    }
    // withdrawing erases what the person gave, identity included
    release_identity(db, &settings, &withdrawn, true).await;
    team_texts::send(
        db,
        &withdrawn.user_id,
        "withdrawn",
        settings.role(&withdrawn.role),
        &withdrawn.role,
        &[],
    )
    .await;
    Ok(Json(ApplyOutcome {
        applied: true,
        problem: None,
        application: Some(mine_application(&settings, &withdrawn)),
        wait_until: Some(now + withdraw_wait_ms(&settings)),
    }))
}

// ---- the reviewers' side ----

/// What came of a reviewer's press
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReviewOutcome {
    pub applied: bool,
    /// Why not: `already_decided`, `withdrawn`, `gone`, `own_application`,
    /// `already_interview`, `not_eligible` (the account is suspended or
    /// banned)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub problem: Option<String>,
    /// `interview`, `accept` or `reject`
    pub action: String,
    pub id: String,
    pub role: String,
    pub role_name: String,
    /// `name#1234` of the applicant (empty when the application is gone)
    pub applicant_name: String,
    /// Status after this call
    pub status: String,
    pub reviewer_name: String,
    /// Who last acted (the reviewer when `applied`)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_at: Option<i64>,
    /// `accept`: `granted`, `pending_join` or `manual`
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub grant: Option<String>,
    /// `accept`: name of the server the role belongs to
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub server_name: Option<String>,
    /// `interview`: the private conversation between reviewer and applicant
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dm_channel: Option<String>,
}

/// Give the role of an accepted application, if the system may. Never for
/// a manual role, and never a role that can manage the server.
async fn give_role(db: &Database, role: Option<&TeamRole>, user_id: &str) -> TeamGrant {
    let Some(role) = role else {
        return TeamGrant::Manual;
    };
    // a suspended or banned account gets nothing; it waits, and is given
    // the role by a later look once it is in good standing again
    match db.fetch_user(user_id).await {
        Ok(person) if !barred(&person) => {}
        _ => return TeamGrant::PendingJoin,
    }
    if role.is_manual() || role.role.trim().is_empty() {
        return TeamGrant::Manual;
    }
    let Ok(server) = db.fetch_server(&role.server).await else {
        return TeamGrant::Manual;
    };
    if powerful(db, &server, &role.role).await {
        log::warn!(
            "team role `{}` names a server role that can manage the server: not given",
            role.key
        );
        return TeamGrant::Manual;
    }
    // the server's "Zrzur Team" role goes with it, under the same rule; one
    // that is missing or can manage the server is left out, not the whole grant
    let mut wanted = vec![role.role.clone()];
    if !role.team_role.trim().is_empty() && role.team_role != role.role {
        if powerful(db, &server, &role.team_role).await {
            log::warn!(
                "team role `{}`: its `team_role` is missing or can manage the server: not given",
                role.key
            );
        } else {
            wanted.push(role.team_role.clone());
        }
    }
    let Ok(mut member) = db.fetch_member(&server.id, user_id).await else {
        return TeamGrant::PendingJoin;
    };
    let mut roles = member.roles.clone();
    for id in wanted {
        if !roles.contains(&id) {
            roles.push(id);
        }
    }
    if roles.len() == member.roles.len() {
        return TeamGrant::Granted;
    }
    match member
        .update(
            db,
            PartialMember {
                roles: Some(roles),
                ..Default::default()
            },
            vec![],
        )
        .await
    {
        Ok(()) => TeamGrant::Granted,
        Err(error) => {
            log::warn!("team role `{}` not given: {:?}", role.key, error.error_type);
            TeamGrant::PendingJoin
        }
    }
}

pub(crate) async fn server_name(db: &Database, role: Option<&TeamRole>) -> String {
    match role {
        Some(role) => db
            .fetch_server(&role.server)
            .await
            .map(|server| server.name)
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// # Team desk: act for the reviewer who pressed
///
/// The bot hands back the interaction of a button press (`ta:interview:<id>`,
/// `ta:accept:<id>`) or of the submitted rejection form (`ta:reject:<id>`,
/// field `reason`) in the staff channel. What is done and to which
/// application is read from that stored interaction.
#[post("/act", data = "<data>")]
async fn act(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskAct>,
) -> Result<Json<ReviewOutcome>> {
    let settings = desk(&user).await?;
    let interaction = genuine(db, &settings, &data, None).await?;
    // a press outside every staff channel counts for nothing; which staff
    // channel an application belongs in is checked once it is read
    if !settings
        .review_channels()
        .iter()
        .any(|channel| channel == &interaction.channel_id)
    {
        return Err(no_permission());
    }
    let (action, id) = match pressed(&interaction) {
        Some((action @ ("interview" | "accept" | "reject"), id)) if !id.is_empty() => {
            (action.to_string(), id.to_string())
        }
        _ => return Err(create_error!(InvalidOperation)),
    };
    // a rejection comes from the form (it carries the reason); the rest
    // from a button
    let reason = match (action.as_str(), interaction.kind) {
        ("reject", InteractionType::ModalSubmit) => {
            let reason = field(&interaction, "reason").to_string();
            let length = reason.chars().count();
            if !(MIN_REASON..=MAX_REASON).contains(&length) {
                return Err(length_problem("reason", MIN_REASON, MAX_REASON));
            }
            Some(reason)
        }
        ("reject", _) | (_, InteractionType::ModalSubmit) => {
            return Err(create_error!(InvalidOperation))
        }
        (_, InteractionType::Component) => None,
        _ => return Err(create_error!(InvalidOperation)),
    };

    purge_due(db, staff_now()).await?;
    let found = db.fetch_team_application(&id).await.ok();
    // an application that is gone is judged as one of no role: only the
    // common reviewers get an answer about it, in the common channel
    let key = found.as_ref().map(|a| a.role.as_str()).unwrap_or_default();
    let person = reviewer(db, &settings, &interaction, settings.role(key)).await?;
    if interaction.channel_id != settings.review_channel(key) {
        return Err(no_permission());
    }
    let reviewer_name = full_name(&person);

    let mut outcome = ReviewOutcome {
        applied: false,
        problem: None,
        action: action.clone(),
        id: id.clone(),
        role: found.as_ref().map(|a| a.role.clone()).unwrap_or_default(),
        role_name: found
            .as_ref()
            .map(|a| role_name(&settings, &a.role))
            .unwrap_or_default(),
        applicant_name: String::new(),
        status: found
            .as_ref()
            .map(|a| status_word(a.status))
            .unwrap_or("Gone")
            .to_string(),
        reviewer_name: reviewer_name.clone(),
        acted_by_name: None,
        acted_at: found.as_ref().and_then(|a| a.acted_at),
        grant: None,
        server_name: None,
        dm_channel: None,
    };
    let refuse = |mut outcome: ReviewOutcome, problem: &str| {
        outcome.problem = Some(problem.to_string());
        Ok(Json(outcome))
    };
    let Some(application) = found else {
        return refuse(outcome, "gone");
    };
    outcome.applicant_name = name_of(db, &application.user_id).await;
    if let Some(id) = &application.acted_by {
        outcome.acted_by_name = Some(name_of(db, id).await);
    }
    if application.status == TeamApplicationStatus::Withdrawn {
        return refuse(outcome, "withdrawn");
    }
    if !application.is_open() {
        return refuse(outcome, "already_decided");
    }
    if application.user_id == person.id {
        return refuse(outcome, "own_application");
    }
    if action == "interview" && application.status == TeamApplicationStatus::Interview {
        return refuse(outcome, "already_interview");
    }
    let Ok(applicant) = db.fetch_user(&application.user_id).await else {
        return refuse(outcome, "gone");
    };
    let role = settings.role(&application.role);
    if action == "accept" {
        // a suspended or banned account is not taken into the team
        if barred(&applicant) {
            return refuse(outcome, "not_eligible");
        }
        // an acceptance counts only from the private question this server
        // worded: a card button that says something else accepts nobody
        let expected = confirm_text(
            &outcome.applicant_name,
            &outcome.role_name,
            &server_name(db, role).await,
            role.is_none_or(|role| role.is_manual()),
        );
        if !confirmed(db, &settings, &interaction, &id, &expected).await {
            return Err(create_error!(InvalidOperation));
        }
    }

    let now = staff_now();
    let mut decided = application.clone();
    decided.acted_by = Some(person.id.clone());
    decided.acted_at = Some(now);
    match action.as_str() {
        "interview" => {
            decided.status = TeamApplicationStatus::Interview;
            decided.interviewer = Some(person.id.clone());
        }
        "accept" => {
            decided.status = TeamApplicationStatus::Accepted;
            decided.purge_at = Some(now + days_ms(settings.accepted_retention_days));
            // written with the decision: if the server stops before the role
            // is given, the next look gives it
            decided.grant = Some(TeamGrant::PendingJoin);
        }
        _ => {
            decided.status = TeamApplicationStatus::Rejected;
            decided.reason = reason.clone();
            decided.purge_at = Some(now + days_ms(settings.rejected_days()));
        }
    }

    let mut record = AuditRecord {
        actor_id: person.id.clone(),
        action: format!("team.application.{action}"),
        target_kind: Some("user".to_string()),
        target_id: Some(application.user_id.clone()),
        // the reason is about a person and is deleted with the application:
        // the log keeps that there was one, not what it said
        reason: None,
        summary: Some(format!(
            "role {} application {} via bot {} interaction {}{}",
            application.role,
            application.id,
            settings.bot,
            interaction.id,
            match &reason {
                Some(reason) => format!(" reason of {} characters", reason.chars().count()),
                None => String::new(),
            }
        )),
        ..Default::default()
    };
    // the record first: no record, no decision
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    if !db
        .replace_team_application_if(&decided, application.status)
        .await?
    {
        // someone decided, or the person withdrew, in the moment between
        record.summary = Some("lost the race: no longer open".to_string());
        let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
        return match db.fetch_team_application(&id).await {
            Ok(current) => {
                outcome.status = status_word(current.status).to_string();
                outcome.acted_at = current.acted_at;
                outcome.acted_by_name = match &current.acted_by {
                    Some(id) => Some(name_of(db, id).await),
                    None => None,
                };
                let problem = if current.status == TeamApplicationStatus::Withdrawn {
                    "withdrawn"
                } else {
                    "already_decided"
                };
                refuse(outcome, problem)
            }
            Err(_) => refuse(outcome, "gone"),
        };
    }

    match action.as_str() {
        "reject" => release_identity(db, &settings, &decided, false).await,
        "accept" => keep_identity(db, &application.user_id).await,
        _ => {}
    }

    match action.as_str() {
        "interview" => {
            // the conversation the reviewer writes in; opened for both
            if let Ok(mut channel) = Channel::create_dm(db, &person, &applicant).await {
                if let Channel::DirectMessage { active: false, .. } = &channel {
                    let _ = channel
                        .update(
                            db,
                            PartialChannel {
                                active: Some(true),
                                ..Default::default()
                            },
                            vec![],
                        )
                        .await;
                }
                outcome.dm_channel = Some(channel.id().to_string());
            }
            team_texts::send(
                db,
                &applicant.id,
                "interview",
                role,
                &application.role,
                &[("reviewer", &reviewer_name)],
            )
            .await;
        }
        "accept" => {
            // the acceptance is stored: the person is a volunteer now
            if settings.volunteers.enabled {
                if let Err(error) =
                    volunteers::enrol(db, &settings, &application, &person.id).await
                {
                    log::warn!("team member record not made: {:?}", error.error_type);
                }
            }
            let grant = give_role(db, role, &applicant.id).await;
            decided.grant = Some(grant);
            // the status is ours now; only the grant is added
            let _ = db
                .replace_team_application_if(&decided, TeamApplicationStatus::Accepted)
                .await;
            let server = server_name(db, role).await;
            let key = match grant {
                TeamGrant::Granted => "accepted",
                TeamGrant::PendingJoin => "accepted_join",
                TeamGrant::Manual => "accepted_manual",
            };
            team_texts::send(
                db,
                &applicant.id,
                key,
                role,
                &application.role,
                &[("server", &server)],
            )
            .await;
            // the invite to the team server goes with the welcome, in private
            if settings.volunteers.enabled {
                if let Some(code) =
                    volunteers::invite_to_team_server(db, &settings, &applicant.id).await
                {
                    team_texts::send(
                        db,
                        &applicant.id,
                        "volunteer.invite",
                        role,
                        &application.role,
                        &[("invite", &code)],
                    )
                    .await;
                }
            }
            if grant == TeamGrant::Manual {
                // the owner of the server gives such a role by hand
                let owner = match role {
                    Some(role) => db.fetch_server(&role.server).await.ok(),
                    None => None,
                };
                let owner = match owner {
                    Some(server) => Some(server.owner),
                    None => db.fetch_server(&settings.server).await.ok().map(|s| s.owner),
                };
                if let Some(owner) = owner {
                    team_texts::send(
                        db,
                        &owner,
                        "owner_manual",
                        role,
                        &application.role,
                        &[
                            ("reviewer", &reviewer_name),
                            ("applicant", &outcome.applicant_name),
                        ],
                    )
                    .await;
                }
            }
            // outside the bot: the reviewer reads whom they accepted
            team_texts::send(
                db,
                &person.id,
                "accepted_notice",
                role,
                &application.role,
                &[("applicant", &outcome.applicant_name)],
            )
            .await;
            outcome.grant = Some(grant_word(grant).to_string());
            outcome.server_name = Some(server);
        }
        _ => {
            team_texts::send(
                db,
                &applicant.id,
                "rejected",
                role,
                &application.role,
                &[
                    ("reason", reason.as_deref().unwrap_or_default()),
                    ("days", &settings.cooldown_days.max(0).to_string()),
                ],
            )
            .await;
        }
    }

    outcome.applied = true;
    outcome.status = status_word(decided.status).to_string();
    outcome.acted_by_name = Some(reviewer_name);
    outcome.acted_at = Some(now);
    Ok(Json(outcome))
}

/// What "Show identity" shows a reviewer. Name and country are already
/// escaped (`team_texts::plain`): mentions, links and markdown are dead text.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IdentityView {
    pub name: String,
    pub age: u8,
    pub country: String,
    /// The stated age and the account's own age group disagree
    pub age_note: bool,
    /// There is nothing to show: no "About you", or it was erased
    pub missing: bool,
}

/// # Team desk: show identity
///
/// The bot hands back the press of `ta:identity:<application id>` in the
/// role's review channel. The presser must be a reviewer for that role right
/// now. Every view is logged (who looked at whom, never what was shown); a
/// refused press is logged too.
#[post("/identity", data = "<data>")]
async fn identity(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskAct>,
) -> Result<Json<IdentityView>> {
    let settings = desk(&user).await?;
    if !settings.volunteers.enabled {
        return Err(create_error!(InvalidOperation));
    }
    let interaction = genuine(db, &settings, &data, None).await?;
    // one press, one look
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }
    let id = match (interaction.kind, pressed(&interaction)) {
        (InteractionType::Component, Some(("identity", id))) if !id.is_empty() => id.to_string(),
        _ => return Err(create_error!(InvalidOperation)),
    };

    // only "not found" means gone; a database fault is an error, not an answer
    let found = match db.fetch_team_application(&id).await {
        Ok(application) => Some(application),
        Err(error) if matches!(error.error_type, revolt_result::ErrorType::NotFound) => None,
        Err(error) => return Err(error),
    };
    let key = found.as_ref().map(|a| a.role.as_str()).unwrap_or_default();
    let target = found.as_ref().map(|a| a.user_id.clone());
    // the same words for a view and a refusal: the log says who, about whom
    let record = |summary: String| AuditRecord {
        actor_id: interaction.user_id.clone(),
        action: "team.identity.view".to_string(),
        target_kind: target.as_ref().map(|_| "user".to_string()),
        target_id: target.clone(),
        reason: None,
        summary: Some(summary),
        ..Default::default()
    };
    let note = format!(
        "application {} via bot {} interaction {}",
        id, settings.bot, interaction.id
    );
    let refused = |why: &'static str| {
        let record = record(format!("refused, {why}: {note}"));
        async move {
            let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
            Err::<Json<IdentityView>, _>(no_permission())
        }
    };

    // the press must come from a review channel of an official server
    let in_official = interaction
        .server_id
        .as_deref()
        .is_some_and(|server| official_servers(&settings).iter().any(|s| s == server));
    if !in_official
        || !settings
            .review_channels()
            .iter()
            .any(|channel| channel == &interaction.channel_id)
    {
        return refused("not a review channel").await;
    }
    let allowed = reviewer(db, &settings, &interaction, settings.role(key)).await;
    if allowed.is_err() {
        return refused("not a reviewer").await;
    }
    if interaction.channel_id != settings.review_channel(key) {
        return refused("wrong review channel").await;
    }
    let person = allowed?;

    let nothing = || async {
        record_platform_audit(
            db,
            record(format!("nothing on file: {note}")),
            AuditOutcome::Success,
        )
        .await?;
        Ok::<_, revolt_result::Error>(Json(missing_identity()))
    };
    let Some(application) = found else {
        return nothing().await;
    };
    // nobody reads their own
    if application.user_id == person.id {
        return refused("own application").await;
    }
    purge_due(db, staff_now()).await?;
    let Some(view) = identity_view(db, &application.user_id).await? else {
        return nothing().await;
    };
    // the record first: no record, no view
    record_platform_audit(
        db,
        record(note),
        AuditOutcome::Success,
    )
    .await?;
    Ok(Json(view))
}

/// What is on file for a person, escaped; `None` when there is nothing. The
/// caller logs the view first.
pub(crate) async fn identity_view(db: &Database, user_id: &str) -> Result<Option<IdentityView>> {
    let Some(stored) = db.fetch_team_identity(user_id).await? else {
        return Ok(None);
    };
    let age_note = match db.fetch_user(user_id).await {
        Ok(person) => age_disagrees(stored.age, owner_band(db, &person).await),
        Err(_) => false,
    };
    Ok(Some(IdentityView {
        name: team_texts::plain(&stored.name),
        age: stored.age,
        country: team_texts::plain(&stored.country),
        age_note,
        missing: false,
    }))
}

/// The answer when nothing is on file
pub(crate) fn no_identity() -> IdentityView {
    missing_identity()
}

fn missing_identity() -> IdentityView {
    IdentityView {
        name: String::new(),
        age: 0,
        country: String::new(),
        age_note: false,
        missing: true,
    }
}

/// A role given after the person joined the server
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settled {
    pub id: String,
    pub applicant_name: String,
    pub role_name: String,
    pub server_name: String,
}

/// # Team desk: give roles that waited for a join
///
/// Accepted people who were not yet in the role's server receive the role
/// once they are. Takes nothing from the bot; it only asks the server to
/// look again. Also removes applications whose time is up.
#[post("/settle")]
async fn settle(db: &State<Database>, user: User) -> Result<Json<Vec<Settled>>> {
    let settings = desk(&user).await?;
    purge_due(db, staff_now()).await?;
    let mut settled = Vec::new();
    if settings.volunteers.enabled {
        enrol_accepted(db, &settings).await?;
    }
    for mut application in db.fetch_team_applications_pending_grant().await? {
        let role = settings.role(&application.role);
        let grant = give_role(db, role, &application.user_id).await;
        if grant == TeamGrant::PendingJoin {
            continue;
        }
        application.grant = Some(grant);
        if !db
            .replace_team_application_if(&application, TeamApplicationStatus::Accepted)
            .await?
        {
            continue;
        }
        if grant != TeamGrant::Granted {
            continue;
        }
        let server = server_name(db, role).await;
        let shown_role = role_name(&settings, &application.role);
        team_texts::send(
            db,
            &application.user_id,
            "granted",
            role,
            &application.role,
            &[("server", &server)],
        )
        .await;
        settled.push(Settled {
            id: application.id.clone(),
            applicant_name: name_of(db, &application.user_id).await,
            role_name: shown_role,
            server_name: server,
        });
    }
    Ok(Json(settled))
}

/// People accepted before the records existed (or whose record was not
/// written) get one now. Each who has not given their details yet is asked,
/// once, to add them: the record being new is what makes it once.
async fn enrol_accepted(db: &Database, settings: &FeaturesTeamApplications) -> Result<()> {
    // one look at every record, not one per application
    let mut known: std::collections::HashMap<(String, String), revolt_database::TeamMember> = db
        .fetch_team_members(&Default::default())
        .await?
        .into_iter()
        .map(|m| ((m.user_id.clone(), m.role.clone()), m))
        .collect();
    for application in db.fetch_team_applications_accepted().await? {
        let key = (application.user_id.clone(), application.role.clone());
        if settings.role(&application.role).is_none() || known.contains_key(&key) {
            continue;
        }
        match volunteers::enrol(db, settings, &application, "system").await {
            Ok(member) => {
                known.insert(key, member);
            }
            Err(error) => log::warn!("team member record not made: {:?}", error.error_type),
        }
    }
    // everyone current without details who was not asked yet is asked, once
    for mut member in known.into_values() {
        if !member.is_current()
            || member.history.iter().any(|h| h.action == "identity_requested")
            || settings.role(&member.role).is_none()
        {
            continue;
        }
        if matches!(db.fetch_team_identity(&member.user_id).await, Ok(Some(_))) {
            continue;
        }
        member.history.push(revolt_database::TeamHistory {
            at: staff_now(),
            actor: "system".to_string(),
            action: "identity_requested".to_string(),
        });
        if db.update_team_member(&member).await.is_err() {
            continue;
        }
        team_texts::send(
            db,
            &member.user_id,
            "volunteer.identity_request",
            settings.role(&member.role),
            &member.role,
            &[],
        )
        .await;
    }
    Ok(())
}

// ---- the volunteer's own buttons ----

/// The one label a leaving may be confirmed with
pub const LEAVE_LABEL: &str = "Yes, leave";

/// One current place of a person in the team
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MyMembership {
    pub id: String,
    pub role: String,
    pub role_name: String,
    /// Escaped (`team_texts::plain`)
    pub server_name: String,
    /// `trial`, `active`, `away` or `inactive`
    pub status: String,
    pub trial_ends_at: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub away_until: Option<i64>,
}

/// Counts of this UTC month against the minimum
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MyMonth {
    pub contributions: u32,
    pub minimum: i64,
}

/// The private question before leaving, worded by the server: the bot shows
/// `text` unchanged with one button (`label`, `custom_id`)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LeaveConfirm {
    pub text: String,
    pub label: String,
    pub custom_id: String,
}

/// What a volunteer sees about themselves, and nobody else
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MyStatus {
    pub memberships: Vec<MyMembership>,
    pub month: MyMonth,
    /// Longest "away" in days, for the form
    pub away_max_days: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub confirm: Option<LeaveConfirm>,
}

pub(crate) fn member_word(status: revolt_database::TeamMemberStatus) -> &'static str {
    use revolt_database::TeamMemberStatus as S;
    match status {
        S::Trial => "trial",
        S::Active => "active",
        S::Away => "away",
        S::Inactive => "inactive",
        S::Left => "left",
    }
}

async fn my_status(
    db: &Database,
    settings: &FeaturesTeamApplications,
    user_id: &str,
) -> Result<MyStatus> {
    let mut memberships = Vec::new();
    for member in db.fetch_team_members_of(user_id).await? {
        if !member.is_current() {
            continue;
        }
        let server = db
            .fetch_server(&member.server)
            .await
            .map(|server| server.name)
            .unwrap_or_default();
        memberships.push(MyMembership {
            role_name: role_name(settings, &member.role),
            server_name: team_texts::plain(&server),
            status: member_word(member.status).to_string(),
            trial_ends_at: member.trial_ends_at,
            away_until: member.away_until,
            id: member.id,
            role: member.role,
        });
    }
    let (from, to) = volunteers::month_bounds(staff_now());
    let days = db.fetch_team_activity(user_id, from, to).await?;
    Ok(MyStatus {
        memberships,
        month: MyMonth {
            contributions: volunteers::month_total(&days),
            minimum: settings.volunteers.monthly_minimum,
        },
        away_max_days: settings.volunteers.away_max_days,
        confirm: None,
    })
}

/// The settings and the stored interaction of a volunteer's own press: the
/// switch is on, the hand-back is genuine, and it comes from a person (not a
/// bot, not suspended or banned), who is the one the answer is about.
async fn own_press(
    db: &Database,
    user: &User,
    data: &DataDeskAct,
) -> Result<(FeaturesTeamApplications, Interaction)> {
    let settings = desk(user).await?;
    if !settings.volunteers.enabled {
        return Err(create_error!(InvalidOperation));
    }
    let interaction = genuine(db, &settings, data, None).await?;
    let person = db
        .fetch_user(&interaction.user_id)
        .await
        .map_err(|_| no_permission())?;
    if barred(&person) {
        return Err(no_permission());
    }
    Ok((settings, interaction))
}

/// # Team desk: my status
///
/// The bot hands back the press of `ta:status`, in a server channel or in a
/// private message. The answer is always about the person who pressed.
#[post("/status", data = "<data>")]
async fn status(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskAct>,
) -> Result<Json<MyStatus>> {
    let (settings, interaction) = own_press(db, &user, &data).await?;
    match (interaction.kind, pressed(&interaction)) {
        (InteractionType::Component, Some(("status", ""))) => {}
        _ => return Err(create_error!(InvalidOperation)),
    }
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }
    Ok(Json(my_status(db, &settings, &interaction.user_id).await?))
}

/// # Team desk: away
///
/// The bot hands back the submitted form `ta:away`, field `days`: 1 up to the
/// longest away the settings allow, or `0` to be back. It applies to every
/// current place of the person.
#[post("/away", data = "<data>")]
async fn away(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskAct>,
) -> Result<Json<MyStatus>> {
    let (settings, interaction) = own_press(db, &user, &data).await?;
    match (interaction.kind, pressed(&interaction)) {
        (InteractionType::ModalSubmit, Some(("away", ""))) => {}
        _ => return Err(create_error!(InvalidOperation)),
    }
    let max = settings.volunteers.away_max_days.max(0);
    let days = match volunteers::parse_days(field(&interaction, "days")) {
        Some(days) if days <= max => days,
        _ => return Err(length_problem("days", 0, max as usize)),
    };
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }
    let mut first: Option<String> = None;
    for mut member in db.fetch_team_members_of(&interaction.user_id).await? {
        if volunteers::set_away(db, &mut member, &interaction.user_id, days, max).await?
            && first.is_none()
        {
            first = Some(member.role.clone());
        }
    }
    if let Some(role) = first {
        let key = if days > 0 { "volunteer.away" } else { "volunteer.back" };
        team_texts::send(
            db,
            &interaction.user_id,
            key,
            settings.role(&role),
            &role,
            &[("days", &days.to_string())],
        )
        .await;
    }
    Ok(Json(my_status(db, &settings, &interaction.user_id).await?))
}

/// The words a person confirms before leaving. The server writes them, the
/// bot shows them unchanged, and `leave` reads them back from the private
/// message that was pressed.
fn leave_text(role: &str, server: &str) -> String {
    let place = confirm_word(server);
    let place = if place.is_empty() { String::new() } else { format!(" in {place}") };
    format!(
        "Leave the Zrzur Team as {}{place}? Your role there ends now. Thank you for the time you gave; you are welcome back whenever you like.",
        confirm_word(role)
    )
}

/// # Team desk: leave
///
/// Two steps. The press of `ta:leave:<member id>` is answered with the
/// server's own question (`confirm`); the press of its one button,
/// `ta:leave_confirm:<member id>`, from that unchanged private message, ends
/// the membership. Only the person's own membership can be named.
#[post("/leave", data = "<data>")]
async fn leave(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskAct>,
) -> Result<Json<MyStatus>> {
    let (settings, interaction) = own_press(db, &user, &data).await?;
    let (confirming, id) = match (interaction.kind, pressed(&interaction)) {
        (InteractionType::Component, Some(("leave", id))) if !id.is_empty() => {
            (false, id.to_string())
        }
        (InteractionType::Component, Some(("leave_confirm", id))) if !id.is_empty() => {
            (true, id.to_string())
        }
        _ => return Err(create_error!(InvalidOperation)),
    };
    // someone else's, gone, or already left: the same answer for all
    let Some(mut member) = db
        .fetch_team_members_of(&interaction.user_id)
        .await?
        .into_iter()
        .find(|member| member.id == id && member.is_current())
    else {
        return Err(create_error!(NotFound));
    };
    let server = db
        .fetch_server(&member.server)
        .await
        .map(|server| server.name)
        .unwrap_or_default();
    let expected = leave_text(&role_name(&settings, &member.role), &server);
    let button = format!("ta:leave_confirm:{id}");

    if !confirming {
        if !db.claim_interaction_use(&interaction.id).await? {
            return Err(create_error!(InteractionExpired));
        }
        let mut shown = my_status(db, &settings, &interaction.user_id).await?;
        shown.confirm = Some(LeaveConfirm {
            text: expected,
            label: LEAVE_LABEL.to_string(),
            custom_id: button,
        });
        return Ok(Json(shown));
    }

    if !confirmed_button(db, &settings, &interaction, &expected, LEAVE_LABEL, &button).await {
        return Err(create_error!(InvalidOperation));
    }
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }
    volunteers::end_membership(db, &settings, &mut member, &interaction.user_id, "left", None)
        .await?;
    Ok(Json(my_status(db, &settings, &interaction.user_id).await?))
}

// ---- the weekly report ----

/// One team's week. Counts and display names only.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TeamWeek {
    pub role: String,
    pub role_name: String,
    /// Escaped (`team_texts::plain`)
    pub server_name: String,
    /// Volunteers who belonged to the team at any time during the week
    pub volunteers: u32,
    pub trial: u32,
    pub active: u32,
    pub away: u32,
    pub inactive: u32,
    /// The five kinds of count in the team's own server over the week, for
    /// the team's volunteers only
    pub contributions: u32,
    /// Account display names (escaped) of volunteers who joined this week
    pub new_names: Vec<String>,
}

/// The weekly report for the staff channel
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WeekReport {
    /// The Monday, yyyymmdd
    pub week: i32,
    pub teams: Vec<TeamWeek>,
}

/// Whether `week` (yyyymmdd) is a real date and a Monday
fn is_monday(week: i32) -> bool {
    use revolt_database::util::team_activity::{day_of, day_start};
    if week <= 0 || day_of(day_start(week)) != week {
        return false;
    }
    // day 0 (1970-01-01) was a Thursday
    (day_start(week) / DAY_MS + 3).rem_euclid(7) == 0
}

/// # Team desk: the weekly report
///
/// `week` is the Monday (yyyymmdd) of a completed week, Monday to Sunday
/// UTC; the current or a future week is refused. Only the desk's bot, and
/// only while the volunteer care is on. Per team role: `volunteers` are the
/// memberships that were current at any time during the week; the four
/// status counts are their status now, so someone who left during the week
/// counts in `volunteers` only. `contributions` are the five counts in the
/// team's own server, for those volunteers only, each person once per team.
/// `new_names` are account display names of those who joined that week;
/// never name, age, country or anything of their identity.
#[get("/report?<week>")]
async fn report(db: &State<Database>, user: User, week: i32) -> Result<Json<WeekReport>> {
    use revolt_database::util::team_activity::{add_days, contributions, day_start};
    use revolt_database::TeamMemberStatus as S;

    let settings = desk(&user).await?;
    if !settings.volunteers.enabled || !is_monday(week) {
        return Err(create_error!(InvalidOperation));
    }
    let from = day_start(week);
    let until = from + 7 * DAY_MS;
    // only a finished week
    if until > staff_now() {
        return Err(create_error!(InvalidOperation));
    }
    let last = add_days(week, 6);

    let mut teams = Vec::new();
    for role in settings.roles.iter().filter(|role| role.ready()) {
        if role.server.trim().is_empty() {
            continue;
        }
        let members = db
            .fetch_team_members(&revolt_database::TeamMemberFilter {
                role: Some(role.key.clone()),
                ..Default::default()
            })
            .await?;
        let mut seen = std::collections::HashSet::new();
        let mut team = TeamWeek {
            role: role.key.clone(),
            role_name: role_name(&settings, &role.key),
            server_name: team_texts::plain(&server_name(db, Some(role)).await),
            volunteers: 0,
            trial: 0,
            active: 0,
            away: 0,
            inactive: 0,
            contributions: 0,
            new_names: Vec::new(),
        };
        for member in members {
            let current =
                member.joined_at < until && member.left_at.is_none_or(|left| left >= from);
            if !current || !seen.insert(member.user_id.clone()) {
                continue;
            }
            team.volunteers += 1;
            match member.status {
                S::Trial => team.trial += 1,
                S::Active => team.active += 1,
                S::Away => team.away += 1,
                S::Inactive => team.inactive += 1,
                S::Left => {}
            }
            for day in db.fetch_team_activity(&member.user_id, week, last).await? {
                for count in day.counts.iter().filter(|count| count.server == role.server) {
                    team.contributions = team.contributions.saturating_add(contributions(count));
                }
            }
            if member.joined_at >= from {
                if let Ok(person) = db.fetch_user(&member.user_id).await {
                    let shown = person.display_name.clone().unwrap_or(person.username.clone());
                    team.new_names.push(team_texts::plain(&shown));
                }
            }
        }
        team.new_names.sort();
        teams.push(team);
    }
    Ok(Json(WeekReport { week, teams }))
}
