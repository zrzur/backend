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
//! 7. they have an open desk session, not idle, else `InvalidSession` (the
//!    bot then offers sign-in). Skipped only by the sign-in route.
//!
//! The place and the presser come from the stored interaction, never from
//! anything the bot sends.

use revolt_config::FeaturesStaffDesk;
use revolt_database::{
    record_platform_audit, staff_now, AuditOutcome, AuditRecord, Channel, Database, Interaction,
    StaffGrant, User, STAFF_SESSION_IDLE_MS,
};
use revolt_models::v0::{CommandOptionType, InteractionType};
use revolt_result::{create_error, Result};
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
    /// A button with this custom id (`zd:<action>`); for later phases
    #[allow(dead_code)]
    Button(&'a str),
    /// A submitted form with this custom id (`zd:<action>`)
    Modal(&'a str),
}

impl Expect<'_> {
    /// The route's short name, for the context's `path`
    fn route(&self) -> &str {
        match self {
            Expect::Command(sub) => sub,
            Expect::Button(id) | Expect::Modal(id) => id.strip_prefix("zd:").unwrap_or(id),
        }
    }

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
    /// `/staff/desk/<route>`
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

/// Steps 1 to 6: the press is genuine, of the expected kind, used once,
/// from a configured place, by a person with an active staff grant now
pub(super) async fn press(
    db: &Database,
    caller: &User,
    data: &DataDeskInteraction,
    expect: Expect<'_>,
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

    let path = format!("/staff/desk/{}", expect.route());
    let presser = interaction.user_id.clone();

    // 5. the place, from the stored interaction
    if !in_place(db, &settings, &interaction).await {
        let mut record = presser_record(&presser, "staff.desk.place", "desk: outside the desk's places");
        record.target_kind = Some("channel".to_string());
        record.target_id = Some(interaction.channel_id.clone());
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }

    // 6. a person with an active grant now, re-read on every press
    let now = staff_now();
    let user = db.fetch_user(&presser).await.ok().filter(|u| u.bot.is_none());
    let grant = match &user {
        Some(_) => db.fetch_staff_grant(&presser).await?.filter(|g| g.is_active(now)),
        None => None,
    };
    let (Some(user), Some(grant)) = (user, grant) else {
        // access was removed (or never given): no desk session survives it
        db.revoke_desk_sessions_for_user(&presser, now).await?;
        let record = presser_record(&presser, "staff.desk.no_grant", "desk: no active staff grant");
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    };

    Ok(Press {
        settings,
        interaction,
        user,
        grant,
        now,
        path,
    })
}

/// Step 7 on a checked press: the presser's open desk session for this bot,
/// not idle, as the console's own [`StaffContext`]
pub(super) async fn context(db: &Database, press: Press) -> Result<(StaffContext, Interaction)> {
    let session = db
        .fetch_desk_session(&press.user.id, &press.settings.bot, press.now)
        .await?
        .filter(|s| press.now - s.last_seen_at < STAFF_SESSION_IDLE_MS)
        .ok_or_else(|| create_error!(InvalidSession))?;
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
/// = their grant now, `path` = `/staff/desk/<route>`
pub(crate) async fn desk_actor(
    db: &Database,
    caller: &User,
    data: &DataDeskInteraction,
    expect: Expect<'_>,
) -> Result<(StaffContext, Interaction)> {
    let press = press(db, caller, data, expect).await?;
    context(db, press).await
}
