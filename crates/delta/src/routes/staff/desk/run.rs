//! The generic desk: the command tree, one dispatcher, and confirmation of
//! changing commands through server-stored pending actions.
//!
//! - `GET /staff/desk/commands`: the `/desk` tree for the enabled areas.
//! - `POST /staff/desk/run`: a `/desk` press. Every Phase 0 check
//!   (`desk_actor`), then the command must be registered exactly as
//!   offered; group, sub-command, options and language come only from the
//!   stored interaction. A reading command answers at once; a changing one
//!   only stores a pending action and answers with its confirmation card.
//! - `POST /staff/desk/confirm`: a press on the card's Confirm button
//!   (`zd:confirm:<pending id>`). Nothing but the id is taken from the
//!   button; the action, target, options and language are the stored ones.
//!
//! The card check: the pending action names the command interaction whose
//! first private answer the card is (`card_interaction`, known to the
//! server when it answers `/run`). A confirmation counts only when the
//! button was pressed on that first answer (never a follow-up), by the same
//! person, and the answer's text is exactly the server's
//! `canonical_render` of the card, with one row holding only the Confirm
//! button (style Danger for an irreversible action). The first press binds
//! the message (`card_message`); a later press (after a step-up) must come
//! from the same message.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures::future::BoxFuture;
use revolt_config::FeaturesStaffDesk;
use revolt_database::{
    AuditOutcome, AuditRecord, Database, DeskPending, Interaction, InteractionSource, User,
    record_platform_audit, staff_now,
};
use revolt_models::v0::{
    ButtonStyle, CommandOptionType, Component, ComponentBlock, InteractionDataOption,
    InteractionOptionValue, InteractionType,
};
use revolt_result::{ErrorType, Result, create_error};
use rocket::{State, serde::json::Json};
use ulid::Ulid;

use super::super::{StaffContext, audited::audited};
use super::actor::{DataDeskInteraction, Expect, desk_actor};
use super::lookup::PAGE_PREFIX;
use super::registry::{self, DeskCommand, DeskCommandTree, DeskInput, Lang, texts};
use super::view::{DeskButton, DeskButtonStyle, DeskView};
use crate::util::on_heap::on_heap;

const PATH_RUN: &str = "/staff/desk/run";
const PATH_CONFIRM: &str = "/staff/desk/confirm";
const PATH_ACT: &str = "/staff/desk/act";
/// Custom id prefix of the review room's buttons and forms
/// (`zd:act:<action>:<id>`)
pub const ACT_PREFIX: &str = "zd:act:";
/// The room's form that edits a bot's profile (`zd:act:bot-edit:<bot id>`)
const EDIT_PREFIX: &str = "zd:act:bot-edit:";
/// Longest description of a bot's profile, as for anyone's profile
const DESCRIPTION_MAX: usize = 2000;
/// Custom id prefix of a confirmation button
pub const CONFIRM_PREFIX: &str = "zd:confirm:";

/// # Desk: command tree
///
/// The `/desk` command the bot registers, exactly: the always-offered
/// sub-commands and those of the enabled areas. 404 when the desk is off
/// or the caller is not its bot.
#[get("/desk/commands")]
pub async fn commands(user: User) -> Result<Json<Vec<DeskCommandTree>>> {
    let settings = super::settings(&user).await?;
    Ok(Json(registry::desk_tree(
        &settings,
        registry::plugins_on().await,
    )))
}

