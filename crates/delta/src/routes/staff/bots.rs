//! Bots in the admin console: list and look up bots, disable one (through
//! a staff case, so the owner sees it and can appeal), and review
//! verification requests.
//!
//! - `platform.users.read`: list bots and see one
//! - `platform.reports.resolve`: disable a bot
//! - `platform.servers.read`: see the verification queue
//! - `platform.servers.restrict`: verify, deny, take the mark away
//!
//! Every change is written to the platform audit log first.

use revolt_database::{
    record_platform_audit, AuditOutcome, BotInformation, BotVerification, BotVerificationStatus,
    Database, PartialBot, PartialUser, StaffPermission,
};
use revolt_models::v0::{
    BotFlags, DataSafetyAction, SafetyAction, SafetyActionKind, SafetyCategory, SafetyTargetKind,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::{Deserialize, Serialize};

use super::{reason, StaffContext};
use crate::routes::{
    bots::verification::{status_name, VERIFIED},
    safety::{actions::take_as, reports::open_staff_case},
};

const SUSPENDED: i32 = BotFlags::Suspended as i32;
const OFFICIAL: i32 = BotFlags::Official as i32;

#[derive(Serialize)]
pub struct BotRow {
    pub id: String,
    pub username: Option<String>,
    pub discriminator: Option<String>,
    pub owner_id: String,
    pub owner_username: Option<String>,
    pub public: bool,
    pub discoverable: bool,
    pub verified: bool,
    pub official: bool,
    pub suspended: bool,
}

#[derive(Serialize)]
pub struct VerificationView {
    pub id: String,
    pub bot_id: String,
    pub owner_id: String,
    pub statement: String,
    pub website: Option<String>,
    pub support: Option<String>,
    pub privacy_policy_url: Option<String>,
    pub terms_of_service_url: Option<String>,
    pub intents: i64,
    pub status: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
    pub decided_by: Option<String>,
    pub decision_note: Option<String>,
}

fn view(r: BotVerification) -> VerificationView {
    VerificationView {
        status: status_name(r.status).to_string(),
        id: r.id,
        bot_id: r.bot_id,
        owner_id: r.owner_id,
        statement: r.statement,
        website: r.website,
        support: r.support,
        privacy_policy_url: r.privacy_policy_url,
        terms_of_service_url: r.terms_of_service_url,
        intents: r.intents,
        created_at: r.created_at,
        decided_at: r.decided_at,
        decided_by: r.decided_by,
        decision_note: r.decision_note,
    }
}

#[derive(Serialize)]
pub struct BotDetail {
    #[serde(flatten)]
    pub row: BotRow,
    pub servers: usize,
    pub interactions_url: Option<String>,
    pub terms_of_service_url: Option<String>,
    pub privacy_policy_url: Option<String>,
    pub verifications: Vec<VerificationView>,
}

async fn rows(db: &Database, bots: Vec<revolt_database::Bot>) -> Result<Vec<BotRow>> {
    let mut ids: Vec<String> = bots.iter().map(|b| b.id.clone()).collect();
    ids.extend(bots.iter().map(|b| b.owner.clone()));
    ids.sort();
    ids.dedup();
    let users = db.fetch_users(&ids).await?;
    let find = |id: &str| users.iter().find(|u| u.id == id);
    Ok(bots
        .into_iter()
        .map(|bot| {
            let flags = bot.flags.unwrap_or_default();
            let user = find(&bot.id);
            BotRow {
                username: user.map(|u| u.username.clone()),
                discriminator: user.map(|u| u.discriminator.clone()),
                owner_username: find(&bot.owner).map(|u| format!("{}#{}", u.username, u.discriminator)),
                owner_id: bot.owner,
                public: bot.public,
                discoverable: bot.discoverable,
                verified: flags & VERIFIED != 0,
                official: flags & OFFICIAL != 0,
                suspended: flags & SUSPENDED != 0,
                id: bot.id,
            }
        })
        .collect())
}

/// # Bots
///
/// Newest first; `q` is a bot id or an owner's user id.
#[get("/bots?<q>&<before>&<limit>")]
pub async fn list(
    db: &State<Database>,
    staff: StaffContext,
    q: Option<&str>,
    before: Option<&str>,
    limit: Option<usize>,
) -> Result<Json<Vec<BotRow>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    if q.is_some_and(|q| q.len() > 64) {
        return Err(create_error!(FailedValidation {
            error: "query too long".to_string()
        }));
    }
    let bots = db
        .staff_list_bots(q, before, limit.unwrap_or(50).clamp(1, 100))
        .await?;
    rows(db, bots).await.map(Json)
}

