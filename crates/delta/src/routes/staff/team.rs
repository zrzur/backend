//! The Team page of the admin console: the volunteers of the Zrzur team.
//!
//! Every route needs `platform.team.manage` (the owner role only, by
//! default), checked here on the server; hiding a button is never the
//! protection. Showing a person's identity and removing a person also need a
//! fresh step-up. Every act is written to the platform audit log before it
//! happens, with the staff member as the actor. An audit record never holds
//! the identity, the note or the text of a message.
//!
//! Only counts are shown (`month_contributions`, `months`): no message
//! content, no channel names. Names and servers are sent as they are
//! (trimmed): the console shows them as text, so nothing is
//! escaped for markdown here. Notes are shown to staff only here; the person
//! gets them in their own data export (GDPR Art. 15).

use std::collections::HashMap;

use revolt_database::{
    AuditOutcome, AuditRecord, Database, StaffPermission, TeamHistory, TeamMember,
    TeamMemberFilter, TeamMemberStatus, record_platform_audit,
    util::team_activity::{
        COUNTING_RESUMED, COUNTING_STOPPED, counting_stopped, day_of, previous_month,
    },
};
use revolt_result::{Result, create_error};
use rocket::{State, serde::json::Json};
use serde::{Deserialize, Serialize};

use super::StaffContext;
use crate::routes::team_desk::{
    self, IdentityView, identity_view, length_problem, member_word, no_identity, role_name,
    server_name, volunteers,
};

/// Rows of the list a page holds, when the caller names none
const DEFAULT_LIMIT: u32 = 100;
/// The most rows a page holds
const MAX_LIMIT: u32 = 200;
/// Months of counts a person's page shows, the current one included
const MONTHS: usize = 6;
const MIN_REMOVE_REASON: usize = 10;
const MAX_REMOVE_REASON: usize = 500;
const MAX_NOTE: usize = 2000;

/// One volunteer in the list
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StaffTeamRow {
    pub member_id: String,
    pub user_id: String,
    /// Display name, else the username (raw, trimmed: shown as text)
    pub account_name: String,
    /// Key of the team role
    pub role: String,
    pub role_name: String,
    /// Raw, trimmed
    pub server_name: String,
    /// trial, active, away, inactive or left
    pub status: String,
    pub joined_at: i64,
    pub trial_ends_at: i64,
    pub away_until: Option<i64>,
    /// All five kinds of count in the current UTC month
    pub month_contributions: u32,
    /// Whether "About you" is on file (the identity itself is not here)
    pub has_identity: bool,
    /// The monthly minimum of the settings (0 or more), so the page shows the live value
    pub monthly_minimum: i64,
}