/// The command a page press belongs to, and the page it asks for. The
/// press must name a `/desk` command of the same person and the desk bot
/// (checked before anything else is said about it), be made on an answer
/// of that command or of an earlier page press of it, and ask for a page
/// from 1 to 1000. A refusal gives the reason for the record.
async fn page_origin(
    db: &Database,
    settings: &FeaturesStaffDesk,
    pressed: &Interaction,
) -> std::result::Result<(Interaction, u32), (&'static str, revolt_result::Error)> {
    let refuse = |why: &'static str| (why, create_error!(InvalidOperation));
    let rest = pressed
        .custom_id
        .as_deref()
        .and_then(|custom| custom.strip_prefix(PAGE_PREFIX))
        .ok_or_else(|| refuse("not a page button"))?;
    let (origin_id, page) = rest
        .rsplit_once(':')
        .ok_or_else(|| refuse("not a page button"))?;
    // whose command it is comes first: a stranger learns nothing about it
    let origin = db
        .fetch_interaction(origin_id)
        .await
        .map_err(|_| refuse("not a command of the presser"))?;
    if origin.kind != InteractionType::Command
        || !origin
            .command_name
            .as_deref()
            .is_some_and(registry::is_desk_command)
        || origin.application_id != settings.bot
        || origin.user_id != pressed.user_id
    {
        return Err(refuse("not a command of the presser"));
    }
    let Some(InteractionSource::Ephemeral { interaction_id, .. }) = &pressed.source else {
        return Err(refuse("not pressed on a private answer"));
    };
    if interaction_id != &origin.id {
        let parent = db
            .fetch_interaction(interaction_id)
            .await
            .map_err(|_| refuse("not pressed on an answer of that command"))?;
        let of_origin = format!("{PAGE_PREFIX}{}:", origin.id);
        let same = parent.user_id == pressed.user_id
            && parent.application_id == settings.bot
            && parent
                .custom_id
                .as_deref()
                .is_some_and(|custom| custom.starts_with(&of_origin));
        if !same {
            return Err(refuse("not pressed on an answer of that command"));
        }
    }
    let page: u32 = page
        .parse()
        .ok()
        .filter(|p| (1..=1000).contains(p))
        .ok_or_else(|| refuse("page out of range"))?;
    if origin.is_expired() {
        return Err(("command expired", create_error!(InteractionExpired)));
    }
    Ok((origin, page))
}