/// # One bot
#[get("/bots/<id>")]
pub async fn detail(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<BotDetail>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let bot = db.fetch_bot(id).await?;
    let servers = db.fetch_server_count(&bot.id).await.unwrap_or(0);
    let verifications = db
        .fetch_bot_verifications_for_bot(&bot.id)
        .await?
        .into_iter()
        .map(view)
        .collect();
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let (interactions_url, terms, privacy) = (
        some(&bot.interactions_url),
        some(&bot.terms_of_service_url),
        some(&bot.privacy_policy_url),
    );
    let row = rows(db, vec![bot]).await?.pop().ok_or_else(|| create_error!(NotFound))?;
    Ok(Json(BotDetail {
        row,
        servers,
        interactions_url,
        terms_of_service_url: terms,
        privacy_policy_url: privacy,
        verifications,
    }))
}

#[derive(Deserialize)]
pub struct DataDisableBot {
    pub category: SafetyCategory,
    /// Shown to the owner in their account standing
    pub public_reason: String,
    pub internal_note: Option<String>,
}

/// # Disable a bot
///
/// The bot can't sign in and is unlisted. A staff case is opened, so the
/// owner sees the reason and can appeal; reverse it from the case.
#[post("/bots/<id>/disable", data = "<data>")]
pub async fn disable(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataDisableBot>,
) -> Result<Json<SafetyAction>> {
    staff.require(db, StaffPermission::ReportsResolve).await?;
    let data = data.into_inner();
    let public_reason = reason(&data.public_reason)?;
    let bot = db.fetch_bot(id).await?;
    let bot_user = db.fetch_user(&bot.id).await?;
    let mut record = staff.record("staff.bot.disable", Some(StaffPermission::ReportsResolve));
    record.target_kind = Some("bot".to_string());
    record.target_id = Some(bot.id.clone());
    record.reason = Some(public_reason.clone());
    record.summary = Some(format!("owner={}", bot.owner));
    let protected = bot.owner == staff.user.id
        || db
            .fetch_staff_grant(&bot.owner)
            .await?
            .is_some_and(|g| g.is_active(staff.now));
    if protected {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    let result = async {
        let case = open_staff_case(
            db,
            &staff.user,
            SafetyTargetKind::Bot,
            &bot_user,
            &bot.owner,
            data.category,
            data.internal_note.as_deref().unwrap_or(&public_reason),
        )
        .await?;
        take_as(
            db,
            &staff.user,
            &super::safety::bridged(&staff),
            &case.id,
            DataSafetyAction {
                kind: SafetyActionKind::DisableBot,
                category: Some(data.category),
                public_reason: public_reason.clone(),
                internal_note: data.internal_note.clone(),
                duration_days: None,
                reporter_note: None,
                version: case.version,
            },
        )
        .await
    }
    .await;
    if let Err(error) = &result {
        let mut failed = record;
        failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
    }
    result.map(Json)
}

#[derive(Serialize)]
pub struct VerificationEntry {
    #[serde(flatten)]
    pub request: VerificationView,
    pub bot: Option<BotRow>,
    pub servers: usize,
}

/// # Verification requests
///
/// `status` is Pending (default), Approved, Denied, Revoked or all; oldest
/// first.
#[get("/bot-verifications?<status>&<after>")]
pub async fn verifications(
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
    after: Option<&str>,
) -> Result<Json<Vec<VerificationEntry>>> {
    staff.require(db, StaffPermission::ServersRead).await?;
    let status = match status {
        None | Some("Pending") => Some(BotVerificationStatus::Pending),
        Some("Approved") => Some(BotVerificationStatus::Approved),
        Some("Denied") => Some(BotVerificationStatus::Denied),
        Some("Revoked") => Some(BotVerificationStatus::Revoked),
        Some("all") => None,
        _ => return Err(create_error!(InvalidOperation)),
    };
    let requests = db
        .fetch_bot_verifications(status, after.map(str::to_string), 50)
        .await?;
    let mut entries = Vec::with_capacity(requests.len());
    for request in requests {
        let bot = match db.fetch_bot(&request.bot_id).await {
            Ok(bot) => rows(db, vec![bot]).await?.pop(),
            Err(_) => None,
        };
        let servers = db.fetch_server_count(&request.bot_id).await.unwrap_or(0);
        entries.push(VerificationEntry {
            request: view(request),
            bot,
            servers,
        });
    }
    Ok(Json(entries))
}

/// Sets or clears the verified flag and the mark on the bot's profile;
/// `intents` are added to the approved privileged intents
async fn set_verified(db: &Database, bot_id: &str, verified: bool, intents: i64) -> Result<()> {
    let mut bot = db.fetch_bot(bot_id).await?;
    let flags = bot.flags.unwrap_or_default();
    let flags = if verified { flags | VERIFIED } else { flags & !VERIFIED };
    let approved_intents = (intents != 0).then_some(bot.approved_intents | intents);
    bot.update(
        db,
        PartialBot {
            flags: Some(flags),
            approved_intents,
            ..Default::default()
        },
        vec![],
    )
    .await?;
    if let Err(error) = revolt_database::sync_verified_bot_developer(db, &bot.owner).await {
        log::error!("verified developer badge for {}: {error:?}", bot.owner);
    }
    let mut user = db.fetch_user(&bot.id).await?;
    user.update(
        db,
        PartialUser {
            bot: Some(BotInformation {
                owner: bot.owner.clone(),
                verified,
            }),
            ..Default::default()
        },
        vec![],
    )
    .await
}

#[derive(Deserialize)]
pub struct DataVerificationDecision {
    pub approve: bool,
    /// Shown to the owner; required to deny
    pub note: Option<String>,
}

/// # Decide a verification request
#[post("/bot-verifications/<id>", data = "<data>")]
pub async fn decide_verification(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataVerificationDecision>,
) -> Result<Json<VerificationView>> {
    staff.require(db, StaffPermission::ServersRestrict).await?;
    let data = data.into_inner();
    let note = match data.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(note) => Some(reason(note)?),
        None if data.approve => None,
        None => {
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::new("note", "required"),
            ]))
        }
    };
    let mut request = db.fetch_bot_verification(id).await?;
    if request.status != BotVerificationStatus::Pending {
        return Err(create_error!(InvalidOperation));
    }
    let bot = db.fetch_bot(&request.bot_id).await?;
    // nobody verifies their own bot
    if bot.owner == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let mut record = staff.record("staff.bot.verification", Some(StaffPermission::ServersRestrict));
    record.target_kind = Some("bot".to_string());
    record.target_id = Some(bot.id.clone());
    record.reason = note.clone();
    record.summary = Some(if data.approve { "approve" } else { "deny" }.to_string());
    record_platform_audit(db, record, AuditOutcome::Success).await?;

    request.status = if data.approve {
        BotVerificationStatus::Approved
    } else {
        BotVerificationStatus::Denied
    };
    request.decided_at = Some(staff.now);
    request.decided_by = Some(staff.user.id.clone());
    request.decision_note = note;
    // two reviewers at once: only the first decision counts
    if !db
        .decide_bot_verification(&request, BotVerificationStatus::Pending)
        .await?
    {
        return Err(create_error!(InvalidOperation));
    }
    if data.approve {
        set_verified(db, &bot.id, true, request.intents).await?;
    }
    let bot_name = db.fetch_user(&bot.id).await.map(|u| u.username).unwrap_or_default();
    let key = if data.approve { "bot.verified" } else { "bot.denied" };
    crate::util::official::bot(db, &bot.owner, key, &bot_name, request.decision_note.as_deref()).await;
    Ok(Json(view(request)))
}

