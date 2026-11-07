//! Staff tools: grant, revoke and suspend creators, and set platform
//! permissions. Mounted at `/admin`. Every change is logged in
//! `creator_permissions`.

use std::collections::BTreeSet;

use revolt_database::{
    is_declared_adult, platform_permissions, safety_now, util::cdn_purge::purge_files_later,
    CommunityItemQuery, CommunityItemStatus, CreatorAction, CreatorPermissionLog, CreatorStanding, Database,
    PlatformPermission, PlatformPermissions, PlatformRole, User,
};
use revolt_models::v0::{
    self, SafetyAction, SafetyActionKind, SafetyCategory, SafetyRevocation, SafetyTarget,
    SafetyTargetKind,
};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::{serde::json::Json, Route, State};
use ulid::Ulid;
use validator::Validate;

use super::{accounts_for, now_ms, require};

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![list_creators, creator_action, creator_history, set_permissions]
}

fn record(user: v0::CreatorAccount, role: Option<&PlatformRole>) -> v0::CreatorRecord {
    v0::CreatorRecord {
        user,
        permissions: role
            .map(|r| r.permissions.iter().map(|p| p.name().to_string()).collect())
            .unwrap_or_default(),
        creator: role.and_then(|r| r.creator.as_ref()).map(|c| format!("{c:?}")),
        updated_at: role.map(|r| r.updated_at),
        updated_by: role.map(|r| r.updated_by.clone()),
    }
}

async fn target(db: &Database, id: &str) -> Result<User> {
    let user = db.fetch_user(id).await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    Ok(user)
}

/// The creator rank is 18+. The answer names the rule, and the console and
/// the app turn it into a sentence.
async fn require_adult(db: &Database, user: &User) -> Result<()> {
    if is_declared_adult(db, user).await {
        return Ok(());
    }
    Err(revolt_result::failed_validation(vec![
        revolt_result::ValidationProblem::new("user", "adult"),
    ]))
}

async fn save(
    db: &Database,
    actor_id: &str,
    user_id: &str,
    permissions: BTreeSet<PlatformPermission>,
    creator: Option<CreatorStanding>,
    action: CreatorAction,
    reason: Option<String>,
) -> Result<v0::CreatorRecord> {
    // a change of standing never changes the creator's own privacy choice
    let show_public_name = db
        .fetch_platform_role(user_id)
        .await?
        .map(|r| r.show_public_name)
        .unwrap_or(false);
    let role = PlatformRole {
        id: user_id.to_string(),
        permissions: permissions.into_iter().collect(),
        creator,
        show_public_name,
        updated_at: now_ms(),
        updated_by: actor_id.to_string(),
    };
    db.save_platform_role(&role).await?;
    db.insert_creator_log(&CreatorPermissionLog {
        id: Ulid::new().to_string(),
        user_id: user_id.to_string(),
        actor_id: actor_id.to_string(),
        action,
        permissions: role.permissions.clone(),
        reason: reason.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()),
        created_at: role.updated_at,
    })
    .await?;
    // the Creator badge follows the creator record
    if let Err(error) = revolt_database::sync_creator_badge(db, user_id).await {
        log::error!("creator badge for {user_id}: {error:?}");
    }
    let user = accounts_for(db, [user_id.to_string()])
        .await?
        .pop()
        .ok_or_else(|| create_error!(NotFound))?;
    Ok(record(user, Some(&role)))
}

/// # Creators
///
/// Without a query: everyone with a creator record or platform permission.
/// With a query: users matching it by name or id, with their standing.
/// Needs `cosmetics.manage_creators`.
#[openapi(tag = "Community")]
#[get("/creators?<options..>")]
pub async fn list_creators(
    db: &State<Database>,
    user: User,
    options: v0::OptionsCreatorSearch,
) -> Result<Json<Vec<v0::CreatorRecord>>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsManageCreators)?;
    options.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    creator_list(db, options.query.as_deref()).await.map(Json)
}

/// Creators and other platform-role holders, or users matching `query`
pub(crate) async fn creator_list(db: &Database, query: Option<&str>) -> Result<Vec<v0::CreatorRecord>> {
    let roles = match query.map(str::trim).filter(|q| !q.is_empty()) {
        Some(query) => {
            let ids: Vec<String> = db
                .search_users_by_name(query.trim_start_matches('@'), 25)
                .await?
                .into_iter()
                .filter(|u| u.bot.is_none())
                .map(|u| u.id)
                .collect();
            let roles = db.fetch_platform_roles(&ids).await?;
            let users = accounts_for(db, ids.clone()).await?;
            return Ok(ids
                .iter()
                .filter_map(|id| users.iter().find(|u| &u.id == id).cloned())
                .map(|u| {
                    let role = roles.iter().find(|r| r.id == u.id);
                    record(u, role)
                })
                .collect());
        }
        None => db.fetch_all_platform_roles().await?,
    };
    let users = accounts_for(db, roles.iter().map(|r| r.id.clone())).await?;
    Ok(roles
        .iter()
        .filter_map(|role| {
            users
                .iter()
                .find(|u| u.id == role.id)
                .cloned()
                .map(|u| record(u, Some(role)))
        })
        .collect())
}

