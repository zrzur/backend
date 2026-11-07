//! Server-side check of the values a user sends with an interaction.
//!
//! Nothing from the client is trusted: names and types must match the stored
//! definition, values must respect choices and bounds, and every referenced
//! user, role or channel must exist in this server and be visible to the user.

use std::collections::HashSet;

use futures::future::BoxFuture;
use revolt_database::{
    util::permissions::DatabasePermissionQuery, Channel, Database, File, Server, User,
};
use revolt_models::v0::{
    interaction_limits, ApplicationCommandOption, CommandChannelType, CommandOptionType,
    CommandOptionValue, InteractionDataOption, InteractionOptionValue, InteractionResolved,
};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};

pub fn invalid(error: impl Into<String>) -> revolt_result::Error {
    create_error!(FailedValidation {
        error: error.into()
    })
}

pub struct OptionContext<'a> {
    pub db: &'a Database,
    pub user: &'a User,
    /// Whose view picked people are shown from: never the person's, whose
    /// friends and blocks are none of the bot's business
    pub bot_user: &'a User,
    /// `None` in a DM with the bot, where people, roles and channels can't be picked
    pub server: Option<&'a Server>,
    pub resolved: InteractionResolved,
    /// Autocomplete: the option being typed into. Its value is partial text,
    /// and required options may still be missing.
    pub focused: Option<String>,
    /// Autocomplete: type of the focused option once found
    pub focused_type: Option<CommandOptionType>,
    /// What claimed attachments are recorded as used for
    pub attachment_parent: String,
}

impl<'a> OptionContext<'a> {
    pub fn new(
        db: &'a Database,
        user: &'a User,
        bot_user: &'a User,
        server: Option<&'a Server>,
    ) -> Self {
        OptionContext {
            db,
            user,
            bot_user,
            server,
            resolved: Default::default(),
            focused: None,
            focused_type: None,
            attachment_parent: ulid::Ulid::new().to_string(),
        }
    }

    /// Validate the user's options against the command definition
    pub async fn check(
        &mut self,
        definitions: &[ApplicationCommandOption],
        input: Vec<InteractionDataOption>,
    ) -> Result<Vec<InteractionDataOption>> {
        let output = self.check_level(definitions, input, 0).await?;
        if self.focused.is_some() && self.focused_type.is_none() {
            return Err(invalid("the focused option does not support autocomplete"));
        }
        Ok(output)
    }