/// # Desk: run a command
///
/// From any `/desk` command of the registry, or a page button of a list it
/// answered (`zd:page:<command>:<page>`). Reading commands answer with
/// their view; changing ones with a confirmation card.
#[post("/desk/run", data = "<data>")]
pub async fn run(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskInteraction>,
) -> Result<Json<DeskView>> {
    let (staff, pressed) = desk_actor(
        db,
        &user,
        &data,
        Expect::CommandOrButton(PAGE_PREFIX),
        PATH_RUN,
    )
    .await?;
    let settings = super::settings(&user).await?;
    // a page press stands for its command: every check below is on that
    let paging = pressed.kind == InteractionType::Component;
    let (interaction, page) = if paging {
        match page_origin(db, &settings, &pressed).await {
            Ok(found) => found,
            Err((why, error)) => {
                let mut record = staff.record("staff.desk.page", None);
                record.summary = Some(format!("desk: {why}"));
                record_platform_audit(db, record, AuditOutcome::Denied).await?;
                return Err(error);
            }
        }
    } else {
        (pressed, 1)
    };
    if !registry::registered_as_offered(db, &settings, &interaction).await {
        let mut record = staff.record("staff.desk.command", None);
        record.summary = Some("desk: command not as offered".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    }
    // `login`, `logout` and `stepup` have their own routes
    let (group, name, options) =
        registry::chosen(&interaction).ok_or_else(|| create_error!(InvalidOperation))?;
    let entry = registry::find(
        interaction.command_name.as_deref().unwrap_or_default(),
        group,
        name,
    )
    .ok_or_else(|| create_error!(InvalidOperation))?;
    if !entry.offered(&settings) {
        return Err(create_error!(NotFound));
    }
    // the presser may use this command at all: refused (and recorded)
    // before anyone is looked up
    if let Some(permission) = entry.permission {
        staff.require_protective(db, permission).await?;
    }
    // people are typed as an id or name#tag: found here, before the card
    let options = registry::resolve_people(db, entry, options).await?;
    let input = DeskInput {
        options,
        areas: settings.areas.clone(),
        lang: Lang::from_locale(interaction.locale.as_deref()),
        pending: None,
        page,
        origin: interaction.id.clone(),
    };
    if entry.changes {
        // a changing command has no pages: its card is made once, from
        // the command itself, never from a button
        if paging {
            let mut record = staff.record("staff.desk.page", None);
            record.summary = Some("desk: a changing command has no pages".to_string());
            record_platform_audit(db, record, AuditOutcome::Denied).await?;
            return Err(create_error!(InvalidOperation));
        }
        return propose(db, &staff, &settings, &interaction, entry, input)
            .await
            .map(Json);
    }
    registry::dispatch(db, &staff, entry, &input)
        .await
        .map(|view| Json(view.finish()))
}

/// What a review room press stands for: the desk command (command, group,
/// name) and its options. Only these actions exist; the request id is the
/// only value taken from the button, and the note only from the form's own
/// field. Anything else is refused.
pub(super) fn room_command(
    custom_id: &str,
    note: &str,
) -> Option<(&'static str, Option<&'static str>, &'static str, Vec<InteractionDataOption>)> {
    let rest = custom_id.strip_prefix(ACT_PREFIX)?;
    let (action, id) = rest.split_once(':')?;
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let text = |name: &str, value: &str| InteractionDataOption {
        name: name.to_string(),
        kind: CommandOptionType::String,
        value: Some(InteractionOptionValue::String(value.to_string())),
        options: None,
    };
    let note = note.trim();
    match action {
        // a bot asking for the verified mark
        "verify-approve" | "verify-deny" => {
            let deny = action == "verify-deny";
            if deny && note.is_empty() {
                return None;
            }
            let mut options = vec![
                text("request", id),
                text("decision", if deny { "deny" } else { "approve" }),
            ];
            if !note.is_empty() {
                options.push(text("note", note));
            }
            Some(("desk-review", Some("verification"), "decide", options))
        }
        // a support ticket answered from its card, closed with it or not
        "ticket-reply" | "ticket-reply-close" => {
            if note.is_empty() {
                return None;
            }
            let options = vec![
                text("ticket", id),
                text("text", note),
                InteractionDataOption {
                    name: "close".to_string(),
                    kind: CommandOptionType::Boolean,
                    value: Some(InteractionOptionValue::Boolean(action == "ticket-reply-close")),
                    options: None,
                },
            ];
            Some(("desk-support", Some("ticket"), "reply", options))
        }
        // a design waiting in the gallery's queue; refusing needs a reason
        "design-approve" | "design-reject" | "design-suspend" => {
            let decision = &action["design-".len()..];
            if decision != "approve" && note.is_empty() {
                return None;
            }
            let mut options = vec![text("design", id), text("action", decision)];
            if !note.is_empty() {
                options.push(text("reason", note));
            }
            Some(("desk-review", Some("design"), "review", options))
        }
        _ => None,
    }
}

/// A reviewer fixes a bot's public description and links (one per line)
/// before deciding: the same checks as anyone's own profile (clean links,
/// the platform's word list), for staff who may decide reviews. Recorded in
/// the platform audit log first, with what it was before.
async fn edit_bot_profile(
    db: &Database,
    staff: &StaffContext,
    bot_id: &str,
    description: &str,
    links: &str,
) -> Result<DeskView> {
    staff
        .require_protective(db, revolt_database::StaffPermission::ReviewsDecide)
        .await?;
    if bot_id.is_empty() || bot_id.len() > 64 || !bot_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(create_error!(InvalidOperation));
    }
    let bot = db.fetch_bot(bot_id).await?;
    if bot.owner == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    if description.chars().count() > DESCRIPTION_MAX {
        return Err(create_error!(FailedValidation {
            error: format!("the description is longer than {DESCRIPTION_MAX} characters")
        }));
    }
    let raw: Vec<String> = links
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let links = crate::routes::users::profile_fields::clean_links(&raw)?;
    for text in std::iter::once(description.to_string()).chain(links.iter().cloned()) {
        if let Some(category) = revolt_database::check_platform_words(db, &text).await? {
            return Err(create_error!(ProfileTextFiltered {
                category: format!("{category:?}")
            }));
        }
    }
    let mut user = db.fetch_user(&bot.id).await?;
    let mut profile = user.profile.clone().unwrap_or_default();
    let before = (profile.content.clone().unwrap_or_default(), profile.links.join(" "));
    profile.content = (!description.is_empty()).then(|| description.to_string());
    profile.links = links;
    let mut record = staff.record(
        "staff.bot.profile.edit",
        Some(revolt_database::StaffPermission::ReviewsDecide),
    );
    record.target_kind = Some("bot".to_string());
    record.target_id = Some(bot.id.clone());
    record.summary = Some(format!(
        "desk: profile edited (before: {} chars, links {})",
        before.0.chars().count(),
        if before.1.is_empty() { "none" } else { before.1.as_str() }
    ));
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    user.update(
        db,
        revolt_database::PartialUser {
            profile: Some(profile),
            ..Default::default()
        },
        vec![],
    )
    .await?;
    Ok(DeskView::new("Bot profile updated")
        .line(format!("The description and links of <@{}> are saved.", bot.id))
        .finish())
}

