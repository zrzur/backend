//! Discover review desk: open Discover requests are shown in one staff
//! channel by one bot, and people holding one role in that server approve
//! or reject them there (`[features.discover_review]`).
//!
//! The bot has no power of its own. It can read the open queue, and it can
//! hand back an interaction: the record this server made when a person
//! pressed a button or sent a form, with the one-time token only the bot
//! received. Everything else is decided here:
//!
//! - who pressed: the person stored on the interaction, read again from the
//!   database (still a member of the review server, holds the role now);
//! - what they pressed: the `custom_id` stored on the interaction
//!   (`dr:<action>:<kind>:<id>:<submitted_at>`), never a value the bot sends;
//! - the reason of a rejection: the form field stored on the interaction.
//!
//! So a stolen bot token alone can read the queue and decide nothing: every
//! action needs a press by a person who holds the role right now. What it
//! can still do is mislabel a button in the staff channel, because the bot
//! writes its own notices; the audit record shows what was really done and
//! by whom. A decision is written to the platform audit log first, under the
//! person's name, and applies only while the request is still open and still
//! the same filing. A visiting invite is given once per press, and only for
//! a server that is open to everyone.
//! `featured` is not reachable here: it stays in the admin console. Taking
//! a listed server or bot off Discover is, for holders of a second role
//! (`remove_role`), from a form that needs a written reason; with no such
//! role named nobody can.
//!
//! Reading is asked for the same way (`POST /ask`): the bot hands back the
//! interaction of a command or a press, and the answer (what is listed, the
//! facts of any server or bot, the record of what was done to one, the
//! counts) is given only while the person behind it holds one of the two
//! roles. The bot alone can read the queue and, for its log channel, the
//! desk's own audit records in order (`GET /log`): who did what to which
//! listing, and the reason they wrote.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::{config, FeaturesDiscoverReview};
use revolt_database::{
    record_platform_audit, staff_now, AuditOutcome, AuditRecord, Channel, Database,
    DiscoverRequest, DiscoverRequestStatus, DiscoverRequestType, Interaction, Invite,
    PlatformAuditEntry, PlatformAuditQuery, User,
};
use revolt_models::v0::{self, InteractionType};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, Route, State};
use serde::{Deserialize, Serialize};

use super::discover::{decide::tell_owner, listing::set_discoverable};

#[cfg(test)]
mod tests;

pub fn routes() -> Vec<Route> {
    routes![queue, request_state, find, act, ask, log]
}

/// Shortest reason of a rejection, as in the admin console
const MIN_REASON: usize = 3;
const MAX_REASON: usize = 512;
/// How long a reviewer's visiting invite works
const VISIT_MINUTES: i64 = 30;
/// Listings on one page of an answer
const LISTED_PAGE: usize = 5;
/// Records in one answer about one server or bot
const HISTORY_RECORDS: usize = 15;
/// Records the log gives the bot in one call
const LOG_RECORDS: usize = 20;
/// How far back the counts per reviewer go
const STATS_DAYS: i64 = 30;
/// Every record the desk writes starts with this
const DESK_ACTIONS: &str = "discover.review.";

// ---- who may call ----

#[cfg(test)]
pub(crate) mod test_settings {
    //! Tests run side by side, each with its own bot: the desk settings of
    //! a test are looked up by the calling bot instead of the one config.
    use revolt_config::FeaturesDiscoverReview;
    use std::{collections::HashMap, sync::Mutex};

    static DESKS: Mutex<Option<HashMap<String, FeaturesDiscoverReview>>> = Mutex::new(None);

    pub fn register(settings: FeaturesDiscoverReview) {
        DESKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(settings.bot.clone(), settings);
    }

    pub fn get(bot: &str) -> Option<FeaturesDiscoverReview> {
        DESKS.lock().unwrap().as_ref().and_then(|desks| desks.get(bot).cloned())
    }
}

/// The desk settings, if the desk is switched on and `caller` is its bot.
/// Anyone else gets "not found": the routes do not exist for them.
async fn desk(caller: &User) -> Result<FeaturesDiscoverReview> {
    if caller.bot.is_none() {
        return Err(create_error!(NotFound));
    }
    #[cfg(test)]
    if let Some(settings) = test_settings::get(&caller.id) {
        return Ok(settings);
    }
    let settings = config().await.features.discover_review;
    if settings.enabled() && settings.bot == caller.id {
        Ok(settings)
    } else {
        Err(create_error!(NotFound))
    }
}

// ---- what the bot sees ----

