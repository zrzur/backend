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
//!   refused and the try is written to the platform audit log as Denied;
//! - that the command is registered exactly as the server offers it (`user`
//!   a User option, `badge` choices equal to the catalogue with the server's
//!   names), so a bot cannot relabel a choice. Otherwise Denied as well.
//!
//! Only `/badge` interactions of the configured bot keep their options
//! (`Interaction::command_options`, gone when the interaction expires).
//! One press writes one audit record; the owner also gets a confirmation
//! from Zrzur for every change, independent of what the bot shows.
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
    badge_definition, held_badges, record_platform_audit, sync_user_badges, AuditOutcome,
    AuditRecord, BadgeDefinition, BadgeGrantRule, Database, Interaction, User, BADGE_CATALOGUE,
};
use revolt_models::v0::{
    CommandOptionType, CommandOptionValue, DataGrantBadge, DataRevokeBadge,
    InteractionDataOption, InteractionOptionValue, InteractionType,
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
    match current(&caller.id).await {
        Some(settings) if settings.bot == caller.id => Ok(settings),
        _ => Err(create_error!(NotFound)),
    }
}

/// The settings that apply to `bot`, both ids trimmed; `None` when off
async fn current(bot: &str) -> Option<FeaturesBadgeCommand> {
    let trimmed = |s: FeaturesBadgeCommand| FeaturesBadgeCommand {
        owner: s.owner.trim().to_string(),
        bot: s.bot.trim().to_string(),
    };
    #[cfg(test)]
    if let Some(settings) = test_settings::get(bot) {
        return Some(trimmed(settings)).filter(|s| s.enabled());
    }
    let _ = bot;
    Some(trimmed(config().await.features.badge_command)).filter(|s| s.enabled())
}

/// Whether a new command interaction keeps its options: only `/badge` of
/// the configured bot, so no other bot's options are ever stored
pub(crate) async fn keeps_options(bot: &str, command_name: &str) -> bool {
    command_name == COMMAND && current(bot).await.is_some_and(|s| s.bot == bot)
}

/// Badges the command offers: every hand-given one, privileged and retired
/// included; never one that follows a record
fn offered(definition: &BadgeDefinition) -> bool {
    matches!(definition.rule, BadgeGrantRule::Staff | BadgeGrantRule::Privileged)
}