/// # Desk: a press in the review room
///
/// A button (or the form of one) on a card the desk bot posted in its
/// review room: `zd:act:<action>:<id>`. Every check of `/run` applies (the
/// desk's place, the presser's staff grant and desk session), the press
/// stands for one desk command with fixed options, and the answer is that
/// command's confirmation card: nothing changes before Confirm.
#[post("/desk/act", data = "<data>")]
pub async fn act(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskInteraction>,
) -> Result<Json<DeskView>> {
    let (staff, pressed) =
        desk_actor(db, &user, &data, Expect::PressPrefix(ACT_PREFIX), PATH_ACT).await?;
    let settings = super::settings(&user).await?;
    // the bot's profile, edited from the room's form: done at once, recorded
    if let Some(bot_id) = pressed
        .custom_id
        .as_deref()
        .and_then(|custom| custom.strip_prefix(EDIT_PREFIX))
    {
        if pressed.kind != InteractionType::ModalSubmit {
            return Err(create_error!(InvalidOperation));
        }
        return edit_bot_profile(
            db,
            &staff,
            bot_id,
            super::actor::field(&pressed, "description"),
            super::actor::field(&pressed, "links"),
        )
        .await
        .map(Json);
    }
    let note = super::actor::field(&pressed, "note");
    let Some((command, group, name, options)) = pressed
        .custom_id
        .as_deref()
        .and_then(|custom| room_command(custom, note))
    else {
        let mut record = staff.record("staff.desk.act", None);
        record.summary = Some("desk: not a review room action".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    };
    let entry =
        registry::find(command, group, name).ok_or_else(|| create_error!(InvalidOperation))?;
    if !entry.offered(&settings) {
        return Err(create_error!(NotFound));
    }
    if let Some(permission) = entry.permission {
        staff.require_protective(db, permission).await?;
    }
    let input = DeskInput {
        options,
        areas: settings.areas.clone(),
        lang: Lang::from_locale(pressed.locale.as_deref()),
        pending: None,
        page: 1,
        origin: pressed.id.clone(),
    };
    propose(db, &staff, &settings, &pressed, entry, input)
        .await
        .map(Json)
}

/// The confirmation card of a pending action, as the bot must show it
fn card(pending: &DeskPending, irreversible: bool) -> DeskView {
    let lang = Lang::from_locale(pending.locale.as_deref());
    DeskView {
        content: String::new(),
        title: pending.title.clone(),
        fields: vec![],
        lines: pending.lines.clone(),
        page: None,
        pages: None,
        buttons: vec![DeskButton {
            label: texts::CONFIRM.get(lang).to_string(),
            action_id: format!("{CONFIRM_PREFIX}{}", pending.id),
            style: if irreversible {
                DeskButtonStyle::Danger
            } else {
                DeskButtonStyle::Primary
            },
        }],
    }
    .finish()
}

/// A changing command: checks the presser may do it, stores what was asked
/// and answers with the confirmation card. Nothing changes yet, and nothing
/// is recorded (the confirmation and refusals are).
async fn propose(
    db: &Database,
    staff: &StaffContext,
    settings: &FeaturesStaffDesk,
    interaction: &Interaction,
    entry: &DeskCommand,
    input: DeskInput,
) -> Result<DeskView> {
    if let Some(permission) = entry.permission {
        staff.require_protective(db, permission).await?;
    }
    let prepare = entry
        .prepare
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let prepared = prepare(db, staff, &input).await?;
    let irreversible = entry.irreversible || prepared.irreversible;
    let mut lines = prepared.lines;
    if entry
        .options
        .iter()
        .any(|o| matches!(o.name, "reason" | "note" | "public_note"))
    {
        lines.push(texts::WORDS_KEPT.get(input.lang).to_string());
    }
    if prepared.permanent {
        lines.push(texts::PERMANENT.get(input.lang).to_string());
    }
    if irreversible {
        lines.push(texts::FRESH_CODE.get(input.lang).to_string());
    }
    lines.push(texts::WITHIN.get(input.lang).to_string());
    // a card is never cut: what the person confirms is all of it
    if !super::view::fits(&prepared.title, &lines) {
        return Err(create_error!(FailedValidation {
            error: texts::TOO_LONG.to_string()
        }));
    }
    let pending = DeskPending {
        id: Ulid::new().to_string(),
        actor: staff.user.id.clone(),
        session: staff.session.id.clone(),
        bot: settings.bot.clone(),
        action: entry.key(),
        target_kind: prepared.target_kind,
        target: prepared.target,
        options: input.options.into_iter().chain(prepared.stored).collect(),
        card_interaction: interaction.id.clone(),
        card_message: None,
        title: prepared.title,
        lines,
        locale: interaction.locale.clone(),
        created_at: staff_now(),
        expires_at: DeskPending::expiry_from_now(),
        used: false,
        irreversible,
    };
    db.insert_desk_pending(&pending).await?;
    Ok(card(&pending, pending.irreversible))
}

/// Whether the button was pressed on the card the server made for this
/// pending action (see the module notes); binds the card message at the
/// first press
async fn on_the_card(
    db: &Database,
    settings: &FeaturesStaffDesk,
    interaction: &Interaction,
    pending: &DeskPending,
    irreversible: bool,
) -> Result<bool> {
    let Some(InteractionSource::Ephemeral {
        interaction_id,
        message_id,
    }) = &interaction.source
    else {
        return Ok(false);
    };
    if interaction_id != &pending.card_interaction {
        return Ok(false);
    }
    let Ok(parent) = db.fetch_interaction(interaction_id).await else {
        return Ok(false);
    };
    if parent.application_id != settings.bot || parent.user_id != interaction.user_id {
        return Ok(false);
    }
    // only the first answer: a follow-up is never the card
    let Some(message) = parent
        .ephemeral_message
        .as_ref()
        .filter(|m| &m.id == message_id)
    else {
        return Ok(false);
    };
    let expected = card(pending, irreversible);
    if !message.embeds.is_empty() || message.content.as_deref() != Some(expected.content.as_str()) {
        return Ok(false);
    }
    let want = &expected.buttons[0];
    let only_confirm = match message.components.as_slice() {
        [ComponentBlock::ActionRow { components }] => matches!(
            components.as_slice(),
            [Component::Button { style, label: Some(label), custom_id: Some(custom_id), .. }]
                if label == &want.label
                    && custom_id == &want.action_id
                    && (!irreversible || *style == ButtonStyle::Danger)
        ),
        _ => false,
    };
    if !only_confirm {
        return Ok(false);
    }
    db.bind_desk_pending_card(&pending.id, message_id).await
}

/// Runs a claimed pending action's handler through `audit` (production:
/// [`audited`], Success first, Failed on error). When the action did not
/// run (the first record could not be written) or its core asked for a
/// step-up, the claim is given back: the same card works again.
pub(super) async fn execute<'a, A, AF>(
    db: &'a Database,
    staff: &'a StaffContext,
    entry: &'a DeskCommand,
    pending: &DeskPending,
    input: &'a DeskInput,
    record: AuditRecord,
    audit: A,
) -> Result<DeskView>
where
    A: FnOnce(AuditRecord, BoxFuture<'a, Result<DeskView>>) -> AF,
    AF: std::future::Future<Output = Result<DeskView>>,
{
    let ran = Arc::new(AtomicBool::new(false));
    let started = ran.clone();
    let change: BoxFuture<'a, Result<DeskView>> = Box::pin(async move {
        started.store(true, Ordering::SeqCst);
        (entry.handler)(db, staff, input).await
    });
    match audit(record, change).await {
        Ok(view) => Ok(view.finish()),
        Err(error) => {
            if !ran.load(Ordering::SeqCst) || matches!(error.error_type, ErrorType::NotElevated) {
                db.release_desk_pending(&pending.id).await?;
            }
            Err(error)
        }
    }
}