/// An open request with what its notice shows
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReviewEntry {
    /// `server` or `bot`
    pub kind: String,
    pub id: String,
    /// `<kind>:<id>:<submitted_at>`: names one filing; the buttons carry it
    pub key: String,
    /// `Pending` or `UnderReview`
    pub status: String,
    /// Milliseconds since the epoch; 0 for requests filed before it was recorded
    pub submitted_at: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
    pub owner_id: String,
    /// `name#1234`
    pub owner_name: String,
    /// The name the owner shows, when they set one
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub owner_display_name: Option<String>,
    /// Members (servers) or servers using it (bots)
    pub count: i64,
    /// When the server or bot was created, milliseconds since the epoch
    pub created_at: i64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub language: Option<String>,
    /// Servers: the public link name, if it has one
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link: Option<String>,
    /// Its picture on the file server, as `<bucket>/<file id>` (a server's
    /// icon, a bot's avatar)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub icon: Option<String>,
    /// Its banner on the file server, as `<bucket>/<file id>` (a server's
    /// banner, a bot's profile background)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub banner: Option<String>,
    /// Marks it carries: `Official`, `Verified`
    #[serde(default)]
    pub marks: Vec<String>,
    /// Servers: `Open`, `Approval` or `Locked`. Bots: `Public` or `Private`.
    pub access: String,
    /// Whether it can be listed as it stands (an open server, a public bot)
    pub listable: bool,
    pub mature: bool,
    /// The owner is a Feather member: reviewed first
    pub priority: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_at: Option<i64>,
    /// The reason written with a rejection or a removal; only in an answer
    /// to a person who asked about this one server or bot
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<String>,
}

/// Where a request stands, for a notice whose request left the queue
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReviewState {
    /// `Pending`, `UnderReview`, `Approved`, `Denied`, `Removed` or
    /// `Withdrawn` (the owner took it back, or the item is gone)
    pub status: String,
    pub submitted_at: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_at: Option<i64>,
}

fn kind_word(kind: &DiscoverRequestType) -> &'static str {
    match kind {
        DiscoverRequestType::Server => "server",
        DiscoverRequestType::Bot => "bot",
    }
}

fn kind_of(word: &str) -> Option<DiscoverRequestType> {
    match word {
        "server" => Some(DiscoverRequestType::Server),
        "bot" => Some(DiscoverRequestType::Bot),
        _ => None,
    }
}

fn status_word(status: &DiscoverRequestStatus) -> &'static str {
    match status {
        DiscoverRequestStatus::Pending => "Pending",
        DiscoverRequestStatus::UnderReview => "UnderReview",
        DiscoverRequestStatus::Approved(_) => "Approved",
        DiscoverRequestStatus::Denied(_) => "Denied",
        DiscoverRequestStatus::Removed(_) => "Removed",
    }
}

fn is_open(status: &DiscoverRequestStatus) -> bool {
    matches!(
        status,
        DiscoverRequestStatus::Pending | DiscoverRequestStatus::UnderReview
    )
}

fn created_at(id: &str) -> i64 {
    ulid::Ulid::from_string(id)
        .map(|ulid| ulid.timestamp_ms() as i64)
        .unwrap_or_default()
}

async fn name_of(db: &Database, user_id: &str) -> String {
    match db.fetch_user(user_id).await {
        Ok(user) => format!("{}#{}", user.username, user.discriminator),
        Err(_) => user_id.to_string(),
    }
}

fn key_of(request: &DiscoverRequest) -> String {
    format!(
        "{}:{}:{}",
        kind_word(&request.request_type),
        request.request_id,
        request.submitted_at.unwrap_or_default()
    )
}

/// The notice of one open request; `None` when its server or bot is gone
async fn entry(db: &Database, request: &DiscoverRequest) -> Option<ReviewEntry> {
    let path = |file: &Option<revolt_database::File>| {
        file.as_ref().map(|file| format!("{}/{}", file.tag, file.id))
    };
    let mark = |flags: Option<i32>, official: i32, verified: i32| {
        let flags = flags.unwrap_or_default();
        let mut marks = Vec::new();
        if flags & official != 0 {
            marks.push("Official".to_string());
        }
        if flags & verified != 0 {
            marks.push("Verified".to_string());
        }
        marks
    };
    let (name, description, owner_id, count, link, access, listable, mature, icon, banner, marks) =
        match request.request_type {
            DiscoverRequestType::Server => {
                let server = db.fetch_server(&request.request_id).await.ok()?;
                let access = server.access.clone().unwrap_or_default();
                (
                    server.name.clone(),
                    server.description.clone(),
                    server.owner.clone(),
                    server.get_approximate_member_count(db).await as i64,
                    server.vanity_invite.as_ref().map(|vanity| vanity.code.clone()),
                    format!("{access:?}"),
                    access == v0::ServerAccess::Open,
                    server.nsfw,
                    path(&server.icon),
                    path(&server.banner),
                    // server flags: 1 official, 2 verified
                    mark(server.flags, 1, 2),
                )
            }
            DiscoverRequestType::Bot => {
                let bot = db.fetch_bot(&request.request_id).await.ok()?;
                let bot_user = db.fetch_user(&request.request_id).await.ok()?;
                (
                    bot_user.username.clone(),
                    bot_user
                        .profile
                        .as_ref()
                        .and_then(|profile| profile.public_content())
                        .map(str::to_string),
                    bot.owner.clone(),
                    db.fetch_server_count(&bot.id).await.unwrap_or(0) as i64,
                    None,
                    if bot.public { "Public" } else { "Private" }.to_string(),
                    bot.public,
                    false,
                    path(&bot_user.avatar),
                    path(
                        &bot_user
                            .profile
                            .as_ref()
                            .and_then(|profile| profile.background.clone()),
                    ),
                    // the Official flag is the one source of the official mark
                    mark(
                        bot.flags,
                        v0::BotFlags::Official as i32,
                        v0::BotFlags::Verified as i32,
                    ),
                )
            }
        };

    let (owner_name, owner_display_name, priority) = match db.fetch_user(&owner_id).await {
        Ok(owner) => (
            format!("{}#{}", owner.username, owner.discriminator),
            owner.display_name.clone(),
            owner.has_plus().await,
        ),
        Err(_) => (owner_id.clone(), None, false),
    };
    let acted_by_name = match &request.acted_by {
        Some(id) => Some(name_of(db, id).await),
        None => None,
    };

    Some(ReviewEntry {
        kind: kind_word(&request.request_type).to_string(),
        id: request.request_id.clone(),
        key: key_of(request),
        status: status_word(&request.status).to_string(),
        submitted_at: request.submitted_at.unwrap_or_default(),
        name,
        description,
        owner_id,
        owner_name,
        owner_display_name,
        count,
        created_at: created_at(&request.request_id),
        tags: request.tags.clone(),
        category: request.category.clone(),
        language: request.language.clone(),
        link,
        icon,
        banner,
        marks,
        access,
        listable,
        mature,
        priority,
        acted_by: request.acted_by.clone(),
        acted_by_name,
        acted_at: request.acted_at,
        reason: None,
    })
}