#[derive(Deserialize)]
pub struct DataUnverify {
    pub reason: String,
}

/// # Take the verified mark away
#[post("/bots/<id>/unverify", data = "<data>")]
pub async fn unverify(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataUnverify>,
) -> Result<()> {
    staff.require(db, StaffPermission::ServersRestrict).await?;
    let why = reason(&data.reason)?;
    let bot = db.fetch_bot(id).await?;
    if bot.flags.unwrap_or_default() & VERIFIED == 0 {
        return Err(create_error!(NoEffect));
    }
    let mut record = staff.record("staff.bot.unverify", Some(StaffPermission::ServersRestrict));
    record.target_kind = Some("bot".to_string());
    record.target_id = Some(bot.id.clone());
    record.reason = Some(why.clone());
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    set_verified(db, &bot.id, false, 0).await?;
    if let Some(mut approved) = db
        .fetch_bot_verifications_for_bot(&bot.id)
        .await?
        .into_iter()
        .find(|r| r.status == BotVerificationStatus::Approved)
    {
        approved.status = BotVerificationStatus::Revoked;
        approved.decided_at = Some(staff.now);
        approved.decided_by = Some(staff.user.id.clone());
        approved.decision_note = Some(why.clone());
        db.decide_bot_verification(&approved, BotVerificationStatus::Approved)
            .await?;
    }
    let name = db.fetch_user(&bot.id).await.map(|u| u.username).unwrap_or_default();
    crate::util::official::bot(db, &bot.owner, "bot.unverified", &name, Some(&why)).await;
    Ok(())
}
