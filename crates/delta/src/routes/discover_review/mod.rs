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
//! So a stolen bot token can read the queue and nothing more. A decision is
//! written to the platform audit log first, under the person's name, and
//! applies only while the request is still open and still the same filing.
//! `Remove` and `featured` are not reachable here: they stay in the admin
//! console.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::{config, FeaturesDiscoverReview};
use revolt_database::{
    record_platform_audit, staff_now, AuditOutcome, AuditRecord, Channel, Database,
    DiscoverRequest, DiscoverRequestStatus, DiscoverRequestType, Interaction, Invite, User,
};
use revolt_models::v0::{self, InteractionType};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, Route, State};
use serde::{Deserialize, Serialize};

use super::discover::{decide::tell_owner, listing::set_discoverable};

#[cfg(test)]
mod tests;

pub fn routes() -> Vec<Route> {
    routes![queue, request_state, act]
}

/// Shortest reason of a rejection, as in the admin console
const MIN_REASON: usize = 3;
const MAX_REASON: usize = 512;
/// How long a reviewer's visiting invite works
const VISIT_MINUTES: i64 = 30;

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
    let (name, description, owner_id, count, link, access, listable, mature) =
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
                )
            }
        };

    let (owner_name, priority) = match db.fetch_user(&owner_id).await {
        Ok(owner) => (
            format!("{}#{}", owner.username, owner.discriminator),
            owner.has_plus().await,
        ),
        Err(_) => (owner_id.clone(), false),
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
        count,
        created_at: created_at(&request.request_id),
        tags: request.tags.clone(),
        category: request.category.clone(),
        language: request.language.clone(),
        link,
        access,
        listable,
        mature,
        priority,
        acted_by: request.acted_by.clone(),
        acted_by_name,
        acted_at: request.acted_at,
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
    /// `own_item`, `not_listable`, `no_channel`
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub problem: Option<String>,
    /// `review`, `approve`, `deny` or `visit`
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
}

impl Action {
    fn word(self) -> &'static str {
        match self {
            Action::Review => "review",
            Action::Approve => "approve",
            Action::Deny => "deny",
            Action::Visit => "visit",
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
    if interaction.server_id.as_deref() != Some(settings.server.as_str())
        || interaction.channel_id != settings.channel
    {
        return Err(no_review_permission());
    }
    Ok(interaction)
}

/// The person who pressed, if they may review right now
async fn reviewer(
    db: &Database,
    settings: &FeaturesDiscoverReview,
    interaction: &Interaction,
) -> Result<User> {
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
    if !member.roles.contains(&settings.role) {
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

    // A rejection comes from the form (it carries the reason); everything
    // else from a button
    let reason = match (pressed.action, interaction.kind) {
        (Action::Deny, InteractionType::ModalSubmit) => {
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
        (Action::Deny, _) | (_, InteractionType::ModalSubmit) => {
            return Err(create_error!(InvalidOperation))
        }
        (_, InteractionType::Component) => None,
        _ => return Err(create_error!(InvalidOperation)),
    };

    let person = reviewer(db, &settings, &interaction).await?;
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
        reason: reason.as_ref().map(|r| r.chars().take(500).collect()),
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
        let server = db.fetch_server(&pressed.id).await?;
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
        record_platform_audit(db, record, AuditOutcome::Success).await?;
        let expires = Timestamp::now_utc().checked_add(Duration::minutes(VISIT_MINUTES));
        let invite = Invite::create_channel_invite(db, &person, &channel, Some(1), expires).await?;
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
        Action::Visit => unreachable!(),
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
        // put the request back as it was: nothing was listed
        let _ = db.save_discover_request(&request).await;
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