/// # Review desk: open requests
///
/// Requests waiting for a decision, oldest filing first within Feather
/// members and then everyone else. Only the desk's bot.
#[get("/queue")]
async fn queue(db: &State<Database>, user: User) -> Result<Json<Vec<ReviewEntry>>> {
    desk(&user).await?;
    let mut entries = Vec::new();
    for request in db.fetch_open_discover_requests().await? {
        if let Some(entry) = entry(db, &request).await {
            entries.push(entry);
        }
    }
    entries.sort_by_key(|entry| (!entry.priority, entry.submitted_at));
    Ok(Json(entries))
}

/// # Review desk: find a request by what a person typed
///
/// `q` is a server id, a bot id or a server's link name. Gives the request
/// with what its notice shows, whatever its status (`status` is then one of
/// `Pending`, `UnderReview`, `Approved`, `Denied`, `Removed`). Only the
/// desk's bot.
#[get("/find?<q>")]
async fn find(db: &State<Database>, user: User, q: &str) -> Result<Json<ReviewEntry>> {
    desk(&user).await?;
    let q = q.trim();
    if q.is_empty() || q.len() > 64 || !q.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(create_error!(NotFound));
    }
    let mut request = None;
    for kind in [DiscoverRequestType::Server, DiscoverRequestType::Bot] {
        if let Ok(found) = db.fetch_discover_request_by_item_id(kind, q).await {
            request = Some(found);
            break;
        }
    }
    if request.is_none() {
        // a link name: the server it leads to
        if let Ok(Invite::Server { server, .. }) = db.fetch_invite(q).await {
            request = db
                .fetch_discover_request_by_item_id(DiscoverRequestType::Server, &server)
                .await
                .ok();
        }
    }
    let request = request.ok_or_else(|| create_error!(NotFound))?;
    entry(db, &request)
        .await
        .map(Json)
        .ok_or_else(|| create_error!(NotFound))
}

/// # Review desk: where one request stands
///
/// For a notice whose request is no longer in the queue. Gives the status
/// word and who acted, never the reason.
#[get("/requests/<kind>/<id>")]
async fn request_state(
    db: &State<Database>,
    user: User,
    kind: &str,
    id: &str,
) -> Result<Json<ReviewState>> {
    desk(&user).await?;
    let kind = kind_of(kind).ok_or_else(|| create_error!(InvalidOperation))?;
    let Ok(request) = db.fetch_discover_request_by_item_id(kind, id).await else {
        return Ok(Json(ReviewState {
            status: "Withdrawn".to_string(),
            submitted_at: 0,
            acted_by: None,
            acted_by_name: None,
            acted_at: None,
        }));
    };
    let acted_by_name = match &request.acted_by {
        Some(id) => Some(name_of(db, id).await),
        None => None,
    };
    Ok(Json(ReviewState {
        status: status_word(&request.status).to_string(),
        submitted_at: request.submitted_at.unwrap_or_default(),
        acted_by: request.acted_by,
        acted_by_name,
        acted_at: request.acted_at,
    }))
}

// ---- acting for the person who pressed ----

/// The interaction the bot hands back: nothing else is taken from the bot
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DataReviewAct {
    pub interaction_id: String,
    pub token: String,
}

/// What came of it
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReviewOutcome {
    /// Whether the act was carried out
    pub applied: bool,
    /// Why not, when `applied` is false: `already_decided`, `withdrawn`,
    /// `own_item`, `not_listable`, `no_channel`, `not_listed`, `already_member`
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub problem: Option<String>,
    /// `review`, `approve`, `deny`, `visit` or `remove`
    pub action: String,
    pub kind: String,
    pub id: String,
    /// Name of the server or bot (empty when it is gone)
    pub name: String,
    /// Status after this call
    pub status: String,
    /// The person who pressed
    pub reviewer: String,
    pub reviewer_name: String,
    /// Who last acted on the request (the reviewer when `applied`)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_at: Option<i64>,
    /// `deny`: the reason as stored
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<String>,
    /// `visit`: code of a one-use invite for the reviewer
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub invite: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Review,
    Approve,
    Deny,
    Visit,
    Remove,
}

