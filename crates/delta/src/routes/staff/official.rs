//! Giving and removing the official mark, with two people.
//!
//! One staff member proposes, a different one confirms; both need
//! `platform.official.manage` and a fresh TOTP step-up, and neither can be
//! the account in question. Every step goes to the audit log. Nothing else
//! writes the mark: no public route, not the raw edit of a user.

use revolt_database::{
    is_seeded_official, passes_for_official, record_platform_audit, AuditOutcome, AuditRecord, Database,
    NameRecord, NameScan, OfficialMarkRequest, OfficialMarkStatus, PartialUser, StaffPermission,
    User, OFFICIAL_MARK_REQUEST_TTL_MS,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use super::audited::{audited, audited_noting};
use super::{reason, StaffContext};

/// Move a request out of Pending, with the audit record written first.
///
/// The record comes before the status changes. `follow_up` is the change the
/// decision stands for; if it fails, the request is put back to Pending so
/// it can be decided again.
async fn decide_audited<T>(
    db: &Database,
    record: AuditRecord,
    decided: OfficialMarkRequest,
    follow_up: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let mut open = decided.clone();
    open.status = OfficialMarkStatus::Pending;
    open.decided_by = None;
    open.decided_at = None;
    audited_noting(db, record, async {
        if !db
            .decide_official_mark_request(&decided, OfficialMarkStatus::Pending)
            .await
            .map_err(|error| (error, None))?
        {
            return Err((create_error!(InvalidOperation), None));
        }
        match follow_up.await {
            Ok(value) => Ok(value),
            Err(error) => {
                let back = db.decide_official_mark_request(&open, decided.status).await;
                if matches!(back, Ok(true)) {
                    Err((error, None))
                } else {
                    log::error!("official mark request {} could not be put back", decided.id);
                    Err((error, Some("revert failed")))
                }
            }
        }
    })
    .await
}

/// Names read per request of the list of names that break the rule
const NAME_PAGE: i64 = 1000;

#[derive(Serialize)]
pub struct OfficialAccountRow {
    pub user_id: String,
    pub username: String,
    pub discriminator: String,
    pub display_name: Option<String>,
    /// Kept by the server: the mark can't be removed
    pub seeded: bool,
}

#[derive(Serialize)]
pub struct OfficialRequestView {
    pub id: String,
    pub user_id: String,
    pub username: Option<String>,
    pub grant: bool,
    pub requested_by: String,
    pub requested_by_name: Option<String>,
    pub reason: String,
    /// Pending, Confirmed, Refused, Withdrawn or Expired
    pub status: &'static str,
    pub created_at: i64,
    pub expires_at: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<i64>,
}

#[derive(Serialize)]
pub struct OfficialOverview {
    pub accounts: Vec<OfficialAccountRow>,
    pub requests: Vec<OfficialRequestView>,
}

fn status_of(request: &OfficialMarkRequest, now: i64) -> &'static str {
    match request.status {
        OfficialMarkStatus::Pending if now >= request.expires_at => "Expired",
        OfficialMarkStatus::Pending => "Pending",
        OfficialMarkStatus::Confirmed => "Confirmed",
        OfficialMarkStatus::Refused => "Refused",
        OfficialMarkStatus::Withdrawn => "Withdrawn",
    }
}

async fn name_of(db: &Database, id: &str) -> Option<String> {
    db.fetch_user(id).await.ok().map(|user| revolt_models::v0::user_handle(&user.username, &user.discriminator))
}

async fn view(db: &Database, request: &OfficialMarkRequest, now: i64) -> OfficialRequestView {
    OfficialRequestView {
        id: request.id.clone(),
        user_id: request.user_id.clone(),
        username: name_of(db, &request.user_id).await,
        grant: request.grant,
        requested_by: request.requested_by.clone(),
        requested_by_name: name_of(db, &request.requested_by).await,
        reason: request.reason.clone(),
        status: status_of(request, now),
        created_at: request.created_at,
        expires_at: request.expires_at,
        decided_by: request.decided_by.clone(),
        decided_at: request.decided_at,
    }
}

fn row(user: &User) -> OfficialAccountRow {
    OfficialAccountRow {
        user_id: user.id.clone(),
        username: user.username.clone(),
        discriminator: user.discriminator.clone(),
        display_name: user.display_name.clone(),
        seeded: is_seeded_official(&user.id),
    }
}

