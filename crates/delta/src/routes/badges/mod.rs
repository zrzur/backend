//! Profile badges: the catalogue, your own badges, and read-only staff
//! views. Hand-made changes go through `revolt_database::mutate_badge` (the
//! one authorization gate); `/badges/admin/grant|revoke` are retired and
//! refuse every change, written to the audit log.

pub mod beta;
pub mod bugs;
pub mod command;
#[cfg(test)]
mod authorize_tests;
#[cfg(test)]
mod command_tests;
#[cfg(test)]
mod test;

use iso8601_timestamp::Timestamp;
use revolt_database::{
    badge_definition, badge_eligibility, badge_rule_check, beta_features_for, bug_hunter_stats,
    creator_eligibility, epoch_ms, mutate_badge, platform_permissions, set_badge_hidden,
    sync_creator_badge, sync_user_badges, BadgeActor, BadgeChange, BadgeDenial, BadgeGrant,
    BadgeGrantRule, BadgeMutation, BadgeOutcome, BadgePath, BadgeSource, BetaMember, Database,
    PlatformPermission, StaffPermission, User, BADGE_CATALOGUE, ELITE_MIN_VERIFIED,
    SUPER_CREATOR_MIN_DESIGNS,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::{serde::json::Json, Route, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use super::community::{accounts_for, require};

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        catalogue,
        mine,
        visibility,
        user_badges,
        admin_search,
        admin_user,
        admin_grant,
        admin_revoke
    ]
}

pub(crate) fn now_ms() -> i64 {
    epoch_ms(Timestamp::now_utc())
}

pub(crate) fn validated<T: Validate>(data: T) -> Result<T> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    Ok(data)
}

/// Bots never use staff tools or receive badges
pub(crate) fn no_bots(user: &User) -> Result<()> {
    if user.bot.is_some() {
        Err(create_error!(IsBot))
    } else {
        Ok(())
    }
}

fn rule_name(rule: BadgeGrantRule) -> &'static str {
    match rule {
        BadgeGrantRule::Dynamic => "dynamic",
        BadgeGrantRule::Automatic => "automatic",
        BadgeGrantRule::Staff => "staff",
        BadgeGrantRule::Privileged => "privileged",
        BadgeGrantRule::Locked => "locked",
        BadgeGrantRule::Appointment => "appointment",
        BadgeGrantRule::Historical => "historical",
    }
}

fn source_name(source: &BadgeSource) -> &'static str {
    match source {
        BadgeSource::Admin => "admin",
        BadgeSource::System => "system",
        BadgeSource::Legacy => "legacy",
    }
}

fn own(grant: BadgeGrant) -> v0::OwnBadge {
    v0::OwnBadge {
        source: source_name(&grant.source).to_string(),
        badge: grant.badge,
        granted_at: grant.granted_at,
        expires_at: grant.expires_at,
        revoked_at: grant.revoked_at,
        revoke_reason: grant.revoke_reason,
        hidden: grant.hidden,
    }
}

pub(crate) fn admin_grant_view(grant: BadgeGrant, now: i64) -> v0::AdminBadgeGrant {
    v0::AdminBadgeGrant {
        held: grant.held(now),
        source: source_name(&grant.source).to_string(),
        badge: grant.badge,
        source_ref: grant.source_ref,
        granted_by: grant.granted_by,
        granted_at: grant.granted_at,
        reason: grant.reason,
        expires_at: grant.expires_at,
        revoked_at: grant.revoked_at,
        revoked_by: grant.revoked_by,
        revoke_reason: grant.revoke_reason,
        hidden: grant.hidden,
    }
}

pub(crate) fn beta_view(member: &BetaMember, features: Vec<String>) -> v0::BetaMembership {
    v0::BetaMembership {
        status: format!("{:?}", member.status),
        features,
        joined_at: member.joined_at,
        ended_at: member.ended_at,
        contributions: member.contributions,
    }
}