impl Action {
    fn word(self) -> &'static str {
        match self {
            Action::Review => "review",
            Action::Approve => "approve",
            Action::Deny => "deny",
            Action::Visit => "visit",
            Action::Remove => "remove",
        }
    }
}

/// What the person pressed, read from the stored `custom_id`
struct Pressed {
    action: Action,
    kind: DiscoverRequestType,
    id: String,
    /// The filing the notice was about
    submitted_at: i64,
}

fn parse_pressed(custom_id: &str) -> Option<Pressed> {
    let mut parts = custom_id.split(':');
    if parts.next()? != "dr" {
        return None;
    }
    let action = match parts.next()? {
        "review" => Action::Review,
        "approve" => Action::Approve,
        "deny" => Action::Deny,
        "visit" => Action::Visit,
        "remove" => Action::Remove,
        _ => return None,
    };
    let kind = kind_of(parts.next()?)?;
    let id = parts.next()?.to_string();
    let submitted_at = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || id.is_empty() || id.len() > 64 {
        return None;
    }
    Some(Pressed {
        action,
        kind,
        id,
        submitted_at,
    })
}

fn no_review_permission() -> revolt_result::Error {
    create_error!(MissingPermission {
        permission: "DiscoverReview".to_string()
    })
}

/// The stored interaction behind `data`, if it is a genuine press in the
/// review channel that this bot received
async fn genuine_press(
    db: &Database,
    settings: &FeaturesDiscoverReview,
    data: &DataReviewAct,
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
    // in the review server; which channel a decision belongs to is checked
    // once the pressed button is read
    if interaction.server_id.as_deref() != Some(settings.server.as_str()) {
        return Err(no_review_permission());
    }
    Ok(interaction)
}

/// The person who pressed, if they may review right now
async fn reviewer(
    db: &Database,
    settings: &FeaturesDiscoverReview,
    interaction: &Interaction,
    role: &str,
) -> Result<User> {
    // no role named for this act: nobody may
    if role.trim().is_empty() {
        return Err(no_review_permission());
    }
    let person = db
        .fetch_user(&interaction.user_id)
        .await
        .map_err(|_| no_review_permission())?;
    let barred = v0::UserFlags::SuspendedUntil as i32
        | v0::UserFlags::Deleted as i32
        | v0::UserFlags::Banned as i32
        | v0::UserFlags::Spam as i32;
    if person.bot.is_some() || person.flags.unwrap_or_default() & barred != 0 {
        return Err(no_review_permission());
    }
    // read now, not taken from the click: the role may be gone since
    let member = db
        .fetch_member(&settings.server, &person.id)
        .await
        .map_err(|_| no_review_permission())?;
    if !member.roles.iter().any(|held| held == role) {
        return Err(no_review_permission());
    }
    Ok(person)
}

/// Name and owner of the item, and whether it can be listed as it stands
async fn item(
    db: &Database,
    kind: &DiscoverRequestType,
    id: &str,
) -> Option<(String, String, bool)> {
    match kind {
        DiscoverRequestType::Server => {
            let server = db.fetch_server(id).await.ok()?;
            let listable = server.access.clone().unwrap_or_default() == v0::ServerAccess::Open;
            Some((server.name, server.owner, listable))
        }
        DiscoverRequestType::Bot => {
            let bot = db.fetch_bot(id).await.ok()?;
            let name = db.fetch_user(id).await.map(|u| u.username).unwrap_or_default();
            Some((name, bot.owner, bot.public))
        }
    }
}