    fn check_level<'s>(
        &'s mut self,
        definitions: &'s [ApplicationCommandOption],
        input: Vec<InteractionDataOption>,
        depth: usize,
    ) -> BoxFuture<'s, Result<Vec<InteractionDataOption>>> {
        Box::pin(async move {
            if depth > 2 || input.len() > definitions.len() {
                return Err(invalid("too many options"));
            }

            let has_subcommands = definitions.iter().any(|definition| {
                matches!(
                    definition.kind,
                    CommandOptionType::SubCommand | CommandOptionType::SubCommandGroup
                )
            });

            if has_subcommands {
                // Exactly one sub-command or group is chosen
                let [chosen] = <[InteractionDataOption; 1]>::try_from(input)
                    .map_err(|_| invalid("choose exactly one sub-command"))?;
                let definition = definitions
                    .iter()
                    .find(|d| d.name == chosen.name && d.kind == chosen.kind)
                    .ok_or_else(|| invalid(format!("unknown sub-command '{}'", chosen.name)))?;
                if chosen.value.is_some() {
                    return Err(invalid("sub-commands take no value"));
                }
                let children = self
                    .check_level(
                        definition.options.as_deref().unwrap_or_default(),
                        chosen.options.unwrap_or_default(),
                        depth + 1,
                    )
                    .await?;
                return Ok(vec![InteractionDataOption {
                    name: definition.name.clone(),
                    kind: definition.kind,
                    value: None,
                    options: Some(children),
                }]);
            }

            let mut seen = HashSet::new();
            let mut output = Vec::with_capacity(input.len());
            for option in input {
                if !seen.insert(option.name.clone()) {
                    return Err(invalid(format!("option '{}' given twice", option.name)));
                }
                let definition = definitions
                    .iter()
                    .find(|d| d.name == option.name)
                    .ok_or_else(|| invalid(format!("unknown option '{}'", option.name)))?;
                if option.kind != definition.kind || option.options.is_some() {
                    return Err(invalid(format!("option '{}' has the wrong type", option.name)));
                }
                let value = option
                    .value
                    .ok_or_else(|| invalid(format!("option '{}' needs a value", option.name)))?;

                let value = if self.focused.as_deref() == Some(definition.name.as_str()) {
                    self.check_focused(definition, value)?
                } else {
                    self.check_value(definition, value).await?
                };
                output.push(InteractionDataOption {
                    name: definition.name.clone(),
                    kind: definition.kind,
                    value: Some(value),
                    options: None,
                });
            }

            if self.focused.is_none() {
                if let Some(missing) = definitions
                    .iter()
                    .find(|d| d.required && !seen.contains(&d.name))
                {
                    return Err(invalid(format!("option '{}' is required", missing.name)));
                }
            }

            Ok(output)
        })
    }

    /// The option being typed into: any short partial text is fine
    fn check_focused(
        &mut self,
        definition: &ApplicationCommandOption,
        value: InteractionOptionValue,
    ) -> Result<InteractionOptionValue> {
        if !definition.autocomplete {
            return Err(invalid("the focused option does not support autocomplete"));
        }
        let text = match value {
            InteractionOptionValue::String(text) => text,
            InteractionOptionValue::Integer(n) => n.to_string(),
            InteractionOptionValue::Number(n) => n.to_string(),
            InteractionOptionValue::Boolean(_) => return Err(invalid("wrong focused value")),
        };
        if text.chars().count() > interaction_limits::MAX_FOCUSED_VALUE {
            return Err(invalid("focused value is too long"));
        }
        self.focused_type = Some(definition.kind);
        Ok(InteractionOptionValue::String(text))
    }

    async fn check_value(
        &mut self,
        definition: &ApplicationCommandOption,
        value: InteractionOptionValue,
    ) -> Result<InteractionOptionValue> {
        let name = &definition.name;
        let wrong_type = || invalid(format!("option '{name}' has the wrong type"));

        let value = match (definition.kind, value) {
            (CommandOptionType::Boolean, InteractionOptionValue::Boolean(b)) => {
                InteractionOptionValue::Boolean(b)
            }
            (CommandOptionType::String, InteractionOptionValue::String(text)) => {
                let len = text.chars().count();
                let min = definition.min_length.unwrap_or(0) as usize;
                let max = definition
                    .max_length
                    .map(|max| max as usize)
                    .unwrap_or(interaction_limits::MAX_STRING_VALUE);
                if len < min || len > max {
                    return Err(invalid(format!("option '{name}' must be {min}-{max} characters")));
                }
                InteractionOptionValue::String(text)
            }
            (CommandOptionType::Integer, InteractionOptionValue::Integer(n)) => {
                self.check_bounds(definition, n as f64)?;
                InteractionOptionValue::Integer(n)
            }
            (CommandOptionType::Number, InteractionOptionValue::Integer(n)) => {
                self.check_bounds(definition, n as f64)?;
                InteractionOptionValue::Number(n as f64)
            }
            (CommandOptionType::Number, InteractionOptionValue::Number(n)) => {
                if !n.is_finite() {
                    return Err(wrong_type());
                }
                self.check_bounds(definition, n)?;
                InteractionOptionValue::Number(n)
            }
            (
                CommandOptionType::User
                | CommandOptionType::Role
                | CommandOptionType::Channel
                | CommandOptionType::Mentionable,
                InteractionOptionValue::String(id),
            ) => {
                self.resolve_id(
                    definition.kind,
                    name,
                    definition.channel_types.as_deref(),
                    &id,
                )
                .await?;
                InteractionOptionValue::String(id)
            }
            (CommandOptionType::Attachment, InteractionOptionValue::String(id)) => {
                self.use_attachment(name, &id).await?;
                InteractionOptionValue::String(id)
            }
            _ => return Err(wrong_type()),
        };

        if let Some(choices) = &definition.choices {
            let allowed = choices.iter().any(|choice| match (&choice.value, &value) {
                (CommandOptionValue::String(a), InteractionOptionValue::String(b)) => a == b,
                (CommandOptionValue::Integer(a), InteractionOptionValue::Integer(b)) => a == b,
                (CommandOptionValue::Integer(a), InteractionOptionValue::Number(b)) => {
                    *a as f64 == *b
                }
                (CommandOptionValue::Number(a), InteractionOptionValue::Number(b)) => a == b,
                _ => false,
            });
            if !allowed {
                return Err(invalid(format!("option '{name}' must be one of its choices")));
            }
        }

        Ok(value)
    }

    /// Claim a file the user just uploaded so the bot can download it.
    /// Only the uploader can hand it over, and only once.
    pub async fn use_attachment(&mut self, name: &str, id: &str) -> Result<()> {
        if id.is_empty() || id.len() > 128 {
            return Err(invalid(format!("'{name}' refers to nothing usable")));
        }
        // autocomplete runs on every key press: claim only when the command is sent
        if self.focused.is_some() {
            return Ok(());
        }
        let file = File::use_attachment(self.db, id, &self.attachment_parent, &self.user.id)
            .await
            .map_err(|_| invalid(format!("'{name}' refers to nothing usable")))?;
        self.resolved
            .attachments
            .insert(id.to_string(), file.into());
        Ok(())
    }

    fn check_bounds(&self, definition: &ApplicationCommandOption, value: f64) -> Result<()> {
        if definition.min_value.is_some_and(|min| value < min)
            || definition.max_value.is_some_and(|max| value > max)
        {
            return Err(invalid(format!("option '{}' is out of range", definition.name)));
        }
        Ok(())
    }

    /// Resolve an id to a user, role or channel in this server the user may see.
    ///
    /// `kind` is the option type (`User`, `Role`, `Channel`, `Mentionable`).
    pub async fn resolve_id(
        &mut self,
        kind: CommandOptionType,
        name: &str,
        channel_types: Option<&[CommandChannelType]>,
        id: &str,
    ) -> Result<()> {
        let not_found = || invalid(format!("'{name}' refers to nothing usable"));
        if id.len() != 26 {
            return Err(not_found());
        }
        let Some(here) = self.server else {
            return Err(not_found());
        };

        let wants_user = matches!(kind, CommandOptionType::User | CommandOptionType::Mentionable);
        let wants_role = matches!(kind, CommandOptionType::Role | CommandOptionType::Mentionable);

        if wants_role {
            if let Some(role) = here.roles.get(id) {
                self.resolved
                    .roles
                    .insert(id.to_string(), role.clone().into());
                return Ok(());
            }
        }

        if wants_user {
            if let Ok(member) = self.db.fetch_member(&here.id, id).await {
                let user = self.db.fetch_user(id).await.map_err(|_| not_found())?;
                self.resolved
                    .users
                    .insert(id.to_string(), user.into(self.db, Some(self.bot_user)).await);
                self.resolved.members.insert(id.to_string(), member.into());
                return Ok(());
            }
        }

        if kind == CommandOptionType::Channel {
            let channel = self.db.fetch_channel(id).await.map_err(|_| not_found())?;
            let (in_server, is_voice) = match &channel {
                Channel::TextChannel { server, voice, .. } => {
                    (server == &here.id, voice.is_some())
                }
                _ => (false, false),
            };
            if !in_server {
                return Err(not_found());
            }
            if let Some(types) = channel_types {
                let channel_kind = if is_voice {
                    CommandChannelType::Voice
                } else {
                    CommandChannelType::Text
                };
                if !types.contains(&channel_kind) {
                    return Err(invalid(format!(
                        "'{name}' does not accept this kind of channel"
                    )));
                }
            }

            // Hidden channels look like missing ones
            let mut query = DatabasePermissionQuery::new(self.db, self.user).channel(&channel);
            if !calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::ViewChannel)
            {
                return Err(not_found());
            }

            self.resolved.channels.insert(id.to_string(), channel.into());
            return Ok(());
        }

        Err(not_found())
    }
}
