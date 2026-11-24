//! Badges in the admin console: see a user's badges, grant and revoke the
//! hand-given ones, and approve the sensitive changes another staff member
//! asked for. Every change goes through `revolt_database::mutate_badge`,
//! the one authorization gate shared with `/badge` and `zrzur-staff badge`:
//!
//! - `platform.users.read`: see the catalogue, a user's badge record and
//!   the open requests
//! - `platform.badges.manage`: grant and revoke `Staff`-rule badges
//! - `Privileged`-rule badges (Zrzur Developer, old official badges): an
//!   Owner only, with a fresh step-up
//! - Founder: nobody (`[api.users] founder_id`); Staff and Platform
//!   Moderator: only an appointment (`zrzur-staff appoint`)
//! - giving Partner or Elite Bug Hunter, and taking away a `Privileged`
//!   badge: a second staff member approves (never the one who asked, never
//!   the person concerned)
//!
//! Every change and every refusal is written to the platform audit log.

use revolt_database::{
    approve_badge_request, badge_request_open, decide_community_check, mutate_badge, refuse_badge_request,
    BadgeActor, BadgeChange, BadgeChangeRequest, BadgeMutation, BadgeOutcome, BadgePath,
    CommunityBadgeCheck, CommunityCheckStatus, CommunityThresholds, Database, StaffPermission,
};
use revolt_models::v0::{AdminBadgeGrant, AdminUserBadges, BadgeInfo};
use revolt_result::Result;
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};

use super::StaffContext;
use crate::routes::badges::{admin_grant_view, catalogue, now_ms, user_record};

/// The badge rights of this staff session in the console
fn actor(staff: &StaffContext) -> BadgeActor {
    actor_on(staff, BadgePath::StaffConsole)
}

/// The badge rights of this staff session on a path (console or Zrzur Desk)
pub(super) fn actor_on(staff: &StaffContext, path: BadgePath) -> BadgeActor {
    BadgeActor {
        id: staff.user.id.clone(),
        path,
        staff: staff.has(StaffPermission::BadgesManage),
        owner: staff.grant.is_owner(staff.now),
        step_up: staff.session.step_up_until.is_some_and(|until| staff.now < until),
        session_id: Some(staff.session.id.clone()),
    }
}

/// # Badge catalogue
#[get("/badges/catalogue")]
pub async fn badge_catalogue(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<BadgeInfo>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    Ok(catalogue().await)
}

/// # A user's badges
///
/// Grants, badge history and which badges you may give them now.
#[get("/users/<id>/badges")]
pub async fn user_badges(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<AdminUserBadges>> {
    user_badges_core(db, &staff, id).await.map(Json)
}

/// The console's `user_badges`, for any caller holding a [`StaffContext`]
/// (the console route and Zrzur Desk): the same checks
pub(super) async fn user_badges_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
) -> Result<AdminUserBadges> {
    staff.require(db, StaffPermission::UsersRead).await?;
    user_record(db, &actor(staff), id).await
}

#[derive(Deserialize)]
pub struct DataBadgeChange {
    pub badge: String,
    pub reason: String,
    /// Milliseconds since the epoch (grants only)
    pub expires_at: Option<i64>,
}

/// A badge change request, as the console shows it
#[derive(Serialize, Debug)]
pub struct BadgeRequestView {
    pub id: String,
    pub user_id: String,
    pub badge: String,
    pub grant: bool,
    pub reason: String,
    pub grant_expires_at: Option<i64>,
    pub requested_by: String,
    pub requested_via: String,
    /// Pending, Approved, Refused or Expired
    pub status: &'static str,
    pub created_at: i64,
    pub expires_at: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<i64>,
}

pub(crate) fn request_view(request: &BadgeChangeRequest, now: i64) -> BadgeRequestView {
    use revolt_database::BadgeRequestStatus::*;
    BadgeRequestView {
        id: request.id.clone(),
        user_id: request.user_id.clone(),
        badge: request.badge.clone(),
        grant: request.grant,
        reason: request.reason.clone(),
        grant_expires_at: request.grant_expires_at,
        requested_by: request.requested_by.clone(),
        requested_via: request.requested_via.clone(),
        status: match request.status {
            Pending if badge_request_open(request, now) => "Pending",
            Pending => "Expired",
            Approved => "Approved",
            Refused => "Refused",
        },
        created_at: request.created_at,
        expires_at: request.expires_at,
        decided_by: request.decided_by.clone(),
        decided_at: request.decided_at,
    }
}