/// # Badge catalogue
///
/// Every badge, its bit in `User.badges`, tier and how it is given out.
#[openapi(tag = "Badges")]
#[get("/catalogue")]
pub async fn catalogue() -> Json<Vec<v0::BadgeInfo>> {
    Json(
        BADGE_CATALOGUE
            .iter()
            .map(|b| v0::BadgeInfo {
                id: b.id.to_string(),
                bit: b.bit,
                category: b.category.to_string(),
                tier: b.tier,
                priority: b.priority,
                rule: rule_name(b.rule).to_string(),
                retired: b.retired,
            })
            .collect(),
    )
}

/// # My badges
///
/// Your badges (held, expired and revoked) and how close you are to the
/// ones you can earn.
#[openapi(tag = "Badges")]
#[get("/@me")]
pub async fn mine(db: &State<Database>, user: User) -> Result<Json<v0::MyBadges>> {
    no_bots(&user)?;
    // creators who published before badges existed get theirs here
    sync_creator_badge(db, &user.id).await?;
    let user = sync_user_badges(db, &user.id).await?;
    let mut badges: Vec<v0::OwnBadge> = db
        .fetch_badge_grants(&user.id)
        .await?
        .into_iter()
        .map(own)
        .collect();
    badges.sort_by_key(|b| badge_definition(&b.badge).map(|d| d.priority).unwrap_or(u16::MAX));

    let role = db.fetch_platform_role(&user.id).await?;
    let perms = platform_permissions(db, &user).await?;
    let (eligible, published) = creator_eligibility(db, &user.id).await?;
    let (verified, security_verified) = bug_hunter_stats(db, &user.id).await?;
    let beta = match db.fetch_beta_member(&user.id).await? {
        Some(member) => {
            let features = beta_features_for(db, &member).await?;
            Some(beta_view(&member, features))
        }
        None => None,
    };
    let plus = user.has_plus().await;
    Ok(Json(v0::MyBadges {
        badges,
        creator: v0::CreatorProgress {
            standing: role.and_then(|r| r.creator).map(|c| format!("{c:?}")),
            can_submit: perms.has(PlatformPermission::CosmeticsSubmit),
            published,
            eligible,
            super_creator_designs: SUPER_CREATOR_MIN_DESIGNS,
        },
        bugs: v0::BugProgress {
            verified,
            security_verified,
            elite_verified: ELITE_MIN_VERIFIED,
        },
        beta,
        plus,
        permissions: perms.names(),
        privileged: user.privileged,
    }))
}

/// # Show or hide a badge
///
/// Hide one of your badges from your profile, or show it again.
#[openapi(tag = "Badges")]
#[patch("/@me/<badge>", data = "<data>")]
pub async fn visibility(
    db: &State<Database>,
    user: User,
    badge: &str,
    data: Json<v0::DataBadgeVisibility>,
) -> Result<EmptyResponse> {
    no_bots(&user)?;
    // the founder's Founder badge follows `founder_id` and is always shown
    if badge == "founder" && revolt_config::config().await.api.users.founder().is_some() {
        return Err(create_error!(InvalidOperation));
    }
    set_badge_hidden(db, &user.id, badge, data.hidden).await?;
    Ok(EmptyResponse)
}

/// # A user's badges
///
/// When each shown badge was earned. Never says why or who granted it.
#[openapi(tag = "Badges")]
#[get("/users/<id>")]
pub async fn user_badges(
    db: &State<Database>,
    _user: User,
    id: &str,
) -> Result<Json<Vec<v0::PublicBadge>>> {
    let now = now_ms();
    let config = revolt_config::config().await;
    let grants = db.fetch_badge_grants(id).await?;
    Ok(Json(public_badges(grants, config.api.users.founder(), id, now)))
}