/// # Review desk: act for the person who pressed
///
/// The bot hands back the interaction of a button press or a submitted
/// rejection form. What is done, to which request and with which reason is
/// read from that stored interaction. Only `review`, `approve`, `deny` and
/// `visit` exist here.
#[post("/act", data = "<data>")]
async fn act(
    db: &State<Database>,
    user: User,
    data: Json<DataReviewAct>,
) -> Result<Json<ReviewOutcome>> {
    let settings = desk(&user).await?;
    let data = data.into_inner();
    let interaction = genuine_press(db, &settings, &data).await?;
    let pressed = interaction
        .custom_id
        .as_deref()
        .and_then(parse_pressed)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    // a bot is decided in the bots' channel, a server in the servers' one;
    // a removal may be asked for anywhere in the review server
    if pressed.action != Action::Remove
        && interaction.channel_id != settings.channel_for(pressed.kind == DiscoverRequestType::Bot)
    {
        return Err(no_review_permission());
    }

    // A rejection comes from the form (it carries the reason); everything
    // else from a button
    let reason = match (pressed.action, interaction.kind) {
        (Action::Deny | Action::Remove, InteractionType::ModalSubmit) => {
            let reason = interaction
                .modal_fields
                .iter()
                .find(|field| field.custom_id == "reason")
                .and_then(|field| field.value.as_deref())
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let length = reason.chars().count();
            if !(MIN_REASON..=MAX_REASON).contains(&length) {
                return Err(revolt_result::failed_validation(vec![
                    revolt_result::ValidationProblem::length("reason", MIN_REASON, MAX_REASON),
                ]));
            }
            Some(reason)
        }
        (Action::Deny | Action::Remove, _) | (_, InteractionType::ModalSubmit) => {
            return Err(create_error!(InvalidOperation))
        }
        (_, InteractionType::Component) => None,
        _ => return Err(create_error!(InvalidOperation)),
    };

    let role = if pressed.action == Action::Remove {
        settings.remove_role.clone()
    } else {
        settings.role.clone()
    };
    let person = reviewer(db, &settings, &interaction, &role).await?;
    let reviewer_name = format!("{}#{}", person.username, person.discriminator);

    let kind_text = kind_word(&pressed.kind).to_string();
    let found = item(db, &pressed.kind, &pressed.id).await;
    let request = db
        .fetch_discover_request_by_item_id(pressed.kind.clone(), &pressed.id)
        .await
        .ok();

    let mut outcome = ReviewOutcome {
        applied: false,
        problem: None,
        action: pressed.action.word().to_string(),
        kind: kind_text.clone(),
        id: pressed.id.clone(),
        name: found.as_ref().map(|(name, _, _)| name.clone()).unwrap_or_default(),
        status: request
            .as_ref()
            .map(|r| status_word(&r.status))
            .unwrap_or("Withdrawn")
            .to_string(),
        reviewer: person.id.clone(),
        reviewer_name,
        acted_by_name: None,
        acted_at: request.as_ref().and_then(|r| r.acted_at),
        reason: None,
        invite: None,
    };
    if let Some(id) = request.as_ref().and_then(|r| r.acted_by.as_ref()) {
        outcome.acted_by_name = Some(name_of(db, id).await);
    }
    let refuse = |mut outcome: ReviewOutcome, problem: &str| {
        outcome.problem = Some(problem.to_string());
        Ok(Json(outcome))
    };

    // The request must be there, be the filing the notice was about, and
    // still be open
    let (Some(request), Some((_, owner, listable))) = (request, found) else {
        outcome.status = "Withdrawn".to_string();
        return refuse(outcome, "withdrawn");
    };
    if pressed.action == Action::Remove {
        // taking a listed item off: it must be listed, and be the filing the
        // person was shown
        if request.submitted_at.unwrap_or_default() != pressed.submitted_at
            || !matches!(request.status, DiscoverRequestStatus::Approved(_))
        {
            return refuse(outcome, "not_listed");
        }
        let status = DiscoverRequestStatus::Removed(reason.clone());
        let mut record = AuditRecord {
            actor_id: person.id.clone(),
            action: "discover.review.remove".to_string(),
            target_kind: Some(kind_text),
            target_id: Some(pressed.id.clone()),
            reason: reason.as_ref().map(|r| r.chars().take(MAX_REASON).collect()),
            summary: Some(format!(
                "via bot {} interaction {}",
                settings.bot, interaction.id
            )),
            ..Default::default()
        };
        // the record first: no record, no removal
        record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
        let now = staff_now();
        let won = db
            .remove_listed_discover_request(
                pressed.kind.clone(),
                &pressed.id,
                request.submitted_at,
                status.clone(),
                &person.id,
                now,
            )
            .await?;
        if !won {
            record.summary = Some("lost the race: no longer listed".to_string());
            let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
            return refuse(outcome, "not_listed");
        }
        if let Err(error) = set_discoverable(db, pressed.kind.clone(), &pressed.id, false).await {
            // the request already says removed; the listing flag did not follow
            record.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
            let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
            return Err(error);
        }
        tell_owner(db, &pressed.kind, &pressed.id, &status).await;
        outcome.applied = true;
        outcome.status = status_word(&status).to_string();
        outcome.acted_by_name = Some(outcome.reviewer_name.clone());
        outcome.acted_at = Some(now);
        outcome.reason = reason;
        return Ok(Json(outcome));
    }

    if request.submitted_at.unwrap_or_default() != pressed.submitted_at {
        // filed anew since: this notice is about an older filing
        return refuse(outcome, "already_decided");
    }
    if !is_open(&request.status) {
        return refuse(outcome, "already_decided");
    }
    if owner == person.id {
        return refuse(outcome, "own_item");
    }

    let mut record = AuditRecord {
        actor_id: person.id.clone(),
        action: format!("discover.review.{}", pressed.action.word()),
        target_kind: Some(kind_text),
        target_id: Some(pressed.id.clone()),
        reason: reason.as_ref().map(|r| r.chars().take(MAX_REASON).collect()),
        summary: Some(format!(
            "via bot {} interaction {}",
            settings.bot, interaction.id
        )),
        ..Default::default()
    };

    if pressed.action == Action::Visit {
        if pressed.kind != DiscoverRequestType::Server {
            return Err(create_error!(InvalidOperation));
        }
        // a server that asks before letting people in is not walked into
        if !listable {
            return refuse(outcome, "not_listable");
        }
        let server = db.fetch_server(&pressed.id).await?;
        // someone already inside needs no invite, and joining again would fail
        if db.fetch_member(&server.id, &person.id).await.is_ok() {
            return refuse(outcome, "already_member");
        }
        let mut channel = None;
        for id in &server.channels {
            if let Ok(found @ Channel::TextChannel { .. }) = db.fetch_channel(id).await {
                channel = Some(found);
                break;
            }
        }
        let Some(channel) = channel else {
            return refuse(outcome, "no_channel");
        };
        // one press, one invite: handing the same press back gives nothing
        if !db.claim_interaction_use(&interaction.id).await? {
            return Err(create_error!(InteractionExpired));
        }
        record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
        let expires = Timestamp::now_utc().checked_add(Duration::minutes(VISIT_MINUTES));
        let invite =
            match Invite::create_channel_invite(db, &person, &channel, Some(1), expires).await {
                Ok(invite) => invite,
                Err(error) => {
                    record.summary =
                        Some(format!("{:?}", error.error_type).chars().take(200).collect());
                    let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
                    return Err(error);
                }
            };
        outcome.applied = true;
        outcome.invite = Some(invite.code().to_string());
        return Ok(Json(outcome));
    }

    if pressed.action == Action::Approve && !listable {
        return refuse(outcome, "not_listable");
    }

    let status = match pressed.action {
        Action::Review => DiscoverRequestStatus::UnderReview,
        Action::Approve => DiscoverRequestStatus::Approved(None),
        Action::Deny => DiscoverRequestStatus::Denied(reason.clone()),
        Action::Visit | Action::Remove => unreachable!(),
    };

    // the record first: no record, no decision
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    let now = staff_now();
    let won = db
        .act_on_open_discover_request(
            pressed.kind.clone(),
            &pressed.id,
            request.submitted_at,
            status.clone(),
            &person.id,
            now,
        )
        .await?;
    if !won {
        // someone decided in the moment between
        record.summary = Some("lost the race: already decided".to_string());
        let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
        if let Ok(current) = db
            .fetch_discover_request_by_item_id(pressed.kind.clone(), &pressed.id)
            .await
        {
            outcome.status = status_word(&current.status).to_string();
            outcome.acted_at = current.acted_at;
            outcome.acted_by_name = match &current.acted_by {
                Some(id) => Some(name_of(db, id).await),
                None => None,
            };
        }
        return refuse(outcome, "already_decided");
    }

    let listing = match pressed.action {
        Action::Approve => set_discoverable(db, pressed.kind.clone(), &pressed.id, true).await,
        Action::Deny => set_discoverable(db, pressed.kind.clone(), &pressed.id, false).await,
        _ => Ok(()),
    };
    if let Err(error) = listing {
        // put the request back as it was: nothing was listed. Only while it
        // still carries this decision, so a decision made since is kept
        let ours = db
            .fetch_discover_request_by_item_id(pressed.kind.clone(), &pressed.id)
            .await
            .map(|current| {
                current.acted_at == Some(now) && current.acted_by.as_deref() == Some(person.id.as_str())
            })
            .unwrap_or(false);
        if ours {
            let _ = db.save_discover_request(&request).await;
        }
        record.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
        return Err(error);
    }

    if pressed.action != Action::Review {
        tell_owner(db, &pressed.kind, &pressed.id, &status).await;
    }

    outcome.applied = true;
    outcome.status = status_word(&status).to_string();
    outcome.acted_by_name = Some(outcome.reviewer_name.clone());
    outcome.acted_at = Some(now);
    outcome.reason = reason;
    Ok(Json(outcome))
}

