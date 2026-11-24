//! The desk actor: turns a press the bot hands back into the console's own
//! [`StaffContext`], after every check, in this order:
//!
//! 1. the desk is on and the caller is its bot, else 404;
//! 2. the interaction is genuine (token, application, not expired), else
//!    `UnknownInteraction` or `InteractionExpired`;
//! 3. its kind and name are the ones the route expects, else
//!    `InvalidOperation`;
//! 4. it is used once (`InteractionExpired` the second time);
//! 5. it happened in a configured place (or a DM with the bot when
//!    `allow_dms`), else Denied `staff.desk.place` and `NotPrivileged`;
//! 6. the presser is a person with an active staff grant now, else Denied
//!    `staff.desk.no_grant`, their desk sessions revoked, and
//!    `NotPrivileged`;
//! 7. they have an open desk session, not idle, opened from the ordinary
//!    session (sign-in) this press came from, which still exists; else
//!    `InvalidSession` (the bot then offers sign-in). Skipped only by the
//!    sign-in route.
//!
//! The place and the presser come from the stored interaction, never from
//! anything the bot sends.

use revolt_config::FeaturesStaffDesk;
use revolt_database::{
    record_platform_audit, staff_now, AuditOutcome, AuditRecord, Channel, Database, Interaction,
    StaffGrant, User, STAFF_SESSION_IDLE_MS,
};
use revolt_models::v0::{CommandOptionType, InteractionType};
use revolt_result::{create_error, ErrorType, Result};
use serde::{Deserialize, Serialize};

use super::super::{guard::TOUCH_EVERY_MS, StaffContext};
use super::{settings, COMMAND};

/// The interaction the bot hands back: nothing else is taken from the bot
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DataDeskInteraction {
    pub interaction_id: String,
    pub token: String,
}

/// What a route accepts
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect<'a> {
    /// `/desk <sub-command>`
    Command(&'a str),
    /// Any `/desk` command (the generic `/staff/desk/run`, which then reads
    /// the stored group and sub-command itself)
    AnyCommand,
    /// A button with this custom id (`zd:<action>`); for later phases
    #[allow(dead_code)]
    Button(&'a str),
    /// A submitted form with this custom id (`zd:<action>`)
    Modal(&'a str),
    /// A button whose custom id starts with this (`zd:confirm:`)
    ButtonPrefix(&'a str),
}

impl Expect<'_> {
    /// Whether the stored interaction is exactly this kind and name
    fn matches(&self, interaction: &Interaction) -> bool {
        match self {
            Expect::Command(sub) => {
                interaction.kind == InteractionType::Command
                    && interaction.command_name.as_deref() == Some(COMMAND)
                    && interaction
                        .command_options
                        .iter()
                        .any(|o| o.kind == CommandOptionType::SubCommand && o.name == *sub)
            }
            Expect::AnyCommand => {
                interaction.kind == InteractionType::Command
                    && interaction.command_name.as_deref() == Some(COMMAND)
            }
            Expect::ButtonPrefix(prefix) => {
                interaction.kind == InteractionType::Component
                    && interaction
                        .custom_id
                        .as_deref()
                        .is_some_and(|id| id.starts_with(prefix) && id.len() > prefix.len())
            }
            Expect::Button(id) => {
                interaction.kind == InteractionType::Component
                    && interaction.custom_id.as_deref() == Some(*id)
            }
            Expect::Modal(id) => {
                interaction.kind == InteractionType::ModalSubmit
                    && interaction.custom_id.as_deref() == Some(*id)
            }
        }
    }
}

/// A press that passed steps 1 to 6
pub(super) struct Press {
    pub settings: FeaturesStaffDesk,
    pub interaction: Interaction,
    pub user: User,
    pub grant: StaffGrant,
    pub now: i64,
    /// The route's own path (`/staff/desk/...`)
    pub path: String,
}

impl Press {
    /// A record with the presser as actor, `summary` starting `desk`
    pub fn record(&self, action: &str, summary: &str) -> AuditRecord {
        presser_record(&self.interaction.user_id, action, summary)
    }

    /// The text value of a field of the submitted form, trimmed
    pub fn field(&self, id: &str) -> &str {
        field(&self.interaction, id)
    }
}

/// The text value of a field of a submitted form, trimmed (empty if absent)
pub(crate) fn field<'a>(interaction: &'a Interaction, id: &str) -> &'a str {
    interaction
        .modal_fields
        .iter()
        .find(|field| field.custom_id == id)
        .and_then(|field| field.value.as_deref())
        .map(str::trim)
        .unwrap_or_default()
}

fn presser_record(presser: &str, action: &str, summary: &str) -> AuditRecord {
    AuditRecord {
        actor_id: presser.to_string(),
        action: action.to_string(),
        target_kind: Some("user".to_string()),
        target_id: Some(presser.to_string()),
        summary: Some(summary.to_string()),
        ..Default::default()
    }
}

/// Whether the press happened where the desk is used: a configured channel,
/// or (only with `allow_dms`) the presser's DM with the bot
async fn in_place(db: &Database, settings: &FeaturesStaffDesk, interaction: &Interaction) -> bool {
    if settings.places.iter().any(|place| place == &interaction.channel_id) {
        return true;
    }
    if !settings.allow_dms || interaction.server_id.is_some() {
        return false;
    }
    match db.fetch_channel(&interaction.channel_id).await {
        Ok(Channel::DirectMessage { recipients, .. }) => {
            recipients.iter().any(|r| r == &settings.bot)
                && recipients.iter().any(|r| r == &interaction.user_id)
        }
        _ => false,
    }
}

