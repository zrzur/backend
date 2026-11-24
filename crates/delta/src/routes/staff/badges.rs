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
    approve_badge_request, badge_request_open, mutate_badge, refuse_badge_request, BadgeActor,
    BadgeChange, BadgeChangeRequest, BadgeMutation, BadgeOutcome, BadgePath, Database, StaffPermission,
};
use revolt_models::v0::{AdminBadgeGrant, AdminUserBadges, BadgeInfo};
use revolt_result::Result;
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};

use super::StaffContext;
use crate::routes::badges::{admin_grant_view, catalogue, now_ms, user_record};

/// The badge rights of this staff session
fn actor(staff: &StaffContext) -> BadgeActor {
    BadgeActor {
        id: staff.user.id.clone(),
        path: BadgePath::StaffConsole,
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
    staff.require(db, StaffPermission::UsersRead).await?;
    user_record(db, &actor(&staff), id).await.map(Json)
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

async fn change(
    db: &Database,
    staff: &StaffContext,
    id: &str,
    data: DataBadgeChange,
    change: BadgeChange,
) -> Result<Json<StaffBadgeChange>> {
    let (badge, note) = (data.badge.clone(), data.reason.clone());
    let outcome = mutate_badge(
        db,
        &actor(staff),
        BadgeMutation {
            user_id: id.to_string(),
            badge: data.badge,
            change,
            reason: data.reason,
            expires_at: if change == BadgeChange::Grant { data.expires_at } else { None },
        },
    )
    .await?;
    Ok(Json(match outcome {
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
    }))
}

/// # Grant a badge
#[post("/users/<id>/badges/grant", data = "<data>")]
pub async fn grant(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataBadgeChange>,
) -> Result<Json<StaffBadgeChange>> {
    change(db, &staff, id, data.into_inner(), BadgeChange::Grant).await
}

/// # Revoke a badge
#[post("/users/<id>/badges/revoke", data = "<data>")]
pub async fn revoke(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataBadgeChange>,
) -> Result<Json<StaffBadgeChange>> {
    change(db, &staff, id, data.into_inner(), BadgeChange::Revoke).await
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