/// # Change a creator
///
/// `grant` makes someone a creator, `suspend` pauses them, `revoke` takes
/// it away and `reinstate` gives it back. Suspended and revoked creators
/// can't submit. A suspended creator's designs are hidden from everyone
/// until they are reinstated; a revoked creator's stay. Suspending needs a
/// reason, shown to the creator. `grant` and `reinstate` need someone who
/// declared an age of 18 or over. Needs `cosmetics.manage_creators`; it can't
/// be bought or come from a server.
#[openapi(tag = "Community")]
#[post("/creators", data = "<data>")]
pub async fn creator_action(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataCreatorAction>,
) -> Result<Json<v0::CreatorRecord>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsManageCreators)?;
    creator_change(db, &user.id, data.into_inner()).await.map(Json)
}

/// Grant, suspend, revoke or reinstate a creator (callers check who may)
pub(crate) async fn creator_change(
    db: &Database,
    actor_id: &str,
    data: v0::DataCreatorAction,
) -> Result<v0::CreatorRecord> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let subject = target(db, &data.user).await?;
    // staff can't make themselves creators (that would grant the badge)
    if subject.id == actor_id {
        return Err(create_error!(NotPrivileged));
    }
    let role = db.fetch_platform_role(&subject.id).await?;
    let was_suspended = role
        .as_ref()
        .is_some_and(|r| r.creator == Some(CreatorStanding::Suspended));
    let public_reason = data
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(str::to_string);
    if data.action == "suspend" && public_reason.is_none() {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("reason", "required"),
        ]));
    }
    if matches!(data.action.as_str(), "grant" | "reinstate") {
        require_adult(db, &subject).await?;
    }
    let mut permissions: BTreeSet<_> = role
        .as_ref()
        .map(|r| r.permissions.iter().copied().collect())
        .unwrap_or_default();

    let (standing, action) = match data.action.as_str() {
        "grant" | "reinstate" => {
            permissions.insert(PlatformPermission::CosmeticsSubmit);
            permissions.insert(PlatformPermission::CosmeticsManageOwn);
            let action = if role.as_ref().and_then(|r| r.creator.as_ref()).is_some() {
                CreatorAction::Reinstate
            } else {
                CreatorAction::Grant
            };
            (CreatorStanding::Active, action)
        }
        "suspend" => (CreatorStanding::Suspended, CreatorAction::Suspend),
        "revoke" => {
            permissions.remove(&PlatformPermission::CosmeticsSubmit);
            (CreatorStanding::Revoked, CreatorAction::Revoke)
        }
        _ => return Err(create_error!(InvalidOperation)),
    };
    let suspending = standing == CreatorStanding::Suspended && !was_suspended;
    let reinstating = standing == CreatorStanding::Active && was_suspended;
    let record = save(
        db,
        actor_id,
        &subject.id,
        permissions,
        Some(standing),
        action,
        data.reason,
    )
    .await?;
    if suspending {
        take_designs_down(db, &subject.id).await?;
        insert_suspension(db, actor_id, &subject.id, public_reason.unwrap_or_default()).await?;
    }
    if reinstating {
        close_suspensions(db, actor_id, &subject.id).await?;
    }
    Ok(record)
}

/// A suspended creator's designs are already hidden by their standing. This
/// removes what standing can't reach: people wearing them and the CDN.
async fn take_designs_down(db: &Database, creator_id: &str) -> Result<()> {
    const PAGE: usize = 100;
    let mut files = Vec::new();
    loop {
        let items = db
            .fetch_community_items(CommunityItemQuery {
                creator_id: Some(creator_id),
                statuses: vec![CommunityItemStatus::Published],
                limit: PAGE,
                offset: files.len(),
                ..Default::default()
            })
            .await?;
        let last_page = items.len() < PAGE;
        for item in items {
            db.unset_community_item_from_users(&item.id).await?;
            files.push(item.file);
        }
        if last_page {
            break;
        }
    }
    purge_files_later(files);
    Ok(())
}