/// The counts of one UTC month, per kind
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct StaffTeamMonth {
    /// yyyymm
    pub month: i32,
    pub messages: u32,
    pub replies: u32,
    pub moderation: u32,
    pub reviews: u32,
    pub tickets: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StaffTeamPerson {
    pub row: StaffTeamRow,
    /// The last six UTC months, oldest first, the current one last
    pub months: Vec<StaffTeamMonth>,
    pub history: Vec<TeamHistory>,
    pub notes: Option<String>,
    /// Same value as `row.monthly_minimum`
    pub monthly_minimum: i64,
    /// The person objected and is not counted (staff stopped it)
    #[serde(default)]
    pub counting_stopped: bool,
    /// The longest away the settings allow, in days (the console's limit)
    pub away_max_days: i64,
}

#[derive(Deserialize)]
pub struct DataAway {
    pub days: i64,
}

#[derive(Deserialize)]
pub struct DataNote {
    pub text: String,
}

#[derive(Deserialize)]
pub struct DataCounting {
    /// `false` stops the counting (an objection), `true` starts it again
    pub on: bool,
}

#[derive(Deserialize)]
pub struct DataRemoveVolunteer {
    pub reason: String,
}

/// The status word of the query (`?status=`)
fn parse_status(word: &str) -> Result<TeamMemberStatus> {
    use TeamMemberStatus as S;
    [S::Trial, S::Active, S::Away, S::Inactive, S::Left]
        .into_iter()
        .find(|status| member_word(*status) == word)
        .ok_or_else(|| create_error!(InvalidOperation))
}

async fn row(
    db: &Database,
    settings: &revolt_config::FeaturesTeamApplications,
    member: &TeamMember,
    servers: &mut HashMap<String, String>,
    now: i64,
) -> Result<StaffTeamRow> {
    let role = settings.role(&member.role);
    if !servers.contains_key(&member.server) {
        let name = match role {
            Some(role) if role.server == member.server => server_name(db, Some(role)).await,
            _ => db
                .fetch_server(&member.server)
                .await
                .map(|server| server.name)
                .unwrap_or_default(),
        };
        servers.insert(member.server.clone(), name.trim().to_string());
    }
    let account_name = match db.fetch_user(&member.user_id).await {
        Ok(person) => person
            .display_name
            .clone()
            .unwrap_or(person.username)
            .trim()
            .to_string(),
        Err(_) => String::new(),
    };
    let (from, to) = volunteers::month_bounds(now);
    let days = db.fetch_team_activity(&member.user_id, from, to).await?;
    Ok(StaffTeamRow {
        member_id: member.id.clone(),
        user_id: member.user_id.clone(),
        account_name,
        role: member.role.clone(),
        role_name: role_name(settings, &member.role),
        server_name: servers.get(&member.server).cloned().unwrap_or_default(),
        status: member_word(member.status).to_string(),
        joined_at: member.joined_at,
        trial_ends_at: member.trial_ends_at,
        away_until: member.away_until,
        month_contributions: volunteers::month_total(&days),
        has_identity: db
            .fetch_team_identity(&member.user_id)
            .await?
            .is_some_and(|stored| !stored.purge_at.is_some_and(|at| at <= now)),
        monthly_minimum: settings.volunteers.monthly_minimum.max(0),
    })
}

/// The last `MONTHS` UTC months (yyyymm), oldest first
fn last_months(now: i64) -> Vec<i32> {
    let mut months = vec![day_of(now) / 100];
    while months.len() < MONTHS {
        months.insert(0, previous_month(months[0]));
    }
    months
}

async fn person(
    db: &Database,
    settings: &revolt_config::FeaturesTeamApplications,
    member: &TeamMember,
    now: i64,
) -> Result<StaffTeamPerson> {
    let row = row(db, settings, member, &mut HashMap::new(), now).await?;
    let months = last_months(now);
    let days = db
        .fetch_team_activity(
            &member.user_id,
            months[0] * 100 + 1,
            months[MONTHS - 1] * 100 + 31,
        )
        .await?;
    let months = months
        .into_iter()
        .map(|month| {
            let mut total = StaffTeamMonth {
                month,
                ..Default::default()
            };
            for day in days.iter().filter(|day| day.day / 100 == month) {
                for count in &day.counts {
                    total.messages = total.messages.saturating_add(count.messages);
                    total.replies = total.replies.saturating_add(count.replies);
                    total.moderation = total.moderation.saturating_add(count.moderation);
                    total.reviews = total.reviews.saturating_add(count.reviews);
                    total.tickets = total.tickets.saturating_add(count.tickets);
                }
            }
            total
        })
        .collect();
    let all = db.fetch_team_members_of(&member.user_id).await?;
    Ok(StaffTeamPerson {
        row,
        months,
        history: member.history.clone(),
        notes: member.notes.clone(),
        monthly_minimum: settings.volunteers.monthly_minimum.max(0),
        counting_stopped: counting_stopped(&all),
        away_max_days: settings.volunteers.away_max_days.max(0),
    })
}

/// The membership named in the path; "not found" for anything else
async fn member_of(db: &Database, id: &str) -> Result<TeamMember> {
    db.fetch_team_member(id).await
}

/// Write the audit record first: no record, no act
async fn log(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    member: &TeamMember,
    reason: Option<String>,
    summary: String,
) -> Result<()> {
    let mut record: AuditRecord = staff.record(action, Some(StaffPermission::TeamManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(member.user_id.clone());
    record.reason = reason;
    record.summary = Some(summary);
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    Ok(())
}

/// # Team: the volunteers
///
/// Newest last, by member id. `after` is the last member id of the page
/// before; `limit` is 100 by default and 200 at most.
#[get("/team?<role>&<status>&<after>&<limit>")]
pub async fn list(
    db: &State<Database>,
    staff: StaffContext,
    role: Option<&str>,
    status: Option<&str>,
    after: Option<&str>,
    limit: Option<u32>,
) -> Result<Json<Vec<StaffTeamRow>>> {
    list_core(db, &staff, role, status, after, limit)
        .await
        .map(Json)
}

/// The console's `list`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks and records
pub(super) async fn list_core(
    db: &Database,
    staff: &StaffContext,
    role: Option<&str>,
    status: Option<&str>,
    after: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<StaffTeamRow>> {
    staff.require(db, StaffPermission::TeamManage).await?;
    let settings = team_desk::staff_desk(&staff.user.id).await?;
    let filter = TeamMemberFilter {
        role: role.filter(|r| !r.is_empty()).map(str::to_string),
        status: status
            .filter(|s| !s.is_empty())
            .map(parse_status)
            .transpose()?,
        after: after.filter(|a| !a.is_empty()).map(str::to_string),
        limit: Some(limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)),
    };
    let mut servers = HashMap::new();
    let mut rows = Vec::new();
    for member in db.fetch_team_members(&filter).await? {
        rows.push(row(db, &settings, &member, &mut servers, staff.now).await?);
    }
    Ok(rows)
}

/// # Team: one volunteer
#[get("/team/<id>")]
pub async fn detail(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
) -> Result<Json<StaffTeamPerson>> {
    detail_core(db, &staff, id).await.map(Json)
}

/// The console's `detail`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks and records
pub(super) async fn detail_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
) -> Result<StaffTeamPerson> {
    staff.require(db, StaffPermission::TeamManage).await?;
    let settings = team_desk::staff_desk(&staff.user.id).await?;
    let member = member_of(db, id).await?;
    Ok(person(db, &settings, &member, staff.now).await?)
}

/// `require_step_up`; the guard records a missing step-up itself
async fn step_up(db: &Database, staff: &StaffContext) -> Result<()> {
    staff.require_step_up(db, StaffPermission::TeamManage).await
}

/// # Team: show identity
///
/// Needs a fresh step-up. Every view is logged: who looked at whom, never
/// what was shown.
#[post("/team/<id>/identity")]
pub async fn identity(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
) -> Result<Json<IdentityView>> {
    identity_core(db, &staff, id).await.map(Json)
}

/// The console's `identity`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks and records
pub(super) async fn identity_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
) -> Result<IdentityView> {
    staff.require(db, StaffPermission::TeamManage).await?;
    step_up(db, &staff).await?;
    let _ = team_desk::staff_desk(&staff.user.id).await?;
    let member = member_of(db, id).await?;
    team_desk::purge_due(db, staff.now).await?;
    log(
        db,
        &staff,
        "team.identity.view",
        &member,
        None,
        format!("admin console, member {}", member.id),
    )
    .await?;
    Ok(identity_view(db, &member.user_id, false)
        .await?
        .unwrap_or_else(no_identity))
}

/// # Team: confirm a volunteer on trial
#[post("/team/<id>/confirm")]
pub async fn confirm(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
) -> Result<Json<StaffTeamPerson>> {
    confirm_core(db, &staff, id).await.map(Json)
}

/// The console's `confirm`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks and records
pub(super) async fn confirm_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
) -> Result<StaffTeamPerson> {
    staff.require(db, StaffPermission::TeamManage).await?;
    let settings = team_desk::staff_desk(&staff.user.id).await?;
    let member = member_of(db, id).await?;
    if member.status != TeamMemberStatus::Trial {
        return Err(create_error!(InvalidOperation));
    }
    log(
        db,
        &staff,
        "team.member.confirm",
        &member,
        None,
        format!("member {}", member.id),
    )
    .await?;
    // read again right before the write: a leave or removal in between wins
    let mut member = member_of(db, id).await?;
    if member.status != TeamMemberStatus::Trial {
        return Err(create_error!(InvalidOperation));
    }
    member.status = TeamMemberStatus::Active;
    member.confirmed_at = Some(staff.now);
    member.history.push(TeamHistory {
        at: staff.now,
        actor: staff.user.id.clone(),
        action: "confirmed".to_string(),
    });
    db.update_team_member(&member).await?;
    Ok(person(db, &settings, &member, staff.now).await?)
}