/// The open desk session of the presser for this bot, if any (so refusals
/// name it in `staff_session_id`)
async fn open_desk_session(db: &Database, presser: &str, bot: &str, now: i64) -> Option<String> {
    db.fetch_desk_session(presser, bot, now).await.ok().flatten().map(|s| s.id)
}

/// Whether the ordinary session (sign-in) `id` still exists and is
/// `user`'s. Signing out, "log out everywhere" and a password change delete
/// ordinary sessions.
pub(super) async fn session_alive(db: &Database, id: &str, user: &str) -> Result<bool> {
    match db.fetch_session(id).await {
        Ok(session) => {
            // the same rule as the User guard: not idle (see `Session::is_open`)
            let idle_days = revolt_config::config().await.api.sessions.idle_days;
            let now = iso8601_timestamp::Timestamp::now_utc();
            Ok(session.user_id == user && !session.is_idle_at(now, idle_days))
        }
        Err(error) if matches!(error.error_type, ErrorType::UnknownUser | ErrorType::NotFound) => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// Steps 1 to 6: the press is genuine, of the expected kind, used once,
/// from a configured place, by a person with an active staff grant now.
/// `path` is the route's own path (`/staff/desk/...`).
pub(super) async fn press(
    db: &Database,
    caller: &User,
    data: &DataDeskInteraction,
    expect: Expect<'_>,
    path: &str,
) -> Result<Press> {
    // 1. only the desk's bot, only while the desk is on
    let settings = settings(caller).await?;

    // 2. genuine
    let interaction = db
        .fetch_interaction(&data.interaction_id)
        .await
        .map_err(|_| create_error!(UnknownInteraction))?;
    if !interaction.token_matches(&data.token) || interaction.application_id != settings.bot {
        return Err(create_error!(UnknownInteraction));
    }
    // a form's code is read from the copy in hand; the stored one is gone
    // now, whatever refusal comes next (wrong kind, expired, used, ...)
    if interaction.kind == InteractionType::ModalSubmit {
        db.clear_interaction_modal_fields(&interaction.id).await?;
    }
    if interaction.is_expired() {
        return Err(create_error!(InteractionExpired));
    }

    // 3. a sign-out is never turned into a sign-in, nor a button into a command
    if !expect.matches(&interaction) {
        return Err(create_error!(InvalidOperation));
    }

    // 4. one press, one action (and one refusal in the log)
    if !db.claim_interaction_use(&interaction.id).await? {
        return Err(create_error!(InteractionExpired));
    }

    let presser = interaction.user_id.clone();
    let now = staff_now();

    // 5. the place, from the stored interaction
    if !in_place(db, &settings, &interaction).await {
        let mut record = presser_record(&presser, "staff.desk.place", "desk: outside the desk's places");
        record.target_kind = Some("channel".to_string());
        record.target_id = Some(interaction.channel_id.clone());
        record.staff_session_id = open_desk_session(db, &presser, &settings.bot, now).await;
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }

    // 6. a person with an active grant now, re-read on every press
    let user = match db.fetch_user(&presser).await {
        Ok(user) if user.bot.is_none() => Some(user),
        Ok(_) => None,
        Err(error) if matches!(error.error_type, ErrorType::NotFound | ErrorType::UnknownUser) => None,
        Err(error) => return Err(error),
    };
    let grant = match &user {
        Some(_) => db.fetch_staff_grant(&presser).await?.filter(|g| g.is_active(now)),
        None => None,
    };
    let (Some(user), Some(grant)) = (user, grant) else {
        let mut record = presser_record(&presser, "staff.desk.no_grant", "desk: no active staff grant");
        record.staff_session_id = open_desk_session(db, &presser, &settings.bot, now).await;
        // access was removed (or never given): no desk session survives it
        db.revoke_desk_sessions_for_user(&presser, now).await?;
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    };

    Ok(Press {
        settings,
        interaction,
        user,
        grant,
        now,
        path: path.to_string(),
    })
}

/// Step 7 on a checked press: the presser's open desk session for this bot,
/// not idle, opened from the same ordinary session (sign-in) the press came
/// from, which must still exist; as the console's own [`StaffContext`]
pub(super) async fn context(db: &Database, press: Press) -> Result<(StaffContext, Interaction)> {
    let session = db
        .fetch_desk_session(&press.user.id, &press.settings.bot, press.now)
        .await?
        .filter(|s| press.now - s.last_seen_at < STAFF_SESSION_IDLE_MS)
        .ok_or_else(|| create_error!(InvalidSession))?;
    // the sign-in it was opened from is gone: the desk session ends with it
    if !session_alive(db, &session.session_id, &press.user.id).await? {
        db.revoke_staff_session(&session.id, press.now).await?;
        return Err(create_error!(InvalidSession));
    }
    // pressed from another sign-in of the same person: not this session
    if press.interaction.session_id.as_deref() != Some(session.session_id.as_str()) {
        return Err(create_error!(InvalidSession));
    }
    if press.now - session.last_seen_at > TOUCH_EVERY_MS {
        let _ = db.touch_staff_session(&session.id, press.now).await;
    }
    Ok((
        StaffContext {
            permissions: press.grant.effective(press.now),
            user: press.user,
            session,
            grant: press.grant,
            now: press.now,
            path: press.path,
        },
        press.interaction,
    ))
}

/// Every check (steps 1 to 7), then the console's own [`StaffContext`] for
/// the presser: user = presser, session = their desk session, permissions
/// = their grant now, `path` = the route's own path
pub(crate) async fn desk_actor(
    db: &Database,
    caller: &User,
    data: &DataDeskInteraction,
    expect: Expect<'_>,
    path: &str,
) -> Result<(StaffContext, Interaction)> {
    let press = press(db, caller, data, expect, path).await?;
    context(db, press).await
}