/// What came of a grant or revoke: `applied` (the grant's fields follow, as
/// before) or `pending` (a second staff member must approve `request`)
#[derive(Serialize, Debug)]
pub struct StaffBadgeChange {
    pub status: &'static str,
    #[serde(flatten)]
    pub grant: Option<AdminBadgeGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<BadgeRequestView>,
}

/// A grant or revoke through beta's single gate, on `path` (the console,
/// or Zrzur Desk with its own audit prefix `staff.desk.badge`)
async fn change(
    db: &Database,
    staff: &StaffContext,
    path: BadgePath,
    id: &str,
    data: DataBadgeChange,
    change: BadgeChange,
) -> Result<StaffBadgeChange> {
    let (badge, note) = (data.badge.clone(), data.reason.clone());
    let actor = actor_on(staff, path);
    let mutation = BadgeMutation {
        user_id: id.to_string(),
        badge: data.badge,
        change,
        reason: data.reason,
        expires_at: if change == BadgeChange::Grant { data.expires_at } else { None },
        source_ref: None,
    };
    // the person reads the reason and the audit log keeps it: 300 at most
    // (DK-13). Rights first: a change the gate refuses goes on to the gate,
    // which records the refusal.
    if mutation.reason.trim().chars().count() > 300
        && revolt_database::authorize_badge_mutation(db, &actor, &mutation).await?.is_ok()
    {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length("reason", 3, 300),
        ]));
    }
    let outcome = mutate_badge(db, &actor, mutation).await?;
    Ok(match outcome {
        BadgeOutcome::Applied(grant) => {
            crate::util::official::badge(db, id, change == BadgeChange::Grant, &badge, &note).await;
            StaffBadgeChange {
                status: "applied",
                grant: Some(admin_grant_view(grant, now_ms())),
                request: None,
            }
        }
        BadgeOutcome::Pending(request) => StaffBadgeChange {
            status: "pending",
            grant: None,
            request: Some(request_view(&request, now_ms())),
        },
    })
}

/// The console's grant, for Zrzur Desk: the same gate on the desk's path
pub(super) async fn grant_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataBadgeChange,
) -> Result<StaffBadgeChange> {
    change(db, staff, BadgePath::Desk, id, data, BadgeChange::Grant).await
}

/// The console's revoke, for Zrzur Desk: the same gate on the desk's path
pub(super) async fn revoke_core(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataBadgeChange,
) -> Result<StaffBadgeChange> {
    change(db, staff, BadgePath::Desk, id, data, BadgeChange::Revoke).await
}

/// # Grant a badge
#[post("/users/<id>/badges/grant", data = "<data>")]
pub async fn grant(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataBadgeChange>,
) -> Result<Json<StaffBadgeChange>> {
    change(db, &staff, BadgePath::StaffConsole, id, data.into_inner(), BadgeChange::Grant)
        .await
        .map(Json)
}

/// # Revoke a badge
#[post("/users/<id>/badges/revoke", data = "<data>")]
pub async fn revoke(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataBadgeChange>,
) -> Result<Json<StaffBadgeChange>> {
    change(db, &staff, BadgePath::StaffConsole, id, data.into_inner(), BadgeChange::Revoke)
        .await
        .map(Json)
}

/// # Badge changes waiting for a second person (and recent decisions)
#[get("/badges/requests")]
pub async fn requests(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<BadgeRequestView>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let now = now_ms();
    // every waiting request, however old the list is, and recent decisions
    let mut list = db.fetch_pending_badge_requests().await?;
    for request in db.fetch_badge_requests().await? {
        if !list.iter().any(|r| r.id == request.id) {
            list.push(request);
        }
    }
    list.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(Json(list.iter().map(|r| request_view(r, now)).collect()))
}