/// # Team: stop or resume counting a person
///
/// A volunteer who objects to the counting (GDPR Art. 21) tells the owner;
/// `on: false` stops it for the person, in every current membership
/// (history `counting_stopped`), and the daily job counts none of their days
/// until `on: true` starts it again (`counting_resumed`, from the next day).
/// Logged before it takes effect; asking for the state it is already in
/// changes nothing and logs nothing.
#[post("/team/<id>/counting", data = "<data>")]
pub async fn counting(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataCounting>,
) -> Result<Json<StaffTeamPerson>> {
    counting_core(db, &staff, id, data.into_inner())
        .await
        .map(Json)
}

/// The console's `counting`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks and records
pub(super) async fn counting_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataCounting,
) -> Result<StaffTeamPerson> {
    staff.require(db, StaffPermission::TeamManage).await?;
    let settings = team_desk::staff_desk(&staff.user.id).await?;
    let member = member_of(db, id).await?;
    if !member.is_current() {
        return Err(create_error!(InvalidOperation));
    }
    let all = db.fetch_team_members_of(&member.user_id).await?;
    if counting_stopped(&all) != data.on {
        return Ok(person(db, &settings, &member, staff.now).await?);
    }
    let word = if data.on {
        COUNTING_RESUMED
    } else {
        COUNTING_STOPPED
    };
    log(
        db,
        &staff,
        "team.member.counting",
        &member,
        None,
        format!("member {}, {word}", member.id),
    )
    .await?;
    for other in all.iter().filter(|m| m.is_current()) {
        // read again right before the write: a leave in between wins
        let mut other = member_of(db, &other.id).await?;
        if !other.is_current() {
            continue;
        }
        other.history.push(TeamHistory {
            at: staff.now,
            actor: staff.user.id.clone(),
            action: word.to_string(),
        });
        db.update_team_member(&other).await?;
    }
    let member = member_of(db, id).await?;
    Ok(person(db, &settings, &member, staff.now).await?)
}