/// What others see of `id`'s badges. Founder follows `founder_id` alone
/// once it is set, the same as the badge bits (`founder_bit`): anyone
/// else's old founder grant is history, and the founder is listed even
/// without a grant row. The Founder badge can't be hidden then: it is a
/// fact of the platform, not a badge one wears by choice.
pub(crate) fn public_badges(grants: Vec<BadgeGrant>, founder: Option<&str>, id: &str, now: i64) -> Vec<v0::PublicBadge> {
    let is_founder = founder == Some(id);
    let mut shown: Vec<v0::PublicBadge> = grants
        .iter()
        .filter(|g| g.held(now))
        // computed badges (time on Zrzur) are in `User.badges` only: an
        // old stored grant of one is history
        .filter(|g| badge_definition(&g.badge).is_some_and(|b| b.rule != BadgeGrantRule::Dynamic))
        .filter(|g| match g.badge.as_str() {
            "founder" if founder.is_some() => is_founder,
            _ => !g.hidden,
        })
        .map(|g| v0::PublicBadge {
            badge: g.badge.clone(),
            granted_at: g.granted_at,
        })
        .collect();
    if is_founder && !shown.iter().any(|b| b.badge == "founder") {
        // since the account exists: no grant row records a date
        let since = ulid::Ulid::from_string(id).map(|u| u.timestamp_ms() as i64).unwrap_or(0);
        shown.insert(0, v0::PublicBadge { badge: "founder".to_string(), granted_at: since });
    }
    shown
}

/// The rights for the retired admin routes: every change through them is
/// refused (and written to the audit log) by `mutate_badge`
fn admin_route_actor(actor: &User) -> BadgeActor {
    BadgeActor {
        id: actor.id.clone(),
        path: BadgePath::AdminRoute,
        staff: false,
        owner: false,
        step_up: false,
        session_id: None,
    }
}

/// Badge staff by real permissions only: `badges.grant` from a platform
/// role, or `platform.badges.manage` from an active staff grant. The
/// `privileged` flag counts for nothing here.
async fn badge_staff(db: &Database, actor: &User) -> Result<bool> {
    if actor.bot.is_some() {
        return Ok(false);
    }
    let mut plain = actor.clone();
    plain.privileged = false;
    if platform_permissions(db, &plain).await?.has(PlatformPermission::BadgesGrant) {
        return Ok(true);
    }
    let now = now_ms();
    Ok(db
        .fetch_staff_grant(&actor.id)
        .await?
        .is_some_and(|grant| grant.effective(now).contains(&StaffPermission::BadgesManage)))
}

/// Staff need `badges.grant` (or `platform.badges.manage`) to look
async fn staff_only(db: &Database, actor: &User) -> Result<()> {
    no_bots(actor)?;
    if badge_staff(db, actor).await? {
        return Ok(());
    }
    Err(create_error!(MissingPlatformPermission {
        permission: PlatformPermission::BadgesGrant.name().to_string()
    }))
}

/// # Find badge holders
///
/// Users matching a name or id, or everyone holding one badge.
#[openapi(tag = "Badges")]
#[get("/admin/users?<options..>")]
pub async fn admin_search(
    db: &State<Database>,
    user: User,
    options: v0::OptionsBadgeSearch,
) -> Result<Json<Vec<v0::BadgeHolder>>> {
    // beta staff search here too, to invite testers
    no_bots(&user)?;
    let perms = platform_permissions(db, &user).await?;
    if !perms.has(PlatformPermission::BadgesGrant) && !perms.has(PlatformPermission::BetaManage) {
        require(&perms, PlatformPermission::BadgesGrant)?;
    }
    let options = validated(options)?;
    let ids: Vec<String> = match (
        options.query.as_deref().map(str::trim).filter(|q| !q.is_empty()),
        options.badge.as_deref(),
    ) {
        (Some(query), _) => db
            .search_users_by_name(query.trim_start_matches('@'), 25)
            .await?
            .into_iter()
            .filter(|u| u.bot.is_none())
            .map(|u| u.id)
            .collect(),
        (None, Some(badge)) => db
            .fetch_badge_holders(badge, 100)
            .await?
            .into_iter()
            .map(|g| g.user_id)
            .collect(),
        (None, None) => vec![],
    };
    let now = now_ms();
    let grants = db.fetch_badge_grants_for(&ids).await?;
    let users = accounts_for(db, ids.clone()).await?;
    Ok(Json(
        ids.iter()
            .filter_map(|id| users.iter().find(|u| &u.id == id).cloned())
            .map(|u| v0::BadgeHolder {
                badges: grants
                    .iter()
                    .filter(|g| g.user_id == u.id && g.held(now))
                    .map(|g| g.badge.clone())
                    .collect(),
                user: u,
            })
            .collect(),
    ))
}