/// The action a creator appeals against, on their standing
async fn insert_suspension(
    db: &Database,
    actor_id: &str,
    creator_id: &str,
    public_reason: String,
) -> Result<()> {
    db.insert_safety_action(&SafetyAction {
        id: Ulid::new().to_string(),
        case_id: None,
        actor_id: actor_id.to_string(),
        kind: SafetyActionKind::SuspendCreator,
        target: SafetyTarget {
            kind: SafetyTargetKind::User,
            id: creator_id.to_string(),
            facet: None,
            message_id: None,
        },
        subject_user_id: Some(creator_id.to_string()),
        category: SafetyCategory::Other,
        facts: public_reason.clone(),
        public_reason,
        internal_note: None,
        created_at: safety_now(),
        expires_at: None,
        revoked: None,
        appealable: true,
        automated_detection: false,
        automated_decision: false,
        ground: Default::default(),
        territorial_scope: "worldwide".to_string(),
        basis: Default::default(),
    })
    .await
}

/// A reinstated creator has no suspension left in force
async fn close_suspensions(db: &Database, actor_id: &str, creator_id: &str) -> Result<()> {
    for mut action in db.fetch_safety_actions_for_user(creator_id).await? {
        if action.kind == SafetyActionKind::SuspendCreator && action.revoked.is_none() {
            action.revoked = Some(SafetyRevocation {
                actor_id: actor_id.to_string(),
                reason: "reinstated".to_string(),
                at: safety_now(),
            });
            db.save_safety_action(&action).await?;
        }
    }
    Ok(())
}

/// An appeal was upheld or the action reversed: back to active. Files need
/// nothing, the designs come back with the standing.
pub(crate) async fn lift_creator_suspension(
    db: &Database,
    actor_id: &str,
    creator_id: &str,
    reason: String,
) -> Result<()> {
    let Some(role) = db.fetch_platform_role(creator_id).await? else {
        return Ok(());
    };
    if role.creator != Some(CreatorStanding::Suspended) {
        return Ok(());
    }
    let mut permissions: BTreeSet<_> = role.permissions.iter().copied().collect();
    permissions.insert(PlatformPermission::CosmeticsSubmit);
    permissions.insert(PlatformPermission::CosmeticsManageOwn);
    save(
        db,
        actor_id,
        creator_id,
        permissions,
        Some(CreatorStanding::Active),
        CreatorAction::Reinstate,
        Some(reason),
    )
    .await
    .map(|_| ())
}

/// # Creator history
///
/// Every change to one user's creator standing and permissions.
#[openapi(tag = "Community")]
#[get("/creators/<id>/history")]
pub async fn creator_history(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<Vec<v0::CreatorLogEntry>>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsManageCreators)?;
    Ok(Json(
        db.fetch_creator_logs(id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// # Set platform permissions
///
/// Make someone a reviewer or creator manager. Only admins with
/// `cosmetics.manage_all` (or privileged staff) can do this.
#[openapi(tag = "Community")]
#[post("/permissions", data = "<data>")]
pub async fn set_permissions(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataPlatformPermissions>,
) -> Result<Json<v0::CreatorRecord>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsManageAll)?;
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let subject = target(db, &data.user).await?;
    // nobody changes their own platform permissions
    if subject.id == user.id {
        return Err(create_error!(NotPrivileged));
    }
    let permissions = data
        .permissions
        .iter()
        .map(|name| {
            PlatformPermission::from_name(name).ok_or_else(|| {
                create_error!(FailedValidation {
                    error: format!("unknown permission {name}")
                })
            })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    // badge, beta and bug permissions (added or removed) need a privileged
    // account; a design admin can only hand out design permissions
    if !user.privileged {
        let current: BTreeSet<_> = db
            .fetch_platform_role(&subject.id)
            .await?
            .map(|r| r.permissions.into_iter().collect())
            .unwrap_or_default();
        if permissions
            .symmetric_difference(&current)
            .any(|p| !PlatformPermissions::is_cosmetics(*p))
        {
            return Err(create_error!(NotPrivileged));
        }
    }
    let held = db.fetch_platform_role(&subject.id).await?;
    let adds_submit = permissions.contains(&PlatformPermission::CosmeticsSubmit)
        && !held
            .as_ref()
            .is_some_and(|r| r.permissions.contains(&PlatformPermission::CosmeticsSubmit));
    if adds_submit {
        require_adult(db, &subject).await?;
    }
    let mut standing = held.and_then(|r| r.creator);
    if permissions.contains(&PlatformPermission::CosmeticsSubmit) && standing.is_none() {
        standing = Some(CreatorStanding::Active);
    }
    Ok(Json(
        save(
            db,
            &user.id,
            &subject.id,
            permissions,
            standing,
            CreatorAction::SetPermissions,
            data.reason,
        )
        .await?,
    ))
}
