//! Closed beta program. Membership in `beta_members` is the only thing
//! that unlocks a beta feature ([`revolt_database::beta_access`]); the
//! Beta Tester badge just shows it. Mounted at `/beta`.

use revolt_database::{
    beta_access, beta_features_for, platform_permissions, sync_beta_badges, BetaFeature, BADGE_SYSTEM_ACTOR,
    BetaMember, BetaStatus, Database, PlatformPermission, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::{serde::json::Json, Route, State};

use super::{beta_view, no_bots, now_ms, validated};
use crate::routes::community::{accounts_for, require};

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        me,
        request,
        accept,
        leave,
        access,
        admin_features,
        admin_save_feature,
        admin_members,
        admin_member
    ]
}

async fn membership(db: &Database, member: &BetaMember) -> Result<Json<v0::BetaMembership>> {
    let features = beta_features_for(db, member).await?;
    Ok(Json(beta_view(member, features)))
}

/// # My beta membership
#[openapi(tag = "Beta")]
#[get("/@me")]
pub async fn me(db: &State<Database>, user: User) -> Result<Json<Option<v0::BetaMembership>>> {
    no_bots(&user)?;
    Ok(Json(match db.fetch_beta_member(&user.id).await? {
        Some(member) => Some(membership(db, &member).await?.into_inner()),
        None => None,
    }))
}

/// # Ask to join the beta
///
/// The program is closed: this only puts you on the list for staff to
/// approve.
#[openapi(tag = "Beta")]
#[post("/@me/request")]
pub async fn request(db: &State<Database>, user: User) -> Result<Json<v0::BetaMembership>> {
    no_bots(&user)?;
    if db.fetch_beta_member(&user.id).await?.is_some() {
        return Err(create_error!(NoEffect));
    }
    let member = BetaMember {
        id: user.id.clone(),
        status: BetaStatus::Requested,
        features: vec![],
        approved_by: None,
        created_at: now_ms(),
        joined_at: None,
        ended_at: None,
        note: None,
        contributions: 0,
    };
    db.save_beta_member(&member).await?;
    membership(db, &member).await
}

/// # Accept a beta invitation
#[openapi(tag = "Beta")]
#[post("/@me/accept")]
pub async fn accept(db: &State<Database>, user: User) -> Result<Json<v0::BetaMembership>> {
    no_bots(&user)?;
    let mut member = db
        .fetch_beta_member(&user.id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    if member.status != BetaStatus::Invited {
        return Err(create_error!(InvalidOperation));
    }
    member.status = BetaStatus::Active;
    member.joined_at = Some(now_ms());
    db.save_beta_member(&member).await?;
    sync_beta_badges(db, &member, BADGE_SYSTEM_ACTOR).await?;
    membership(db, &member).await
}

/// # Leave the beta
///
/// Beta features stop working right away. You keep Beta Veteran if you
/// already completed it.
#[openapi(tag = "Beta")]
#[post("/@me/leave")]
pub async fn leave(db: &State<Database>, user: User) -> Result<Json<v0::BetaMembership>> {
    no_bots(&user)?;
    let mut member = db
        .fetch_beta_member(&user.id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    if !matches!(
        member.status,
        BetaStatus::Active | BetaStatus::Invited | BetaStatus::Requested
    ) {
        return Err(create_error!(InvalidOperation));
    }
    member.status = BetaStatus::Removed;
    member.ended_at = Some(now_ms());
    db.save_beta_member(&member).await?;
    sync_beta_badges(db, &member, BADGE_SYSTEM_ACTOR).await?;
    membership(db, &member).await
}

/// # Can I use a beta feature?
///
/// Checked against your membership and the feature flag; this is the
/// same check the server uses.
#[openapi(tag = "Beta")]
#[get("/features/<feature>")]
pub async fn access(db: &State<Database>, user: User, feature: &str) -> Result<Json<v0::BetaAccess>> {
    Ok(Json(v0::BetaAccess {
        feature: feature.to_string(),
        allowed: beta_access(db, &user, feature).await?,
    }))
}

async fn beta_staff(db: &Database, user: &User) -> Result<()> {
    no_bots(user)?;
    let perms = platform_permissions(db, user).await?;
    require(&perms, PlatformPermission::BetaManage)
}

fn feature_view(feature: BetaFeature) -> v0::BetaFeature {
    v0::BetaFeature {
        id: feature.id,
        name: feature.name,
        description: feature.description,
        enabled: feature.enabled,
        all_testers: feature.all_testers,
        updated_at: feature.updated_at,
    }
}

/// # Beta features
#[openapi(tag = "Beta")]
#[get("/admin/features")]
pub async fn admin_features(db: &State<Database>, user: User) -> Result<Json<Vec<v0::BetaFeature>>> {
    beta_staff(db, &user).await?;
    features_list(db).await.map(Json)
}

/// Every beta feature, by id
pub(crate) async fn features_list(db: &Database) -> Result<Vec<v0::BetaFeature>> {
    let mut features = db.fetch_beta_features().await?;
    features.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(features.into_iter().map(feature_view).collect())
}

/// # Create or change a beta feature
///
/// Switch it on or off, or give it to every active tester.
#[openapi(tag = "Beta")]
#[post("/admin/features", data = "<data>")]
pub async fn admin_save_feature(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataBetaFeature>,
) -> Result<Json<v0::BetaFeature>> {
    beta_staff(db, &user).await?;
    save_feature(db, &user.id, data.into_inner()).await.map(Json)
}

/// Create or change a beta feature (callers check who may)
pub(crate) async fn save_feature(db: &Database, actor_id: &str, data: v0::DataBetaFeature) -> Result<v0::BetaFeature> {
    let data = validated(data)?;
    if !data
        .id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(create_error!(FailedValidation {
            error: "id may only use a-z, 0-9, - and _".to_string()
        }));
    }
    let feature = BetaFeature {
        id: data.id,
        name: data.name.trim().to_string(),
        description: data
            .description
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty()),
        enabled: data.enabled,
        all_testers: data.all_testers,
        updated_at: now_ms(),
        updated_by: actor_id.to_string(),
    };
    db.save_beta_feature(&feature).await?;
    Ok(feature_view(feature))
}