/// # Desk: confirm a changing command
///
/// From the Confirm button of a card `/run` made. Same person, same desk
/// session, within ten minutes, once, on that card; irreversible actions
/// need a step-up made after the card (else `NotElevated`, and the same
/// card works after it). Executes the stored action.
#[post("/desk/confirm", data = "<data>")]
pub async fn confirm(
    db: &State<Database>,
    user: User,
    data: Json<DataDeskInteraction>,
) -> Result<Json<DeskView>> {
    let (staff, interaction) = desk_actor(
        db,
        &user,
        &data,
        Expect::ButtonPrefix(CONFIRM_PREFIX),
        PATH_CONFIRM,
    )
    .await?;
    let settings = super::settings(&user).await?;
    let id = interaction
        .custom_id
        .as_deref()
        .and_then(|custom| custom.strip_prefix(CONFIRM_PREFIX))
        .unwrap_or_default()
        .to_string();

    // unknown and someone else's answer alike: nothing tells them apart
    let Some(pending) = db.fetch_desk_pending(&id).await? else {
        let mut record = staff.record("staff.desk.confirm", None);
        record.summary = Some("desk: no such pending action".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(InteractionExpired));
    };
    let entry = registry::find_key(&pending.action);
    let base = || {
        let mut record = staff.record(
            &format!("staff.desk.{}", pending.action),
            entry.and_then(|e| e.permission),
        );
        record.target_kind = pending.target_kind.clone();
        record.target_id = pending.target.clone();
        record
    };
    let deny = |why: &str| -> AuditRecord {
        let mut record = base();
        record.summary = Some(format!("desk: {why}"));
        record
    };

    if pending.actor != staff.user.id || pending.bot != settings.bot {
        record_platform_audit(db, deny("not the person who asked"), AuditOutcome::Denied).await?;
        return Err(create_error!(InteractionExpired));
    }
    if pending.session != staff.session.id {
        record_platform_audit(
            db,
            deny("asked on another desk session"),
            AuditOutcome::Denied,
        )
        .await?;
        return Err(create_error!(NotPrivileged));
    }
    if pending.is_expired() {
        record_platform_audit(db, deny("confirmation expired"), AuditOutcome::Denied).await?;
        return Err(create_error!(InteractionExpired));
    }
    if pending.used {
        record_platform_audit(db, deny("already confirmed"), AuditOutcome::Denied).await?;
        return Err(create_error!(InteractionExpired));
    }
    let Some(entry) = entry.filter(|e| e.changes && e.offered(&settings)) else {
        record_platform_audit(db, deny("action no longer offered"), AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    };
    let irreversible = entry.irreversible || pending.irreversible;
    if !on_the_card(db, &settings, &interaction, &pending, irreversible).await? {
        record_platform_audit(
            db,
            deny("not pressed on the card the server made"),
            AuditOutcome::Denied,
        )
        .await?;
        return Err(create_error!(InvalidOperation));
    }
    // the console's full check, again, on this press (step-up list included)
    let permission = entry
        .permission
        .ok_or_else(|| create_error!(InvalidOperation))?;
    staff.require(db, permission).await?;
    // irreversible: a step-up made after the card was made; the pending
    // action stays usable
    if irreversible {
        let fresh = staff
            .session
            .step_up_until
            .is_some_and(|until| until > staff.now)
            && staff
                .session
                .step_up_at
                .is_some_and(|at| at > pending.created_at);
        if !fresh {
            let mut record = staff.record("staff.step_up.missing", Some(permission));
            record.summary = Some(staff.path.clone());
            record_platform_audit(db, record, AuditOutcome::Denied).await?;
            return Err(create_error!(NotElevated));
        }
    }
    // the entry's own last check, on the stored options
    if let Some(verify) = entry.verify {
        let check = DeskInput {
            options: pending.options.clone(),
            areas: settings.areas.clone(),
            lang: Lang::from_locale(pending.locale.as_deref()),
            pending: Some(pending.id.clone()),
            page: 1,
            origin: pending.card_interaction.clone(),
        };
        if let Some(why) = verify(db, &staff, &check).await? {
            record_platform_audit(db, deny(why), AuditOutcome::Denied).await?;
            return Err(create_error!(InvalidOperation));
        }
    }
    // once, and only before it expires: a second press (or a race) finds it used
    if !db.claim_desk_pending(&pending.id, staff_now()).await? {
        record_platform_audit(db, deny("already confirmed"), AuditOutcome::Denied).await?;
        return Err(create_error!(InteractionExpired));
    }

    let input = DeskInput {
        options: pending.options.clone(),
        areas: settings.areas.clone(),
        lang: Lang::from_locale(pending.locale.as_deref()),
        pending: Some(pending.id.clone()),
        page: 1,
        origin: pending.card_interaction.clone(),
    };
    let mut record = base();
    record.summary = Some(format!("desk: confirmed {}", pending.action));
    let db: &Database = db;
    // on the heap: the change is the deepest call of the API (see on_heap)
    on_heap(|| {
        execute(
            db,
            &staff,
            entry,
            &pending,
            &input,
            record,
            |record, change| audited(db, record, change),
        )
    })
    .await
    .map(Json)
}

#[cfg(test)]
mod room_tests {
    use super::*;

    fn value(options: &[InteractionDataOption], name: &str) -> Option<String> {
        options.iter().find(|o| o.name == name).and_then(|o| match &o.value {
            Some(InteractionOptionValue::String(v)) => Some(v.clone()),
            _ => None,
        })
    }

    #[test]
    fn a_room_press_stands_for_the_desk_decision_with_fixed_options() {
        let (command, group, name, options) =
            room_command("zd:act:verify-approve:01ABCDEF", "").unwrap();
        assert_eq!((command, group, name), ("desk-review", Some("verification"), "decide"));
        assert_eq!(value(&options, "request").as_deref(), Some("01ABCDEF"));
        assert_eq!(value(&options, "decision").as_deref(), Some("approve"));
        assert_eq!(value(&options, "note"), None);

        let (_, _, _, options) = room_command("zd:act:verify-deny:01ABCDEF", " not ready ").unwrap();
        assert_eq!(value(&options, "decision").as_deref(), Some("deny"));
        assert_eq!(value(&options, "note").as_deref(), Some("not ready"));
    }

    #[test]
    fn anything_else_is_refused() {
        // a denial needs the owner's note
        assert!(room_command("zd:act:verify-deny:01ABCDEF", "  ").is_none());
        // only the known actions, and only a plain id
        assert!(room_command("zd:act:ban:01ABCDEF", "").is_none());
        assert!(room_command("zd:act:verify-approve:", "").is_none());
        assert!(room_command("zd:act:verify-approve:01ABC:extra", "").is_none());
        assert!(room_command("zd:act:verify-approve:../x", "").is_none());
        assert!(room_command("zd:confirm:01ABCDEF", "").is_none());
        // a ticket answer needs its text, a refused design its reason
        assert!(room_command("zd:act:ticket-reply:01ABCDEF", " ").is_none());
        assert!(room_command("zd:act:design-reject:01ABCDEF", "").is_none());
        assert!(room_command("zd:act:design-suspend:01ABCDEF", "").is_none());
        assert!(room_command("zd:act:design-delete:01ABCDEF", "x").is_none());
    }

    #[test]
    fn tickets_and_designs_stand_for_their_desk_commands() {
        let (command, group, name, options) =
            room_command("zd:act:ticket-reply-close:01ABCDEF", " Fixed, thanks ").unwrap();
        assert_eq!((command, group, name), ("desk-support", Some("ticket"), "reply"));
        assert_eq!(value(&options, "ticket").as_deref(), Some("01ABCDEF"));
        assert_eq!(value(&options, "text").as_deref(), Some("Fixed, thanks"));
        let close = options.iter().find(|o| o.name == "close").and_then(|o| match o.value {
            Some(InteractionOptionValue::Boolean(v)) => Some(v),
            _ => None,
        });
        assert_eq!(close, Some(true));
        let (_, _, _, options) = room_command("zd:act:ticket-reply:01ABCDEF", "hi").unwrap();
        assert!(options.iter().any(|o| o.name == "close"
            && matches!(o.value, Some(InteractionOptionValue::Boolean(false)))));

        let (command, group, name, options) =
            room_command("zd:act:design-approve:01ABCDEF", "").unwrap();
        assert_eq!((command, group, name), ("desk-review", Some("design"), "review"));
        assert_eq!(value(&options, "design").as_deref(), Some("01ABCDEF"));
        assert_eq!(value(&options, "action").as_deref(), Some("approve"));
        assert_eq!(value(&options, "reason"), None);
        let (_, _, _, options) = room_command("zd:act:design-reject:01ABCDEF", "blurry").unwrap();
        assert_eq!(value(&options, "action").as_deref(), Some("reject"));
        assert_eq!(value(&options, "reason").as_deref(), Some("blurry"));
    }
}
