//! The generic desk: the command tree, one dispatcher, and confirmation of
//! changing commands through server-stored pending actions.
//!
//! - `GET /staff/desk/commands`: the `/desk` tree for the enabled areas.
//! - `POST /staff/desk/run`: a `/desk` press. Every Phase 0 check
//!   (`desk_actor`), then the command must be registered exactly as
//!   offered; group, sub-command and options come only from the stored
//!   interaction. A reading command answers at once; a changing one only
//!   stores a pending action and answers with its confirmation card.
//! - `POST /staff/desk/confirm`: a press on the card's Confirm button
//!   (`zd:confirm:<pending id>`). Nothing but the id is taken from the
//!   button; the action, target and options are the stored ones.
//!
//! The card check: the pending action names the command interaction whose
//! private answer the card is (`card_interaction`, known to the server
//! when it answers `/run`). A confirmation counts only when the button was
//! pressed on a private message of exactly that interaction, by the same
//! person, and that message still shows the server's title and every
//! consequence line, with one row holding only the Confirm button. The
//! first press binds the message (`card_message`); a later press (after a
//! step-up) must come from the same message.

use revolt_database::{
    record_platform_audit, AuditOutcome, AuditRecord, Database, DeskPending, Interaction,
    InteractionSource, User,
};
use revolt_models::v0::{Component, ComponentBlock};
use revolt_result::{create_error, ErrorType, Result};
use rocket::{serde::json::Json, State};
use ulid::Ulid;

use super::super::StaffContext;
use super::actor::{desk_actor, DataDeskInteraction, Expect};
use super::registry::{self, DeskCommand, DeskCommandTree, DeskInput};
use super::view::{DeskButton, DeskButtonStyle, DeskView};
use revolt_config::FeaturesStaffDesk;

const PATH_RUN: &str = "/staff/desk/run";
const PATH_CONFIRM: &str = "/staff/desk/confirm";
/// Custom id prefix of a confirmation button
pub const CONFIRM_PREFIX: &str = "zd:confirm:";
/// The confirmation button's label; the card must still carry it
pub const CONFIRM_LABEL: &str = "Confirm";

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

/// # Desk: run a command
///
/// From any `/desk` command of the registry. Reading commands answer with
/// their view; changing ones with a confirmation card.
#[post("/desk/run", data = "<data>")]
pub async fn run(db: &State<Database>, user: User, data: Json<DataDeskInteraction>) -> Result<Json<DeskView>> {
    let (staff, interaction) = desk_actor(db, &user, &data, Expect::AnyCommand, PATH_RUN).await?;
    let settings = super::settings(&user).await?;
    if !registry::registered_as_offered(db, &settings, &interaction).await {
        let mut record = staff.record("staff.desk.command", None);
        record.summary = Some("desk: command not as offered".to_string());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    }
    // `login`, `logout` and `stepup` have their own routes
    let (group, name, options) = registry::chosen(&interaction).ok_or_else(|| create_error!(InvalidOperation))?;
    let entry = registry::find(group, name).ok_or_else(|| create_error!(InvalidOperation))?;
    if !entry.offered(&settings) {
        return Err(create_error!(NotFound));
    }
    let input = DeskInput {
        options,
        areas: settings.areas.clone(),
    };
    if entry.changes {
        return propose(db, &staff, &settings, &interaction, entry, input).await.map(Json);
    }
    registry::dispatch(db, &staff, entry, &input).await.map(Json)
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
    let prepare = entry.prepare.ok_or_else(|| create_error!(InvalidOperation))?;
    let prepared = prepare(db, staff, &input).await?;
    let mut lines = prepared.lines;
    if entry.irreversible {
        lines.push("This cannot be undone. You will be asked for a fresh code.".to_string());
    }
    lines.push("Press Confirm within 10 minutes.".to_string());
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
        created_at: iso8601_timestamp::Timestamp::now_utc(),
        expires_at: DeskPending::expiry_from_now(),
        used: false,
    };
    db.insert_desk_pending(&pending).await?;
    Ok(DeskView {
        title: pending.title.clone(),
        fields: vec![],
        lines: pending.lines.clone(),
        page: None,
        pages: None,
        buttons: vec![DeskButton {
            label: CONFIRM_LABEL.to_string(),
            action_id: format!("{CONFIRM_PREFIX}{}", pending.id),
            style: if entry.irreversible {
                DeskButtonStyle::Danger
            } else {
                DeskButtonStyle::Primary
            },
        }],
    })
}