/// # Official accounts and open proposals
#[get("/official")]
pub async fn overview(db: &State<Database>, staff: StaffContext) -> Result<Json<OfficialOverview>> {
    staff.require(db, StaffPermission::OfficialManage).await?;
    let mut accounts: Vec<_> = db.fetch_official_users().await?.iter().map(row).collect();
    accounts.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    let mut requests = vec![];
    for request in db.fetch_official_mark_requests().await? {
        requests.push(view(db, &request, staff.now).await);
    }
    Ok(Json(OfficialOverview { accounts, requests }))
}

#[derive(Deserialize)]
pub struct DataOfficialProposal {
    /// `true` gives the mark, `false` removes it
    pub grant: bool,
    pub reason: String,
}

/// # Propose giving or removing the official mark
///
/// Nothing changes until a different staff member confirms.
#[post("/users/<id>/official", data = "<data>")]
pub async fn propose(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataOfficialProposal>,
) -> Result<Json<OfficialRequestView>> {
    staff.require_step_up(db, StaffPermission::OfficialManage).await?;
    let data = data.into_inner();
    let why = reason(&data.reason)?;
    let subject = db.fetch_user(id).await?;
    if subject.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if subject.id == staff.user.id {
        return Err(create_error!(CannotReportYourself));
    }
    if subject.official == data.grant {
        return Err(create_error!(NoEffect));
    }
    if !data.grant && is_seeded_official(&subject.id) {
        return Err(create_error!(InvalidOperation));
    }
    let open = db.fetch_official_mark_requests().await?;
    if open.iter().any(|r| r.user_id == subject.id && status_of(r, staff.now) == "Pending") {
        return Err(create_error!(InvalidOperation));
    }

    let request = OfficialMarkRequest {
        id: Ulid::new().to_string(),
        user_id: subject.id.clone(),
        grant: data.grant,
        requested_by: staff.user.id.clone(),
        reason: why.clone(),
        status: OfficialMarkStatus::Pending,
        created_at: staff.now,
        expires_at: staff.now + OFFICIAL_MARK_REQUEST_TTL_MS,
        decided_by: None,
        decided_at: None,
    };
    let mut record = staff.record("staff.official.propose", Some(StaffPermission::OfficialManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(subject.id.clone());
    record.reason = Some(why);
    record.summary = Some(format!("{} {}", if data.grant { "give" } else { "remove" }, request.id));
    audited(db, record, db.insert_official_mark_request(&request)).await?;
    Ok(Json(view(db, &request, staff.now).await))
}

/// # Confirm a proposal
///
/// Only a different staff member can, within 7 days. This is the only place
/// the mark changes after the first start.
#[post("/official/requests/<id>/confirm")]
pub async fn confirm(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
) -> Result<Json<OfficialRequestView>> {
    staff.require_step_up(db, StaffPermission::OfficialManage).await?;
    let mut request = db.fetch_official_mark_request(id).await?;
    let mut record = staff.record("staff.official.confirm", Some(StaffPermission::OfficialManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(request.user_id.clone());
    record.reason = Some(request.reason.clone());
    record.summary = Some(format!(
        "{} {} proposed by {}",
        if request.grant { "give" } else { "remove" },
        request.id,
        request.requested_by
    ));

    if request.requested_by == staff.user.id || request.user_id == staff.user.id {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    if status_of(&request, staff.now) != "Pending" {
        return Err(create_error!(InvalidOperation));
    }
    let mut subject = db.fetch_user(&request.user_id).await?;

    request.status = OfficialMarkStatus::Confirmed;
    request.decided_by = Some(staff.user.id.clone());
    request.decided_at = Some(staff.now);
    let official = request.grant;
    decide_audited(
        db,
        record,
        request.clone(),
        subject.update(
            db,
            PartialUser {
                official: Some(official),
                ..Default::default()
            },
            vec![],
        ),
    )
    .await?;
    Ok(Json(view(db, &request, staff.now).await))
}

#[derive(Deserialize)]
pub struct DataOfficialRefusal {
    pub reason: String,
}

/// # Refuse or withdraw a proposal
#[post("/official/requests/<id>/refuse", data = "<data>")]
pub async fn refuse(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataOfficialRefusal>,
) -> Result<Json<OfficialRequestView>> {
    staff.require_step_up(db, StaffPermission::OfficialManage).await?;
    let why = reason(&data.reason)?;
    let mut request = db.fetch_official_mark_request(id).await?;
    if status_of(&request, staff.now) != "Pending" {
        return Err(create_error!(InvalidOperation));
    }
    let own = request.requested_by == staff.user.id;
    request.status = if own { OfficialMarkStatus::Withdrawn } else { OfficialMarkStatus::Refused };
    request.decided_by = Some(staff.user.id.clone());
    request.decided_at = Some(staff.now);
    let action = if own { "staff.official.withdraw" } else { "staff.official.refuse" };
    let mut record = staff.record(action, Some(StaffPermission::OfficialManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(request.user_id.clone());
    record.reason = Some(why);
    record.summary = Some(request.id.clone());
    decide_audited(db, record, request.clone(), async { Ok(()) }).await?;
    Ok(Json(view(db, &request, staff.now).await))
}

#[derive(Serialize)]
pub struct NameConflicts {
    pub conflicts: Vec<NameRecord>,
    /// Ask again with this offset; `None` when this list is done
    pub next_offset: Option<u64>,
}

/// # Existing names that break the name rule
///
/// Read-only: nothing is changed. One page of one list (`users`, `nicknames`
/// or `webhooks`).
#[get("/official/name-conflicts?<list>&<offset>")]
pub async fn name_conflicts(
    db: &State<Database>,
    staff: StaffContext,
    list: &str,
    offset: Option<u64>,
) -> Result<Json<NameConflicts>> {
    staff.require(db, StaffPermission::OfficialManage).await?;
    let scan = match list {
        "users" => NameScan::Users,
        "nicknames" => NameScan::Nicknames,
        "webhooks" => NameScan::Webhooks,
        _ => return Err(create_error!(InvalidOperation)),
    };
    let offset = offset.unwrap_or_default();
    let page = db.fetch_name_records(scan, offset, NAME_PAGE).await?;
    let read = page.len() as u64;
    Ok(Json(NameConflicts {
        conflicts: page
            .into_iter()
            .filter(|record| !record.official && passes_for_official(&record.name))
            .collect(),
        next_offset: (read > 0).then_some(offset + NAME_PAGE as u64),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test::TestHarness;
    use revolt_database::PlatformAuditQuery;

    fn request(id: &str) -> OfficialMarkRequest {
        OfficialMarkRequest {
            id: id.to_string(),
            user_id: "subject".to_string(),
            grant: true,
            requested_by: "proposer".to_string(),
            reason: "test".to_string(),
            status: OfficialMarkStatus::Pending,
            created_at: 0,
            expires_at: i64::MAX,
            decided_by: None,
            decided_at: None,
        }
    }

    fn record() -> AuditRecord {
        AuditRecord {
            actor_id: "confirmer".to_string(),
            action: "staff.official.confirm".to_string(),
            permission: None,
            target_kind: None,
            target_id: None,
            reason: None,
            staff_session_id: None,
            summary: None,
        }
    }

    async fn status(harness: &TestHarness, id: &str) -> OfficialMarkStatus {
        harness.db.fetch_official_mark_request(id).await.unwrap().status
    }

    /// The record is written before the status changes, and a change that
    /// fails afterwards puts the request back to Pending (open again).
    #[rocket::async_test]
    async fn official_confirm_audit_failure_leaves_request_open() {
        let harness = TestHarness::new().await;
        let open = request("req-failing");
        harness.db.insert_official_mark_request(&open).await.unwrap();
        let mut decided = open.clone();
        decided.status = OfficialMarkStatus::Confirmed;
        decided.decided_by = Some("confirmer".to_string());
        decided.decided_at = Some(1);

        let result: Result<()> = decide_audited(&harness.db, record(), decided.clone(), async {
            Err(create_error!(NotFound))
        })
        .await;
        assert!(result.is_err());
        assert!(matches!(status(&harness, "req-failing").await, OfficialMarkStatus::Pending));

        let entries = harness
            .db
            .fetch_platform_audit(PlatformAuditQuery { limit: 100, ..Default::default() })
            .await
            .unwrap();
        let outcomes: Vec<String> = entries
            .iter()
            .filter(|e| e.action == "staff.official.confirm")
            .map(|e| format!("{:?}", e.outcome))
            .collect();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.contains(&"Success".to_string()) && outcomes.contains(&"Failed".to_string()));

        // when the change works, the request stays decided
        let ok: Result<()> = decide_audited(&harness.db, record(), decided, async { Ok(()) }).await;
        assert!(ok.is_ok());
        assert!(matches!(status(&harness, "req-failing").await, OfficialMarkStatus::Confirmed));
    }
}