/// # Team: set away, or bring back
///
/// The same rules and the same longest stay as the volunteer's own "away";
/// `days` of 0 brings the person back. Logged once it took effect.
#[post("/team/<id>/away", data = "<data>")]
pub async fn away(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataAway>,
) -> Result<Json<StaffTeamPerson>> {
    away_core(db, &staff, id, data.into_inner()).await.map(Json)
}

/// The console's `away`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks and records
pub(super) async fn away_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataAway,
) -> Result<StaffTeamPerson> {
    staff.require(db, StaffPermission::TeamManage).await?;
    let settings = team_desk::staff_desk(&staff.user.id).await?;
    let max = settings.volunteers.away_max_days.max(0);
    if data.days < 0 || data.days > max {
        return Err(length_problem("days", 0, max as usize));
    }
    let mut member = member_of(db, id).await?;
    // set_away reads the record again right before it writes
    if !volunteers::set_away(db, &mut member, &staff.user.id, data.days, max).await? {
        return Err(create_error!(InvalidOperation));
    }
    log(
        db,
        &staff,
        "team.member.away",
        &member,
        None,
        format!("member {}, days {}", member.id, data.days),
    )
    .await?;
    // the volunteer hears of it, as after their own "I'm away": until when
    // (as stored), or the status they are back to
    let (key, vars) = if member.status == TeamMemberStatus::Away {
        let until = member
            .away_until
            .map(|at| format!("<t:{}:D>", at / 1000))
            .unwrap_or_default();
        ("volunteer.away_by_staff", vec![("until", until)])
    } else {
        (
            "volunteer.back_by_staff",
            vec![("state", member_word(member.status).to_string())],
        )
    };
    let vars: Vec<(&str, &str)> = vars
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    crate::util::team_texts::send(
        db,
        &member.user_id,
        key,
        settings.role(&member.role),
        &member.role,
        &vars,
    )
    .await;
    Ok(person(db, &settings, &member, staff.now).await?)
}

