//! Zrzur developer platform: application commands.
//!
//! An application is a bot: the application id is the bot's id (and its
//! user id). Commands are either global (every server the bot is in) or
//! scoped to one server.

use std::collections::HashMap;

use once_cell::sync::Lazy;
use regex::Regex;

/// Same derives as `auto_derived!` without `Eq`: commands carry `f64` bounds.
macro_rules! command_derived {
    ( $( $item:item )+ ) => {
        $(
            #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
            #[cfg_attr(feature = "schemas", derive(JsonSchema))]
            #[cfg_attr(feature = "utoipa", derive(ToSchema))]
            #[derive(Debug, Clone, PartialEq)]
            $item
        )+
    };
}

/// Localised strings, keyed by locale (`ar`, `en`, `de`, `pt-BR`, ...)
pub type Localizations = HashMap<String, String>;

command_derived!(
    /// Kind of application command
    #[derive(Default, Copy)]
    pub enum ApplicationCommandType {
        /// Typed in the message box as `/name`
        #[default]
        ChatInput,
        /// Shown in a user's context menu
        User,
        /// Shown in a message's context menu
        Message,
    }

    /// Kind of command option
    #[derive(Copy)]
    pub enum CommandOptionType {
        SubCommand,
        SubCommandGroup,
        String,
        Integer,
        Number,
        Boolean,
        User,
        Role,
        Channel,
        Mentionable,
        Attachment,
    }

    /// Channel kinds a `Channel` option may be restricted to
    #[derive(Copy, Eq)]
    pub enum CommandChannelType {
        Text,
        Voice,
    }

    /// Value of a predefined choice
    #[cfg_attr(feature = "serde", serde(untagged))]
    pub enum CommandOptionValue {
        Integer(i64),
        Number(f64),
        String(String),
    }

    /// Predefined choice for a `String`, `Integer` or `Number` option
    pub struct CommandOptionChoice {
        /// Shown to the user
        pub name: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub name_localizations: Option<Localizations>,
        /// Sent to the bot
        pub value: CommandOptionValue,
    }

    /// Parameter, sub-command or sub-command group of a command
    pub struct ApplicationCommandOption {
        #[cfg_attr(feature = "serde", serde(rename = "type"))]
        pub kind: CommandOptionType,
        pub name: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub name_localizations: Option<Localizations>,
        pub description: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub description_localizations: Option<Localizations>,
        /// Whether the user must fill this option in
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub required: bool,
        /// Fixed list of accepted values
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub choices: Option<Vec<CommandOptionChoice>>,
        /// Nested options of a sub-command or sub-command group
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub options: Option<Vec<ApplicationCommandOption>>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub channel_types: Option<Vec<CommandChannelType>>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub min_value: Option<f64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub max_value: Option<f64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub min_length: Option<u32>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub max_length: Option<u32>,
        /// Suggestions are fetched from the bot while the user types
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub autocomplete: bool,
    }

    /// Registered application command
    pub struct ApplicationCommand {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Application (bot) id
        pub application_id: String,
        /// Server the command is limited to; global when absent
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub server_id: Option<String>,
        #[cfg_attr(feature = "serde", serde(rename = "type"))]
        pub kind: ApplicationCommandType,
        pub name: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub name_localizations: Option<Localizations>,
        /// Empty for context menu commands
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "String::is_empty", default))]
        pub description: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub description_localizations: Option<Localizations>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub options: Vec<ApplicationCommandOption>,
        /// Server permissions a member needs to see and use the command,
        /// on top of the channel's own checks
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub default_member_permissions: Option<i64>,
        /// Only usable in channels marked as mature
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub nsfw: bool,
        /// Changes on every update, so an interaction names the exact revision used
        pub version: String,
    }

    /// Command definition sent by a developer (create and bulk sync)
    #[derive(Default)]
    pub struct DataApplicationCommand {
        #[cfg_attr(feature = "serde", serde(rename = "type", default))]
        pub kind: ApplicationCommandType,
        pub name: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub name_localizations: Option<Localizations>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub description: String,
        #[cfg_attr(feature = "serde", serde(default))]
        pub description_localizations: Option<Localizations>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub options: Vec<ApplicationCommandOption>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub default_member_permissions: Option<i64>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub nsfw: bool,
    }

    /// Partial update of a command; the type cannot change
    #[derive(Default)]
    pub struct DataEditApplicationCommand {
        pub name: Option<String>,
        pub name_localizations: Option<Localizations>,
        pub description: Option<String>,
        pub description_localizations: Option<Localizations>,
        pub options: Option<Vec<ApplicationCommandOption>>,
        pub default_member_permissions: Option<i64>,
        pub nsfw: Option<bool>,
    }
);

