use std::collections::HashSet;

use revolt_database::{
    events::client::EventV1, util::permissions::DatabasePermissionQuery, Bot, Channel, Database,
    Interaction, InteractionSource, Member, Server, User, AMQP,
};
use revolt_models::v0::{
    self, find_component, interaction_limits, ApplicationCommandType, CommandOptionType,
    Component, DataCreateInteraction, InteractionAccepted, InteractionCommandData,
    InteractionComponentData, InteractionModalData, InteractionType, ModalFieldValue,
    INTERACTION_VERSION,
};
use revolt_permissions::{
    calculate_channel_permissions, ChannelPermission, PermissionQuery, PermissionValue,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::options::{invalid, OptionContext};
use crate::util::interaction_endpoint;

fn accepted(interaction: &Interaction) -> InteractionAccepted {
    InteractionAccepted {
        id: interaction.id.clone(),
        application_id: interaction.application_id.clone(),
        channel_id: interaction.channel_id.clone(),
        respond_by: interaction.respond_by,
        expires_at: interaction.expires_at,
    }
}

/// Where an interaction happens, with everyone's permissions
struct Scope {
    channel: Channel,
    /// `None` in a DM with the bot
    server: Option<Server>,
    member: Option<Member>,
    user_permissions: PermissionValue,
    bot: Bot,
    bot_user: User,
    app_permissions: PermissionValue,
}

/// Check the channel, the user's access, and that the bot is installed and can
/// see the channel. Components and forms (`in_dm`) also work in the person's
/// DM with the bot itself; commands stay in server channels.
async fn scope(
    db: &Database,
    user: &User,
    channel_id: &str,
    application_id: &str,
    need_send: bool,
    in_dm: bool,
) -> Result<Scope> {
    let channel = db.fetch_channel(channel_id).await?;
    let server_id = match &channel {
        Channel::TextChannel { server, .. } => Some(server.clone()),
        Channel::DirectMessage { recipients, .. }
            if in_dm && recipients.iter().any(|id| id == application_id) =>
        {
            None
        }
        _ => return Err(invalid("interactions are only available in server channels")),
    };

    let mut query = DatabasePermissionQuery::new(db, user).channel(&channel);
    let user_permissions = calculate_channel_permissions(&mut query).await;
    user_permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    if need_send {
        user_permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;
        // the answer lands in the thread, so the rules for sending there apply
        revolt_database::util::threads::assert_can_write(db, &channel, &user_permissions).await?;
        if let Channel::TextChannel {
            thread: Some(thread),
            ..
        } = &channel
        {
            if thread.locked {
                user_permissions
                    .throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;
            }
        }
    }
    query.are_we_a_member().await;
    let member = query.member_ref().as_ref().map(|m| m.clone().into_owned());
    let server = match &server_id {
        Some(id) => Some(db.fetch_server(id).await?),
        None => None,
    };

    let bot = db
        .fetch_bot(application_id)
        .await
        .map_err(|_| create_error!(UnknownApplication))?;
    // nothing reaches a disabled bot, on any path and of any kind
    if !revolt_database::bot_may_receive(db, &bot).await {
        return Err(create_error!(ApplicationOffline));
    }
    if let Some(server_id) = &server_id {
        db.fetch_member(server_id, &bot.id)
            .await
            .map_err(|_| create_error!(ApplicationNotInstalled))?;
    }
    let bot_user = db.fetch_user(&bot.id).await?;
    let mut bot_query = DatabasePermissionQuery::new(db, &bot_user).channel(&channel);
    let app_permissions = calculate_channel_permissions(&mut bot_query).await;
    if !app_permissions.has_channel_permission(ChannelPermission::ViewChannel) {
        return Err(create_error!(ApplicationNotInstalled));
    }

    Ok(Scope {
        channel,
        server,
        member,
        user_permissions,
        bot,
        bot_user,
        app_permissions,
    })
}

/// Payload for the bot, without the kind-specific parts
async fn payload(
    db: &Database,
    user: &User,
    scope: &Scope,
    interaction: &Interaction,
    token: String,
    locale: Option<String>,
) -> v0::Interaction {
    v0::Interaction {
        id: interaction.id.clone(),
        version: INTERACTION_VERSION,
        application_id: scope.bot.id.clone(),
        kind: interaction.kind,
        token: Some(token),
        user: user.clone().into(db, Some(&scope.bot_user)).await,
        member: scope.member.clone().map(Into::into),
        server_id: scope.server.as_ref().map(|server| server.id.clone()),
        channel_id: interaction.channel_id.clone(),
        user_permissions: u64::from(scope.user_permissions),
        app_permissions: u64::from(scope.app_permissions),
        user_permissions_string: u64::from(scope.user_permissions).to_string(),
        app_permissions_string: u64::from(scope.app_permissions).to_string(),
        locale: locale.filter(|locale| locale.len() <= 16),
        command: None,
        message_id: None,
        message_ephemeral: false,
        component: None,
        modal: None,
        respond_by: interaction.respond_by,
        expires_at: interaction.expires_at,
    }
}

/// Command or autocomplete: check the command and its options
async fn command(
    db: &Database,
    user: &User,
    data: DataCreateInteraction,
) -> Result<(Interaction, v0::Interaction)> {
    let autocomplete = data.kind == InteractionType::Autocomplete;
    let application_id = data
        .application_id
        .as_deref()
        .ok_or_else(|| invalid("application_id is required"))?;
    let command_id = data
        .command_id
        .as_deref()
        .ok_or_else(|| invalid("command_id is required"))?;
    let scope = scope(db, user, &data.channel_id, application_id, true, false).await?;
    // commands never reach the DM path of `scope`
    let server = scope
        .server
        .as_ref()
        .ok_or_else(|| invalid("interactions are only available in server channels"))?;

    // The command must belong to the bot, be global or for this server, and be current
    let command = db
        .fetch_application_command(command_id)
        .await
        .map_err(|_| create_error!(UnknownCommand))?;
    if command.application_id != scope.bot.id
        || command
            .server_id
            .as_ref()
            .is_some_and(|id| id != &server.id)
    {
        return Err(create_error!(UnknownCommand));
    }
    if data
        .command_version
        .as_ref()
        .is_some_and(|version| version != &command.version)
    {
        return Err(invalid("this command changed, reload and try again"));
    }
    let channel_nsfw = matches!(&scope.channel, Channel::TextChannel { nsfw: true, .. });
    if command.nsfw && !channel_nsfw {
        return Err(create_error!(UnknownCommand));
    }
    if let Some(required) = command.default_member_permissions {
        if !scope.user_permissions.has(required as u64) {
            return Err(create_error!(MissingPermission {
                permission: "Command".to_string()
            }));
        }
    }

    let mut context = OptionContext::new(db, user, &scope.bot_user, Some(server));
    if autocomplete {
        if command.kind != ApplicationCommandType::ChatInput {
            return Err(create_error!(InvalidOperation));
        }
        context.focused = Some(
            data.focused
                .clone()
                .ok_or_else(|| invalid("focused is required"))?,
        );
    }

    let options = match command.kind {
        ApplicationCommandType::ChatInput => {
            if data.target_id.is_some() {
                return Err(create_error!(InvalidOperation));
            }
            context.check(&command.options, data.options).await?
        }
        ApplicationCommandType::User => {
            let target = data
                .target_id
                .as_deref()
                .ok_or(create_error!(InvalidOperation))?;
            context
                .resolve_id(CommandOptionType::User, "target", None, target)
                .await
                .map_err(|_| create_error!(UnknownUser))?;
            vec![]
        }
        ApplicationCommandType::Message => {
            let target = data
                .target_id
                .as_deref()
                .ok_or(create_error!(InvalidOperation))?;
            let message = db.fetch_message(target).await?;
            if message.channel != scope.channel.id() {
                return Err(create_error!(UnknownMessage));
            }
            vec![]
        }
    };

    let (mut interaction, token) = Interaction::new(
        data.kind,
        scope.bot.id.clone(),
        user.id.clone(),
        Some(server.id.clone()),
        scope.channel.id().to_string(),
        data.nonce,
    );
    interaction.command_id = Some(command.id.clone());
    interaction.command_name = Some(command.name.clone());
    interaction.autocomplete_type = context.focused_type;

    let mut payload = payload(db, user, &scope, &interaction, token, data.locale).await;
    payload.command = Some(InteractionCommandData {
        id: command.id,
        name: command.name,
        kind: command.kind,
        version: command.version,
        options,
        target_id: data.target_id,
        focused: context.focused.clone(),
        resolved: Some(context.resolved),
    });
    Ok((interaction, payload))
}

/// Who may use a component
fn check_restriction(component: &Component, user: &User, member: &Option<Member>) -> Result<()> {
    if let Some(restrict) = component.restriction() {
        if restrict.users.is_empty() && restrict.roles.is_empty() {
            return Ok(());
        }
        let by_user = restrict.users.contains(&user.id);
        let by_role = member
            .as_ref()
            .is_some_and(|m| m.roles.iter().any(|role| restrict.roles.contains(role)));
        if !by_user && !by_role {
            return Err(create_error!(MissingPermission {
                permission: "Component".to_string()
            }));
        }
    }
    Ok(())
}

/// Chosen values of a select: count, uniqueness, and each value checked again
async fn check_select_values(
    component: &Component,
    values: Vec<String>,
    context: &mut OptionContext<'_>,
) -> Result<Vec<String>> {
    let (min, max) = match component {
        Component::StringSelect {
            min_values,
            max_values,
            ..
        }
        | Component::UserSelect {
            min_values,
            max_values,
            ..
        }
        | Component::RoleSelect {
            min_values,
            max_values,
            ..
        }
        | Component::MentionableSelect {
            min_values,
            max_values,
            ..
        }
        | Component::ChannelSelect {
            min_values,
            max_values,
            ..
        } => (min_values.unwrap_or(1) as usize, max_values.unwrap_or(1) as usize),
        _ => {
            if !values.is_empty() {
                return Err(invalid("buttons take no values"));
            }
            return Ok(values);
        }
    };

    if values.len() < min || values.len() > max {
        return Err(invalid(format!("choose {min}-{max} values")));
    }
    if values.iter().collect::<HashSet<_>>().len() != values.len() {
        return Err(invalid("values must be unique"));
    }

    for value in &values {
        match component {
            Component::StringSelect { options, .. } => {
                if !options.iter().any(|option| &option.value == value) {
                    return Err(invalid("value is not one of the options"));
                }
            }
            Component::UserSelect { .. } => {
                context
                    .resolve_id(CommandOptionType::User, "value", None, value)
                    .await?
            }
            Component::RoleSelect { .. } => {
                context
                    .resolve_id(CommandOptionType::Role, "value", None, value)
                    .await?
            }
            Component::MentionableSelect { .. } => {
                context
                    .resolve_id(CommandOptionType::Mentionable, "value", None, value)
                    .await?
            }
            Component::ChannelSelect { channel_types, .. } => {
                context
                    .resolve_id(
                        CommandOptionType::Channel,
                        "value",
                        channel_types.as_deref(),
                        value,
                    )
                    .await?
            }
            _ => unreachable!(),
        }
    }
    Ok(values)
}

/// Button click or select choice
async fn component(
    db: &Database,
    user: &User,
    data: DataCreateInteraction,
) -> Result<(Interaction, v0::Interaction)> {
    let message_id = data
        .message_id
        .as_deref()
        .ok_or_else(|| invalid("message_id is required"))?;
    let custom_id = data
        .custom_id
        .as_deref()
        .ok_or_else(|| invalid("custom_id is required"))?;

    // A channel message by a bot, or this user's own ephemeral answer
    let (application_id, components, source) = match db.fetch_message(message_id).await {
        Ok(message) if message.channel == data.channel_id => (
            // a webhook has no one to answer: clicks go to the bot that made it
            match message.webhook {
                Some(_) => db.fetch_webhook(&message.author).await?.creator_id,
                None => message.author.clone(),
            },
            message.components.unwrap_or_default(),
            InteractionSource::Message {
                id: message.id.clone(),
            },
        ),
        _ => {
            let parent = db
                .fetch_interaction_by_ephemeral_message(&user.id, message_id)
                .await?
                .filter(|parent| parent.channel_id == data.channel_id)
                .ok_or(create_error!(UnknownMessage))?;
            // the first answer or one of its follow-ups
            let message = parent
                .find_ephemeral(message_id)
                .cloned()
                .ok_or(create_error!(UnknownMessage))?;
            (
                parent.application_id.clone(),
                message.components,
                InteractionSource::Ephemeral {
                    interaction_id: parent.id.clone(),
                    message_id: message.id,
                },
            )
        }
    };
    if data
        .application_id
        .as_ref()
        .is_some_and(|id| id != &application_id)
    {
        return Err(create_error!(UnknownApplication));
    }

    let scope = scope(db, user, &data.channel_id, &application_id, false, true).await?;

    let component = find_component(&components, custom_id)
        .ok_or_else(|| invalid("this component no longer exists"))?;
    if component.is_disabled() {
        return Err(create_error!(InvalidOperation));
    }
    if let Component::Button {
        style: v0::ButtonStyle::Link,
        ..
    } = component
    {
        return Err(create_error!(InvalidOperation));
    }
    if component.is_form_only() {
        return Err(create_error!(InvalidOperation));
    }
    check_restriction(component, user, &scope.member)?;

    let mut context = OptionContext::new(db, user, &scope.bot_user, scope.server.as_ref());
    let values =
        check_select_values(component, data.values.unwrap_or_default(), &mut context).await?;

    let (mut interaction, token) = Interaction::new(
        InteractionType::Component,
        scope.bot.id.clone(),
        user.id.clone(),
        scope.server.as_ref().map(|server| server.id.clone()),
        scope.channel.id().to_string(),
        data.nonce,
    );
    let ephemeral = matches!(source, InteractionSource::Ephemeral { .. });
    interaction.source = Some(source);

    let mut payload = payload(db, user, &scope, &interaction, token, data.locale).await;
    payload.message_id = Some(message_id.to_string());
    payload.message_ephemeral = ephemeral;
    payload.component = Some(InteractionComponentData {
        custom_id: custom_id.to_string(),
        component_type: component.kind_name().to_string(),
        values,
        resolved: Some(context.resolved),
    });
    Ok((interaction, payload))
}

/// Check submitted fields against the form the bot opened.
///
/// Users, roles and channels are resolved and files claimed through
/// `context`, the same way as for command options.
async fn check_modal_fields(
    modal: &v0::Modal,
    fields: Vec<ModalFieldValue>,
    context: &mut OptionContext<'_>,
) -> Result<Vec<ModalFieldValue>> {
    let mut seen = HashSet::new();
    for field in &fields {
        if !seen.insert(field.custom_id.as_str()) {
            return Err(invalid("a field was sent twice"));
        }
        if !modal
            .fields()
            .any(|definition| definition.custom_id() == Some(field.custom_id.as_str()))
        {
            return Err(invalid("unknown field"));
        }
    }

    let mut output = Vec::new();
    for definition in modal.fields() {
        let custom_id = definition.custom_id().unwrap_or_default();
        let given = fields.iter().find(|field| field.custom_id == custom_id);
        match definition {
            Component::TextInput {
                required,
                min_length,
                max_length,
                ..
            } => {
                let value = given.and_then(|field| field.value.clone()).unwrap_or_default();
                let len = value.chars().count();
                if len == 0 {
                    if *required {
                        return Err(invalid("a required field is empty"));
                    }
                } else if len < min_length.unwrap_or(0) as usize
                    || len > max_length.unwrap_or(v0::component_limits::TEXT_INPUT_LENGTH) as usize
                {
                    return Err(invalid("a field has the wrong length"));
                }
                output.push(ModalFieldValue {
                    custom_id: custom_id.to_string(),
                    value: Some(value),
                    values: None,
                    checked: None,
                });
            }
            Component::StringSelect {
                options,
                min_values,
                max_values,
                ..
            } => {
                let values = given.and_then(|field| field.values.clone()).unwrap_or_default();
                let (min, max) = (
                    min_values.unwrap_or(1) as usize,
                    max_values.unwrap_or(1) as usize,
                );
                if values.len() < min
                    || values.len() > max
                    || values.iter().collect::<HashSet<_>>().len() != values.len()
                    || values
                        .iter()
                        .any(|value| !options.iter().any(|option| &option.value == value))
                {
                    return Err(invalid("a select field has invalid values"));
                }
                output.push(ModalFieldValue {
                    custom_id: custom_id.to_string(),
                    value: None,
                    values: Some(values),
                    checked: None,
                });
            }
            Component::UserSelect { .. }
            | Component::RoleSelect { .. }
            | Component::MentionableSelect { .. }
            | Component::ChannelSelect { .. } => {
                let values = given.and_then(|field| field.values.clone()).unwrap_or_default();
                let values = check_select_values(definition, values, context).await?;
                output.push(ModalFieldValue {
                    custom_id: custom_id.to_string(),
                    value: None,
                    values: Some(values),
                    checked: None,
                });
            }
            Component::RadioGroup {
                options, required, ..
            } => {
                let value = given
                    .and_then(|field| field.value.clone())
                    .filter(|value| !value.is_empty());
                match &value {
                    None if *required => return Err(invalid("a required field is empty")),
                    Some(value) if !options.iter().any(|option| &option.value == value) => {
                        return Err(invalid("a choice is not one of the options"))
                    }
                    _ => {}
                }
                output.push(ModalFieldValue {
                    custom_id: custom_id.to_string(),
                    value,
                    values: None,
                    checked: None,
                });
            }
            Component::CheckboxGroup {
                options,
                min_values,
                max_values,
                required,
                ..
            } => {
                let values = given.and_then(|field| field.values.clone()).unwrap_or_default();
                let min = min_values.unwrap_or(if *required { 1 } else { 0 }) as usize;
                let max = max_values.map(usize::from).unwrap_or(options.len());
                if values.len() < min
                    || values.len() > max
                    || values.iter().collect::<HashSet<_>>().len() != values.len()
                    || values
                        .iter()
                        .any(|value| !options.iter().any(|option| &option.value == value))
                {
                    return Err(invalid("a checkbox group has invalid values"));
                }
                output.push(ModalFieldValue {
                    custom_id: custom_id.to_string(),
                    value: None,
                    values: Some(values),
                    checked: None,
                });
            }
            Component::Checkbox { .. } => {
                output.push(ModalFieldValue {
                    custom_id: custom_id.to_string(),
                    value: None,
                    values: None,
                    checked: Some(given.and_then(|field| field.checked).unwrap_or(false)),
                });
            }
            Component::FileUpload {
                min_values,
                max_values,
                required,
                file_types,
                ..
            } => {
                let ids = given.and_then(|field| field.values.clone()).unwrap_or_default();
                let min = min_values.unwrap_or(if *required { 1 } else { 0 }) as usize;
                let max = max_values.unwrap_or(1) as usize;
                if ids.len() < min
                    || ids.len() > max
                    || ids.iter().collect::<HashSet<_>>().len() != ids.len()
                {
                    return Err(invalid("a file field has the wrong number of files"));
                }
                for id in &ids {
                    context.use_attachment("file", id).await?;
                    let file = &context.resolved.attachments[id];
                    if !file_type_allowed(file_types, &file.filename, &file.content_type) {
                        return Err(invalid("a file has a type this form does not accept"));
                    }
                }
                output.push(ModalFieldValue {
                    custom_id: custom_id.to_string(),
                    value: None,
                    values: Some(ids),
                    checked: None,
                });
            }
            _ => return Err(create_error!(InternalError)),
        }
    }
    Ok(output)
}

/// `image`, `video`, `audio` match the file's type, `.ext` its name
fn file_type_allowed(file_types: &[String], filename: &str, content_type: &str) -> bool {
    if file_types.is_empty() {
        return true;
    }
    let name = filename.to_ascii_lowercase();
    file_types.iter().any(|kind| match kind.as_str() {
        "image" | "video" | "audio" => content_type.starts_with(&format!("{kind}/")),
        extension => name.ends_with(&extension.to_ascii_lowercase()),
    })
}

/// A form the bot opened was submitted
async fn modal_submit(
    db: &Database,
    user: &User,
    data: DataCreateInteraction,
) -> Result<(Interaction, v0::Interaction)> {
    let parent_id = data
        .parent_id
        .as_deref()
        .ok_or_else(|| invalid("parent_id is required"))?;
    let parent = db.fetch_interaction(parent_id).await?;
    if parent.user_id != user.id || parent.channel_id != data.channel_id {
        return Err(create_error!(UnknownInteraction));
    }
    if parent.is_expired() {
        return Err(create_error!(InteractionExpired));
    }
    let modal = parent.modal.clone().ok_or(create_error!(InvalidOperation))?;
    if data.custom_id.as_deref() != Some(modal.custom_id.as_str()) {
        return Err(invalid("custom_id does not match the form"));
    }

    let scope = scope(db, user, &data.channel_id, &parent.application_id, false, true).await?;
    let mut context = OptionContext::new(db, user, &scope.bot_user, scope.server.as_ref());
    let fields = check_modal_fields(&modal, data.fields.unwrap_or_default(), &mut context).await?;

    // One submit per form
    if !db.claim_modal_submission(&parent.id).await? {
        return Err(create_error!(InteractionAlreadyAcknowledged));
    }

    let (mut interaction, token) = Interaction::new(
        InteractionType::ModalSubmit,
        scope.bot.id.clone(),
        user.id.clone(),
        scope.server.as_ref().map(|server| server.id.clone()),
        scope.channel.id().to_string(),
        data.nonce,
    );
    // A form opened from a component can update that component's message
    interaction.source = parent.source.clone();

    let mut payload = payload(db, user, &scope, &interaction, token, data.locale).await;
    match &parent.source {
        Some(InteractionSource::Message { id }) => payload.message_id = Some(id.clone()),
        Some(InteractionSource::Ephemeral { message_id, .. }) => {
            payload.message_id = Some(message_id.clone());
            payload.message_ephemeral = true;
        }
        None => {}
    }
    let resolved = context.resolved;
    payload.modal = Some(InteractionModalData {
        custom_id: modal.custom_id,
        fields,
        resolved: (!resolved.users.is_empty()
            || !resolved.roles.is_empty()
            || !resolved.channels.is_empty()
            || !resolved.attachments.is_empty())
        .then_some(resolved),
    });
    Ok((interaction, payload))
}

/// Store the interaction and hand it to the bot only: to its endpoint if it
/// has one, else to its own sessions
async fn dispatch(
    db: &Database,
    amqp: &AMQP,
    interaction: Interaction,
    payload: v0::Interaction,
) -> Result<InteractionAccepted> {
    let bot = db.fetch_bot(&interaction.application_id).await?;
    if let Some(endpoint) = &bot.interactions_endpoint {
        let delivery = interaction_endpoint::prepare(db, &bot, endpoint, &payload).await?;
        db.insert_interaction(&interaction).await?;
        let accepted = accepted(&interaction);
        // the person gets their answer as events, whatever happens here
        let (db, amqp) = (db.clone(), amqp.clone());
        tokio::spawn(delivery.send(db, amqp, interaction));
        return Ok(accepted);
    }

    // Fail fast when no one is connected to answer
    if !revolt_presence::is_online(&interaction.application_id).await {
        return Err(create_error!(ApplicationOffline));
    }

    db.insert_interaction(&interaction).await?;
    EventV1::InteractionCreate {
        interaction: payload,
    }
    .p(interaction.application_id.clone())
    .await;

    Ok(accepted(&interaction))
}

fn check_nonce(data: &DataCreateInteraction) -> Result<()> {
    if data.nonce.is_empty() || data.nonce.len() > interaction_limits::NONCE_LENGTH {
        return Err(invalid("nonce must be 1-64 characters"));
    }
    Ok(())
}

/// # Create Interaction
///
/// Use an application command, a component on a bot's message, or submit a
/// form the bot opened. The server checks every value and permission, then
/// sends the interaction to the bot only. The answer arrives as
/// `InteractionStateUpdate` plus a channel message, a message update,
/// `EphemeralMessageCreate` or `InteractionModal`, for this user.
///
/// Sending the same `nonce` again returns the first interaction.
#[openapi(tag = "Interactions")]
#[post("/", data = "<data>")]
pub async fn create_interaction(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    data: Json<DataCreateInteraction>,
) -> Result<Json<InteractionAccepted>> {
    let data = data.into_inner();
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    check_nonce(&data)?;

    // A retry of a request that already went through
    if let Some(existing) = db.fetch_interaction_by_nonce(&user.id, &data.nonce).await? {
        if existing.kind != data.kind || existing.channel_id != data.channel_id {
            return Err(create_error!(DuplicateNonce));
        }
        return Ok(Json(accepted(&existing)));
    }

    let (interaction, payload) = match data.kind {
        InteractionType::Command => command(db, &user, data).await?,
        InteractionType::Component => component(db, &user, data).await?,
        InteractionType::ModalSubmit => modal_submit(db, &user, data).await?,
        InteractionType::Autocomplete => return Err(create_error!(InvalidOperation)),
    };
    dispatch(db, amqp, interaction, payload).await.map(Json)
}

/// # Request Autocomplete
///
/// Ask the bot for suggestions while the user types into an option with
/// `autocomplete`. The suggestions arrive as `InteractionAutocompleteResult`.
/// Clients should wait for a pause in typing and drop answers to older requests.
#[openapi(tag = "Interactions")]
#[post("/autocomplete", data = "<data>")]
pub async fn request_autocomplete(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    data: Json<DataCreateInteraction>,
) -> Result<Json<InteractionAccepted>> {
    let data = data.into_inner();
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if data.kind != InteractionType::Autocomplete {
        return Err(create_error!(InvalidOperation));
    }
    check_nonce(&data)?;

    let (interaction, payload) = command(db, &user, data).await?;
    dispatch(db, amqp, interaction, payload).await.map(Json)
}
