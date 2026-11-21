//! The owner's `/badge` command (`[features.badge_command]`), offered by one
//! bot (the Apply bot). The bot decides nothing: it hands back the person's
//! command interaction (id and token, nothing else), and the server checks
//!
//! - that the interaction is genuine, received by the configured bot, not
//!   expired, and a `/badge give` or `/badge remove` as the server itself
//!   checked and stored it (the bot cannot change who pressed or what they
//!   chose);
//! - that it is used once;
//! - that the person who used it is the configured owner. Anyone else is
//!   refused and the try is written to the platform audit log as Denied.
//!
//! The owner may give every hand-given badge, the privileged and retired
//! ones included (owner's decision, 2026-11-21). Badges that follow a record
//! (Plus, Creator, Beta, ...) still can't be given by hand, nobody changes
//! their own badges, and bots get none. Every change is written to the
//! platform audit log before it is made, and the person gets the same
//! official notice as for a grant in the admin console.
//!
//! Kept out of the public OpenAPI document, like the other bot desks.

use std::collections::BTreeMap;

use revolt_config::{config, FeaturesBadgeCommand};
use revolt_database::{
    badge_definition, record_platform_audit, AuditOutcome, AuditRecord, BadgeDefinition,
    BadgeGrantRule, Database, Interaction, User, BADGE_CATALOGUE,
};
use revolt_models::v0::{
    CommandOptionType, DataGrantBadge, DataRevokeBadge, InteractionDataOption,
    InteractionOptionValue, InteractionType,
};
use revolt_result::{create_error, ErrorType, Result};
use rocket::{serde::json::Json, Route, State};
use serde::{Deserialize, Serialize};

use super::{grant_checked, revoke_with, BadgeRights};

/// The command's name as the bot registers it
pub const COMMAND: &str = "badge";
/// Reason written when the owner gives none
pub const DEFAULT_REASON: &str = "Given by the owner";
const MIN_REASON: usize = 3;
const MAX_REASON: usize = 500;

pub fn routes() -> Vec<Route> {
    routes![catalogue, grant, revoke]
}

#[cfg(test)]
pub(crate) mod test_settings {
    //! Tests run side by side, each with its own bot: a test's settings are
    //! looked up by the calling bot instead of the one config.
    use revolt_config::FeaturesBadgeCommand;
    use std::{collections::HashMap, sync::Mutex};

    static SETTINGS: Mutex<Option<HashMap<String, FeaturesBadgeCommand>>> = Mutex::new(None);

    pub fn register(settings: FeaturesBadgeCommand) {
        SETTINGS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(settings.bot.clone(), settings);
    }

    pub fn get(bot: &str) -> Option<FeaturesBadgeCommand> {
        SETTINGS.lock().unwrap().as_ref().and_then(|all| all.get(bot).cloned())
    }
}

/// The settings, if the command is switched on and `caller` is its bot.
/// Anyone else gets "not found": the routes do not exist for them.
async fn settings(caller: &User) -> Result<FeaturesBadgeCommand> {
    if caller.bot.is_none() {
        return Err(create_error!(NotFound));
    }
    #[cfg(test)]
    if let Some(settings) = test_settings::get(&caller.id) {
        return Ok(settings);
    }
    let settings = config().await.features.badge_command;
    if settings.enabled() && settings.bot == caller.id {
        Ok(settings)
    } else {
        Err(create_error!(NotFound))
    }
}

/// Badges the command offers: every hand-given one, privileged and retired
/// included; never one that follows a record
fn offered(definition: &BadgeDefinition) -> bool {
    matches!(definition.rule, BadgeGrantRule::Staff | BadgeGrantRule::Privileged)
}

fn rule_word(rule: BadgeGrantRule) -> &'static str {
    match rule {
        BadgeGrantRule::Privileged => "privileged",
        _ => "staff",
    }
}

#[derive(Serialize, Debug)]
pub struct CommandBadge {
    pub id: String,
    /// `staff` or `privileged`
    pub rule: &'static str,
    pub retired: bool,
    /// Name by language (`en`, `ar`, `de`)
    pub names: BTreeMap<&'static str, String>,
}

#[derive(Serialize, Debug)]
pub struct CommandCatalogue {
    /// Always true here: when the command is off the route answers 404
    pub enabled: bool,
    pub badges: Vec<CommandBadge>,
}

/// # Badge command: what the bot may offer
///
/// The badges for the command's choices. 404 when the command is off or the
/// caller is not its bot.
#[get("/catalogue")]
async fn catalogue(user: User) -> Result<Json<CommandCatalogue>> {
    settings(&user).await?;
    let mut badges: Vec<&BadgeDefinition> = BADGE_CATALOGUE.iter().filter(|b| offered(b)).collect();
    badges.sort_by_key(|b| (b.retired, b.priority));
    Ok(Json(CommandCatalogue {
        enabled: true,
        badges: badges
            .into_iter()
            .map(|b| CommandBadge {
                id: b.id.to_string(),
                rule: rule_word(b.rule),
                retired: b.retired,
                names: crate::util::official::badge_names(b.id),
            })
            .collect(),
    }))
}

/// The interaction the bot hands back: nothing else is taken from the bot
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DataBadgeCommand {
    pub interaction_id: String,
    pub token: String,
}

#[derive(Serialize, Debug)]
pub struct CommandPerson {
    pub id: String,
    /// `username#discriminator`
    pub name: String,
}