/// Registration limits, per application and scope (global or one server)
pub mod command_limits {
    pub const MAX_CHAT_INPUT: usize = 100;
    pub const MAX_USER: usize = 5;
    pub const MAX_MESSAGE: usize = 5;
    pub const MAX_OPTIONS: usize = 25;
    pub const MAX_CHOICES: usize = 25;
    pub const MAX_LOCALIZATIONS: usize = 30;
    pub const NAME_LENGTH: usize = 32;
    pub const DESCRIPTION_LENGTH: usize = 100;
    pub const CHOICE_NAME_LENGTH: usize = 100;
    pub const CHOICE_VALUE_LENGTH: usize = 100;
    pub const MAX_STRING_LENGTH: u32 = 6000;
    /// Sum of every name, description and choice name, localisations included
    pub const MAX_TOTAL_CHARACTERS: usize = 8000;
    /// Largest integer a JavaScript client can hold exactly
    pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
}

/// Slash command and option names: lowercase letters (any script), digits, `-` and `_`
static RE_COMMAND_NAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[\p{Ll}\p{Lm}\p{Lo}\p{M}\p{N}_-]+$").unwrap());
static RE_LOCALE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z]{2,3}(-[A-Za-z0-9]{2,8})?$").unwrap());

impl ApplicationCommandType {
    /// Most commands of this type an application may register in one scope
    pub fn limit(&self) -> usize {
        match self {
            ApplicationCommandType::ChatInput => command_limits::MAX_CHAT_INPUT,
            ApplicationCommandType::User => command_limits::MAX_USER,
            ApplicationCommandType::Message => command_limits::MAX_MESSAGE,
        }
    }
}

impl CommandOptionType {
    fn is_subcommand(&self) -> bool {
        matches!(
            self,
            CommandOptionType::SubCommand | CommandOptionType::SubCommandGroup
        )
    }