/// # Team: write the staff note
///
/// Up to 2000 characters, trimmed, empty clears it. Part of the person's
/// own data export; never in the audit log.
#[post("/team/<id>/note", data = "<data>")]
pub async fn note(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataNote>,
) -> Result<Json<StaffTeamPerson>> {
    note_core(db, &staff, id, data.into_inner()).await.map(Json)
}

/// The console's `note`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks and records
pub(super) async fn note_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataNote,
) -> Result<StaffTeamPerson> {
    staff.require(db, StaffPermission::TeamManage).await?;
    let settings = team_desk::staff_desk(&staff.user.id).await?;
    let text = data.text.trim();
    let length = text.chars().count();
    if length > MAX_NOTE {
        return Err(length_problem("text", 0, MAX_NOTE));
    }
    let member = member_of(db, id).await?;
    log(
        db,
        &staff,
        "team.member.note",
        &member,
        None,
        format!("member {}, {} characters", member.id, length),
    )
    .await?;
    // read again right before the write: a leave or removal in between wins
    let mut member = member_of(db, id).await?;
    if !member.is_current() {
        return Err(create_error!(InvalidOperation));
    }
    member.notes = if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    };
    db.update_team_member(&member).await?;
    Ok(person(db, &settings, &member, staff.now).await?)
}

/// # Team: remove a volunteer
///
/// Needs a fresh step-up and a reason of 10 to 500 characters. The person
/// gets the polite message from the official account, with the reason;
/// the audit log keeps only its length.
#[post("/team/<id>/remove", data = "<data>")]
pub async fn remove(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataRemoveVolunteer>,
) -> Result<Json<StaffTeamPerson>> {
    remove_core(db, &staff, id, data.into_inner())
        .await
        .map(Json)
}

/// The console's `remove`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks and records
pub(super) async fn remove_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataRemoveVolunteer,
) -> Result<StaffTeamPerson> {
    staff.require(db, StaffPermission::TeamManage).await?;
    step_up(db, &staff).await?;
    let settings = team_desk::staff_desk(&staff.user.id).await?;
    let reason = data.reason.trim();
    let length = reason.chars().count();
    if !(MIN_REMOVE_REASON..=MAX_REMOVE_REASON).contains(&length) {
        return Err(length_problem(
            "reason",
            MIN_REMOVE_REASON,
            MAX_REMOVE_REASON,
        ));
    }
    let mut member = member_of(db, id).await?;
    if !member.is_current() {
        return Err(create_error!(InvalidOperation));
    }
    // the reason is about a person and goes only to them: the log keeps
    // that there was one, not what it said
    log(
        db,
        &staff,
        "team.member.remove",
        &member,
        None,
        format!("member {}, reason of {} characters", member.id, length),
    )
    .await?;
    // end_membership reads the record again and does nothing if it is gone
    volunteers::end_membership(
        db,
        &settings,
        &mut member,
        &staff.user.id,
        "removed",
        Some(reason),
    )
    .await?;
    let member = member_of(db, id).await?;
    Ok(person(db, &settings, &member, staff.now).await?)
}