/// The offered badges in the catalogue's order: active first, by priority
pub(crate) fn offered_badges() -> Vec<&'static BadgeDefinition> {
    let mut badges: Vec<&'static BadgeDefinition> = BADGE_CATALOGUE.iter().filter(|b| offered(b)).collect();
    badges.sort_by_key(|b| (b.retired, b.priority));
    badges
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
    Ok(Json(CommandCatalogue {
        enabled: true,
        badges: offered_badges()
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

/// The value of option `name`, only if it has the expected kind
fn value_of(options: &[InteractionDataOption], name: &str, kind: CommandOptionType) -> Option<String> {
    options
        .iter()
        .find(|o| o.name == name && o.kind == kind)
        .and_then(|o| match &o.value {
            Some(InteractionOptionValue::String(value)) => Some(value.trim().to_string()),
            _ => None,
        })
}

fn text(options: &[InteractionDataOption], name: &str) -> Option<String> {
    value_of(options, name, CommandOptionType::String)
}

/// The command the person used is registered exactly as the server offers
/// it: `user` a User option, `reason` (if any) a String option, and `badge`
/// a String option whose choices are the catalogue, in order, each with the
/// server's own names. A bot that relabels a choice ("Partner" sending
/// `founder`) or asks for a user as text is refused.
async fn registered_as_offered(db: &Database, interaction: &Interaction, sub: &str) -> bool {
    let Some(id) = interaction.command_id.as_deref() else {
        return false;
    };
    let Ok(command) = db.fetch_application_command(id).await else {
        return false;
    };
    if command.application_id != interaction.application_id || command.name != COMMAND {
        return false;
    }
    // the command the person saw, not one registered since: a bot could
    // show a relabelled command, take the press, then put the honest one back
    if interaction.command_version.as_deref() != Some(command.version.as_str()) {
        return false;
    }
    let Some(chosen) = command
        .options
        .iter()
        .find(|o| o.kind == CommandOptionType::SubCommand && o.name == sub)
    else {
        return false;
    };
    let options = chosen.options.as_deref().unwrap_or_default();
    let kind_of = |name: &str| options.iter().find(|o| o.name == name).map(|o| o.kind);
    if kind_of("user") != Some(CommandOptionType::User)
        || kind_of("badge") != Some(CommandOptionType::String)
        || kind_of("reason").is_some_and(|kind| kind != CommandOptionType::String)
        || options.iter().any(|o| !["user", "badge", "reason"].contains(&o.name.as_str()))
    {
        return false;
    }
    let choices = options
        .iter()
        .find(|o| o.name == "badge")
        .and_then(|o| o.choices.as_deref())
        .unwrap_or_default();
    let offered = offered_badges();
    choices.len() == offered.len()
        && choices.iter().zip(offered).all(|(choice, badge)| {
            let names = crate::util::official::badge_names(badge.id);
            let english = names.get("en").cloned().unwrap_or_else(|| badge.id.to_string());
            matches!(&choice.value, CommandOptionValue::String(value) if value == badge.id)
                && choice.name == english
                && choice.name_localizations.as_ref().is_none_or(|labels| {
                    labels
                        .iter()
                        .all(|(lang, label)| label == names.get(lang.as_str()).unwrap_or(&english))
                })
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
    // a user only from a User option, a badge only from a String option
    let subject = value_of(options, "user", CommandOptionType::User);
    let badge = text(options, "badge").unwrap_or_default();
    // the log names a badge only if the catalogue knows it
    let known = badge_definition(&badge).map(|d| d.id).unwrap_or("unknown");

    // one press, one change (and one refusal in the log)
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }

    let deny = |actor: &str, why: &str| AuditRecord {
        actor_id: actor.to_string(),
        action: action.to_string(),
        target_kind: Some("user".to_string()),
        target_id: subject.clone(),
        summary: Some(format!("{known} ({why})")),
        ..Default::default()
    };
    if interaction.user_id != settings.owner {
        record_platform_audit(db, deny(&interaction.user_id, "owner only"), AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    // the owner's press, but on a command the bot registered other than offered
    if subject.is_none() || !registered_as_offered(db, &interaction, sub).await {
        record_platform_audit(db, deny(&settings.owner, "command not as offered"), AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    }

    let subject = subject.filter(|s| !s.is_empty()).ok_or_else(|| create_error!(InvalidOperation))?;
    let definition = badge_definition(&badge).ok_or_else(|| create_error!(NotFound))?;
    if !offered(definition) {
        return Err(create_error!(InvalidOperation));
    }
    let person = db.fetch_user(&subject).await.map_err(|_| create_error!(UnknownUser))?;
    let person_name = format!("{}#{}", person.username, person.discriminator);
    let answer = |result: &'static str| BadgeCommandResult {
        action: sub,
        result,
        badge: definition.id.to_string(),
        names: crate::util::official::badge_names(definition.id),
        user: CommandPerson {
            id: person.id.clone(),
            name: person_name.clone(),
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
    let mut record = AuditRecord {
        actor_id: settings.owner.clone(),
        action: action.to_string(),
        target_kind: Some("user".to_string()),
        target_id: Some(person.id.clone()),
        reason: Some(reason.clone()),
        summary: Some(definition.id.to_string()),
        ..Default::default()
    };
    // nothing to do: said with one record, and no change
    sync_user_badges(db, &person.id).await?;
    let holds = held_badges(db, &person.id).await?.contains(definition.id);
    if holds == give {
        let result = if give { "already" } else { "not_held" };
        record.summary = Some(format!("{} (no change: {result})", definition.id));
        record_platform_audit(db, record, AuditOutcome::Success).await?;
        return Ok(answer(result));
    }
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
            // the owner's own confirmation, from Zrzur: it does not depend on the bot
            crate::util::official::badge_command_done(db, &settings.owner, give, definition.id, &person_name, &reason).await;
            Ok(answer("done"))
        }
        // someone else changed it in the same moment: nothing was done
        Err(error) if matches!(error.error_type, ErrorType::NoEffect) => Ok(answer(if give { "already" } else { "not_held" })),
        Err(error) => {
            let mut failed = record;
            failed.summary = Some(format!("{} {:?}", definition.id, error.error_type).chars().take(200).collect());
            let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
            Err(error)
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
