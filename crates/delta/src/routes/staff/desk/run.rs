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
use revolt_models::v0::{ButtonStyle, Component, ComponentBlock, InteractionType};
use revolt_result::{ErrorType, Result, create_error};
use rocket::{State, serde::json::Json};
use ulid::Ulid;

use super::super::{StaffContext, audited::audited};
use super::actor::{DataDeskInteraction, Expect, desk_actor};
use super::lookup::PAGE_PREFIX;
use super::registry::{self, DeskCommand, DeskCommandTree, DeskInput, Lang, texts};
use super::view::{DeskButton, DeskButtonStyle, DeskView};

const PATH_RUN: &str = "/staff/desk/run";
const PATH_CONFIRM: &str = "/staff/desk/confirm";
/// Custom id prefix of a confirmation button
pub const CONFIRM_PREFIX: &str = "zd:confirm:";

/// # Desk: command tree
///
/// The `/desk` command the bot registers, exactly: the always-offered
/// sub-commands and those of the enabled areas. 404 when the desk is off
/// or the caller is not its bot.
#[get("/desk/commands")]
pub async fn commands(user: User) -> Result<Json<DeskCommandTree>> {
    let settings = super::settings(&user).await?;
    Ok(Json(registry::desk_tree(&settings)))
}

/// The command a page press belongs to, and the page it asks for. The
/// press must name a `/desk` command of the same person and the desk bot,
/// and be made on an answer of that command or of an earlier page press of
/// it. The page number is bounded; anything else is refused.
async fn page_origin(
    db: &Database,
    settings: &FeaturesStaffDesk,
    pressed: &Interaction,
) -> Result<(Interaction, u32)> {
    let refuse = || create_error!(InvalidOperation);
    let rest = pressed
        .custom_id
        .as_deref()
        .and_then(|custom| custom.strip_prefix(PAGE_PREFIX))
        .ok_or_else(refuse)?;
    let (origin_id, page) = rest.rsplit_once(':').ok_or_else(refuse)?;
    let page: u32 = page
        .parse()
        .ok()
        .filter(|p| (1..=1000).contains(p))
        .ok_or_else(refuse)?;
    let origin = db
        .fetch_interaction(origin_id)
        .await
        .map_err(|_| create_error!(InteractionExpired))?;
    if origin.is_expired() {
        return Err(create_error!(InteractionExpired));
    }
    if origin.kind != InteractionType::Command
        || origin.command_name.as_deref() != Some(super::COMMAND)
        || origin.application_id != settings.bot
        || origin.user_id != pressed.user_id
    {
        return Err(refuse());
    }
    let Some(InteractionSource::Ephemeral { interaction_id, .. }) = &pressed.source else {
        return Err(refuse());
    };
    if interaction_id != &origin.id {
        let parent = db
            .fetch_interaction(interaction_id)
            .await
            .map_err(|_| refuse())?;
        let of_origin = format!("{PAGE_PREFIX}{}:", origin.id);
        let same = parent.user_id == pressed.user_id
            && parent.application_id == settings.bot
            && parent
                .custom_id
                .as_deref()
                .is_some_and(|custom| custom.starts_with(&of_origin));
        if !same {
            return Err(refuse());
        }
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
    let (interaction, page) = if pressed.kind == InteractionType::Component {
        page_origin(db, &settings, &pressed).await?
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
    let entry = registry::find(group, name).ok_or_else(|| create_error!(InvalidOperation))?;
    if !entry.offered(&settings) {
        return Err(create_error!(NotFound));
    }
    let input = DeskInput {
        options,
        areas: settings.areas.clone(),
        lang: Lang::from_locale(interaction.locale.as_deref()),
        pending: None,
        page,
        origin: interaction.id.clone(),
    };
    if entry.changes {
        // a changing command has no pages: its card is made once
        if page != 1 {
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
    let mut lines = prepared.lines;
    if entry.irreversible {
        lines.push(texts::IRREVERSIBLE.get(input.lang).to_string());
    }
    lines.push(texts::WITHIN.get(input.lang).to_string());
    let pending = DeskPending {
        id: Ulid::new().to_string(),
        actor: staff.user.id.clone(),
        session: staff.session.id.clone(),
        bot: settings.bot.clone(),
        action: entry.key(),
        target_kind: prepared.target_kind,
        target: prepared.target,
        options: input.options,
        card_interaction: interaction.id.clone(),
        card_message: None,
        title: prepared.title,
        lines,
        locale: interaction.locale.clone(),
        created_at: staff_now(),
        expires_at: DeskPending::expiry_from_now(),
        used: false,
    };
    db.insert_desk_pending(&pending).await?;
    Ok(card(&pending, entry.irreversible))
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
    if !on_the_card(db, &settings, &interaction, &pending, entry.irreversible).await? {
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
    if entry.irreversible {
        let fresh = staff
            .session
            .step_up_until
            .is_some_and(|until| until > staff.now)
            && staff
                .session
                .step_up_at
                .is_some_and(|at| at >= pending.created_at);
        if !fresh {
            let mut record = staff.record("staff.step_up.missing", Some(permission));
            record.summary = Some(staff.path.clone());
            record_platform_audit(db, record, AuditOutcome::Denied).await?;
            return Err(create_error!(NotElevated));
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
    execute(
        db,
        &staff,
        entry,
        &pending,
        &input,
        record,
        |record, change| audited(db, record, change),
    )
    .await
    .map(Json)
}
