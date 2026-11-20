//! What the server keeps about a person once they are a volunteer: the
//! membership record per team role, the invite to the team server, and the
//! way out again. Shared by the accept path, the settle look and the later
//! tasks (confirmation, away, removal).
//!
//! Nothing here runs unless `volunteers.enabled` is on; the callers check.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::FeaturesTeamApplications;
use revolt_database::{
    official_account, staff_now, Channel, Database, Invite, OfficialAccount, PartialMember,
    RemovalIntention, TeamApplication, TeamHistory, TeamMember, TeamMemberStatus,
};
use revolt_result::Result;

use super::{days_ms, out_of_reach};
use crate::util::team_texts;

/// Days a team-server invite lives
const INVITE_DAYS: i64 = 7;

fn history(actor: &str, action: &str) -> TeamHistory {
    TeamHistory {
        at: staff_now(),
        actor: actor.to_string(),
        action: action.to_string(),
    }
}

/// The record of an accepted person in the role of `application`; one per
/// person and role. An existing one is returned as it is; one that was left
/// is reopened as a trial (history `rejoined`).
pub(super) async fn enrol(
    db: &Database,
    settings: &FeaturesTeamApplications,
    application: &TeamApplication,
    actor: &str,
) -> Result<TeamMember> {
    let now = staff_now();
    let trial_ends_at = now + days_ms(settings.volunteers.trial_days);

    let find = |all: Vec<TeamMember>| all.into_iter().find(|m| m.role == application.role);
    if let Some(mut member) = find(db.fetch_team_members_of(&application.user_id).await?) {
        if member.status == TeamMemberStatus::Left {
            member.status = TeamMemberStatus::Trial;
            member.joined_at = now;
            member.trial_ends_at = trial_ends_at;
            member.confirmed_at = None;
            member.away_until = None;
            member.left_at = None;
            member.low_months = 0;
            member.history.push(history(actor, "rejoined"));
            db.update_team_member(&member).await?;
        }
        return Ok(member);
    }

    let member = TeamMember {
        id: ulid::Ulid::new().to_string(),
        user_id: application.user_id.clone(),
        role: application.role.clone(),
        server: settings
            .role(&application.role)
            .map(|role| role.server.clone())
            .unwrap_or_default(),
        status: TeamMemberStatus::Trial,
        joined_at: now,
        trial_ends_at,
        confirmed_at: None,
        away_until: None,
        left_at: None,
        low_months: 0,
        notes: None,
        history: vec![history(actor, "accepted")],
    };
    match db.insert_team_member(&member).await {
        Ok(()) => Ok(member),
        // two looks at once: the record that got in first is the record
        Err(error) => find(db.fetch_team_members_of(&application.user_id).await?).ok_or(error),
    }
}

/// Take server roles off a person in one server (they may not be there any
/// more, or the roles gone: nothing to do then)
async fn take_roles(db: &Database, server: &str, user_id: &str, drop: &[String]) {
    let Ok(mut member) = db.fetch_member(server, user_id).await else {
        return;
    };
    let roles: Vec<String> = member
        .roles
        .iter()
        .filter(|id| !drop.contains(id))
        .cloned()
        .collect();
    if roles.len() == member.roles.len() {
        return;
    }
    if let Err(error) = member
        .update(
            db,
            PartialMember {
                roles: Some(roles),
                ..Default::default()
            },
            vec![],
        )
        .await
    {
        log::warn!("team roles not taken back: {:?}", error.error_type);
    }
}

/// The person leaves the team server, unless that is one of the servers a
/// team role belongs to or the person owns it
async fn leave_team_server(db: &Database, settings: &FeaturesTeamApplications, user_id: &str) {
    let id = settings.volunteers.team_server.trim();
    if id.is_empty() {
        return;
    }
    if settings.roles.iter().any(|role| role.server == id) {
        log::warn!("the team server is also a team role's server: nobody is removed from it");
        return;
    }
    let Ok(server) = db.fetch_server(id).await else {
        return;
    };
    if server.owner == user_id {
        return;
    }
    let Ok(member) = db.fetch_member(id, user_id).await else {
        return;
    };
    if let Err(error) = member
        .remove(db, &server, RemovalIntention::Kick, true)
        .await
    {
        log::warn!("not removed from the team server: {:?}", error.error_type);
    }
}