// ---- reading for the person who asked ----

/// The interaction of a command or a press, and what the person wants to read
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DataReviewAsk {
    pub interaction_id: String,
    pub token: String,
    /// `queue`, `listed`, `lookup` or `stats`
    pub what: String,
    /// `lookup`: a server id, a bot id, a link name or an invite code
    #[serde(default)]
    pub q: Option<String>,
    /// `listed`: only `server` or only `bot`
    #[serde(default)]
    pub kind: Option<String>,
    /// `listed`: which page, from 0
    #[serde(default)]
    pub page: Option<usize>,
}

/// One thing the desk did, as its audit record tells it
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReviewRecord {
    pub seq: i64,
    /// Milliseconds since the epoch
    pub at: i64,
    /// `review`, `approve`, `deny`, `visit` or `remove`
    pub action: String,
    pub actor_id: String,
    pub actor_name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub id: Option<String>,
    /// Name of the server or bot (empty when it is gone)
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<String>,
}

/// Decisions of one reviewer in the counted days
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ReviewCount {
    pub name: String,
    pub approved: usize,
    pub denied: usize,
    pub removed: usize,
}

/// The answer to `ask`; only the parts that belong to `what` are filled
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ReviewAnswer {
    pub what: String,
    /// Whether the person who asked may take listings off Discover
    pub may_remove: bool,
    /// `queue`, `listed`
    #[serde(default)]
    pub entries: Vec<ReviewEntry>,
    /// `queue`, `listed`: how many there are in all
    pub total: usize,
    pub page: usize,
    pub pages: usize,
    /// `lookup`
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub entry: Option<ReviewEntry>,
    /// `lookup`: what the desk did to it, newest first
    #[serde(default)]
    pub records: Vec<ReviewRecord>,
    /// `stats`
    pub pending: usize,
    pub under_review: usize,
    pub listed_servers: usize,
    pub listed_bots: usize,
    pub days: i64,
    #[serde(default)]
    pub counts: Vec<ReviewCount>,
}