async fn member_record(db: &Database, member: BetaMember) -> Result<v0::BetaMemberRecord> {
    let user = accounts_for(db, [member.id.clone()])
        .await?
        .pop()
        .ok_or_else(|| create_error!(NotFound))?;
    Ok(v0::BetaMemberRecord {
        user,
        status: format!("{:?}", member.status),
        features: member.features,
        created_at: member.created_at,
        joined_at: member.joined_at,
        ended_at: member.ended_at,
        contributions: member.contributions,
        note: member.note,
    })
}

/// # Beta members
#[openapi(tag = "Beta")]
#[get("/admin/members?<status>")]
pub async fn admin_members(
    db: &State<Database>,
    user: User,
    status: Option<String>,
) -> Result<Json<Vec<v0::BetaMemberRecord>>> {
    beta_staff(db, &user).await?;
    members_list(db, status.as_deref()).await.map(Json)
}

/// Beta members, optionally with one status
pub(crate) async fn members_list(db: &Database, status: Option<&str>) -> Result<Vec<v0::BetaMemberRecord>> {
    let status = match status {
        None | Some("") => None,
        Some("Requested") => Some(BetaStatus::Requested),
        Some("Invited") => Some(BetaStatus::Invited),
        Some("Active") => Some(BetaStatus::Active),
        Some("Removed") => Some(BetaStatus::Removed),
        Some("Completed") => Some(BetaStatus::Completed),
        Some(_) => return Err(create_error!(InvalidOperation)),
    };
    let mut records = vec![];
    for member in db.fetch_beta_members(status).await? {
        if let Ok(record) = member_record(db, member).await {
            records.push(record);
        }
    }
    Ok(records)
}

/// Contributions (feedback or bug reports) needed to complete the beta
pub const BETA_COMPLETION_CONTRIBUTIONS: i64 = 1;

/// # Change a beta membership
///
/// `invite`, `approve` (a request or invitation), `remove`, `complete`
/// (needs an active tester with at least one feedback or bug report;
/// gives Beta Veteran) or `features` (which beta features they get).
#[openapi(tag = "Beta")]
#[post("/admin/members", data = "<data>")]
pub async fn admin_member(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataBetaMember>,
) -> Result<Json<v0::BetaMemberRecord>> {
    beta_staff(db, &user).await?;
    member_change(db, &user.id, data.into_inner()).await.map(Json)
}

/// Invite, approve, remove, complete or set features (callers check who may)
pub(crate) async fn member_change(
    db: &Database,
    actor_id: &str,
    data: v0::DataBetaMember,
) -> Result<v0::BetaMemberRecord> {
    let data = validated(data)?;
    let subject = db.fetch_user(&data.user).await?;
    no_bots(&subject)?;
    // staff can't put themselves in the beta (that would grant the badge)
    if subject.id == actor_id {
        return Err(create_error!(NotPrivileged));
    }
    if let Some(features) = &data.features {
        let known = db.fetch_beta_features().await?;
        if let Some(missing) = features.iter().find(|f| !known.iter().any(|k| &k.id == *f)) {
            return Err(create_error!(FailedValidation {
                error: format!("unknown beta feature {missing}")
            }));
        }
    }
    let now = now_ms();
    let existing = db.fetch_beta_member(&subject.id).await?;
    let mut member = existing.clone().unwrap_or(BetaMember {
        id: subject.id.clone(),
        status: BetaStatus::Removed,
        features: vec![],
        approved_by: None,
        created_at: now,
        joined_at: None,
        ended_at: None,
        note: None,
        contributions: 0,
    });
    let status = existing.as_ref().map(|m| m.status.clone());
    match (data.action.as_str(), status) {
        // a request is approved right away; anyone else gets an invitation
        ("invite", Some(BetaStatus::Requested)) | ("approve", Some(BetaStatus::Requested | BetaStatus::Invited)) => {
            member.status = BetaStatus::Active;
            member.joined_at = Some(now);
            member.ended_at = None;
        }
        ("invite", None | Some(BetaStatus::Removed | BetaStatus::Completed)) => {
            member.status = BetaStatus::Invited;
            member.ended_at = None;
        }
        ("remove", Some(BetaStatus::Active | BetaStatus::Invited | BetaStatus::Requested)) => {
            member.status = BetaStatus::Removed;
            member.ended_at = Some(now);
        }
        ("complete", Some(BetaStatus::Active)) => {
            if member.contributions < BETA_COMPLETION_CONTRIBUTIONS {
                return Err(create_error!(FailedValidation {
                    error: "a tester completes the beta after sending feedback or a bug report"
                        .to_string()
                }));
            }
            member.status = BetaStatus::Completed;
            member.ended_at = Some(now);
        }
        ("features", Some(_)) => {}
        _ => return Err(create_error!(InvalidOperation)),
    }
    if let Some(features) = data.features {
        member.features = features;
    }
    if let Some(note) = data.note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
        member.note = Some(note);
    }
    member.approved_by = Some(actor_id.to_string());
    db.save_beta_member(&member).await?;
    sync_beta_badges(db, &member, actor_id).await?;
    member_record(db, member).await
}