/// # Approve a badge change
///
/// A different staff member than the one who asked, never the person
/// concerned, with the right to make the change and a fresh step-up.
#[post("/badges/requests/<id>/approve")]
pub async fn approve(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<StaffBadgeChange>> {
    staff.require_step_up(db, StaffPermission::BadgesManage).await?;
    let request = db.fetch_badge_request(id).await?;
    let grant = approve_badge_request(db, &actor(&staff), id).await?;
    crate::util::official::badge(db, &request.user_id, request.grant, &request.badge, &request.reason).await;
    let decided = db.fetch_badge_request(id).await?;
    Ok(Json(StaffBadgeChange {
        status: "applied",
        grant: Some(admin_grant_view(grant, now_ms())),
        request: Some(request_view(&decided, now_ms())),
    }))
}

/// # Refuse a badge change
///
/// Someone with the right to make it, or the one who asked (withdraw).
#[post("/badges/requests/<id>/refuse")]
pub async fn refuse(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<BadgeRequestView>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let decided = refuse_badge_request(db, &actor(&staff), id).await?;
    Ok(Json(request_view(&decided, now_ms())))
}

/// A server's community badge count, as the console shows it: counts and
/// times only, never member ids
#[derive(Serialize, Debug)]
pub struct CommunityCheckView {
    pub server_id: String,
    pub server_name: Option<String>,
    pub owner_id: String,
    pub members: u32,
    pub counted: u32,
    pub join_burst: u32,
    pub signup_burst: u32,
    /// Clear, Held, Released or Refused
    pub status: String,
    pub builder_to: Option<String>,
    pub big_to: Option<String>,
    pub checked_at: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<i64>,
}

fn community_view(check: CommunityBadgeCheck, server_name: Option<String>) -> CommunityCheckView {
    CommunityCheckView {
        server_id: check.id,
        server_name,
        owner_id: check.owner_id,
        members: check.members,
        counted: check.counted,
        join_burst: check.join_burst,
        signup_burst: check.signup_burst,
        status: format!("{:?}", check.status),
        builder_to: check.builder_to,
        big_to: check.big_to,
        checked_at: check.checked_at,
        decided_by: check.decided_by,
        decided_at: check.decided_at,
    }
}

/// # Community badge counts
///
/// Servers counted for Community Builder and Big Community; `status=Held`
/// lists the ones waiting for a staff look.
#[get("/badges/community?<status>")]
pub async fn community_checks(
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
) -> Result<Json<Vec<CommunityCheckView>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let status = match status {
        None => None,
        Some("Held") => Some(CommunityCheckStatus::Held),
        Some("Clear") => Some(CommunityCheckStatus::Clear),
        Some("Released") => Some(CommunityCheckStatus::Released),
        Some("Refused") => Some(CommunityCheckStatus::Refused),
        Some(_) => return Err(revolt_result::create_error!(InvalidOperation)),
    };
    let checks = db.fetch_community_checks(status, 200).await?;
    let ids: Vec<String> = checks.iter().map(|c| c.id.clone()).collect();
    let servers = db.fetch_servers(&ids).await?;
    Ok(Json(
        checks
            .into_iter()
            .map(|check| {
                let name = servers.iter().find(|s| s.id == check.id).map(|s| s.name.clone());
                community_view(check, name)
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct DataCommunityDecision {
    pub reason: String,
}

async fn decide_community(
    db: &Database,
    staff: &StaffContext,
    server: &str,
    release: bool,
    reason: &str,
) -> Result<Json<CommunityCheckView>> {
    let check = decide_community_check(
        db,
        &actor(staff),
        server,
        release,
        reason,
        CommunityThresholds::default(),
        now_ms(),
    )
    .await?;
    let name = db.fetch_server(server).await.ok().map(|s| s.name);
    Ok(Json(community_view(check, name)))
}

/// # Release a held server
///
/// The counted members joined in a burst; a staff member looked and lets
/// the badges through (given at once while the count still earns them).
#[post("/badges/community/<server>/release", data = "<data>")]
pub async fn community_release(
    db: &State<Database>,
    staff: StaffContext,
    server: &str,
    data: Json<DataCommunityDecision>,
) -> Result<Json<CommunityCheckView>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    decide_community(db, &staff, server, true, &data.reason).await
}

/// # Refuse a server's community badges
///
/// The members look manipulated: the server gives no community badge until
/// a staff member releases it.
#[post("/badges/community/<server>/refuse", data = "<data>")]
pub async fn community_refuse(
    db: &State<Database>,
    staff: StaffContext,
    server: &str,
    data: Json<DataCommunityDecision>,
) -> Result<Json<CommunityCheckView>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    decide_community(db, &staff, server, false, &data.reason).await
}