/// What came of the command, for the bot's private answer
#[derive(Serialize, Debug)]
pub struct BadgeCommandResult {
    /// `give` or `remove`
    pub action: &'static str,
    /// `done`; `already` (give: they had it); `not_held` (remove: they did
    /// not have it); `self` (nobody changes their own badges)
    pub result: &'static str,
    pub badge: String,
    pub names: BTreeMap<&'static str, String>,
    pub user: CommandPerson,
}

/// The options of the chosen sub-command, as the server stored them
fn chosen<'a>(interaction: &'a Interaction, sub: &str) -> Option<&'a [InteractionDataOption]> {
    interaction
        .command_options
        .iter()
        .find(|o| o.kind == CommandOptionType::SubCommand && o.name == sub)
        .map(|o| o.options.as_deref().unwrap_or_default())
}

fn text(options: &[InteractionDataOption], name: &str) -> Option<String> {
    options.iter().find(|o| o.name == name).and_then(|o| match &o.value {
        Some(InteractionOptionValue::String(value)) => Some(value.trim().to_string()),
        _ => None,
    })
}

fn reason_of(options: &[InteractionDataOption]) -> String {
    match text(options, "reason") {
        Some(reason) if reason.chars().count() >= MIN_REASON => reason.chars().take(MAX_REASON).collect(),
        _ => DEFAULT_REASON.to_string(),
    }
}

/// Give (`give`) or take away a badge for the owner, after every check
async fn act(db: &Database, caller: &User, data: DataBadgeCommand, give: bool) -> Result<BadgeCommandResult> {
    let settings = settings(caller).await?;
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
    let (sub, action) = if give {
        ("give", "badge.command.grant")
    } else {
        ("remove", "badge.command.revoke")
    };
    // a `/badge give` is never turned into a removal, nor a button into a command
    if interaction.kind != InteractionType::Command || interaction.command_name.as_deref() != Some(COMMAND) {
        return Err(create_error!(InvalidOperation));
    }
    let options = chosen(&interaction, sub).ok_or_else(|| create_error!(InvalidOperation))?;
    let subject = text(options, "user");
    let badge = text(options, "badge").unwrap_or_default();

    // one press, one change (and one refusal in the log)
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }

    if interaction.user_id != settings.owner {
        record_platform_audit(
            db,
            AuditRecord {
                actor_id: interaction.user_id.clone(),
                action: action.to_string(),
                target_kind: Some("user".to_string()),
                target_id: subject.clone(),
                summary: Some(format!("{badge} (owner only)")),
                ..Default::default()
            },
            AuditOutcome::Denied,
        )
        .await?;
        return Err(create_error!(NotPrivileged));
    }

    let subject = subject.filter(|s| !s.is_empty()).ok_or_else(|| create_error!(InvalidOperation))?;
    let definition = badge_definition(&badge).ok_or_else(|| create_error!(NotFound))?;
    if !offered(definition) {
        return Err(create_error!(InvalidOperation));
    }
    let person = db.fetch_user(&subject).await.map_err(|_| create_error!(UnknownUser))?;
    let answer = |result: &'static str| BadgeCommandResult {
        action: sub,
        result,
        badge: definition.id.to_string(),
        names: crate::util::official::badge_names(definition.id),
        user: CommandPerson {
            id: person.id.clone(),
            name: format!("{}#{}", person.username, person.discriminator),
        },
    };
    // nobody changes their own badges, the owner neither
    if person.id == settings.owner {
        return Ok(answer("self"));
    }
    if person.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let reason = reason_of(options);
    let record = AuditRecord {
        actor_id: settings.owner.clone(),
        action: action.to_string(),
        target_kind: Some("user".to_string()),
        target_id: Some(person.id.clone()),
        reason: Some(reason.clone()),
        summary: Some(definition.id.to_string()),
        ..Default::default()
    };
    // written before the change: a failed write stops it
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    let rights = BadgeRights {
        staff: true,
        privileged: true,
    };
    let change = if give {
        grant_checked(
            db,
            &settings.owner,
            rights,
            DataGrantBadge {
                user: person.id.clone(),
                badge: definition.id.to_string(),
                reason: reason.clone(),
                expires_at: None,
            },
            true,
        )
        .await
    } else {
        revoke_with(
            db,
            &settings.owner,
            rights,
            DataRevokeBadge {
                user: person.id.clone(),
                badge: definition.id.to_string(),
                reason: reason.clone(),
            },
        )
        .await
    };
    match change {
        Ok(_) => {
            crate::util::official::badge(db, &person.id, give, definition.id, &reason).await;
            Ok(answer("done"))
        }
        Err(error) => {
            let mut failed = record;
            failed.summary = Some(format!("{} {:?}", definition.id, error.error_type).chars().take(200).collect());
            let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
            match error.error_type {
                ErrorType::NoEffect => Ok(answer(if give { "already" } else { "not_held" })),
                _ => Err(error),
            }
        }
    }
}

/// # Badge command: give
#[post("/grant", data = "<data>")]
async fn grant(db: &State<Database>, user: User, data: Json<DataBadgeCommand>) -> Result<Json<BadgeCommandResult>> {
    act(db, &user, data.into_inner(), true).await.map(Json)
}

/// # Badge command: take away
#[post("/revoke", data = "<data>")]
async fn revoke(db: &State<Database>, user: User, data: Json<DataBadgeCommand>) -> Result<Json<BadgeCommandResult>> {
    act(db, &user, data.into_inner(), false).await.map(Json)
}