/// Whether the button was pressed on the card the server made for this
/// pending action (see the module notes); binds the card message at the
/// first press
async fn on_the_card(
    db: &Database,
    settings: &FeaturesStaffDesk,
    interaction: &Interaction,
    pending: &DeskPending,
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
    let Some(message) = parent.find_ephemeral(message_id) else {
        return Ok(false);
    };
    let content = message.content.as_deref().unwrap_or_default();
    if !message.embeds.is_empty()
        || !content.contains(pending.title.as_str())
        || pending.lines.iter().any(|line| !content.contains(line.as_str()))
    {
        return Ok(false);
    }
    let want = format!("{CONFIRM_PREFIX}{}", pending.id);
    let only_confirm = match message.components.as_slice() {
        [ComponentBlock::ActionRow { components }] => matches!(
            components.as_slice(),
            [Component::Button { label: Some(label), custom_id: Some(custom_id), .. }]
                if label == CONFIRM_LABEL && custom_id == &want
        ),
        _ => false,
    };
    if !only_confirm {
        return Ok(false);
    }
    db.bind_desk_pending_card(&pending.id, message_id).await
}

/// # Desk: confirm a changing command
///
/// From the Confirm button of a card `/run` made. Same person, same desk
/// session, within ten minutes, once, on that card; irreversible actions
/// need a step-up (then `NotElevated`, and the same card works again after
/// it). Executes the stored action.
#[post("/desk/confirm", data = "<data>")]
pub async fn confirm(db: &State<Database>, user: User, data: Json<DataDeskInteraction>) -> Result<Json<DeskView>> {
    let (staff, interaction) = desk_actor(db, &user, &data, Expect::ButtonPrefix(CONFIRM_PREFIX), PATH_CONFIRM).await?;
    let settings = super::settings(&user).await?;
    let id = interaction
        .custom_id
        .as_deref()
        .and_then(|custom| custom.strip_prefix(CONFIRM_PREFIX))
        .unwrap_or_default()
        .to_string();

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
        return Err(create_error!(NotPrivileged));
    }
    if pending.session != staff.session.id {
        record_platform_audit(db, deny("asked on another desk session"), AuditOutcome::Denied).await?;
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
    if !on_the_card(db, &settings, &interaction, &pending).await? {
        record_platform_audit(db, deny("not pressed on the card the server made"), AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    }
    let Some(entry) = entry.filter(|e| e.changes && e.offered(&settings)) else {
        record_platform_audit(db, deny("action no longer offered"), AuditOutcome::Denied).await?;
        return Err(create_error!(InvalidOperation));
    };
    // the console's permission check, again, on this press
    if let Some(permission) = entry.permission {
        staff.require_protective(db, permission).await?;
    }
    // irreversible: always a fresh code; the pending action stays usable
    if entry.irreversible {
        let permission = entry.permission.ok_or_else(|| create_error!(InvalidOperation))?;
        staff.require_step_up(db, permission).await?;
    }
    // once: a second press (or a race) finds it used
    if !db.claim_desk_pending(&pending.id).await? {
        record_platform_audit(db, deny("already confirmed"), AuditOutcome::Denied).await?;
        return Err(create_error!(InteractionExpired));
    }

    let input = DeskInput {
        options: pending.options.clone(),
        areas: settings.areas.clone(),
    };
    match (entry.handler)(db, &staff, &input).await {
        Ok(view) => {
            let mut record = base();
            record.summary = Some(format!("desk: confirmed {}", pending.action));
            record_platform_audit(db, record, AuditOutcome::Success).await?;
            Ok(Json(view))
        }
        // the core asked for a step-up (and recorded it): nothing ran, the
        // card works again after the step-up
        Err(error) if matches!(error.error_type, ErrorType::NotElevated) => {
            db.release_desk_pending(&pending.id).await?;
            Err(error)
        }
        Err(error) => {
            let mut record = base();
            record.summary = Some(
                format!("desk: {} failed: {:?}", pending.action, error.error_type)
                    .chars()
                    .take(200)
                    .collect(),
            );
            let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
            Err(error)
        }
    }
}
