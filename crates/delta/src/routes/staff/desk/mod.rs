//! Zrzur Desk: the staff bot (`[features.staff_desk]`), mounted at
//! `/staff/desk`. Kept out of the public OpenAPI document, like the other
//! bot desks.
//!
//! The bot decides nothing. It hands back the press it received (the
//! interaction's id and token, nothing else); the server checks that the
//! press is genuine, single use, from a configured place, and that the
//! person who pressed holds an active staff grant now and an open desk
//! session. It then builds the console's own [`StaffContext`], so every
//! permission check, step-up and audit record is the console's code, run
//! as the presser. A stolen bot token gains nothing: without a person's
//! press, their grant and their desk session, every route refuses.
//!
//! [`StaffContext`]: super::StaffContext

mod actor;
#[cfg(test)]
mod framework_tests;
mod lookup;
#[cfg(test)]
mod lookup_tests;
mod moderation;
#[cfg(test)]
mod moderation_tests;
mod overview;
pub(crate) mod registry;
mod run;
mod session;
#[cfg(test)]
mod tests;
pub(crate) mod view;

// for the desk routes of later phases
#[allow(unused_imports)]
pub(crate) use actor::{DataDeskInteraction, Expect, desk_actor, field};

use revolt_config::{FeaturesStaffDesk, config};
use revolt_database::User;
use revolt_result::{Result, create_error};
use rocket::Route;

/// The command's name as the bot registers it (`/desk me`, `/desk logout`)
pub const COMMAND: &str = "desk";
/// The desk's forms have custom ids starting with this
pub const FORM_PREFIX: &str = "zd:";
/// The form field that carries the TOTP code
pub const CODE_FIELD: &str = "code";

/// The routes, mounted under `/staff` (so at `/staff/desk/...`)
pub fn routes() -> Vec<Route> {
    routes![
        session::open,
        session::close,
        session::step_up,
        session::me,
        overview::overview,
        run::commands,
        run::run,
        run::confirm
    ]
}

#[cfg(test)]
pub(crate) mod test_settings {
    //! Tests run side by side, each with its own bot: a test's settings are
    //! looked up by the calling bot instead of the one config.
    use revolt_config::FeaturesStaffDesk;
    use std::{collections::HashMap, sync::Mutex};

    static SETTINGS: Mutex<Option<HashMap<String, FeaturesStaffDesk>>> = Mutex::new(None);

    pub fn register(settings: FeaturesStaffDesk) {
        SETTINGS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(settings.bot.trim().to_string(), settings);
    }

    pub fn get(bot: &str) -> Option<FeaturesStaffDesk> {
        SETTINGS
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|all| all.get(bot).cloned())
    }
}

/// The settings, if the desk is on and `caller` is its bot. Anyone else
/// gets "not found": the routes do not exist for them.
pub(crate) async fn settings(caller: &User) -> Result<FeaturesStaffDesk> {
    if caller.bot.is_none() {
        return Err(create_error!(NotFound));
    }
    match current(&caller.id).await {
        Some(settings) if settings.bot == caller.id => Ok(settings),
        _ => Err(create_error!(NotFound)),
    }
}

/// The settings that apply to `bot`, ids trimmed; `None` when off
async fn current(bot: &str) -> Option<FeaturesStaffDesk> {
    let trimmed = |s: FeaturesStaffDesk| FeaturesStaffDesk {
        bot: s.bot.trim().to_string(),
        places: s
            .places
            .iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        areas: s
            .areas
            .iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect(),
        ..s
    };
    #[cfg(test)]
    if let Some(settings) = test_settings::get(bot) {
        return Some(trimmed(settings)).filter(|s| s.on());
    }
    let _ = bot;
    Some(trimmed(config().await.features.staff_desk)).filter(|s| s.on())
}

/// Whether a submitted form's `code` field is kept from the bot: any `zd:`
/// form of the configured desk bot, also while the desk is switched off
pub(crate) async fn hides_field_from_bot(bot: &str, form: &str) -> bool {
    if !form.starts_with(FORM_PREFIX) || bot.is_empty() {
        return false;
    }
    #[cfg(test)]
    if let Some(settings) = test_settings::get(bot) {
        return settings.bot.trim() == bot;
    }
    config().await.features.staff_desk.bot.trim() == bot
}

/// Whether every answer to `interaction` must be private: a desk command,
/// or a `zd:` button or form, of the configured desk bot (also while the
/// desk is off). Staff data never goes to a channel.
pub(crate) async fn answers_privately(interaction: &revolt_database::Interaction) -> bool {
    let bot = interaction.application_id.trim();
    let desk = interaction
        .command_name
        .as_deref()
        .is_some_and(registry::is_desk_command)
        || interaction
            .custom_id
            .as_deref()
            .is_some_and(|id| id.starts_with(FORM_PREFIX));
    if !desk || bot.is_empty() {
        return false;
    }
    #[cfg(test)]
    if let Some(settings) = test_settings::get(bot) {
        return settings.bot.trim() == bot;
    }
    config().await.features.staff_desk.bot.trim() == bot
}

/// Whether a new command interaction keeps its options: only `/desk` of
/// the configured desk bot, so the desk routes can read the sub-command
/// and options the server itself checked; no other bot's options are ever
/// stored. Such a press must name the command version the person saw
/// (`/interactions` refuses it without one), and the version is stored
/// with the options for the registered-as-offered check.
pub(crate) async fn keeps_options(bot: &str, command_name: &str) -> bool {
    registry::is_desk_command(command_name) && current(bot).await.is_some_and(|s| s.bot == bot)
}