/// End one membership. The role of the team role goes; the server's "Zrzur
/// Team" role goes only when no other current membership is left in that
/// server; the person leaves the team server only when nothing current is
/// left anywhere. `action` is `left` (their own word) or `removed` (staff);
/// it is the history word and picks the thank-you message.
#[allow(dead_code)] // used by the leave and removal routes of the next tasks
pub(super) async fn end_membership(
    db: &Database,
    settings: &FeaturesTeamApplications,
    member: &mut TeamMember,
    actor: &str,
    action: &str,
    reason: Option<&str>,
) -> Result<()> {
    // already gone: no second history entry, no second thank-you
    if member.status == TeamMemberStatus::Left {
        return Ok(());
    }
    let now = staff_now();
    member.status = TeamMemberStatus::Left;
    member.left_at = Some(now);
    member.away_until = None;
    member.history.push(history(actor, action));
    db.update_team_member(member).await?;

    let role = settings.role(&member.role);
    let others: Vec<TeamMember> = db
        .fetch_team_members_of(&member.user_id)
        .await?
        .into_iter()
        .filter(|other| other.is_current() && other.id != member.id)
        .collect();
    let same_server: Vec<&TeamMember> = others
        .iter()
        .filter(|other| other.server == member.server)
        .collect();

    if let Some(role) = role {
        let mut drop = Vec::new();
        // a role only the owner gives by hand is taken back by hand
        let shared_role = same_server
            .iter()
            .filter_map(|other| settings.role(&other.role))
            .any(|other| other.role == role.role || other.team_role == role.role);
        if !role.is_manual() && !role.role.trim().is_empty() && !shared_role {
            drop.push(role.role.clone());
        }
        // the server's Zrzur Team role stays while another membership is there
        if !role.team_role.trim().is_empty() && same_server.is_empty() {
            drop.push(role.team_role.clone());
        }
        if !drop.is_empty() {
            take_roles(db, &member.server, &member.user_id, &drop).await;
        }
    }

    if others.is_empty() {
        leave_team_server(db, settings, &member.user_id).await;
        // the details are deleted after the keeping time, unless an open
        // application still holds them
        if out_of_reach(db, &member.user_id, "").await {
            if let Ok(Some(mut identity)) = db.fetch_team_identity(&member.user_id).await {
                identity.purge_at =
                    Some(now + days_ms(settings.volunteers.identity_keep_days));
                let _ = db.upsert_team_identity(&identity).await;
            }
        }
    }

    let key = if action == "removed" {
        "volunteer.removed"
    } else {
        "volunteer.left"
    };
    team_texts::send(
        db,
        &member.user_id,
        key,
        role,
        &member.role,
        &[("reason", reason.unwrap_or_default())],
    )
    .await;
    Ok(())
}

/// A single-use invite code to the team server for a person who is a
/// current volunteer, valid for seven days. Never posted in a channel: the
/// caller puts it in the welcome message. `None`, quietly, when there is no
/// team server, the person is already in it, or the invite cannot be made.
pub(super) async fn invite_to_team_server(
    db: &Database,
    settings: &FeaturesTeamApplications,
    user_id: &str,
) -> Option<String> {
    let id = settings.volunteers.team_server.trim();
    if !settings.volunteers.enabled || id.is_empty() {
        return None;
    }
    let current = db
        .fetch_team_members_of(user_id)
        .await
        .ok()?
        .iter()
        .any(|member| member.is_current());
    if !current || db.fetch_member(id, user_id).await.is_ok() {
        return None;
    }
    let skip = |why: &str| {
        log::warn!("no team server invite: {why}");
        None
    };
    let Ok(server) = db.fetch_server(id).await else {
        return skip("the team server is not found");
    };
    let mut channel = None;
    for channel_id in &server.channels {
        if let Ok(found @ Channel::TextChannel { .. }) = db.fetch_channel(channel_id).await {
            channel = Some(found);
            break;
        }
    }
    let Some(channel) = channel else {
        return skip("the team server has no text channel");
    };
    let Ok(creator) = official_account(db, OfficialAccount::Zrzur).await else {
        return skip("the official account is not available");
    };
    let expires = Timestamp::now_utc().checked_add(Duration::days(INVITE_DAYS));
    match Invite::create_channel_invite(db, &creator, &channel, Some(1), expires).await {
        Ok(invite) => Some(invite.code().to_string()),
        Err(error) => skip(&format!("{:?}", error.error_type)),
    }
}