async fn record_of(db: &Database, entry: PlatformAuditEntry) -> ReviewRecord {
    let kind = entry.target_kind.as_deref().and_then(kind_of);
    let name = match (&kind, &entry.target_id) {
        (Some(kind), Some(id)) => item(db, kind, id)
            .await
            .map(|found| found.0)
            .unwrap_or_default(),
        _ => String::new(),
    };
    ReviewRecord {
        seq: entry.seq,
        at: entry.created_at,
        action: entry
            .action
            .strip_prefix(DESK_ACTIONS)
            .unwrap_or(&entry.action)
            .to_string(),
        actor_name: name_of(db, &entry.actor_id).await,
        actor_id: entry.actor_id,
        kind: entry.target_kind,
        id: entry.target_id,
        name,
        reason: entry.reason,
    }
}

fn typed(q: &str) -> Option<&str> {
    let q = q.trim();
    (!q.is_empty()
        && q.len() <= 64
        && q.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then_some(q)
}

/// The server or bot a person typed: an id, a link name or an invite code
async fn typed_item(db: &Database, q: &str) -> Option<(DiscoverRequestType, String)> {
    if db.fetch_server(q).await.is_ok() {
        return Some((DiscoverRequestType::Server, q.to_string()));
    }
    if db.fetch_bot(q).await.is_ok() {
        return Some((DiscoverRequestType::Bot, q.to_string()));
    }
    match db.fetch_invite(q).await {
        Ok(Invite::Server { server, .. }) => Some((DiscoverRequestType::Server, server)),
        _ => None,
    }
}

/// What the desk recorded about one server or bot, newest first
async fn records_about(db: &Database, id: &str) -> Result<Vec<ReviewRecord>> {
    let mut records = Vec::new();
    for entry in db
        .fetch_platform_audit(PlatformAuditQuery {
            target_id: Some(id.to_string()),
            action_prefix: Some(DESK_ACTIONS.to_string()),
            limit: HISTORY_RECORDS * 2,
            ..Default::default()
        })
        .await?
    {
        if entry.outcome == AuditOutcome::Success && records.len() < HISTORY_RECORDS {
            records.push(record_of(db, entry).await);
        }
    }
    Ok(records)
}

/// # Review desk: read for the person who asked
///
/// The bot hands back the interaction of a command or of a press. The
/// answer is given only while the person behind it is in the review server
/// and holds the reviewer role or the removal role.
#[post("/ask", data = "<data>")]
async fn ask(
    db: &State<Database>,
    user: User,
    data: Json<DataReviewAsk>,
) -> Result<Json<ReviewAnswer>> {
    let settings = desk(&user).await?;
    let data = data.into_inner();
    let interaction = genuine_press(
        db,
        &settings,
        &DataReviewAct {
            interaction_id: data.interaction_id.clone(),
            token: data.token.clone(),
        },
    )
    .await?;
    if !matches!(
        interaction.kind,
        InteractionType::Command | InteractionType::Component
    ) {
        return Err(create_error!(InvalidOperation));
    }
    let may_remove = reviewer(db, &settings, &interaction, &settings.remove_role)
        .await
        .is_ok();
    if !may_remove {
        reviewer(db, &settings, &interaction, &settings.role).await?;
    }

    let mut answer = ReviewAnswer {
        what: data.what.clone(),
        may_remove,
        ..Default::default()
    };
    match data.what.as_str() {
        "queue" => {
            for request in db.fetch_open_discover_requests().await? {
                if let Some(entry) = entry(db, &request).await {
                    answer.entries.push(entry);
                }
            }
            answer
                .entries
                .sort_by_key(|entry| (!entry.priority, entry.submitted_at));
            answer.total = answer.entries.len();
        }
        "listed" => {
            let only = data.kind.as_deref().and_then(kind_of);
            let mut listed: Vec<DiscoverRequest> = db
                .fetch_listed_discover_requests()
                .await?
                .into_iter()
                .filter(|request| only.as_ref().is_none_or(|kind| &request.request_type == kind))
                .collect();
            // newest decision first; the id settles what has no time
            listed.sort_by(|a, b| {
                (b.acted_at.or(b.submitted_at).unwrap_or_default(), &b.request_id).cmp(&(
                    a.acted_at.or(a.submitted_at).unwrap_or_default(),
                    &a.request_id,
                ))
            });
            answer.total = listed.len();
            answer.pages = listed.len().div_ceil(LISTED_PAGE).max(1);
            answer.page = data.page.unwrap_or_default().min(answer.pages - 1);
            for request in listed
                .iter()
                .skip(answer.page * LISTED_PAGE)
                .take(LISTED_PAGE)
            {
                if let Some(entry) = entry(db, request).await {
                    answer.entries.push(entry);
                }
            }
        }
        "lookup" => {
            let q = data
                .q
                .as_deref()
                .and_then(typed)
                .ok_or_else(|| create_error!(NotFound))?;
            let (kind, id) = typed_item(db, q)
                .await
                .ok_or_else(|| create_error!(NotFound))?;
            let request = db
                .fetch_discover_request_by_item_id(kind.clone(), &id)
                .await
                .ok();
            // something that never asked to be listed is shown the same way
            let never = request.is_none();
            let request = request.unwrap_or(DiscoverRequest {
                request_type: kind,
                request_id: id.clone(),
                status: DiscoverRequestStatus::Pending,
                tags: vec![],
                category: None,
                language: None,
                featured: false,
                submitted_at: None,
                acted_by: None,
                acted_at: None,
            });
            let mut found = entry(db, &request)
                .await
                .ok_or_else(|| create_error!(NotFound))?;
            if never {
                found.status = "None".to_string();
            }
            found.reason = match &request.status {
                DiscoverRequestStatus::Denied(reason) | DiscoverRequestStatus::Removed(reason) => {
                    reason.clone()
                }
                _ => None,
            };
            answer.entry = Some(found);
            answer.records = records_about(db, &id).await?;
        }
        "stats" => {
            for request in db.fetch_open_discover_requests().await? {
                match request.status {
                    DiscoverRequestStatus::UnderReview => answer.under_review += 1,
                    _ => answer.pending += 1,
                }
            }
            for request in db.fetch_listed_discover_requests().await? {
                match request.request_type {
                    DiscoverRequestType::Server => answer.listed_servers += 1,
                    DiscoverRequestType::Bot => answer.listed_bots += 1,
                }
            }
            answer.days = STATS_DAYS;
            let since = staff_now() - STATS_DAYS * 24 * 60 * 60 * 1000;
            let mut counts: Vec<(String, ReviewCount)> = Vec::new();
            for entry in db
                .fetch_platform_audit(PlatformAuditQuery {
                    action_prefix: Some(DESK_ACTIONS.to_string()),
                    limit: 1000,
                    ..Default::default()
                })
                .await?
            {
                if entry.created_at < since {
                    break;
                }
                if entry.outcome != AuditOutcome::Success {
                    continue;
                }
                let at = match counts.iter().position(|(id, _)| id == &entry.actor_id) {
                    Some(at) => at,
                    None => {
                        counts.push((
                            entry.actor_id.clone(),
                            ReviewCount {
                                name: name_of(db, &entry.actor_id).await,
                                ..Default::default()
                            },
                        ));
                        counts.len() - 1
                    }
                };
                match entry.action.strip_prefix(DESK_ACTIONS) {
                    Some("approve") => counts[at].1.approved += 1,
                    Some("deny") => counts[at].1.denied += 1,
                    Some("remove") => counts[at].1.removed += 1,
                    _ => {}
                }
            }
            answer.counts = counts
                .into_iter()
                .map(|(_, count)| count)
                .filter(|count| count.approved + count.denied + count.removed > 0)
                .collect();
            answer.counts.sort_by_key(|count| {
                std::cmp::Reverse(count.approved + count.denied + count.removed)
            });
        }
        _ => return Err(create_error!(InvalidOperation)),
    }
    Ok(Json(answer))
}

/// What the desk did since `after`, oldest first
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReviewLog {
    pub records: Vec<ReviewRecord>,
    /// Ask again with this as `after`
    pub upto: i64,
}

/// # Review desk: its records in order
///
/// For the bot's log channel: the desk's own audit records after `after`,
/// oldest first. Without `after` it gives no records, only where the log
/// stands now. Only the desk's bot.
#[get("/log?<after>")]
async fn log(db: &State<Database>, user: User, after: Option<i64>) -> Result<Json<ReviewLog>> {
    desk(&user).await?;
    let newest = db
        .fetch_platform_audit(PlatformAuditQuery {
            limit: 1,
            ..Default::default()
        })
        .await?
        .first()
        .map(|entry| entry.seq)
        .unwrap_or_default();
    let Some(after) = after.filter(|after| *after >= 0 && *after < newest) else {
        return Ok(Json(ReviewLog {
            records: vec![],
            upto: newest,
        }));
    };
    let mut records = Vec::new();
    let mut upto = after;
    // the audit log holds everything staff do: read on in steps until the
    // desk's own records fill one answer, or a fair number were passed over
    for _ in 0..10 {
        let batch = db.fetch_platform_audit_range(upto + 1, 200).await?;
        if batch.is_empty() {
            break;
        }
        for entry in batch {
            if records.len() == LOG_RECORDS {
                return Ok(Json(ReviewLog { records, upto }));
            }
            upto = entry.seq;
            if entry.action.starts_with(DESK_ACTIONS) && entry.outcome == AuditOutcome::Success {
                records.push(record_of(db, entry).await);
            }
        }
        if upto >= newest {
            break;
        }
    }
    Ok(Json(ReviewLog { records, upto }))
}