    fn takes_choices(&self) -> bool {
        matches!(
            self,
            CommandOptionType::String | CommandOptionType::Integer | CommandOptionType::Number
        )
    }
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn check_chat_name(what: &str, name: &str) -> Result<(), String> {
    let len = chars(name);
    if len == 0 || len > command_limits::NAME_LENGTH {
        return Err(format!(
            "{what} must be 1-{} characters",
            command_limits::NAME_LENGTH
        ));
    }
    if !RE_COMMAND_NAME.is_match(name) {
        return Err(format!(
            "{what} may only contain lowercase letters, digits, '-' and '_'"
        ));
    }
    Ok(())
}

fn check_menu_name(what: &str, name: &str) -> Result<(), String> {
    let len = chars(name);
    if len == 0 || len > command_limits::NAME_LENGTH {
        return Err(format!(
            "{what} must be 1-{} characters",
            command_limits::NAME_LENGTH
        ));
    }
    if name.trim() != name || name.chars().any(|c| c.is_control()) {
        return Err(format!(
            "{what} may not have surrounding spaces or control characters"
        ));
    }
    Ok(())
}

fn check_description(what: &str, text: &str) -> Result<(), String> {
    let len = chars(text);
    if len == 0 || len > command_limits::DESCRIPTION_LENGTH {
        return Err(format!(
            "{what} must be 1-{} characters",
            command_limits::DESCRIPTION_LENGTH
        ));
    }
    if text.chars().any(|c| c.is_control()) {
        return Err(format!("{what} may not contain control characters"));
    }
    Ok(())
}

/// Checks locale keys and each value, and adds the values to `total`
fn check_localizations(
    what: &str,
    map: &Option<Localizations>,
    total: &mut usize,
    check: impl Fn(&str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let Some(map) = map else { return Ok(()) };
    if map.len() > command_limits::MAX_LOCALIZATIONS {
        return Err(format!(
            "{what} may have at most {} localizations",
            command_limits::MAX_LOCALIZATIONS
        ));
    }
    for (locale, value) in map {
        if !RE_LOCALE.is_match(locale) {
            return Err(format!("{what} has an invalid locale '{locale}'"));
        }
        check(&format!("{what} ({locale})"), value)?;
        *total += chars(value);
    }
    Ok(())
}

fn check_bound(what: &str, value: f64, integer: bool) -> Result<(), String> {
    if !value.is_finite() || value.abs() > command_limits::MAX_SAFE_INTEGER {
        return Err(format!("{what} is out of range"));
    }
    if integer && value.fract() != 0.0 {
        return Err(format!("{what} must be a whole number"));
    }
    Ok(())
}

fn validate_option(
    option: &ApplicationCommandOption,
    depth: usize,
    total: &mut usize,
) -> Result<(), String> {
    let what = format!("option '{}'", option.name);
    check_chat_name(&what, &option.name)?;
    check_description(&format!("{what} description"), &option.description)?;
    *total += chars(&option.name) + chars(&option.description);
    check_localizations(&what, &option.name_localizations, total, check_chat_name)?;
    check_localizations(
        &format!("{what} description"),
        &option.description_localizations,
        total,
        check_description,
    )?;

    let kind = option.kind;
    let is_text = kind == CommandOptionType::String;
    let is_numeric = matches!(kind, CommandOptionType::Integer | CommandOptionType::Number);

    if kind.is_subcommand() {
        if option.required
            || option.choices.is_some()
            || option.channel_types.is_some()
            || option.min_value.is_some()
            || option.max_value.is_some()
            || option.min_length.is_some()
            || option.max_length.is_some()
            || option.autocomplete
        {
            return Err(format!(
                "{what}: sub-commands and groups only take a name, description and options"
            ));
        }

        let children = option.options.as_deref().unwrap_or_default();
        match kind {
            CommandOptionType::SubCommandGroup => {
                if depth != 0 {
                    return Err(format!("{what}: groups are only allowed at the top level"));
                }
                if children.is_empty() {
                    return Err(format!("{what}: a group needs at least one sub-command"));
                }
                if children
                    .iter()
                    .any(|child| child.kind != CommandOptionType::SubCommand)
                {
                    return Err(format!("{what}: a group may only contain sub-commands"));
                }
            }
            _ => {
                if depth > 1 {
                    return Err(format!("{what}: sub-commands nest at most one group deep"));
                }
                if children.iter().any(|child| child.kind.is_subcommand()) {
                    return Err(format!("{what}: a sub-command may not contain sub-commands"));
                }
            }
        }

        return validate_option_list(children, depth + 1, total);
    }

    if option.options.is_some() {
        return Err(format!("{what}: only sub-commands and groups have options"));
    }

    if let Some(choices) = &option.choices {
        if !kind.takes_choices() {
            return Err(format!("{what}: only string, integer and number options take choices"));
        }
        if option.autocomplete {
            return Err(format!("{what}: choices and autocomplete are exclusive"));
        }
        if choices.is_empty() || choices.len() > command_limits::MAX_CHOICES {
            return Err(format!(
                "{what} must have 1-{} choices",
                command_limits::MAX_CHOICES
            ));
        }

        let mut names = std::collections::HashSet::new();
        for choice in choices {
            let len = chars(&choice.name);
            if len == 0 || len > command_limits::CHOICE_NAME_LENGTH {
                return Err(format!("{what}: choice names must be 1-100 characters"));
            }
            if !names.insert(choice.name.as_str()) {
                return Err(format!("{what}: duplicate choice '{}'", choice.name));
            }
            *total += len;
            check_localizations(
                &format!("{what} choice '{}'", choice.name),
                &choice.name_localizations,
                total,
                |w, v| {
                    let len = chars(v);
                    if len == 0 || len > command_limits::CHOICE_NAME_LENGTH {
                        Err(format!("{w} must be 1-100 characters"))
                    } else {
                        Ok(())
                    }
                },
            )?;

            match (&choice.value, kind) {
                (CommandOptionValue::String(value), CommandOptionType::String) => {
                    let len = chars(value);
                    if len == 0 || len > command_limits::CHOICE_VALUE_LENGTH {
                        return Err(format!("{what}: choice values must be 1-100 characters"));
                    }
                }
                (CommandOptionValue::Integer(value), CommandOptionType::Integer)
                | (CommandOptionValue::Integer(value), CommandOptionType::Number) => {
                    check_bound(&format!("{what} choice value"), *value as f64, false)?
                }
                (CommandOptionValue::Number(value), CommandOptionType::Number) => {
                    check_bound(&format!("{what} choice value"), *value, false)?
                }
                _ => return Err(format!("{what}: choice value has the wrong type")),
            }
        }
    }

    if option.autocomplete && !kind.takes_choices() {
        return Err(format!(
            "{what}: only string, integer and number options support autocomplete"
        ));
    }

    if option.min_value.is_some() || option.max_value.is_some() {
        if !is_numeric {
            return Err(format!("{what}: only integer and number options have min/max values"));
        }
        let integer = kind == CommandOptionType::Integer;
        if let Some(min) = option.min_value {
            check_bound(&format!("{what} min_value"), min, integer)?;
        }
        if let Some(max) = option.max_value {
            check_bound(&format!("{what} max_value"), max, integer)?;
        }
        if let (Some(min), Some(max)) = (option.min_value, option.max_value) {
            if min > max {
                return Err(format!("{what}: min_value is larger than max_value"));
            }
        }
    }

    if option.min_length.is_some() || option.max_length.is_some() {
        if !is_text {
            return Err(format!("{what}: only string options have a length range"));
        }
        let min = option.min_length.unwrap_or(0);
        let max = option.max_length.unwrap_or(command_limits::MAX_STRING_LENGTH);
        if max == 0 || max > command_limits::MAX_STRING_LENGTH || min > max {
            return Err(format!(
                "{what}: length range must lie within 0-{} with min <= max and max >= 1",
                command_limits::MAX_STRING_LENGTH
            ));
        }
    }

    if let Some(types) = &option.channel_types {
        if kind != CommandOptionType::Channel {
            return Err(format!("{what}: only channel options take channel_types"));
        }
        let unique: std::collections::HashSet<_> = types.iter().map(|t| *t as u8).collect();
        if types.is_empty() || unique.len() != types.len() {
            return Err(format!("{what}: channel_types must be non-empty and unique"));
        }
    }

    Ok(())
}

fn validate_option_list(
    options: &[ApplicationCommandOption],
    depth: usize,
    total: &mut usize,
) -> Result<(), String> {
    if options.len() > command_limits::MAX_OPTIONS {
        return Err(format!(
            "at most {} options are allowed at each level",
            command_limits::MAX_OPTIONS
        ));
    }

    let has_subcommands = options.iter().any(|option| option.kind.is_subcommand());
    if has_subcommands && options.iter().any(|option| !option.kind.is_subcommand()) {
        return Err("sub-commands and plain options cannot be mixed on one level".to_string());
    }

    let mut names = std::collections::HashSet::new();
    let mut seen_optional = false;
    for option in options {
        if !names.insert(option.name.as_str()) {
            return Err(format!("duplicate option name '{}'", option.name));
        }
        if !option.kind.is_subcommand() {
            if option.required && seen_optional {
                return Err(format!(
                    "required option '{}' must come before optional options",
                    option.name
                ));
            }
            seen_optional |= !option.required;
        }
        validate_option(option, depth, total)?;
    }

    Ok(())
}

impl DataApplicationCommand {
    /// Checks every structural rule of a command definition.
    ///
    /// Returns a message suitable for `FailedValidation`.
    pub fn validate_command(&self) -> Result<(), String> {
        let mut total = 0;
        match self.kind {
            ApplicationCommandType::ChatInput => {
                check_chat_name("command name", &self.name)?;
                check_description("command description", &self.description)?;
                check_localizations(
                    "command name",
                    &self.name_localizations,
                    &mut total,
                    check_chat_name,
                )?;
                check_localizations(
                    "command description",
                    &self.description_localizations,
                    &mut total,
                    check_description,
                )?;
                validate_option_list(&self.options, 0, &mut total)?;
            }
            ApplicationCommandType::User | ApplicationCommandType::Message => {
                check_menu_name("command name", &self.name)?;
                check_localizations(
                    "command name",
                    &self.name_localizations,
                    &mut total,
                    check_menu_name,
                )?;
                if !self.description.is_empty() || self.description_localizations.is_some() {
                    return Err("context menu commands have no description".to_string());
                }
                if !self.options.is_empty() {
                    return Err("context menu commands have no options".to_string());
                }
            }
        }

        total += chars(&self.name) + chars(&self.description);
        if total > command_limits::MAX_TOTAL_CHARACTERS {
            return Err(format!(
                "names, descriptions and choices may total at most {} characters",
                command_limits::MAX_TOTAL_CHARACTERS
            ));
        }

        if matches!(self.default_member_permissions, Some(value) if value < 0) {
            return Err("default_member_permissions must not be negative".to_string());
        }

        Ok(())
    }

    /// Applies a partial update on top of this definition
    pub fn apply_edit(&mut self, edit: DataEditApplicationCommand) {
        if let Some(name) = edit.name {
            self.name = name;
        }
        if let Some(value) = edit.name_localizations {
            self.name_localizations = Some(value);
        }
        if let Some(value) = edit.description {
            self.description = value;
        }
        if let Some(value) = edit.description_localizations {
            self.description_localizations = Some(value);
        }
        if let Some(value) = edit.options {
            self.options = value;
        }
        if let Some(value) = edit.default_member_permissions {
            self.default_member_permissions = Some(value);
        }
        if let Some(value) = edit.nsfw {
            self.nsfw = value;
        }
    }
}

impl From<ApplicationCommand> for DataApplicationCommand {
    fn from(command: ApplicationCommand) -> Self {
        DataApplicationCommand {
            kind: command.kind,
            name: command.name,
            name_localizations: command.name_localizations,
            description: command.description,
            description_localizations: command.description_localizations,
            options: command.options,
            default_member_permissions: command.default_member_permissions,
            nsfw: command.nsfw,
        }
    }
}

/// Checks a full set of commands for one scope: every definition, the
/// per-type limits and duplicate names.
pub fn validate_command_set(commands: &[DataApplicationCommand]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    let mut counts = HashMap::new();
    for command in commands {
        command
            .validate_command()
            .map_err(|error| format!("'{}': {error}", command.name))?;
        if !seen.insert((command.kind as u8, command.name.as_str())) {
            return Err(format!("duplicate command name '{}'", command.name));
        }
        let count = counts.entry(command.kind as u8).or_insert(0usize);
        *count += 1;
        if *count > command.kind.limit() {
            return Err(format!(
                "at most {} commands of this type are allowed",
                command.kind.limit()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(kind: CommandOptionType, name: &str) -> ApplicationCommandOption {
        ApplicationCommandOption {
            kind,
            name: name.to_string(),
            name_localizations: None,
            description: "desc".to_string(),
            description_localizations: None,
            required: false,
            choices: None,
            options: None,
            channel_types: None,
            min_value: None,
            max_value: None,
            min_length: None,
            max_length: None,
            autocomplete: false,
        }
    }

    fn hello() -> DataApplicationCommand {
        DataApplicationCommand {
            name: "hello".to_string(),
            description: "Say hello".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn accepts_simple_and_arabic_names() {
        assert!(hello().validate_command().is_ok());
        let mut arabic = hello();
        arabic.name = "مرحبا".to_string();
        arabic.name_localizations = Some(HashMap::from([("ar".into(), "أهلا".into())]));
        assert!(arabic.validate_command().is_ok());
    }

    #[test]
    fn rejects_bad_names() {
        for name in ["", "Hello", "with space", "a".repeat(33).as_str()] {
            let mut command = hello();
            command.name = name.to_string();
            assert!(command.validate_command().is_err(), "{name}");
        }
        let mut command = hello();
        command.name_localizations = Some(HashMap::from([("not a locale".into(), "x".into())]));
        assert!(command.validate_command().is_err());
    }

    #[test]
    fn context_menus_have_no_description_or_options() {
        let mut command = DataApplicationCommand {
            kind: ApplicationCommandType::User,
            name: "Report User".to_string(),
            ..Default::default()
        };
        assert!(command.validate_command().is_ok());
        command.description = "x".into();
        assert!(command.validate_command().is_err());
    }

    #[test]
    fn subcommand_structure() {
        let mut group = option(CommandOptionType::SubCommandGroup, "config");
        let mut sub = option(CommandOptionType::SubCommand, "set");
        sub.options = Some(vec![option(CommandOptionType::String, "value")]);
        group.options = Some(vec![sub.clone()]);

        let mut command = hello();
        command.options = vec![group.clone(), sub.clone()];
        assert!(command.validate_command().is_ok());

        // Mixed with plain options
        command.options = vec![sub.clone(), option(CommandOptionType::String, "x")];
        assert!(command.validate_command().is_err());

        // Group inside a group
        let mut nested = group.clone();
        nested.options = Some(vec![group.clone()]);
        command.options = vec![nested];
        assert!(command.validate_command().is_err());

        // Sub-command marked required
        let mut required = sub;
        required.required = true;
        command.options = vec![required];
        assert!(command.validate_command().is_err());
    }

    #[test]
    fn option_rules() {
        let mut command = hello();

        let mut required = option(CommandOptionType::String, "a");
        required.required = true;
        command.options = vec![option(CommandOptionType::String, "b"), required.clone()];
        assert!(command.validate_command().is_err(), "required after optional");

        command.options = vec![required.clone(), required.clone()];
        assert!(command.validate_command().is_err(), "duplicate names");

        let mut choices = option(CommandOptionType::Integer, "n");
        choices.choices = Some(vec![CommandOptionChoice {
            name: "one".into(),
            name_localizations: None,
            value: CommandOptionValue::String("1".into()),
        }]);
        command.options = vec![choices.clone()];
        assert!(command.validate_command().is_err(), "choice type mismatch");

        choices.choices.as_mut().unwrap()[0].value = CommandOptionValue::Integer(1);
        command.options = vec![choices.clone()];
        assert!(command.validate_command().is_ok());

        choices.autocomplete = true;
        command.options = vec![choices];
        assert!(command.validate_command().is_err(), "choices with autocomplete");

        let mut bounds = option(CommandOptionType::Integer, "n");
        bounds.min_value = Some(5.0);
        bounds.max_value = Some(1.0);
        command.options = vec![bounds.clone()];
        assert!(command.validate_command().is_err(), "min > max");
        bounds.max_value = Some(1.5);
        bounds.min_value = None;
        command.options = vec![bounds];
        assert!(command.validate_command().is_err(), "fractional integer bound");

        let mut length = option(CommandOptionType::Boolean, "b");
        length.max_length = Some(5);
        command.options = vec![length];
        assert!(command.validate_command().is_err(), "length on boolean");

        let mut channel = option(CommandOptionType::Channel, "c");
        channel.channel_types = Some(vec![CommandChannelType::Text, CommandChannelType::Text]);
        command.options = vec![channel];
        assert!(command.validate_command().is_err(), "duplicate channel types");
    }

    #[test]
    fn command_sets() {
        assert!(validate_command_set(&[hello(), hello()]).is_err());

        let mut user = hello();
        user.kind = ApplicationCommandType::User;
        user.description = String::new();
        // Same name, different type
        assert!(validate_command_set(&[hello(), user.clone()]).is_ok());

        let many: Vec<_> = (0..6)
            .map(|i| DataApplicationCommand {
                name: format!("u{i}"),
                ..user.clone()
            })
            .collect();
        assert!(validate_command_set(&many).is_err());
    }
}