/// # A user's badge record
///
/// Grants, audit log and which badges staff can give now.
#[openapi(tag = "Badges")]
#[get("/admin/users/<id>")]
pub async fn admin_user(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<v0::AdminUserBadges>> {
    staff_only(db, &user).await?;
    // read only: changes happen in the admin console
    user_record(db, &admin_route_actor(&user), id).await.map(Json)
}

/// A user's badge record, with what `actor` may give them now (a step-up
/// counts as available: the console asks for one before the change)
pub(crate) async fn user_record(db: &Database, actor: &BadgeActor, id: &str) -> Result<v0::AdminUserBadges> {
    let mut actor = actor.clone();
    actor.step_up = true;
    let subject = db.fetch_user(id).await?;
    no_bots(&subject)?;
    sync_user_badges(db, &subject.id).await?;
    let now = now_ms();
    let grants = db.fetch_badge_grants(&subject.id).await?;
    let mut eligibility = vec![];
    for definition in BADGE_CATALOGUE.iter().filter(|b| !b.retired) {
        let reason = match badge_rule_check(&actor, definition, &subject, BadgeChange::Grant) {
            Err(BadgeDenial::Locked) => Some("locked: follows the founder setting".to_string()),
            Err(BadgeDenial::Appointment) => Some("follows an official appointment".to_string()),
            Err(_) => Some("not allowed for you".to_string()),
            Ok(_) => badge_eligibility(db, &subject.id, definition.id).await?,
        };
        eligibility.push(v0::BadgeEligibility {
            badge: definition.id.to_string(),
            allowed: reason.is_none(),
            reason,
        });
    }
    Ok(v0::AdminUserBadges {
        user: accounts_for(db, [subject.id.clone()])
            .await?
            .pop()
            .ok_or_else(|| create_error!(NotFound))?,
        grants: grants.into_iter().map(|g| admin_grant_view(g, now)).collect(),
        log: db
            .fetch_badge_logs(&subject.id)
            .await?
            .into_iter()
            .map(|e| v0::BadgeLogEntry {
                id: e.id,
                badge: e.badge,
                actor_id: e.actor_id,
                action: format!("{:?}", e.action),
                reason: e.reason,
                created_at: e.created_at,
            })
            .collect(),
        eligibility,
    })
}

/// # Grant a badge (retired)
///
/// Retired: badges are given only in the admin console. Every call is
/// refused with a clear error and written to the platform audit log.
#[openapi(tag = "Badges")]
#[post("/admin/grant", data = "<data>")]
pub async fn admin_grant(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataGrantBadge>,
) -> Result<Json<v0::AdminBadgeGrant>> {
    no_bots(&user)?;
    let data = data.into_inner();
    retired_change(db, &user, data.user, data.badge, BadgeChange::Grant, data.reason).await
}

/// # Revoke a badge (retired)
///
/// Retired: badges are taken away only in the admin console. Every call is
/// refused with a clear error and written to the platform audit log.
#[openapi(tag = "Badges")]
#[post("/admin/revoke", data = "<data>")]
pub async fn admin_revoke(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataRevokeBadge>,
) -> Result<Json<v0::AdminBadgeGrant>> {
    no_bots(&user)?;
    let data = data.into_inner();
    retired_change(db, &user, data.user, data.badge, BadgeChange::Revoke, data.reason).await
}

/// Refused for everyone. Only a staff caller's try is written to the audit
/// log (through the one gate, without the typed reason, and with the
/// target only if it is a real id): anyone else gets the same answer and
/// writes nothing, so the log can't be filled by any signed-in account.
async fn retired_change(
    db: &Database,
    actor: &User,
    user_id: String,
    badge: String,
    change: BadgeChange,
    reason: String,
) -> Result<Json<v0::AdminBadgeGrant>> {
    if !badge_staff(db, actor).await? {
        return Err(BadgeDenial::RouteRetired.error());
    }
    let request = BadgeMutation {
        user_id,
        badge,
        change,
        reason,
        expires_at: None,
    };
    match mutate_badge(db, &admin_route_actor(actor), request).await? {
        BadgeOutcome::Applied(grant) => Ok(Json(admin_grant_view(grant, now_ms()))),
        BadgeOutcome::Pending(_) => Err(create_error!(InvalidOperation)),
    }
}
